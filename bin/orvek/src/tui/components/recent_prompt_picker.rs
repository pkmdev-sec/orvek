//! Picker for prompts from the current session or all persisted sessions.

use super::{
    file_finder::{fuzzy_score, visible_query_tail},
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{
    sessions::checkpoint::RecentPrompt,
    tui::{
        format::{format_age, sanitize_terminal_text_inline, truncate_display, wrap_display_lines},
        theme::Theme,
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use std::{
    cmp::Reverse,
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const KEY_BINDINGS: [(&str, &str); 5] = [
    ("↑↓", "move"),
    ("pgup/pgdn", "preview"),
    ("enter/tab", "use"),
    ("ctrl+f", "scope"),
    ("esc", "close"),
];
const LIST_HEIGHT: u16 = 7;
const SEARCH_LABEL: &str = "Search: ";

pub(super) enum RecentPromptPickerEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum RecentPromptPickerEffect {
    Dismiss,
    Insert(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecentPromptScope {
    Global,
    CurrentSession,
}

pub(super) struct RecentPromptPicker {
    prompts: Vec<RecentPrompt>,
    current_session_id: String,
    scope: RecentPromptScope,
    query: String,
    visible: Vec<usize>,
    selected: usize,
    preview_scroll: u16,
    max_preview_scroll: u16,
    labels: Vec<String>,
    ages: Vec<String>,
    preview: Option<Preview>,
    list_area: Rect,
    preview_area: Rect,
    offset: usize,
    last_click: Option<(usize, Instant)>,
    has_draft_images: bool,
}

struct Preview {
    index: usize,
    width: u16,
    lines: Vec<String>,
    source_start: usize,
}

impl RecentPromptPicker {
    pub(super) fn new(prompts: Vec<RecentPrompt>, current_session_id: String) -> Self {
        let visible = (0..prompts.len()).collect();
        let labels = prompts
            .iter()
            .map(|prompt| one_line_preview(&prompt.text))
            .collect();
        let ages = prompts
            .iter()
            .map(|prompt| format_age(prompt.recorded_at_unix_ms))
            .collect();
        Self {
            prompts,
            current_session_id,
            scope: RecentPromptScope::Global,
            query: String::new(),
            visible,
            selected: 0,
            preview_scroll: 0,
            max_preview_scroll: 0,
            labels,
            ages,
            preview: None,
            list_area: Rect::default(),
            preview_area: Rect::default(),
            offset: 0,
            last_click: None,
            has_draft_images: false,
        }
    }

    pub(super) fn set_has_draft_images(&mut self, has_images: bool) {
        self.has_draft_images = has_images;
    }

    #[cfg(test)]
    const fn scope(&self) -> RecentPromptScope {
        self.scope
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<RecentPromptPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        self.last_click = None;
        match key.code {
            KeyCode::Esc => Self::effect(RecentPromptPickerEffect::Dismiss),
            KeyCode::Backspace if !self.query.is_empty() => {
                if let Some((index, _)) = self.query.grapheme_indices(true).next_back() {
                    self.query.truncate(index);
                    self.refresh_visible();
                }
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Backspace => Self::effect(RecentPromptPickerEffect::Dismiss),
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                self.preview_scroll = 0;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down => {
                if !self.visible.is_empty() {
                    self.selected = (self.selected + 1).min(self.visible.len() - 1);
                }
                self.preview_scroll = 0;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageUp => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::PageDown => {
                self.preview_scroll = self
                    .preview_scroll
                    .saturating_add(1)
                    .min(self.max_preview_scroll);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter | KeyCode::Tab => self.select(),
            KeyCode::Char('f') if key.modifiers == KeyModifiers::CONTROL => self.toggle_scope(),
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.query.push(character);
                self.refresh_visible();
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            _ => ComponentUpdate::none(),
        }
    }

    fn insert_paste(&mut self, text: &str) -> ComponentUpdate<RecentPromptPickerEffect> {
        self.query
            .extend(text.chars().filter(|character| !character.is_control()));
        self.refresh_visible();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn toggle_scope(&mut self) -> ComponentUpdate<RecentPromptPickerEffect> {
        self.scope = match self.scope {
            RecentPromptScope::Global => RecentPromptScope::CurrentSession,
            RecentPromptScope::CurrentSession => RecentPromptScope::Global,
        };
        self.refresh_visible();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn refresh_visible(&mut self) {
        let query = self.query.to_ascii_lowercase();
        let mut visible = self
            .prompts
            .iter()
            .enumerate()
            .filter_map(|(index, prompt)| {
                if self.scope == RecentPromptScope::CurrentSession
                    && prompt.session_id != self.current_session_id
                {
                    return None;
                }
                fuzzy_score(&prompt.text, &query).map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        visible.sort_by_key(|(index, score)| (Reverse(*score), *index));
        self.visible = visible.into_iter().map(|(index, _)| index).collect();
        self.selected = 0;
        self.preview_scroll = 0;
        self.max_preview_scroll = 0;
        self.offset = 0;
        self.last_click = None;
        self.list_area = Rect::default();
    }

    fn select(&self) -> ComponentUpdate<RecentPromptPickerEffect> {
        let Some(prompt) = self.selected_prompt() else {
            return ComponentUpdate::none();
        };
        Self::effect(RecentPromptPickerEffect::Insert(prompt.text.clone()))
    }

    fn selected_prompt(&self) -> Option<&RecentPrompt> {
        let index = self.visible.get(self.selected)?;
        self.prompts.get(*index)
    }

    fn effect(effect: RecentPromptPickerEffect) -> ComponentUpdate<RecentPromptPickerEffect> {
        ComponentUpdate {
            effects: vec![effect],
            render: RenderRequest::Immediate,
        }
    }

    fn render_scope(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let scope = match self.scope {
            RecentPromptScope::Global => "All sessions",
            RecentPromptScope::CurrentSession => "Current session",
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw("  "),
                Span::styled("Scope: ", Style::default().fg(theme.muted())),
                Span::styled(
                    scope,
                    Style::default()
                        .fg(theme.text())
                        .add_modifier(Modifier::BOLD),
                ),
            ])),
            area,
        );
    }

    fn render_search(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let marker = "  ";
        let prefix_width = marker.width() + SEARCH_LABEL.width();
        let query_width = usize::from(area.width).saturating_sub(prefix_width);
        let query = visible_query_tail(&self.query, query_width);
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
    ) -> ComponentUpdate<RecentPromptPickerEffect> {
        let point = Position::new(mouse.column, mouse.row);
        if self.preview_area.contains(point) {
            self.last_click = None;
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.preview_scroll = self.preview_scroll.saturating_sub(1)
                }
                MouseEventKind::ScrollDown => {
                    self.preview_scroll = self
                        .preview_scroll
                        .saturating_add(1)
                        .min(self.max_preview_scroll)
                }
                _ => return ComponentUpdate::none(),
            }
        } else if self.list_area.contains(point) {
            match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.last_click = None;
                    self.selected = self.selected.saturating_sub(1);
                }
                MouseEventKind::ScrollDown => {
                    self.last_click = None;
                    self.selected = (self.selected + 1).min(self.visible.len().saturating_sub(1));
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    let index = self.offset + usize::from(mouse.row - self.list_area.y);
                    if index >= self.visible.len() {
                        return ComponentUpdate::none();
                    }
                    let confirm = self.last_click.is_some_and(|(previous, at)| {
                        previous == index
                            && now.saturating_duration_since(at) <= Duration::from_millis(500)
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
            self.preview_scroll = 0;
        } else {
            self.last_click = None;
            return ComponentUpdate::none();
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_prompts(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = area;
        if area.is_empty() {
            return;
        }
        if self.visible.is_empty() {
            let message = if self.query.is_empty() {
                "  No prompts in this scope"
            } else {
                "  No matching prompts"
            };
            frame.render_widget(
                Paragraph::new(message).style(Style::default().fg(theme.muted())),
                area,
            );
            return;
        }
        let capacity = usize::from(area.height);
        self.offset = self
            .offset
            .min(self.visible.len().saturating_sub(capacity))
            .min(self.selected);
        if self.selected >= self.offset + capacity {
            self.offset = self.selected + 1 - capacity;
        }
        let width = usize::from(area.width).saturating_sub(4);
        for (row, index) in self
            .visible
            .iter()
            .skip(self.offset)
            .take(capacity)
            .enumerate()
        {
            let position = row + self.offset;
            let selected = position == self.selected;
            let age = if area.width >= 58 {
                self.ages[*index].as_str()
            } else {
                ""
            };
            let label = truncate_display(
                &format!("{}. {}", position + 1, self.labels[*index]),
                width.saturating_sub(age.width() + usize::from(!age.is_empty()) * 2),
            );
            let padding = width.saturating_sub(label.width() + age.width());
            let style = Style::default().fg(if selected {
                theme.accent()
            } else {
                theme.text()
            });
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(if selected { "› " } else { "  " }, style),
                    Span::styled(label, style),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(age, Style::default().fg(theme.muted())),
                ])),
                Rect::new(area.x, area.y + row as u16, area.width, 1),
            );
        }
    }

    fn render_preview(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.preview_area = area;
        if area.is_empty() {
            self.max_preview_scroll = 0;
            return;
        }
        frame.render_widget(
            Block::new()
                .borders(Borders::TOP)
                .title(" Preview ")
                .border_style(Style::default().fg(theme.border()))
                .title_style(Style::default().fg(theme.muted())),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let Some(index) = self.visible.get(self.selected).copied() else {
            self.max_preview_scroll = 0;
            return;
        };
        let width = area.width.saturating_sub(4);
        if self
            .preview
            .as_ref()
            .is_none_or(|preview| preview.index != index || preview.width != width)
        {
            let prompt = &self.prompts[index];
            let mut lines = wrap_display_lines(&prompt.text, usize::from(width));
            lines.push(String::new());
            let source_start = lines.len();
            lines.extend(wrap_display_lines(
                &format!(
                    "Source\nWorkspace: {}\nSession: {}\nSent: {} ago",
                    sanitize_terminal_text_inline(&prompt.workspace.to_string_lossy()),
                    sanitize_terminal_text_inline(&prompt.session_id),
                    self.ages[index]
                ),
                usize::from(width),
            ));
            self.preview = Some(Preview {
                index,
                width,
                lines,
                source_start,
            });
        }
        if area.height < 2 {
            return;
        }
        let prompt = &self.prompts[index];
        let from = if prompt.session_id == self.current_session_id {
            "current session".to_owned()
        } else {
            prompt.workspace.to_string_lossy().into_owned()
        };
        frame.render_widget(
            Paragraph::new(truncate_display(
                &format!("From: {from}"),
                usize::from(width),
            ))
            .style(Style::default().fg(theme.muted())),
            Rect::new(area.x + 2.min(area.width), area.y + 1, width, 1),
        );
        let preview = self.preview.as_ref().expect("selected preview is prepared");
        let capacity = usize::from(area.height.saturating_sub(2));
        self.max_preview_scroll = preview
            .lines
            .len()
            .saturating_sub(capacity)
            .min(usize::from(u16::MAX)) as u16;
        self.preview_scroll = self.preview_scroll.min(self.max_preview_scroll);
        for (row, line) in preview
            .lines
            .iter()
            .enumerate()
            .skip(usize::from(self.preview_scroll))
            .take(capacity)
        {
            let style = Style::default().fg(if row < preview.source_start {
                theme.text()
            } else {
                theme.muted()
            });
            frame.render_widget(
                Paragraph::new(line.as_str()).style(style),
                Rect::new(
                    area.x + 2.min(area.width),
                    area.y + 2 + (row - usize::from(self.preview_scroll)) as u16,
                    width,
                    1,
                ),
            );
        }
    }
}

impl Component for RecentPromptPicker {
    type Event = RecentPromptPickerEvent;
    type Effect = RecentPromptPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            RecentPromptPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            RecentPromptPickerEvent::Terminal(Event::Mouse(mouse)) => {
                self.update_mouse(mouse, Instant::now())
            }
            RecentPromptPickerEvent::Terminal(Event::Paste(text)) => self.insert_paste(&text),
            RecentPromptPickerEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
        self.preview_area = Rect::default();
        if area.is_empty() {
            return;
        }

        let layout =
            Floating::new("Recent prompts", 82, 22, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }

        let body = layout.body;
        let row = |offset| Rect::new(body.x, body.y + offset, body.width, 1).intersection(body);
        self.render_search(frame, row(0), theme);
        self.render_scope(frame, row(1), theme);
        let heading = if body.width >= 58 {
            format!(
                "  Prompt{}Sent",
                " ".repeat(usize::from(body.width).saturating_sub(14))
            )
        } else {
            "  Prompt".to_owned()
        };
        frame.render_widget(
            Paragraph::new(heading).style(Style::default().fg(theme.muted())),
            row(2),
        );
        let remaining = body.height.saturating_sub(4);
        let list_height = if remaining > 6 {
            LIST_HEIGHT.min(remaining - 6)
        } else {
            remaining / 3
        };
        let list_area = Rect::new(body.x, body.y + 3, body.width, list_height).intersection(body);
        let preview_area = Rect::new(
            body.x,
            list_area.bottom(),
            body.width,
            remaining.saturating_sub(list_height),
        )
        .intersection(body);
        self.render_prompts(frame, list_area, theme);
        self.render_preview(frame, preview_area, theme);
        if body.height > 3 {
            frame.render_widget(
                Paragraph::new(if self.has_draft_images {
                    "  Replaces draft + images."
                } else {
                    "  Use replaces your draft."
                })
                .style(Style::default().fg(theme.muted())),
                row(body.height - 1),
            );
        }
    }
}

fn one_line_preview(text: &str) -> String {
    sanitize_terminal_text_inline(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        Component, RecentPromptPicker, RecentPromptPickerEffect, RecentPromptPickerEvent,
        RecentPromptScope,
    };
    use crate::{sessions::checkpoint::RecentPrompt, tui::theme::Theme};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    fn prompt(text: &str, session_id: &str, workspace: &str) -> RecentPrompt {
        RecentPrompt {
            text: text.to_owned(),
            recorded_at_unix_ms: 1,
            session_id: session_id.to_owned(),
            workspace: PathBuf::from(workspace),
        }
    }

    fn picker() -> RecentPromptPicker {
        RecentPromptPicker::new(
            vec![
                prompt("newest", "other", "/work/other"),
                prompt("  exact\n\n    prompt  ", "current", "/work/current"),
                prompt("oldest", "current", "/work/current"),
            ],
            "current".to_owned(),
        )
    }

    fn key(code: KeyCode) -> RecentPromptPickerEvent {
        key_with_modifiers(code, KeyModifiers::NONE)
    }

    fn key_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> RecentPromptPickerEvent {
        RecentPromptPickerEvent::Terminal(Event::Key(KeyEvent::new(code, modifiers)))
    }

    #[test]
    fn defaults_to_global_and_preserves_loader_order() {
        let mut picker = picker();

        assert_eq!(picker.scope(), RecentPromptScope::Global);
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [RecentPromptPickerEffect::Insert("newest".to_owned())]
        );
        picker.update(key(KeyCode::Down));
        assert_eq!(
            picker.update(key(KeyCode::Tab)).effects,
            [RecentPromptPickerEffect::Insert(
                "  exact\n\n    prompt  ".to_owned()
            )]
        );
    }

    #[test]
    fn typing_fuzzy_searches_prompt_text_before_selection() {
        let mut picker = picker();

        for character in "XcP".chars() {
            picker.update(key(KeyCode::Char(character)));
        }

        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [RecentPromptPickerEffect::Insert(
                "  exact\n\n    prompt  ".to_owned()
            )]
        );
    }

    #[test]
    fn control_f_toggles_current_session_filter() {
        let mut picker = picker();

        picker.update(key(KeyCode::Char('f')));
        assert_eq!(picker.scope(), RecentPromptScope::Global);
        assert!(picker.visible.is_empty());
        picker.update(key(KeyCode::Backspace));

        picker.update(key_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(picker.scope(), RecentPromptScope::CurrentSession);
        assert_eq!(picker.visible, [1, 2]);
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [RecentPromptPickerEffect::Insert(
                "  exact\n\n    prompt  ".to_owned()
            )]
        );

        picker.update(key_with_modifiers(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        ));
        assert_eq!(picker.scope(), RecentPromptScope::Global);
        assert_eq!(picker.visible, [0, 1, 2]);
    }

    #[test]
    fn paste_and_backspace_edit_the_search_query() {
        let mut picker = picker();

        picker.update(RecentPromptPickerEvent::Terminal(Event::Paste(
            "x\ncp".to_owned(),
        )));
        assert_eq!(picker.query, "xcp");
        assert_eq!(picker.visible, [1]);

        for expected in ["xc", "x", ""] {
            assert!(picker.update(key(KeyCode::Backspace)).effects.is_empty());
            assert_eq!(picker.query, expected);
        }
        assert_eq!(picker.visible, [0, 1, 2]);
        assert_eq!(
            picker.update(key(KeyCode::Backspace)).effects,
            [RecentPromptPickerEffect::Dismiss]
        );
    }

    #[test]
    fn navigation_clamps_and_escape_dismisses() {
        let mut picker = picker();

        picker.update(key(KeyCode::Up));
        assert_eq!(picker.selected, 0);
        for _ in 0..5 {
            picker.update(key(KeyCode::Down));
        }
        assert_eq!(picker.selected, 2);
        assert_eq!(
            picker.update(key(KeyCode::Esc)).effects,
            [RecentPromptPickerEffect::Dismiss]
        );
    }

    #[test]
    fn long_previews_can_be_scrolled_and_reset_for_the_next_prompt() {
        let mut picker = picker();

        picker.prompts[0].text = "line\n".repeat(80);
        let mut terminal = Terminal::new(TestBackend::new(82, 22)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        for _ in 0..12 {
            picker.update(key(KeyCode::PageDown));
        }
        assert_eq!(picker.preview_scroll, 12);

        picker.update(key(KeyCode::PageUp));
        assert_eq!(picker.preview_scroll, 11);

        picker.update(key(KeyCode::Down));
        assert_eq!(picker.preview_scroll, 0);
    }

    #[test]
    fn preview_preserves_leading_spaces_blank_lines_and_trailing_spaces() {
        let mut picker = RecentPromptPicker::new(
            vec![prompt(
                "first\n  indented\n\nlast  ",
                "current",
                "/work/current",
            )],
            "current".to_owned(),
        );
        let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();

        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let preview_border = (0..buffer.area.height)
            .find(|&row| {
                (0..buffer.area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .contains("Preview")
            })
            .expect("preview border");
        let first_row = preview_border + 2;
        assert_eq!(buffer[(7, first_row)].symbol(), "f");
        assert_eq!(buffer[(9, first_row + 1)].symbol(), "i");
        assert_eq!(buffer[(7, first_row + 2)].symbol(), " ");
        assert_eq!(buffer[(7, first_row + 3)].symbol(), "l");
        assert_eq!(buffer[(11, first_row + 3)].symbol(), " ");
        assert_eq!(buffer[(12, first_row + 3)].symbol(), " ");
    }

    #[test]
    fn render_includes_numbered_global_metadata() {
        let mut picker = picker();
        let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();

        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let rows = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        let search_row = rows
            .iter()
            .position(|row| row.contains("Search:"))
            .expect("search row");
        let scope_row = rows
            .iter()
            .position(|row| row.contains("Scope:"))
            .expect("scope row");
        assert_eq!(scope_row, search_row + 1);
        assert!(rows[scope_row].contains("All sessions"));
        assert_eq!(
            rows[search_row].find("Search:"),
            rows[scope_row].find("Scope:")
        );
        assert!(rows.iter().any(|row| row.contains("From: /work/other")));
        let row = rows
            .into_iter()
            .find(|row| row.contains("1. newest"))
            .expect("numbered prompt row");
        assert!(!row.contains("/work/other"));
    }

    #[test]
    fn renders_on_a_narrow_terminal() {
        let mut picker = picker();
        let mut terminal = Terminal::new(TestBackend::new(3, 3)).unwrap();

        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert_eq!(terminal.backend().buffer().area.width, 3);
    }

    #[test]
    fn short_terminal_preserves_the_help_footer_and_bottom_border() {
        let mut picker = picker();
        let mut terminal = Terminal::new(TestBackend::new(100, 5)).unwrap();

        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let footer = (2..=3)
            .flat_map(|row| {
                (0..buffer.area.width).map(move |column| buffer[(column, row)].symbol())
            })
            .collect::<String>();
        assert!(footer.contains("enter/tab use"));
        assert_eq!(buffer[(9, 4)].symbol(), "╰");
        assert_eq!(buffer[(90, 4)].symbol(), "╯");
    }
    #[test]
    fn regression_preview_scroll_stops_before_an_empty_page() {
        let mut picker = picker();
        let mut terminal = Terminal::new(TestBackend::new(82, 22)).unwrap();
        for _ in 0..200 {
            picker.update(key(KeyCode::PageDown));
            terminal
                .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
                .unwrap();
        }
        assert!(
            picker.preview_scroll < 100,
            "scroll escaped the wrapped preview"
        );
    }
    #[test]
    fn full_prompt_precedes_full_source_and_image_warning_does_not_change_selection() {
        let raw = "  first\n\nlast  \x1b";
        let mut picker = RecentPromptPicker::new(
            vec![prompt(raw, "opaque-full-session-id", "/other/workspace")],
            "current".into(),
        );
        picker.set_has_draft_images(true);
        let mut terminal = Terminal::new(TestBackend::new(82, 22)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("Replaces draft + images."));
        let preview = picker.preview.as_ref().unwrap();
        assert_eq!(preview.lines[0], "  first");
        assert_eq!(preview.lines[1], "");
        assert!(preview.lines[2].starts_with("last  "));
        assert!(
            preview.lines[preview.source_start..]
                .iter()
                .any(|line| line.contains("opaque-full-session-id"))
        );
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [RecentPromptPickerEffect::Insert(raw.into())]
        );
    }

    #[test]
    fn mouse_regions_select_and_scroll_without_sending_or_resuming() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let now = std::time::Instant::now();
        let mut picker = picker();
        let mut terminal = Terminal::new(TestBackend::new(32, 16)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let list = picker.list_area;
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x,
            row: list.y,
            modifiers: KeyModifiers::NONE,
        };
        assert!(picker.update_mouse(mouse, now).effects.is_empty());
        assert_eq!(
            picker
                .update_mouse(mouse, now + std::time::Duration::from_millis(100))
                .effects,
            [RecentPromptPickerEffect::Insert("newest".into())]
        );
        let before = picker.selected;
        let preview = picker.preview_area;
        picker.update_mouse(
            MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: preview.x,
                row: preview.y,
                modifiers: KeyModifiers::NONE,
            },
            now,
        );
        assert_eq!(picker.selected, before);
        assert!(picker.preview_scroll <= picker.max_preview_scroll);
        for width in 0..20 {
            for height in 0..16 {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| picker.render(frame, frame.area(), &Theme::default()))
                    .unwrap();
            }
        }
    }
}
