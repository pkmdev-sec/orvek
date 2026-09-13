//! Searchable modal menu for actions exposed by the TUI.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    format::{truncate_display, wrap_display_lines},
    theme::Theme,
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACTIONS: [Action; 17] = [
    Action::Effort,
    Action::FastMode,
    Action::Theme,
    Action::NewSession,
    Action::ResumeSession,
    Action::Fork,
    Action::Keybindings,
    Action::ReloadConfig,
    Action::EditConfig,
    Action::Memory,
    Action::Subagents,
    Action::DebugContext,
    Action::Reflection,
    Action::Handoff,
    Action::Review,
    Action::Model,
    Action::Compact,
];
const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "open"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";
const SELECTION_MARKER: &str = "› ";

pub(super) enum ActionsEvent {
    Terminal(Event),
}

pub(super) struct ActionAvailability {
    pub(super) compact: bool,
    pub(super) new_session: bool,
    pub(super) fork: bool,
    pub(super) fast_mode: bool,
    pub(super) memory: bool,
    pub(super) model: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Action {
    Compact,
    Handoff,
    Review,
    Subagents,
    Effort,
    Model,
    FastMode,
    Theme,
    NewSession,
    ResumeSession,
    Fork,
    Keybindings,
    ReloadConfig,
    EditConfig,
    Memory,
    DebugContext,
    Reflection,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ActionsEffect {
    Dismiss,
    Trigger(Action),
}

pub(super) struct ActionsMenu {
    query: String,
    selected: usize,
    matches: Vec<usize>,
    availability: ActionAvailability,
    list_area: Rect,
    offset: usize,
}

impl ActionsMenu {
    pub(super) fn new(availability: ActionAvailability) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            matches: (0..ACTIONS.len()).collect(),
            availability,
            list_area: Rect::default(),
            offset: 0,
        }
    }

    pub(super) fn set_fork_available(&mut self, available: bool) {
        self.availability.fork = available;
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<ActionsEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::Esc => Self::dismiss(),
            KeyCode::Backspace if !self.query.is_empty() => {
                self.remove_last_grapheme();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::dismiss(),
            KeyCode::Enter | KeyCode::Tab => self.trigger_selected(),
            KeyCode::Up => {
                self.select_previous();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                self.select_next();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(character);
                self.refresh_matches();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<ActionsEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn dismiss() -> ComponentUpdate<ActionsEffect> {
        ComponentUpdate {
            effects: vec![ActionsEffect::Dismiss],
            render: RenderRequest::Immediate,
        }
    }

    fn remove_last_grapheme(&mut self) {
        let Some((index, _)) = self.query.grapheme_indices(true).next_back() else {
            return;
        };
        self.query.truncate(index);
        self.refresh_matches();
    }

    fn refresh_matches(&mut self) {
        self.matches = ACTIONS
            .iter()
            .enumerate()
            .filter(|(_, action)| {
                action.matches(&self.query)
                    || contains_ignore_ascii_case(self.display_label(**action), &self.query)
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
        self.offset = 0;
        self.list_area = Rect::default();
    }

    fn update_mouse(&mut self, mouse: MouseEvent) -> ComponentUpdate<ActionsEffect> {
        if !self
            .list_area
            .contains(Position::new(mouse.column, mouse.row))
        {
            return ComponentUpdate::none();
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.select_previous(),
            MouseEventKind::ScrollDown => self.select_next(),
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from(mouse.row - self.list_area.y);
                if index >= self.matches.len() {
                    return ComponentUpdate::none();
                }
                self.selected = index;
                let mut update = self.trigger_selected();
                update.render = RenderRequest::Immediate;
                return update;
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn select_previous(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = (self.selected + 1).min(self.matches.len() - 1);
    }

    fn trigger_selected(&self) -> ComponentUpdate<ActionsEffect> {
        let Some(action) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        self.trigger(ACTIONS[*action])
    }

    fn trigger(&self, action: Action) -> ComponentUpdate<ActionsEffect> {
        if !self.is_enabled(action) {
            return ComponentUpdate::none();
        }
        ComponentUpdate {
            effects: vec![ActionsEffect::Trigger(action)],
            render: RenderRequest::Immediate,
        }
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let visible_query = visible_query_tail(&self.query, query_width);
        let label_style = Style::default().fg(theme.muted());
        let line = Line::from(vec![
            Span::styled(marker, label_style),
            Span::styled(SEARCH_LABEL, label_style),
            Span::styled(visible_query, Style::default().fg(theme.text())),
        ]);
        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_actions(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, wide: bool) {
        self.list_area = area;
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            frame.render_widget(
                Paragraph::new("  No matching actions").style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let capacity = usize::from(area.height);
        self.offset = self
            .offset
            .min(self.matches.len().saturating_sub(capacity))
            .min(self.selected);
        if self.selected >= self.offset + capacity {
            self.offset = self.selected + 1 - capacity;
        }
        let width = usize::from(area.width).saturating_sub(4);
        for (row, index) in self
            .matches
            .iter()
            .skip(self.offset)
            .take(capacity)
            .enumerate()
        {
            let action = ACTIONS[*index];
            let enabled = self.is_enabled(action);
            let selected = self.offset + row == self.selected;
            let color = if !enabled {
                theme.muted()
            } else if selected {
                theme.accent()
            } else {
                theme.text()
            };
            let style = Style::default().fg(color);
            let alias = action
                .alias()
                .filter(|_| wide && (enabled || action != Action::Memory))
                .map(|alias| format!("(alias: {alias})"))
                .unwrap_or_default();
            let label_width =
                width.saturating_sub(alias.width() + usize::from(!alias.is_empty()) * 2);
            let label = truncate_display(self.display_label(action), label_width);
            let padding = width.saturating_sub(label.width() + alias.width());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(if selected { SELECTION_MARKER } else { "  " }, style),
                    Span::styled(
                        label,
                        if selected && enabled {
                            style.add_modifier(Modifier::BOLD)
                        } else {
                            style
                        },
                    ),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(alias, Style::default().fg(theme.muted())),
                ])),
                Rect::new(area.x, area.y + row as u16, area.width, 1),
            );
        }
    }

    const fn is_enabled(&self, action: Action) -> bool {
        match action {
            Action::Compact => self.availability.compact,
            Action::Handoff | Action::Review | Action::Reflection => self.availability.new_session,
            Action::Subagents => true,
            Action::Effort => true,
            Action::Model => self.availability.model,
            Action::FastMode => true,
            Action::Theme => true,
            Action::NewSession => self.availability.new_session,
            Action::ResumeSession => self.availability.new_session,
            Action::Fork => self.availability.fork,
            Action::Keybindings => true,
            Action::ReloadConfig => true,
            Action::EditConfig => true,
            Action::Memory => self.availability.memory,
            Action::DebugContext => true,
        }
    }

    const fn display_label(&self, action: Action) -> &'static str {
        match action {
            Action::FastMode if self.availability.fast_mode => "Disable fast mode",
            _ => action.label(),
        }
    }

    const fn disabled_reason(&self, action: Action) -> Option<&'static str> {
        if self.is_enabled(action) {
            return None;
        }
        Some(match action {
            Action::Compact => "Finish a conversation turn first",
            Action::Fork => "One fork at a time",
            Action::Model => "Start a new session first",
            Action::Memory => "Enable in config: memory.enabled = true",
            _ => "Finish active work first",
        })
    }
}

impl Action {
    const fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact context",
            Self::Handoff => "Prepare handoff",
            Self::Review => "Review changes",
            Self::Subagents => "Subagents",
            Self::Effort => "Change effort",
            Self::Model => "Select model",
            Self::FastMode => "Enable fast mode",
            Self::Theme => "Select theme",
            Self::NewSession => "New session",
            Self::ResumeSession => "Resume session",
            Self::Fork => "Fork session",
            Self::Keybindings => "Keyboard shortcuts",
            Self::ReloadConfig => "Reload config",
            Self::EditConfig => "Edit config",
            Self::Memory => "Memory",
            Self::DebugContext => "Debug context",
            Self::Reflection => "Reflect on session",
        }
    }

    const fn alias(self) -> Option<&'static str> {
        match self {
            Self::Compact => Some("compress"),
            Self::Handoff => Some("handoff"),
            Self::Review => Some("review"),
            Self::Subagents => Some("agents"),
            Self::Effort => Some("thinking"),
            Self::Model => Some("intelligence"),
            Self::FastMode => Some("priority"),
            Self::Theme => Some("appearance"),
            Self::NewSession => Some("clear"),
            Self::ResumeSession => Some("restore"),
            Self::Fork => Some("btw"),
            Self::ReloadConfig => Some("refresh"),
            Self::Memory => Some("remember/forget"),
            Self::Reflection => Some("reflection"),
            Self::Keybindings | Self::EditConfig | Self::DebugContext => None,
        }
    }

    fn matches(self, query: &str) -> bool {
        contains_ignore_ascii_case(self.label(), query)
            || self
                .alias()
                .is_some_and(|alias| contains_ignore_ascii_case(alias, query))
    }
}

impl Component for ActionsMenu {
    type Event = ActionsEvent;
    type Effect = ActionsEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ActionsEvent::Terminal(Event::Key(key)) => self.update_key(key),
            ActionsEvent::Terminal(Event::Mouse(mouse)) => self.update_mouse(mouse),
            ActionsEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            ActionsEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
        if area.is_empty() {
            return;
        }

        let layout = Floating::new("Actions", 58, 19, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search_area = Rect {
            height: 1,
            ..layout.body
        };
        let wide = area.width >= 54;
        let detail = self
            .matches
            .get(self.selected)
            .map(|index| {
                let action = ACTIONS[*index];
                self.disabled_reason(action)
                    .map(str::to_owned)
                    .or_else(|| {
                        (!wide)
                            .then(|| action.alias().map(|alias| format!("Alias: {alias}")))
                            .flatten()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let width = usize::from(layout.body.width.saturating_sub(4));
        let details = wrap_display_lines(&detail, width);
        let detail_height =
            (details.len().clamp(1, 2) as u16).min(layout.body.height.saturating_sub(2));
        let actions_area = Rect::new(
            layout.body.x,
            layout.body.y + 1,
            layout.body.width,
            layout.body.height.saturating_sub(1 + detail_height),
        );
        self.render_search(frame, search_area, theme);
        self.render_actions(frame, actions_area, theme, wide);
        for (row, line) in details.iter().take(usize::from(detail_height)).enumerate() {
            frame.render_widget(
                Paragraph::new(line.as_str()).style(Style::default().fg(theme.muted())),
                Rect::new(
                    layout.body.x + 2.min(layout.body.width),
                    actions_area.bottom() + row as u16,
                    width as u16,
                    1,
                ),
            );
        }
    }
}

fn contains_ignore_ascii_case(value: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    if query.len() > value.len() {
        return false;
    }
    value
        .as_bytes()
        .windows(query.len())
        .any(|window| window.eq_ignore_ascii_case(query.as_bytes()))
}

fn visible_query_tail(query: &str, width: usize) -> &str {
    let mut used = 0;
    for (index, grapheme) in query.grapheme_indices(true).rev() {
        used += grapheme.width();
        if used > width {
            return &query[index + grapheme.len()..];
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::{Action, ActionAvailability, ActionsEffect, ActionsEvent, ActionsMenu, Component};
    use crate::tui::theme::Theme;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    fn key(code: KeyCode) -> ActionsEvent {
        ActionsEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn available() -> ActionAvailability {
        ActionAvailability {
            compact: true,
            new_session: true,
            fork: true,
            fast_mode: false,
            memory: true,
            model: true,
        }
    }

    #[test]
    fn compaction_action_obeys_idle_availability() {
        for enabled in [false, true] {
            let mut availability = available();
            availability.compact = enabled;
            let mut menu = ActionsMenu::new(availability);
            for character in "compact".chars() {
                menu.update(key(KeyCode::Char(character)));
            }
            let result = menu.update(key(KeyCode::Enter));
            assert_eq!(
                result.effects,
                if enabled {
                    vec![ActionsEffect::Trigger(Action::Compact)]
                } else {
                    vec![]
                }
            );
        }
    }

    fn render(menu: &mut ActionsMenu) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal
            .draw(|frame| menu.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
    }

    fn row_segment(terminal: &Terminal<TestBackend>, y: u16, x: u16, width: u16) -> String {
        let buffer = terminal.backend().buffer();
        (x..x + width)
            .map(|column| buffer[(column, y)].symbol())
            .collect()
    }

    #[test]
    fn popup_aligns_aliases_and_keeps_native_action_order() {
        let mut menu = ActionsMenu::new(available());
        let terminal = render(&mut menu);
        assert_eq!(
            row_segment(&terminal, 0, 1, 58),
            "╭─────────────────────── Actions ────────────────────────╮"
        );
        let first = row_segment(&terminal, 2, 1, 58);
        let second = row_segment(&terminal, 3, 1, 58);
        assert!(first.contains("› Change effort"));
        assert!(second.contains("Enable fast mode"));
        assert_eq!(
            first[..first.find("(alias:").unwrap()].width(),
            second[..second.find("(alias:").unwrap()].width()
        );
        assert!(row_segment(&terminal, 17, 1, 58).contains("enter/tab open"));
        assert!(row_segment(&terminal, 18, 1, 58).ends_with('╯'));
        assert_eq!(
            terminal.backend().buffer()[(4, 2)].fg,
            Theme::default().accent()
        );
    }

    #[test]
    fn search_filters_actions_and_backspace_edits_before_dismissing() {
        let mut menu = ActionsMenu::new(available());
        menu.update(key(KeyCode::Char('E')));
        menu.update(key(KeyCode::Char('F')));
        menu.update(key(KeyCode::Char('F')));

        assert_eq!(menu.matches, [0]);
        assert!(menu.update(key(KeyCode::Backspace)).effects.is_empty());
        assert_eq!(menu.query, "EF");

        menu.update(key(KeyCode::Backspace));
        menu.update(key(KeyCode::Backspace));
        assert!(matches!(
            menu.update(key(KeyCode::Backspace)).effects.as_slice(),
            [ActionsEffect::Dismiss]
        ));
    }

    #[test]
    fn arrows_navigate_while_typing_continues_to_search() {
        let mut menu = ActionsMenu::new(available());
        menu.update(key(KeyCode::Down));
        assert_eq!(menu.selected, 1);
        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::FastMode)]
        );

        menu.update(key(KeyCode::Char('t')));
        menu.update(key(KeyCode::Down));
        menu.update(key(KeyCode::Char('h')));
        assert_eq!(menu.query, "th");
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn tab_triggers_the_selected_action() {
        let mut menu = ActionsMenu::new(available());
        menu.update(key(KeyCode::Down));

        assert_eq!(
            menu.update(key(KeyCode::Tab)).effects,
            [ActionsEffect::Trigger(Action::FastMode)]
        );
    }

    #[test]
    fn handoff_alias_triggers_the_handoff_action() {
        let mut menu = ActionsMenu::new(available());
        for character in "handoff".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Handoff)]
        );
    }

    #[test]
    fn effort_action_triggers_when_available() {
        let mut enabled = ActionsMenu::new(available());
        for character in "thinking".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Effort)]
        );
    }

    #[test]
    fn model_action_is_available_only_before_the_first_prompt() {
        let mut enabled = ActionsMenu::new(available());
        for character in "intelligence".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Model)]
        );

        let mut availability = available();
        availability.model = false;
        let mut disabled = ActionsMenu::new(availability);
        for character in "intelligence".chars() {
            disabled.update(key(KeyCode::Char(character)));
        }
        let terminal = render(&mut disabled);
        assert!((0..20).any(|row| {
            row_segment(&terminal, row, 0, 60).contains("Start a new session first")
        }));
        assert!(disabled.update(key(KeyCode::Enter)).effects.is_empty());
    }

    #[test]
    fn review_action_is_searchable() {
        let mut menu = ActionsMenu::new(available());
        for character in "review".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Review)]
        );
    }

    #[test]
    fn reflection_action_is_searchable_and_waits_for_idle_work() {
        let mut enabled = ActionsMenu::new(available());
        for character in "reflection".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Reflection)]
        );

        let mut availability = available();
        availability.new_session = false;
        let mut disabled = ActionsMenu::new(availability);
        for character in "reflection".chars() {
            disabled.update(key(KeyCode::Char(character)));
        }
        assert!(disabled.update(key(KeyCode::Enter)).effects.is_empty());
    }

    #[test]
    fn fast_mode_search_matches_visible_original_and_alias_labels() {
        for query in ["disable fast mode", "enable fast mode", "priority"] {
            let mut availability = available();
            availability.fast_mode = true;
            let mut menu = ActionsMenu::new(availability);
            menu.update(ActionsEvent::Terminal(Event::Paste(query.to_owned())));
            assert_eq!(
                menu.update(key(KeyCode::Enter)).effects,
                [ActionsEffect::Trigger(Action::FastMode)],
                "{query}"
            );
        }
    }

    #[test]
    fn fast_mode_action_reflects_the_current_setting() {
        let mut enabled = ActionsMenu::new(available());
        for character in "priority".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::FastMode)]
        );

        let mut availability = available();
        availability.fast_mode = true;
        let mut disabled = ActionsMenu::new(availability);
        let terminal = render(&mut disabled);
        assert!(row_segment(&terminal, 3, 1, 58).contains("Disable fast mode"));
    }

    #[test]
    fn config_actions_are_individually_searchable() {
        let mut menu = ActionsMenu::new(available());
        for character in "edit config".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::EditConfig)]
        );

        let mut reload = ActionsMenu::new(available());
        for character in "refresh".chars() {
            reload.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            reload.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::ReloadConfig)]
        );
    }

    #[test]
    fn new_session_action_supports_clear_alias_and_busy_explanation() {
        let mut enabled = ActionsMenu::new(available());
        for character in "clear".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::NewSession)]
        );

        let mut availability = available();
        availability.new_session = false;
        let mut disabled = ActionsMenu::new(availability);
        disabled.update(key(KeyCode::Down));
        disabled.update(key(KeyCode::Down));
        disabled.update(key(KeyCode::Down));
        let terminal = render(&mut disabled);
        assert!(row_segment(&terminal, 5, 1, 58).contains("› New session"));
        assert!(
            (0..20)
                .any(|row| row_segment(&terminal, row, 0, 60).contains("Finish active work first"))
        );
        assert_eq!(
            terminal.backend().buffer()[(4, 5)].fg,
            Theme::default().muted()
        );
    }

    #[test]
    fn resume_session_action_supports_restore_alias() {
        let mut menu = ActionsMenu::new(available());
        for character in "restore".chars() {
            menu.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::ResumeSession)]
        );
    }

    #[test]
    fn theme_action_is_searchable_by_appearance() {
        let mut menu = ActionsMenu::new(available());
        for character in "appearance".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Theme)]
        );
    }

    #[test]
    fn keybindings_action_is_searchable_and_triggers_immediately() {
        let mut menu = ActionsMenu::new(available());
        for character in "keyboard".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Keybindings)]
        );
    }

    #[test]
    fn debug_context_action_is_searchable_and_triggers() {
        let mut menu = ActionsMenu::new(available());
        for character in "debug context".chars() {
            menu.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::DebugContext)]
        );
    }

    #[test]
    fn memory_aliases_are_searchable_and_disabled_state_explains_configuration() {
        for alias in ["remember", "forget"] {
            let mut enabled = ActionsMenu::new(available());
            for character in alias.chars() {
                enabled.update(key(KeyCode::Char(character)));
            }
            assert_eq!(
                enabled.update(key(KeyCode::Enter)).effects,
                [ActionsEffect::Trigger(Action::Memory)]
            );
        }

        let mut availability = available();
        availability.memory = false;
        let mut disabled = ActionsMenu::new(availability);
        for character in "memory".chars() {
            disabled.update(key(KeyCode::Char(character)));
        }
        assert!(disabled.update(key(KeyCode::Enter)).effects.is_empty());

        let terminal = render(&mut disabled);
        assert!(row_segment(&terminal, 2, 1, 58).contains("› Memory"));
        assert!((0..20).any(|row| {
            row_segment(&terminal, row, 0, 60).contains("Enable in config: memory.enabled = true")
        }));
        assert_eq!(
            terminal.backend().buffer()[(4, 2)].fg,
            Theme::default().muted()
        );
    }

    #[test]
    fn fork_alias_is_searchable_and_disabled_while_a_fork_is_open() {
        let mut enabled = ActionsMenu::new(available());
        for character in "btw".chars() {
            enabled.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            enabled.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Fork)]
        );

        let mut availability = available();
        availability.fork = false;
        let mut disabled = ActionsMenu::new(availability);
        for character in "btw".chars() {
            disabled.update(key(KeyCode::Char(character)));
        }
        assert!(disabled.update(key(KeyCode::Enter)).effects.is_empty());
    }

    #[test]
    fn enter_does_nothing_when_search_has_no_matches() {
        let mut menu = ActionsMenu::new(available());
        menu.update(key(KeyCode::Char('z')));

        assert!(menu.update(key(KeyCode::Enter)).effects.is_empty());
    }

    #[test]
    fn escape_dismisses() {
        let mut menu = ActionsMenu::new(available());

        assert!(matches!(
            menu.update(key(KeyCode::Esc)).effects.as_slice(),
            [ActionsEffect::Dismiss]
        ));
    }

    #[test]
    fn narrow_terminals_do_not_overflow_the_popup() {
        let mut menu = ActionsMenu::new(available());
        let mut terminal = Terminal::new(TestBackend::new(3, 2)).unwrap();

        terminal
            .draw(|frame| menu.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 3);
    }
    #[test]
    fn mouse_uses_the_visible_scrolled_row_and_disabled_guard() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut availability = available();
        availability.compact = false;
        let mut menu = ActionsMenu::new(availability);
        for _ in 0..30 {
            menu.update(key(KeyCode::Down));
        }
        render(&mut menu);
        let mouse = |kind, row| {
            ActionsEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column: menu.list_area.x,
                row,
                modifiers: KeyModifiers::NONE,
            }))
        };
        let row = menu.list_area.y + (menu.selected - menu.offset) as u16;
        let click = mouse(MouseEventKind::Down(MouseButton::Left), row);
        assert!(menu.update(click).effects.is_empty());
        assert_eq!(menu.selected, 16);
        menu.set_fork_available(false);
        menu.update(ActionsEvent::Terminal(Event::Paste("btw".into())));
        render(&mut menu);
        let click = ActionsEvent::Terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: menu.list_area.x,
            row: menu.list_area.y,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(menu.update(click).effects.is_empty());
        menu.set_fork_available(true);
        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::Fork)]
        );
    }

    #[test]
    fn narrow_alias_and_empty_state_are_visible_and_tiny_areas_are_safe() {
        let mut menu = ActionsMenu::new(available());
        let mut terminal = Terminal::new(TestBackend::new(32, 19)).unwrap();
        terminal
            .draw(|frame| menu.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Alias: thinking"));
        menu.update(ActionsEvent::Terminal(Event::Paste("zzzz".into())));
        terminal
            .draw(|frame| menu.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("No matching actions"));
        for width in 0..12 {
            for height in 0..8 {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| menu.render(frame, frame.area(), &Theme::default()))
                    .unwrap();
            }
        }
    }
    #[test]
    fn mouse_wheel_and_click_follow_keyboard_action_selection() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut menu = ActionsMenu::new(available());
        render(&mut menu);
        let mouse = |kind, area: ratatui::layout::Rect| {
            ActionsEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }))
        };
        menu.update(mouse(MouseEventKind::ScrollDown, menu.list_area));
        assert_eq!(
            menu.update(key(KeyCode::Enter)).effects,
            [ActionsEffect::Trigger(Action::FastMode)]
        );
        menu.update(mouse(MouseEventKind::ScrollUp, menu.list_area));
        assert_eq!(
            menu.update(mouse(
                MouseEventKind::Down(MouseButton::Left),
                menu.list_area
            ))
            .effects,
            menu.update(key(KeyCode::Enter)).effects
        );
    }
}
