//! Workspace capabilities admit only explicit tool shapes and trusted host context.
//!
//! The controller owns task/generation admission, writer exclusion, job recording,
//! evidence invalidation, and completion. These handlers neither interpret output
//! as a check result nor certify a task. File updates compare expected bytes before
//! atomic publication; the host must quiesce noncooperating concurrent writers,
//! because POSIX rename cannot atomically compare a file's content digest.

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod files;

pub mod host;

use crate::{
    Digest,
    runtime::{
        DockerExecutor, ExecutionPolicy, ExecutionRequest, ExecutionResult, ExecutionStatus,
        RuntimeError,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_REQUEST_BYTES: usize = 2 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
const OUTPUT_METADATA_BYTES: usize = 1024;

/// Authenticated host data, deliberately not deserializable from tool arguments.
#[derive(Clone, Debug)]
pub struct ToolContext {
    pub workspace: PathBuf,
    pub task_id: Uuid,
    pub generation: u64,
    pub job_id: Uuid,
    pub readonly: bool,
    pub can_write: bool,
    pub max_output_bytes: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("unknown workspace tool")]
    UnknownTool,
    #[error("tool arguments do not match the declared schema")]
    InvalidArguments,
    #[error("invalid trusted tool context or output budget")]
    InvalidContext,
    #[error("tool request exceeds its byte limit")]
    RequestTooLarge,
    #[error("workspace path is not permitted")]
    PathDenied,
    #[error("workspace path was not found")]
    NotFound,
    #[error("only regular unlinked files on the workspace filesystem are supported")]
    UnsupportedFile,
    #[error("file exceeds the supported byte limit")]
    FileTooLarge,
    #[error("workspace writes are not authorized")]
    Readonly,
    #[error("file no longer matches the expected identity")]
    Conflict,
    #[error("workspace operation was cancelled")]
    Cancelled,
    #[error("workspace operation exceeded its deadline")]
    TimedOut,
    #[error("workspace output cannot fit the admitted byte budget")]
    OutputBudget,
    #[error("Docker execution could not be started")]
    Execution,
    #[error("execution outcome is unknown; reconcile the admitted job before settling it")]
    OutcomeUnknown,
    #[error("file tools require Linux or macOS")]
    UnsupportedPlatform,
    #[error("workspace filesystem operation failed: {0}")]
    Io(#[source] io::Error),
}

impl ToolError {
    pub fn requires_reconciliation(&self) -> bool {
        matches!(self, Self::OutcomeUnknown)
    }
}

impl From<io::Error> for ToolError {
    fn from(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::NotFound
        } else {
            Self::Io(error)
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_read_bytes")]
    max_bytes: usize,
}
fn default_read_bytes() -> usize {
    64 * 1024
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    path: String,
    #[serde(default = "default_matches")]
    max_results: usize,
    #[serde(default = "default_files")]
    max_files: usize,
    #[serde(default = "default_search_bytes")]
    max_bytes: usize,
}
fn default_matches() -> usize {
    50
}
fn default_files() -> usize {
    1000
}
fn default_search_bytes() -> usize {
    8 * 1024 * 1024
}

#[derive(Clone, Copy, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Expected {
    Absent,
    Digest { digest: Digest },
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum WriteArgs {
    Replace {
        path: String,
        expected: Expected,
        content: String,
    },
    Delete {
        path: String,
        expected: Expected,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    command: String,
}

/// The sole command backend is the strict Docker runtime supplied by the host.
#[derive(Clone)]
pub struct WorkspaceTools {
    executor: Arc<DockerExecutor>,
}

pub struct ToolRun {
    pub result: Result<Value, ToolError>,
    pub execution: Option<ExecutionResult>,
    pub diagnostic: Option<String>,
}

impl WorkspaceTools {
    pub fn new(executor: Arc<DockerExecutor>) -> Self {
        Self { executor }
    }

    pub fn definitions() -> Vec<Value> {
        let path = json!({"type":"string","minLength":1,"maxLength":4096,"description":"Relative workspace path. Symlinks and parent traversal are forbidden."});
        let expected = json!({"oneOf":[
            {"type":"object","properties":{"kind":{"const":"absent"}},"required":["kind"],"additionalProperties":false},
            {"type":"object","properties":{"kind":{"const":"digest"},"digest":{"type":"string","pattern":"^[0-9a-fA-F]{64}$"}},"required":["kind","digest"],"additionalProperties":false}
        ]});
        vec![
            json!({"type":"function","name":"read_file","description":"Read a bounded byte range of a regular workspace file. Returns the whole-file digest and explicit truncation/encoding metadata.","parameters":{"type":"object","properties":{"path":path,"offset":{"type":"integer","minimum":0},"max_bytes":{"type":"integer","minimum":1,"maximum":MAX_FILE_BYTES}},"required":["path"],"additionalProperties":false}}),
            json!({"type":"function","name":"search","description":"Search for a single-line literal byte sequence in regular workspace files. Returns at most one match per line and explicit coverage limits; never runs repository code.","parameters":{"type":"object","properties":{"query":{"type":"string","minLength":1,"maxLength":256},"path":{"type":"string","maxLength":4096,"description":"Relative directory; empty means the workspace root."},"max_results":{"type":"integer","minimum":1,"maximum":200},"max_files":{"type":"integer","minimum":1,"maximum":10000},"max_bytes":{"type":"integer","minimum":1,"maximum":16777216}},"required":["query"],"additionalProperties":false}}),
            json!({"type":"function","name":"write_file","description":"Atomically replace/create or delete a regular file after checking its expected digest or absence. Parent directories must exist. Writes require host authorization.","parameters":{"type":"object","oneOf":[
                {"type":"object","properties":{"operation":{"const":"replace"},"path":path,"expected":expected,"content":{"type":"string","maxLength":MAX_FILE_BYTES}},"required":["operation","path","expected","content"],"additionalProperties":false},
                {"type":"object","properties":{"operation":{"const":"delete"},"path":path,"expected":expected},"required":["operation","path","expected"],"additionalProperties":false}
            ]}}),
            json!({"type":"function","name":"exec_command","description":"Run a shell command inside the host-admitted isolated Docker workspace. Host context controls write access, deadlines, output limits, and job identity.","parameters":{"type":"object","properties":{"command":{"type":"string","minLength":1,"maxLength":65536}},"required":["command"],"additionalProperties":false}}),
        ]
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: Value,
        context: ToolContext,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError> {
        self.execute_recorded(name, arguments, context, cancellation)
            .await
            .result
    }

    pub async fn execute_recorded(
        &self,
        name: &str,
        arguments: Value,
        context: ToolContext,
        cancellation: CancellationToken,
    ) -> ToolRun {
        if name != "exec_command" {
            return ToolRun {
                result: Self::execute_file_tool(name, arguments, &context, &cancellation),
                execution: None,
                diagnostic: None,
            };
        }
        let prepared = validate(&context, &arguments)
            .and_then(|()| decode::<ExecArgs>(arguments))
            .and_then(|args| {
                if args.command.trim().is_empty() || args.command.len() > 65536 {
                    return Err(ToolError::InvalidArguments);
                }
                Control::new(&context, &cancellation).check()?;
                Ok(args)
            });
        let args = match prepared {
            Ok(args) => args,
            Err(error) => {
                return ToolRun {
                    result: Err(error),
                    execution: None,
                    diagnostic: None,
                };
            }
        };
        let result = self
            .executor
            .run_with_policy(
                &ExecutionRequest {
                    job_id: context.job_id,
                    workspace: context.workspace.clone(),
                    command: args.command,
                    readonly: context.readonly || !context.can_write,
                    timeout_ms: context.timeout_ms,
                    output_bytes: data_budget(&context).max(1) as u64,
                },
                if context.readonly || !context.can_write {
                    ExecutionPolicy::Protected
                } else {
                    ExecutionPolicy::Workspace
                },
                cancellation,
            )
            .await;
        match result {
            Ok(execution) => ToolRun {
                result: execution_output(&context, execution.clone()),
                execution: Some(execution),
                diagnostic: None,
            },
            Err(error) => {
                let diagnostic = Some(error.to_string());
                let result = Err(match error {
                    RuntimeError::Request(_) => ToolError::Execution,
                    _ => ToolError::OutcomeUnknown,
                });
                ToolRun {
                    result,
                    execution: None,
                    diagnostic,
                }
            }
        }
    }

    /// File-only entry point for host tooling and native filesystem verification.
    /// It cannot execute commands or accept model-supplied authority. All IO is
    /// bounded, synchronous, and cancellation-checked; await command execution
    /// after cancelling it rather than assuming a dropped future stopped a job.
    pub fn execute_file_tool(
        name: &str,
        arguments: Value,
        context: &ToolContext,
        cancellation: &CancellationToken,
    ) -> Result<Value, ToolError> {
        if !matches!(name, "read_file" | "search" | "write_file") {
            return Err(ToolError::UnknownTool);
        }
        validate(context, &arguments)?;
        let control = Control::new(context, cancellation);
        control.check()?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let root = files::Workspace::open(&context.workspace)?;
            let result = match name {
                "read_file" => root.read(decode(arguments)?, context, &control),
                "search" => root.search(decode(arguments)?, context, &control),
                "write_file" => {
                    if context.readonly || !context.can_write {
                        return Err(ToolError::Readonly);
                    }
                    root.write(decode(arguments)?, context, &control)
                }
                _ => unreachable!("validated tool name"),
            }?;
            envelope(context, result)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(ToolError::UnsupportedPlatform)
    }
}

fn execution_output(context: &ToolContext, result: ExecutionResult) -> Result<Value, ToolError> {
    let (status, detail, detail_truncated) = match result.status {
        ExecutionStatus::Exited(code) => (json!({"kind":"exited","code":code}), None, false),
        ExecutionStatus::Cancelled => (json!({"kind":"cancelled"}), None, false),
        ExecutionStatus::TimedOut => (json!({"kind":"timed_out"}), None, false),
        ExecutionStatus::OutputLimit => (json!({"kind":"output_limit"}), None, false),
        ExecutionStatus::Failed(message) => (
            json!({"kind":"failed"}),
            Some("executor reported a failure; inspect the protected host job record"),
            !message.is_empty(),
        ),
        ExecutionStatus::Unknown(_) => return Err(ToolError::OutcomeUnknown),
    };
    let limited = status["kind"] == "output_limit";
    envelope(
        context,
        json!({"status":status,"stdout":encoded(&result.stdout),"stderr":encoded(&result.stderr),"output_truncated":limited,"elapsed_ms":result.elapsed_ms,"image_id":result.image_id,"container_name":result.container_name,"detail":detail,"detail_truncated":detail_truncated}),
    )
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ToolError> {
    serde_json::from_value(value).map_err(|_| ToolError::InvalidArguments)
}

fn validate(context: &ToolContext, arguments: &Value) -> Result<(), ToolError> {
    if !context.workspace.is_absolute()
        || context.workspace.parent().is_none()
        || context.max_output_bytes < OUTPUT_METADATA_BYTES
        || context.max_output_bytes > MAX_OUTPUT_BYTES
        || context.timeout_ms == 0
        || context.timeout_ms > 3_600_000
    {
        return Err(ToolError::InvalidContext);
    }
    struct Counter(usize);
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_REQUEST_BYTES {
                return Err(io::Error::other("request bound"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter(0), arguments).map_err(|_| ToolError::RequestTooLarge)
}

fn envelope(context: &ToolContext, result: Value) -> Result<Value, ToolError> {
    let value = json!({"task_id":context.task_id,"generation":context.generation,"job_id":context.job_id,"result":result});
    if serde_json::to_vec(&value)
        .map_err(|_| ToolError::OutputBudget)?
        .len()
        > context.max_output_bytes
    {
        return Err(ToolError::OutputBudget);
    }
    Ok(value)
}

fn data_budget(context: &ToolContext) -> usize {
    context
        .max_output_bytes
        .saturating_sub(OUTPUT_METADATA_BYTES)
        / 6
}

fn encoded(bytes: &[u8]) -> Value {
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({"encoding":"utf8","data":text,"bytes":bytes.len()}),
        Err(_) => json!({"encoding":"base64","data":STANDARD.encode(bytes),"bytes":bytes.len()}),
    }
}

struct Control<'a> {
    cancel: &'a CancellationToken,
    deadline: Instant,
}
impl<'a> Control<'a> {
    fn new(context: &ToolContext, cancel: &'a CancellationToken) -> Self {
        Self {
            cancel,
            deadline: Instant::now() + Duration::from_millis(context.timeout_ms),
        }
    }
    fn check(&self) -> Result<(), ToolError> {
        if self.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(ToolError::TimedOut);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_and_result(status: ExecutionStatus) -> (ToolContext, ExecutionResult) {
        let job_id = Uuid::new_v4();
        let context = ToolContext {
            workspace: PathBuf::from("/isolated/workspace"),
            task_id: Uuid::new_v4(),
            generation: 9,
            job_id,
            readonly: false,
            can_write: true,
            max_output_bytes: 4096,
            timeout_ms: 1000,
        };
        let result = ExecutionResult {
            job_id,
            status,
            stdout: b"repository printed PASS".to_vec(),
            stderr: Vec::new(),
            elapsed_ms: 10,
            image_id: "sha256:fixture".into(),
            container_name: "fixture".into(),
        };
        (context, result)
    }

    #[test]
    fn unknown_runtime_outcomes_cannot_be_settled_by_success_text() {
        let (context, result) = context_and_result(ExecutionStatus::Unknown(
            "container termination was not confirmed".into(),
        ));
        let error = execution_output(&context, result).unwrap_err();
        assert!(error.requires_reconciliation());
        assert!(matches!(error, ToolError::OutcomeUnknown));
    }

    #[test]
    fn nonzero_exits_keep_the_actual_code_even_when_stdout_claims_pass() {
        let (context, result) = context_and_result(ExecutionStatus::Exited(23));
        let output = execution_output(&context, result).unwrap();
        assert_eq!(
            output["result"]["status"],
            json!({"kind":"exited","code":23})
        );
        assert_eq!(
            output["result"]["stdout"]["data"],
            "repository printed PASS"
        );
        assert_eq!(output["generation"], 9);
    }
}
