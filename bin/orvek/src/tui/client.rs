//! Terminal command routing and subscriptions. The detached host owns all work.

use super::{
    StartupMode,
    children::{ChildId, ChildStatus, ChildUpdate, ChildView},
    components::{AppEffect, AppEvent, AppNode, DraftReset, StartupScreen},
    event_loop::schedule,
    host_projection::{HostProjection, ViewChange},
    pane::PaneId,
    scheduler::{RenderScheduler, STREAM_FRAME_INTERVAL},
    session,
    terminal::TerminalSession,
    transcript::TranscriptRecord,
};
use crate::{
    app::{
        config::Config,
        error::{Error, Result, RuntimeError},
        host::HostClient,
        submission::{self as submissions, SubmitFailure},
    },
    core::ConfiguredSession,
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use orvek_harness::ipc::{Command, Request, Response, SessionView, WatchFrame};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) struct Pane {
    pub(super) view: SessionView,
    pub(super) client: HostClient,
    pub(super) projection: HostProjection,
    pub(super) generation: u64,
    pub(super) watch: CancellationToken,
    pub(super) handoff: Option<CancellationToken>,
    pub(super) pending: Option<(Request, super::prompt::Submission)>,
    /// Commands dispatched as host shell submissions, keyed by request id so
    /// journal events that carry only ids can still render the command.
    pub(super) shell_commands: HashMap<Uuid, String>,
    pub(super) queue_sequence: u64,
    pub(super) queue_loading: bool,
    pub(super) queue_dirty: bool,
}
pub(super) struct PreparedSession {
    pub(super) configured: ConfiguredSession,
    pub(super) projection: HostProjection,
    pub(super) queue: session::QueueSnapshot,
    pub(super) history: Vec<ViewChange>,
}
pub(super) struct PreparedHandoffSession {
    pub(super) prompt: String,
    pub(super) session: PreparedSession,
}

pub(super) enum Update {
    /// A journal-derived display change that needed an extra host round trip
    /// (for example shell output artifacts) before it could render.
    Enriched {
        pane: PaneId,
        generation: u64,
        change: ViewChange,
    },
    MemoryLoaded {
        pane: PaneId,
        generation: u64,
        access: orvek_memory::MemoryAccess,
        records: Vec<orvek_memory::MemoryRecord>,
    },
    MemoryListFailed {
        pane: PaneId,
        generation: u64,
        source: orvek_memory::MemorySource,
        access: Option<orvek_memory::MemoryAccess>,
        error: String,
    },
    MemoryRemoved {
        pane: PaneId,
        generation: u64,
        key: orvek_memory::MemoryKey,
    },
    MemoryRemoveFailed {
        pane: PaneId,
        generation: u64,
        error: String,
        conflict: bool,
    },
    Handoff {
        pane: PaneId,
        generation: u64,
        // Boxed to keep this variant close in size to the others; a prepared
        // handoff carries a whole configured session.
        result: Box<
            std::result::Result<PreparedHandoffSession, super::handoff_controller::HandoffFailure>,
        >,
    },
    Queue {
        pane: PaneId,
        generation: u64,
        result: Result<session::QueueSnapshot>,
    },
    Submission {
        pane: PaneId,
        generation: u64,
        request: Option<Request>,
        prompt: super::prompt::Submission,
        // Boxed for the same reason as `Handoff`: an acknowledged submission is
        // by far the largest payload this enum carries.
        result: Box<std::result::Result<orvek_harness::submission::Submission, SubmitFailure>>,
    },
    Watch {
        pane: PaneId,
        generation: u64,
        frame: WatchFrame,
    },
    Degraded {
        pane: PaneId,
        generation: u64,
        error: String,
    },
    Recovered {
        pane: PaneId,
        generation: u64,
    },
    Reply {
        pane: PaneId,
        generation: u64,
        result: Result<Response>,
    },
    Review {
        pane: PaneId,
        generation: u64,
        result: std::result::Result<Option<crate::review::ReviewText>, crate::review::ReviewError>,
    },
    ReviewReady {
        pane: PaneId,
        generation: u64,
        url: String,
    },
    Session {
        pane: PaneId,
        generation: u64,
        result: Box<Result<PreparedSession>>,
        replacement: SessionReplacement,
    },
    Event {
        pane: PaneId,
        generation: u64,
        event: AppEvent,
    },
}

#[derive(Clone, Copy)]
pub(super) enum SessionReplacement {
    New(DraftReset),
    Fork,
    Settings,
}

pub(super) struct SuccessorRequest {
    pub(super) config: Config,
    pub(super) workspace: std::path::PathBuf,
    pub(super) model: orvek_harness::inference::ModelSettings,
    pub(super) context_window_tokens: u64,
}

impl SessionReplacement {
    pub(super) const fn draft_reset(self) -> DraftReset {
        match self {
            Self::New(reset) => reset,
            Self::Fork => DraftReset::Clear,
            Self::Settings => DraftReset::Preserve,
        }
    }

    pub(super) const fn persists_settings(self) -> bool {
        matches!(self, Self::Settings)
    }

    pub(super) fn failure_event(self, pane: PaneId, error: String) -> AppEvent {
        match self {
            Self::Fork => AppEvent::ForkFailed { pane, error },
            Self::New(_) | Self::Settings => AppEvent::NewSessionFailed { pane, error },
        }
    }
}

pub(super) async fn prepare_startup(
    config: &Config,
    startup: StartupMode,
    shutdown: &CancellationToken,
    terminal: &mut TerminalSession,
    input: &mut EventStream,
) -> Result<Option<PreparedSession>> {
    let message = match &startup {
        StartupMode::NewSession(_) => "Starting a new session",
        StartupMode::ResumeSession(_) => "Restoring session",
        StartupMode::ResumeSelector(_) => "Loading sessions",
    };
    let load = async {
        let configured = match startup {
            StartupMode::NewSession(model) | StartupMode::ResumeSelector(model) => {
                ConfiguredSession::create(
                    config,
                    config.agent().thinking(),
                    config.agent().reasoning_mode(),
                    model,
                )
                .await?
            }
            StartupMode::ResumeSession(id) => ConfiguredSession::resume_label(config, &id).await?,
        };
        prepare_install(configured).await
    };
    tokio::pin!(load);

    let mut screen = StartupScreen::new(Instant::now(), message);
    let theme = config.theme().clone();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    loop {
        let now = Instant::now();
        if scheduler.is_due(now) {
            terminal
                .draw(|frame| screen.render(frame, frame.area(), &theme))
                .map_err(RuntimeError::Terminal)?;
            scheduler.presented(now);
        }
        let deadline = scheduler.deadline().map_or_else(
            || screen.animation_deadline(),
            |scheduled| scheduled.min(screen.animation_deadline()),
        );
        tokio::select! {
            result = &mut load => return result.map(Some),
            () = shutdown.cancelled() => return Ok(None),
            Some(event) = input.next() => {
                let event = event.map_err(RuntimeError::Terminal)?;
                if matches!(
                    event,
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press
                            && key.code == KeyCode::Char('c')
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                ) {
                    return Ok(None);
                }
                if matches!(event, Event::Resize(_, _)) {
                    scheduler.request_immediate(Instant::now());
                }
            }
            () = tokio::time::sleep_until(deadline.into()) => {
                let now = Instant::now();
                if screen.advance(now) {
                    scheduler.request_streaming(now);
                }
            }
        }
    }
}

pub(super) async fn prepare_install(configured: ConfiguredSession) -> Result<PreparedSession> {
    let mut projection = HostProjection::at_snapshot(&configured.session);
    let queue = session::queued(&configured.client, configured.session.id).await?;
    if let Some((request, visible)) = queue.active_auxiliary {
        projection.classify_auxiliary(request, visible);
    }
    let history = session::history(&configured.client, &configured.session).await?;
    Ok(PreparedSession {
        configured,
        projection,
        queue,
        history,
    })
}

pub(super) fn install(
    pane: PaneId,
    generation: u64,
    prepared: PreparedSession,
    panes: &mut HashMap<PaneId, Pane>,
    app: &mut AppNode,
    sender: &mpsc::Sender<Update>,
) {
    let PreparedSession {
        configured,
        projection,
        queue,
        history,
    } = prepared;
    if let Some(old) = panes.remove(&pane) {
        old.watch.cancel();
    }
    let queue_sequence = queue.sequence;
    app.update(AppEvent::QueueChanged {
        pane,
        inputs: queue.inputs,
    });
    let cursor = projection.cursor();
    app.update(AppEvent::SessionCost {
        pane,
        cost: crate::tui::context::SessionCost {
            total: configured.session.cost_usd,
            uncertain: configured.session.cost_uncertain,
        },
    });
    for change in history {
        app.update(AppEvent::Transcript {
            pane,
            record: Arc::new(TranscriptRecord::from_host(
                configured.session.journal_sequence,
                0,
                cursor.clone(),
                change,
            )),
        });
    }
    if let Some(request) = configured
        .session
        .active_request
        .filter(|request| projection.visible_request(*request))
    {
        app.update(AppEvent::Transcript {
            pane,
            record: Arc::new(TranscriptRecord::from_host(
                configured.session.journal_sequence,
                0,
                cursor,
                ViewChange::RequestStarted { request },
            )),
        });
    }
    let cancel = CancellationToken::new();
    watch(
        pane,
        generation,
        configured.client.clone(),
        configured.session.id,
        configured.session.journal_sequence,
        cancel.clone(),
        sender.clone(),
    );
    panes.insert(
        pane,
        Pane {
            view: configured.session,
            client: configured.client,
            projection,
            generation,
            watch: cancel,
            handoff: None,
            pending: None,
            shell_commands: HashMap::new(),
            queue_sequence,
            queue_loading: false,
            queue_dirty: false,
        },
    );
}

pub(super) fn watch(
    pane: PaneId,
    generation: u64,
    client: HostClient,
    session: orvek_harness::session::SessionId,
    mut after: u64,
    cancel: CancellationToken,
    sender: mpsc::Sender<Update>,
) {
    tokio::spawn(async move {
        let mut failures = 0u32;
        let mut degraded = false;
        loop {
            let result = tokio::select! {
                () = cancel.cancelled() => return,
                result = client.subscribe(after, session) => result,
            };
            let error = match result {
                Ok(mut watch) => {
                    failures = 0;
                    let through = watch
                        .through()
                        .expect("subscribe validates watch readiness");
                    if degraded && after == through {
                        if !send_watch(&sender, &cancel, Update::Recovered { pane, generation })
                            .await
                        {
                            return;
                        }
                        degraded = false;
                    }
                    loop {
                        let result = tokio::select! {
                            () = cancel.cancelled() => return,
                            result = watch.next() => result,
                        };
                        match result {
                            Ok(frame) => {
                                after = watch.cursor();
                                if !send_watch(
                                    &sender,
                                    &cancel,
                                    Update::Watch {
                                        pane,
                                        generation,
                                        frame,
                                    },
                                )
                                .await
                                {
                                    return;
                                }
                                if degraded && after >= through {
                                    if !send_watch(
                                        &sender,
                                        &cancel,
                                        Update::Recovered { pane, generation },
                                    )
                                    .await
                                    {
                                        return;
                                    }
                                    degraded = false;
                                }
                            }
                            Err(error) => break error,
                        }
                    }
                }
                Err(error) => error,
            };
            failures = failures.saturating_add(1);
            if !degraded {
                degraded = true;
                if !send_watch(
                    &sender,
                    &cancel,
                    Update::Degraded {
                        pane,
                        generation,
                        error: format!("Reconnecting view: {error}"),
                    },
                )
                .await
                {
                    return;
                }
            }
            let exponent = failures.saturating_sub(1).min(5);
            let delay = Duration::from_millis(100 * (1u64 << exponent));
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
        }
    });
}
pub(super) async fn send_watch(
    sender: &mpsc::Sender<Update>,
    cancel: &CancellationToken,
    update: Update,
) -> bool {
    tokio::select! {()=cancel.cancelled()=>false,result=sender.send(update)=>result.is_ok()}
}

pub(super) fn dispatch(
    jobs: &mut JoinSet<()>,
    sender: &mpsc::Sender<Update>,
    pane: PaneId,
    generation: u64,
    client: HostClient,
    request: Request,
    deadline: Duration,
) {
    let sender = sender.clone();
    jobs.spawn(async move {
        let result = client.call(&request, deadline).await;
        let _ = sender
            .send(Update::Reply {
                pane,
                generation,
                result,
            })
            .await;
    });
}

pub(super) fn spawn_successor(
    jobs: &mut JoinSet<()>,
    sender: &mpsc::Sender<Update>,
    pane: PaneId,
    generation: u64,
    request: SuccessorRequest,
    replacement: SessionReplacement,
) {
    let sender = sender.clone();
    jobs.spawn(async move {
        let result = async {
            let configured = ConfiguredSession::create_successor(
                &request.config,
                &request.workspace,
                request.model,
                request.context_window_tokens,
            )
            .await?;
            prepare_install(configured).await
        }
        .await;
        let _ = sender
            .send(Update::Session {
                pane,
                generation,
                result: Box::new(result),
                replacement,
            })
            .await;
    });
}

pub(super) fn reject_session_replacement(
    replacement: SessionReplacement,
    pane: PaneId,
    error: String,
    previous_settings: Option<(
        orvek_harness::inference::ModelSettings,
        crate::app::config::ReasoningMode,
    )>,
    app: &mut AppNode,
    scheduler: &mut RenderScheduler,
    effects: &mut VecDeque<AppEffect>,
) {
    if let Some((model, preferred)) = previous_settings {
        schedule(
            app.update(AppEvent::SettingsConfirmed {
                pane,
                model,
                preferred,
            }),
            scheduler,
            effects,
        );
    }
    schedule(
        app.update(replacement.failure_event(pane, error)),
        scheduler,
        effects,
    );
}

/// Resolve recorded invocation input through the authenticated host, not its store.
pub(super) async fn task_input(
    client: &HostClient,
    invocation: &orvek_harness::state::JobInvocation,
) -> std::result::Result<serde_json::Value, String> {
    use base64::Engine as _;
    const MAX_INPUT_BYTES: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    loop {
        let Response::Artifact(page) = client
            .query(Command::ReadArtifact {
                digest: invocation.input,
                offset: bytes.len(),
                limit: 64 * 1024,
            })
            .await
            .map_err(|error| error.to_string())?
        else {
            return Err("host returned an unexpected artifact response".into());
        };
        let total = page["bytes"].as_u64().ok_or("artifact length is missing")?;
        if total > MAX_INPUT_BYTES as u64 {
            return Err("invocation input exceeds the 1 MiB display limit".into());
        }
        let data = page["data"].as_str().ok_or("artifact data is missing")?;
        let chunk = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|error| format!("invalid artifact encoding: {error}"))?;
        if chunk.is_empty() && bytes.len() < total as usize {
            return Err("artifact download made no progress".into());
        }
        bytes.extend(chunk);
        if bytes.len() == total as usize {
            return decode_task_input(&bytes, &invocation.capability);
        }
        if bytes.len() > total as usize {
            return Err("artifact length does not match its data".into());
        }
    }
}

pub(super) fn decode_task_input(
    bytes: &[u8],
    capability: &str,
) -> std::result::Result<serde_json::Value, String> {
    #[derive(serde::Deserialize)]
    struct Input {
        name: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    }
    let input: Input = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid invocation input: {error}"))?;
    if input.name != capability {
        return Err("input capability does not match the recorded invocation".into());
    }
    Ok(serde_json::Value::Object(input.arguments))
}

#[allow(clippy::too_many_arguments)]
/// Renders a published shell run for the transcript. The report artifact
/// carries the execution status and stdout/stderr artifact digests; both
/// streams are fetched in bounded chunks and truncated for display.
pub(super) async fn shell_output_text(
    client: &crate::app::host::HostClient,
    report: orvek_harness::Digest,
) -> Option<String> {
    use base64::Engine as _;
    let engine = &base64::engine::general_purpose::STANDARD;

    async fn artifact(
        client: &crate::app::host::HostClient,
        engine: &base64::engine::GeneralPurpose,
        digest: orvek_harness::Digest,
        limit: usize,
    ) -> Option<String> {
        let Response::Artifact(page) = client
            .query(Command::ReadArtifact {
                digest,
                offset: 0,
                limit,
            })
            .await
            .ok()?
        else {
            return None;
        };
        let data = page.get("data")?.as_str()?;
        let bytes = engine.decode(data).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
    let report_text = artifact(client, engine, report, 64 * 1024).await?;
    let parsed: serde_json::Value = serde_json::from_str(&report_text).ok()?;
    let digest_of = |value: &serde_json::Value| {
        value
            .as_str()
            .and_then(|text| text.strip_prefix("sha256:").unwrap_or(text).parse().ok())
    };
    let mut parts = Vec::new();
    if let Some(status) = parsed.get("status").and_then(|value| value.as_str()) {
        parts.push(format!("status: {status}"));
    }
    if let Some(error) = parsed.get("error").and_then(|value| value.as_str()) {
        parts.push(format!("error: {error}"));
    }
    for stream in ["stdout", "stderr"] {
        let Some(digest) = parsed.get(stream).and_then(digest_of) else {
            continue;
        };
        if let Some(text) = artifact(client, engine, digest, 16 * 1024).await
            && !text.trim().is_empty()
        {
            parts.push(format!("{stream}:\n{}", text.trim_end()));
        }
    }
    Some(parts.join("\n\n"))
}

/// One outbound turn: who submits it, what the user typed, and an optional
/// pre-built request for intents the client constructs itself.
pub(super) struct SubmissionJob {
    pub(super) client: HostClient,
    pub(super) session: orvek_harness::session::SessionId,
    pub(super) prompt: super::prompt::Submission,
    pub(super) existing: Option<Request>,
}

/// Maps a host subagent lifecycle event onto the TUI child tree contract.
/// Schema-valid results are referenced by digest, never inlined.
pub(super) fn subagent_update(
    event: &orvek_harness::controller::SubagentEvent,
) -> Option<ChildUpdate> {
    use orvek_harness::controller::SubagentEvent;
    match event {
        SubagentEvent::Spawned {
            session,
            agent,
            parent,
            role,
            task,
            model,
            ..
        } => {
            let model = model.parse().ok()?;
            Some(ChildUpdate::Added(ChildView {
                id: ChildId(*agent),
                session_id: session.to_string(),
                model,
                role: role.clone(),
                task: task.clone(),
                parent: parent.map(ChildId),
            }))
        }
        SubagentEvent::Returned { agent, output, .. } => Some(ChildUpdate::Status {
            id: ChildId(*agent),
            status: ChildStatus::Returned { output: *output },
        }),
        SubagentEvent::Unsubmitted {
            agent, diagnostic, ..
        } => Some(ChildUpdate::Status {
            id: ChildId(*agent),
            status: ChildStatus::Failed {
                error: format!("unsubmitted: {diagnostic}"),
            },
        }),
        SubagentEvent::Failed { agent, error, .. } => Some(ChildUpdate::Status {
            id: ChildId(*agent),
            status: ChildStatus::Failed {
                error: error.clone(),
            },
        }),
        SubagentEvent::Cancelled { agent, .. } => Some(ChildUpdate::Status {
            id: ChildId(*agent),
            status: ChildStatus::Cancelled,
        }),
    }
}

/// Resolves the command text of a shell submission from the durable record,
/// so journal replay after a restart renders shells without in-process state.
pub(super) async fn submission_command(
    client: &crate::app::host::HostClient,
    session: orvek_harness::session::SessionId,
    request: Uuid,
) -> Option<String> {
    use base64::Engine as _;
    let engine = &base64::engine::general_purpose::STANDARD;
    let Response::Submission(submission) = client
        .query(Command::Submission { session, request })
        .await
        .ok()?
    else {
        return None;
    };
    let Response::Artifact(page) = client
        .query(Command::ReadArtifact {
            digest: submission.input,
            offset: 0,
            limit: 64 * 1024,
        })
        .await
        .ok()?
    else {
        return None;
    };
    let data = page.get("data")?.as_str()?;
    let bytes = engine.decode(data).ok()?;
    let messages: Vec<serde_json::Value> = serde_json::from_slice(&bytes).ok()?;
    messages
        .iter()
        .find_map(|message| {
            message.get("content")?.as_array()?.iter().find_map(|part| {
                (part.get("type")? == "input_text")
                    .then(|| part.get("text")?.as_str().map(str::to_owned))
                    .flatten()
            })
        })
        .filter(|text| !text.trim().is_empty())
}

/// One bounded line describing recorded review feedback.
pub(super) async fn review_feedback_text(
    client: &crate::app::host::HostClient,
    feedback: orvek_harness::Digest,
) -> Option<String> {
    use base64::Engine as _;
    let engine = &base64::engine::general_purpose::STANDARD;
    let Response::Artifact(page) = client
        .query(Command::ReadArtifact {
            digest: feedback,
            offset: 0,
            limit: 8 * 1024,
        })
        .await
        .ok()?
    else {
        return None;
    };
    let data = page.get("data")?.as_str()?;
    let bytes = engine.decode(data).ok()?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let disposition = parsed
        .get("disposition")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let first_line = parsed
        .get("body")
        .and_then(|value| value.as_str())
        .and_then(|body| body.lines().find(|line| !line.trim().is_empty()))
        .unwrap_or("");
    Some(format!("{disposition} · {first_line}"))
}

pub(super) fn is_image_paste_shortcut(event: &Event) -> bool {
    matches!(event, Event::Key(key)
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('v')
            && key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER))
}

/// The recovery hint belongs to the terminal, so host errors must not repeat it.
pub(super) fn submission_notice(failure: &SubmitFailure) -> String {
    let hint = if failure.uncertain {
        "Enter retries the same request"
    } else {
        "Draft retained"
    };
    format!("{} · {hint}", failure.error)
}

pub(super) fn dispatch_submission(
    jobs: &mut JoinSet<()>,
    sender: &mpsc::Sender<Update>,
    pane: PaneId,
    generation: u64,
    submission: SubmissionJob,
) {
    let sender = sender.clone();
    let SubmissionJob {
        client,
        session,
        prompt,
        existing,
    } = submission;
    jobs.spawn(async move {
        let request = match existing {
            Some(request) => Ok(request),
            None => submissions::intent(&client, session).await.map(|intent| {
                Request::new(Command::Submit {
                    session,
                    content: prompt.host_content(),
                    intent,
                })
            }),
        };
        let (request, result) = match request {
            Ok(request) => {
                let result = submissions::acknowledge(&client, &request).await;
                (Some(request), result)
            }
            Err(error) => (
                None,
                Err(SubmitFailure {
                    uncertain: false,
                    error: error.into(),
                }),
            ),
        };
        let _ = sender
            .send(Update::Submission {
                pane,
                generation,
                request,
                prompt,
                result: Box::new(result),
            })
            .await;
    });
}

pub(super) fn refresh_queue(
    jobs: &mut JoinSet<()>,
    sender: &mpsc::Sender<Update>,
    pane: PaneId,
    current: &mut Pane,
) {
    if current.queue_loading {
        current.queue_dirty = true;
        return;
    }
    current.queue_loading = true;
    current.queue_dirty = false;
    let client = current.client.clone();
    let session = current.view.id;
    let generation = current.generation;
    let sender = sender.clone();
    jobs.spawn(async move {
        let result =
            match tokio::time::timeout(Duration::from_secs(20), session::queued(&client, session))
                .await
            {
                Ok(result) => result.map_err(Error::from),
                Err(_) => Err(Error::HostRequest(
                    "queue refresh timed out; its host work continues".into(),
                )),
            };
        let _ = sender
            .send(Update::Queue {
                pane,
                generation,
                result,
            })
            .await;
    });
}

#[cfg(test)]
mod image_paste_shortcut_tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState};

    fn key(modifiers: KeyModifiers, kind: KeyEventKind) -> Event {
        Event::Key(KeyEvent {
            code: KeyCode::Char('v'),
            modifiers,
            kind,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn accepts_control_and_command_image_paste_shortcuts() {
        assert!(is_image_paste_shortcut(&key(
            KeyModifiers::CONTROL,
            KeyEventKind::Press
        )));
        assert!(is_image_paste_shortcut(&key(
            KeyModifiers::SUPER,
            KeyEventKind::Press
        )));
        assert!(!is_image_paste_shortcut(&key(
            KeyModifiers::NONE,
            KeyEventKind::Press
        )));
        assert!(!is_image_paste_shortcut(&key(
            KeyModifiers::SUPER,
            KeyEventKind::Release
        )));
    }
}

#[cfg(test)]
#[path = "client_task_input_tests.rs"]
mod task_input_tests;
