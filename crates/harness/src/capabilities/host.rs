//! Primary-session tools run with the user's native authority, not sandbox authority.
//! Process groups clean up ordinary descendants, not processes that escape by daemonizing.
use super::{Expected, ReadArgs, SearchArgs, WriteArgs, encoded};
use crate::Digest;
use orvek_executor::MAX_COMMAND_BYTES;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) const DEFAULT_EXEC_TIMEOUT_MS: u64 = 10 * 60 * 1_000;

#[derive(Clone, Debug)]
pub struct HostToolContext {
    pub cwd: PathBuf,
    pub task_id: Uuid,
    pub generation: u64,
    pub job_id: Uuid,
    pub timeout_ms: Option<u64>,
    pub max_output_bytes: usize,
}
#[derive(Debug, thiserror::Error)]
pub enum HostToolError {
    #[error("unknown native host tool")]
    UnknownTool,
    #[error("invalid native tool arguments or context")]
    InvalidArguments,
    #[error("file no longer matches the expected digest or absence")]
    Conflict,
    #[error("native operation cancelled")]
    Cancelled,
    #[error("native operation timed out")]
    TimedOut,
    #[error("native operation exceeds its byte budget")]
    OutputBudget,
    #[error("native process outcome is unknown; do not retry automatically")]
    OutcomeUnknown,
    #[error("native tool requires a Unix host")]
    UnsupportedPlatform,
    #[error("native filesystem or process operation failed: {0}")]
    Io(#[from] io::Error),
}
impl HostToolError {
    pub fn requires_reconciliation(&self) -> bool {
        matches!(self, Self::OutcomeUnknown)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum NativeExecutionStatus {
    Exited(i32),
    Signaled,
    TimedOut,
    Cancelled,
    OutputLimit,
    Unknown(String),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeEnvironment {
    pub backend: String,
    pub cwd: PathBuf,
    pub command: String,
    pub os: String,
    pub arch: String,
    /// Records inheritance policy, never environment variable values.
    pub environment: String,
    pub termination_scope: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeExecutionResult {
    pub job_id: Uuid,
    pub status: NativeExecutionStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub output_truncated: bool,
    pub metadata: NativeEnvironment,
}
pub struct HostToolRun {
    pub result: Result<Value, HostToolError>,
    pub execution: Option<NativeExecutionResult>,
    pub diagnostic: Option<String>,
}
#[derive(Clone, Default)]
pub struct HostTools;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecArgs {
    command: String,
    cwd: Option<String>,
}
impl HostTools {
    pub fn new() -> Self {
        Self
    }
    pub fn definitions() -> Vec<Value> {
        let mut definitions = super::WorkspaceTools::definitions();
        for definition in &mut definitions {
            definition["description"] = match definition["name"].as_str().unwrap_or_default() {
                "read_file" => "Read a native host file, including outside the project. Returns digest and bounded content.",
                "search" => "Search literal text in native host files or directories. Follows ordinary symlinks; reports bounded coverage.",
                "write_file" => "Create, replace, or delete a native host file after checking expected digest or absence. Creates missing parent directories. Ordinary symlinks are followed. Concurrent external writers are not fenced.",
                _ => "Run /bin/sh on the native host with inherited HOME, PATH, environment and network. No Docker or sandbox. Optional cwd accepts absolute, relative or ~/ paths. User cancellation or the ten-minute deadline kills the process group, but cannot fence daemonized escapes. Output is truncated without stopping the command. Unknown outcomes must not be retried automatically.",
            }.into();
            fn paths(value: &mut Value) {
                if let Some(object) = value.as_object_mut() {
                    if let Some(path) = object.get_mut("path") {
                        path["description"] = "Native host path: absolute, relative to cwd, or ~/; parent traversal and ordinary symlinks are allowed.".into();
                    }
                    for child in object.values_mut() {
                        paths(child);
                    }
                } else if let Some(array) = value.as_array_mut() {
                    for child in array {
                        paths(child);
                    }
                }
            }
            paths(&mut definition["parameters"]);
            if definition["name"] == "exec_command" {
                definition["parameters"]["properties"]["cwd"] =
                    json!({"type":"string","minLength":1});
            }
        }
        definitions
    }
    pub async fn execute_recorded(
        &self,
        name: &str,
        arguments: Value,
        context: HostToolContext,
        cancellation: CancellationToken,
    ) -> HostToolRun {
        let mut execution = None;
        let result = async {
            if !context.cwd.is_absolute() || context.timeout_ms == Some(0) || context.max_output_bytes < 4096 || context.max_output_bytes > 16*1024*1024 || serde_json::to_vec(&arguments).map_err(|_| HostToolError::InvalidArguments)?.len() > 2*1024*1024 { return Err(HostToolError::InvalidArguments); }
            let cwd = fs::canonicalize(&context.cwd)?;
            let result = if name == "exec_command" {
                let args: ExecArgs = decode(arguments)?;
                if args.command.trim().is_empty() || args.command.len() > MAX_COMMAND_BYTES { return Err(HostToolError::InvalidArguments); }
                let actual_cwd = fs::canonicalize(resolve(&cwd, args.cwd.as_deref().unwrap_or("."))?)?;
                let native = run_command(args.command, actual_cwd, &context, cancellation).await?;
                let outcome_unknown = matches!(native.status, NativeExecutionStatus::Unknown(_));
                let value = json!({"status":native.status,"stdout":encoded(&native.stdout),"stderr":encoded(&native.stderr),"output_truncated":native.output_truncated,"elapsed_ms":native.elapsed_ms,"metadata":native.metadata});
                execution = Some(native);
                if outcome_unknown {
                    return Err(HostToolError::OutcomeUnknown);
                }
                value
            } else {
                let name = name.to_owned();
                let context = context.clone();
                tokio::task::spawn_blocking(move || file_tool(&name, arguments, &cwd, &context, &cancellation)).await.map_err(|_| HostToolError::OutcomeUnknown)??
            };
            let value = json!({"task_id":context.task_id,"generation":context.generation,"job_id":context.job_id,"backend":"native_host","cwd":context.cwd,"result":result});
            if serde_json::to_vec(&value).map_err(|_| HostToolError::OutputBudget)?.len() > context.max_output_bytes { return Err(HostToolError::OutputBudget); }
            Ok(value)
        }.await;
        HostToolRun {
            result,
            execution,
            diagnostic: None,
        }
    }
}
fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, HostToolError> {
    serde_json::from_value(value).map_err(|_| HostToolError::InvalidArguments)
}
fn resolve(cwd: &Path, value: &str) -> Result<PathBuf, HostToolError> {
    if value.contains('\0') {
        return Err(HostToolError::InvalidArguments);
    }
    if value == "~" || value.starts_with("~/") {
        let home = std::env::var_os("HOME").ok_or(HostToolError::InvalidArguments)?;
        return Ok(PathBuf::from(home).join(value.strip_prefix("~/").unwrap_or("")));
    }
    Ok(cwd.join(value))
}
fn check(cancel: &CancellationToken, deadline: Option<Instant>) -> Result<(), HostToolError> {
    if cancel.is_cancelled() {
        return Err(HostToolError::Cancelled);
    }
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        return Err(HostToolError::TimedOut);
    }
    Ok(())
}
fn read_bounded(path: &Path, max: usize) -> Result<Vec<u8>, HostToolError> {
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(HostToolError::InvalidArguments);
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(HostToolError::OutputBudget);
    }
    Ok(bytes)
}
fn file_tool(
    name: &str,
    arguments: Value,
    cwd: &Path,
    context: &HostToolContext,
    cancel: &CancellationToken,
) -> Result<Value, HostToolError> {
    let deadline = context
        .timeout_ms
        .map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms));
    check(cancel, deadline)?;
    let budget = context.max_output_bytes.saturating_sub(2048) / 6;
    match name {
        "read_file" => {
            let args: ReadArgs = decode(arguments)?;
            if args.path.is_empty() || args.max_bytes == 0 || args.max_bytes > 1024 * 1024 {
                return Err(HostToolError::InvalidArguments);
            }
            let path = resolve(cwd, &args.path)?;
            let bytes = read_bounded(&path, 16 * 1024 * 1024)?;
            check(cancel, deadline)?;
            let start = args.offset.min(bytes.len());
            let end = start
                .saturating_add(args.max_bytes.min(budget))
                .min(bytes.len());
            Ok(
                json!({"path":path,"digest":Digest::of(&bytes),"size_bytes":bytes.len(),"offset":start,"content":encoded(&bytes[start..end]),"truncated":end<bytes.len()}),
            )
        }
        "write_file" => {
            let args: WriteArgs = decode(arguments)?;
            let (path, expected, content) = match args {
                WriteArgs::Replace {
                    path,
                    expected,
                    content,
                } => (path, expected, Some(content)),
                WriteArgs::Delete { path, expected } => (path, expected, None),
            };
            if path.is_empty() || content.as_ref().is_some_and(|s| s.len() > 1024 * 1024) {
                return Err(HostToolError::InvalidArguments);
            }
            let path = resolve(cwd, &path)?;
            let current = match read_bounded(&path, 16 * 1024 * 1024) {
                Ok(bytes) => Some(bytes),
                Err(HostToolError::Io(e)) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            match (expected, current.as_ref()) {
                (Expected::Absent, None) => {}
                (Expected::Digest { digest }, Some(bytes)) if Digest::of(bytes) == digest => {}
                _ => return Err(HostToolError::Conflict),
            }
            check(cancel, deadline)?;
            if let Some(content) = content {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                // Follow the final symlink as ordinary host editors do. The digest check is
                // optimistic; external writers can race it, and in-place writes are not atomic.
                let mut options = fs::OpenOptions::new();
                options.write(true);
                if matches!(expected, Expected::Absent) {
                    options.create_new(true);
                } else {
                    options.truncate(true);
                }
                let mut file = options.open(&path).map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        HostToolError::Conflict
                    } else {
                        e.into()
                    }
                })?;
                file.write_all(content.as_bytes())?;
                Ok(
                    json!({"path":path,"digest":Digest::of(content.as_bytes()),"written_bytes":content.len()}),
                )
            } else {
                fs::remove_file(&path)?;
                Ok(json!({"path":path,"deleted":true}))
            }
        }
        "search" => {
            let args: SearchArgs = decode(arguments)?;
            if args.query.is_empty()
                || args.query.len() > 256
                || args.query.contains('\n')
                || args.max_files == 0
                || args.max_files > 10000
                || args.max_results == 0
                || args.max_results > 200
                || args.max_bytes == 0
                || args.max_bytes > 16 * 1024 * 1024
            {
                return Err(HostToolError::InvalidArguments);
            }
            let mut pending = vec![resolve(cwd, &args.path)?];
            let mut seen = HashSet::new();
            let mut matches = Vec::new();
            let mut files = 0;
            let mut bytes_read = 0;
            let mut truncated = false;
            let mut output_bytes = 0;
            let mut skipped = 0;
            while let Some(path) = pending.pop() {
                check(cancel, deadline)?;
                if files >= args.max_files
                    || bytes_read >= args.max_bytes
                    || matches.len() >= args.max_results
                {
                    truncated = true;
                    break;
                }
                let canonical = match fs::canonicalize(&path) {
                    Ok(p) => p,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                if !seen.insert(canonical.clone()) {
                    continue;
                }
                let metadata = match fs::metadata(&canonical) {
                    Ok(m) => m,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                if metadata.is_dir() {
                    let entries = match fs::read_dir(&canonical) {
                        Ok(e) => e,
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    };
                    for entry in entries {
                        check(cancel, deadline)?;
                        if pending.len() >= 10000 || seen.len() >= 20000 {
                            truncated = true;
                            break;
                        }
                        match entry {
                            Ok(e) => pending.push(e.path()),
                            Err(_) => skipped += 1,
                        }
                    }
                    continue;
                }
                if !metadata.is_file() {
                    skipped += 1;
                    continue;
                }
                files += 1;
                let remaining = args.max_bytes - bytes_read;
                let mut bytes = Vec::new();
                match fs::File::open(&canonical)
                    .and_then(|file| file.take(remaining as u64).read_to_end(&mut bytes))
                {
                    Ok(_) => {}
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                }
                bytes_read += bytes.len();
                if metadata.len() > bytes.len() as u64 {
                    truncated = true;
                }
                for (line_number, line) in bytes.split(|b| *b == b'\n').enumerate() {
                    check(cancel, deadline)?;
                    if line
                        .windows(args.query.len())
                        .any(|window| window == args.query.as_bytes())
                    {
                        let value = json!({"path":canonical,"line":line_number+1,"content":encoded(&line[..line.len().min(1024)])});
                        output_bytes += serde_json::to_vec(&value)
                            .map_err(|_| HostToolError::OutputBudget)?
                            .len();
                        if output_bytes > budget || matches.len() >= args.max_results {
                            truncated = true;
                            break;
                        }
                        matches.push(value);
                    }
                }
            }
            Ok(
                json!({"matches":matches,"files_scanned":files,"bytes_scanned":bytes_read,"skipped":skipped,"truncated":truncated}),
            )
        }
        _ => Err(HostToolError::UnknownTool),
    }
}

#[cfg(unix)]
async fn run_command(
    command: String,
    cwd: PathBuf,
    context: &HostToolContext,
    cancel: CancellationToken,
) -> Result<NativeExecutionResult, HostToolError> {
    check(
        &cancel,
        context
            .timeout_ms
            .map(|timeout_ms| Instant::now() + Duration::from_millis(timeout_ms)),
    )?;
    let started = Instant::now();
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child
        .id()
        .and_then(|id| rustix::process::Pid::from_raw(id as i32))
        .ok_or(HostToolError::OutcomeUnknown)?;
    let mut stdout_pipe = child.stdout.take().ok_or(HostToolError::OutcomeUnknown)?;
    let mut stderr_pipe = child.stderr.take().ok_or(HostToolError::OutcomeUnknown)?;
    let command_timeout =
        Duration::from_millis(context.timeout_ms.unwrap_or(DEFAULT_EXEC_TIMEOUT_MS));
    let deadline = tokio::time::sleep(command_timeout);
    tokio::pin!(deadline);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut output_truncated = false;
    let (mut out_buffer, mut err_buffer) = ([0u8; 4096], [0u8; 4096]);
    let (mut out_open, mut err_open) = (true, true);
    let mut exited = None;
    let limit = context.max_output_bytes.saturating_sub(2048) / 6;
    let status = loop {
        if !out_open
            && !err_open
            && let Some(status) = exited.take()
        {
            break status;
        }
        tokio::select! {
            biased;
            _=cancel.cancelled()=>break NativeExecutionStatus::Cancelled,
            _=&mut deadline=>break NativeExecutionStatus::TimedOut,
            result=child.wait(), if exited.is_none()=>{
                match result { Ok(status)=>exited=Some(status.code().map(NativeExecutionStatus::Exited).unwrap_or(NativeExecutionStatus::Signaled)), Err(_)=>break NativeExecutionStatus::Unknown("native wait failed".into()) }
            }
            result=stdout_pipe.read(&mut out_buffer), if out_open=>{
                let count=match result {Ok(n)=>n,Err(_)=>break NativeExecutionStatus::Unknown("stdout capture failed".into())};
                out_open=count!=0;
                let remaining=limit.saturating_sub(stdout.len()+stderr.len());
                stdout.extend_from_slice(&out_buffer[..count.min(remaining)]);
                output_truncated |= count > remaining;
            }
            result=stderr_pipe.read(&mut err_buffer), if err_open=>{
                let count=match result {Ok(n)=>n,Err(_)=>break NativeExecutionStatus::Unknown("stderr capture failed".into())};
                err_open=count!=0;
                let remaining=limit.saturating_sub(stdout.len()+stderr.len());
                stderr.extend_from_slice(&err_buffer[..count.min(remaining)]);
                output_truncated |= count > remaining;
            }
        }
    };
    // Clean up ordinary background descendants even after the shell exits.
    let killed = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    let cleanup = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let status = if killed.is_err_and(|error| error != rustix::io::Errno::SRCH)
        || !matches!(cleanup, Ok(Ok(_)))
    {
        NativeExecutionStatus::Unknown(
            "native process group termination or wait was not confirmed".into(),
        )
    } else if matches!(
        status,
        NativeExecutionStatus::Cancelled | NativeExecutionStatus::TimedOut
    ) {
        NativeExecutionStatus::Unknown(
            "native command was interrupted, but escaped descendants cannot be proven absent"
                .into(),
        )
    } else {
        status
    };
    Ok(NativeExecutionResult {
        job_id: context.job_id,
        status,
        stdout,
        stderr,
        elapsed_ms: started.elapsed().as_millis() as u64,
        output_truncated,
        metadata: NativeEnvironment {
            backend: "native_host".into(),
            cwd,
            command,
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            environment: "inherited; values not recorded".into(),
            termination_scope: "process group only; daemonized escapes are not fenced".into(),
        },
    })
}
#[cfg(not(unix))]
async fn run_command(
    _: String,
    _: PathBuf,
    _: &HostToolContext,
    _: CancellationToken,
) -> Result<NativeExecutionResult, HostToolError> {
    Err(HostToolError::UnsupportedPlatform)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(cwd: &Path) -> HostToolContext {
        HostToolContext {
            cwd: cwd.to_owned(),
            task_id: Uuid::new_v4(),
            generation: 1,
            job_id: Uuid::new_v4(),
            timeout_ms: Some(10_000),
            max_output_bytes: 1 << 20,
        }
    }

    async fn run(
        name: &str,
        arguments: Value,
        context: HostToolContext,
    ) -> Result<Value, HostToolError> {
        HostTools::new()
            .execute_recorded(name, arguments, context, CancellationToken::new())
            .await
            .result
    }

    #[tokio::test]
    async fn read_write_search_cover_absolute_relative_and_home_paths() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("home");
        let project = directory.path().join("project");
        fs::create_dir_all(home.join("deep/nested")).unwrap();
        fs::create_dir_all(&project).unwrap();
        let elsewhere = home.join("deep/nested/morphex.txt");
        fs::write(&elsewhere, "production marker").unwrap();
        fs::write(project.join("local.txt"), "inside").unwrap();

        let context = context(&project);
        // Absolute path outside the working directory.
        let absolute = run(
            "read_file",
            json!({"path": elsewhere.to_str().unwrap()}),
            context.clone(),
        )
        .await
        .unwrap();
        assert_eq!(absolute["result"]["content"], encoded(b"production marker"));
        // ~/ resolution against the real HOME.
        let home_read = run(
            "read_file",
            json!({"path": "~/orvek-host-tools-test-marker"}),
            context.clone(),
        )
        .await;
        assert!(
            matches!(home_read, Err(HostToolError::Io(ref e)) if e.kind()==io::ErrorKind::NotFound)
        );
        // Parent traversal is allowed on the native host.
        let parent = run(
            "search",
            json!({"query":"production marker","path":".."}),
            context.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            parent["result"]["matches"].as_array().map(Vec::len),
            Some(1)
        );

        let digest = absolute["result"]["digest"].clone();
        let written = run("write_file", json!({"operation":"replace","path": elsewhere.to_str().unwrap(),"expected":{"kind":"digest","digest":digest},"content":"updated by host tools"}), context.clone()).await.unwrap();
        assert_eq!(
            written["result"]["written_bytes"],
            "updated by host tools".len()
        );
        let stale = run("write_file", json!({"operation":"replace","path": elsewhere.to_str().unwrap(),"expected":{"kind":"digest","digest":digest},"content":"x"}), context.clone()).await;
        assert!(matches!(stale, Err(HostToolError::Conflict)));
    }

    #[tokio::test]
    async fn exec_runs_native_shells_with_optional_cwd_and_reports_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let context = context(&directory.path().join("missing-cwd-default"));
        // Relative default cwd would fail canonicalize, so run from tempdir root instead.
        let context = HostToolContext {
            cwd: directory.path().to_owned(),
            ..context
        };
        let result = run(
            "exec_command",
            json!({"command":"pwd","cwd": nested.to_str().unwrap()}),
            context.clone(),
        )
        .await
        .unwrap();
        assert_eq!(result["result"]["status"]["kind"], "exited");
        assert_eq!(result["result"]["metadata"]["backend"], "native_host");
        assert_eq!(
            result["result"]["metadata"]["cwd"],
            json!(fs::canonicalize(&nested).unwrap().to_str().unwrap())
        );

        let home_probe = run(
            "exec_command",
            json!({"command":"test -n \"$HOME\" && echo home-present"}),
            context,
        )
        .await
        .unwrap();
        assert_eq!(home_probe["result"]["status"]["kind"], "exited");
    }

    #[tokio::test]
    async fn exec_without_deadline_truncates_output_without_stopping_the_command() {
        let directory = tempfile::tempdir().unwrap();
        let context = HostToolContext {
            timeout_ms: None,
            max_output_bytes: 4096,
            ..context(directory.path())
        };
        let result = run(
            "exec_command",
            json!({"command":"head -c 10000 /dev/zero; printf finished > completed"}),
            context,
        )
        .await
        .unwrap();

        assert_eq!(result["result"]["status"]["kind"], "exited");
        assert_eq!(result["result"]["output_truncated"], true);
        assert_eq!(
            fs::read_to_string(directory.path().join("completed")).unwrap(),
            "finished"
        );
    }

    #[cfg(unix)]
    fn daemon_command(pid_file: &Path, ready_file: Option<&Path>) -> String {
        let ready = ready_file.map_or_else(String::new, |_| {
            "pathlib.Path(sys.argv[2]).write_text(\"ready\"); ".to_owned()
        });
        let ready_arg =
            ready_file.map_or_else(String::new, |path| format!(" '{}'", path.display()));
        format!(
            "python3 -c 'import os, pathlib, sys, time; os.setsid(); pathlib.Path(sys.argv[1]).write_text(str(os.getpid())); {ready}time.sleep(30)' '{}'{} </dev/null >/dev/null 2>&1 & wait",
            pid_file.display(),
            ready_arg
        )
    }

    #[cfg(unix)]
    fn cleanup_daemon(pid_file: &Path) {
        if let Ok(pid) = fs::read_to_string(pid_file)
            && let Ok(pid) = pid.trim().parse::<i32>()
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemonized_timeout_is_unknown_and_the_escaped_process_is_cleaned_up() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("daemon.pid");
        let context = HostToolContext {
            timeout_ms: Some(500),
            ..context(directory.path())
        };
        let run = HostTools::new()
            .execute_recorded(
                "exec_command",
                json!({"command":daemon_command(&pid_file, None)}),
                context,
                CancellationToken::new(),
            )
            .await;

        cleanup_daemon(&pid_file);
        assert!(matches!(run.result, Err(HostToolError::OutcomeUnknown)));
        assert!(matches!(
            run.execution.map(|execution| execution.status),
            Some(NativeExecutionStatus::Unknown(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn daemonized_cancel_is_unknown_and_the_escaped_process_is_cleaned_up() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("daemon.pid");
        let ready_file = directory.path().join("daemon.ready");
        let command = daemon_command(&pid_file, Some(&ready_file));
        let cancel = CancellationToken::new();
        let cancellation = cancel.clone();
        let context = context(directory.path());
        let execution = tokio::spawn(async move {
            HostTools::new()
                .execute_recorded(
                    "exec_command",
                    json!({"command":command}),
                    context,
                    cancellation,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if tokio::fs::try_exists(&ready_file).await.unwrap() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("daemon did not signal readiness");
        assert!(pid_file.exists());
        cancel.cancel();
        let run = execution.await.unwrap();

        cleanup_daemon(&pid_file);
        assert!(matches!(run.result, Err(HostToolError::OutcomeUnknown)));
        assert!(matches!(
            run.execution.map(|execution| execution.status),
            Some(NativeExecutionStatus::Unknown(_))
        ));
    }

    #[tokio::test]
    async fn write_creates_missing_parent_directories_and_absent_semantics_hold() {
        let directory = tempfile::tempdir().unwrap();
        let context = context(directory.path());
        let target = directory.path().join("a/b/c/new.txt");
        let created = run("write_file", json!({"operation":"replace","path": target.to_str().unwrap(),"expected":{"kind":"absent"},"content":"fresh"}), context.clone()).await.unwrap();
        assert_eq!(
            created["result"]["digest"],
            json!(Digest::of(b"fresh").to_string())
        );
        let again = run("write_file", json!({"operation":"replace","path": target.to_str().unwrap(),"expected":{"kind":"absent"},"content":"fresh"}), context).await;
        assert!(matches!(again, Err(HostToolError::Conflict)));
    }
}
