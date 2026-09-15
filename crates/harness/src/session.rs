use crate::{
    Digest,
    evolution::{
        Channel, HarnessBinding, HarnessProvenance, ModelIdentity, PolicyIdentity, TargetProfile,
        ValidatedHarnessRevision,
    },
    inference::ModelSettings,
    state::{Outcome, RequestKind, TaskId},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, str::FromStr};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}
impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for SessionId {
    type Err = uuid::Error;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(text).map(Self)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub workspace: PathBuf,
    pub model: ModelSettings,
    pub instructions: String,
    #[serde(default = "default_context_window_tokens")]
    pub context_window_tokens: u64,
}

/// Bounded client intent for a new session. The Host supplies every privileged
/// behavior and runtime identity after it canonicalizes this request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAdmissionRequest {
    workspace: PathBuf,
    model: ModelSettings,
    #[serde(default = "default_context_window_tokens")]
    context_window_tokens: u64,
    channel: Channel,
}

impl SessionAdmissionRequest {
    pub fn new(
        workspace: PathBuf,
        model: ModelSettings,
        context_window_tokens: u64,
        channel: Channel,
    ) -> Self {
        Self {
            workspace,
            model,
            context_window_tokens,
            channel,
        }
    }

    pub fn workspace(&self) -> &PathBuf {
        &self.workspace
    }

    pub const fn model(&self) -> ModelSettings {
        self.model
    }

    pub const fn context_window_tokens(&self) -> u64 {
        self.context_window_tokens
    }

    pub const fn channel(&self) -> Channel {
        self.channel
    }

    pub(crate) fn canonicalized(mut self, workspace: PathBuf) -> Self {
        self.workspace = workspace;
        self
    }
}

/// Immutable Host-derived authority used for every execution path of a
/// session. Fields are private so callers cannot assemble a privileged profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAdmissionProfile {
    version: u32,
    request: SessionAdmissionRequest,
    binding: HarnessBinding,
    provenance: HarnessProvenance,
    authority: Digest,
    request_digest: Digest,
    revision_manifest: Vec<u8>,
    behavior_instructions: String,
}

impl SessionAdmissionProfile {
    pub(crate) fn new(
        request: SessionAdmissionRequest,
        binding: HarnessBinding,
        provenance: HarnessProvenance,
        authority: Digest,
        revision: &ValidatedHarnessRevision,
    ) -> Result<Self, serde_json::Error> {
        let request_digest = Digest::of_value(&request)?;
        Ok(Self {
            version: 1,
            request,
            binding,
            provenance,
            authority,
            request_digest,
            revision_manifest: revision.canonical_bytes().to_vec(),
            behavior_instructions: revision.behavior_instructions().to_owned(),
        })
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn request(&self) -> &SessionAdmissionRequest {
        &self.request
    }

    pub const fn binding(&self) -> HarnessBinding {
        self.binding
    }

    pub const fn provenance(&self) -> HarnessProvenance {
        self.provenance
    }

    pub const fn authority(&self) -> Digest {
        self.authority
    }

    pub const fn request_digest(&self) -> Digest {
        self.request_digest
    }

    pub fn workspace(&self) -> &PathBuf {
        self.request.workspace()
    }

    pub const fn model(&self) -> ModelSettings {
        self.request.model()
    }

    pub const fn context_window_tokens(&self) -> u64 {
        self.request.context_window_tokens()
    }

    pub(crate) fn behavior_instructions(&self) -> &str {
        &self.behavior_instructions
    }

    pub(crate) fn revision_manifest(&self) -> &[u8] {
        &self.revision_manifest
    }

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 {
            return Err("unsupported session admission profile");
        }
        if !self.request.workspace.is_absolute() || !self.request.workspace.is_dir() {
            return Err("session workspace must be an absolute directory");
        }
        if self.request.context_window_tokens == 0 {
            return Err("session context window must be positive");
        }
        if self.behavior_instructions.is_empty() || self.behavior_instructions.len() > 32 * 1024 {
            return Err("session behavior instructions exceed their bound");
        }
        if Digest::of_value(&self.request).ok() != Some(self.request_digest) {
            return Err("session admission request identity mismatch");
        }
        let revision = ValidatedHarnessRevision::from_manifest_json(&self.revision_manifest)
            .map_err(|_| "session harness revision is invalid")?;
        if revision.digest() != self.binding.revision()
            || revision.behavior_digest() != self.binding.behavior()
            || revision.envelope_digest() != self.binding.envelope()
            || PolicyIdentity::from_digest(Digest::of(revision.policy_id().as_bytes()))
                != self.binding.policy()
        {
            return Err("session harness revision differs from its binding");
        }
        if revision.behavior_instructions() != self.behavior_instructions {
            return Err("session behavior differs from its harness revision");
        }
        let target: TargetProfile = self.binding.target();
        if target.model
            != ModelIdentity::from_digest(
                Digest::of_value(&self.request.model)
                    .map_err(|_| "session model identity could not be computed")?,
            )
            || target.channel != self.request.channel
        {
            return Err("session admission target differs from its request");
        }
        Ok(())
    }
}

const fn default_context_window_tokens() -> u64 {
    crate::context::DEFAULT_WINDOW_TOKENS
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use crate::evolution::{
        BaselineReason, EnvironmentIdentity, ProtocolIdentity, TaskProfileIdentity,
    };

    fn profile(workspace: PathBuf) -> SessionAdmissionProfile {
        let model = ModelSettings::default();
        let request = SessionAdmissionRequest::new(
            workspace,
            model,
            default_context_window_tokens(),
            Channel::Stable,
        );
        let target = TargetProfile::new(
            ModelIdentity::from_digest(Digest::of_value(&model).unwrap()),
            ProtocolIdentity::from_digest(Digest::of(b"protocol")),
            EnvironmentIdentity::from_digest(Digest::of(b"environment")),
            TaskProfileIdentity::from_digest(Digest::of(b"task-profile")),
            Channel::Stable,
        );
        let revision = ValidatedHarnessRevision::compiled_baseline().unwrap();
        let binding = HarnessBinding::baseline(
            target,
            revision.digest(),
            revision.behavior_digest(),
            revision.envelope_digest(),
            PolicyIdentity::from_digest(Digest::of(revision.policy_id().as_bytes())),
        );
        SessionAdmissionProfile::new(
            request,
            binding,
            HarnessProvenance::CompiledBaseline {
                reason: BaselineReason::StoreFixture,
            },
            Digest::of(b"authority"),
            &revision,
        )
        .unwrap()
    }

    #[test]
    fn serialized_behavior_tampering_is_rejected() {
        let workspace = tempfile::tempdir().unwrap();
        let mut serialized = serde_json::to_value(profile(workspace.path().to_owned())).unwrap();
        serialized["behavior_instructions"] = json!("forged behavior");
        let tampered: SessionAdmissionProfile = serde_json::from_value(serialized).unwrap();

        assert_eq!(
            tampered.validate(),
            Err("session behavior differs from its harness revision")
        );
    }

    #[test]
    fn request_target_tampering_is_rejected_after_digest_recomputation() {
        let workspace = tempfile::tempdir().unwrap();
        let mut tampered = profile(workspace.path().to_owned());
        tampered.request.model.reasoning_mode = crate::inference::ReasoningMode::Pro;
        tampered.request_digest = Digest::of_value(&tampered.request).unwrap();

        assert_eq!(
            tampered.validate(),
            Err("session admission target differs from its request")
        );
    }
}

/// A reference to journaled state, never a second serialized model machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionCursor {
    pub version: u32,
    pub session: SessionId,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceSeed {
    pub origin: Digest,
    pub source: Digest,
    pub task: Option<TaskId>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionBranch {
    pub workspace: Option<WorkspaceSeed>,
    pub fresh_context: bool,
    pub pending_task: Option<TaskId>,
    pub pending_shell: Option<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub feedbacks: std::collections::BTreeSet<Digest>,
    pub branch: SessionBranch,
    pub id: SessionId,
    pub revision: u64,
    pub settled_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<SessionAdmissionProfile>,
    /// Legacy forensic request data. Execution authority comes only from
    /// `admission`; these fields remain to verify schema-v3 projections.
    pub initial_config: SessionConfig,
    pub config: SessionConfig,
    pub parent: Option<SessionCursor>,
    pub history: Vec<Value>,
    pub operations: BTreeMap<Uuid, Digest>,
    pub current_task: Option<TaskId>,
    pub tasks_by_request: BTreeMap<Uuid, TaskId>,
    pub tool_calls: BTreeMap<String, RecordedToolCall>,
    pub kind: RequestKind,
    pub active_request: Option<Uuid>,
    pub outcome: Option<Outcome>,
    pub error: Option<String>,
    pub started_ms: u64,
    pub title: Option<String>,
    pub imported: Option<ImportedSource>,
    pub submissions: BTreeMap<Uuid, crate::submission::Submission>,
    pub queue_order: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImportedSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_operation: Option<Uuid>,
    pub import_id: Digest,
    pub manifest: Digest,
    pub source_snapshot: Digest,
    pub source_session: String,
    pub title: String,
    pub request_fingerprint: Digest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordedToolCall {
    pub request: Uuid,
    pub output: Option<Digest>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum SessionEvent {
    Created {
        branch: SessionBranch,
        config: SessionConfig,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        admission: Option<Box<SessionAdmissionProfile>>,
        parent: Option<SessionCursor>,
        history: Vec<Value>,
        at_ms: u64,
        /// Boxed to keep this variant close in size to `Command`; the archive
        /// descriptor is the largest field and is absent for native sessions.
        /// `Option<Box<_>>` serializes exactly like `Option<_>`, so the journal
        /// format is unchanged.
        imported: Option<Box<ImportedSource>>,
    },
    Command {
        operation: Uuid,
        command: SessionCommand,
        at_ms: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SessionCommand {
    AdmissionPinned {
        profile: Box<SessionAdmissionProfile>,
        legacy_config_digest: Digest,
    },
    ReviewRecorded {
        feedback: Digest,
    },
    LegacyImportBound {
        fingerprint: Digest,
    },
    ShellStarted {
        job: crate::manual::ManualJob,
    },
    ShellPublished {
        request: Uuid,
        report: Digest,
        seed: Option<WorkspaceSeed>,
        settled: bool,
    },
    WorkspaceSaved {
        request: Uuid,
        seed: WorkspaceSeed,
    },
    AuxiliaryStarted,
    AuxiliaryRecorded {
        request: Uuid,
        record: Digest,
    },
    AuxiliaryPublished {
        request: Uuid,
        report: Digest,
        text: Option<String>,
    },
    QueueMoved {
        request: Uuid,
        expected_input: Digest,
        before: Option<Uuid>,
    },
    QueueEdited {
        request: Uuid,
        expected_input: Digest,
        input: Option<Digest>,
    },
    Submitted(Box<crate::submission::Submission>),
    SubmissionChanged {
        request: Uuid,
        status: crate::submission::SubmissionStatus,
    },
    Input {
        kind: RequestKind,
        content: Vec<Value>,
    },
    Response {
        request: Uuid,
        items: Vec<Value>,
    },
    ProviderUsage {
        request: Uuid,
        usage: crate::inference::Usage,
    },
    Feedback {
        message: String,
    },
    ToolResult {
        request: Uuid,
        call_id: String,
        output: String,
    },
    TaskLinked {
        request: Uuid,
        task: TaskId,
    },
    TurnSettled {
        request: Uuid,
        outcome: Option<Outcome>,
        error: Option<String>,
    },
    SettingsChanged(ModelSettings),
    ContextProjected {
        source_revision: u64,
        projection: Vec<Value>,
    },
}

pub(crate) struct SessionCreation {
    pub(crate) branch: SessionBranch,
    pub(crate) config: SessionConfig,
    pub(crate) admission: Option<SessionAdmissionProfile>,
    pub(crate) parent: Option<SessionCursor>,
    pub(crate) history: Vec<Value>,
    pub(crate) started_ms: u64,
    pub(crate) imported: Option<ImportedSource>,
}

impl SessionState {
    pub fn fork_cursor(&self) -> SessionCursor {
        SessionCursor {
            revision: self.settled_revision,
            ..self.cursor()
        }
    }

    pub fn cursor(&self) -> SessionCursor {
        SessionCursor {
            version: 1,
            session: self.id,
            revision: self.revision,
        }
    }

    pub(crate) fn create(id: SessionId, creation: SessionCreation) -> Self {
        let SessionCreation {
            branch,
            config,
            admission,
            parent,
            history,
            started_ms,
            imported,
        } = creation;
        let title = imported
            .as_ref()
            .map(|source| {
                source
                    .title
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(120)
                    .collect()
            })
            .or_else(|| first_title(&history));
        let operations = if parent.is_none() {
            imported
                .as_ref()
                .and_then(|source| {
                    source
                        .first_operation
                        .map(|operation| (operation, source.request_fingerprint))
                })
                .into_iter()
                .collect()
        } else {
            BTreeMap::new()
        };
        Self {
            branch,
            feedbacks: std::collections::BTreeSet::new(),
            id,
            revision: 1,
            settled_revision: 1,
            admission,
            initial_config: config.clone(),
            config,
            parent,
            history,
            operations,
            current_task: None,
            tasks_by_request: BTreeMap::new(),
            tool_calls: BTreeMap::new(),
            kind: RequestKind::Conversation,
            active_request: None,
            outcome: None,
            error: None,
            started_ms,
            title,
            imported,
            submissions: BTreeMap::new(),
            queue_order: Vec::new(),
        }
    }

    pub(crate) fn apply(
        &mut self,
        operation: Uuid,
        command: &SessionCommand,
    ) -> Result<(), serde_json::Error> {
        match command {
            SessionCommand::AdmissionPinned {
                profile,
                legacy_config_digest,
            } => {
                if self.admission.is_some()
                    || Digest::of_value(&self.config)? != *legacy_config_digest
                    || profile.validate().is_err()
                {
                    return Err(serde_json::Error::io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid session admission pin",
                    )));
                }
                self.admission = Some(profile.as_ref().clone());
            }
            SessionCommand::ReviewRecorded { feedback } => {
                self.feedbacks.insert(*feedback);
            }
            SessionCommand::LegacyImportBound { .. } => {}
            SessionCommand::ShellStarted { job } => {
                self.branch.pending_shell = Some(operation);
                self.branch.workspace = Some(WorkspaceSeed {
                    origin: job.origin,
                    source: job.before,
                    task: job.task,
                });
                self.active_request = Some(operation);
                self.kind = RequestKind::Auxiliary;
                self.outcome = None;
                self.error = None;
                if let Some(submission) = self.submissions.get_mut(&operation) {
                    submission.manual_job = Some(job.clone());
                }
                if let Some(task) = job.task {
                    self.tasks_by_request.insert(operation, task);
                    self.branch.pending_task = Some(task);
                }
            }
            SessionCommand::ShellPublished {
                request,
                report,
                seed,
                settled,
            } => {
                if *settled {
                    self.branch.pending_shell = None;
                }
                if let Some(submission) = self.submissions.get_mut(request) {
                    submission.result = Some(*report);
                }
                if let Some(seed) = seed {
                    self.branch.workspace = Some(seed.clone());
                    self.branch.pending_task = None;
                    self.branch.pending_shell = None;
                }
            }
            SessionCommand::WorkspaceSaved { seed, .. } => {
                self.branch.workspace = Some(seed.clone());
                self.branch.pending_task = None;
            }
            SessionCommand::AuxiliaryStarted => {
                self.active_request = Some(operation);
                self.kind = RequestKind::Auxiliary;
                self.outcome = None;
                self.error = None;
            }
            SessionCommand::AuxiliaryRecorded { request, record } => {
                if let Some(submission) = self.submissions.get_mut(request) {
                    submission.records.push(*record);
                }
            }
            SessionCommand::AuxiliaryPublished {
                request,
                report,
                text,
            } => {
                if let Some(submission) = self.submissions.get_mut(request) {
                    submission.result = Some(*report);
                }
                if let Some(text) = text {
                    self.history
                        .push(json!({"role":"assistant","content":text}));
                }
            }
            SessionCommand::QueueMoved {
                request, before, ..
            } => {
                self.queue_order.retain(|id| id != request);
                let position = before
                    .and_then(|before| self.queue_order.iter().position(|id| *id == before))
                    .unwrap_or(self.queue_order.len());
                self.queue_order.insert(position, *request);
            }
            SessionCommand::QueueEdited { request, input, .. } => {
                if let Some(submission) = self.submissions.get_mut(request) {
                    if let Some(input) = input {
                        submission.input = *input;
                    } else {
                        self.queue_order.retain(|id| id != request);
                        self.queue_order.insert(0, *request);
                    }
                }
            }
            SessionCommand::Submitted(submission) => {
                self.submissions
                    .insert(submission.id, submission.as_ref().clone());
                self.queue_order.push(submission.id);
            }
            SessionCommand::SubmissionChanged { request, status } => {
                if let Some(submission) = self.submissions.get_mut(request) {
                    submission.status = status.clone();
                }
                if !status.pending() {
                    self.queue_order.retain(|id| id != request);
                }
            }
            SessionCommand::Input { kind, content } => {
                for message in content {
                    if let Some(parts) = message["content"].as_array() {
                        for part in parts {
                            if part["type"] == "tact_review" {
                                self.feedbacks
                                    .insert(serde_json::from_value(part["digest"].clone())?);
                            }
                        }
                    }
                }
                if self.title.is_none() {
                    self.title = first_title(content);
                }
                self.kind = *kind;
                self.active_request = Some(operation);
                self.outcome = None;
                self.error = None;
                self.history.extend(content.clone());
            }
            SessionCommand::Response { request, items } => {
                for item in items.iter().filter(|item| item["type"] == "function_call") {
                    if let Some(id) = item["call_id"].as_str() {
                        self.tool_calls.insert(
                            id.into(),
                            RecordedToolCall {
                                request: *request,
                                output: None,
                            },
                        );
                    }
                }
                self.history.extend(items.clone());
            }
            SessionCommand::ProviderUsage { .. } => {}
            SessionCommand::Feedback { message } => self
                .history
                .push(json!({"role":"developer","content":message})),
            SessionCommand::ToolResult {
                call_id, output, ..
            } => {
                if let Some(call) = self.tool_calls.get_mut(call_id) {
                    call.output = Some(Digest::of(output.as_bytes()));
                }
                self.history
                    .push(json!({"type":"function_call_output","call_id":call_id,"output":output}));
            }
            SessionCommand::TaskLinked { request, task } => {
                self.branch.pending_task = Some(*task);
                self.current_task = Some(*task);
                self.tasks_by_request.insert(*request, *task);
            }
            SessionCommand::TurnSettled { outcome, error, .. } => {
                self.active_request = None;
                self.outcome = *outcome;
                self.error = error.clone();
            }
            SessionCommand::SettingsChanged(settings) => {
                if self.admission.is_some() {
                    return Err(serde_json::Error::io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "bound session settings are immutable",
                    )));
                }
                self.config.model = *settings;
            }
            SessionCommand::ContextProjected { projection, .. } => {
                self.history = projection.clone()
            }
        }
        self.operations
            .insert(operation, Digest::of_value(command)?);
        self.revision += 1;
        if self.active_request.is_none() {
            self.settled_revision = self.revision;
        }
        Ok(())
    }

    pub fn admission(&self) -> Option<&SessionAdmissionProfile> {
        self.admission.as_ref()
    }

    pub fn workspace(&self) -> &PathBuf {
        self.admission
            .as_ref()
            .map_or(&self.config.workspace, SessionAdmissionProfile::workspace)
    }

    pub fn model(&self) -> ModelSettings {
        self.admission
            .as_ref()
            .map_or(self.config.model, SessionAdmissionProfile::model)
    }

    pub fn context_window_tokens(&self) -> u64 {
        self.admission.as_ref().map_or(
            self.config.context_window_tokens,
            SessionAdmissionProfile::context_window_tokens,
        )
    }

    pub(crate) fn behavior_instructions(&self) -> Result<&str, &'static str> {
        self.admission
            .as_ref()
            .ok_or("session has no trusted admission profile")
            .map(SessionAdmissionProfile::behavior_instructions)
    }
}

fn first_title(items: &[Value]) -> Option<String> {
    let message = items.iter().find(|item| item["role"] == "user")?;
    let text = if let Some(text) = message["content"].as_str() {
        text.to_owned()
    } else {
        message["content"]
            .as_array()?
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let title = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(120)
        .collect::<String>();
    Some(if title.trim().is_empty() {
        "Image input".into()
    } else {
        title
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalRecord {
    pub sequence: u64,
    pub aggregate: String,
    pub kind: String,
    pub revision: u64,
    pub event: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecentInput {
    pub session: SessionId,
    pub revision: u64,
    pub sequence: u64,
    pub at_ms: u64,
    pub workspace: PathBuf,
    pub text: String,
    pub truncated: bool,
}
