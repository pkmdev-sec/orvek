//! Searchable picker for resumable persisted sessions.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
    typography::{CHOICE_MARKER, ChoiceStyle, SearchField},
};
use crate::tui::{
    session::{SessionSummary, format_age},
    theme::Theme,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;

const RESUME_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "resume"), ("esc", "close")];
const MENTION_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];

pub(super) enum SessionPickerEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SessionPickerEffect {
    Dismiss,
    Resume(String),
    Mention(String),
}

#[derive(Clone, Copy)]
pub(super) enum SessionPickerMode {
    Resume,
    Mention,
}

pub(super) struct SessionPicker {
    sessions: Vec<SessionSummary>,
    query: String,
    matches: Vec<usize>,
    selected: usize,
    navigation_area: Rect,
    mode: SessionPickerMode,
}

impl SessionPicker {
    pub(super) fn new(sessions: Vec<SessionSummary>, mode: SessionPickerMode) -> Self {
        let matches = (0..sessions.len()).collect();
        Self {
            sessions,
            query: String::new(),
            matches,
            selected: 0,
            navigation_area: Rect::default(),
            mode,
        }
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<SessionPickerEffect> {
        let next = self
            .selected
            .saturating_add_signed(delta)
            .min(self.matches.len().saturating_sub(1));
        if next == self.selected {
            return ComponentUpdate::none();
        }
        self.selected = next;
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::PageUp => self.select_bounded(
                -(isize::try_from(self.navigation_area.height / 2)
                    .unwrap_or(1)
                    .max(1)),
            ),
            KeyCode::PageDown => self.select_bounded(
                isize::try_from(self.navigation_area.height / 2)
                    .unwrap_or(1)
                    .max(1),
            ),
            KeyCode::Esc => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_matches();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1).min(self.matches.len() - 1);
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter | KeyCode::Tab => self.select(),
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

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<SessionPickerEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_matches();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn select(&mut self) -> ComponentUpdate<SessionPickerEffect> {
        let Some(index) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        let session_id = self.sessions[*index].session_id.clone();
        let effect = match self.mode {
            SessionPickerMode::Resume => SessionPickerEffect::Resume(session_id),
            SessionPickerMode::Mention => SessionPickerEffect::Mention(session_id),
        };
        Self::effect(effect)
    }

    fn effect(effect: SessionPickerEffect) -> ComponentUpdate<SessionPickerEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn refresh_matches(&mut self) {
        let query = self.query.to_ascii_lowercase();
        self.matches = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, session)| session.matches(&query))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        SearchField::new(&self.query).render(frame, area, theme);
    }

    fn render_sessions(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            let message = match self.mode {
                SessionPickerMode::Resume => "  No resumable sessions found",
                SessionPickerMode::Mention => "  No other sessions found",
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let items = self.matches.iter().enumerate().map(|(position, index)| {
            let session = &self.sessions[*index];
            let typography = ChoiceStyle::new(position == self.selected, true);
            let title = format!(
                "{} · {}",
                format_age(session.started_at_unix_ms),
                session.session_id,
            );
            let detail = format!(
                "{} · {} · {} · {}",
                session.preview,
                session.model,
                session
                    .effort
                    .map(|effort| effort.as_str())
                    .unwrap_or("unknown effort"),
                session.workspace.display()
            );
            ListItem::new(vec![
                Line::from(Span::styled(
                    crate::tui::format::sanitize_terminal_text_inline(&title).into_owned(),
                    typography.primary(theme),
                )),
                Line::from(Span::styled(
                    crate::tui::format::sanitize_terminal_text_inline(&detail).into_owned(),
                    typography.detail(theme),
                )),
            ])
        });
        let list = List::new(items)
            .highlight_symbol(CHOICE_MARKER)
            .highlight_style(ChoiceStyle::new(true, true).highlight(theme));
        let selected = (!self.matches.is_empty()).then_some(self.selected);
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl SessionSummary {
    fn matches(&self, query: &str) -> bool {
        query.is_empty()
            || self.session_id.to_ascii_lowercase().contains(query)
            || self.preview.to_ascii_lowercase().contains(query)
            || self.model.to_ascii_lowercase().contains(query)
            || self
                .workspace
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains(query)
    }
}

impl Component for SessionPicker {
    type Event = SessionPickerEvent;
    type Effect = SessionPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            SessionPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            SessionPickerEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            SessionPickerEvent::Terminal(Event::Mouse(mouse))
                if self
                    .navigation_area
                    .contains(Position::new(mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.select_bounded(-1),
                    MouseEventKind::ScrollDown => self.select_bounded(1),
                    _ => ComponentUpdate::none(),
                }
            }
            SessionPickerEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.navigation_area = Rect::default();
        let (title, key_bindings) = match self.mode {
            SessionPickerMode::Resume => ("Resume session", &RESUME_KEY_BINDINGS),
            SessionPickerMode::Mention => ("Mention session", &MENTION_KEY_BINDINGS),
        };
        let layout = Floating::new(title, 76, 18, key_bindings).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search = Rect {
            height: 1,
            ..layout.body
        };
        let sessions = Rect {
            y: layout.body.y + 1,
            height: layout.body.height.saturating_sub(1),
            ..layout.body
        };
        self.navigation_area = sessions;
        self.render_search(frame, search, theme);
        self.render_sessions(frame, sessions, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Component, SessionPicker, SessionPickerEffect, SessionPickerEvent, SessionPickerMode,
    };
    use crate::{
        app::config::{ReasoningEffort, ReasoningMode},
        tui::session::SessionSummary,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn key(code: KeyCode) -> SessionPickerEvent {
        SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn summary(id: &str, preview: &str) -> SessionSummary {
        SessionSummary {
            session_id: id.to_owned(),
            started_at_unix_ms: 1,
            model: "gpt".to_owned(),
            effort: Some(ReasoningEffort::Medium),
            reasoning_mode: Some(ReasoningMode::Standard),
            workspace: PathBuf::from("/work"),
            preview: preview.to_owned(),
        }
    }

    #[test]
    fn search_selects_a_session_by_preview() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );
        for character in "docs".chars() {
            picker.update(key(KeyCode::Char(character)));
        }
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );
    }

    #[test]
    fn mention_mode_returns_a_reference_instead_of_resuming() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser")],
            SessionPickerMode::Mention,
        );

        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Mention("one".to_owned())]
        );
    }

    #[test]
    fn tab_resumes_the_selected_session() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );
        for character in "docs".chars() {
            picker.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            picker.update(key(KeyCode::Tab)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );
    }

    #[test]
    fn arrows_navigate_while_typing_continues_to_search() {
        let mut picker = SessionPicker::new(
            vec![summary("one", "fix parser"), summary("two", "write docs")],
            SessionPickerMode::Resume,
        );

        picker.update(key(KeyCode::Down));
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Resume("two".to_owned())]
        );

        for character in "fix".chars() {
            picker.update(key(KeyCode::Char(character)));
        }
        assert_eq!(picker.query, "fix");
        assert_eq!(picker.matches, [0]);
        assert_eq!(picker.selected, 0);
    }
    #[test]
    fn rendered_picker_bounds_wheel_and_page_navigation() {
        let mut picker = SessionPicker::new(
            (0..30)
                .map(|index| summary(&format!("session-{index:02}"), "preview"))
                .collect(),
            SessionPickerMode::Resume,
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        let body = picker.navigation_area;
        assert!(!body.is_empty());
        let mouse = |kind, column, row| {
            SessionPickerEvent::Terminal(Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }))
        };
        picker.update(mouse(crossterm::event::MouseEventKind::ScrollDown, 0, 0));
        assert_eq!(picker.selected, 0);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 1);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollUp,
            body.x,
            body.y,
        ));
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollUp,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 0);
        picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.selected,
            picker
                .matches
                .len()
                .saturating_sub(1)
                .min(usize::from(body.height / 2).max(1))
        );
        for _ in 0..40 {
            picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE,
            ))));
        }
        let last = picker.matches.len().saturating_sub(1);
        assert_eq!(picker.selected, last);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, last);
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        assert_eq!(picker.selected, last);
        let buffer = terminal.backend().buffer();
        assert!((body.y..body.bottom()).any(|row| {
            let text = (body.x..body.right())
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>();
            text.contains("› ") && text.contains("session-29")
        }));
        for _ in 0..40 {
            picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::NONE,
            ))));
        }
        assert_eq!(picker.selected, 0);
        terminal
            .draw(|frame| {
                picker.render(
                    frame,
                    ratatui::layout::Rect::default(),
                    &crate::tui::theme::Theme::default(),
                )
            })
            .unwrap();
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.selected, 0);
    }
}
