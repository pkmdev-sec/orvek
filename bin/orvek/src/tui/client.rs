//! Terminal command routing and subscriptions. The detached host owns all work.

use super::{
    StartupMode,
    children::{ChildId, ChildStatus, ChildUpdate, ChildView},
    components::{
        AppEffect, AppEvent, AppNode, ComponentUpdate, DraftReset, RenderRequest, RootEffect,
        RootNode,
    },
    editor::{self, EditorOutcome},
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
        herdr::Reporter,
        host::HostClient,
        submission::{self as submissions, SubmitFailure},
    },
    core::ConfiguredSession,
};
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use orvek_harness::ipc::{Command, Request, Response, SessionView, WatchFrame};
use orvek_memory::MemoryStore;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{sync::mpsc, task::JoinSet};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Pane {
    view: SessionView,
    client: HostClient,
    projection: HostProjection,
    generation: u64,
    watch: CancellationToken,
    handoff: Option<CancellationToken>,
    pending: Option<(Request, super::prompt::Submission)>,
    /// Commands dispatched as host shell submissions, keyed by request id so
    /// journal events that carry only ids can still render the command.
    shell_commands: HashMap<Uuid, String>,
    queue_sequence: u64,
    queue_loading: bool,
    queue_dirty: bool,
}
enum Update {
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
            std::result::Result<
                super::handoff_controller::PreparedHandoff,
                super::handoff_controller::HandoffFailure,
            >,
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
    Disconnected {
        pane: PaneId,
        generation: u64,
        error: String,
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
        result: Box<Result<ConfiguredSession>>,
        replacement: SessionReplacement,
    },
    Event {
        pane: PaneId,
        generation: u64,
        event: AppEvent,
    },
}

#[derive(Clone, Copy)]
enum SessionReplacement {
    New(DraftReset),
    Settings,
}

struct SuccessorRequest {
    config: Config,
    workspace: std::path::PathBuf,
    model: orvek_harness::inference::ModelSettings,
    context_window_tokens: u64,
}

impl SessionReplacement {
    const fn draft_reset(self) -> DraftReset {
        match self {
            Self::New(reset) => reset,
            Self::Settings => DraftReset::Preserve,
        }
    }

    const fn persists_settings(self) -> bool {
        matches!(self, Self::Settings)
    }
}

pub(super) async fn run(
    mut config: Config,
    startup: StartupMode,
    shutdown: CancellationToken,
) -> Result<Option<String>> {
    let selector = matches!(startup, StartupMode::ResumeSelector(_));
    let configured = match startup {
        StartupMode::NewSession(model) | StartupMode::ResumeSelector(model) => {
            ConfiguredSession::create(
                &config,
                config.agent().thinking(),
                config.agent().reasoning_mode(),
                model,
            )
            .await?
        }
        StartupMode::ResumeSession(id) => ConfiguredSession::resume_label(&config, &id).await?,
    };
    let workspace = configured.session.workspace.clone();
    let mut root = RootNode::new(&workspace, configured.session.model.thinking.into());
    root.set_model(configured.session.model.model);
    root.set_fast_mode(configured.session.model.fast_mode);
    root.set_context_window_tokens(configured.session.context_window_tokens);
    root.set_reasoning_modes(
        configured.session.model.reasoning_mode.into(),
        config.agent().reasoning_mode(),
    );
    root.set_skills(configured.skills.clone());
    root.set_memory_enabled(configured.memory_enabled);
    let mut app = AppNode::new(config.theme().clone(), workspace.clone(), root);
    let (sender, mut updates) = mpsc::channel(128);
    // `ThemeMode::Auto` is the default and resolves through the theme's system
    // scheme, so without this watcher the "follow the operating system" mode
    // never actually follows anything.
    let (schemes, mut system_schemes) = mpsc::unbounded_channel();
    super::theme::watch_system_scheme(schemes, shutdown.clone());
    let mut jobs = JoinSet::new();
    {
        // Runs in the background and fails silently: a network or registry
        // outage must never disturb the terminal.
        let out = sender.clone();
        jobs.spawn(async move {
            if let Ok(Some(version)) = crate::app::update::check_for_update().await {
                let _ = out
                    .send(Update::Event {
                        pane: PaneId::Main,
                        generation: 0,
                        event: AppEvent::UpdateAvailable(version),
                    })
                    .await;
            }
        });
    }
    let mut panes = HashMap::new();
    // Inert unless the operator sets the HERDR_* variables. A resumed session
    // can already have a request in flight, and `from_env` starts out idle, so
    // correct that before reporting anything else.
    let mut herdr = Reporter::from_env(&configured.session.id.to_string());
    if configured.session.active_request.is_some() {
        herdr.working(None);
    }
    install(PaneId::Main, 0, configured, &mut panes, &mut app, &sender).await?;
    let mut expected_generations = HashMap::from([(PaneId::Main, 0u64)]);
    let mut next_generation = 1u64;
    let mut effects = VecDeque::new();
    if selector {
        effects.extend(app.open_resume_selector().effects);
    }
    let mut terminal = TerminalSession::enter().map_err(RuntimeError::Terminal)?;
    terminal
        .report_working_directory(&workspace)
        .map_err(RuntimeError::Terminal)?;
    let mut input = EventStream::new();
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut exiting = false;
    while !exiting && !shutdown.is_cancelled() {
        while let Some(effect) = effects.pop_front() {
            match effect {
                AppEffect::Shutdown => {
                    exiting = true;
                    break;
                }
                AppEffect::ClosePane(pane) => {
                    expected_generations.remove(&pane);
                    if let Some(old) = panes.remove(&pane) {
                        old.watch.cancel();
                    }
                }
                AppEffect::SetTheme(mode) => {
                    config.persist_theme_mode(mode)?;
                }
                AppEffect::SetMaxSubagents(limit) => {
                    config.persist_max_subagents(limit)?;
                    config.set_max_subagents(limit);
                }
                AppEffect::OpenFork { pane, parent } => {
                    let generation = next_generation;
                    next_generation = next_generation.saturating_add(1);
                    expected_generations.insert(pane, generation);
                    if let Some(parent) = panes.get(&parent) {
                        let parent_id = parent.view.id;
                        let client = parent.client.clone();
                        let config = config.clone();
                        let out = sender.clone();
                        jobs.spawn(async move {
                            let result = async {
                                let parent = session::view(&client, parent_id).await?;
                                let Response::Session(view) = client
                                    .query(Command::ForkSession {
                                        id: Default::default(),
                                        parent: parent.fork_cursor,
                                    })
                                    .await?
                                else {
                                    return Err(Error::HostRequest("unexpected fork reply".into()));
                                };
                                ConfiguredSession::resume(&config, view.id).await
                            }
                            .await;
                            let _ = out
                                .send(Update::Session {
                                    pane,
                                    generation,
                                    result: Box::new(result),
                                    replacement: SessionReplacement::New(DraftReset::Clear),
                                })
                                .await;
                        });
                    }
                }
                AppEffect::Pane { pane, effect } => {
                    let Some(current) = panes.get(&pane) else {
                        continue;
                    };
                    let client = current.client.clone();
                    let session = current.view.id;
                    let generation = current.generation;
                    match effect {
                        RootEffect::Submit(prompt) => {
                            dispatch_submission(
                                &mut jobs,
                                &sender,
                                pane,
                                generation,
                                SubmissionJob {
                                    client,
                                    session,
                                    prompt,
                                    existing: None,
                                },
                            );
                        }
                        RootEffect::RunShell(command) => {
                            let prompt = super::prompt::Submission::from(command.clone());
                            let spec = orvek_harness::manual::ShellSpec {
                                command,
                                expected_task: None,
                                scope_revision: None,
                                timeout_ms: 120_000,
                                output_bytes: 256 * 1024,
                            };
                            let request = Request::new(Command::Submit {
                                session,
                                content: prompt.host_content(),
                                intent: orvek_harness::submission::SubmitIntent::Shell { spec },
                            });
                            if let Some(current) = panes.get_mut(&pane) {
                                current
                                    .shell_commands
                                    .insert(request.id, prompt.display_text().to_owned());
                            }
                            dispatch_submission(
                                &mut jobs,
                                &sender,
                                pane,
                                generation,
                                SubmissionJob {
                                    client,
                                    session,
                                    prompt,
                                    existing: Some(request),
                                },
                            );
                        }
                        RootEffect::LoadMemories => {
                            let workspace = current.view.workspace.clone();
                            let config = config.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let event = match crate::core::configured_memory_store(
                                    &config,
                                    &workspace,
                                ) {
                                    Ok(Some(store)) => match store.access().await {
                                        Ok(access) => match MemoryStore::list(&store).await {
                                            Ok(records) => Update::MemoryLoaded {
                                                pane,
                                                generation,
                                                access,
                                                records,
                                            },
                                            Err(error) => Update::MemoryListFailed {
                                                pane,
                                                generation,
                                                source: store.source(),
                                                access: None,
                                                error: error.to_string(),
                                            },
                                        },
                                        Err(error) => Update::MemoryListFailed {
                                            pane,
                                            generation,
                                            source: store.source(),
                                            access: None,
                                            error: error.to_string(),
                                        },
                                    },
                                    Ok(None) => Update::MemoryListFailed {
                                        pane,
                                        generation,
                                        source: orvek_memory::MemorySource::Local,
                                        access: None,
                                        error: "Memory is disabled. Enable it with memory.enabled = true."
                                            .to_owned(),
                                    },
                                    Err(error) => Update::MemoryListFailed {
                                        pane,
                                        generation,
                                        source: orvek_memory::MemorySource::Local,
                                        access: None,
                                        error: error.to_string(),
                                    },
                                };
                                let _ = out.send(event).await;
                            });
                        }
                        RootEffect::DeleteMemory(key) => {
                            let workspace = current.view.workspace.clone();
                            let config = config.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let event =
                                    match crate::core::configured_memory_store(&config, &workspace)
                                    {
                                        Ok(Some(store)) => {
                                            let result =
                                                MemoryStore::delete(&store, key.clone()).await;
                                            match result {
                                                Ok(()) => Update::MemoryRemoved {
                                                    pane,
                                                    generation,
                                                    key,
                                                },
                                                Err(error) => Update::MemoryRemoveFailed {
                                                    pane,
                                                    generation,
                                                    conflict: matches!(
                                                        error,
                                                        orvek_memory::MemoryError::Conflict
                                                    ),
                                                    error: error.to_string(),
                                                },
                                            }
                                        }
                                        Ok(None) => Update::MemoryRemoveFailed {
                                            pane,
                                            generation,
                                            conflict: false,
                                            error:
                                                "Memory was disabled before the deletion completed."
                                                    .to_owned(),
                                        },
                                        Err(error) => Update::MemoryRemoveFailed {
                                            pane,
                                            generation,
                                            conflict: false,
                                            error: error.to_string(),
                                        },
                                    };
                                let _ = out.send(event).await;
                            });
                        }
                        RootEffect::Handoff => {
                            let cancellation = CancellationToken::new();
                            if let Some(current) = panes.get_mut(&pane) {
                                current.handoff = Some(cancellation.clone());
                            }
                            let config = config.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let result = super::handoff_controller::prepare(
                                    &config,
                                    &client,
                                    session,
                                    &cancellation,
                                )
                                .await;
                                let _ = out
                                    .send(Update::Handoff {
                                        pane,
                                        generation,
                                        result: Box::new(result),
                                    })
                                    .await;
                            });
                        }
                        RootEffect::CancelHandoff => {
                            if let Some(cancellation) = &current.handoff {
                                cancellation.cancel();
                            }
                        }
                        RootEffect::Reflect(prompt) => {
                            let spec = crate::app::auxiliary::spec(
                                orvek_harness::auxiliary::AuxiliaryKind::Reflection,
                                orvek_harness::auxiliary::AuxiliaryContext::CurrentConversation,
                                None,
                            );
                            let request = Request::new(Command::Submit {
                                session,
                                content: prompt.host_content(),
                                intent: orvek_harness::submission::SubmitIntent::Auxiliary { spec },
                            });
                            dispatch_submission(
                                &mut jobs,
                                &sender,
                                pane,
                                generation,
                                SubmissionJob {
                                    client,
                                    session,
                                    prompt,
                                    existing: Some(request),
                                },
                            );
                        }
                        RootEffect::RetrySubmission => {
                            if let Some((request, prompt)) = &current.pending {
                                dispatch_submission(
                                    &mut jobs,
                                    &sender,
                                    pane,
                                    generation,
                                    SubmissionJob {
                                        client,
                                        session,
                                        prompt: prompt.clone(),
                                        existing: Some(request.clone()),
                                    },
                                );
                            }
                        }
                        RootEffect::Steer { id, expected_input } => dispatch(
                            &mut jobs,
                            &sender,
                            pane,
                            generation,
                            client,
                            Request::new(Command::PromoteSubmission {
                                session,
                                request: id.0,
                                expected_input,
                            }),
                            Duration::from_secs(10),
                        ),
                        RootEffect::ReplaceQueued {
                            id,
                            expected_input,
                            prompt,
                        } => dispatch(
                            &mut jobs,
                            &sender,
                            pane,
                            generation,
                            client,
                            Request::new(Command::ReplaceSubmission {
                                session,
                                request: id.0,
                                expected_input,
                                content: prompt.host_content(),
                            }),
                            Duration::from_secs(10),
                        ),
                        RootEffect::RemoveQueued { id } => dispatch(
                            &mut jobs,
                            &sender,
                            pane,
                            generation,
                            client,
                            Request::new(Command::CancelSubmission {
                                session,
                                request: id.0,
                            }),
                            Duration::from_secs(10),
                        ),
                        RootEffect::MoveQueued {
                            id,
                            expected_input,
                            before,
                        } => dispatch(
                            &mut jobs,
                            &sender,
                            pane,
                            generation,
                            client,
                            Request::new(Command::MoveSubmission {
                                session,
                                request: id.0,
                                expected_input,
                                before: before.map(|id| id.0),
                            }),
                            Duration::from_secs(10),
                        ),
                        RootEffect::EditQueued { id, expected_input } => {
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let result = async {
                                    let parts = submissions::input_parts(
                                        &client,
                                        expected_input,
                                        true,
                                        &CancellationToken::new(),
                                    )
                                    .await?;
                                    super::prompt::Submission::from_host_content(parts)
                                        .map_err(|message| Error::HostRequest(message.into()))
                                }
                                .await;
                                let event = match result {
                                    Ok(prompt) => AppEvent::QueueEditReady {
                                        pane,
                                        id,
                                        expected_input,
                                        prompt,
                                    },
                                    Err(error) => AppEvent::NotifyError {
                                        pane,
                                        error: error.to_string(),
                                    },
                                };
                                let _ = out
                                    .send(Update::Event {
                                        pane,
                                        generation,
                                        event,
                                    })
                                    .await;
                            });
                        }
                        RootEffect::CancelTurns => dispatch(
                            &mut jobs,
                            &sender,
                            pane,
                            generation,
                            client,
                            Request::new(Command::Cancel { session }),
                            Duration::from_secs(10),
                        ),
                        RootEffect::SetEffort {
                            effort,
                            reasoning_mode,
                        } => {
                            let mut model = current.view.model;
                            model.thinking = effort.into();
                            model.reasoning_mode = reasoning_mode.into();
                            let replacement = next_generation;
                            next_generation = next_generation.saturating_add(1);
                            expected_generations.insert(pane, replacement);
                            spawn_successor(
                                &mut jobs,
                                &sender,
                                pane,
                                replacement,
                                SuccessorRequest {
                                    config: config.clone(),
                                    workspace: current.view.workspace.clone(),
                                    model,
                                    context_window_tokens: current.view.context_window_tokens,
                                },
                                SessionReplacement::Settings,
                            );
                        }
                        RootEffect::SetFastMode(enabled) => {
                            let mut model = current.view.model;
                            model.fast_mode = enabled;
                            let replacement = next_generation;
                            next_generation = next_generation.saturating_add(1);
                            expected_generations.insert(pane, replacement);
                            spawn_successor(
                                &mut jobs,
                                &sender,
                                pane,
                                replacement,
                                SuccessorRequest {
                                    config: config.clone(),
                                    workspace: current.view.workspace.clone(),
                                    model,
                                    context_window_tokens: current.view.context_window_tokens,
                                },
                                SessionReplacement::Settings,
                            );
                        }
                        RootEffect::NewSession(model) => {
                            let replacement = next_generation;
                            next_generation = next_generation.saturating_add(1);
                            expected_generations.insert(pane, replacement);
                            let config = config.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let result = ConfiguredSession::create(
                                    &config,
                                    config.agent().thinking(),
                                    config.agent().reasoning_mode(),
                                    model,
                                )
                                .await;
                                let _ = out
                                    .send(Update::Session {
                                        pane,
                                        generation: replacement,
                                        result: Box::new(result),
                                        replacement: SessionReplacement::New(DraftReset::Clear),
                                    })
                                    .await;
                            });
                        }
                        RootEffect::SetModel(selected) => {
                            let mut model = current.view.model;
                            model.model = selected;
                            let replacement = next_generation;
                            next_generation = next_generation.saturating_add(1);
                            expected_generations.insert(pane, replacement);
                            spawn_successor(
                                &mut jobs,
                                &sender,
                                pane,
                                replacement,
                                SuccessorRequest {
                                    config: config.clone(),
                                    workspace: current.view.workspace.clone(),
                                    model,
                                    context_window_tokens: current.view.context_window_tokens,
                                },
                                SessionReplacement::New(DraftReset::Preserve),
                            );
                        }
                        RootEffect::ResumeSession(id) => {
                            let replacement = next_generation;
                            next_generation = next_generation.saturating_add(1);
                            expected_generations.insert(pane, replacement);
                            let config = config.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let result =
                                    async { ConfiguredSession::resume_label(&config, &id).await }
                                        .await;
                                let _ = out
                                    .send(Update::Session {
                                        pane,
                                        generation: replacement,
                                        result: Box::new(result),
                                        replacement: SessionReplacement::New(DraftReset::Clear),
                                    })
                                    .await;
                            });
                        }
                        RootEffect::LoadSessions(kind) => {
                            let path = config.path().to_owned();
                            let workspace = current.view.workspace.clone();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let event = match session::list_async(
                                    path,
                                    workspace,
                                    matches!(kind, super::components::SessionListKind::Resume),
                                )
                                .await
                                {
                                    Ok(sessions) => AppEvent::SessionsLoaded { pane, sessions },
                                    Err(error) => AppEvent::SessionLoadFailed {
                                        pane,
                                        error: error.to_string(),
                                    },
                                };
                                let _ = out
                                    .send(Update::Event {
                                        pane,
                                        generation,
                                        event,
                                    })
                                    .await;
                            });
                        }
                        RootEffect::LoadRecentPrompts(_) => {
                            let path = config.path().to_owned();
                            let out = sender.clone();
                            jobs.spawn(async move {
                                let event = match session::load_recent_prompts_async(path).await {
                                    Ok(prompts) => AppEvent::RecentPromptsLoaded {
                                        pane,
                                        session_id: session.to_string(),
                                        prompts,
                                    },
                                    Err(error) => AppEvent::RecentPromptLoadFailed {
                                        pane,
                                        error: error.to_string(),
                                    },
                                };
                                let _ = out
                                    .send(Update::Event {
                                        pane,
                                        generation,
                                        event,
                                    })
                                    .await;
                            });
                        }
                        RootEffect::Copy(text) => {
                            terminal
                                .copy_to_clipboard(&text)
                                .map_err(RuntimeError::Terminal)?;
                            let _ = super::clipboard::copy_text(&text);
                            let _ = super::clipboard::copy_to_tmux(&text);
                        }
                        RootEffect::OpenLink(url) => {
                            if let Err(error) = crate::app::browser::open(&url) {
                                effects.extend(
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: error.to_string(),
                                    })
                                    .effects,
                                );
                            }
                        }
                        RootEffect::OpenDraftEditor => {
                            let draft = app
                                .root(pane)
                                .map(|root| root.composer().draft().to_owned())
                                .unwrap_or_default();
                            terminal.suspend().map_err(RuntimeError::Terminal)?;
                            let result = editor::edit(&draft, &current.view.workspace).await;
                            terminal.resume().map_err(RuntimeError::Terminal)?;
                            match result {
                                Ok(EditorOutcome::Updated(draft)) => effects.extend(
                                    app.update(AppEvent::EditorDraft { pane, draft }).effects,
                                ),
                                Ok(EditorOutcome::Unchanged) => {}
                                Err(error) => {
                                    app.update(AppEvent::NotifyError {
                                        pane,
                                        error: error.to_string(),
                                    });
                                }
                            }
                            scheduler.request_immediate(Instant::now());
                        }
                        RootEffect::OpenConfigEditor => {
                            terminal.suspend().map_err(RuntimeError::Terminal)?;
                            let result =
                                editor::edit_config(config.path(), &current.view.workspace).await;
                            terminal.resume().map_err(RuntimeError::Terminal)?;
                            if let Err(error) = result {
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: error.to_string(),
                                });
                            }
                            effects.push_back(AppEffect::Pane {
                                pane,
                                effect: RootEffect::ReloadConfig,
                            });
                        }
                        RootEffect::OpenFile(path) => {
                            terminal.suspend().map_err(RuntimeError::Terminal)?;
                            let result = editor::open_file(&path, &current.view.workspace).await;
                            terminal.resume().map_err(RuntimeError::Terminal)?;
                            if let Err(error) = result {
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: error.to_string(),
                                });
                            }
                            scheduler.request_immediate(Instant::now());
                        }
                        RootEffect::ReloadConfig => match config.reload() {
                            Ok(reloaded) => {
                                let (next, _) = reloaded.into_parts();
                                match HostClient::connect(&next).await {
                                    Ok(_) => {
                                        config = next;
                                        app.update(AppEvent::ConfigReloaded {
                                            pane,
                                            theme: config.theme().clone(),
                                            preferred_reasoning_mode: config
                                                .agent()
                                                .reasoning_mode(),
                                            memory_enabled: config.memory().enabled(),
                                            message: "Configuration reloaded".into(),
                                        });
                                    }
                                    Err(error) => {
                                        app.update(AppEvent::ConfigReloadFailed {
                                            pane,
                                            error: error.to_string(),
                                        });
                                    }
                                }
                            }
                            Err(error) => {
                                app.update(AppEvent::ConfigReloadFailed {
                                    pane,
                                    error: error.to_string(),
                                });
                            }
                        },
                        RootEffect::SetTheme(mode) => effects.push_back(AppEffect::SetTheme(mode)),
                        RootEffect::Review { download_assets } => {
                            schedule(
                                app.update(AppEvent::ReviewStarted(pane)),
                                &mut scheduler,
                                &mut effects,
                            );
                            let assets = match crate::review::ReviewAssets::availability() {
                                Ok(crate::review::AssetAvailability::Ready(assets)) => Some(assets),
                                Ok(crate::review::AssetAvailability::DownloadRequired)
                                    if download_assets =>
                                {
                                    None
                                }
                                Ok(crate::review::AssetAvailability::DownloadRequired) => {
                                    schedule(
                                        app.update(AppEvent::ConfirmReviewDownload { pane }),
                                        &mut scheduler,
                                        &mut effects,
                                    );
                                    continue;
                                }
                                Ok(
                                    crate::review::AssetAvailability::DevelopmentInstallRequired {
                                        path,
                                    },
                                ) => {
                                    schedule(
                                        app.update(AppEvent::ReviewFailed {
                                            pane,
                                            error: format!(
                                                "Review assets are unavailable in this development install: {}",
                                                path.display()
                                            ),
                                        }),
                                        &mut scheduler,
                                        &mut effects,
                                    );
                                    continue;
                                }
                                Err(error) => {
                                    schedule(
                                        app.update(AppEvent::ReviewFailed {
                                            pane,
                                            error: error.to_string(),
                                        }),
                                        &mut scheduler,
                                        &mut effects,
                                    );
                                    continue;
                                }
                            };
                            let review_client = current.client.clone();
                            let feedback_client = current.client.clone();
                            let task = current.view.current_task;
                            let workspace = current.view.workspace.clone();
                            let out = sender.clone();
                            let review_agent: crate::review::ReviewAgent =
                                Arc::new(|_prompt, _manifest, shutdown| {
                                    Box::pin(async move {
                                        // Honor cancellation even while the model
                                        // adapter is unavailable, so a cancelled
                                        // overview is reported as cancelled.
                                        if shutdown.is_cancelled() {
                                            return Err(crate::review::ReviewAgentError::Cancelled);
                                        }
                                        Err(crate::review::ReviewAgentError::Failed(
                                            "review overview model adapter is not admitted yet"
                                                .into(),
                                        ))
                                    })
                                });
                            jobs.spawn(async move {
                                let result = async {
                                    let assets = match assets {
                                        Some(assets) => assets,
                                        None => crate::review::ReviewAssets::download().await?,
                                    };
                                    let review = crate::review::ReviewService::start(
                                        review_client,
                                        task,
                                        review_agent,
                                        &workspace,
                                        assets,
                                    )
                                    .await?;
                                    let _ = out
                                        .send(Update::ReviewReady {
                                            pane,
                                            generation,
                                            url: review.url(),
                                        })
                                        .await;
                                    let decision = review.wait().await?;
                                    let Some(decision) = decision else {
                                        return Ok(None);
                                    };
                                    let disposition =
                                        if decision.markdown.contains("## Review: Approved") {
                                            orvek_harness::feedback::Disposition::Approved
                                        } else {
                                            orvek_harness::feedback::Disposition::ChangesRequested
                                        };
                                    let feedback = feedback_client
                                        .query(Command::RecordReview {
                                            session,
                                            manifest: decision.manifest,
                                            source_identity: decision.source_identity,
                                            disposition,
                                            body: decision.markdown.clone(),
                                        })
                                        .await
                                        .map_err(|error| {
                                            crate::review::ReviewError::Feedback(error.to_string())
                                        })?;
                                    let Response::ReviewFeedback(feedback) = feedback else {
                                        return Err(crate::review::ReviewError::Feedback(
                                            "host returned an invalid review feedback receipt"
                                                .into(),
                                        ));
                                    };
                                    Ok(Some(decision.with_feedback(feedback)))
                                }
                                .await;
                                let _ = out
                                    .send(Update::Review {
                                        pane,
                                        generation,
                                        result,
                                    })
                                    .await;
                            });
                        }
                        RootEffect::Fork => {}
                        RootEffect::Shutdown => effects.push_back(AppEffect::Shutdown),
                        pending => {
                            app.update(AppEvent::NotifyError {pane,error:format!("This action is waiting for its host capability adapter: {pending:?}")});
                            scheduler.request_immediate(Instant::now());
                        }
                    }
                }
            }
        }
        if exiting {
            break;
        }
        let now = Instant::now();
        if scheduler.is_due(now) {
            terminal
                .draw(|frame| app.render(frame))
                .map_err(RuntimeError::Terminal)?;
            scheduler.presented(now);
        }
        let deadline = scheduler
            .deadline()
            .into_iter()
            .chain(app.animation_deadline())
            .min();
        tokio::select! {
            ()=shutdown.cancelled()=>break,
            Some(scheme)=system_schemes.recv()=>{
                schedule(app.update(AppEvent::SystemThemeChanged(scheme)),&mut scheduler,&mut effects);
            }
            Some(event)=input.next()=>{
                let event=event.map_err(RuntimeError::Terminal)?;
                if matches!(&event,Event::Key(key) if key.kind==KeyEventKind::Press && key.code==KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL))
                    && let Some(image)=super::clipboard::image_data_url() {schedule(app.update(AppEvent::PasteImage(image)),&mut scheduler,&mut effects);continue;}
                schedule(app.update(AppEvent::Terminal(event)),&mut scheduler,&mut effects);
            }
            Some(update)=updates.recv()=>match update {
                Update::Handoff {pane,generation,result}=>{
                    if panes.get(&pane).is_some_and(|current|current.generation==generation) {
                        if let Some(current)=panes.get_mut(&pane) {current.handoff=None;}
                        match *result {
                            Ok(prepared)=>{
                                let replacement=next_generation;next_generation=next_generation.saturating_add(1);expected_generations.insert(pane,replacement);
                                let view=prepared.configured.session.clone();let skills=prepared.configured.skills.clone();
                                schedule(app.update(AppEvent::HandoffReady {pane,prompt:prepared.prompt,effort:view.model.thinking.into(),reasoning_mode:view.model.reasoning_mode.into(),fast_mode:view.model.fast_mode,model:view.model.model,context_window_tokens:view.context_window_tokens,skills}),&mut scheduler,&mut effects);
                                install(pane,replacement,prepared.configured,&mut panes,&mut app,&sender).await?;
                            }
                            Err(failure)=>{
                                let event=if matches!(&*failure.error,Error::AuxiliaryCancelled){AppEvent::HandoffCancelled(pane)}else{AppEvent::HandoffFailed {pane,error:failure.error.to_string()}};
                                schedule(app.update(event),&mut scheduler,&mut effects);
                                if let Some(prompt)=failure.prompt {schedule(app.update(AppEvent::EditorDraft {pane,draft:prompt}),&mut scheduler,&mut effects);}
                            }
                        }
                    }
                }

                Update::Queue {pane,generation,result}=>{
                    if let Some(current)=panes.get_mut(&pane).filter(|value|value.generation==generation) {
                        current.queue_loading=false;
                        match result {
                            Ok(snapshot) if snapshot.sequence>=current.queue_sequence=>{
                                current.queue_sequence=snapshot.sequence;
                                schedule(app.update(AppEvent::QueueChanged {pane,inputs:snapshot.inputs}),&mut scheduler,&mut effects);
                            }
                            Ok(_)=>{},
                            Err(error)=>schedule(app.update(AppEvent::NotifyError {pane,error:error.to_string()}),&mut scheduler,&mut effects),
                        }
                        if current.queue_dirty {refresh_queue(&mut jobs,&sender,pane,current);}
                    }
                }
                Update::Submission {pane,generation,request,prompt,result}=>{
                    if let Some(current)=panes.get_mut(&pane).filter(|value|value.generation==generation) {
                        match *result {
                            Ok(_receipt)=>{
                                current.pending=None;
                                refresh_queue(&mut jobs,&sender,pane,current);
                                schedule(app.update(AppEvent::SubmissionAcknowledged {pane,prompt}),&mut scheduler,&mut effects);
                            }
                            Err(failure)=>{
                                current.pending=if failure.uncertain {request.map(|request|(request,prompt))} else {None};
                                schedule(app.update(AppEvent::SubmissionFailed {pane,uncertain:failure.uncertain,error:format!("{}{}",failure.error,if failure.uncertain {" · Enter retries the same request"} else {" · Draft retained"})}),&mut scheduler,&mut effects);
                            }
                        }
                    }
                }
                Update::Watch {pane,generation,frame}=>{
                    if let WatchFrame::Subagent { session, event } = &frame {
                        if let Some(current)=panes.get(&pane).filter(|value|value.generation==generation && value.view.id==*session) {
                            let _ = current;
                            if let Some(update) = subagent_update(event) {
                                schedule(app.update(AppEvent::Subagent { pane, update }),&mut scheduler,&mut effects);
                            }
                        }
                        continue;
                    }
                    if let Some(current)=panes.get_mut(&pane).filter(|value|value.generation==generation) {
                        for change in current.projection.apply(frame) {
                            if matches!(&change,ViewChange::Submission(_)|ViewChange::SubmissionChanged {..}|ViewChange::QueueChanged) {refresh_queue(&mut jobs,&sender,pane,current);}
                            if let ViewChange::Task { event: orvek_harness::state::TaskEvent::JobStarted(job), .. } = &change
                                && let Some(invocation) = &job.invocation
                                && invocation.session == current.view.id
                                && invocation.call_id.is_some()
                                && matches!(invocation.capability.as_str(), "exec_command" | "write_stdin")
                            {
                                let client = current.client.clone();
                                let invocation = invocation.clone();
                                let job = job.id;
                                let out = sender.clone();
                                jobs.spawn(async move {
                                    let change = match task_input(&client, &invocation).await {
                                        Ok(arguments) => ViewChange::TaskInput { job, arguments },
                                        Err(error) => ViewChange::Warning(format!("Cannot display input for job {job}: {error}")),
                                    };
                                    let _ = out.send(Update::Enriched { pane, generation, change }).await;
                                });
                            }
                            if let ViewChange::ShellStarted { request } = &change {
                                let client = current.client.clone();
                                let known = current.shell_commands.get(request).cloned();
                                let session_id = current.view.id;
                                let out = sender.clone();
                                let request = *request;
                                jobs.spawn(async move {
                                    let command = match known {
                                        Some(command) => Some(command),
                                        None => {
                                            submission_command(&client, session_id, request).await
                                        }
                                    };
                                    if let Some(command) = command {
                                        let _ = out
                                            .send(Update::Enriched {
                                                pane,
                                                generation,
                                                change: ViewChange::ToolProposed {
                                                    item_id: None,
                                                    request: Some(request),
                                                    call_id: format!("shell-{request}"),
                                                    name: "shell".into(),
                                                    arguments: serde_json::json!({
                                                        "command": command
                                                    })
                                                    .to_string(),
                                                },
                                            })
                                            .await;
                                    }
                                });
                            }
                            if let ViewChange::ReviewRecorded { feedback } = &change {
                                let client = current.client.clone();
                                let out = sender.clone();
                                let feedback = *feedback;
                                jobs.spawn(async move {
                                    if let Some(text) =
                                        review_feedback_text(&client, feedback).await
                                    {
                                        let _ = out
                                            .send(Update::Enriched {
                                                pane,
                                                generation,
                                                change: ViewChange::Status(format!(
                                                    "Review feedback recorded · {text}"
                                                )),
                                            })
                                            .await;
                                    }
                                });
                            }
                            if let ViewChange::ShellPublished { request, report } = &change {
                                let client = current.client.clone();
                                let out = sender.clone();
                                let request = *request;
                                let report = *report;
                                jobs.spawn(async move {
                                    if let Some(output) = shell_output_text(&client, report).await {
                                        let _ = out.send(Update::Enriched { pane, generation, change: ViewChange::ToolResult {
                                            request: Some(request),
                                            call_id: format!("shell-{request}"),
                                            output,
                                        } }).await;
                                    }
                                });
                            }
                            let change = match change {
                                ViewChange::ShellStarted { request } => {
                                    match current.shell_commands.get(&request) {
                                        Some(command) => ViewChange::ToolProposed {
                                            item_id: None,
                                            request: Some(request),
                                            call_id: format!("shell-{request}"),
                                            name: "shell".into(),
                                            arguments: serde_json::json!({"command": command}).to_string(),
                                        },
                                        None => ViewChange::ShellStarted { request },
                                    }
                                }
                                other => other,
                            };
                            match &change {
                                ViewChange::Settings(settings)=>current.view.model = *settings,
                                ViewChange::WorkspaceSaved(seed)=>current.view.branch.workspace=Some(seed.clone()),
                                ViewChange::TaskLinked(task)=>current.view.current_task=Some(*task),
                                ViewChange::RequestStarted {request}=>{
                                    current.view.active_request=Some(*request);
                                    // Only the main pane's state is reported; a
                                    // herdr pane models one primary session,
                                    // not this process's internal forks.
                                    if pane==PaneId::Main {herdr.working(Some(&current.view.id.to_string()));}
                                }
                                ViewChange::RequestSettled {request,..} if current.view.active_request==Some(*request)=>{
                                    current.view.active_request=None;
                                    if pane==PaneId::Main {herdr.idle(Some(&current.view.id.to_string()));}
                                }
                                ViewChange::RequestSettled {request,..} if current.shell_commands.contains_key(request)=>{
                                    schedule(app.update(AppEvent::ShellFinished(pane)),&mut scheduler,&mut effects);
                                }
                                _=>{},
                            }
                            current.view.revision=current.projection.cursor().revision;
                            let record=Arc::new(TranscriptRecord::from_host(current.projection.sequence(),current.projection.recorded_ms(),current.projection.cursor(),change));
                            schedule(app.update(AppEvent::Transcript {pane,record}),&mut scheduler,&mut effects);
                        }
                    }
                }
                Update::Enriched {pane,generation,change}=>{
                    if let Some(current)=panes.get(&pane).filter(|value|value.generation==generation) {
                        let record=Arc::new(TranscriptRecord::from_host(current.projection.sequence(),current.projection.recorded_ms(),current.projection.cursor(),change));
                        schedule(app.update(AppEvent::Transcript {pane,record}),&mut scheduler,&mut effects);
                    }
                }
                Update::MemoryLoaded {pane,generation,access,records}=>{
                    if panes.get(&pane).is_some_and(|value|value.generation==generation) {
                        schedule(app.update(AppEvent::MemoriesLoaded {pane,access,records}),&mut scheduler,&mut effects);
                    }
                }
                Update::MemoryListFailed {pane,generation,source,access,error}=>{
                    if panes.get(&pane).is_some_and(|value|value.generation==generation) {
                        schedule(app.update(AppEvent::MemoryLoadFailed {pane,source,access,error}),&mut scheduler,&mut effects);
                    }
                }
                Update::MemoryRemoved {pane,generation,key}=>{
                    if panes.get(&pane).is_some_and(|value|value.generation==generation) {
                        schedule(app.update(AppEvent::MemoryDeleted {pane,key}),&mut scheduler,&mut effects);
                    }
                }
                Update::MemoryRemoveFailed {pane,generation,error,conflict}=>{
                    if panes.get(&pane).is_some_and(|value|value.generation==generation) {
                        schedule(app.update(AppEvent::MemoryDeleteFailed {pane,error,conflict}),&mut scheduler,&mut effects);
                    }
                }
                Update::Disconnected {pane,generation,error}=>{
                    if panes.get(&pane).is_some_and(|value|value.generation==generation) {schedule(app.update(AppEvent::NotifyError {pane,error}),&mut scheduler,&mut effects);}
                }
                Update::Reply {pane,generation,result}=>{
                    if let Some(current)=panes.get_mut(&pane).filter(|value|value.generation==generation) {
                        match result {Ok(Response::Submission(_))=>refresh_queue(&mut jobs,&sender,pane,current),Ok(Response::TaskFinished(_))|Ok(Response::Cancelled {..})=>{},Ok(_)=>{},Err(error)=>{
                            refresh_queue(&mut jobs,&sender,pane,current);
                            schedule(app.update(AppEvent::NotifyError {pane,error:error.to_string()}),&mut scheduler,&mut effects);}}
                    }
                }
                Update::Review { pane, generation, result } => {
                    if panes.get(&pane).is_some_and(|current| current.generation == generation) {
                        let event = match result {
                            Ok(Some(review)) => {
                                let feedback = review.feedback();
                                AppEvent::ReviewFinished {
                                    pane,
                                    markdown: review.markdown,
                                    feedback,
                                }
                            }
                            Ok(None) => AppEvent::ReviewCancelled(pane),
                            Err(error) => AppEvent::ReviewFailed {
                                pane,
                                error: error.user_message(),
                            },
                        };
                        schedule(app.update(event), &mut scheduler, &mut effects);
                    }
                }
                Update::ReviewReady { pane, generation, url } => {
                    if panes.get(&pane).is_some_and(|current| current.generation == generation) {
                        schedule(
                            app.update(AppEvent::ReviewReady { pane, url: url.clone() }),
                            &mut scheduler,
                            &mut effects,
                        );
                        if let Err(error) = crate::app::browser::open(&url) {
                            schedule(
                                app.update(AppEvent::NotifyError {
                                    pane,
                                    error: format!("Could not open review: {error}"),
                                }),
                                &mut scheduler,
                                &mut effects,
                            );
                        }
                    }
                }
                Update::Session {pane,generation,result,replacement} if expected_generations.get(&pane) == Some(&generation) => match *result {
                    Ok(configured)=>{
                        let view=configured.session.clone();let skills=configured.skills.clone();
                        if replacement.persists_settings() {
                            config.set_thinking(view.model.thinking.into());
                            config.set_reasoning_mode(view.model.reasoning_mode.into());
                            config.set_fast_mode(view.model.fast_mode);
                            config.persist_thinking(view.model.thinking.into())?;
                            config.persist_reasoning_mode(view.model.reasoning_mode.into())?;
                            config.persist_fast_mode(view.model.fast_mode)?;
                        }
                        schedule(app.update(AppEvent::NewSessionReady {pane,effort:view.model.thinking.into(),reasoning_mode:view.model.reasoning_mode.into(),fast_mode:view.model.fast_mode,model:view.model.model,context_window_tokens:view.context_window_tokens,draft_reset:replacement.draft_reset(),skills}),&mut scheduler,&mut effects);
                        install(pane,generation,configured,&mut panes,&mut app,&sender).await?;
                        schedule(app.update(AppEvent::ForkReady {pane}),&mut scheduler,&mut effects);
                    }
                    Err(error)=>{
                        if replacement.persists_settings()
                            && let Some(current)=panes.get(&pane)
                        {
                            schedule(app.update(AppEvent::SettingsConfirmed {pane,model:current.view.model,preferred:config.agent().reasoning_mode()}),&mut scheduler,&mut effects);
                        }
                        schedule(app.update(AppEvent::NewSessionFailed {pane,error:error.to_string()}),&mut scheduler,&mut effects)
                    },
                },
                Update::Session { .. } => {},
                Update::Event {pane,generation,event} => { if panes.get(&pane).is_some_and(|value|value.generation==generation) { schedule(app.update(event),&mut scheduler,&mut effects); } },
            },
            Some(_)=jobs.join_next(),if !jobs.is_empty()=>{},
            ()=async {match deadline {Some(at)=>tokio::time::sleep_until(at.into()).await,None=>std::future::pending().await}}=>schedule(app.update(AppEvent::AnimationFrame(Instant::now())),&mut scheduler,&mut effects),
        }
    }
    let session = app
        .main_pane()
        .and_then(|pane| panes.get(&pane))
        .map(|pane| pane.view.id.to_string());
    for pane in panes.values() {
        pane.watch.cancel();
    }
    // Dropping viewer/RPC futures closes connections only. No task cancellation is sent on exit.
    jobs.abort_all();
    Ok(session)
}

async fn install(
    pane: PaneId,
    generation: u64,
    configured: ConfiguredSession,
    panes: &mut HashMap<PaneId, Pane>,
    app: &mut AppNode,
    sender: &mpsc::Sender<Update>,
) -> Result<()> {
    if let Some(old) = panes.remove(&pane) {
        old.watch.cancel();
    }
    let mut projection = HostProjection::at_snapshot(&configured.session);
    let queue = session::queued(&configured.client, configured.session.id).await?;
    if let Some((request, visible)) = queue.active_auxiliary {
        projection.classify_auxiliary(request, visible);
    }
    let queue_sequence = queue.sequence;
    app.update(AppEvent::QueueChanged {
        pane,
        inputs: queue.inputs,
    });
    let cursor = projection.cursor();
    for change in session::history(&configured.client, &configured.session).await? {
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
    Ok(())
}
fn watch(
    pane: PaneId,
    generation: u64,
    client: HostClient,
    mut after: u64,
    cancel: CancellationToken,
    sender: mpsc::Sender<Update>,
) {
    tokio::spawn(async move {
        let mut failures = 0u32;
        loop {
            let result = tokio::select! {()=cancel.cancelled()=>return,result=client.subscribe(after)=>result};
            let error = match result {
                Ok(mut watch) => loop {
                    let result =
                        tokio::select! {()=cancel.cancelled()=>return,result=watch.next()=>result};
                    match result {
                        Ok(frame) => {
                            if matches!(&frame, WatchFrame::Journal(_)) {
                                failures = 0;
                            }
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
                        }
                        Err(error) => break error,
                    }
                },
                Err(error) => error,
            };
            failures += 1;
            let message = if failures > 5 {
                format!(
                    "View disconnected after bounded retries: {error}. Host work continues; resume the session to reconnect."
                )
            } else {
                format!("Reconnecting view: {error}")
            };
            if !send_watch(
                &sender,
                &cancel,
                Update::Disconnected {
                    pane,
                    generation,
                    error: message,
                },
            )
            .await
                || failures > 5
            {
                return;
            }
            tokio::select! {()=cancel.cancelled()=>return,()=tokio::time::sleep(Duration::from_millis(100*(1u64<<failures.min(5))))=>{}}
        }
    });
}
async fn send_watch(
    sender: &mpsc::Sender<Update>,
    cancel: &CancellationToken,
    update: Update,
) -> bool {
    tokio::select! {()=cancel.cancelled()=>false,result=sender.send(update)=>result.is_ok()}
}

fn dispatch(
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

fn spawn_successor(
    jobs: &mut JoinSet<()>,
    sender: &mpsc::Sender<Update>,
    pane: PaneId,
    generation: u64,
    request: SuccessorRequest,
    replacement: SessionReplacement,
) {
    let sender = sender.clone();
    jobs.spawn(async move {
        let result = ConfiguredSession::create_successor(
            &request.config,
            &request.workspace,
            request.model,
            request.context_window_tokens,
        )
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

fn schedule(
    update: ComponentUpdate<AppEffect>,
    scheduler: &mut RenderScheduler,
    effects: &mut VecDeque<AppEffect>,
) {
    effects.extend(update.effects);
    match update.render {
        RenderRequest::None => {}
        RenderRequest::Streaming => scheduler.request_streaming(Instant::now()),
        RenderRequest::Immediate => scheduler.request_immediate(Instant::now()),
    }
}

/// Resolve recorded invocation input through the authenticated host, not its store.
async fn task_input(
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

fn decode_task_input(
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
async fn shell_output_text(
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
struct SubmissionJob {
    client: HostClient,
    session: orvek_harness::session::SessionId,
    prompt: super::prompt::Submission,
    existing: Option<Request>,
}

/// Maps a host subagent lifecycle event onto the TUI child tree contract.
/// Schema-valid results are referenced by digest, never inlined.
fn subagent_update(event: &orvek_harness::controller::SubagentEvent) -> Option<ChildUpdate> {
    use orvek_harness::controller::SubagentEvent;
    match event {
        SubagentEvent::Spawned {
            agent,
            role,
            task,
            model,
            ..
        } => {
            let model = model.parse().ok()?;
            Some(ChildUpdate::Added(ChildView {
                id: ChildId(*agent),
                session_id: String::new(),
                model,
                role: role.clone(),
                task: task.clone(),
                parent: None,
            }))
        }
        SubagentEvent::Returned { agent, output, .. } => Some(ChildUpdate::Status {
            id: ChildId(*agent),
            status: ChildStatus::Returned { output: *output },
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
async fn submission_command(
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
async fn review_feedback_text(
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

fn dispatch_submission(
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

fn refresh_queue(
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
#[path = "client_task_input_tests.rs"]
mod task_input_tests;
