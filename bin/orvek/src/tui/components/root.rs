//! Root layout and component event routing.

use super::{
    actions::{Action, ActionAvailability, ActionsEffect, ActionsEvent, ActionsMenu},
    activity::ActivityState,
    activity_mark::ActivityMark,
    animation::DeadlineSet,
    composer::{Composer, ComposerChromeTarget, ComposerDraft, ComposerEffect, ComposerEvent},
    context_diagnostics::{
        ContextDiagnosticsEffect, ContextDiagnosticsEvent, ContextDiagnosticsPanel,
    },
    dialog::{ConfirmationChoice, ConfirmationDialog, KeyConfirmationLabels, TimedKeyConfirmation},
    effort::{EffortEffect, EffortEvent, EffortSelector},
    file_finder::{FileFinder, FileFinderEffect, FileFinderEvent},
    keybindings::{KeybindingsEffect, KeybindingsEvent, KeybindingsHelp},
    layout::RootLayout,
    memory::{MemoryBrowser, MemoryBrowserEffect, MemoryBrowserEvent},
    model_selector::{ModelSelector, ModelSelectorEffect, ModelSelectorEvent},
    node::{Component, ComponentUpdate, Node, RenderRequest},
    queue::{MessageQueue, QueueEffect, QueueEvent, QueueId, QueuedInput},
    recent_prompt_picker::{RecentPromptPicker, RecentPromptPickerEffect, RecentPromptPickerEvent},
    review_confirmation::review_download_confirmation,
    selection::{Selection, Surface, TextSpan},
    session_picker::{SessionPicker, SessionPickerEffect, SessionPickerEvent, SessionPickerMode},
    skill_picker::{SkillPicker, SkillPickerEffect, SkillPickerEvent},
    subagents::{SubagentEffect, SubagentOverlay, SubagentTree},
    theme_selector::{ThemeSelector, ThemeSelectorEffect, ThemeSelectorEvent},
    toast::{Toast, ToastStack},
    transcript::{ScrollCommand, Transcript, TranscriptEvent},
};
use crate::{
    app::config::{ReasoningEffort, ReasoningMode},
    core::extensions::Skill,
    tui::{
        children::{ChildUpdate, MessageOrigin},
        context::{ContextDiagnostics, SessionCost},
        prompt::Submission,
        session::{MAX_RECENT_PROMPTS, RecentPrompt, SessionSummary},
        theme::{FeedbackTone, Theme, ThemeMode},
        transcript::TranscriptRecord,
    },
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orvek_harness::inference::Model;
use orvek_memory::{MemoryAccess, MemoryKey, MemoryRecord, MemorySource};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
};
use semver::Version;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const KEY_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(2);
const SELECTION_SCROLL_INTERVAL: Duration = Duration::from_millis(60);

#[derive(Clone, Copy, Eq, PartialEq)]
enum ConfirmationAction {
    Interrupt,
    CancelReview,
    Exit,
}

impl ConfirmationAction {
    const fn title_key(self) -> &'static str {
        match self {
            Self::Interrupt => "Esc",
            Self::CancelReview => "Esc",
            Self::Exit => "Ctrl+C",
        }
    }

    const fn action_label(self) -> &'static str {
        match self {
            Self::Interrupt => "Interrupt",
            Self::CancelReview => "Cancel review",
            Self::Exit => "Quit",
        }
    }

    const fn effect(self) -> RootEffect {
        match self {
            Self::Interrupt => RootEffect::CancelTurns,
            Self::CancelReview => RootEffect::CancelReview,
            Self::Exit => RootEffect::Shutdown,
        }
    }
}

struct SelectionAutoScroll {
    direction: isize,
    position: Position,
    deadline: Instant,
}

pub(crate) enum RootEvent {
    SettingsConfirmed {
        model: orvek_harness::inference::ModelSettings,
        preferred: ReasoningMode,
    },
    QueueEditReady {
        id: QueueId,
        expected_input: orvek_harness::Digest,
        prompt: Submission,
    },
    QueueChanged(Vec<QueuedInput>),
    SubmissionAcknowledged(Submission),
    SubmissionFailed {
        uncertain: bool,
        error: String,
    },
    Terminal(Event),
    PasteImage(String),
    #[cfg(test)]
    ContextTokens(u64),
    Transcript(Arc<TranscriptRecord>),
    SessionCost(SessionCost),
    ViewDisconnected,
    Subagent(ChildUpdate),
    ReplaceDraft(String),
    HandoffFinished(String),
    HandoffCancelled,
    HandoffFailed(String),
    ReviewStarted,
    ReviewReady(String),
    ReviewCancelled,
    ReviewFinished {
        markdown: String,
        feedback: Option<orvek_harness::Digest>,
    },
    ReviewFailed(String),
    ShellFinished,
    TurnsCancelled,
    ForkReady,
    NewSessionFailed(String),
    SessionsLoaded(Vec<SessionSummary>),
    RecentPromptsLoaded {
        session_id: String,
        prompts: Vec<RecentPrompt>,
    },
    RecentPromptLoadFailed(String),
    SessionLoadFailed(String),
    MemoriesLoaded {
        access: MemoryAccess,
        records: Vec<MemoryRecord>,
    },
    MemoryLoadFailed {
        source: MemorySource,
        access: Option<MemoryAccess>,
        error: String,
    },
    MemoryDeleted {
        key: MemoryKey,
    },
    MemoryDeleteFailed {
        error: String,
        conflict: bool,
    },
    NotifyError(String),
    NotifySuccess(String),
    ConfirmReviewDownload,
    UpdateAvailable(Version),
    AnimationFrame(Instant),
}

pub(crate) struct RestoredSessionProjection {
    transcript: Transcript,
    context_diagnostics: ContextDiagnostics,
    context_tokens: Option<u64>,
    recent_prompts: Vec<RecentPromptDraft>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecentPromptDraft {
    pub(crate) text: String,
    pub(crate) recorded_at_unix_ms: u64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum SessionListKind {
    Resume,
    Mention,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum RootEffect {
    Submit(Submission),
    RetrySubmission,
    Reflect(Submission),
    RunShell(String),
    OpenDraftEditor,
    OpenConfigEditor,
    OpenLink(String),
    OpenFile(PathBuf),
    ReloadConfig,
    NewSession(Model),
    LoadSessions(SessionListKind),
    LoadRecentPrompts(Vec<RecentPromptDraft>),
    LoadMemories,
    DeleteMemory(MemoryKey),
    ResumeSession(String),
    Steer {
        id: QueueId,
        expected_input: orvek_harness::Digest,
    },
    EditQueued {
        id: QueueId,
        expected_input: orvek_harness::Digest,
    },
    ReplaceQueued {
        id: QueueId,
        expected_input: orvek_harness::Digest,
        prompt: Submission,
    },
    RemoveQueued {
        id: QueueId,
    },
    MoveQueued {
        id: QueueId,
        expected_input: orvek_harness::Digest,
        before: Option<QueueId>,
    },
    Copy(String),
    Handoff,
    Review {
        download_assets: bool,
    },
    SetEffort {
        effort: ReasoningEffort,
        reasoning_mode: ReasoningMode,
    },
    SetModel(Model),
    SetFastMode(bool),
    SetMaxSubagents(usize),
    SetTheme(ThemeMode),
    Fork,
    CancelTurns,
    CancelReview,
    CancelHandoff,
    Shutdown,
}

enum Overlay {
    Actions(Node<ActionsMenu>),
    ContextDiagnostics(Node<ContextDiagnosticsPanel>),
    Effort(Node<EffortSelector>),
    Model(Node<ModelSelector>),
    Theme(Node<ThemeSelector>),
    FileFinder(FileMention),
    Skills(SkillMention),
    Keybindings(Node<KeybindingsHelp>),
    Memory(Node<MemoryBrowser>),
    RecentPrompts(Node<RecentPromptPicker>),
    Sessions(Node<SessionPicker>),
    ReviewDownload(Node<ConfirmationDialog>),
    Subagents(SubagentOverlay),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlockingTask {
    Handoff,
    Review,
}

struct FileMention {
    finder: Node<FileFinder>,
    start: usize,
}

struct SkillMention {
    picker: Node<SkillPicker>,
    start: usize,
}

struct QueueEdit {
    id: QueueId,
    expected_input: orvek_harness::Digest,
    original_draft: Option<ComposerDraft>,
    original_input_mode: Option<String>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ThreadState {
    New,
    Started,
}

#[derive(Clone, Copy)]
pub(crate) enum DraftReset {
    Clear,
    Preserve,
}

/// Owns layout and routing so future screen components do not widen the event loop.
pub(crate) struct RootNode {
    transcript: Node<Transcript>,
    composer: Node<Composer>,
    queue: Node<MessageQueue>,
    workspace: PathBuf,
    overlay: Option<Overlay>,
    thread: ThreadState,
    key_confirmation: Option<TimedKeyConfirmation<ConfirmationAction>>,
    toasts: ToastStack,
    discarded_draft: Option<ComposerDraft>,
    queue_edit: Option<QueueEdit>,
    selection: Selection,
    selection_auto_scroll: Option<SelectionAutoScroll>,
    transcript_area: Rect,
    composer_area: Rect,
    composer_content_area: Rect,
    queue_area: Rect,
    in_flight_turns: usize,
    pending_submission: Option<Submission>,
    pending_reflection: bool,
    submission_uncertain: bool,
    in_flight_shells: usize,
    blocking_task: Option<BlockingTask>,
    activity: ActivityMark,
    activity_outcome: ActivityState,
    transcript_activity: Option<ActivityState>,
    review_url: Option<String>,
    fork_available: bool,
    skills: Arc<[Skill]>,
    memory_enabled: bool,
    interactive: bool,
    theme_mode: ThemeMode,
    preferred_reasoning_mode: ReasoningMode,
    subagents: SubagentTree,
    context_diagnostics: ContextDiagnostics,
    recent_prompts: Vec<RecentPromptDraft>,
    pending_session_mention: Option<usize>,
    reflection_input: bool,
}

impl RootNode {
    pub(crate) fn new(workspace: &Path, thinking: ReasoningEffort) -> Self {
        let mut transcript = Transcript::with_effort(thinking);
        transcript.set_workspace(workspace);
        let mut subagents = SubagentTree::new(thinking);
        subagents.set_workspace(workspace);
        Self {
            transcript: Node::new(transcript),
            composer: Node::new(Composer::new(workspace, thinking)),
            queue: Node::new(MessageQueue::default()),
            workspace: workspace.to_path_buf(),
            overlay: None,
            thread: ThreadState::New,
            key_confirmation: None,
            toasts: ToastStack::default(),
            discarded_draft: None,
            queue_edit: None,
            selection: Selection::default(),
            selection_auto_scroll: None,
            transcript_area: Rect::default(),
            composer_area: Rect::default(),
            composer_content_area: Rect::default(),
            queue_area: Rect::default(),
            in_flight_turns: 0,
            pending_submission: None,
            pending_reflection: false,
            submission_uncertain: false,
            in_flight_shells: 0,
            blocking_task: None,
            activity: ActivityMark::new(Instant::now()),
            activity_outcome: ActivityState::Idle,
            transcript_activity: None,
            review_url: None,
            fork_available: true,
            skills: Arc::from([]),
            memory_enabled: false,
            interactive: true,
            theme_mode: ThemeMode::Auto,
            preferred_reasoning_mode: ReasoningMode::Standard,
            subagents,
            context_diagnostics: ContextDiagnostics::default(),
            recent_prompts: Vec::new(),
            pending_session_mention: None,
            reflection_input: false,
        }
    }

    pub(crate) fn fork(&self, workspace: &Path, thinking: ReasoningEffort) -> Self {
        let mut root = Self::new(workspace, thinking);
        root.transcript = Node::new(self.transcript.component().fork_snapshot());
        root.composer
            .component_mut()
            .update(ComposerEvent::ContextTokens(
                self.composer.component().context_tokens(),
            ));
        root.set_fast_mode(self.composer.component().fast_mode());
        root.set_model(self.composer.component().model());
        root.set_reasoning_modes(
            self.composer.component().reasoning_mode(),
            self.preferred_reasoning_mode,
        );
        root.set_max_subagents(self.subagents.max_subagents());
        root.thread = ThreadState::Started;
        root.fork_available = false;
        root.set_skills(Arc::clone(&self.skills));
        root.memory_enabled = self.memory_enabled;
        root.theme_mode = self.theme_mode;
        root.context_diagnostics = self.context_diagnostics.clone();
        let _ = root
            .composer
            .component_mut()
            .update(ComposerEvent::SessionCost(
                root.context_diagnostics.session_cost(),
            ));
        if let Some(window_tokens) = root.context_diagnostics.model_window_tokens {
            root.set_context_window_tokens(window_tokens);
        }
        root.interactive = false;
        root.composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Forking session…".to_owned()),
                now: Instant::now(),
            });
        root
    }

    pub(crate) fn set_fork_available(&mut self, available: bool) {
        self.fork_available = available;
        let can_fork = self.can_fork();
        if let Some(Overlay::Actions(actions)) = &mut self.overlay {
            actions.component_mut().set_fork_available(can_fork);
        }
    }

    pub(crate) fn set_skills(&mut self, skills: Arc<[Skill]>) {
        self.skills = skills;
        if self.skills.is_empty() && matches!(&self.overlay, Some(Overlay::Skills(_))) {
            self.overlay = None;
        }
    }

    pub(crate) fn set_memory_enabled(&mut self, enabled: bool) {
        self.memory_enabled = enabled;
        if !enabled && matches!(&self.overlay, Some(Overlay::Memory(_))) {
            self.overlay = None;
        }
    }

    pub(crate) fn set_theme_mode(&mut self, mode: ThemeMode) {
        self.theme_mode = mode;
    }

    pub(crate) fn set_fast_mode(&mut self, enabled: bool) {
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetFastMode(enabled));
    }

    pub(crate) fn set_model(&mut self, model: Model) {
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetModel(model));
    }

    pub(crate) fn set_reasoning_modes(&mut self, actual: ReasoningMode, preferred: ReasoningMode) {
        self.preferred_reasoning_mode = preferred;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SetReasoningMode(actual));
    }

    pub(crate) const fn set_preferred_reasoning_mode(&mut self, mode: ReasoningMode) {
        self.preferred_reasoning_mode = mode;
    }

    #[cfg(test)]
    pub(crate) const fn preferred_reasoning_mode(&self) -> ReasoningMode {
        self.preferred_reasoning_mode
    }

    pub(crate) fn set_max_subagents(&mut self, limit: usize) {
        self.subagents.set_max_subagents(limit);
    }

    pub(crate) fn reset_session(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        draft_reset: DraftReset,
    ) {
        let current_draft = self.composer.component_mut().take_draft();
        let previous_discarded_draft = self.discarded_draft.take();
        let replaced_draft = current_draft.is_some() && matches!(draft_reset, DraftReset::Clear);
        let (preserved_draft, discarded_draft) = match draft_reset {
            DraftReset::Clear => (None, current_draft.or(previous_discarded_draft)),
            DraftReset::Preserve => (current_draft, previous_discarded_draft),
        };
        let fork_available = self.fork_available;
        let memory_enabled = self.memory_enabled;
        let theme_mode = self.theme_mode;
        let max_subagents = self.subagents.max_subagents();
        *self = Self::new(workspace, thinking);
        self.set_reasoning_modes(reasoning_mode, preferred_reasoning_mode);
        self.discarded_draft = discarded_draft;
        self.fork_available = fork_available;
        self.memory_enabled = memory_enabled;
        self.theme_mode = theme_mode;
        self.set_max_subagents(max_subagents);
        if let Some(draft) = preserved_draft {
            self.composer.component_mut().restore_draft(draft);
        }
        if replaced_draft {
            self.show_draft_saved();
        }
    }

    #[allow(dead_code, reason = "used by restoration benchmarks and focused tests")]
    pub(crate) fn restore_session(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        fast_mode: bool,
        records: Vec<Arc<TranscriptRecord>>,
    ) {
        let projection = Self::project_session(thinking, records);
        self.install_session_projection(
            workspace,
            thinking,
            reasoning_mode,
            preferred_reasoning_mode,
            fast_mode,
            projection,
        );
    }

    pub(crate) fn project_session(
        thinking: ReasoningEffort,
        records: Vec<Arc<TranscriptRecord>>,
    ) -> RestoredSessionProjection {
        let mut transcript = Transcript::with_effort(thinking);
        let mut context_diagnostics = ContextDiagnostics::default();
        let mut context_tokens = None;
        let mut recent_prompts = Vec::new();
        for record in records {
            if let Some(prompt) = recent_prompt(&record) {
                remember_recent_prompt(&mut recent_prompts, prompt);
            }
            let observation = context_diagnostics.observe(&record);
            if observation.completed_tokens.is_some() {
                context_tokens = observation.completed_tokens;
            }
            let _ = transcript.update(TranscriptEvent::Record(record));
        }
        let _ = transcript.update(TranscriptEvent::AgentStreamClosed);
        RestoredSessionProjection {
            transcript,
            context_diagnostics,
            context_tokens,
            recent_prompts,
        }
    }

    pub(crate) fn install_session_projection(
        &mut self,
        workspace: &Path,
        thinking: ReasoningEffort,
        reasoning_mode: ReasoningMode,
        preferred_reasoning_mode: ReasoningMode,
        fast_mode: bool,
        mut projection: RestoredSessionProjection,
    ) {
        let context_window_tokens = self.context_diagnostics.model_window_tokens;
        self.reset_session(
            workspace,
            thinking,
            reasoning_mode,
            preferred_reasoning_mode,
            DraftReset::Clear,
        );
        self.set_fast_mode(fast_mode);
        projection.transcript.set_workspace(workspace);
        self.transcript = Node::new(projection.transcript);
        self.context_diagnostics = projection.context_diagnostics;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::SessionCost(
                self.context_diagnostics.session_cost(),
            ));
        self.recent_prompts = projection.recent_prompts;
        if let Some(window_tokens) = context_window_tokens {
            self.set_context_window_tokens(window_tokens);
        }
        if let Some(tokens) = projection.context_tokens {
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ContextTokens(tokens));
        }
        self.thread = ThreadState::Started;
    }

    pub(crate) fn set_context_window_tokens(&mut self, window_tokens: u64) {
        self.context_diagnostics.model_window_tokens = Some(window_tokens);
        self.context_diagnostics.request_token_limit = Some(
            orvek_harness::context::projection_token_limit(window_tokens),
        );
        self.composer
            .component_mut()
            .update(ComposerEvent::ContextWindowTokens(window_tokens));
    }

    pub(crate) const fn composer(&self) -> &Composer {
        self.composer.component()
    }

    pub(crate) fn render_focused(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        focused: bool,
    ) {
        self.render_root(frame, area, theme, focused);
    }

    pub(crate) fn animation_deadline(&self) -> Option<Instant> {
        let selector = match &self.overlay {
            Some(Overlay::Effort(selector)) => selector.component().animation_deadline(),
            Some(Overlay::Model(selector)) => selector.component().animation_deadline(),
            _ => None,
        };
        let mut deadlines = DeadlineSet::new();
        deadlines
            .include(selector)
            .include(self.activity.deadline())
            .include(self.transcript.component().animation_deadline())
            .include(self.composer.component().animation_deadline())
            .include(self.queue.component().animation_deadline())
            .include(
                self.key_confirmation
                    .as_ref()
                    .map(TimedKeyConfirmation::deadline),
            )
            .include(self.toasts.deadline())
            .include(
                self.selection_auto_scroll
                    .as_ref()
                    .map(|scroll| scroll.deadline),
            )
            .include(self.subagents.animation_deadline());
        deadlines.earliest()
    }

    fn render_root(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, focused: bool) {
        frame.buffer_mut().set_style(area, Style::reset());
        let desired_composer_height = self.composer.component_mut().desired_height(area.width);
        let layout = RootLayout::calculate(
            area,
            desired_composer_height,
            self.queue.component().desired_height(),
        );
        self.activity
            .set_motion(area.height >= 16 && area.width >= 32);
        let activity = self.activity.visual();
        let transcript_area = layout.transcript;
        let queue_area = layout.queue;
        let composer_area = layout.composer;
        self.transcript_area = transcript_area;
        self.queue_area = queue_area;
        self.composer_area = composer_area;
        self.composer_content_area = layout.composer_content;
        self.transcript.render(frame, transcript_area, theme);
        self.queue.render(frame, queue_area, theme);
        let composer_selection = (self.selection.surface() == Some(Surface::Composer))
            .then(|| self.selection.range())
            .flatten();
        self.composer.component_mut().set_activity(activity);
        self.composer.component_mut().render_focused_with_selection(
            frame,
            composer_area,
            theme,
            focused
                && self.blocking_task.is_none()
                && !self.transcript.component().expandables_focused()
                && (!self.queue.component().focused() || self.queue_edit.is_some()),
            composer_selection,
        );
        if self.selection.surface() == Some(Surface::Transcript)
            && let Some(range) = self.selection.range()
        {
            self.transcript
                .component()
                .render_selection(frame.buffer_mut(), range);
        }
        self.transcript
            .component_mut()
            .render_chrome(frame, transcript_area, theme);
        self.render_overlay(frame, area, theme);
        self.render_transient_surfaces(frame, layout.content, composer_area, theme);
    }

    fn render_overlay(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        match overlay {
            Overlay::Actions(actions) => actions.render(frame, area, theme),
            Overlay::ContextDiagnostics(panel) => panel.render(frame, area, theme),
            Overlay::Effort(selector) => selector.render(frame, area, theme),
            Overlay::Model(selector) => selector.render(frame, area, theme),
            Overlay::Theme(selector) => selector.render(frame, area, theme),
            Overlay::FileFinder(mention) => mention.finder.render(frame, area, theme),
            Overlay::Skills(mention) => mention.picker.render(frame, area, theme),
            Overlay::Keybindings(help) => help.render(frame, area, theme),
            Overlay::Memory(browser) => browser.render(frame, area, theme),
            Overlay::RecentPrompts(picker) => picker.render(frame, area, theme),
            Overlay::Sessions(picker) => picker.render(frame, area, theme),
            Overlay::ReviewDownload(confirmation) => confirmation.render(frame, area, theme),
            Overlay::Subagents(SubagentOverlay::Tree) => {
                self.subagents.render_tree(frame, area, theme);
            }
            Overlay::Subagents(SubagentOverlay::Transcript(id)) => {
                self.subagents.render_transcript(*id, frame, area, theme);
            }
        }
    }

    fn render_transient_surfaces(
        &self,
        frame: &mut Frame<'_>,
        content_area: Rect,
        composer_area: Rect,
        theme: &Theme,
    ) {
        self.toasts.render(frame, content_area, theme);
        let Some(confirmation) = &self.key_confirmation else {
            return;
        };
        let action = confirmation.action();
        confirmation.render(
            frame,
            content_area,
            composer_area,
            theme,
            KeyConfirmationLabels::new(
                action.title_key(),
                action.action_label(),
                if action == ConfirmationAction::Exit {
                    "Esc"
                } else {
                    "Any other key"
                },
            ),
        );
    }

    fn update_terminal(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if matches!(event, Event::Resize(_, _)) {
            self.selection.clear();
            self.selection_auto_scroll = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if is_confirmation_key_repeat(&event) {
            return ComponentUpdate::none();
        }
        if self.reflection_input && is_escape(&event) {
            return self.cancel_reflection();
        }
        if self.blocking_task.is_some() && is_control_c(&event) {
            return self.update_key_confirmation(ConfirmationAction::Exit, Instant::now());
        }
        match self.blocking_task {
            Some(BlockingTask::Review) => return self.update_review_input(event),
            Some(BlockingTask::Handoff) => return self.update_handoff_input(event),
            None => {}
        }
        if is_control_c(&event) {
            if self.overlay.is_none()
                && !self.queue.component().focused()
                && !self.transcript.component().expandables_focused()
                && !self.composer.component().draft().is_empty()
            {
                self.key_confirmation = None;
                return self.discard_draft();
            }
            return self.update_key_confirmation(ConfirmationAction::Exit, Instant::now());
        }
        if is_escape(&event)
            && self
                .key_confirmation
                .as_ref()
                .is_some_and(|confirmation| confirmation.action() == ConfirmationAction::Exit)
        {
            self.key_confirmation = None;
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let confirmation_cleared =
            !is_escape(&event) && !is_key_release(&event) && self.key_confirmation.take().is_some();
        let mut update = self.update_terminal_without_confirmation(event);
        if confirmation_cleared {
            update.render = update.render.max(RenderRequest::Immediate);
        }
        update
    }

    pub(crate) fn refresh_terminal_images(&mut self) {
        self.transcript.component_mut().refresh_terminal_images();
        self.subagents.refresh_terminal_images();
    }

    fn update_terminal_without_confirmation(
        &mut self,
        mut event: Event,
    ) -> ComponentUpdate<RootEffect> {
        if !self.interactive {
            return ComponentUpdate::none();
        }
        if self.queue_edit.is_some() {
            return self.update_queue_editor(event);
        }
        if self.reflection_input && is_plain_enter(&event) {
            return self.submit_reflection();
        }
        if let Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) = self.overlay
            && is_control_key(&event, 'o')
        {
            let render = if self.subagents.toggle_expand_all(id) {
                RenderRequest::Immediate
            } else {
                RenderRequest::None
            };
            return ComponentUpdate::render(render);
        }
        if self.overlay.is_some() {
            return self.update_overlay(event, Instant::now());
        }
        if is_control_key(&event, 'z')
            && !self.queue.component().focused()
            && !self.transcript.component().expandables_focused()
        {
            return self.restore_discarded_draft();
        }
        if is_control_key(&event, 'o') {
            return self.update_transcript(TranscriptEvent::ToggleExpandAll);
        }
        if is_control_key(&event, 's') {
            return self.open_effort();
        }
        if is_control_key(&event, 'd') {
            return self.open_model();
        }
        if is_control_key(&event, 'r') {
            return self.load_recent_prompts();
        }
        if is_control_key(&event, 't') {
            return self.open_fork();
        }
        if is_escape(&event) {
            if self.selection.clear() {
                self.selection_auto_scroll = None;
                self.key_confirmation = None;
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            if self.queue.component().focused() {
                self.key_confirmation = None;
                return self.update_queue(event);
            }
            if self.transcript.component().expandables_focused() {
                self.key_confirmation = None;
                return self.update_transcript(TranscriptEvent::BlurExpandables);
            }
            return self.update_key_confirmation(ConfirmationAction::Interrupt, Instant::now());
        }
        if self.transcript.component().pinned_prompt_clicked(&event) {
            return self.update_transcript(TranscriptEvent::JumpToPinnedPrompt);
        }
        if self.transcript.component().updates_banner_clicked(&event) {
            return self.update_transcript(TranscriptEvent::FollowTail);
        }
        if let Event::Mouse(mouse) = &event
            && matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
            && !mouse.modifiers.contains(KeyModifiers::SHIFT)
        {
            let position = Position::new(mouse.column, mouse.row);
            if self.composer_area.contains(position) {
                let rows = if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                };
                let changed = self
                    .composer
                    .component_mut()
                    .scroll_selection(rows, self.composer_content_area);
                return ComponentUpdate::render(if changed {
                    RenderRequest::Immediate
                } else {
                    RenderRequest::None
                });
            }
            if self.queue_area.contains(position) {
                return self.update_queue(event);
            }
            if self.transcript_area.contains(position)
                && let Some(command) = self.transcript.component().scroll_command(&event)
            {
                return self.update_transcript(TranscriptEvent::Scroll(command));
            }
            return ComponentUpdate::none();
        }
        if let Some(update) = self.update_selection_mouse(&mut event) {
            return update;
        }
        if let Some(destination) = self.transcript.component().link_destination(&event) {
            self.focus_composer();
            let effect = match workspace_file(&destination, &self.workspace) {
                Some(path) => RootEffect::OpenFile(path),
                None => RootEffect::OpenLink(destination.to_string()),
            };
            return ComponentUpdate {
                effects: vec![effect],
                render: RenderRequest::Immediate,
            };
        }
        if let Event::Mouse(mouse) = &event
            && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        {
            let position = Position::new(mouse.column, mouse.row);
            match self.composer.component().chrome_target(position) {
                Some(ComposerChromeTarget::Effort) => return self.open_effort(),
                Some(ComposerChromeTarget::Model) => return self.open_model(),
                Some(ComposerChromeTarget::Subagents) => {
                    self.subagents.open_tree();
                    self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
                    return ComponentUpdate::render(RenderRequest::Immediate);
                }
                None => {}
            }
        }
        if is_focus_toggle(&event) {
            return self.update_focus();
        }
        if is_left_click_in(&event, self.queue_area) {
            let Event::Mouse(mouse) = &event else {
                unreachable!("left click helper only accepts mouse events");
            };
            let _ = self
                .queue
                .component_mut()
                .focus_row(mouse.row, self.queue_area);
            let _ = self
                .transcript
                .component_mut()
                .update(TranscriptEvent::BlurExpandables);
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if is_left_click_in(&event, self.composer_area) {
            self.focus_composer();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Some(command) = self.transcript.component().expandable_command(&event) {
            self.queue.component_mut().set_focused(false);
            return self.update_transcript(TranscriptEvent::Expandable(command));
        }
        if is_left_click(&event) {
            self.focus_composer();
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if self.queue.component().focused() {
            return self.update_queue(event);
        }
        if self.in_flight_turns > 0
            && self.composer.component().draft().is_empty()
            && !self.queue.component().is_empty()
            && !self.queue.component().has_pending_action()
            && is_plain_enter(&event)
        {
            return self.update_queue(event);
        }
        if !self.skills.is_empty()
            && !self.composer.component().draft().starts_with('!')
            && is_skill_picker_trigger(&event)
            && self.composer.component().cursor_is_at_token_boundary()
        {
            let start = self.composer.component().cursor();
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            self.overlay = Some(Overlay::Skills(SkillMention {
                picker: Node::new(SkillPicker::new(Arc::clone(&self.skills))),
                start,
            }));
            return update;
        }
        if is_file_finder_trigger(&event) && self.composer.component().cursor_is_at_token_boundary()
        {
            let start = self.composer.component().cursor();
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            self.overlay = Some(Overlay::FileFinder(FileMention {
                finder: Node::new(FileFinder::new(&self.workspace)),
                start,
            }));
            return update;
        }
        if !self.reflection_input
            && self.composer.component().draft().is_empty()
            && is_actions_trigger(&event)
        {
            let new_session_enabled = self.pending_submission.is_none()
                && self.in_flight_turns == 0
                && self.in_flight_shells == 0
                && self.blocking_task.is_none()
                && self.queue.component().is_empty();
            self.overlay = Some(Overlay::Actions(Node::new(ActionsMenu::new(
                ActionAvailability {
                    new_session: new_session_enabled,
                    fork: self.can_fork(),
                    fast_mode: self.composer.component().fast_mode(),
                    memory: self.memory_enabled,
                    model: self.thread == ThreadState::New,
                },
            ))));
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        if let Some(command) = self.transcript.component().scroll_command(&event) {
            let transcript = self.transcript.update(TranscriptEvent::Scroll(command));
            return ComponentUpdate {
                effects: Vec::new(),
                render: transcript.render,
            };
        }
        if self.transcript.component().expandables_focused() {
            return ComponentUpdate::none();
        }
        self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate)
    }

    fn update_review_input(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_control_key(&event, 't') {
            self.key_confirmation = None;
            return self.open_fork();
        }
        if is_plain_key(&event, 'o')
            && let Some(url) = &self.review_url
        {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::OpenLink(url.clone())],
                render: RenderRequest::None,
            };
        }
        if is_plain_key(&event, 'c')
            && let Some(url) = &self.review_url
        {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::Copy(url.clone())],
                render: RenderRequest::None,
            };
        }
        if is_escape(&event) {
            return self.update_key_confirmation(ConfirmationAction::CancelReview, Instant::now());
        }
        if is_key_release(&event) {
            return ComponentUpdate::none();
        }
        let confirmation_cleared = self.key_confirmation.take().is_some();
        ComponentUpdate::render(if confirmation_cleared {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn update_handoff_input(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_escape(&event) {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![RootEffect::CancelHandoff],
                render: RenderRequest::Immediate,
            };
        }
        if is_key_release(&event) {
            return ComponentUpdate::none();
        }
        let confirmation_cleared = self.key_confirmation.take().is_some();
        ComponentUpdate::render(if confirmation_cleared {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn update_selection_mouse(&mut self, event: &mut Event) -> Option<ComponentUpdate<RootEffect>> {
        let Event::Mouse(mouse) = event else {
            return None;
        };
        let position = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let (surface, span) = self.selection_span_at(position)?;
                self.selection.begin(surface, span);
                self.selection_auto_scroll = None;
                Some(ComponentUpdate::render(RenderRequest::Immediate))
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                let surface = self.selection.surface()?;
                let span = self.selection_span_on(surface, position)?;
                self.selection.drag(span);
                self.begin_selection_auto_scroll(surface, position);
                Some(ComponentUpdate::render(RenderRequest::Immediate))
            }
            MouseEventKind::Up(MouseButton::Left)
                if self.selection.is_active() || self.selection.is_pending() =>
            {
                let surface = self.selection.surface()?;
                self.selection_auto_scroll = None;
                let span = self.selection_span_on(surface, position)?;
                if !self.selection.finish(span) {
                    mouse.kind = MouseEventKind::Down(MouseButton::Left);
                    return None;
                }
                let range = self.selection.take_range()?;
                let text = match surface {
                    Surface::Transcript => self.transcript.component().selection_text(range),
                    Surface::Composer => self.composer.component().selection_text(range),
                };
                Some(ComponentUpdate {
                    effects: text.map(RootEffect::Copy).into_iter().collect(),
                    render: RenderRequest::Immediate,
                })
            }
            _ => None,
        }
    }

    fn selection_span_at(&mut self, position: Position) -> Option<(Surface, TextSpan)> {
        if self.composer_content_area.contains(position) {
            let span = self
                .composer
                .component_mut()
                .selection_span(position, self.composer_content_area)?;
            return Some((Surface::Composer, span));
        }
        if !self.transcript_area.contains(position) {
            return None;
        }
        let span = self.transcript.component().selection_span(position)?;
        Some((Surface::Transcript, span))
    }

    fn selection_span_on(&mut self, surface: Surface, position: Position) -> Option<TextSpan> {
        match surface {
            Surface::Transcript => {
                let position = clamp_to(position, self.transcript_area);
                self.transcript.component().selection_span_nearest(position)
            }
            Surface::Composer => {
                let position = clamp_to(position, self.composer_content_area);
                self.composer
                    .component_mut()
                    .selection_span(position, self.composer_content_area)
            }
        }
    }

    fn begin_selection_auto_scroll(&mut self, surface: Surface, position: Position) {
        let area = match surface {
            Surface::Transcript => self.transcript_area,
            Surface::Composer => self.composer_content_area,
        };
        let direction = if position.y <= area.y {
            -1
        } else if position.y >= area.bottom().saturating_sub(1) {
            1
        } else {
            self.selection_auto_scroll = None;
            return;
        };
        if let Some(scroll) = &mut self.selection_auto_scroll
            && scroll.direction == direction
        {
            scroll.position = position;
            return;
        }
        self.selection_auto_scroll = Some(SelectionAutoScroll {
            direction,
            position,
            deadline: Instant::now() + SELECTION_SCROLL_INTERVAL,
        });
    }

    fn scroll_selected_surface(&mut self, surface: Surface, rows: isize) -> bool {
        match surface {
            Surface::Transcript => {
                self.transcript
                    .update(TranscriptEvent::Scroll(ScrollCommand::Rows(rows as i32)));
                true
            }
            Surface::Composer => self
                .composer
                .component_mut()
                .scroll_selection(rows, self.composer_content_area),
        }
    }

    fn update_key_confirmation(
        &mut self,
        action: ConfirmationAction,
        now: Instant,
    ) -> ComponentUpdate<RootEffect> {
        let confirmed = self
            .key_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.confirms(action, now));
        if confirmed {
            self.key_confirmation = None;
            return ComponentUpdate {
                effects: vec![action.effect()],
                render: RenderRequest::Immediate,
            };
        }
        self.key_confirmation = Some(TimedKeyConfirmation::new(
            action,
            now,
            KEY_CONFIRMATION_TIMEOUT,
        ));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_overlay(&mut self, event: Event, now: Instant) -> ComponentUpdate<RootEffect> {
        match &self.overlay {
            Some(Overlay::Actions(_)) => self.update_actions(event),
            Some(Overlay::ContextDiagnostics(_)) => self.update_context_diagnostics(event),
            Some(Overlay::Effort(_)) => self.update_effort(EffortEvent::Terminal { event, now }),
            Some(Overlay::Model(_)) => {
                self.update_model(ModelSelectorEvent::Terminal { event, now })
            }
            Some(Overlay::Theme(_)) => {
                self.update_theme_selector(ThemeSelectorEvent::Terminal(event))
            }
            Some(Overlay::FileFinder(_)) => self.update_file_finder(event),
            Some(Overlay::Skills(_)) => self.update_skill_picker(event),
            Some(Overlay::Keybindings(_)) => self.update_keybindings(event),
            Some(Overlay::Memory(_)) => self.update_memory(MemoryBrowserEvent::Terminal(event)),
            Some(Overlay::RecentPrompts(_)) => self.update_recent_prompt_picker(event),
            Some(Overlay::Sessions(_)) => self.update_session_picker(event),
            Some(Overlay::ReviewDownload(_)) => self.update_review_confirmation(event),
            Some(Overlay::Subagents(SubagentOverlay::Tree)) => {
                let effect = self.subagents.update_tree(event);
                self.apply_subagent_effect(effect)
            }
            Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) => {
                let effect = self.subagents.update_transcript(*id, event);
                self.apply_subagent_effect(effect)
            }
            None => ComponentUpdate::none(),
        }
    }

    fn apply_subagent_effect(
        &mut self,
        effect: Option<SubagentEffect>,
    ) -> ComponentUpdate<RootEffect> {
        match effect {
            Some(SubagentEffect::Dismiss) => {
                self.subagents.finish_camera_animation();
                self.overlay = None;
            }
            Some(SubagentEffect::Inspect(id)) => {
                self.subagents.finish_camera_animation();
                self.overlay = Some(Overlay::Subagents(SubagentOverlay::Transcript(id)));
            }
            Some(SubagentEffect::Back) => {
                self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
            }
            Some(SubagentEffect::OpenLink(destination)) => {
                return ComponentUpdate {
                    effects: vec![RootEffect::OpenLink(destination)],
                    render: RenderRequest::None,
                };
            }
            Some(SubagentEffect::SetMaxSubagents(limit)) => {
                return ComponentUpdate {
                    effects: vec![RootEffect::SetMaxSubagents(limit)],
                    render: RenderRequest::Immediate,
                };
            }
            None => {}
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_file_finder(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::FileFinder(mention)) = &self.overlay else {
            return ComponentUpdate::none();
        };
        let start = mention.start;

        if is_key_release(&event) {
            return ComponentUpdate::none();
        }

        let starts_session_mention = is_file_finder_trigger(&event)
            && self
                .mention_query(start, '@')
                .is_some_and(|query| query.is_empty());
        if starts_session_mention {
            let composer =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let mut sessions = self.load_session_mentions(start);
            sessions.render = sessions.render.max(composer.render);
            return sessions;
        }

        if is_mention_edit(&event) {
            let keep_open = mention_edit_continues_query(&event, is_file_query_character);
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let query = if keep_open {
                self.mention_query(start, '@')
            } else {
                None
            };
            let Some(query) = query else {
                self.overlay = None;
                return update;
            };
            if let Some(Overlay::FileFinder(mention)) = &mut self.overlay {
                let _ = mention.finder.update(FileFinderEvent::Query(query));
            }
            return update;
        }

        if !is_picker_navigation(&event) {
            self.overlay = None;
            if is_escape(&event) {
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            let mut update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            update.render = update.render.max(RenderRequest::Immediate);
            return update;
        }

        let Some(Overlay::FileFinder(mention)) = &mut self.overlay else {
            unreachable!("file mention was checked above");
        };
        let update = mention.finder.update(FileFinderEvent::Terminal(event));
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            FileFinderEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            FileFinderEffect::Insert(path) => self.update_composer(
                ComposerEvent::ReplaceRange {
                    range: start..self.composer.component().cursor(),
                    text: format!("@{path} "),
                },
                RenderRequest::Immediate,
            ),
        }
    }

    fn update_skill_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Skills(mention)) = &self.overlay else {
            return ComponentUpdate::none();
        };
        let start = mention.start;

        if is_key_release(&event) {
            return ComponentUpdate::none();
        }

        if is_mention_edit(&event) {
            let keep_open = mention_edit_continues_query(&event, is_skill_query_character);
            let update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            let query = if keep_open {
                self.mention_query(start, '$')
            } else {
                None
            };
            let Some(query) = query else {
                self.overlay = None;
                return update;
            };
            if let Some(Overlay::Skills(mention)) = &mut self.overlay {
                let _ = mention.picker.update(SkillPickerEvent::Query(query));
            }
            return update;
        }

        if !is_picker_navigation(&event) {
            self.overlay = None;
            if is_escape(&event) {
                return ComponentUpdate::render(RenderRequest::Immediate);
            }
            let mut update =
                self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate);
            update.render = update.render.max(RenderRequest::Immediate);
            return update;
        }

        let Some(Overlay::Skills(mention)) = &mut self.overlay else {
            unreachable!("skill picker was checked above");
        };
        let update = mention.picker.update(SkillPickerEvent::Terminal(event));
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            SkillPickerEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            SkillPickerEffect::Insert(name) => self.update_composer(
                ComposerEvent::ReplaceRange {
                    range: start..self.composer.component().cursor(),
                    text: format!("${name} "),
                },
                RenderRequest::Immediate,
            ),
        }
    }

    fn mention_query(&self, start: usize, prefix: char) -> Option<String> {
        let composer = self.composer.component();
        composer
            .draft()
            .get(start..composer.cursor())?
            .strip_prefix(prefix)
            .map(str::to_owned)
    }

    fn update_actions(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Actions(actions)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = actions.update(ActionsEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(ActionsEffect::Dismiss) => self.overlay = None,
            Some(ActionsEffect::Trigger(Action::Subagents)) => {
                self.subagents.open_tree();
                self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
            }
            Some(ActionsEffect::Trigger(Action::Effort)) => {
                return self.open_effort();
            }
            Some(ActionsEffect::Trigger(Action::Model)) => {
                return self.open_model();
            }
            Some(ActionsEffect::Trigger(Action::FastMode)) => {
                self.overlay = None;
                let enabled = !self.composer.component().fast_mode();
                self.set_fast_mode(enabled);
                self.begin_settings_successor();
                return ComponentUpdate {
                    effects: vec![RootEffect::SetFastMode(enabled)],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::Theme)) => {
                self.overlay = Some(Overlay::Theme(Node::new(ThemeSelector::new(
                    self.theme_mode,
                ))));
            }
            Some(ActionsEffect::Trigger(Action::NewSession)) => {
                return self.open_new_session();
            }
            Some(ActionsEffect::Trigger(Action::ResumeSession)) => {
                return self.load_sessions();
            }
            Some(ActionsEffect::Trigger(Action::Fork)) => return self.open_fork(),
            Some(ActionsEffect::Trigger(Action::Keybindings)) => {
                self.overlay = Some(Overlay::Keybindings(Node::new(KeybindingsHelp::default())));
            }
            Some(ActionsEffect::Trigger(Action::ReloadConfig)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::ReloadConfig],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::EditConfig)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::OpenConfigEditor],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::Memory)) => {
                if !self.memory_enabled {
                    return ComponentUpdate {
                        effects: Vec::new(),
                        render: update.render,
                    };
                }
                self.overlay = Some(Overlay::Memory(Node::new(MemoryBrowser::new())));
                return ComponentUpdate {
                    effects: vec![RootEffect::LoadMemories],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::DebugContext)) => {
                self.overlay = Some(Overlay::ContextDiagnostics(Node::new(
                    ContextDiagnosticsPanel::new(self.context_diagnostics.clone()),
                )));
            }
            Some(ActionsEffect::Trigger(Action::Reflection)) => {
                self.overlay = None;
                self.reflection_input = true;
                return self.update_composer(
                    ComposerEvent::InputMode(Some(
                        "Reflection instructions · enter start · esc cancel".to_owned(),
                    )),
                    RenderRequest::Immediate,
                );
            }
            Some(ActionsEffect::Trigger(Action::Review)) => {
                self.overlay = None;
                return ComponentUpdate {
                    effects: vec![RootEffect::Review {
                        download_assets: false,
                    }],
                    render: RenderRequest::Immediate,
                };
            }
            Some(ActionsEffect::Trigger(Action::Handoff)) => {
                self.overlay = None;
                self.start_blocking_task(BlockingTask::Handoff);
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: Some("Preparing handoff…".to_owned()),
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                return ComponentUpdate {
                    effects: vec![RootEffect::Handoff],
                    render: waiting.render.max(RenderRequest::Immediate),
                };
            }
            None => {}
        }
        ComponentUpdate {
            effects: Vec::new(),
            render: update.render,
        }
    }

    fn update_context_diagnostics(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = panel.update(ContextDiagnosticsEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(ContextDiagnosticsEffect::Dismiss) => self.overlay = None,
            Some(ContextDiagnosticsEffect::Refresh) => {
                if let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay {
                    panel
                        .component_mut()
                        .replace(self.context_diagnostics.clone());
                }
            }
            None => {}
        }
        ComponentUpdate {
            effects: Vec::new(),
            render: update.render,
        }
    }

    fn update_review_confirmation(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::ReviewDownload(confirmation)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = confirmation.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };
        self.overlay = None;
        match effect {
            ConfirmationChoice::Confirm => ComponentUpdate {
                effects: vec![RootEffect::Review {
                    download_assets: true,
                }],
                render: RenderRequest::Immediate,
            },
            ConfirmationChoice::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
        }
    }

    fn update_memory(&mut self, event: MemoryBrowserEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Memory(browser)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = browser.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        match effect {
            MemoryBrowserEffect::Dismiss => {
                self.overlay = None;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            MemoryBrowserEffect::Refresh => ComponentUpdate {
                effects: vec![RootEffect::LoadMemories],
                render: update.render,
            },
            MemoryBrowserEffect::Delete(key) => ComponentUpdate {
                effects: vec![RootEffect::DeleteMemory(key)],
                render: update.render,
            },
        }
    }

    fn open_effort(&mut self) -> ComponentUpdate<RootEffect> {
        self.overlay = Some(Overlay::Effort(Node::new(EffortSelector::new(
            self.composer.component().effort(),
            self.preferred_reasoning_mode == ReasoningMode::Pro,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn open_model(&mut self) -> ComponentUpdate<RootEffect> {
        if self.thread != ThreadState::New {
            return ComponentUpdate::none();
        }
        self.overlay = Some(Overlay::Model(Node::new(ModelSelector::new(
            self.composer.component().model(),
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_theme_selector(&mut self, event: ThemeSelectorEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Theme(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };
        self.overlay = None;
        match effect {
            ThemeSelectorEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            ThemeSelectorEffect::Apply(mode) => ComponentUpdate {
                effects: vec![RootEffect::SetTheme(mode)],
                render: RenderRequest::Immediate,
            },
        }
    }

    fn open_fork(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.can_fork() {
            return ComponentUpdate::none();
        }
        self.overlay = None;
        ComponentUpdate {
            effects: vec![RootEffect::Fork],
            render: RenderRequest::Immediate,
        }
    }

    fn can_fork(&self) -> bool {
        self.fork_available
    }

    fn open_new_session(&mut self) -> ComponentUpdate<RootEffect> {
        if self.pending_submission.is_some()
            || self.in_flight_turns > 0
            || self.in_flight_shells > 0
            || !self.queue.component().is_empty()
        {
            return ComponentUpdate::none();
        }
        self.overlay = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Starting new session…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::NewSession(self.composer.component().model())],
            render: RenderRequest::Immediate,
        }
    }

    fn begin_settings_successor(&mut self) {
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Starting a new session with these settings…".to_owned()),
                now: Instant::now(),
            });
    }

    pub(super) fn load_sessions(&mut self) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.pending_session_mention = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading sessions…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadSessions(SessionListKind::Resume)],
            render: RenderRequest::Immediate,
        }
    }

    fn load_session_mentions(&mut self, start: usize) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.pending_session_mention = Some(start);
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading sessions…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadSessions(SessionListKind::Mention)],
            render: RenderRequest::Immediate,
        }
    }

    fn load_recent_prompts(&mut self) -> ComponentUpdate<RootEffect> {
        self.overlay = None;
        self.interactive = false;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: true,
                status: Some("Loading recent prompts…".to_owned()),
                now: Instant::now(),
            });
        ComponentUpdate {
            effects: vec![RootEffect::LoadRecentPrompts(self.recent_prompts.clone())],
            render: RenderRequest::Immediate,
        }
    }

    fn recent_prompts_loaded(
        &mut self,
        session_id: String,
        prompts: Vec<RecentPrompt>,
    ) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        self.overlay = Some(Overlay::RecentPrompts(Node::new(RecentPromptPicker::new(
            prompts, session_id,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_recent_prompt_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::RecentPrompts(picker)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = picker.update(RecentPromptPickerEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(RecentPromptPickerEffect::Dismiss) => {
                self.overlay = None;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            Some(RecentPromptPickerEffect::Insert(prompt)) => {
                self.overlay = None;
                self.update_composer(
                    ComposerEvent::ReplaceDraft(prompt),
                    RenderRequest::Immediate,
                )
            }
            None => ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            },
        }
    }

    fn notify(&mut self, message: impl Into<String>, tone: FeedbackTone) {
        self.toasts
            .push(Toast::plain(message, tone, Instant::now()));
    }

    fn recent_prompt_load_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        self.notify(message, FeedbackTone::Error);
        self.update_composer(
            ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            },
            RenderRequest::Immediate,
        )
    }

    fn sessions_loaded(&mut self, sessions: Vec<SessionSummary>) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        let mode = if self.pending_session_mention.is_some() {
            SessionPickerMode::Mention
        } else {
            SessionPickerMode::Resume
        };
        self.overlay = Some(Overlay::Sessions(Node::new(SessionPicker::new(
            sessions, mode,
        ))));
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_session_picker(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Sessions(picker)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = picker.update(SessionPickerEvent::Terminal(event));
        match update.effects.into_iter().next() {
            Some(SessionPickerEffect::Dismiss) => {
                self.overlay = None;
                self.pending_session_mention = None;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            Some(SessionPickerEffect::Resume(session_id)) => {
                self.overlay = None;
                self.interactive = false;
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::Activity {
                        active: true,
                        status: Some("Resuming session…".to_owned()),
                        now: Instant::now(),
                    });
                ComponentUpdate {
                    effects: vec![RootEffect::ResumeSession(session_id)],
                    render: RenderRequest::Immediate,
                }
            }
            Some(SessionPickerEffect::Mention(session_id)) => {
                self.overlay = None;
                let Some(start) = self.pending_session_mention.take() else {
                    return ComponentUpdate::none();
                };
                self.update_composer(
                    ComposerEvent::ReplaceRange {
                        range: start..self.composer.component().cursor(),
                        text: format!("@@{session_id} "),
                    },
                    RenderRequest::Immediate,
                )
            }
            None => ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            },
        }
    }

    fn session_load_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.pending_session_mention = None;
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        self.notify(message, FeedbackTone::Error);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn new_session_failed(&mut self, message: String) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        self.notify(
            format!("Could not start a new session: {message}"),
            FeedbackTone::Error,
        );
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn fork_ready(&mut self) -> ComponentUpdate<RootEffect> {
        self.interactive = true;
        let update = self
            .composer
            .component_mut()
            .update(ComposerEvent::Activity {
                active: false,
                status: None,
                now: Instant::now(),
            });
        debug_assert!(update.changed);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_keybindings(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Keybindings(help)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = help.update(KeybindingsEvent::Terminal(event));
        if matches!(update.effects.as_slice(), [KeybindingsEffect::Dismiss]) {
            self.overlay = None;
        }
        ComponentUpdate::render(update.render)
    }

    fn update_effort(&mut self, event: EffortEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Effort(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            EffortEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            EffortEffect::Apply(effort, pro) => {
                let reasoning_mode = if pro {
                    ReasoningMode::Pro
                } else {
                    ReasoningMode::Standard
                };
                let previous_reasoning_mode = self.preferred_reasoning_mode;
                self.preferred_reasoning_mode = reasoning_mode;
                if reasoning_mode != previous_reasoning_mode {
                    let state = if pro { "enabled" } else { "disabled" };
                    let suffix = if self.composer.component().reasoning_mode() != reasoning_mode {
                        " · start a new session to apply."
                    } else {
                        "."
                    };
                    let message = format!("Pro {state} for new sessions{suffix}");
                    self.notify(message, FeedbackTone::Info);
                }
                self.transcript.component_mut().set_effort(effort);
                self.subagents.set_effort(effort);
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::SetEffort(effort));
                self.begin_settings_successor();
                ComponentUpdate {
                    effects: vec![RootEffect::SetEffort {
                        effort,
                        reasoning_mode,
                    }],
                    render: RenderRequest::Immediate,
                }
            }
        }
    }

    fn update_model(&mut self, event: ModelSelectorEvent) -> ComponentUpdate<RootEffect> {
        let Some(Overlay::Model(selector)) = &mut self.overlay else {
            return ComponentUpdate::none();
        };
        let update = selector.update(event);
        let Some(effect) = update.effects.into_iter().next() else {
            return ComponentUpdate {
                effects: Vec::new(),
                render: update.render,
            };
        };

        self.overlay = None;
        match effect {
            ModelSelectorEffect::Dismiss => ComponentUpdate::render(RenderRequest::Immediate),
            ModelSelectorEffect::Apply(model) if model == self.composer.component().model() => {
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            ModelSelectorEffect::Apply(model) => {
                self.interactive = false;
                let _ = self
                    .composer
                    .component_mut()
                    .update(ComposerEvent::Activity {
                        active: true,
                        status: Some(format!("Starting {} session…", model_name(model))),
                        now: Instant::now(),
                    });
                ComponentUpdate {
                    effects: vec![RootEffect::SetModel(model)],
                    render: RenderRequest::Immediate,
                }
            }
        }
    }

    fn update_focus(&mut self) -> ComponentUpdate<RootEffect> {
        let focus_queue = !self.queue.component().focused() && !self.queue.component().is_empty();
        self.queue.component_mut().set_focused(focus_queue);
        let transcript = self.transcript.update(TranscriptEvent::BlurExpandables);
        ComponentUpdate::render(if focus_queue || transcript.render != RenderRequest::None {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        })
    }

    fn focus_composer(&mut self) {
        self.queue.component_mut().set_focused(false);
        let _ = self
            .transcript
            .component_mut()
            .update(TranscriptEvent::BlurExpandables);
    }

    fn update_queue(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        let update = self.queue.update(QueueEvent::Terminal(event));
        let mut effects = Vec::new();
        let render = update.render;
        for effect in update.effects {
            match effect {
                QueueEffect::Blur => {}
                QueueEffect::Edit {
                    id, expected_input, ..
                } => effects.push(RootEffect::EditQueued { id, expected_input }),
                QueueEffect::Steer { id, expected_input } => {
                    effects.push(RootEffect::Steer { id, expected_input })
                }
                QueueEffect::Remove { id } => effects.push(RootEffect::RemoveQueued { id }),
                QueueEffect::Move {
                    id,
                    expected_input,
                    before,
                } => effects.push(RootEffect::MoveQueued {
                    id,
                    expected_input,
                    before,
                }),
            }
        }
        ComponentUpdate { effects, render }
    }

    fn begin_queue_edit(
        &mut self,
        id: QueueId,
        expected_input: orvek_harness::Digest,
        prompt: Submission,
    ) -> ComponentUpdate<RootEffect> {
        let original_input_mode = self
            .composer
            .component()
            .input_mode()
            .map(ToOwned::to_owned);
        let original_draft = self.composer.component_mut().take_draft();
        self.composer.component_mut().replace_submission(&prompt);
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::InputMode(Some(
                "editing queued message · enter save · esc cancel".to_owned(),
            )));
        self.queue_edit = Some(QueueEdit {
            id,
            expected_input,
            original_draft,
            original_input_mode,
        });
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_queue_editor(&mut self, event: Event) -> ComponentUpdate<RootEffect> {
        if is_escape(&event) {
            return self.finish_queue_edit(false);
        }
        if is_plain_enter(&event) {
            return self.finish_queue_edit(true);
        }
        self.update_composer(ComposerEvent::Terminal(event), RenderRequest::Immediate)
    }

    fn finish_queue_edit(&mut self, save: bool) -> ComponentUpdate<RootEffect> {
        let Some(edit) = self.queue_edit.take() else {
            return ComponentUpdate::none();
        };
        let prompt = self.composer.component().submission();
        self.composer.component_mut().replace_draft(String::new());
        if let Some(draft) = edit.original_draft {
            self.composer.component_mut().restore_draft(draft);
        }
        let _ = self
            .composer
            .component_mut()
            .update(ComposerEvent::InputMode(edit.original_input_mode));

        self.queue.component_mut().cancel_edit(edit.id);
        let effects = if save {
            match prompt {
                Some(prompt) => vec![RootEffect::ReplaceQueued {
                    id: edit.id,
                    expected_input: edit.expected_input,
                    prompt,
                }],
                None => vec![RootEffect::RemoveQueued { id: edit.id }],
            }
        } else {
            Vec::new()
        };
        ComponentUpdate {
            effects,
            render: RenderRequest::Immediate,
        }
    }

    fn update_composer(
        &mut self,
        event: ComposerEvent,
        priority: RenderRequest,
    ) -> ComponentUpdate<RootEffect> {
        if matches!(&event,ComposerEvent::Terminal(event) if is_plain_enter(event))
            && self.pending_submission.is_some()
        {
            let retry = self.submission_uncertain;
            self.submission_uncertain = false;
            return ComponentUpdate {
                effects: if retry {
                    vec![RootEffect::RetrySubmission]
                } else {
                    Vec::new()
                },
                render: RenderRequest::Immediate,
            };
        }
        let update = self.composer.component_mut().update(event);
        let submitted = matches!(&update.effect, Some(ComposerEffect::Submit(_)));
        if submitted {
            self.thread = ThreadState::Started;
        }
        let mut render = if update.changed {
            priority
        } else {
            RenderRequest::None
        };
        if submitted {
            render = render.max(self.update_transcript(TranscriptEvent::FollowTail).render);
        }
        let effects = match update.effect {
            Some(ComposerEffect::Submit(prompt)) => {
                self.pending_reflection = false;
                self.pending_submission = Some(prompt.clone());
                self.submission_uncertain = false;
                vec![RootEffect::Submit(prompt)]
            }
            Some(ComposerEffect::RunShell(command)) => {
                self.in_flight_shells = self.in_flight_shells.saturating_add(1);
                self.refresh_activity(Instant::now());
                vec![RootEffect::RunShell(command)]
            }
            Some(ComposerEffect::OpenDraftEditor) => vec![RootEffect::OpenDraftEditor],
            None => Vec::new(),
        };

        ComponentUpdate { effects, render }
    }

    fn submit_reflection(&mut self) -> ComponentUpdate<RootEffect> {
        let instructions = self
            .composer
            .component()
            .submission()
            .unwrap_or_else(|| Submission::text(String::new()));
        self.reflection_input = false;
        let mode = self.update_composer(ComposerEvent::InputMode(None), RenderRequest::Immediate);
        self.thread = ThreadState::Started;
        self.pending_submission = Some(instructions.clone());
        self.pending_reflection = true;
        self.submission_uncertain = false;
        let transcript = self.update_transcript(TranscriptEvent::FollowTail);
        ComponentUpdate {
            effects: vec![RootEffect::Reflect(instructions)],
            render: mode.render.max(transcript.render),
        }
    }

    fn cancel_reflection(&mut self) -> ComponentUpdate<RootEffect> {
        self.reflection_input = false;
        self.composer.component_mut().replace_draft(String::new());
        self.update_composer(ComposerEvent::InputMode(None), RenderRequest::Immediate)
    }

    fn discard_draft(&mut self) -> ComponentUpdate<RootEffect> {
        let Some(draft) = self.composer.component_mut().take_draft() else {
            return ComponentUpdate::none();
        };
        self.discarded_draft = Some(draft);
        self.show_draft_saved();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn restore_discarded_draft(&mut self) -> ComponentUpdate<RootEffect> {
        if !self.composer.component().draft().is_empty() {
            return ComponentUpdate::none();
        }
        let Some(draft) = self.discarded_draft.take() else {
            return ComponentUpdate::none();
        };
        self.composer.component_mut().restore_draft(draft);
        self.notify("Draft restored.", FeedbackTone::Success);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn show_draft_saved(&mut self) {
        self.notify("Draft cleared · Ctrl+Z to restore", FeedbackTone::Warning);
    }

    fn agent_turn_finished(&mut self) -> ComponentUpdate<RootEffect> {
        self.in_flight_turns = self.in_flight_turns.saturating_sub(1);
        if self.in_flight_turns == 0 {
            self.activity_outcome = ActivityState::Complete;
            self.refresh_activity(Instant::now());
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn turns_cancelled(&mut self) -> ComponentUpdate<RootEffect> {
        self.in_flight_turns = 0;
        self.transcript_activity = None;
        self.activity_outcome = ActivityState::Cancelled;
        self.composer
            .component_mut()
            .update(ComposerEvent::TurnsCleared);
        self.refresh_activity(Instant::now());
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_transcript(&mut self, event: TranscriptEvent) -> ComponentUpdate<RootEffect> {
        let update = self.transcript.update(event);
        let mut render = update.render;
        for effect in update.effects {
            let now = Instant::now();
            self.transcript_activity = effect.state;
            self.refresh_activity(now);
            let composer = self
                .composer
                .component_mut()
                .update(ComposerEvent::Activity {
                    active: effect.active,
                    status: effect.status,
                    now,
                });
            if composer.changed {
                render = render.max(RenderRequest::Streaming);
            }
        }
        ComponentUpdate {
            effects: Vec::new(),
            render,
        }
    }

    fn update_animation(&mut self, now: Instant) -> ComponentUpdate<RootEffect> {
        self.refresh_activity(now);
        let activity = if self.activity.advance(now) {
            RenderRequest::Streaming
        } else {
            RenderRequest::None
        };
        let confirmation = if self
            .key_confirmation
            .as_ref()
            .is_some_and(|confirmation| confirmation.expired(now))
        {
            self.key_confirmation = None;
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        };
        let effort = self.update_effort(EffortEvent::AnimationFrame(now));
        let model = self.update_model(ModelSelectorEvent::AnimationFrame(now));
        let transcript = self.update_transcript(TranscriptEvent::AnimationFrame(now));
        let composer =
            self.update_composer(ComposerEvent::AnimationFrame(now), RenderRequest::Streaming);
        let queue = self.queue.update(QueueEvent::AnimationFrame(now));
        debug_assert!(queue.effects.is_empty());
        let subagents = if self.subagents.advance(now) {
            RenderRequest::Streaming
        } else {
            RenderRequest::None
        };
        let selection = self.update_selection_auto_scroll(now);
        let toasts = if self.toasts.expire(now) {
            RenderRequest::Immediate
        } else {
            RenderRequest::None
        };
        ComponentUpdate {
            effects: effort
                .effects
                .into_iter()
                .chain(model.effects)
                .chain(composer.effects)
                .collect(),
            render: effort
                .render
                .max(model.render)
                .max(transcript.render)
                .max(composer.render)
                .max(queue.render)
                .max(subagents)
                .max(selection)
                .max(activity)
                .max(confirmation)
                .max(toasts),
        }
    }

    fn start_blocking_task(&mut self, task: BlockingTask) {
        self.blocking_task = Some(task);
        self.activity_outcome = ActivityState::Idle;
        self.refresh_activity(Instant::now());
    }

    fn finish_blocking_task(&mut self, outcome: ActivityState) {
        self.blocking_task = None;
        self.activity_outcome = outcome;
        self.refresh_activity(Instant::now());
    }

    /// Projects turn, shell, and overlay activity into the composer outline.
    /// The outcome state persists after a turn settles instead of snapping
    /// straight to idle.
    fn refresh_activity(&mut self, now: Instant) {
        let state = if self.in_flight_shells > 0 || self.subagents.active_count() > 0 {
            ActivityState::Working
        } else if let Some(state) = self.transcript_activity {
            state
        } else if self.in_flight_turns > 0 || self.blocking_task.is_some() {
            ActivityState::Thinking
        } else {
            self.activity_outcome
        };
        self.activity.set_state(state, now);
    }

    fn update_selection_auto_scroll(&mut self, now: Instant) -> RenderRequest {
        let Some(mut scroll) = self.selection_auto_scroll.take() else {
            return RenderRequest::None;
        };
        if now < scroll.deadline {
            self.selection_auto_scroll = Some(scroll);
            return RenderRequest::None;
        }
        let Some(surface) = self.selection.surface() else {
            return RenderRequest::None;
        };
        let Some(span) = self.selection_span_on(surface, scroll.position) else {
            return RenderRequest::None;
        };
        self.selection.drag(span);
        if !self.scroll_selected_surface(surface, scroll.direction) {
            return RenderRequest::None;
        }
        scroll.deadline = now + SELECTION_SCROLL_INTERVAL;
        self.selection_auto_scroll = Some(scroll);
        RenderRequest::Immediate
    }

    fn apply_subagent_update(&mut self, update: ChildUpdate) -> ComponentUpdate<RootEffect> {
        let previous_active = self.subagents.active_count();
        let root_message = match &update {
            ChildUpdate::Message(update)
                if update.thread.messages.iter().any(|message| {
                    message.id == update.message_id
                        && matches!(message.from, MessageOrigin::Child { .. })
                }) =>
            {
                Some(update.clone())
            }
            _ => None,
        };
        let subagents_changed = self.subagents.apply(update);
        let mut result = root_message.map_or_else(ComponentUpdate::none, |update| {
            self.update_transcript(TranscriptEvent::DirectedMessage {
                perspective: MessageOrigin::Root,
                update,
            })
        });
        if !subagents_changed && result.render == RenderRequest::None {
            return result;
        }
        if let Some(Overlay::Subagents(SubagentOverlay::Transcript(id))) = self.overlay
            && !self.subagents.contains(id)
        {
            self.overlay = Some(Overlay::Subagents(SubagentOverlay::Tree));
        }
        let active = self.subagents.active_count();
        if active != previous_active {
            let now = Instant::now();
            self.refresh_activity(now);
            let _ = self
                .composer
                .component_mut()
                .update(ComposerEvent::ActiveSubagents { count: active, now });
        }
        if subagents_changed {
            result.render = result.render.max(RenderRequest::Immediate);
        }
        result
    }
}

impl Component for RootNode {
    type Event = RootEvent;
    type Effect = RootEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            RootEvent::SettingsConfirmed { model, preferred } => {
                self.composer
                    .component_mut()
                    .update(ComposerEvent::SetEffort(model.thinking.into()));
                self.set_fast_mode(model.fast_mode);
                self.set_model(model.model);
                self.set_reasoning_modes(model.reasoning_mode.into(), preferred);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::QueueEditReady {
                id,
                expected_input,
                prompt,
            } => self.begin_queue_edit(id, expected_input, prompt),
            RootEvent::QueueChanged(inputs) => {
                self.queue.component_mut().replace(inputs);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SubmissionAcknowledged(prompt) => {
                if self.pending_submission.as_ref() == Some(&prompt) {
                    self.pending_submission = None;
                    self.submission_uncertain = false;
                    if self.pending_reflection {
                        if self.composer.component().submission().as_ref() == Some(&prompt) {
                            self.composer.component_mut().replace_draft(String::new());
                        }
                    } else {
                        self.composer
                            .component_mut()
                            .acknowledge_submission(&prompt);
                    }
                    self.pending_reflection = false;
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::SubmissionFailed { uncertain, error } => {
                self.submission_uncertain = uncertain;
                self.activity_outcome = ActivityState::Error;
                self.refresh_activity(Instant::now());
                if !uncertain {
                    self.pending_submission = None;
                    if self.pending_reflection {
                        self.reflection_input = true;
                        self.composer
                            .component_mut()
                            .update(ComposerEvent::InputMode(Some(
                                "Reflection instructions · enter retry · esc cancel".into(),
                            )));
                    }
                    self.pending_reflection = false;
                }
                self.notify(error, FeedbackTone::Error);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::Terminal(event) => self.update_terminal(event),
            RootEvent::PasteImage(data_url) => {
                if self.blocking_task.is_some()
                    || self.overlay.is_some()
                    || self.queue.component().focused()
                {
                    ComponentUpdate::none()
                } else {
                    self.update_composer(
                        ComposerEvent::PasteImage(data_url),
                        RenderRequest::Immediate,
                    )
                }
            }
            #[cfg(test)]
            RootEvent::ContextTokens(tokens) => self.update_composer(
                ComposerEvent::ContextTokens(tokens),
                RenderRequest::Streaming,
            ),
            RootEvent::SessionCost(cost) => {
                self.context_diagnostics.restore_session_cost(cost);
                self.update_composer(ComposerEvent::SessionCost(cost), RenderRequest::Immediate)
            }
            RootEvent::Transcript(record) => {
                if let Some(crate::tui::host_projection::ViewChange::RequestStarted { .. }) =
                    record.host()
                {
                    self.in_flight_turns = 1;
                    self.activity_outcome = ActivityState::Thinking;
                    self.refresh_activity(Instant::now());
                    self.thread = ThreadState::Started;
                }
                if let Some(crate::tui::host_projection::ViewChange::Settings(settings)) =
                    record.host()
                {
                    self.composer
                        .component_mut()
                        .update(ComposerEvent::SetEffort(settings.thinking.into()));
                    self.set_fast_mode(settings.fast_mode);
                    self.set_model(settings.model);
                    self.set_reasoning_modes(
                        settings.reasoning_mode.into(),
                        self.preferred_reasoning_mode,
                    );
                }

                if let Some(prompt) = recent_prompt(&record) {
                    remember_recent_prompt(&mut self.recent_prompts, prompt);
                }
                let turn_finished = matches!(
                    record.host(),
                    Some(crate::tui::host_projection::ViewChange::RequestSettled { .. })
                );
                let turn_timer = turn_timer_event(&record);
                let observation = self.context_diagnostics.observe(&record);
                let current_cost = self.context_diagnostics.session_cost();
                let session_cost = self.update_composer(
                    ComposerEvent::SessionCost(current_cost),
                    RenderRequest::Streaming,
                );
                if let Some(Overlay::ContextDiagnostics(panel)) = &mut self.overlay {
                    panel
                        .component_mut()
                        .replace(self.context_diagnostics.clone());
                }
                let mut update = self.update_transcript(TranscriptEvent::Record(record));
                update.effects.extend(session_cost.effects);
                update.render = update.render.max(session_cost.render);
                if let Some(event) = turn_timer {
                    let timer = self.update_composer(event, RenderRequest::Streaming);
                    update.effects.extend(timer.effects);
                    update.render = update.render.max(timer.render);
                }
                if let Some(tokens) = observation.completed_tokens {
                    let context = self.update_composer(
                        ComposerEvent::ContextTokens(tokens),
                        RenderRequest::Streaming,
                    );
                    update.effects.extend(context.effects);
                    update.render = update.render.max(context.render);
                }
                if turn_finished {
                    let finished = self.agent_turn_finished();
                    update.effects.extend(finished.effects);
                    update.render = update.render.max(finished.render);
                }
                update
            }
            RootEvent::ViewDisconnected => {
                self.in_flight_turns = 0;
                self.activity_outcome = ActivityState::Error;
                let mut update = self.update_transcript(TranscriptEvent::AgentStreamClosed);
                self.transcript_activity = None;
                let timer =
                    self.update_composer(ComposerEvent::TurnsCleared, RenderRequest::Immediate);
                update.effects.extend(timer.effects);
                update.render = update.render.max(timer.render);
                self.refresh_activity(Instant::now());
                update
            }
            RootEvent::Subagent(update) => self.apply_subagent_update(update),
            RootEvent::ReplaceDraft(draft) => {
                self.update_composer(ComposerEvent::ReplaceDraft(draft), RenderRequest::Immediate)
            }
            RootEvent::HandoffFinished(prompt) => {
                self.finish_blocking_task(ActivityState::Complete);
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                let mut draft = self.update_composer(
                    ComposerEvent::ReplaceDraft(prompt),
                    RenderRequest::Immediate,
                );
                draft.effects.extend(waiting.effects);
                draft.render = draft.render.max(waiting.render);
                draft
            }
            RootEvent::HandoffCancelled => {
                self.finish_blocking_task(ActivityState::Cancelled);
                self.notify("Handoff cancelled.", FeedbackTone::Warning);
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::HandoffFailed(message) => {
                self.finish_blocking_task(ActivityState::Error);
                self.notify(message, FeedbackTone::Error);
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewStarted => {
                self.start_blocking_task(BlockingTask::Review);
                self.review_url = None;
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewReady(url) => {
                self.review_url = Some(url.clone());
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: true,
                        status: Some("Review ready · O reopen · C copy link".to_owned()),
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewFinished { markdown, feedback } => {
                self.finish_blocking_task(ActivityState::Complete);
                self.review_url = None;
                let waiting = self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                );
                let cursor = self.composer.component().cursor();
                let draft = self.composer.component().draft();
                let before = if draft[..cursor].is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                let after = if draft[cursor..].is_empty() {
                    ""
                } else {
                    "\n\n"
                };
                let inserted = format!("{before}{markdown}{after}");
                let mut update = self.update_composer(
                    ComposerEvent::ReplaceRange {
                        range: cursor..cursor,
                        text: inserted,
                    },
                    RenderRequest::Immediate,
                );
                update.effects.extend(waiting.effects);
                update.render = update.render.max(waiting.render);
                if let Some(feedback) = feedback {
                    self.composer.component_mut().attach_review(feedback);
                }
                update
            }
            RootEvent::ReviewCancelled => {
                self.finish_blocking_task(ActivityState::Cancelled);
                self.review_url = None;
                self.notify("Review cancelled.", FeedbackTone::Warning);
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ReviewFailed(message) => {
                self.finish_blocking_task(ActivityState::Error);
                self.review_url = None;
                self.notify(message, FeedbackTone::Error);
                self.update_composer(
                    ComposerEvent::ReviewWaiting {
                        waiting: false,
                        status: None,
                        now: Instant::now(),
                    },
                    RenderRequest::Immediate,
                )
            }
            RootEvent::ShellFinished => {
                self.in_flight_shells = self.in_flight_shells.saturating_sub(1);
                self.refresh_activity(Instant::now());
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::TurnsCancelled => self.turns_cancelled(),
            RootEvent::ForkReady => self.fork_ready(),
            RootEvent::NewSessionFailed(message) => self.new_session_failed(message),
            RootEvent::SessionsLoaded(sessions) => self.sessions_loaded(sessions),
            RootEvent::RecentPromptsLoaded {
                session_id,
                prompts,
            } => self.recent_prompts_loaded(session_id, prompts),
            RootEvent::RecentPromptLoadFailed(message) => self.recent_prompt_load_failed(message),
            RootEvent::SessionLoadFailed(message) => self.session_load_failed(message),
            RootEvent::MemoriesLoaded { access, records } => {
                self.update_memory(MemoryBrowserEvent::Loaded { access, records })
            }
            RootEvent::MemoryLoadFailed {
                source,
                access,
                error,
            } => self.update_memory(MemoryBrowserEvent::LoadFailed {
                source,
                access,
                error,
            }),
            RootEvent::MemoryDeleted { key } => {
                self.update_memory(MemoryBrowserEvent::Deleted { key })
            }
            RootEvent::MemoryDeleteFailed { error, conflict } => {
                self.update_memory(MemoryBrowserEvent::DeleteFailed { error, conflict })
            }
            RootEvent::NotifyError(message) => {
                self.notify(message, FeedbackTone::Error);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::NotifySuccess(message) => {
                self.notify(message, FeedbackTone::Success);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::ConfirmReviewDownload => {
                self.overlay = Some(Overlay::ReviewDownload(Node::new(
                    review_download_confirmation(),
                )));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::UpdateAvailable(version) => {
                self.toasts
                    .push(Toast::update_available(version, Instant::now()));
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            RootEvent::AnimationFrame(now) => self.update_animation(now),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.render_root(frame, area, theme, true);
    }
}

fn turn_timer_event(record: &TranscriptRecord) -> Option<ComposerEvent> {
    use crate::tui::host_projection::ViewChange;
    match record.host()? {
        ViewChange::RequestStarted { request } if record.recorded_at_unix_ms() != 0 => {
            Some(ComposerEvent::TurnStarted {
                request: *request,
                elapsed: Duration::ZERO,
                now: Instant::now(),
            })
        }
        ViewChange::RequestSettled { request, .. } => {
            Some(ComposerEvent::TurnFinished { request: *request })
        }
        _ => None,
    }
}

fn remember_recent_prompt(prompts: &mut Vec<RecentPromptDraft>, prompt: RecentPromptDraft) {
    if prompts.len() == MAX_RECENT_PROMPTS {
        prompts.remove(0);
    }
    prompts.push(prompt);
}

fn recent_prompt(record: &TranscriptRecord) -> Option<RecentPromptDraft> {
    if record.recorded_at_unix_ms() == 0 {
        return None;
    }
    let text = match (record.host(), record.local()) {
        (Some(crate::tui::host_projection::ViewChange::User { text, .. }), _) => text,
        (
            _,
            Some(
                crate::tui::transcript::LocalEvent::UserSubmitted { text, .. }
                | crate::tui::transcript::LocalEvent::UserSteered { text },
            ),
        ) => text,
        _ => return None,
    };
    Some(RecentPromptDraft {
        text: text.clone(),
        recorded_at_unix_ms: record.recorded_at_unix_ms(),
    })
}

fn clamp_to(position: Position, area: Rect) -> Position {
    Position::new(
        position.x.clamp(area.x, area.right().saturating_sub(1)),
        position.y.clamp(area.y, area.bottom().saturating_sub(1)),
    )
}

fn is_actions_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('/')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_file_finder_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('@')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_skill_picker_trigger(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char('$')
        && !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

fn is_picker_navigation(event: &Event) -> bool {
    match event {
        // Pointer and terminal lifecycle events belong to the open picker;
        // they must not dismiss it or reach the obscured transcript.
        Event::Mouse(_) | Event::Resize(_, _) | Event::FocusGained | Event::FocusLost => true,
        Event::Key(key) => {
            matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && matches!(
                    key.code,
                    KeyCode::Enter
                        | KeyCode::Tab
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::PageUp
                        | KeyCode::PageDown
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Esc
                )
        }
        _ => false,
    }
}

fn is_mention_edit(event: &Event) -> bool {
    match event {
        Event::Key(key) => {
            matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && (key.code == KeyCode::Backspace
                    || matches!(key.code, KeyCode::Char(_))
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT))
        }
        Event::Paste(_) => true,
        _ => false,
    }
}

fn mention_edit_continues_query(event: &Event, valid: fn(char) -> bool) -> bool {
    match event {
        Event::Key(key) if key.code == KeyCode::Backspace => true,
        Event::Key(key) => {
            matches!(key.code, KeyCode::Char(character) if valid(character))
        }
        Event::Paste(text) => text.chars().all(valid),
        _ => false,
    }
}

/// A destination with no URL scheme that resolves to a real file inside the
/// workspace is a source reference, not a link: open it in the editor rather
/// than handing it to the operating system's generic opener.
fn workspace_file(destination: &str, workspace: &Path) -> Option<PathBuf> {
    if destination.contains("://") || destination.starts_with('#') {
        return None;
    }
    let path = workspace.join(destination);
    path.is_file().then_some(path)
}

fn is_file_query_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/')
}

fn model_name(model: Model) -> &'static str {
    match model {
        Model::Luna => "Luna",
        Model::Terra => "Terra",
        Model::Sol => "Sol",
        Model::Astra => "Astra",
        Model::Glm => "GLM 5.3",
        Model::Spark => "Spark",
    }
}

fn is_skill_query_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-'
}

fn is_focus_toggle(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
}

fn is_left_click_in(event: &Event, area: Rect) -> bool {
    if !is_left_click(event) {
        return false;
    }
    let Event::Mouse(mouse) = event else {
        unreachable!("left click helper only accepts mouse events");
    };
    area.contains(ratatui::layout::Position::new(mouse.column, mouse.row))
}

fn is_left_click(event: &Event) -> bool {
    matches!(
        event,
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left)
    )
}

fn is_control_c(event: &Event) -> bool {
    is_control_key(event, 'c')
}

fn is_confirmation_key_repeat(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    if key.kind != KeyEventKind::Repeat {
        return false;
    }
    is_control_c(event) || is_escape(event)
}

fn is_key_release(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
}

fn is_control_key(event: &Event, character: char) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char(character)
        && key.modifiers.contains(KeyModifiers::CONTROL)
}

fn is_escape(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Esc
        && key.modifiers.is_empty()
}

fn is_plain_enter(event: &Event) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Enter
        && key.modifiers.is_empty()
}

fn is_plain_key(event: &Event, character: char) -> bool {
    let Event::Key(key) = event else {
        return false;
    };
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
        && key.code == KeyCode::Char(character)
        && key.modifiers.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{
        ActivityState, Component, ComposerChromeTarget, ConfirmationAction, DraftReset, Overlay,
        RenderRequest, RootEffect, RootEvent, RootNode, SessionListKind, SubagentOverlay,
        ThreadState, TranscriptEvent,
    };
    use crate::{
        app::config::{ReasoningEffort, ReasoningMode},
        core::extensions::Skill,
        tui::{
            children::{ChildId, ChildStatus, ChildUpdate, ChildView, MessageUpdate},
            fixtures::{self, DisplaySample},
            session::{RecentPrompt, SessionSummary},
            theme::{FeedbackTone, Theme, ThemeMode},
            transcript::{LocalEvent, TranscriptRecord, TurnId},
        },
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    };
    use orvek_harness::inference::Model;
    use orvek_memory::{MemoryAccess, MemoryKey, MemoryRecord, MemorySource};
    use ratatui::{
        Terminal,
        backend::TestBackend,
        layout::Position,
        style::{Color, Modifier},
    };
    use semver::Version;
    use serde_json::json;
    use std::{
        fs,
        path::Path,
        sync::Arc,
        time::{Duration, Instant},
    };

    fn toast_message(toast: &super::super::toast::Toast) -> String {
        toast
            .text(&Theme::default())
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn local_memory_access() -> MemoryAccess {
        MemoryAccess {
            source: MemorySource::Local,
            namespace: None,
            role: None,
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> super::RootEvent {
        super::RootEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    fn memory_record(id: i64, version: u64, content: &str) -> MemoryRecord {
        MemoryRecord {
            key: MemoryKey::local(id, version),
            content: content.to_owned(),
            created_at_ms: 0,
            updated_at_ms: 0,
            last_scanned_at_ms: None,
            scan_count: 0,
            last_used_at_ms: None,
            use_count: 0,
            probation_until_ms: None,
        }
    }

    fn key_with_kind(
        code: KeyCode,
        modifiers: KeyModifiers,
        kind: KeyEventKind,
    ) -> super::RootEvent {
        let mut key = KeyEvent::new(code, modifiers);
        key.kind = kind;
        super::RootEvent::Terminal(Event::Key(key))
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> super::RootEvent {
        super::RootEvent::Terminal(Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    fn text_column(buffer: &ratatui::buffer::Buffer, row: u16, text: &str) -> u16 {
        let symbols = text
            .chars()
            .map(|character| character.to_string())
            .collect::<Vec<_>>();
        let width = u16::try_from(symbols.len()).unwrap();
        (0..=buffer.area.width.saturating_sub(width))
            .find(|&column| {
                symbols.iter().enumerate().all(|(offset, symbol)| {
                    buffer[(column + u16::try_from(offset).unwrap(), row)].symbol() == symbol
                })
            })
            .expect("rendered text should be present")
    }

    fn render_root_text(root: &mut RootNode, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn agent_record(
        sequence: u64,
        kind: DisplaySample,
        payload: serde_json::Value,
    ) -> Arc<TranscriptRecord> {
        Arc::new(fixtures::record(sequence, sequence, kind, payload))
    }

    #[test]
    fn session_shell_uses_the_full_terminal_without_a_header() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let rendered = render_root_text(&mut root, 160, 24);

        assert_eq!(root.transcript_area.x, 0);
        assert_eq!(root.transcript_area.y, 0);
        assert_eq!(root.transcript_area.width, 160);
        assert_eq!(root.composer_area.x, 0);
        assert_eq!(root.composer_area.width, 160);
        assert!(!rendered.contains('▪'));
        assert!(!rendered.contains("Complete"));
    }

    #[test]
    fn only_the_composer_uses_the_palette_background() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        terminal
            .draw(|frame| root.render(frame, frame.area(), &theme))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(40, root.transcript_area.y + 1)].bg, Color::Reset);
        assert_eq!(
            buffer[(40, root.composer_area.y + 1)].bg,
            theme.background()
        );
    }

    #[test]
    fn configured_context_window_reaches_the_composer() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        root.set_context_window_tokens(1_000_000);

        assert!(render_root_text(&mut root, 100, 20).contains("0% / 1000k"));
    }

    #[test]
    fn fork_keeps_the_configured_context_window() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_context_window_tokens(1_000_000);

        let mut fork = root.fork(Path::new("/work"), ReasoningEffort::Medium);

        assert!(render_root_text(&mut fork, 100, 20).contains("0% / 1000k"));
    }

    #[test]
    fn restored_session_keeps_the_configured_context_window() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_context_window_tokens(1_000_000);

        root.restore_session(
            Path::new("/work"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            false,
            Vec::new(),
        );

        assert!(render_root_text(&mut root, 100, 20).contains("0% / 1000k"));
    }

    #[test]
    fn provider_cost_reaches_the_composer_border_immediately() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let completed = agent_record(
            1,
            DisplaySample::Usage,
            json!({
                "usage": {
                    "input_tokens": 1_000,
                    "input_tokens_details": {"cached_tokens": 800},
                    "output_tokens": 50,
                    "output_tokens_details": {"reasoning_tokens": 10},
                    "total_tokens": 1_050,
                    "cost": "0.000000250000000001"
                }
            }),
        );

        root.update(super::RootEvent::Transcript(completed));

        assert!(render_root_text(&mut root, 100, 20).contains("$0.000000250000000001"));
    }

    #[test]
    fn context_diagnostics_tracks_restored_and_live_records() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let outbound = agent_record(
            1,
            DisplaySample::ApiUsage,
            json!({
                "direction": "outbound",
                "phase": "generation",
                "event": {
                    "previous_response_id": "opaque-response-id",
                    "prompt_cache_key": "opaque-cache-key"
                }
            }),
        );
        root.restore_session(
            Path::new("/work"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            false,
            vec![outbound],
        );

        let completed = agent_record(
            2,
            DisplaySample::Usage,
            json!({
                "call_index": 1,
                "model": "gpt-5.6-sol",
                "attempt": 1,
                "connection_generation": 1,
                "status": "completed",
                "duration_ns": 1,
                "time_to_first_event_ns": 1,
                "time_to_first_output_ns": 1,
                "tool_calls": 0,
                "usage": {
                    "input_tokens": 1_000,
                    "input_tokens_details": {"cached_tokens": 800},
                    "output_tokens": 50,
                    "total_tokens": 1_050
                }
            }),
        );
        root.update(super::RootEvent::Transcript(completed));
        assert_eq!(root.context_diagnostics.usage.unwrap().cached_input, 800);

        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "debug context".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(root.overlay, Some(Overlay::ContextDiagnostics(_))));

        root.update(key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert_eq!(root.context_diagnostics.usage.unwrap().total, 1_050);
        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(root.overlay.is_none());
    }

    #[test]
    fn context_diagnostics_show_the_million_token_window_and_input_reservation() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        root.set_context_window_tokens(1_000_000);

        assert_eq!(
            root.context_diagnostics.model_window_tokens,
            Some(1_000_000)
        );
        assert_eq!(root.context_diagnostics.request_token_limit, Some(967_232));
    }

    #[test]
    fn restored_session_keeps_unsettled_host_jobs_active_until_receipt() {
        let mut projection = RootNode::project_session(
            ReasoningEffort::Medium,
            vec![
                agent_record(1, DisplaySample::Start, json!({})),
                agent_record(2, DisplaySample::Start, json!({})),
                agent_record(
                    3,
                    DisplaySample::ToolStart,
                    json!({
                        "call_id": "orphaned-shell",
                        "tool": "exec_command",
                        "arguments": {"cmd": "sleep 100"},
                    }),
                ),
            ],
        );

        let restored = projection
            .transcript
            .update(TranscriptEvent::AgentStreamClosed);
        assert!(restored.effects.is_empty());

        let started = projection
            .transcript
            .update(TranscriptEvent::Record(agent_record(
                4,
                DisplaySample::Start,
                json!({}),
            )));
        assert_eq!(started.effects.len(), 1);
        assert!(started.effects[0].active);
        assert_eq!(started.effects[0].status.as_deref(), Some("Thinking…"));

        let completed = projection
            .transcript
            .update(TranscriptEvent::Record(agent_record(
                5,
                DisplaySample::End,
                json!({"duration_ns": 1_000_000}),
            )));
        assert!(completed.effects.is_empty());
        let settled=projection.transcript.update(TranscriptEvent::Record(agent_record(6,DisplaySample::ToolReturn,json!({"call_id":"orphaned-shell","tool":"exec_command","status":"completed","structured_result":{"exit_code":0}}))));
        assert_eq!(settled.effects.len(), 1);
        assert!(!settled.effects[0].active);
    }

    #[test]
    fn cancellation_clears_active_turn_timers() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));
        assert_eq!(root.composer.component().active_turn_timer_count(), 1);

        root.update(RootEvent::TurnsCancelled);

        assert_eq!(root.composer.component().active_turn_timer_count(), 0);
        assert_eq!(root.in_flight_turns, 0);
        assert_eq!(root.activity.visual().state(), ActivityState::Cancelled);
    }

    #[test]
    fn terminal_view_disconnect_clears_timer_and_marks_error() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));

        root.update(RootEvent::ViewDisconnected);

        assert_eq!(root.composer.component().active_turn_timer_count(), 0);
        assert_eq!(root.in_flight_turns, 0);
        assert_eq!(root.activity.visual().state(), ActivityState::Error);
    }

    #[test]
    fn task_state_drives_the_composer_outline_without_a_header() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));
        assert_eq!(root.activity.visual().state(), ActivityState::Thinking);

        root.update(RootEvent::Transcript(agent_record(
            2,
            DisplaySample::ToolStart,
            json!({
                "call_id": "shell",
                "tool": "exec_command",
                "arguments": {"cmd": "cargo test"},
            }),
        )));
        assert_eq!(root.activity.visual().state(), ActivityState::Working);

        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| root.render(frame, frame.area(), &theme))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().all(|cell| cell.symbol() != "▪"));
        let composer = root.composer_area;
        let border_cells = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "·")
            .collect::<Vec<_>>();
        assert!(!border_cells.is_empty());
        let colors = border_cells
            .iter()
            .map(|cell| cell.fg)
            .collect::<std::collections::HashSet<_>>();
        assert!(colors.len() >= 6);
        assert!(colors.contains(&theme.brand_secondary()));
        assert!(
            border_cells
                .iter()
                .all(|cell| { !cell.modifier.intersects(Modifier::BOLD | Modifier::DIM) })
        );
        assert!((composer.y..composer.bottom()).all(|y| {
            (composer.x..composer.right())
                .all(|x| !["─", "│", "╭", "╮", "╰", "╯"].contains(&buffer[(x, y)].symbol()))
        }));

        assert_eq!(root.composer.component().active_turn_timer_count(), 1);
        root.update(RootEvent::TurnsCancelled);
        assert_eq!(root.activity.visual().state(), ActivityState::Cancelled);
        assert_eq!(root.in_flight_turns, 0);
        assert_eq!(root.composer.component().active_turn_timer_count(), 0);

        let mut completed = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        completed.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));
        completed.update(RootEvent::Transcript(agent_record(
            2,
            DisplaySample::End,
            json!({"duration_ns": 1_000_000}),
        )));
        assert_eq!(completed.activity.visual().state(), ActivityState::Complete);
        let rendered = render_root_text(&mut completed, 80, 20);
        assert_eq!(rendered.matches("Completed").count(), 1);
    }

    #[test]
    fn composer_is_anchored_to_the_bottom() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 7)].symbol(), "·");
        assert_eq!(buffer[(0, 11)].symbol(), "·");
        assert_eq!(buffer[(0, 6)].symbol(), " ");
    }

    #[test]
    fn clicking_composer_chrome_opens_model_effort_and_subagents() {
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let top = root.composer_area.y;
        let model_x = text_column(terminal.backend().buffer(), top, "gpt-5.6-sol");
        let effort_x = text_column(terminal.backend().buffer(), top, "medium");
        assert_eq!(
            root.composer
                .component()
                .chrome_target(Position::new(model_x, top)),
            Some(ComposerChromeTarget::Model)
        );
        assert_eq!(
            root.composer
                .component()
                .chrome_target(Position::new(effort_x, top)),
            Some(ComposerChromeTarget::Effort)
        );

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), model_x, top));
        assert!(matches!(root.overlay, Some(Overlay::Model(_))));

        root.overlay = None;
        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            effort_x,
            top,
        ));
        assert!(matches!(root.overlay, Some(Overlay::Effort(_))));

        root.overlay = None;
        root.update(super::RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(1),
            session_id: "child".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "work".to_owned(),
            parent: None,
        })));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let subagents_x = text_column(terminal.backend().buffer(), top, "1 subagents");

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            subagents_x,
            top,
        ));
        assert!(matches!(
            root.overlay,
            Some(Overlay::Subagents(SubagentOverlay::Tree))
        ));
    }

    #[test]
    fn root_messages_render_once_in_main_and_are_projected_into_child_transcripts() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(1),
            session_id: "child".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "verify ordering".to_owned(),
            parent: None,
        })));
        root.update(RootEvent::Transcript(Arc::new(fixtures::record(
            1,
            1,
            DisplaySample::ToolStart,
            json!({
                "call_id": "message-1",
                "tool": "send_agent_message",
                "arguments": {
                    "child_id": 1,
                    "message": "Please verify the ordering.",
                    "priority": "deferred",
                    "purpose": "coordinate"
                }
            }),
        ))));
        let message =
            serde_json::from_value::<MessageUpdate>(crate::tui::fixtures::native_ids(json!({
                "message_id": 1,
                "thread": {
                    "id": 1,
                    "participants": [
                        {"kind": "root"},
                        {"kind": "agent", "child_id": 1}
                    ],
                    "messages": [{
                        "id": 1,
                        "thread_id": 1,
                        "from": {"kind": "root"},
                        "to": 1,
                        "priority": "deferred",
                        "purpose": "coordinate",
                        "body": "Please verify the ordering."
                    }]
                },
                "delivery": {"state": "delivered", "disposition": "started"}
            })))
            .unwrap();
        root.update(RootEvent::Subagent(ChildUpdate::Message(message)));

        let main = render_root_text(&mut root, 100, 20);

        let mut child = Terminal::new(TestBackend::new(100, 40)).unwrap();
        child
            .draw(|frame| {
                root.subagents.render_transcript(
                    ChildId::new(1),
                    frame,
                    frame.area(),
                    &Theme::default(),
                );
            })
            .unwrap();
        let child = child
            .backend()
            .buffer()
            .content
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(main.matches("Message").count(), 1);
        assert!(child.contains("← Message  root → you"));
        assert!(child.contains("Please verify"));
        assert!(child.contains("ordering."));
    }

    #[test]
    fn peer_messages_are_projected_into_the_main_transcript() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for (id, role) in [(1, "sender"), (2, "recipient")] {
            root.update(RootEvent::Subagent(ChildUpdate::Added(ChildView {
                id: ChildId::new(id),
                session_id: format!("child-{id}"),
                model: Model::Sol,
                role: role.to_owned(),
                task: "coordinate with a peer".to_owned(),
                parent: None,
            })));
        }
        let message =
            serde_json::from_value::<MessageUpdate>(crate::tui::fixtures::native_ids(json!({
                "message_id": 1,
                "thread": {
                    "id": 1,
                    "participants": [
                        {"kind": "agent", "child_id": 1},
                        {"kind": "agent", "child_id": 2}
                    ],
                    "messages": [{
                        "id": 1,
                        "thread_id": 1,
                        "from": {"kind": "agent", "child_id": 1},
                        "to": 2,
                        "priority": "deferred",
                        "purpose": "coordinate",
                        "body": "Peer coordination is visible."
                    }]
                },
                "delivery": {"state": "delivered", "disposition": "started"}
            })))
            .unwrap();

        root.update(RootEvent::Subagent(ChildUpdate::Message(message)));

        let main = render_root_text(&mut root, 100, 20);
        assert!(main.contains("← Message  #1 → #2"));
        assert!(main.contains("Peer coordination is visible."));
    }

    #[test]
    fn composer_hides_subagents_after_they_stop_running() {
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(1),
            session_id: "child".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "work".to_owned(),
            parent: None,
        })));
        root.update(super::RootEvent::Subagent(ChildUpdate::Status {
            id: ChildId::new(1),
            status: ChildStatus::Returned {
                output: orvek_harness::Digest::of(b"{\"report\":\"done\"}"),
            },
        }));

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(!rendered.contains("subagents"));
    }

    #[test]
    fn child_return_never_starts_a_frontend_owned_continuation() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(1),
            session_id: "child".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "inspect the queue".to_owned(),
            parent: None,
        })));

        let update = root.update(super::RootEvent::Subagent(ChildUpdate::Status {
            id: ChildId::new(1),
            status: ChildStatus::Returned {
                output: orvek_harness::Digest::of(b"{\"report\":\"queue is sound\"}"),
            },
        }));

        assert!(update.effects.is_empty());
        assert_eq!(root.in_flight_turns, 0);
    }

    #[test]
    fn completed_subagent_does_not_start_a_competing_active_turn() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.update(super::RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(1),
            session_id: "child".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "inspect the queue".to_owned(),
            parent: None,
        })));

        let update = root.update(super::RootEvent::Subagent(ChildUpdate::Status {
            id: ChildId::new(1),
            status: ChildStatus::Returned {
                output: orvek_harness::Digest::of(b"{\"report\":\"queue is sound\"}"),
            },
        }));

        assert!(update.effects.is_empty());
        assert_eq!(root.in_flight_turns, 1);
    }

    #[test]
    fn completed_nested_subagent_does_not_bypass_its_parent() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::Subagent(ChildUpdate::Added(ChildView {
            id: ChildId::new(2),
            session_id: "grandchild".to_owned(),
            model: Model::Sol,
            role: "worker".to_owned(),
            task: "inspect the queue".to_owned(),
            parent: Some(ChildId::new(1)),
        })));

        let update = root.update(super::RootEvent::Subagent(ChildUpdate::Status {
            id: ChildId::new(2),
            status: ChildStatus::Returned {
                output: orvek_harness::Digest::of(b"{\"report\":\"queue is sound\"}"),
            },
        }));

        assert!(update.effects.is_empty());
        assert_eq!(root.in_flight_turns, 0);
    }

    #[test]
    fn transcript_uses_the_space_above_the_composer() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let record = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: "hello transcript".to_owned(),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(record)));

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!((0..7).any(|y| buffer[(0, y)].symbol() == "╭"));
        assert_eq!(buffer[(0, 7)].symbol(), "·");
    }

    #[test]
    fn clicking_a_pinned_prompt_reveals_its_transcript_entry() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let prompt = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: "jump to this prompt".to_owned(),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(prompt)));
        root.update(super::RootEvent::Transcript(agent_record(
            2,
            DisplaySample::Text,
            json!({
                "model_call_index": 1,
                "item_id": "answer",
                "phase": "final_answer",
                "text": (1..=40)
                    .map(|line| format!("answer {line}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            }),
        )));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(mouse(MouseEventKind::ScrollUp, 5, 2));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let pinned_row = (0..root.transcript_area.height)
            .find(|&row| {
                terminal.backend().buffer()[(5, row)].bg == Theme::default().code_background()
            })
            .expect("pinned prompt should be visible");

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            5,
            pinned_row,
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let prompt_row = (0..root.transcript_area.height)
            .find(|&row| {
                (0..40)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("jump to this prompt")
            })
            .expect("clicked prompt should be revealed in the transcript");
        let prompt_column = text_column(
            terminal.backend().buffer(),
            prompt_row,
            "jump to this prompt",
        );
        assert_ne!(
            terminal.backend().buffer()[(prompt_column, prompt_row)].bg,
            Theme::default().code_background()
        );
    }

    #[test]
    fn wheel_targets_the_hovered_surface_without_changing_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=20 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        let draft = (1..=15)
            .map(|row| format!("draft {row}"))
            .collect::<Vec<_>>()
            .join("\n");
        root.composer.component_mut().replace_draft(draft.clone());
        let before = render_root_text(&mut root, 60, 25);
        assert!(before.contains("draft 15"));
        let cursor = root.composer.component().cursor();
        let content = root.composer_content_area;
        root.update(mouse(MouseEventKind::ScrollUp, content.x, content.y));
        let scrolled = render_root_text(&mut root, 60, 25);
        assert!(!scrolled.contains("draft 15"));
        assert!(!scrolled.contains("Scrolled up"));
        assert_eq!(root.composer.component().draft(), draft);
        assert_eq!(root.composer.component().cursor(), cursor);

        let transcript = root.transcript_area;
        root.update(mouse(MouseEventKind::ScrollUp, transcript.x, transcript.y));
        let detached = render_root_text(&mut root, 60, 25);
        assert!(detached.contains("Scrolled up"));
        assert!(!detached.contains("draft 15"));

        root.update(mouse(MouseEventKind::ScrollDown, content.x, content.y));
        let restored = render_root_text(&mut root, 60, 25);
        assert!(restored.contains("draft 15"));
        assert!(restored.contains("Scrolled up"));
    }

    #[test]
    fn detached_banner_can_follow_without_new_updates() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=20 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        render_root_text(&mut root, 60, 20);
        root.update(key(KeyCode::PageUp, KeyModifiers::NONE));
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let row = root.transcript_area.bottom() - 1;
        let column = text_column(terminal.backend().buffer(), row, "Scrolled up");
        root.update(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
        let followed = render_root_text(&mut root, 60, 20);
        assert!(!followed.contains("Scrolled up"));
        assert!(followed.contains("prompt 20"));
    }

    #[test]
    fn clicking_the_updates_banner_returns_to_the_transcript_tail() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=20 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(key(KeyCode::PageUp, KeyModifiers::NONE));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let latest = TranscriptRecord::from_local(
            21,
            21,
            LocalEvent::UserSubmitted {
                id: TurnId::new(21),
                text: "latest prompt".to_owned(),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(latest)));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let banner_row = root.transcript_area.bottom() - 1;
        let banner_column = text_column(terminal.backend().buffer(), banner_row, "1 update");

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            banner_column,
            banner_row,
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("latest prompt"));
        assert!(!rendered.contains("1 update"));
    }

    #[test]
    fn clicking_a_transcript_link_requests_that_it_be_opened() {
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let record = fixtures::record(
            1,
            1,
            DisplaySample::Text,
            json!({
                "model_call_index": 1,
                "item_id": "answer",
                "phase": "final_answer",
                "text": "Open [the site](https://example.com).",
            }),
        );
        root.update(super::RootEvent::Transcript(Arc::new(record)));
        root.queue.component_mut().push("queued".to_owned());
        root.queue.component_mut().set_focused(true);
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let (column, row) = (0..buffer.area.height)
            .find_map(|row| {
                let rendered = (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>();
                rendered
                    .find("the site")
                    .map(|column| (u16::try_from(column).unwrap(), row))
            })
            .expect("link label should be rendered");

        let down = root.update(mouse(MouseEventKind::Down(MouseButton::Left), column, row));
        assert!(down.effects.is_empty());
        let up = root.update(mouse(MouseEventKind::Up(MouseButton::Left), column, row));

        assert_eq!(
            up.effects,
            [RootEffect::OpenLink("https://example.com".to_owned())]
        );
        assert!(!root.queue.component().focused());
    }

    #[test]
    fn submitting_a_prompt_returns_the_transcript_to_the_tail() {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=20 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("old prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(key(KeyCode::PageUp, KeyModifiers::NONE));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        for character in "new prompt".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            submitted.effects,
            [RootEffect::Submit("new prompt".to_owned().into())]
        );
        let record = TranscriptRecord::from_local(
            21,
            21,
            LocalEvent::UserSubmitted {
                id: TurnId::new(21),
                text: "new prompt".to_owned(),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(record)));

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(rendered.contains("new prompt"));
    }

    #[test]
    fn leading_slash_opens_actions_without_changing_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let update = root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

        assert!(matches!(&root.overlay, Some(Overlay::Actions(_))));
        assert!(root.composer().draft().is_empty());
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn slash_after_prompt_text_remains_in_the_composer() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('a'), KeyModifiers::NONE));

        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "a/");
    }

    #[test]
    fn dollar_at_a_token_boundary_opens_skills_and_remains_in_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_skills(
            vec![Skill::new(
                "autofix",
                "Review and repair a pull request until clean.",
            )]
            .into(),
        );
        for character in "use ".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let update = root.update(key(KeyCode::Char('$'), KeyModifiers::NONE));

        assert!(matches!(&root.overlay, Some(Overlay::Skills(_))));
        assert_eq!(root.composer().draft(), "use $");
        assert_eq!(update.render, RenderRequest::Immediate);
        let rendered = render_root_text(&mut root, 90, 20);
        assert!(rendered.contains("$autofix"));
        assert!(rendered.contains("Review and repair a pull request until clean."));
    }

    #[test]
    fn dollar_is_literal_without_available_skills_or_inside_a_token() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('$'), KeyModifiers::NONE));
        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "$");

        root.composer.component_mut().replace_draft(String::new());
        root.set_skills(vec![Skill::new("autofix", "Repair a pull request.")].into());
        for character in "price$5".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "price$5");
    }

    #[test]
    fn dollar_is_literal_in_shell_mode() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_skills(vec![Skill::new("autofix", "Repair a pull request.")].into());

        for character in "!echo $PATH".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "!echo $PATH");

        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            submitted.effects,
            [RootEffect::RunShell("echo $PATH".to_owned())]
        );
    }

    fn assert_skill_selection(key_code: KeyCode) {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_skills(
            vec![
                Skill::new("autofix", "Repair a pull request."),
                Skill::new("open-docs", "Open documentation."),
            ]
            .into(),
        );
        for character in "use later".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        for _ in 0.."later".len() {
            root.update(key(KeyCode::Left, KeyModifiers::NONE));
        }
        for character in "$auto".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(key_code, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "use $autofix later");
    }

    #[test]
    fn enter_selects_a_filtered_skill_at_the_composer_cursor() {
        assert_skill_selection(KeyCode::Enter);
    }

    #[test]
    fn tab_selects_a_filtered_skill_at_the_composer_cursor() {
        assert_skill_selection(KeyCode::Tab);
    }

    #[test]
    fn escape_preserves_a_literal_skill_query() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_skills(vec![Skill::new("autofix", "Repair a pull request.")].into());
        for character in "$auto".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "$auto");
    }

    #[test]
    fn mouse_movement_preserves_mention_popovers() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));

        let file_update = root.update(mouse(MouseEventKind::Moved, 0, 0));

        assert!(root.overlay.is_some());
        assert_eq!(file_update.render, RenderRequest::None);

        root.set_skills(vec![Skill::new("autofix", "Repair a pull request.")].into());
        root.update(key(KeyCode::Char(' '), KeyModifiers::NONE));
        root.update(key(KeyCode::Char('$'), KeyModifiers::NONE));

        let skill_update = root.update(mouse(MouseEventKind::Moved, 0, 0));

        assert!(root.overlay.is_some());
        assert_eq!(skill_update.render, RenderRequest::None);
    }

    #[test]
    fn at_at_a_token_boundary_opens_the_file_finder_and_remains_in_the_draft() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "inspect ".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let update = root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));

        assert!(matches!(&root.overlay, Some(Overlay::FileFinder(_))));
        assert_eq!(root.composer().draft(), "inspect @");
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn a_long_queue_leaves_room_for_the_transcript() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for index in 0..30 {
            root.queue.component_mut().push(format!("queued {index}"));
        }
        render_root_text(&mut root, 60, 24);
        assert!(root.transcript_area.height >= 10);
        assert!(root.queue_area.height >= 3);
        assert!(root.queue_area.height <= 8);
        let draft = root.composer().draft().to_owned();
        let area = root.queue_area;
        let update = root.update(mouse(MouseEventKind::ScrollUp, area.x + 2, area.y + 1));
        assert!(update.effects.is_empty());
        assert_eq!(root.composer().draft(), draft);
        assert!(!render_root_text(&mut root, 60, 24).contains("Scrolled up"));
    }

    #[test]
    fn pointer_and_resize_events_keep_file_suggestions_open() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));
        render_root_text(&mut root, 60, 20);
        for event in [
            mouse(MouseEventKind::ScrollDown, 20, 6),
            mouse(MouseEventKind::Moved, 20, 6),
            RootEvent::Terminal(Event::Resize(50, 18)),
            key(KeyCode::PageDown, KeyModifiers::NONE),
        ] {
            let update = root.update(event);
            assert!(matches!(&root.overlay, Some(Overlay::FileFinder(_))));
            assert!(update.effects.is_empty());
            assert_eq!(root.composer().draft(), "@");
        }
    }

    #[test]
    fn releasing_at_keeps_the_file_finder_open() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));

        let update = root.update(key_with_kind(
            KeyCode::Char('@'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));

        assert!(matches!(&root.overlay, Some(Overlay::FileFinder(_))));
        assert!(update.effects.is_empty());
        assert_eq!(update.render, super::RenderRequest::None);
    }

    #[test]
    fn second_at_switches_from_files_to_session_mentions() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "compare ".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));

        let loading = root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));

        assert_eq!(root.composer().draft(), "compare @@");
        assert!(root.overlay.is_none());
        assert_eq!(
            loading.effects,
            [RootEffect::LoadSessions(SessionListKind::Mention)]
        );
        root.update(RootEvent::SessionsLoaded(vec![SessionSummary {
            session_id: "session-123".to_owned(),
            started_at_unix_ms: 1,
            model: "model".to_owned(),
            effort: Some(ReasoningEffort::Medium),
            reasoning_mode: Some(ReasoningMode::Standard),
            workspace: workspace.path().to_path_buf(),
            preview: "earlier investigation".to_owned(),
        }]));
        assert!(matches!(&root.overlay, Some(Overlay::Sessions(_))));

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "compare @@session-123 ");
    }

    #[test]
    fn later_at_closes_file_suggestions_without_opening_sessions() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "@someone@".chars() {
            let update = root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
            assert!(update.effects.is_empty());
        }

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "@someone@");
    }

    #[test]
    fn at_inside_a_token_is_inserted_without_opening_the_file_finder() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "name@example.com".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "name@example.com");
    }

    fn assert_file_selection(key_code: KeyCode) {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("notes.md"), "remember this").unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "inspect ".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));
        for character in "notes".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(key_code, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "inspect @notes.md ");
    }

    #[test]
    fn enter_selects_a_file_at_the_composer_cursor() {
        assert_file_selection(KeyCode::Enter);
    }

    #[test]
    fn tab_selects_a_file_at_the_composer_cursor() {
        assert_file_selection(KeyCode::Tab);
    }

    #[test]
    fn selecting_a_file_replaces_the_query_in_the_middle_of_a_draft() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("notes.md"), "remember this").unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);
        for character in "inspect later".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        for _ in 0.."later".len() {
            root.update(key(KeyCode::Left, KeyModifiers::NONE));
        }
        for character in "@notes".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "inspect @notes.md later");
    }

    #[test]
    fn escape_preserves_a_literal_mention_and_backspace_removes_it() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);

        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));
        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "@");

        root.update(key(KeyCode::Backspace, KeyModifiers::NONE));
        root.update(key(KeyCode::Char('@'), KeyModifiers::NONE));
        root.update(key(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(root.overlay.is_none());
        assert!(root.composer().draft().is_empty());
    }

    #[test]
    fn mention_query_is_composer_text_and_space_closes_suggestions() {
        let workspace = tempfile::tempdir().unwrap();
        let mut root = RootNode::new(workspace.path(), ReasoningEffort::Medium);

        for character in "@someone ".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "@someone ");
    }

    #[test]
    fn escape_and_empty_backspace_close_actions_immediately() {
        for dismiss in [KeyCode::Esc, KeyCode::Backspace] {
            let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
            root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

            let update = root.update(key(dismiss, KeyModifiers::NONE));

            assert!(root.overlay.is_none());
            assert_eq!(update.render, super::RenderRequest::Immediate);
        }
    }

    #[test]
    fn control_c_requires_confirmation_while_actions_are_open() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

        let first = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(first.effects.is_empty());
        assert!(root.overlay.is_some());
        let rendered = render_root_text(&mut root, 60, 12);
        assert!(rendered.contains("Ctrl+C then"));
        assert!(rendered.contains("Ctrl+C Quit"));
        assert!(rendered.contains("Esc cancel"));

        let second = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(second.effects, [super::RootEffect::Shutdown]);
    }

    #[test]
    fn escape_cancels_a_pending_exit() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        let cancel = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        let next_control_c = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(cancel.effects.is_empty());
        assert_eq!(cancel.render, super::RenderRequest::Immediate);
        assert!(next_control_c.effects.is_empty());
    }

    #[test]
    fn control_c_requires_two_distinct_presses() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        let release = root.update(key_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        ));
        let repeat = root.update(key_with_kind(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            KeyEventKind::Repeat,
        ));

        assert!(release.effects.is_empty());
        assert!(repeat.effects.is_empty());
        assert!(root.key_confirmation.is_some());

        let second_press = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(second_press.effects, [RootEffect::Shutdown]);
    }

    #[test]
    fn confirmation_floats_above_the_composer_top_right() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let popup_bottom = root.composer_area.y - 2;
        assert_eq!(
            buffer[(root.composer_area.right() - 28, popup_bottom)].symbol(),
            "╰"
        );
        assert_eq!(
            buffer[(root.composer_area.right() - 1, popup_bottom)].symbol(),
            "╯"
        );
        assert_eq!(
            buffer[(root.composer_area.right() - 1, root.composer_area.y)].symbol(),
            "·"
        );
    }

    #[test]
    fn control_c_clears_the_focused_composer_before_shutting_down() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('h'), KeyModifiers::NONE));
        root.update(key(KeyCode::Char('i'), KeyModifiers::NONE));

        let clear = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(clear.effects.is_empty());
        assert_eq!(clear.render, super::RenderRequest::Immediate);
        assert!(root.composer().draft().is_empty());

        let confirmation = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(confirmation.effects.is_empty());
        assert!(render_root_text(&mut root, 60, 12).contains("Ctrl+C Quit"));

        let shutdown = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(shutdown.effects, [RootEffect::Shutdown]);
    }

    #[test]
    fn control_z_restores_the_last_cleared_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("first\nλright".to_owned()));
        for _ in 0..5 {
            root.update(key(KeyCode::Left, KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(root.composer().draft().is_empty());
        assert!(root.discarded_draft.is_some());

        let restored = root.update(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        root.update(key(KeyCode::Char('|'), KeyModifiers::NONE));

        assert_eq!(restored.render, super::RenderRequest::Immediate);
        assert_eq!(root.composer().draft(), "first\nλ|right");
        assert!(root.discarded_draft.is_none());
    }

    #[test]
    fn restored_draft_keeps_pasted_images() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("inspect ".to_owned()));
        root.update(RootEvent::PasteImage(
            "data:image/png;base64,restored".to_owned(),
        ));
        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        root.update(key(KeyCode::Char('z'), KeyModifiers::CONTROL));
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        let [RootEffect::Submit(prompt)] = submitted.effects.as_slice() else {
            panic!("restored draft should submit");
        };
        let content = prompt.host_content();
        assert!(content[0]["text"] == "inspect ");
        assert!(
            content[1]["image_url"]
                .as_str()
                .unwrap()
                .ends_with("restored")
        );
    }

    #[test]
    fn control_z_does_not_overwrite_a_nonempty_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("recover me".to_owned()));
        root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        root.update(RootEvent::ReplaceDraft("keep me".to_owned()));

        let update = root.update(key(KeyCode::Char('z'), KeyModifiers::CONTROL));

        assert_eq!(update.render, super::RenderRequest::None);
        assert_eq!(root.composer().draft(), "keep me");
        assert!(root.discarded_draft.is_some());
    }

    #[test]
    fn successful_session_replacement_preserves_the_displaced_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("continue later".to_owned()));

        root.reset_session(
            Path::new("/work"),
            ReasoningEffort::High,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            DraftReset::Clear,
        );
        root.update(key(KeyCode::Char('z'), KeyModifiers::CONTROL));

        assert_eq!(root.composer().draft(), "continue later");
    }

    #[test]
    fn double_escape_interrupts_without_shutting_down() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let first = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        let rendered = render_root_text(&mut root, 60, 12);
        let second = root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert!(first.effects.is_empty());
        assert!(rendered.contains("Esc then"));
        assert!(rendered.contains("Esc Interrupt"));
        assert!(rendered.contains("Any other key cancel"));
        assert_eq!(second.effects, [RootEffect::CancelTurns]);
    }

    #[test]
    fn cancelled_turn_accepts_and_submits_fresh_input() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        let cancel = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(cancel.effects, [RootEffect::CancelTurns]);
        root.update(RootEvent::TurnsCancelled);

        root.update(key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert_eq!(root.composer().draft(), "c");
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            submitted.effects.as_slice(),
            [RootEffect::Submit(_)]
        ));
    }

    #[test]
    fn escape_requires_two_distinct_presses() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        let release = root.update(key_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        let repeat = root.update(key_with_kind(
            KeyCode::Esc,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));

        assert!(release.effects.is_empty());
        assert!(repeat.effects.is_empty());
        assert!(root.key_confirmation.is_some());

        let second_press = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(second_press.effects, [RootEffect::CancelTurns]);
    }

    #[test]
    fn tab_swaps_between_the_queue_and_composer() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        for character in "queued".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.update(RootEvent::SubmissionAcknowledged(
            "queued".to_owned().into(),
        ));
        root.queue.component_mut().push("queued".to_owned());
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        root.update(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(root.queue.component().focused());
        root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(root.composer().draft().is_empty());

        root.update(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(!root.queue.component().focused());
        root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));

        assert_eq!(root.composer().draft(), "x");
    }

    #[test]
    fn enter_in_an_empty_composer_steers_the_selected_queued_message() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.queue.component_mut().push("later".to_owned());
        root.queue.component_mut().push("steer now".to_owned());

        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(matches!(
            update.effects.as_slice(),
            [RootEffect::Steer { id, .. }] if *id==super::QueueId::new(1)
        ));
        assert!(!root.queue.component().focused());
        assert!(root.composer().draft().is_empty());
    }

    #[test]
    fn clicking_the_composer_returns_focus_to_it() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.queue.component_mut().push("queued".to_owned());
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(root.queue.component().focused());

        let down = root.update(mouse(MouseEventKind::Down(MouseButton::Left), 10, 9));
        let up = root.update(mouse(MouseEventKind::Up(MouseButton::Left), 10, 9));

        assert!(!root.queue.component().focused());
        assert_eq!(down.render.max(up.render), super::RenderRequest::Immediate);
    }

    #[test]
    fn clicking_the_queue_keeps_focus_on_the_queue() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.queue.component_mut().push("queued".to_owned());
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let update = root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            root.queue_area.x + 2,
            root.queue_area.y + 1,
        ));

        assert!(root.queue.component().focused());
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn clicking_empty_transcript_space_returns_focus_to_the_composer() {
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.queue.component_mut().push("queued".to_owned());
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(key(KeyCode::Tab, KeyModifiers::NONE));
        assert!(root.queue.component().focused());

        let update = root.update(mouse(MouseEventKind::Down(MouseButton::Left), 10, 2));

        assert!(!root.queue.component().focused());
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn shell_commands_bypass_the_agent_message_queue() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.update(super::RootEvent::ReplaceDraft("!pwd".to_owned()));

        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(submitted.effects, [RootEffect::RunShell("pwd".to_owned())]);
        assert!(root.queue.component().is_empty());
        assert_eq!(root.in_flight_turns, 1);
        assert_eq!(root.activity.visual().state(), ActivityState::Working);

        root.update(RootEvent::ShellFinished);
        assert_eq!(root.activity.visual().state(), ActivityState::Thinking);
    }

    #[test]
    fn shift_enter_in_a_queue_edit_inserts_a_newline() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.queue.component_mut().push("first line".to_owned());
        root.queue.component_mut().set_focused(true);
        root.update(key(KeyCode::Char('e'), KeyModifiers::NONE));
        root.update(RootEvent::QueueEditReady {
            id: super::QueueId::new(0),
            expected_input: orvek_harness::Digest::of(b"fixture"),
            prompt: "first line".to_owned().into(),
        });

        let newline = root.update(key(KeyCode::Enter, KeyModifiers::SHIFT));

        assert!(newline.effects.is_empty());
        assert_eq!(root.composer.component().draft(), "first line\n");
        assert!(root.queue_edit.is_some());
    }

    #[test]
    fn displaced_release_over_the_composer_copies_without_drag_events() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for character in "copy me".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), 1, 8));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), 7, 8));

        assert_eq!(update.effects, [RootEffect::Copy("copy me".to_owned())]);
        assert_eq!(update.render, super::RenderRequest::Immediate);
        assert!(!root.selection.is_active());
        assert!(root.toasts.latest().is_none());
        assert_eq!(root.composer().draft(), "copy me");
        root.update(super::RootEvent::NotifySuccess(
            "Copied selection to clipboard.".to_owned(),
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Copied selection to clipboard."));
        let buffer = terminal.backend().buffer();
        let left = (40 - ("Copied selection to clipboard.".len() as u16 + 4)) / 2;
        assert_eq!(buffer[(left, 0)].symbol(), "╭");
        assert_eq!(buffer[(left, 0)].fg, Theme::default().success());
        assert!(buffer[(left + 2, 1)].modifier.contains(Modifier::BOLD));

        let deadline = root.toasts.latest().unwrap().deadline();
        root.update(super::RootEvent::AnimationFrame(deadline));
        assert!(root.toasts.latest().is_none());
    }

    #[test]
    fn narrow_notifications_keep_wrapped_action_text_visible() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::NotifySuccess(
            "Pro enabled for new sessions · start a new session to apply.".to_owned(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("start a new session"));
        assert!(rendered.contains("apply."));
    }

    #[test]
    fn multiline_notifications_use_rendered_height_and_sanitize_controls() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::NotifySuccess(
            "First line\r\nSecond\tline\u{1b}".to_owned(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();

        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..4)
            .map(|row| {
                (0..40)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows[1].contains("First line"));
        assert!(rows[2].contains("Second    line�"));
        assert!(rows[3].contains('╰'));
    }

    #[test]
    fn update_available_uses_the_success_frame_and_styles_version_and_command() {
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let version = Version::new(1, 2, 3);
        let message = "Update available · v1.2.3 · run `orvek update`";

        root.update(super::RootEvent::UpdateAvailable(version));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let message_width = unicode_width::UnicodeWidthStr::width(message) as u16;
        let left = (80 - (message_width + 4)) / 2;
        let text_start = left + 2;
        let prefix_width = unicode_width::UnicodeWidthStr::width("Update available · ") as u16;
        let version_width = unicode_width::UnicodeWidthStr::width("v1.2.3") as u16;
        let suffix_width = unicode_width::UnicodeWidthStr::width(" · run ") as u16;
        let version_start = text_start + prefix_width;
        let command_start = version_start + version_width + suffix_width;

        assert_eq!(buffer[(left, 0)].symbol(), "╭");
        assert_eq!(buffer[(left, 0)].fg, Theme::default().success());
        for column in text_start..version_start {
            assert_eq!(buffer[(column, 1)].fg, Theme::default().success());
            assert!(!buffer[(column, 1)].modifier.contains(Modifier::BOLD));
        }
        for column in version_start..version_start + version_width {
            assert_eq!(buffer[(column, 1)].fg, Theme::default().success());
            assert!(buffer[(column, 1)].modifier.contains(Modifier::BOLD));
        }
        for column in version_start + version_width..command_start {
            assert_eq!(buffer[(column, 1)].fg, Theme::default().success());
            assert!(!buffer[(column, 1)].modifier.contains(Modifier::BOLD));
        }
        for column in command_start..text_start + message_width {
            assert_eq!(buffer[(column, 1)].fg, Theme::default().text());
            assert!(!buffer[(column, 1)].modifier.contains(Modifier::BOLD));
        }

        let rendered = buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains(message));

        let deadline = root.toasts.latest().unwrap().deadline();
        root.update(super::RootEvent::AnimationFrame(deadline));
        assert!(root.toasts.latest().is_none());
    }

    #[test]
    fn dragging_over_the_transcript_copies_visible_text() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let record = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: "hello transcript".to_owned(),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(record)));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let row = (0..7)
            .find(|&row| {
                (0..40)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("hello transcript")
            })
            .unwrap();
        let start = text_column(terminal.backend().buffer(), row, "hello transcript");

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), start, row));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            start + 4,
            row,
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), start + 4, row));

        assert_eq!(update.effects, [RootEffect::Copy("hello".to_owned())]);
        assert!(!root.selection.is_active());
    }

    #[test]
    fn transcript_selection_copies_code_source_without_rendered_borders() {
        for width in [40, 128, 200] {
            let mut terminal = Terminal::new(TestBackend::new(width, 14)).unwrap();
            let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
            root.update(super::RootEvent::Transcript(agent_record(
                1,
                DisplaySample::Text,
                json!({
                    "model_call_index": 1,
                    "item_id": "answer",
                    "phase": "final_answer",
                    "text": "```rust\n    let answer = 42;\n```",
                }),
            )));
            terminal
                .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
                .unwrap();
            let row = (0..terminal.backend().buffer().area.height)
                .find(|&row| {
                    (0..terminal.backend().buffer().area.width)
                        .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                        .collect::<String>()
                        .contains("    let answer = 42;")
                })
                .expect("code should be visible");

            let start = text_column(terminal.backend().buffer(), row, "    let answer = 42;");
            let end = start + u16::try_from("    let answer = 42;".len()).unwrap() - 1;
            root.update(mouse(MouseEventKind::Down(MouseButton::Left), start, row));
            root.update(mouse(MouseEventKind::Drag(MouseButton::Left), end, row));
            terminal
                .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
                .unwrap();

            let buffer = terminal.backend().buffer();
            assert_ne!(buffer[(start.saturating_sub(1), row)].bg, Color::Yellow);
            assert_eq!(buffer[(start, row)].bg, Color::Yellow);
            assert_ne!(buffer[(end.saturating_add(1), row)].bg, Color::Yellow);

            let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), end, row));
            assert_eq!(
                update.effects,
                [RootEffect::Copy("    let answer = 42;".to_owned())]
            );
        }
    }

    #[test]
    fn transcript_selection_copies_shell_command_and_output_without_chrome_or_soft_wraps() {
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let command = "$HOME/bin/printf output";
        let output = "alpha beta gamma delta epsilon zeta\nsecond line";
        root.update(super::RootEvent::Transcript(agent_record(
            1,
            DisplaySample::ToolStart,
            json!({
                "call_id": "workflow",
                "tool": "exec",
                "arguments": "await tools.exec_command({cmd: '$HOME/bin/printf output'})",
            }),
        )));
        root.update(super::RootEvent::Transcript(agent_record(
            2,
            DisplaySample::ToolStart,
            json!({
                "call_id": "workflow/shell",
                "tool": "exec_command",
                "arguments": {"cmd": command},
            }),
        )));
        root.update(super::RootEvent::Transcript(agent_record(
            3,
            DisplaySample::ToolReturn,
            json!({
                "call_id": "workflow/shell",
                "tool": "exec_command",
                "status": "completed",
                "duration_ns": 1_u64,
                "result": format!(
                    "Wall time: 0.0000 seconds\nProcess exited with code 0\nOutput:\n{output}"
                ),
                "structured_result": {
                    "output": output,
                    "exit_code": 0,
                    "wall_time_seconds": 0.0,
                },
                "metadata": null,
            }),
        )));
        root.update(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let command_row = (0..buffer.area.height)
            .rfind(|&row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .contains(command)
            })
            .expect("the expanded shell command should be visible");
        let command_start = text_column(buffer, command_row, command);
        let command_end = command_start + u16::try_from(command.len()).unwrap();

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            command_start,
            command_row,
        ));
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            command_end,
            command_row,
        ));
        let update = root.update(mouse(
            MouseEventKind::Up(MouseButton::Left),
            command_end,
            command_row,
        ));
        assert_eq!(update.effects, [RootEffect::Copy(command.to_owned())]);

        let first_row = (0..buffer.area.height)
            .find(|&row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .contains("alpha")
            })
            .expect("the first output line should be visible");
        let last_row = (0..buffer.area.height)
            .find(|&row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .contains("second line")
            })
            .expect("the second output line should be visible");
        let start = text_column(buffer, first_row, "alpha");
        let end = text_column(buffer, last_row, "second line") + 10;

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start,
            first_row,
        ));
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            end,
            last_row,
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_ne!(buffer[(0, first_row)].bg, Color::Yellow);
        assert_ne!(
            buffer[(start.saturating_sub(1), first_row)].bg,
            Color::Yellow
        );
        assert_eq!(buffer[(start, first_row)].bg, Color::Yellow);

        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), end, last_row));
        assert_eq!(update.effects, [RootEffect::Copy(output.to_owned())]);
    }

    #[test]
    fn transcript_selection_copies_original_markdown_syntax() {
        let mut terminal = Terminal::new(TestBackend::new(64, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Text,
            json!({
                "model_call_index": 1,
                "item_id": "answer",
                "phase": "final_answer",
                "text": "**bold** and [site](https://example.com)",
            }),
        )));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let row = (0..terminal.backend().buffer().area.height)
            .find(|&row| {
                (0..terminal.backend().buffer().area.width)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("bold and site")
            })
            .expect("message should be visible");
        let start = text_column(terminal.backend().buffer(), row, "bold");
        let end = text_column(terminal.backend().buffer(), row, "site") + 3;

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), start, row));
        root.update(mouse(MouseEventKind::Drag(MouseButton::Left), end, row));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let destination = text_column(terminal.backend().buffer(), row, "https://example.com");
        assert_ne!(
            terminal.backend().buffer()[(destination, row)].bg,
            Color::Yellow
        );

        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), end, row));
        assert_eq!(
            update.effects,
            [RootEffect::Copy(
                "**bold** and [site](https://example.com)".to_owned()
            )]
        );
    }

    #[test]
    fn transcript_selection_highlights_rendered_link_destinations() {
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let markdown = "See [inclusion.rs](/workspace/glue/src/inclusion.rs) or [mailbox.rs](/workspace/glue/src/mailbox.rs).";
        root.update(super::RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Text,
            json!({
                "model_call_index": 1,
                "item_id": "answer",
                "phase": "final_answer",
                "text": markdown,
            }),
        )));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = (0..buffer.area.height)
            .find(|&row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .contains("See inclusion.rs")
            })
            .expect("message should be visible");
        let start = text_column(buffer, row, "See");
        let rendered_link = "mailbox.rs ↗ /workspace/glue/src/mailbox.rs";
        let end = text_column(buffer, row, rendered_link)
            + u16::try_from(rendered_link.chars().count()).unwrap();

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), start, row));
        root.update(mouse(MouseEventKind::Drag(MouseButton::Left), end, row));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        for column in start..end {
            let cell = &buffer[(column, row)];
            assert_eq!(
                (cell.fg, cell.bg, cell.modifier),
                (Color::Black, Color::Yellow, Modifier::empty()),
                "selected cell at column {column} ({:?}) retained link styling",
                cell.symbol()
            );
        }
    }

    #[test]
    fn partial_transcript_selection_does_not_copy_unmatched_markdown_delimiters() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Text,
            json!({
                "model_call_index": 1,
                "item_id": "answer",
                "phase": "final_answer",
                "text": "**bold**",
            }),
        )));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let row = (0..root.transcript_area.height)
            .find(|&row| {
                (0..40)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("bold")
            })
            .unwrap();
        let start = text_column(terminal.backend().buffer(), row, "bold");

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), start, row));
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            start + 1,
            row,
        ));
        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), start + 1, row));

        assert_eq!(update.effects, [RootEffect::Copy("bo".to_owned())]);
    }

    #[test]
    fn transcript_selection_survives_scrolling_beyond_the_viewport() {
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=10 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        root.update(key(KeyCode::Home, KeyModifiers::CONTROL));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let start_row = (0..root.transcript_area.height)
            .find(|&row| {
                (0..32)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("prompt 1")
            })
            .expect("first prompt should be visible");

        let start_column = text_column(terminal.backend().buffer(), start_row, "prompt 1");
        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_column,
            start_row,
        ));
        let mut end_row = None;
        for _ in 0..10 {
            if let Some(row) = (0..root.transcript_area.height).find(|&row| {
                (0..32)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("prompt 5")
            }) {
                end_row = Some(row);
                break;
            }
            root.update(mouse(MouseEventKind::ScrollDown, 4, start_row));
            terminal
                .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
                .unwrap();
        }
        let end_row = end_row.expect("the fifth prompt should become visible");
        let end_column = text_column(terminal.backend().buffer(), end_row, "prompt 5")
            + u16::try_from("prompt 5".len()).unwrap()
            - 1;
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            end_column,
            end_row,
        ));
        let update = root.update(mouse(
            MouseEventKind::Up(MouseButton::Left),
            end_column,
            end_row,
        ));

        assert_eq!(
            update.effects,
            [RootEffect::Copy(
                "prompt 1\n\nprompt 2\n\nprompt 3\n\nprompt 4\n\nprompt 5".to_owned()
            )]
        );
    }

    #[test]
    fn dragging_at_the_viewport_edge_keeps_extending_the_selection() {
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for sequence in 1..=12 {
            let record = TranscriptRecord::from_local(
                sequence,
                sequence,
                LocalEvent::UserSubmitted {
                    id: TurnId::new(sequence),
                    text: format!("prompt {sequence}"),
                },
            )
            .unwrap();
            root.update(super::RootEvent::Transcript(Arc::new(record)));
        }
        root.update(key(KeyCode::Home, KeyModifiers::CONTROL));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let start_row = (0..root.transcript_area.height)
            .find(|&row| {
                (0..32)
                    .map(|column| terminal.backend().buffer()[(column, row)].symbol())
                    .collect::<String>()
                    .contains("prompt 1")
            })
            .unwrap();
        let edge = root.transcript_area.bottom().saturating_sub(1);
        let start_column = text_column(terminal.backend().buffer(), start_row, "prompt 1");
        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_column,
            start_row,
        ));
        root.update(mouse(MouseEventKind::Drag(MouseButton::Left), 31, edge));

        for _ in 0..20 {
            terminal
                .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
                .unwrap();
            let deadline = root
                .selection_auto_scroll
                .as_ref()
                .expect("edge drag should keep scrolling")
                .deadline;
            root.update(super::RootEvent::AnimationFrame(deadline));
        }
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), 31, edge));
        let [RootEffect::Copy(text)] = update.effects.as_slice() else {
            panic!("edge drag should copy the semantic selection");
        };

        assert!(text.starts_with("prompt 1\n\n"));
        assert!(text.contains("prompt 6"));
        assert!(
            !text
                .chars()
                .any(|character| matches!(character, '┃' | '│' | '╭' | '╯'))
        );
        assert!(root.selection_auto_scroll.is_none());
    }

    #[test]
    fn composer_selection_scrolls_without_losing_offscreen_text() {
        let mut terminal = Terminal::new(TestBackend::new(32, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::ReplaceDraft(
            (1..=10)
                .map(|line| format!("line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let start_row = root.composer_content_area.bottom().saturating_sub(1);
        let start_column = text_column(terminal.backend().buffer(), start_row, "line 10");

        root.update(mouse(
            MouseEventKind::Down(MouseButton::Left),
            start_column + 6,
            start_row,
        ));
        root.update(mouse(MouseEventKind::ScrollUp, start_column, start_row));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let end_row = root.composer_content_area.y;
        let end_column = text_column(terminal.backend().buffer(), end_row, "line 2");
        root.update(mouse(
            MouseEventKind::Drag(MouseButton::Left),
            end_column,
            end_row,
        ));
        let update = root.update(mouse(
            MouseEventKind::Up(MouseButton::Left),
            end_column,
            end_row,
        ));

        assert_eq!(
            update.effects,
            [RootEffect::Copy(
                (2..=10)
                    .map(|line| format!("line {line}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )]
        );
    }

    #[test]
    fn transcript_selection_excludes_the_top_right_hint() {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let record = TranscriptRecord::from_local(
            1,
            1,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: ["copy this prompt"; 8].join("\n"),
            },
        )
        .unwrap();
        root.update(super::RootEvent::Transcript(Arc::new(record)));
        root.transcript.component_mut().focus_expandables();
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        root.update(mouse(MouseEventKind::Down(MouseButton::Left), 2, 1));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        root.update(mouse(MouseEventKind::Drag(MouseButton::Left), 39, 1));
        terminal
            .draw(|frame| root.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = root.update(mouse(MouseEventKind::Up(MouseButton::Left), 39, 1));

        assert_eq!(
            update.effects,
            [RootEffect::Copy("copy this prompt".to_owned())]
        );
    }

    #[test]
    fn key_confirmation_expires_and_unrelated_input_resets_it() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let now = Instant::now();

        assert!(
            root.update_key_confirmation(ConfirmationAction::Interrupt, now)
                .effects
                .is_empty()
        );
        assert!(
            root.update_key_confirmation(
                ConfirmationAction::Interrupt,
                now + super::KEY_CONFIRMATION_TIMEOUT + Duration::from_millis(1),
            )
            .effects
            .is_empty()
        );
        root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(
            root.update(key(KeyCode::Esc, KeyModifiers::NONE))
                .effects
                .is_empty()
        );
    }

    #[test]
    fn expired_confirmation_is_removed_immediately() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let now = Instant::now();
        root.update_key_confirmation(ConfirmationAction::Exit, now);

        let update = root.update(super::RootEvent::AnimationFrame(
            now + super::KEY_CONFIRMATION_TIMEOUT,
        ));

        assert!(root.key_confirmation.is_none());
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn effort_action_opens_the_selector_and_applies_the_selection() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

        for character in "effort".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&root.overlay, Some(Overlay::Effort(_))));

        root.update(key(KeyCode::Right, KeyModifiers::NONE));
        assert!(root.animation_deadline().is_some());
        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            update.effects,
            [RootEffect::SetEffort {
                effort: ReasoningEffort::High,
                reasoning_mode: ReasoningMode::Standard,
            }]
        );
        assert_eq!(root.composer().effort(), ReasoningEffort::High);
        assert!(root.overlay.is_none());
    }

    #[test]
    fn pro_preference_does_not_change_the_running_session_mode() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "effort".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        root.update(key(KeyCode::Char('p'), KeyModifiers::NONE));
        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            update.effects,
            [RootEffect::SetEffort {
                effort: ReasoningEffort::Medium,
                reasoning_mode: ReasoningMode::Pro,
            }]
        );
        assert_eq!(root.composer().reasoning_mode(), ReasoningMode::Standard);
        let notification = root.toasts.latest().unwrap();
        let message = toast_message(notification);
        assert_eq!(
            message,
            "Pro enabled for new sessions · start a new session to apply."
        );
        assert_eq!(notification.tone(), FeedbackTone::Info);

        root.reset_session(
            Path::new("/work"),
            ReasoningEffort::Medium,
            ReasoningMode::Pro,
            ReasoningMode::Pro,
            DraftReset::Clear,
        );
        assert_eq!(root.composer().reasoning_mode(), ReasoningMode::Pro);

        root.open_effort();
        root.update(key(KeyCode::Char('p'), KeyModifiers::NONE));
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let notification = root.toasts.latest().unwrap();
        let message = toast_message(notification);
        assert_eq!(
            message,
            "Pro disabled for new sessions · start a new session to apply."
        );
        assert_eq!(notification.tone(), FeedbackTone::Info);
    }

    #[test]
    fn fast_mode_action_toggles_the_runtime_setting() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "fast mode".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let enabled = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(enabled.effects, [RootEffect::SetFastMode(true)]);
        assert!(root.composer().fast_mode());
        assert!(root.overlay.is_none());

        root.reset_session(
            Path::new("/work"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            DraftReset::Preserve,
        );
        root.set_fast_mode(true);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "priority".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let disabled = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(disabled.effects, [RootEffect::SetFastMode(false)]);
        assert!(!root.composer().fast_mode());
    }

    #[test]
    fn theme_action_opens_the_selector_and_applies_the_selection() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "appearance".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&root.overlay, Some(Overlay::Theme(_))));

        root.update(key(KeyCode::Down, KeyModifiers::NONE));
        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(update.effects, [RootEffect::SetTheme(ThemeMode::Light)]);
        assert!(root.overlay.is_none());
    }

    #[test]
    fn subagents_action_reopens_the_active_filter_on_the_oldest_active_agent() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for (id, role) in [(1, "completed"), (2, "active")] {
            root.update(RootEvent::Subagent(ChildUpdate::Added(ChildView {
                id: ChildId::new(id),
                session_id: format!("agent-{id}"),
                model: Model::Sol,
                role: role.to_owned(),
                task: role.to_owned(),
                parent: None,
            })));
        }
        root.update(RootEvent::Subagent(ChildUpdate::Status {
            id: ChildId::new(1),
            status: ChildStatus::Returned {
                output: orvek_harness::Digest::of(b"{\"report\":\"done\"}"),
            },
        }));
        root.subagents.update_tree(Event::Key(KeyEvent::new(
            KeyCode::Char('f'),
            KeyModifiers::NONE,
        )));

        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "agents".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            root.overlay,
            Some(Overlay::Subagents(SubagentOverlay::Tree))
        ));

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            root.overlay,
            Some(Overlay::Subagents(SubagentOverlay::Transcript(id)))
                if id == ChildId::new(2)
        ));
    }

    #[test]
    fn control_s_opens_effort_for_new_and_started_threads() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let opened = root.update(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(&root.overlay, Some(Overlay::Effort(_))));
        assert_eq!(opened.render, super::RenderRequest::Immediate);

        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        root.thread = super::ThreadState::Started;
        let reopened = root.update(key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(matches!(&root.overlay, Some(Overlay::Effort(_))));
        assert_eq!(reopened.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn control_d_selects_a_model_only_before_the_first_prompt() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let opened = root.update(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(matches!(&root.overlay, Some(Overlay::Model(_))));
        assert_eq!(opened.render, super::RenderRequest::Immediate);

        root.update(key(KeyCode::Left, KeyModifiers::NONE));
        let selected = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(selected.effects, [RootEffect::SetModel(Model::Terra)]);

        root.interactive = true;
        root.thread = super::ThreadState::Started;
        let blocked = root.update(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(blocked.effects.is_empty());
        assert!(root.overlay.is_none());
    }

    #[test]
    fn fork_inherits_the_model_and_cannot_change_it() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_model(Model::Luna);

        let mut fork = root.fork(Path::new("/work"), ReasoningEffort::Medium);

        assert_eq!(fork.composer().model(), Model::Luna);
        let update = fork.update(key(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(update.effects.is_empty());
        assert!(fork.overlay.is_none());
    }

    #[test]
    fn model_action_opens_only_for_a_new_thread() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "intelligence".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(root.overlay, Some(Overlay::Model(_))));

        root.overlay = None;
        root.thread = ThreadState::Started;
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "intelligence".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let blocked = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(blocked.effects.is_empty());
        assert!(matches!(root.overlay, Some(Overlay::Actions(_))));
    }

    #[test]
    fn control_r_loads_recent_prompts_and_inserts_from_the_current_session() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("keep while loading".to_owned()));

        let loading = root.update(key(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert_eq!(loading.effects, [RootEffect::LoadRecentPrompts(Vec::new())]);
        assert_eq!(root.composer().draft(), "keep while loading");
        assert!(!root.interactive);

        root.update(RootEvent::RecentPromptsLoaded {
            session_id: "current".to_owned(),
            prompts: vec![
                RecentPrompt {
                    text: "other prompt".to_owned(),
                    recorded_at_unix_ms: 2,
                    session_id: "other".to_owned(),
                    workspace: "/other".into(),
                },
                RecentPrompt {
                    text: "  current\n\n    prompt  ".to_owned(),
                    recorded_at_unix_ms: 1,
                    session_id: "current".to_owned(),
                    workspace: "/work".into(),
                },
            ],
        });
        assert!(matches!(&root.overlay, Some(Overlay::RecentPrompts(_))));

        root.update(key(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for character in "crp".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(root.composer().draft(), "  current\n\n    prompt  ");
    }

    #[test]
    fn recent_prompt_load_failure_preserves_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("keep me".to_owned()));
        root.update(key(KeyCode::Char('r'), KeyModifiers::CONTROL));

        root.update(RootEvent::RecentPromptLoadFailed("load failed".to_owned()));

        assert!(root.interactive);
        assert_eq!(root.composer().draft(), "keep me");
        assert!(root.toasts.latest().is_some());
    }

    #[test]
    fn control_r_includes_the_in_memory_prompt_before_loading_disk_history() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        let prompt = TranscriptRecord::from_local(
            1,
            42,
            LocalEvent::UserSubmitted {
                id: TurnId::new(1),
                text: "just submitted".to_owned(),
            },
        )
        .unwrap();
        root.update(RootEvent::Transcript(Arc::new(prompt)));

        let loading = root.update(key(KeyCode::Char('r'), KeyModifiers::CONTROL));

        assert_eq!(
            loading.effects,
            [RootEffect::LoadRecentPrompts(vec![
                super::RecentPromptDraft {
                    text: "just submitted".to_owned(),
                    recorded_at_unix_ms: 42,
                },
            ])]
        );
    }

    #[test]
    fn control_o_toggles_transcript_expansion_globally() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);

        let expanded = root.update(key(KeyCode::Char('o'), KeyModifiers::CONTROL));
        let collapsed = root.update(key(KeyCode::Char('o'), KeyModifiers::CONTROL));

        assert_eq!(expanded.render, super::RenderRequest::Immediate);
        assert_eq!(collapsed.render, super::RenderRequest::Immediate);
        assert!(root.composer().draft().is_empty());
    }

    #[test]
    fn control_o_does_not_change_the_hidden_transcript_behind_an_overlay() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));

        let update = root.update(key(KeyCode::Char('o'), KeyModifiers::CONTROL));

        assert_eq!(update.render, super::RenderRequest::None);
        assert!(matches!(root.overlay, Some(Overlay::Actions(_))));
    }

    #[test]
    fn escape_that_blurs_expandable_items_does_not_start_the_interrupt_chord() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.transcript.component_mut().focus_expandables();

        let blurred = root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert!(blurred.effects.is_empty());
        assert!(!root.transcript.component().expandables_focused());
        assert!(root.key_confirmation.is_none());

        let chord_started = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(chord_started.effects.is_empty());
        assert!(root.key_confirmation.is_some());
    }

    #[test]
    fn keybindings_action_opens_help_and_escape_closes_it() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "keyboard".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&root.overlay, Some(Overlay::Keybindings(_))));

        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(root.overlay.is_none());
    }

    #[test]
    fn resize_redraws_while_keybindings_help_is_open() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "keyboard".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        let update = root.update(super::RootEvent::Terminal(Event::Resize(100, 30)));

        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn config_action_closes_the_menu_and_requests_the_external_editor() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "edit config".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(update.effects, [RootEffect::OpenConfigEditor]);
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn reflection_action_collects_hidden_optional_instructions() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "reflection".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.reflection_input);
        assert!(render_root_text(&mut root, 100, 20).contains("Reflection instructions"));
        for character in "Focus on validation gaps.".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(
            submitted.effects,
            [RootEffect::Reflect(
                "Focus on validation gaps.".to_owned().into()
            )]
        );
        assert!(!root.reflection_input);
        assert!(root.thread == ThreadState::Started);
        assert_eq!(root.in_flight_turns, 0);
        assert!(root.pending_submission.is_some());
        root.update(RootEvent::SubmissionAcknowledged(
            "Focus on validation gaps.".to_owned().into(),
        ));
        assert!(root.composer().draft().is_empty());
        root.update(key(KeyCode::Up, KeyModifiers::NONE));
        assert!(root.composer().draft().is_empty());
    }

    #[test]
    fn reflection_can_start_without_instructions_or_be_cancelled() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "reflection".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));

        let cancelled = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(cancelled.effects.is_empty());
        assert!(!root.reflection_input);
        assert!(root.composer().draft().is_empty());

        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "reflection".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            submitted.effects,
            [RootEffect::Reflect("".to_owned().into())]
        );
    }

    #[test]
    fn reload_config_action_closes_the_menu_and_requests_a_reload() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "refresh".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(root.overlay.is_none());
        assert_eq!(update.effects, [RootEffect::ReloadConfig]);
        assert_eq!(update.render, super::RenderRequest::Immediate);
    }

    #[test]
    fn memory_action_loads_inspects_and_deletes_without_submitting_a_prompt() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_memory_enabled(true);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "remember".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let opened = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(opened.effects, [RootEffect::LoadMemories]);
        assert!(matches!(&root.overlay, Some(Overlay::Memory(_))));
        assert!(root.composer().draft().is_empty());

        root.update(RootEvent::MemoriesLoaded {
            access: local_memory_access(),
            records: vec![memory_record(7, 3, "remember this")],
        });
        let inspected = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(inspected.effects.is_empty());
        assert!(render_root_text(&mut root, 80, 28).contains("remember this"));

        root.update(key(KeyCode::Char('d'), KeyModifiers::NONE));
        let deleted = root.update(key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(
            deleted.effects,
            [RootEffect::DeleteMemory(MemoryKey::local(7, 3))]
        );
        assert!(root.composer().draft().is_empty());

        root.update(RootEvent::MemoryDeleted {
            key: MemoryKey::local(7, 3),
        });
        assert!(render_root_text(&mut root, 80, 20).contains("Local memory is empty"));
    }

    #[test]
    fn memory_completions_are_ignored_after_the_browser_closes() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_memory_enabled(true);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "memory".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(root.overlay.is_none());

        for event in [
            RootEvent::MemoriesLoaded {
                access: local_memory_access(),
                records: vec![memory_record(1, 1, "stale")],
            },
            RootEvent::MemoryLoadFailed {
                source: MemorySource::Local,
                access: None,
                error: "stale load".to_owned(),
            },
            RootEvent::MemoryDeleted {
                key: MemoryKey::local(1, 1),
            },
            RootEvent::MemoryDeleteFailed {
                error: "stale delete".to_owned(),
                conflict: false,
            },
        ] {
            let update = root.update(event);
            assert!(update.effects.is_empty());
            assert_eq!(update.render, RenderRequest::None);
            assert!(root.overlay.is_none());
        }
    }

    #[test]
    fn memory_availability_survives_reset_and_fork_and_disabling_closes_the_browser() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_memory_enabled(true);
        root.reset_session(
            Path::new("/work"),
            ReasoningEffort::Low,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            DraftReset::Clear,
        );
        let fork = root.fork(Path::new("/work"), ReasoningEffort::Low);
        assert!(root.memory_enabled);
        assert!(fork.memory_enabled);

        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "memory".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(&root.overlay, Some(Overlay::Memory(_))));

        root.set_memory_enabled(false);
        assert!(!root.memory_enabled);
        assert!(root.overlay.is_none());
    }

    #[test]
    fn new_session_action_clears_the_completed_thread_after_runtime_replacement() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.set_model(Model::Luna);
        for character in "old prompt".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.agent_turn_finished();
        root.update(RootEvent::SubmissionAcknowledged(
            "old prompt".to_owned().into(),
        ));
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "clear".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let requested = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert_eq!(requested.effects, [RootEffect::NewSession(Model::Luna)]);
        assert!(root.overlay.is_none());
        assert!(!root.interactive);

        root.reset_session(
            Path::new("/work"),
            ReasoningEffort::Medium,
            ReasoningMode::Standard,
            ReasoningMode::Standard,
            DraftReset::Clear,
        );

        assert!(root.interactive);
        assert!(matches!(root.thread, ThreadState::New));
        assert!(root.composer().draft().is_empty());
        assert_eq!(root.in_flight_turns, 0);
    }

    #[test]
    fn new_session_action_is_unavailable_while_work_is_active() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        for character in "active prompt".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.update(RootEvent::SubmissionAcknowledged(
            "active prompt".to_owned().into(),
        ));
        root.in_flight_turns = 1;
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "clear".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(update.effects.is_empty());
        assert!(matches!(&root.overlay, Some(Overlay::Actions(_))));
    }

    #[test]
    fn effort_action_remains_available_after_the_first_prompt() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('h'), KeyModifiers::NONE));
        root.update(key(KeyCode::Char('i'), KeyModifiers::NONE));

        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            submitted.effects,
            [RootEffect::Submit("hi".to_owned().into())]
        );

        root.update(RootEvent::SubmissionAcknowledged("hi".to_owned().into()));
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "effort".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let update = root.update(key(KeyCode::Enter, KeyModifiers::NONE));

        assert!(update.effects.is_empty());
        assert!(matches!(&root.overlay, Some(Overlay::Effort(_))));
    }

    #[test]
    fn review_feedback_is_inserted_without_replacing_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(super::RootEvent::ReplaceDraft("existing draft".to_owned()));
        root.update(super::RootEvent::ReviewStarted);

        root.update(super::RootEvent::ReviewFinished {
            markdown: "## Review: Approved".to_owned(),
            feedback: Some(
                "0000000000000000000000000000000000000000000000000000000000000000"
                    .parse()
                    .unwrap(),
            ),
        });

        assert_eq!(
            root.composer().draft(),
            "existing draft\n\n## Review: Approved"
        );
        assert_eq!(
            root.composer
                .component()
                .submission()
                .unwrap()
                .reviews()
                .len(),
            1
        );
        assert!(root.blocking_task.is_none());
    }

    #[test]
    fn review_failure_uses_the_red_notification() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);

        root.update(RootEvent::ReviewFailed(
            "The folder must be a git repository.".to_owned(),
        ));

        let notification = root.toasts.latest().unwrap();
        let message = toast_message(notification);
        assert_eq!(message, "The folder must be a git repository.");
        assert_eq!(notification.tone(), FeedbackTone::Error);
        assert!(root.blocking_task.is_none());
    }

    #[test]
    fn review_waiting_is_shown_in_the_composer_instead_of_the_transcript() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);
        assert_eq!(root.activity.visual().state(), ActivityState::Thinking);
        root.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));
        root.update(RootEvent::Transcript(agent_record(
            2,
            DisplaySample::End,
            json!({}),
        )));

        let rendered = render_root_text(&mut root, 100, 20);
        assert!(rendered.contains("Waiting for review"));
        assert!(!rendered.contains("Waiting for browser review"));
        assert!(!rendered.contains("Preparing review overview"));

        root.update(RootEvent::ReviewCancelled);
        assert_eq!(root.activity.visual().state(), ActivityState::Cancelled);
        assert!(!render_root_text(&mut root, 100, 20).contains("Waiting for review"));
    }

    #[test]
    fn review_suspends_composer_input_without_clearing_the_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("keep this draft".to_owned()));
        root.update(RootEvent::ReviewStarted);

        let typed = root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let quit = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let pasted = root.update(RootEvent::PasteImage(
            "data:image/png;base64,abc".to_owned(),
        ));

        assert!(typed.effects.is_empty());
        assert!(submitted.effects.is_empty());
        assert!(quit.effects.is_empty());
        assert!(pasted.effects.is_empty());
        assert_eq!(root.composer().draft(), "keep this draft");
    }

    #[test]
    fn handoff_blocks_input_until_the_continuation_prompt_is_ready() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(key(KeyCode::Char('/'), KeyModifiers::NONE));
        for character in "handoff".chars() {
            root.update(key(KeyCode::Char(character), KeyModifiers::NONE));
        }

        let started = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let typed = root.update(key(KeyCode::Char('x'), KeyModifiers::NONE));
        let submitted = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        let pasted = root.update(RootEvent::PasteImage(
            "data:image/png;base64,abc".to_owned(),
        ));

        assert_eq!(started.effects, [RootEffect::Handoff]);
        assert!(typed.effects.is_empty());
        assert!(submitted.effects.is_empty());
        assert!(pasted.effects.is_empty());
        assert!(root.composer().draft().is_empty());
        assert!(render_root_text(&mut root, 100, 20).contains("Preparing handoff"));

        root.update(RootEvent::HandoffFinished(
            "Continue by implementing the parser.".to_owned(),
        ));

        assert_eq!(
            root.composer().draft(),
            "Continue by implementing the parser."
        );
        assert!(root.blocking_task.is_none());
    }

    #[test]
    fn escape_cancels_an_active_handoff() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.blocking_task = Some(super::BlockingTask::Handoff);

        let update = root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(update.effects, [RootEffect::CancelHandoff]);
    }

    #[test]
    fn review_ready_exposes_a_reopen_action_without_unlocking_input() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);
        root.update(RootEvent::ReviewReady(
            "http://127.0.0.1:4321/review".to_owned(),
        ));

        let rendered = render_root_text(&mut root, 100, 20);
        let update = root.update(key(KeyCode::Char('o'), KeyModifiers::NONE));
        let copy = root.update(key(KeyCode::Char('c'), KeyModifiers::NONE));

        assert!(rendered.contains("Review ready"));
        assert!(rendered.contains("O reopen"));
        assert!(rendered.contains("C copy link"));
        assert!(!rendered.contains("http://127.0.0.1:4321/review"));
        assert_eq!(
            update.effects,
            [RootEffect::OpenLink(
                "http://127.0.0.1:4321/review".to_owned()
            )]
        );
        assert_eq!(
            copy.effects,
            [RootEffect::Copy("http://127.0.0.1:4321/review".to_owned())]
        );
        assert_eq!(root.blocking_task, Some(super::BlockingTask::Review));
    }

    #[test]
    fn escape_twice_cancels_an_active_review() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);

        let first = root.update(key(KeyCode::Esc, KeyModifiers::NONE));
        let second = root.update(key(KeyCode::Esc, KeyModifiers::NONE));

        assert!(first.effects.is_empty());
        assert_eq!(second.effects, [RootEffect::CancelReview]);
    }

    #[test]
    fn control_c_twice_exits_during_an_active_review() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);

        let first = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        let second = root.update(key(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert!(first.effects.is_empty());
        assert_eq!(second.effects, [RootEffect::Shutdown]);
    }

    #[test]
    fn control_t_can_fork_during_an_active_review() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);

        let update = root.update(key(KeyCode::Char('t'), KeyModifiers::CONTROL));

        assert_eq!(update.effects, [RootEffect::Fork]);
    }

    #[test]
    fn active_turn_can_fork_from_the_latest_safe_boundary() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.update(RootEvent::Transcript(agent_record(
            1,
            DisplaySample::Start,
            json!({}),
        )));

        assert_eq!(
            root.update(key(KeyCode::Char('t'), KeyModifiers::CONTROL))
                .effects,
            [RootEffect::Fork]
        );
    }

    #[test]
    fn fork_does_not_inherit_the_active_review() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReviewStarted);

        let mut fork = root.fork(Path::new("/work"), ReasoningEffort::Medium);

        assert!(fork.blocking_task.is_none());
        assert!(!render_root_text(&mut fork, 100, 20).contains("Waiting for review"));
    }
    #[test]
    fn submission_draft_is_retained_until_receipt_and_double_enter_cannot_duplicate() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("repair fixture".into()));
        let sent = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(sent.effects.as_slice(), [RootEffect::Submit(_)]));
        assert_eq!(root.composer().draft(), "repair fixture");
        assert!(
            root.update(key(KeyCode::Enter, KeyModifiers::NONE))
                .effects
                .is_empty()
        );
        root.update(RootEvent::SubmissionAcknowledged(
            "repair fixture".to_owned().into(),
        ));
        assert!(root.composer().draft().is_empty());
        assert!(root.pending_submission.is_none());
    }
    #[test]
    fn rejected_and_uncertain_submissions_keep_the_original_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("repair fixture".into()));
        root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        root.update(RootEvent::SubmissionFailed {
            uncertain: true,
            error: "lost reply".into(),
        });
        assert_eq!(
            root.update(key(KeyCode::Enter, KeyModifiers::NONE)).effects,
            [RootEffect::RetrySubmission]
        );
        assert!(
            root.update(key(KeyCode::Enter, KeyModifiers::NONE))
                .effects
                .is_empty()
        );
        root.update(RootEvent::SubmissionFailed {
            uncertain: false,
            error: "rejected".into(),
        });
        assert_eq!(root.composer().draft(), "repair fixture");
        assert!(root.pending_submission.is_none());
    }
    #[test]
    fn edits_made_during_acknowledgement_are_not_cleared_and_busy_inputs_go_to_host() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.in_flight_turns = 1;
        root.update(RootEvent::ReplaceDraft("queued input".into()));
        assert!(matches!(
            root.update(key(KeyCode::Enter, KeyModifiers::NONE))
                .effects
                .as_slice(),
            [RootEffect::Submit(_)]
        ));
        assert!(root.queue.component().is_empty());
        root.update(RootEvent::ReplaceDraft("new draft".into()));
        root.update(RootEvent::SubmissionAcknowledged(
            "queued input".to_owned().into(),
        ));
        assert_eq!(root.composer().draft(), "new draft");
        assert!(root.agent_turn_finished().effects.is_empty());
    }
    #[test]
    fn queue_edit_uses_frozen_input_identity_and_preserves_unrelated_draft() {
        let mut root = RootNode::new(Path::new("/work"), ReasoningEffort::Medium);
        root.update(RootEvent::ReplaceDraft("my current draft".into()));
        let id = super::QueueId::new(8);
        let digest = orvek_harness::Digest::of(b"queued");
        root.update(RootEvent::QueueEditReady {
            id,
            expected_input: digest,
            prompt: "queued input".to_owned().into(),
        });
        root.update(RootEvent::ReplaceDraft("edited input".into()));
        let saved = root.update(key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(saved.effects.as_slice(),[RootEffect::ReplaceQueued{id:actual,expected_input,prompt}] if *actual==id&&*expected_input==digest&&prompt.display_text()=="edited input")
        );
        assert_eq!(root.composer().draft(), "my current draft");
    }
}
