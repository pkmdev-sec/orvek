//! Searchable picker for resumable persisted sessions.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{
    app::config::ReasoningMode,
    sessions::checkpoint::SessionSummary,
    tui::{
        format::{format_age, sanitize_terminal_text_inline, truncate_display, wrap_display_lines},
        theme::Theme,
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const RESUME_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "resume"), ("esc", "close")];
const MENTION_KEY_BINDINGS: [(&str, &str); 3] =
    [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";

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
    mode: SessionPickerMode,
    list_area: Rect,
    offset: usize,
    last_click: Option<(usize, Instant)>,
}

impl SessionPicker {
    pub(super) fn new(sessions: Vec<SessionSummary>, mode: SessionPickerMode) -> Self {
        let matches = (0..sessions.len()).collect();
        Self {
            sessions,
            query: String::new(),
            matches,
            selected: 0,
            mode,
            list_area: Rect::default(),
            offset: 0,
            last_click: None,
        }
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        self.last_click = None;
        match key.code {
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
        self.offset = 0;
        self.last_click = None;
        self.list_area = Rect::default();
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }
        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let query = visible_tail(&self.query, query_width);
        let label_style = Style::default().fg(theme.muted());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, label_style),
                Span::styled(SEARCH_LABEL, label_style),
                Span::styled(query, Style::default().fg(theme.text())),
            ])),
            area,
        );
    }

    fn update_mouse(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
    ) -> ComponentUpdate<SessionPickerEffect> {
        if !self
            .list_area
            .contains(Position::new(mouse.column, mouse.row))
        {
            self.last_click = None;
            return ComponentUpdate::none();
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                self.last_click = None;
                self.selected = self.selected.saturating_sub(1);
            }
            MouseEventKind::ScrollDown => {
                self.last_click = None;
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let index = self.offset + usize::from((mouse.row - self.list_area.y) / 2);
                if index >= self.matches.len() {
                    return ComponentUpdate::none();
                }
                let confirm = self.last_click.is_some_and(|(previous, time)| {
                    previous == index
                        && now.saturating_duration_since(time) <= Duration::from_millis(500)
                });
                self.selected = index;
                self.last_click = Some((index, now));
                if confirm {
                    self.last_click = None;
                    return self.select();
                }
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_sessions(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect {
            height: area.height / 2 * 2,
            ..area
        };
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            let message = if !self.sessions.is_empty() {
                "  No matching sessions"
            } else {
                match self.mode {
                    SessionPickerMode::Resume => "  No resumable sessions found",
                    SessionPickerMode::Mention => "  No other sessions found",
                }
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let capacity = usize::from(self.list_area.height / 2);
        if capacity == 0 {
            return;
        }
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
            let session = &self.sessions[*index];
            let selected = row + self.offset == self.selected;
            let preview = sanitize_terminal_text_inline(&session.preview);
            let preview = if preview.trim().is_empty() {
                "No preview available"
            } else {
                &preview
            };
            let age = truncate_display(&format_age(session.started_at_unix_ms), width.min(7));
            let title = truncate_display(preview, width.saturating_sub(age.width() + 2));
            let padding = width.saturating_sub(title.width() + age.width());
            let style = Style::default()
                .fg(if selected {
                    theme.accent()
                } else {
                    theme.text()
                })
                .add_modifier(Modifier::BOLD);
            let y = area.y + row as u16 * 2;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(title, style),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(age, Style::default().fg(theme.muted())),
                ])),
                Rect::new(area.x, y, area.width, 1),
            );
            let suffix = format!(
                " · {}{}",
                session.effort.as_str(),
                if session.reasoning_mode == ReasoningMode::Pro {
                    " · pro"
                } else {
                    ""
                }
            );
            let model = truncate_display(&session.model, width.saturating_sub(suffix.width()));
            frame.render_widget(
                Paragraph::new(format!(
                    "  {}",
                    truncate_display(&format!("{model}{suffix}"), width)
                ))
                .style(Style::default().fg(theme.muted())),
                Rect::new(area.x, y + 1, area.width, 1),
            );
        }
    }

    fn details(&self, width: usize) -> Vec<String> {
        let Some(index) = self.matches.get(self.selected) else {
            return Vec::new();
        };
        let session = &self.sessions[*index];
        let mut lines = wrap_display_lines(
            &sanitize_terminal_text_inline(&session.session_id),
            width.saturating_sub(4),
        )
        .into_iter()
        .enumerate()
        .map(|(index, line)| format!("{}{line}", if index == 0 { "ID: " } else { "    " }))
        .collect::<Vec<_>>();
        lines.extend(wrap_display_lines(
            &format!(
                "Workspace: {}",
                sanitize_terminal_text_inline(&session.workspace.to_string_lossy())
            ),
            width,
        ));
        lines
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
            SessionPickerEvent::Terminal(Event::Mouse(mouse)) => {
                self.update_mouse(mouse, Instant::now())
            }
            SessionPickerEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            SessionPickerEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
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
        let width = usize::from(layout.body.width).saturating_sub(4);
        let details = self.details(width);
        let detail_height = (details.len().max(1) as u16).min(layout.body.height.saturating_sub(4));
        let sessions = Rect::new(
            layout.body.x,
            layout.body.y + 2,
            layout.body.width,
            layout.body.height.saturating_sub(2 + detail_height),
        )
        .intersection(layout.body);
        self.render_search(frame, search, theme);
        if layout.body.height > 1 {
            let header = if width >= 16 {
                format!("  Session{}Started", " ".repeat(width.saturating_sub(14)))
            } else {
                "  Session".to_owned()
            };
            frame.render_widget(
                Paragraph::new(header).style(Style::default().fg(theme.muted())),
                Rect::new(layout.body.x, layout.body.y + 1, layout.body.width, 1),
            );
        }
        self.render_sessions(frame, sessions, theme);
        for (row, line) in details.iter().take(usize::from(detail_height)).enumerate() {
            frame.render_widget(
                Paragraph::new(line.as_str()).style(Style::default().fg(theme.muted())),
                Rect::new(
                    layout.body.x + 2.min(layout.body.width),
                    layout.body.bottom() - detail_height + row as u16,
                    width as u16,
                    1,
                ),
            );
        }
    }
}

fn visible_tail(query: &str, width: usize) -> &str {
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
    use super::{
        Component, SessionPicker, SessionPickerEffect, SessionPickerEvent, SessionPickerMode,
    };
    use crate::{
        app::config::{ReasoningEffort, ReasoningMode},
        sessions::checkpoint::SessionSummary,
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
            effort: ReasoningEffort::Medium,
            reasoning_mode: ReasoningMode::Standard,
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
    fn render(
        picker: &mut SessionPicker,
        width: u16,
        height: u16,
    ) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        terminal
    }

    #[test]
    fn preview_metadata_and_wrapped_id_use_saved_values() {
        let id = "019ce701-8101-7001-8a01-000000000001";
        let mut session = summary(id, "Same preview");
        session.reasoning_mode = ReasoningMode::Pro;
        let mut picker = SessionPicker::new(vec![session], SessionPickerMode::Resume);
        let terminal = render(&mut picker, 76, 18);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        for label in [
            "Same preview",
            "Started",
            "gpt · medium · pro",
            id,
            "Workspace: /work",
        ] {
            assert!(text.contains(label), "{label}");
        }
        let lines = picker.details(26);
        assert_eq!(
            lines
                .iter()
                .take(2)
                .map(|line| &line[4..])
                .collect::<String>(),
            id
        );
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SessionPickerEffect::Resume(id.into())]
        );
    }

    #[test]
    fn mouse_inspects_then_confirms_the_same_exact_session_as_keyboard() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let now = std::time::Instant::now();
        for mode in [SessionPickerMode::Resume, SessionPickerMode::Mention] {
            let mut picker = SessionPicker::new(
                (0..12)
                    .map(|index| summary(&format!("id-{index}"), "Duplicate preview"))
                    .collect(),
                mode,
            );
            for _ in 0..11 {
                picker.update(key(KeyCode::Down));
            }
            render(&mut picker, 32, 18);
            let target = picker.list_area;
            let index = picker.offset;
            let mouse = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: target.x,
                row: target.y,
                modifiers: KeyModifiers::NONE,
            };
            assert!(picker.update_mouse(mouse, now).effects.is_empty());
            assert_eq!(picker.selected, index);
            let clicked = picker
                .update_mouse(mouse, now + std::time::Duration::from_millis(100))
                .effects;
            let keyboard = picker.update(key(KeyCode::Tab)).effects;
            assert_eq!(clicked, keyboard);
            let expected = match mode {
                SessionPickerMode::Resume => SessionPickerEffect::Resume(format!("id-{index}")),
                SessionPickerMode::Mention => SessionPickerEffect::Mention(format!("id-{index}")),
            };
            assert_eq!(clicked, [expected]);
        }
    }

    #[test]
    fn empty_discovery_differs_from_no_matches_and_display_is_sanitized() {
        let mut picker = SessionPicker::new(vec![], SessionPickerMode::Resume);
        let terminal = render(&mut picker, 76, 18);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("No resumable sessions found"));
        let mut picker = SessionPicker::new(
            vec![summary("one", "漢字\x1b\npreview")],
            SessionPickerMode::Mention,
        );
        let terminal = render(&mut picker, 32, 18);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .all(|cell| !cell.symbol().contains(char::is_control))
        );
        picker.update(SessionPickerEvent::Terminal(Event::Paste("zzzz".into())));
        let terminal = render(&mut picker, 76, 18);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("No matching sessions"));
        assert!(picker.update(key(KeyCode::Enter)).effects.is_empty());
        for width in 0..18 {
            for height in 0..12 {
                render(&mut picker, width, height);
            }
        }
    }
    #[test]
    fn wheel_and_arrow_navigation_have_the_same_clamped_selection() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut wheel = SessionPicker::new(
            vec![summary("one", "First"), summary("two", "Second")],
            SessionPickerMode::Resume,
        );
        let mut arrows = SessionPicker::new(
            vec![summary("one", "First"), summary("two", "Second")],
            SessionPickerMode::Resume,
        );
        render(&mut wheel, 76, 18);
        for (kind, code) in [
            (MouseEventKind::ScrollDown, KeyCode::Down),
            (MouseEventKind::ScrollDown, KeyCode::Down),
            (MouseEventKind::ScrollUp, KeyCode::Up),
            (MouseEventKind::ScrollUp, KeyCode::Up),
        ] {
            wheel.update(SessionPickerEvent::Terminal(Event::Mouse(MouseEvent {
                kind,
                column: wheel.list_area.x,
                row: wheel.list_area.y,
                modifiers: KeyModifiers::NONE,
            })));
            arrows.update(key(code));
            assert_eq!(
                wheel.update(key(KeyCode::Enter)).effects,
                arrows.update(key(KeyCode::Enter)).effects
            );
        }
    }
}
