//! Searchable picker for resumable persisted sessions.

use super::{
    choice::{ChoicePicker, ScrollIndicator},
    dialog::Dialog,
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
    layout::{Constraint, Position, Rect},
    style::Style,
    text::{Line, Span, Text},
    widgets::{Cell, ListItem, Paragraph, Row, Table, TableState},
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
    choice: ChoicePicker,
    mode: SessionPickerMode,
}

impl SessionPicker {
    pub(super) fn new(sessions: Vec<SessionSummary>, mode: SessionPickerMode) -> Self {
        let matches = (0..sessions.len()).collect::<Vec<_>>();
        Self {
            sessions,
            query: String::new(),
            choice: ChoicePicker::new(matches.len(), 2),
            matches,
            mode,
        }
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<SessionPickerEffect> {
        if self.choice.move_by(delta) {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            ComponentUpdate::none()
        }
    }

    fn page_by(&mut self, pages: isize) -> ComponentUpdate<SessionPickerEffect> {
        if self.choice.page_by(pages) {
            ComponentUpdate::render(RenderRequest::Immediate)
        } else {
            ComponentUpdate::none()
        }
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::PageUp => self.page_by(-1),
            KeyCode::PageDown => self.page_by(1),
            KeyCode::Esc => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_matches();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::effect(SessionPickerEffect::Dismiss),
            KeyCode::Up => self.select_bounded(-1),
            KeyCode::Down => self.select_bounded(1),
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
        let Some(index) = self.matches.get(self.choice.selected_or_zero()) else {
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
        self.choice.reset(self.matches.len());
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        SearchField::new(&self.query).render(frame, area, theme);
    }

    fn render_session_list(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
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
            let typography = ChoiceStyle::new(self.choice.is_selected(position), true);
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
        self.choice
            .render(frame, area, items.collect(), true, theme);
    }

    fn render_sessions(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width >= 92 && !self.matches.is_empty() {
            self.render_session_table(frame, area, theme);
        } else {
            self.render_session_list(frame, area, theme);
        }
    }

    fn render_session_table(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.choice.set_item_count(self.matches.len());
        self.choice.set_area(area);
        let selected = self.choice.selected();
        let rows = self.matches.iter().enumerate().map(|(position, index)| {
            let session = &self.sessions[*index];
            let selected = selected == Some(position);
            let typography = ChoiceStyle::new(selected, true);
            let marker = if selected { CHOICE_MARKER } else { " " };
            Row::new([
                Cell::from(Text::from(vec![
                    Line::styled(marker, Style::default().fg(theme.accent())),
                    Line::raw(""),
                ])),
                Cell::from(Text::from(vec![
                    Line::styled(
                        format_age(session.started_at_unix_ms),
                        typography.primary(theme),
                    ),
                    Line::raw(""),
                ])),
                Cell::from(Text::from(vec![
                    Line::styled(
                        crate::tui::format::sanitize_terminal_text_inline(&session.session_id)
                            .into_owned(),
                        typography.primary(theme),
                    ),
                    Line::styled(
                        crate::tui::format::sanitize_terminal_text_inline(&session.preview)
                            .into_owned(),
                        typography.detail(theme),
                    ),
                ])),
                Cell::from(Text::from(vec![
                    Line::styled(
                        crate::tui::format::sanitize_terminal_text_inline(&session.model)
                            .into_owned(),
                        Style::default().fg(theme.accent()),
                    ),
                    Line::styled(
                        session
                            .effort
                            .map(|effort| effort.as_str())
                            .unwrap_or("unknown"),
                        typography.detail(theme),
                    ),
                ])),
                Cell::from(Text::from(vec![
                    Line::styled(
                        crate::tui::format::sanitize_terminal_text_inline(
                            &session.workspace.display().to_string(),
                        )
                        .into_owned(),
                        typography.primary(theme),
                    ),
                    Line::raw(""),
                ])),
            ])
            .height(2)
            .style(typography.highlight(theme))
        });
        let overflow = self.matches.len() > self.choice.visible_items();
        let table_area = if overflow && area.width > 1 {
            Rect {
                width: area.width - 1,
                ..area
            }
        } else {
            area
        };
        let mut state = TableState::default().with_offset(self.choice.offset());
        frame.render_stateful_widget(
            Table::new(
                rows,
                [
                    Constraint::Length(1),
                    Constraint::Length(9),
                    Constraint::Min(24),
                    Constraint::Length(12),
                    Constraint::Length(24),
                ],
            )
            .column_spacing(1),
            table_area,
            &mut state,
        );
        if overflow {
            ScrollIndicator::new(
                self.choice.offset(),
                self.choice.visible_items(),
                self.matches.len(),
            )
            .render(
                frame,
                Rect {
                    x: area.right() - 1,
                    width: 1,
                    ..area
                },
                theme,
            );
        }
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
                if self.choice.contains(Position::new(mouse.column, mouse.row)) =>
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
        self.choice.set_area(Rect::default());
        let (title, key_bindings) = match self.mode {
            SessionPickerMode::Resume => ("Resume session", &RESUME_KEY_BINDINGS),
            SessionPickerMode::Mention => ("Mention session", &MENTION_KEY_BINDINGS),
        };
        let layout = Dialog::new(title, 104, 18, key_bindings).render(frame, area, theme);
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
        self.choice.set_area(sessions);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
    }
    #[test]
    fn wide_picker_aligns_session_metadata_in_a_table() {
        let mut picker = SessionPicker::new(
            vec![summary("session-01", "fix parser")],
            SessionPickerMode::Resume,
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 18)).unwrap();

        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for expected in ["session-01", "fix parser", "gpt", "medium", "/work"] {
            assert!(
                rendered.contains(expected),
                "missing {expected:?}: {rendered}"
            );
        }
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
        let body = picker.choice.area();
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.choice.selected_or_zero(), 1);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
        picker.update(SessionPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.choice.selected_or_zero(),
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
        assert_eq!(picker.choice.selected_or_zero(), last);
        picker.update(mouse(
            crossterm::event::MouseEventKind::ScrollDown,
            body.x,
            body.y,
        ));
        assert_eq!(picker.choice.selected_or_zero(), last);
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        assert_eq!(picker.choice.selected_or_zero(), last);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
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
        assert_eq!(picker.choice.selected_or_zero(), 0);
    }
}
