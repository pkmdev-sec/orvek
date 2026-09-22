use super::{
    StartupMode,
    client::*,
    components::{
        AppEffect, AppEvent, AppNode, ComponentUpdate, DraftReset, RenderRequest, RootEffect,
        RootNode,
    },
    editor::{self, EditorOutcome},
    file_index::discover_file_index,
    host_projection::ViewChange,
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
        submission as submissions,
    },
    core::ConfiguredSession,
};
use crossterm::event::EventStream;
use futures_util::StreamExt;
use orvek_harness::ipc::{Command, Request, Response, WatchFrame};
use orvek_memory::MemoryStore;
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

pub(super) async fn run(
    mut config: Config,
    startup: StartupMode,
    shutdown: CancellationToken,
) -> Result<Option<String>> {
    let selector = matches!(startup, StartupMode::ResumeSelector(_));
    let mut terminal = TerminalSession::enter().map_err(RuntimeError::Terminal)?;
    let mut input = EventStream::new();
    let Some(prepared) =
        prepare_startup(&config, startup, &shutdown, &mut terminal, &mut input).await?
    else {
        return Ok(None);
    };
    let configured = &prepared.configured;
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
    let (schemes, mut system_schemes) = watch::channel(None);
    let _system_scheme_watcher = super::theme::watch_system_scheme(schemes, shutdown.clone());
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
    install(PaneId::Main, 0, prepared, &mut panes, &mut app, &sender);
    let mut expected_generations = HashMap::new();
    let mut next_generation = 1u64;
    let mut effects = VecDeque::new();
    if selector {
        effects.extend(app.open_resume_selector().effects);
    }
    terminal
        .report_working_directory(&workspace)
        .map_err(RuntimeError::Terminal)?;
    let mut scheduler = RenderScheduler::new(STREAM_FRAME_INTERVAL, Instant::now());
    let mut exiting = false;
    while !exiting && !shutdown.is_cancelled() {
        if matches!(
            dispatch_effects(&mut EffectContext {
                config: &mut config,
                expected_generations: &mut expected_generations,
                panes: &mut panes,
                app: &mut app,
                sender: &sender,
                jobs: &mut jobs,
                effects: &mut effects,
                scheduler: &mut scheduler,
                terminal: &mut terminal,
                next_generation: &mut next_generation,
            })
            .await?,
            DispatchOutcome::Exit
        ) {
            exiting = true;
        }
        if exiting {
            break;
        }
        super::event_loop::render_if_due(&mut scheduler, &mut terminal, &mut app)?;
        let deadline = super::event_loop::next_deadline(&scheduler, app.animation_deadline());
        tokio::select! {
            ()=shutdown.cancelled()=>break,
            Ok(())=system_schemes.changed()=>{
                if let Some(scheme)=*system_schemes.borrow_and_update() {
                    schedule(app.update(AppEvent::SystemThemeChanged(scheme)),&mut scheduler,&mut effects);
                }
            }
            Some(event)=input.next()=>{
                let event=event.map_err(RuntimeError::Terminal)?;
                if is_image_paste_shortcut(&event)
                    && let Some(image)=super::clipboard::image_data_url() {schedule(app.update(AppEvent::PasteImage(image)),&mut scheduler,&mut effects);continue;}
                schedule(app.update(AppEvent::Terminal(event)),&mut scheduler,&mut effects);
            }
            Some(update)=updates.recv()=>handle_update(update, &mut UpdateContext {
                config: &mut config,
                expected_generations: &mut expected_generations,
                panes: &mut panes,
                app: &mut app,
                sender: &sender,
                jobs: &mut jobs,
                effects: &mut effects,
                scheduler: &mut scheduler,
                next_generation: &mut next_generation,
                herdr: &mut herdr,
            }),
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

/// Keep component transitions pure: this boundary owns the side effects and
/// render urgency emitted by every update handled in the client event loop.
struct UpdateContext<'a> {
    config: &'a mut Config,
    expected_generations: &'a mut HashMap<PaneId, u64>,
    panes: &'a mut HashMap<PaneId, Pane>,
    app: &'a mut AppNode,
    sender: &'a mpsc::Sender<Update>,
    jobs: &'a mut JoinSet<()>,
    effects: &'a mut VecDeque<AppEffect>,
    scheduler: &'a mut RenderScheduler,
    next_generation: &'a mut u64,
    herdr: &'a mut Reporter,
}

enum UpdateFamily {
    Handoff(Update),
    Session(Update),
    Stream(Update),
    Child(Update),
}

impl UpdateFamily {
    fn from(update: Update) -> Self {
        match update {
            update
            @ (Update::Handoff { .. } | Update::Queue { .. } | Update::Submission { .. }) => {
                Self::Handoff(update)
            }
            update @ Update::Session { .. } => Self::Session(update),
            update @ (Update::Watch { .. }
            | Update::Enriched { .. }
            | Update::Degraded { .. }
            | Update::Recovered { .. }) => Self::Stream(update),
            update => Self::Child(update),
        }
    }
}

fn handle_update(update: Update, context: &mut UpdateContext<'_>) {
    match UpdateFamily::from(update) {
        UpdateFamily::Handoff(update) => handle_handoff_updates(update, context),
        UpdateFamily::Session(update) => handle_session_updates(update, context),
        UpdateFamily::Stream(update) => handle_stream_updates(update, context),
        UpdateFamily::Child(update) => handle_child_updates(update, context),
    }
}
fn handle_handoff_updates(update: Update, context: &mut UpdateContext<'_>) {
    let UpdateContext {
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        next_generation,
        ..
    } = context;
    match update {
        Update::Handoff {
            pane,
            generation,
            result,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|current| current.generation == generation)
            {
                if let Some(current) = panes.get_mut(&pane) {
                    current.handoff = None;
                }
                match *result {
                    Ok(prepared) => {
                        let view = prepared.session.configured.session.clone();
                        let skills = prepared.session.configured.skills.clone();
                        let replacement = **next_generation;
                        **next_generation = next_generation.saturating_add(1);
                        schedule(
                            app.update(AppEvent::HandoffReady {
                                pane,
                                prompt: prepared.prompt,
                                effort: view.model.thinking.into(),
                                reasoning_mode: view.model.reasoning_mode.into(),
                                fast_mode: view.model.fast_mode,
                                model: view.model.model,
                                context_window_tokens: view.context_window_tokens,
                                skills,
                            }),
                            scheduler,
                            effects,
                        );
                        install(pane, replacement, prepared.session, panes, app, sender);
                    }
                    Err(failure) => {
                        let event = if matches!(&*failure.error, Error::AuxiliaryCancelled) {
                            AppEvent::HandoffCancelled(pane)
                        } else {
                            AppEvent::HandoffFailed {
                                pane,
                                error: failure.error.to_string(),
                            }
                        };
                        schedule(app.update(event), scheduler, effects);
                        if let Some(prompt) = failure.prompt {
                            schedule(
                                app.update(AppEvent::EditorDraft {
                                    pane,
                                    draft: prompt,
                                }),
                                scheduler,
                                effects,
                            );
                        }
                    }
                }
            }
        }

        Update::Queue {
            pane,
            generation,
            result,
        } => {
            if let Some(current) = panes
                .get_mut(&pane)
                .filter(|value| value.generation == generation)
            {
                current.queue_loading = false;
                match result {
                    Ok(snapshot) if snapshot.sequence >= current.queue_sequence => {
                        current.queue_sequence = snapshot.sequence;
                        schedule(
                            app.update(AppEvent::QueueChanged {
                                pane,
                                inputs: snapshot.inputs,
                            }),
                            scheduler,
                            effects,
                        );
                    }
                    Ok(_) => {}
                    Err(error) => schedule(
                        app.update(AppEvent::NotifyError {
                            pane,
                            error: error.to_string(),
                        }),
                        scheduler,
                        effects,
                    ),
                }
                if current.queue_dirty {
                    refresh_queue(jobs, sender, pane, current);
                }
            }
        }
        Update::Submission {
            pane,
            generation,
            request,
            prompt,
            result,
        } => {
            if let Some(current) = panes
                .get_mut(&pane)
                .filter(|value| value.generation == generation)
            {
                match *result {
                    Ok(_receipt) => {
                        current.pending = None;
                        refresh_queue(jobs, sender, pane, current);
                        schedule(
                            app.update(AppEvent::SubmissionAcknowledged { pane, prompt }),
                            scheduler,
                            effects,
                        );
                    }
                    Err(failure) => {
                        current.pending = if failure.uncertain {
                            request.map(|request| (request, prompt))
                        } else {
                            None
                        };
                        schedule(
                            app.update(AppEvent::SubmissionFailed {
                                pane,
                                uncertain: failure.uncertain,
                                error: submission_notice(&failure),
                            }),
                            scheduler,
                            effects,
                        );
                    }
                }
            }
        }
        _ => unreachable!("update family mismatch"),
    }
}
fn handle_stream_updates(update: Update, context: &mut UpdateContext<'_>) {
    let UpdateContext {
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        herdr,
        ..
    } = context;
    match update {
        Update::Watch {
            pane,
            generation,
            frame,
        } => {
            if let WatchFrame::Subagent { session, event } = &frame {
                if panes.get(&pane).is_some_and(|value| {
                    value.generation == generation && value.view.id == *session
                }) && let Some(update) = subagent_update(event)
                {
                    schedule(
                        app.update(AppEvent::Subagent { pane, update }),
                        scheduler,
                        effects,
                    );
                }
                return;
            }
            if let Some(current) = panes
                .get_mut(&pane)
                .filter(|value| value.generation == generation)
            {
                for change in current.projection.apply(frame) {
                    if matches!(
                        &change,
                        ViewChange::Submission(_)
                            | ViewChange::SubmissionChanged { .. }
                            | ViewChange::QueueChanged
                    ) {
                        refresh_queue(jobs, sender, pane, current);
                    }
                    if let ViewChange::Task {
                        event: orvek_harness::state::TaskEvent::JobStarted(job),
                        ..
                    } = &change
                        && let Some(invocation) = &job.invocation
                        && invocation.session == current.view.id
                        && invocation.call_id.is_some()
                        && matches!(
                            invocation.capability.as_str(),
                            "exec_command" | "write_stdin"
                        )
                    {
                        let client = current.client.clone();
                        let invocation = invocation.clone();
                        let job = job.id;
                        let out = sender.clone();
                        jobs.spawn(async move {
                            let change = match task_input(&client, &invocation).await {
                                Ok(arguments) => ViewChange::TaskInput { job, arguments },
                                Err(error) => ViewChange::Warning(format!(
                                    "Cannot display input for job {job}: {error}"
                                )),
                            };
                            let _ = out
                                .send(Update::Enriched {
                                    pane,
                                    generation,
                                    change,
                                })
                                .await;
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
                                None => submission_command(&client, session_id, request).await,
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
                            if let Some(text) = review_feedback_text(&client, feedback).await {
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
                                let _ = out
                                    .send(Update::Enriched {
                                        pane,
                                        generation,
                                        change: ViewChange::ToolResult {
                                            request: Some(request),
                                            call_id: format!("shell-{request}"),
                                            output,
                                        },
                                    })
                                    .await;
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
                        ViewChange::Settings(settings) => current.view.model = *settings,
                        ViewChange::WorkspaceSaved(seed) => {
                            current.view.branch.workspace = Some(seed.clone())
                        }
                        ViewChange::TaskLinked(task) => current.view.current_task = Some(*task),
                        ViewChange::RequestStarted { request } => {
                            current.view.active_request = Some(*request);
                            // Only the main pane's state is reported; a
                            // herdr pane models one primary session,
                            // not this process's internal forks.
                            if pane == PaneId::Main {
                                herdr.working(Some(&current.view.id.to_string()));
                            }
                        }
                        ViewChange::RequestSettled { request, .. }
                            if current.view.active_request == Some(*request) =>
                        {
                            current.view.active_request = None;
                            if pane == PaneId::Main {
                                herdr.idle(Some(&current.view.id.to_string()));
                            }
                        }
                        ViewChange::RequestSettled { request, .. }
                            if current.shell_commands.contains_key(request) =>
                        {
                            schedule(
                                app.update(AppEvent::ShellFinished(pane)),
                                scheduler,
                                effects,
                            );
                        }
                        _ => {}
                    }
                    current.view.revision = current.projection.cursor().revision;
                    let record = Arc::new(TranscriptRecord::from_host(
                        current.projection.sequence(),
                        current.projection.recorded_ms(),
                        current.projection.cursor(),
                        change,
                    ));
                    schedule(
                        app.update(AppEvent::Transcript { pane, record }),
                        scheduler,
                        effects,
                    );
                }
            }
        }
        Update::Enriched {
            pane,
            generation,
            change,
        } => {
            if let Some(current) = panes
                .get(&pane)
                .filter(|value| value.generation == generation)
            {
                let record = Arc::new(TranscriptRecord::from_host(
                    current.projection.sequence(),
                    current.projection.recorded_ms(),
                    current.projection.cursor(),
                    change,
                ));
                schedule(
                    app.update(AppEvent::Transcript { pane, record }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::Degraded {
            pane,
            generation,
            error,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::NotifyError { pane, error }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::Recovered { pane, generation } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::NotifyError {
                        pane,
                        error: "View reconnected.".into(),
                    }),
                    scheduler,
                    effects,
                );
            }
        }
        _ => unreachable!("update family mismatch"),
    }
}
fn handle_child_updates(update: Update, context: &mut UpdateContext<'_>) {
    let UpdateContext {
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        ..
    } = context;
    match update {
        Update::MemoryLoaded {
            pane,
            generation,
            access,
            records,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::MemoriesLoaded {
                        pane,
                        access,
                        records,
                    }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::MemoryListFailed {
            pane,
            generation,
            source,
            access,
            error,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::MemoryLoadFailed {
                        pane,
                        source,
                        access,
                        error,
                    }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::MemoryRemoved {
            pane,
            generation,
            key,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::MemoryDeleted { pane, key }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::MemoryRemoveFailed {
            pane,
            generation,
            error,
            conflict,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(
                    app.update(AppEvent::MemoryDeleteFailed {
                        pane,
                        error,
                        conflict,
                    }),
                    scheduler,
                    effects,
                );
            }
        }
        Update::Reply {
            pane,
            generation,
            result,
        } => {
            if let Some(current) = panes
                .get_mut(&pane)
                .filter(|value| value.generation == generation)
            {
                match result {
                    Ok(Response::Submission(_)) => refresh_queue(jobs, sender, pane, current),
                    Ok(Response::Cancelled { requested }) => {
                        refresh_queue(jobs, sender, pane, current);
                        if requested {
                            schedule(
                                app.update(AppEvent::TurnsCancelled(pane)),
                                scheduler,
                                effects,
                            );
                        }
                    }
                    Ok(Response::TaskFinished(_)) => {}
                    Ok(_) => {}
                    Err(error) => {
                        refresh_queue(jobs, sender, pane, current);
                        schedule(
                            app.update(AppEvent::NotifyError {
                                pane,
                                error: error.to_string(),
                            }),
                            scheduler,
                            effects,
                        );
                    }
                }
            }
        }
        Update::Review {
            pane,
            generation,
            result,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|current| current.generation == generation)
            {
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
                schedule(app.update(event), scheduler, effects);
            }
        }
        Update::ReviewReady {
            pane,
            generation,
            url,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|current| current.generation == generation)
            {
                schedule(
                    app.update(AppEvent::ReviewReady {
                        pane,
                        url: url.clone(),
                    }),
                    scheduler,
                    effects,
                );
                if let Err(error) = crate::app::browser::open(&url) {
                    schedule(
                        app.update(AppEvent::NotifyError {
                            pane,
                            error: format!("Could not open review: {error}"),
                        }),
                        scheduler,
                        effects,
                    );
                }
            }
        }
        Update::Event {
            pane,
            generation,
            event,
        } => {
            if panes
                .get(&pane)
                .is_some_and(|value| value.generation == generation)
            {
                schedule(app.update(event), scheduler, effects);
            }
        }
        _ => unreachable!("update family mismatch"),
    }
}
fn handle_session_updates(update: Update, context: &mut UpdateContext<'_>) {
    let UpdateContext {
        config,
        expected_generations,
        panes,
        app,
        sender,
        effects,
        scheduler,
        ..
    } = context;
    match update {
        Update::Session {
            pane,
            generation,
            result,
            replacement,
        } if expected_generations.get(&pane) == Some(&generation) => {
            expected_generations.remove(&pane);
            let previous_settings = replacement
                .persists_settings()
                .then(|| panes.get(&pane))
                .flatten()
                .map(|current| (current.view.model, config.agent().reasoning_mode()));
            match *result {
                Ok(prepared) => {
                    let view = prepared.configured.session.clone();
                    let skills = prepared.configured.skills.clone();
                    let persisted = if replacement.persists_settings() {
                        config.persist_agent_settings(
                            view.model.thinking.into(),
                            view.model.reasoning_mode.into(),
                            view.model.fast_mode,
                        )
                    } else {
                        Ok(())
                    };
                    if let Err(error) = persisted {
                        reject_session_replacement(
                            replacement,
                            pane,
                            error.to_string(),
                            previous_settings,
                            app,
                            scheduler,
                            effects,
                        );
                    } else {
                        if replacement.persists_settings() {
                            config.set_thinking(view.model.thinking.into());
                            config.set_reasoning_mode(view.model.reasoning_mode.into());
                            config.set_fast_mode(view.model.fast_mode);
                        }
                        schedule(
                            app.update(AppEvent::NewSessionReady {
                                pane,
                                effort: view.model.thinking.into(),
                                reasoning_mode: view.model.reasoning_mode.into(),
                                fast_mode: view.model.fast_mode,
                                model: view.model.model,
                                context_window_tokens: view.context_window_tokens,
                                draft_reset: replacement.draft_reset(),
                                skills,
                            }),
                            scheduler,
                            effects,
                        );
                        install(pane, generation, prepared, panes, app, sender);
                        if matches!(replacement, SessionReplacement::Fork) {
                            schedule(app.update(AppEvent::ForkReady { pane }), scheduler, effects);
                        }
                    }
                }
                Err(error) => reject_session_replacement(
                    replacement,
                    pane,
                    error.to_string(),
                    previous_settings,
                    app,
                    scheduler,
                    effects,
                ),
            }
        }
        Update::Session { .. } => {}
        _ => unreachable!("update family mismatch"),
    }
}

struct EffectContext<'a> {
    config: &'a mut Config,
    expected_generations: &'a mut HashMap<PaneId, u64>,
    panes: &'a mut HashMap<PaneId, Pane>,
    app: &'a mut AppNode,
    sender: &'a mpsc::Sender<Update>,
    jobs: &'a mut JoinSet<()>,
    effects: &'a mut VecDeque<AppEffect>,
    scheduler: &'a mut RenderScheduler,
    terminal: &'a mut TerminalSession,
    next_generation: &'a mut u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DispatchOutcome {
    Continue,
    Exit,
}

async fn dispatch_effects(context: &mut EffectContext<'_>) -> Result<DispatchOutcome> {
    while let Some(effect) = context.effects.pop_front() {
        if matches!(
            dispatch_effect(effect, context).await?,
            DispatchOutcome::Exit
        ) {
            return Ok(DispatchOutcome::Exit);
        }
    }
    Ok(DispatchOutcome::Continue)
}

async fn dispatch_effect(
    effect: AppEffect,
    context: &mut EffectContext<'_>,
) -> Result<DispatchOutcome> {
    let EffectContext {
        config,
        expected_generations,
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        next_generation,
        ..
    } = context;
    match effect {
        AppEffect::Shutdown => return Ok(DispatchOutcome::Exit),
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
            config.set_max_subagents(limit)?;
        }
        AppEffect::OpenFork { pane, parent } => {
            let generation = **next_generation;
            **next_generation = next_generation.saturating_add(1);
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
                        let configured = ConfiguredSession::resume(&config, view.id).await?;
                        prepare_install(configured).await
                    }
                    .await;
                    let _ = out
                        .send(Update::Session {
                            pane,
                            generation,
                            result: Box::new(result),
                            replacement: SessionReplacement::Fork,
                        })
                        .await;
                });
            } else {
                expected_generations.remove(&pane);
                schedule(
                    app.update(AppEvent::ForkFailed {
                        pane,
                        error: "parent session is no longer available".into(),
                    }),
                    scheduler,
                    effects,
                );
            }
        }
        AppEffect::Pane { pane, effect } => dispatch_pane_effect(pane, effect, context).await?,
    }
    Ok(DispatchOutcome::Continue)
}
async fn dispatch_pane_effect(
    pane: PaneId,
    effect: RootEffect,
    context: &mut EffectContext<'_>,
) -> Result<()> {
    let effect = match handle_submission_effect(pane, effect, context).await? {
        Some(effect) => effect,
        None => return Ok(()),
    };
    let effect = match handle_session_effect(pane, effect, context).await? {
        Some(effect) => effect,
        None => return Ok(()),
    };
    let effect = match handle_pane_effect(pane, effect, context).await? {
        Some(effect) => effect,
        None => return Ok(()),
    };
    let effect = match handle_review_effect(pane, effect, context).await? {
        Some(effect) => effect,
        None => return Ok(()),
    };
    handle_pending_effect(pane, effect, context);
    Ok(())
}
async fn handle_submission_effect(
    pane: PaneId,
    effect: RootEffect,
    context: &mut EffectContext<'_>,
) -> Result<Option<RootEffect>> {
    let EffectContext {
        config,
        panes,
        sender,
        jobs,
        ..
    } = context;
    let Some(current) = panes.get(&pane) else {
        return Ok(None);
    };
    let client = current.client.clone();
    let session = current.view.id;
    let generation = current.generation;
    match effect {
        RootEffect::Submit(prompt) => {
            dispatch_submission(
                jobs,
                sender,
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
                jobs,
                sender,
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
                let event = match crate::core::configured_memory_store(&config, &workspace) {
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
                let event = match crate::core::configured_memory_store(&config, &workspace) {
                    Ok(Some(store)) => {
                        let result = MemoryStore::delete(&store, key.clone()).await;
                        match result {
                            Ok(()) => Update::MemoryRemoved {
                                pane,
                                generation,
                                key,
                            },
                            Err(error) => Update::MemoryRemoveFailed {
                                pane,
                                generation,
                                conflict: matches!(error, orvek_memory::MemoryError::Conflict),
                                error: error.to_string(),
                            },
                        }
                    }
                    Ok(None) => Update::MemoryRemoveFailed {
                        pane,
                        generation,
                        conflict: false,
                        error: "Memory was disabled before the deletion completed.".to_owned(),
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
                let result = match super::handoff_controller::prepare(
                    &config,
                    &client,
                    session,
                    &cancellation,
                )
                .await
                {
                    Ok(prepared) => {
                        let prompt = prepared.prompt;
                        match prepare_install(prepared.configured).await {
                            Ok(session) => Ok(PreparedHandoffSession { prompt, session }),
                            Err(error) => Err(super::handoff_controller::HandoffFailure {
                                prompt: Some(prompt),
                                error: Box::new(error),
                            }),
                        }
                    }
                    Err(failure) => Err(failure),
                };
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
                jobs,
                sender,
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
                    jobs,
                    sender,
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
            jobs,
            sender,
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
            jobs,
            sender,
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
            jobs,
            sender,
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
            jobs,
            sender,
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
            jobs,
            sender,
            pane,
            generation,
            client,
            Request::new(Command::Cancel { session }),
            Duration::from_secs(10),
        ),
        effect => return Ok(Some(effect)),
    }
    Ok(None)
}
async fn handle_session_effect(
    pane: PaneId,
    effect: RootEffect,
    context: &mut EffectContext<'_>,
) -> Result<Option<RootEffect>> {
    let EffectContext {
        config,
        expected_generations,
        panes,
        sender,
        jobs,
        next_generation,
        ..
    } = context;
    let Some(current) = panes.get(&pane) else {
        return Ok(None);
    };
    match effect {
        RootEffect::SetEffort {
            effort,
            reasoning_mode,
        } => {
            let mut model = current.view.model;
            model.thinking = effort.into();
            model.reasoning_mode = reasoning_mode.into();
            let replacement = **next_generation;
            **next_generation = next_generation.saturating_add(1);
            expected_generations.insert(pane, replacement);
            spawn_successor(
                jobs,
                sender,
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
            let replacement = **next_generation;
            **next_generation = next_generation.saturating_add(1);
            expected_generations.insert(pane, replacement);
            spawn_successor(
                jobs,
                sender,
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
            let replacement = **next_generation;
            **next_generation = next_generation.saturating_add(1);
            expected_generations.insert(pane, replacement);
            let config = config.clone();
            let out = sender.clone();
            jobs.spawn(async move {
                let result = async {
                    let configured = ConfiguredSession::create(
                        &config,
                        config.agent().thinking(),
                        config.agent().reasoning_mode(),
                        model,
                    )
                    .await?;
                    prepare_install(configured).await
                }
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
            let replacement = **next_generation;
            **next_generation = next_generation.saturating_add(1);
            expected_generations.insert(pane, replacement);
            spawn_successor(
                jobs,
                sender,
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
            let replacement = **next_generation;
            **next_generation = next_generation.saturating_add(1);
            expected_generations.insert(pane, replacement);
            let config = config.clone();
            let out = sender.clone();
            jobs.spawn(async move {
                let result = async {
                    let configured = ConfiguredSession::resume_label(&config, &id).await?;
                    prepare_install(configured).await
                }
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
        effect => return Ok(Some(effect)),
    }
    Ok(None)
}
async fn handle_pane_effect(
    pane: PaneId,
    effect: RootEffect,
    context: &mut EffectContext<'_>,
) -> Result<Option<RootEffect>> {
    let EffectContext {
        config,
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        terminal,
        ..
    } = context;
    let Some(current) = panes.get(&pane) else {
        return Ok(None);
    };
    let session = current.view.id;
    let generation = current.generation;
    match effect {
        RootEffect::LoadFileIndex => {
            let workspace = current.view.workspace.clone();
            let out = sender.clone();
            jobs.spawn(async move {
                let result = tokio::task::spawn_blocking(move || discover_file_index(&workspace))
                    .await
                    .map_err(|error| error.to_string());
                let event = AppEvent::FileIndexFinished { pane, result };
                let _ = out
                    .send(Update::Event {
                        pane,
                        generation,
                        event,
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
                Ok(EditorOutcome::Updated(draft)) => {
                    effects.extend(app.update(AppEvent::EditorDraft { pane, draft }).effects)
                }
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
            let result = editor::edit_config(config.path(), &current.view.workspace).await;
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
                        **config = next;
                        app.update(AppEvent::ConfigReloaded {
                            pane,
                            theme: config.theme().clone(),
                            preferred_reasoning_mode: config.agent().reasoning_mode(),
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
        effect => return Ok(Some(effect)),
    }
    Ok(None)
}
async fn handle_review_effect(
    pane: PaneId,
    effect: RootEffect,
    context: &mut EffectContext<'_>,
) -> Result<Option<RootEffect>> {
    let EffectContext {
        panes,
        app,
        sender,
        jobs,
        effects,
        scheduler,
        ..
    } = context;
    let Some(current) = panes.get(&pane) else {
        return Ok(None);
    };
    let session = current.view.id;
    let generation = current.generation;
    match effect {
        RootEffect::Review { download_assets } => {
            schedule(
                app.update(AppEvent::ReviewStarted(pane)),
                scheduler,
                effects,
            );
            let assets = match crate::review::ReviewAssets::availability() {
                Ok(crate::review::AssetAvailability::Ready(assets)) => Some(assets),
                Ok(crate::review::AssetAvailability::DownloadRequired) if download_assets => None,
                Ok(crate::review::AssetAvailability::DownloadRequired) => {
                    schedule(
                        app.update(AppEvent::ConfirmReviewDownload { pane }),
                        scheduler,
                        effects,
                    );
                    return Ok(None);
                }
                Ok(crate::review::AssetAvailability::DevelopmentInstallRequired { path }) => {
                    schedule(
                        app.update(AppEvent::ReviewFailed {
                            pane,
                            error: format!(
                                "Review assets are unavailable in this development install: {}",
                                path.display()
                            ),
                        }),
                        scheduler,
                        effects,
                    );
                    return Ok(None);
                }
                Err(error) => {
                    schedule(
                        app.update(AppEvent::ReviewFailed {
                            pane,
                            error: error.to_string(),
                        }),
                        scheduler,
                        effects,
                    );
                    return Ok(None);
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
                            "review overview model adapter is not admitted yet".into(),
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
                    let disposition = if decision.markdown.contains("## Review: Approved") {
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
                        .map_err(|error| crate::review::ReviewError::Feedback(error.to_string()))?;
                    let Response::ReviewFeedback(feedback) = feedback else {
                        return Err(crate::review::ReviewError::Feedback(
                            "host returned an invalid review feedback receipt".into(),
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
        effect => return Ok(Some(effect)),
    }
    Ok(None)
}
fn handle_pending_effect(pane: PaneId, effect: RootEffect, context: &mut EffectContext<'_>) {
    match effect {
        RootEffect::Fork => {}
        RootEffect::Shutdown => context.effects.push_back(AppEffect::Shutdown),
        pending => {
            context.app.update(AppEvent::NotifyError {
                pane,
                error: format!(
                    "This action is waiting for its host capability adapter: {pending:?}"
                ),
            });
            context.scheduler.request_immediate(Instant::now());
        }
    }
}

pub(super) fn schedule(
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

pub(super) fn render_if_due(
    scheduler: &mut RenderScheduler,
    terminal: &mut super::terminal::TerminalSession,
    app: &mut super::components::AppNode,
) -> Result<()> {
    let now = Instant::now();
    if scheduler.is_due(now) {
        terminal
            .draw(|frame| app.render(frame))
            .map_err(RuntimeError::Terminal)?;
        scheduler.presented(now);
    }
    Ok(())
}

pub(super) fn next_deadline(
    scheduler: &RenderScheduler,
    animation: Option<Instant>,
) -> Option<Instant> {
    scheduler.deadline().into_iter().chain(animation).min()
}
