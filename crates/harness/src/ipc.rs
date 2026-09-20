//! Bounded local operator protocol. This socket is never exposed to model executors.

use crate::{
    Digest,
    contract::Contract,
    controller::{Host, HostWarning, TaskRun},
    inference::ModelSettings,
    session::{
        JournalRecord, SessionAdmissionProfile, SessionAdmissionRequest, SessionCursor, SessionId,
        SessionState,
    },
    state::{Outcome, TaskId},
    verification::CheckProgram,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 6;
pub const UNSUPPORTED_PROTOCOL_VERSION: &str = "unsupported operator protocol version";
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const HISTORY_PAGE_BYTES: usize = 768 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    pub id: Uuid,
    pub command: Command,
}

impl Request {
    pub fn new(command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: Uuid::new_v4(),
            command,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Command {
    MonitorReport {
        offset: usize,
        limit: usize,
    },
    MonitorSampling {
        every: u32,
    },
    InstallReadBehavior {
        expected: Digest,
        bytes: u32,
        note: String,
    },
    RollbackReadBehavior {
        expected: Digest,
    },
    RegisterEventSource {
        config: crate::event_intake::SourceConfig,
    },
    EventSource {
        source: Uuid,
    },
    DisableEventSource {
        source: Uuid,
    },
    DeliverEvent {
        source: Uuid,
        key: String,
        payload: String,
    },
    InspectEvent {
        source: Uuid,
        key: String,
    },
    CancelEvent {
        source: Uuid,
        key: String,
    },
    RecordReview {
        session: SessionId,
        manifest: Digest,
        source_identity: Digest,
        disposition: crate::feedback::Disposition,
        body: String,
    },
    InspectSessionReview {
        session: SessionId,
    },
    HandoffSession {
        id: SessionId,
        parent: SessionCursor,
    },
    InspectTaskReview {
        task: TaskId,
    },
    MoveSubmission {
        session: SessionId,
        request: Uuid,
        expected_input: Digest,
        before: Option<Uuid>,
    },
    ReviewCatalog {
        workspace: PathBuf,
    },
    InspectWorkspace {
        workspace: PathBuf,
        range: crate::review::ReviewRange,
    },
    ReviewFile {
        manifest: Digest,
        side: crate::review::ReviewSide,
        path: String,
    },
    ReviewFiles {
        manifest: Digest,
        side: crate::review::ReviewSide,
        offset: usize,
        limit: usize,
    },
    Submissions {
        session: SessionId,
        offset: usize,
        limit: usize,
    },
    ReplaceSubmission {
        session: SessionId,
        request: Uuid,
        expected_input: Digest,
        content: Vec<serde_json::Value>,
    },
    PromoteSubmission {
        session: SessionId,
        request: Uuid,
        expected_input: Digest,
    },
    Submit {
        session: SessionId,
        content: Vec<serde_json::Value>,
        intent: crate::submission::SubmitIntent,
    },
    Submission {
        session: SessionId,
        request: Uuid,
    },
    CancelSubmission {
        session: SessionId,
        request: Uuid,
    },
    LegacySessions {
        database: PathBuf,
        offset: usize,
        limit: usize,
    },
    ImportLegacy {
        database: PathBuf,
        source_session: String,
        request: SessionAdmissionRequest,
    },
    LegacyPage {
        session: SessionId,
        cursor: Option<crate::import::ImportCursor>,
        max_records: usize,
        max_bytes: usize,
    },
    ExecuteInput {
        session: SessionId,
        content: Vec<serde_json::Value>,
        limits: crate::contract::Limits,
        policy: crate::admission::RequestPolicy,
    },
    InspectTask {
        task: TaskId,
    },
    ReadArtifact {
        digest: Digest,
        offset: usize,
        limit: usize,
    },
    ArtifactFile {
        snapshot: Digest,
        path: String,
    },
    ArtifactTree {
        snapshot: Digest,
        offset: usize,
        limit: usize,
    },
    RecentInputs {
        limit: usize,
        before: Option<u64>,
        workspace: Option<PathBuf>,
    },
    Info,
    ShutdownIfIdle,
    Sessions {
        offset: usize,
        limit: usize,
    },
    ForkSession {
        id: SessionId,
        parent: SessionCursor,
    },
    History {
        cursor: SessionCursor,
        start: usize,
        limit: usize,
    },
    HistoryText {
        cursor: SessionCursor,
        item: usize,
        content_index: usize,
        offset: usize,
        limit: usize,
    },
    CreateSession {
        id: SessionId,
        request: SessionAdmissionRequest,
    },
    Session {
        id: SessionId,
    },
    Task {
        id: TaskId,
    },
    RegisterProgram {
        program: CheckProgram,
    },
    ExecuteContract {
        session: SessionId,
        input: String,
        contract: Contract,
    },
    ExecuteRequest {
        session: SessionId,
        input: String,
        limits: crate::contract::Limits,
        policy: crate::admission::RequestPolicy,
    },
    ResumeTask {
        session: SessionId,
        task: TaskId,
        expected_revision: u64,
        reason: String,
    },
    Cancel {
        session: SessionId,
    },
    Journal {
        after: u64,
        limit: u32,
    },
    Watch {
        after: u64,
        #[serde(default)]
        session: Option<SessionId>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum WatchFrame {
    Warnings {
        warnings: Vec<HostWarning>,
    },
    Journal(JournalRecord),
    Preview {
        session: SessionId,
        request: Uuid,
        delta: crate::inference::Delta,
    },
    PreviewGap {
        dropped: u64,
    },
    Subagent {
        session: SessionId,
        event: crate::controller::SubagentEvent,
    },
    Ready {
        after: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionView {
    pub branch: crate::session::SessionBranch,
    pub id: SessionId,
    pub revision: u64,
    pub fork_cursor: SessionCursor,
    pub workspace: PathBuf,
    pub model: ModelSettings,
    pub context_window_tokens: u64,
    pub admission: Option<SessionAdmissionView>,
    pub parent: Option<SessionCursor>,
    pub current_task: Option<TaskId>,
    pub active_request: Option<Uuid>,
    pub history_items: usize,
    pub outcome: Option<Outcome>,
    pub error: Option<String>,
    pub journal_sequence: u64,
    pub started_ms: u64,
    pub title: Option<String>,
    pub imported: Option<crate::session::ImportedSource>,
    #[serde(default)]
    pub cost_usd: crate::inference::UsdCost,
    #[serde(default)]
    pub cost_uncertain: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionAdmissionView {
    pub version: u32,
    pub binding: crate::admission_profile::HarnessBinding,
    pub provenance: crate::admission_profile::HarnessProvenance,
    pub authority: Digest,
    pub request_digest: Digest,
}

impl From<&SessionAdmissionProfile> for SessionAdmissionView {
    fn from(profile: &SessionAdmissionProfile) -> Self {
        Self {
            version: profile.version(),
            binding: profile.binding(),
            provenance: profile.provenance(),
            authority: profile.authority(),
            request_digest: profile.request_digest(),
        }
    }
}

impl From<(SessionState, u64, crate::session::ProviderCostSummary)> for SessionView {
    fn from(
        (state, journal_sequence, cost): (SessionState, u64, crate::session::ProviderCostSummary),
    ) -> Self {
        let workspace = state.workspace().clone();
        let model = state.model();
        let context_window_tokens = state.context_window_tokens();
        let admission = state.admission().map(SessionAdmissionView::from);
        Self {
            fork_cursor: state.fork_cursor(),
            branch: state.branch,
            id: state.id,
            revision: state.revision,
            workspace,
            model,
            context_window_tokens,
            admission,
            parent: state.parent,
            current_task: state.current_task,
            active_request: state.active_request,
            history_items: state.history.len(),
            outcome: state.outcome,
            error: state.error,
            journal_sequence,
            started_ms: state.started_ms,
            title: state.title,
            imported: state.imported,
            cost_usd: cost.total,
            cost_uncertain: cost.uncertain,
        }
    }
}

/// Entries count toward the history cursor even when their exact output needs text paging.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum HistoryEntry {
    Inline(serde_json::Value),
    /// Read content index zero with `HistoryText` at this entry's cursor and index.
    /// This is a transport reference, not a shortened tool result or a new receipt.
    ToolOutput {
        call_id: String,
        bytes: usize,
        digest: Digest,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryPage {
    pub cursor: SessionCursor,
    pub start: usize,
    pub items: Vec<HistoryEntry>,
    pub next: Option<usize>,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Response {
    Monitor(Box<crate::monitor::MonitorReport>),
    Behavior(Digest),
    EventSource(crate::event_intake::SourceRecord),
    Event(crate::event_intake::EventRecord),
    ReviewFeedback(Digest),
    TaskReview {
        view: crate::controller::ArtifactView,
        review: crate::review::ReviewInspection,
    },
    ReviewCatalog(crate::review::ReviewCatalog),
    WorkspaceReview(crate::review::ReviewInspection),
    ReviewFile(Option<crate::review::FrozenFile>),
    ReviewFiles(crate::review::ReviewFilePage),
    Submissions(crate::submission::SubmissionPage),
    Submission(crate::submission::Submission),
    LegacySessions(serde_json::Value),
    LegacyPage(crate::import::ImportPage),
    ArtifactView(crate::controller::ArtifactView),
    Artifact(serde_json::Value),
    RecentInputs(Vec<crate::session::RecentInput>),
    Info(crate::controller::HostInfo),
    Shutdown {
        accepted: bool,
    },
    Sessions(Vec<SessionView>),
    History(HistoryPage),
    HistoryText {
        cursor: SessionCursor,
        page: crate::context::TextPage,
    },
    Session(Box<SessionView>),
    Task {
        id: TaskId,
        revision: u64,
        scope_revision: u64,
        outcome: Option<Outcome>,
        reason: Option<String>,
        requirements: Option<usize>,
        evidence: usize,
        certificate: Option<Digest>,
    },
    Program {
        digest: Digest,
    },
    TaskFinished(Box<TaskRun>),
    Cancelled {
        requested: bool,
    },
    Journal(Vec<JournalRecord>),
    Error {
        message: String,
    },
}

pub async fn serve(host: Arc<Host>, shutdown: CancellationToken) -> io::Result<()> {
    host.start_queued().await.map_err(io::Error::other)?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let root = host.state_directory();
    let socket = root.join("host.sock");
    let uid = std::fs::metadata(root)?.uid();
    if socket.exists() {
        let metadata = std::fs::symlink_metadata(&socket)?;
        use std::os::unix::fs::FileTypeExt;
        if !metadata.file_type().is_socket() || metadata.uid() != uid {
            return Err(io::Error::other(
                "refusing to replace an unexpected socket path",
            ));
        }
        std::fs::remove_file(&socket)?;
    }
    // Host already holds the exclusive store-owner lock, so an old socket cannot have a live owner.
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let clients = Arc::new(Semaphore::new(16));
    let runs = Arc::new(Semaphore::new(4));
    let watchers = Arc::new(Semaphore::new(8));
    let mut handlers = JoinSet::new();
    let monitor_host = host.clone();
    let monitor_shutdown = shutdown.child_token();
    handlers.spawn(async move {
        tokio::select! {
            () = monitor_shutdown.cancelled() => {},
            () = monitor_host.run_monitor() => {},
        }
    });
    let intake_host = host.clone();
    let intake_shutdown = shutdown.child_token();
    handlers.spawn(async move {
        tokio::select! {
            () = intake_shutdown.cancelled() => {},
            result = intake_host.run_event_intake() => {
                if result.is_err() { intake_host.warn(HostWarning::EventIntakeStopped); }
            }
        }
    });
    let recovery_host = host.clone();
    let recovery_shutdown = shutdown.child_token();
    handlers.spawn(async move {
        tokio::select! {
            () = recovery_shutdown.cancelled() => {},
            result = recovery_host.recover_completion_hooks() => {
                if result.is_err() {
                    recovery_host.warn(HostWarning::CompletionHookRecoveryFailed);
                }
            }
        }
    });
    loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            completed = handlers.join_next(), if !handlers.is_empty() => { let _ = completed; }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if !stream.peer_cred().is_ok_and(|credentials| credentials.uid() == uid) { continue; }
                let Ok(permit) = clients.clone().try_acquire_owned() else { continue; };
                let host = host.clone();
                let runs = runs.clone();
                let watchers = watchers.clone();
                let stop = shutdown.child_token();
                let service_stop = shutdown.clone();
                handlers.spawn(async move {
                    let _permit = permit;
                    let _ = handle(stream, host, runs, watchers, stop, service_stop).await;
                });
            }
        }
    }
    while handlers.join_next().await.is_some() {}
    drop(listener);
    std::fs::remove_file(socket)?;
    Ok(())
}

async fn handle(
    mut stream: UnixStream,
    host: Arc<Host>,
    runs: Arc<Semaphore>,
    watchers: Arc<Semaphore>,
    shutdown: CancellationToken,
    service_shutdown: CancellationToken,
) -> io::Result<()> {
    let request: Request = timeout(IO_TIMEOUT, read_frame(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request deadline"))??;
    if matches!(request.command, Command::ShutdownIfIdle) {
        let accepted = host.shutdown_if_idle().await;
        let reply = timeout(
            IO_TIMEOUT,
            write_frame(&mut stream, &Response::Shutdown { accepted }),
        )
        .await;
        if accepted {
            service_shutdown.cancel();
        }
        return reply
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "shutdown reply deadline"))?;
    }
    if request.version == PROTOCOL_VERSION
        && let Command::Watch { after, session } = request.command
    {
        let _permit = watchers
            .try_acquire_owned()
            .map_err(|_| io::Error::other("host spectator capacity occupied"))?;
        return watch(&mut stream, host, after, session, shutdown).await;
    }
    let response = if request.version != PROTOCOL_VERSION {
        Response::Error {
            message: UNSUPPORTED_PROTOCOL_VERSION.into(),
        }
    } else {
        execute(host, runs, request, shutdown)
            .await
            .unwrap_or_else(|error| Response::Error {
                message: error.to_string(),
            })
    };
    timeout(IO_TIMEOUT, write_frame(&mut stream, &response))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response deadline"))?
}

async fn execute(
    host: Arc<Host>,
    runs: Arc<Semaphore>,
    request: Request,
    shutdown: CancellationToken,
) -> Result<Response, crate::controller::HostError> {
    Ok(match request.command {
        Command::MonitorReport { offset, limit } => {
            Response::Monitor(Box::new(host.monitor_report(offset, limit).await?))
        }
        Command::MonitorSampling { every } => {
            host.set_monitor_sampling(every).await?;
            Response::Monitor(Box::new(host.monitor_report(0, 100).await?))
        }
        Command::InstallReadBehavior {
            expected,
            bytes,
            note,
        } => Response::Behavior(host.install_read_behavior(expected, bytes, note).await?),
        Command::RollbackReadBehavior { expected } => {
            Response::Behavior(host.rollback_read_behavior(expected).await?)
        }
        Command::RegisterEventSource { config } => {
            Response::EventSource(host.register_event_source(config).await?)
        }
        Command::EventSource { source } => Response::EventSource(host.event_source(source).await?),
        Command::DisableEventSource { source } => {
            Response::EventSource(host.disable_event_source(source).await?)
        }
        Command::DeliverEvent {
            source,
            key,
            payload,
        } => Response::Event(host.deliver_event(source, &key, &payload).await?),
        Command::InspectEvent { source, key } => Response::Event(host.event(source, &key).await?),
        Command::CancelEvent { source, key } => {
            Response::Event(host.cancel_event(source, &key).await?)
        }
        Command::RecordReview {
            session,
            manifest,
            source_identity,
            disposition,
            body,
        } => Response::ReviewFeedback(
            host.record_review(
                session,
                request.id,
                manifest,
                source_identity,
                disposition,
                body,
            )
            .await?,
        ),
        Command::InspectSessionReview { session } => {
            Response::WorkspaceReview(host.inspect_session_review(session, shutdown).await?)
        }
        Command::HandoffSession { id, parent } => {
            host.handoff_session(id, parent).await?;
            Response::Session(Box::new(host.session_snapshot(id).await?.into()))
        }
        Command::InspectTaskReview { task } => {
            let (view, review) = host.inspect_task_review(task, shutdown).await?;
            Response::TaskReview { view, review }
        }
        Command::MoveSubmission {
            session,
            request: moved,
            expected_input,
            before,
        } => Response::Submission(
            host.move_submission(session, request.id, moved, expected_input, before)
                .await?,
        ),
        Command::ReviewCatalog { workspace } => {
            Response::ReviewCatalog(host.review_catalog(workspace, shutdown).await?)
        }
        Command::InspectWorkspace { workspace, range } => {
            Response::WorkspaceReview(host.inspect_workspace(workspace, range, shutdown).await?)
        }
        Command::ReviewFile {
            manifest,
            side,
            path,
        } => Response::ReviewFile(host.review_file(manifest, side, path).await?),
        Command::ReviewFiles {
            manifest,
            side,
            offset,
            limit,
        } => Response::ReviewFiles(host.review_files(manifest, side, offset, limit).await?),
        Command::Submissions {
            session,
            offset,
            limit,
        } => Response::Submissions(host.submissions(session, offset, limit).await?),
        Command::ReplaceSubmission {
            session,
            request: edited,
            expected_input,
            content,
        } => Response::Submission(
            host.edit_submission(session, request.id, edited, expected_input, Some(content))
                .await?,
        ),
        Command::PromoteSubmission {
            session,
            request: edited,
            expected_input,
        } => Response::Submission(
            host.edit_submission(session, request.id, edited, expected_input, None)
                .await?,
        ),
        Command::Submit {
            session,
            content,
            intent,
        } => Response::Submission(host.submit(session, request.id, content, intent).await?),
        Command::Submission { session, request } => {
            Response::Submission(host.submission(session, request).await?)
        }
        Command::CancelSubmission { session, request } => {
            Response::Submission(host.cancel_submission(session, request).await?)
        }
        Command::LegacySessions {
            database,
            offset,
            limit,
        } => Response::LegacySessions(serde_json::to_value(
            host.legacy_sessions(database, offset, limit).await?,
        )?),
        Command::ImportLegacy {
            database,
            source_session,
            request: admission,
        } => {
            let session = host
                .import_legacy_request(request.id, database, source_session, admission)
                .await?;
            Response::Session(Box::new(host.session_snapshot(session.id).await?.into()))
        }
        Command::LegacyPage {
            session,
            cursor,
            max_records,
            max_bytes,
        } => Response::LegacyPage(
            host.legacy_page(session, cursor, max_records, max_bytes)
                .await?,
        ),
        Command::ExecuteInput {
            session,
            content,
            limits,
            policy,
        } => {
            let Ok(_permit) = runs.try_acquire_owned() else {
                return Ok(Response::Error {
                    message: "host execution capacity is occupied; retry this request ID later"
                        .into(),
                });
            };
            Response::TaskFinished(Box::new(
                host.execute_input(
                    session,
                    request.id,
                    content,
                    limits,
                    policy,
                    shutdown,
                    Arc::new(|_| {}),
                )
                .await?,
            ))
        }
        Command::InspectTask { task } => {
            Response::ArtifactView(host.inspect_artifacts(task, shutdown).await?)
        }
        Command::ReadArtifact {
            digest,
            offset,
            limit,
        } => Response::Artifact(host.read_artifact(digest, offset, limit).await?),
        Command::ArtifactFile { snapshot, path } => {
            Response::Artifact(host.artifact_file(snapshot, path).await?)
        }
        Command::ArtifactTree {
            snapshot,
            offset,
            limit,
        } => Response::Artifact(host.artifact_tree(snapshot, offset, limit).await?),
        Command::RecentInputs {
            limit,
            before,
            workspace,
        } => Response::RecentInputs(host.recent_inputs(limit, before, workspace).await?),
        Command::Info => Response::Info(host.info().await?),
        Command::ShutdownIfIdle => unreachable!("shutdown has a service-level handler"),
        Command::Sessions { offset, limit } => {
            let (sessions, sequence) = host.sessions(offset, limit).await?;
            Response::Sessions(
                sessions
                    .into_iter()
                    .map(|(state, cost)| SessionView::from((state, sequence, cost)))
                    .collect(),
            )
        }
        Command::ForkSession { id, parent } => {
            host.fork_session(id, parent).await?;
            Response::Session(Box::new(host.session_snapshot(id).await?.into()))
        }
        Command::History {
            cursor,
            start,
            limit,
        } => Response::History(host.history_page(cursor, start, limit).await?),
        Command::HistoryText {
            cursor,
            item,
            content_index,
            offset,
            limit,
        } => Response::HistoryText {
            page: host
                .history_text(&cursor, item, content_index, offset, limit)
                .await?,
            cursor,
        },
        Command::Watch { .. } => unreachable!("watch requests have a streaming handler"),
        Command::CreateSession {
            id,
            request: admission,
        } => {
            host.create_session_with_id(id, admission).await?;
            Response::Session(Box::new(host.session_snapshot(id).await?.into()))
        }
        Command::Session { id } => {
            Response::Session(Box::new(host.session_snapshot(id).await?.into()))
        }
        Command::Task { id } => {
            let state = host.task(id).await?;
            Response::Task {
                id,
                revision: state.revision,
                scope_revision: state.scope_revision,
                outcome: state.outcome,
                reason: state.disposition_reason,
                requirements: state
                    .contract
                    .as_ref()
                    .map(|contract| contract.requirements.len()),
                evidence: state.evidence.len(),
                certificate: state
                    .certificates
                    .last()
                    .map(Digest::of_value)
                    .transpose()?,
            }
        }
        Command::RegisterProgram { program } => Response::Program {
            digest: host.register_program(&program).await?,
        },
        Command::Cancel { session } => Response::Cancelled {
            requested: host.cancel(session).await,
        },
        Command::Journal { after, limit } => {
            Response::Journal(host.journal_page(after, limit).await?)
        }
        Command::ExecuteContract {
            session,
            input,
            contract,
        } => {
            let Ok(_permit) = runs.try_acquire_owned() else {
                return Ok(Response::Error {
                    message: "host execution capacity is occupied; retry this request ID later"
                        .into(),
                });
            };
            // The execution future belongs to the server handler, not to the client's socket lifetime.
            Response::TaskFinished(Box::new(
                host.execute_contract_request(
                    session,
                    request.id,
                    input,
                    contract,
                    shutdown,
                    Arc::new(|_| {}),
                )
                .await?,
            ))
        }
        Command::ExecuteRequest {
            session,
            input,
            limits,
            policy,
        } => {
            let Ok(_permit) = runs.try_acquire_owned() else {
                return Ok(Response::Error {
                    message: "host execution capacity is occupied; retry this request ID later"
                        .into(),
                });
            };
            Response::TaskFinished(Box::new(
                host.execute_request(
                    session,
                    request.id,
                    input,
                    limits,
                    policy,
                    shutdown,
                    Arc::new(|_| {}),
                )
                .await?,
            ))
        }
        Command::ResumeTask {
            session,
            task,
            expected_revision,
            reason,
        } => {
            let Ok(_permit) = runs.try_acquire_owned() else {
                return Ok(Response::Error {
                    message: "host execution capacity is occupied; retry this request ID later"
                        .into(),
                });
            };
            Response::TaskFinished(Box::new(
                host.resume_task_request(
                    session,
                    request.id,
                    task,
                    expected_revision,
                    reason,
                    shutdown,
                    Arc::new(|_| {}),
                )
                .await?,
            ))
        }
    })
}

async fn watch(
    stream: &mut UnixStream,
    host: Arc<Host>,
    mut after: u64,
    session: Option<SessionId>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let mut previews = host.subscribe_previews();
    let mut subagents = host.subscribe_subagents();
    let mut warnings = host.subscribe_warnings();
    let mut sent_warnings = None;
    let mut unexpected = [0u8; 1];
    send_watch(stream, &WatchFrame::Ready { after }).await?;
    if let Some(session) = session {
        send_subagent_snapshot(stream, &host, session).await?;
    }
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let current_warnings = warnings
            .borrow_and_update()
            .iter()
            .copied()
            .collect::<Vec<_>>();
        if sent_warnings.as_ref() != Some(&current_warnings) {
            send_watch(
                stream,
                &WatchFrame::Warnings {
                    warnings: current_warnings.clone(),
                },
            )
            .await?;
            sent_warnings = Some(current_warnings);
        }
        let events = host
            .journal_page(after, 16)
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        if !events.is_empty() {
            for record in events {
                let sequence = record.sequence;
                send_watch(stream, &WatchFrame::Journal(record)).await?;
                after = sequence;
            }
            continue;
        }
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            read = stream.read(&mut unexpected) => {
                return match read? {
                    0 => Ok(()),
                    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "watch socket accepts no additional commands")),
                };
            }
            () = tokio::time::sleep(Duration::from_millis(50)) => {},
            changed = warnings.changed() => { if changed.is_err() { return Ok(()); } },
            preview = previews.recv() => match preview {
                Ok(crate::controller::HostUpdate::Provisional { session, request, delta }) => send_watch(stream, &WatchFrame::Preview { session, request, delta }).await?,
                Ok(crate::controller::HostUpdate::PreviewGap { .. }) => send_watch(stream, &WatchFrame::PreviewGap { dropped: 1 }).await?,
                Ok(_) => {},
                Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => send_watch(stream, &WatchFrame::PreviewGap { dropped }).await?,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            },
            subagent_event = subagents.recv() => match subagent_event {
                Ok(event) => {
                    let event_session = subagent_event_session(&event);
                    if session.is_none_or(|session| session == event_session) {
                        send_watch(stream, &WatchFrame::Subagent { session: event_session, event }).await?;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Some(session) = session {
                        send_subagent_snapshot(stream, &host, session).await?;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
            }
        }
    }
}

fn subagent_event_session(event: &crate::controller::SubagentEvent) -> SessionId {
    match event {
        crate::controller::SubagentEvent::Spawned { session, .. }
        | crate::controller::SubagentEvent::Returned { session, .. }
        | crate::controller::SubagentEvent::Unsubmitted { session, .. }
        | crate::controller::SubagentEvent::Failed { session, .. }
        | crate::controller::SubagentEvent::Cancelled { session, .. } => *session,
    }
}

async fn send_subagent_snapshot(
    stream: &mut UnixStream,
    host: &Host,
    session: SessionId,
) -> io::Result<()> {
    for event in host.subagent_snapshot(session).await {
        send_watch(stream, &WatchFrame::Subagent { session, event }).await?;
    }
    Ok(())
}

async fn send_watch(stream: &mut UnixStream, frame: &WatchFrame) -> io::Result<()> {
    timeout(IO_TIMEOUT, write_frame(stream, frame))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "slow spectator disconnected; reconnect with the last journal cursor",
            )
        })?
}

pub async fn subscribe(socket: &Path, after: u64) -> io::Result<UnixStream> {
    let mut stream = timeout(IO_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host connection deadline"))??;
    timeout(
        IO_TIMEOUT,
        write_frame(
            &mut stream,
            &Request::new(Command::Watch {
                after,
                session: None,
            }),
        ),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "watch request deadline"))??;
    Ok(stream)
}

pub async fn call(
    socket: &Path,
    request: &Request,
    response_timeout: Duration,
) -> io::Result<Response> {
    let mut stream = timeout(IO_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host connection deadline"))??;
    write_frame(&mut stream, request).await?;
    timeout(response_timeout, read_frame(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "host response deadline"))?
}

pub async fn read_frame<T: DeserializeOwned>(
    stream: &mut (impl AsyncRead + Unpin),
) -> io::Result<T> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator frame exceeds limit",
        ));
    }
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

pub async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    value: &impl Serialize,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator response exceeds limit; use bounded queries",
        ));
    }
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(&bytes).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference::{
        Limits, ResponsesClient, Route, Transport,
        auth::{Auth, SecretString},
    };

    #[tokio::test]
    async fn previous_protocol_clients_cannot_subscribe_to_warning_frames() {
        let root = tempfile::tempdir().unwrap();
        let provider = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            Limits {
                max_attempts: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let host =
            Arc::new(Host::open_native(root.path(), provider, Digest::of(b"fixture")).unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let stop = CancellationToken::new();
        let serving = tokio::spawn(handle(
            server,
            host,
            Arc::new(Semaphore::new(1)),
            Arc::new(Semaphore::new(1)),
            stop.clone(),
            stop.clone(),
        ));
        let mut request = Request::new(Command::Watch {
            after: 0,
            session: None,
        });
        request.version = PROTOCOL_VERSION - 1;
        write_frame(&mut client, &request).await.unwrap();
        let response: serde_json::Value = read_frame(&mut client).await.unwrap();
        stop.cancel();
        serving.await.unwrap().unwrap();
        assert_eq!(
            response["type"], "error",
            "older clients cannot decode warning frames; reject before watch readiness"
        );
        assert_eq!(response["data"]["message"], UNSUPPORTED_PROTOCOL_VERSION);
    }

    fn diagnostic_host(root: &Path) -> Arc<Host> {
        let provider = ResponsesClient::new(
            Auth::api_key(SecretString::new("fixture".into())).unwrap(),
            Route::new(Transport::Http, "http://127.0.0.1:1/v1/responses").unwrap(),
            Limits {
                max_attempts: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        Arc::new(Host::open_native(root, provider, Digest::of(b"fixture")).unwrap())
    }

    async fn warning_frame(stream: &mut UnixStream, expected: &[HostWarning]) {
        timeout(Duration::from_secs(3), async {
            loop {
                let frame: WatchFrame = read_frame(stream).await.unwrap();
                if let WatchFrame::Warnings { warnings } = frame {
                    if warnings == expected {
                        return;
                    }
                }
            }
        })
        .await
        .expect("host warnings must reach watch clients");
    }

    #[tokio::test]
    async fn diagnostic_global_warnings_reach_session_watches_live_and_on_reconnect() {
        let root = tempfile::tempdir().unwrap();
        let host = diagnostic_host(root.path());
        let db = rusqlite::Connection::open(root.path().join("v1.sqlite3")).unwrap();
        let status: Vec<u8> = db
            .query_row("SELECT record FROM monitor_state", [], |row| row.get(0))
            .unwrap();
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let stop = CancellationToken::new();
        let task_host = host.clone();
        let task_stop = stop.clone();
        let serving = tokio::spawn(async move {
            watch(&mut server, task_host, 0, Some(SessionId::new()), task_stop).await
        });
        warning_frame(&mut client, &[]).await;
        db.execute("DELETE FROM monitor_state", []).unwrap();
        assert!(host.monitor_tick().await.is_err());
        let expected = [
            HostWarning::MonitorUnavailable,
            HostWarning::MonitorStatusUnavailable,
        ];
        warning_frame(&mut client, &expected).await;
        assert!(host.monitor_tick().await.is_err());
        assert_eq!(host.info().await.unwrap().warnings, expected);
        stop.cancel();
        serving.await.unwrap().unwrap();

        let (mut client, mut server) = UnixStream::pair().unwrap();
        let stop = CancellationToken::new();
        let task_host = host.clone();
        let task_stop = stop.clone();
        let serving = tokio::spawn(async move {
            watch(&mut server, task_host, 0, Some(SessionId::new()), task_stop).await
        });
        warning_frame(&mut client, &expected).await;
        db.execute("INSERT INTO monitor_state VALUES(1,?1)", [status])
            .unwrap();
        host.monitor_tick().await.unwrap();
        warning_frame(&mut client, &[]).await;
        assert!(host.info().await.unwrap().warnings.is_empty());
        stop.cancel();
        serving.await.unwrap().unwrap();
        drop(host);
        assert!(
            diagnostic_host(root.path())
                .info()
                .await
                .unwrap()
                .warnings
                .is_empty()
        );
    }
}
