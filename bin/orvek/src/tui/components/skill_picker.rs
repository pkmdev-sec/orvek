//! Searchable picker for skills available to the active session.

use super::{
    file_finder::{fuzzy_score, visible_query_tail},
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{
    core::extensions::Skill,
    tui::{
        format::{sanitize_terminal_text_inline, truncate_display, wrap_display_lines},
        theme::Theme,
    },
};
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::{
    cmp::Reverse,
    sync::Arc,
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "move"), ("enter/tab", "insert"), ("esc", "close")];
const SEARCH_LABEL: &str = "Search: ";
const FOCUS_MARKER: &str = "› ";

pub(super) enum SkillPickerEvent {
    Terminal(Event),
    Query(String),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum SkillPickerEffect {
    Dismiss,
    Insert(String),
}

pub(super) struct SkillPicker {
    skills: Arc<[Skill]>,
    query: String,
    selected: usize,
    matches: Vec<usize>,
    list_area: Rect,
    offset: usize,
    last_click: Option<(usize, Instant)>,
}

impl SkillPicker {
    pub(super) fn new(skills: Arc<[Skill]>) -> Self {
        let matches = (0..skills.len()).collect();
        Self {
            skills,
            query: String::new(),
            selected: 0,
            matches,
            list_area: Rect::default(),
            offset: 0,
            last_click: None,
        }
    }

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<SkillPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        self.last_click = None;
        match key.code {
            KeyCode::Esc => Self::dismiss(),
            KeyCode::Enter | KeyCode::Tab => self.handle_enter(),
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
            _ => ComponentUpdate::none(),
        }
    }

    fn set_query(&mut self, query: String) -> ComponentUpdate<SkillPickerEffect> {
        self.query = query;
        let query = self.query.to_ascii_lowercase();
        let mut matches = self
            .skills
            .iter()
            .enumerate()
            .filter_map(|(index, skill)| {
                fuzzy_score(skill.name(), &query).map(|score| (index, score))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(index, score)| (Reverse(*score), self.skills[*index].name()));
        self.matches = matches.into_iter().map(|(index, _)| index).collect();
        self.selected = 0;
        self.offset = 0;
        self.last_click = None;
        self.list_area = Rect::default();
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn dismiss() -> ComponentUpdate<SkillPickerEffect> {
        ComponentUpdate {
            effects: vec![SkillPickerEffect::Dismiss],
            render: RenderRequest::Immediate,
        }
    }

    fn handle_enter(&self) -> ComponentUpdate<SkillPickerEffect> {
        let Some(index) = self.matches.get(self.selected) else {
            return ComponentUpdate::none();
        };
        ComponentUpdate {
            effects: vec![SkillPickerEffect::Insert(
                self.skills[*index].name().to_owned(),
            )],
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
        let label_style = Style::default().fg(theme.muted());
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(marker, label_style),
                Span::styled(SEARCH_LABEL, label_style),
                Span::styled(
                    visible_query_tail(&self.query, query_width),
                    Style::default().fg(theme.text()),
                ),
            ])),
            area,
        );
    }

    fn update_mouse(
        &mut self,
        mouse: MouseEvent,
        now: Instant,
    ) -> ComponentUpdate<SkillPickerEffect> {
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
                let index = self.offset + usize::from(mouse.row - self.list_area.y);
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
                    return self.handle_enter();
                }
            }
            _ => return ComponentUpdate::none(),
        }
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render_skills(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme, wide: bool) {
        self.list_area = area;
        if area.is_empty() {
            return;
        }
        if self.matches.is_empty() {
            let message = if self.skills.is_empty() {
                "  No skills available"
            } else {
                "  No matching skills"
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
            let skill = &self.skills[*index];
            let selected = self.offset + row == self.selected;
            let name =
                truncate_display(&format!("${}", skill.name()), if wide { 22 } else { width });
            let padding = if wide {
                24usize.saturating_sub(name.width())
            } else {
                0
            };
            let description = if wide {
                truncate_display(skill.description(), width.saturating_sub(24))
            } else {
                String::new()
            };
            let style = Style::default()
                .fg(if selected {
                    theme.accent()
                } else {
                    theme.text()
                })
                .add_modifier(Modifier::BOLD);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(if selected { FOCUS_MARKER } else { "  " }, style),
                    Span::styled(name, style),
                    Span::raw(" ".repeat(padding)),
                    Span::styled(description, Style::default().fg(theme.muted())),
                ])),
                Rect::new(area.x, area.y + row as u16, area.width, 1),
            );
        }
    }

    fn render_details(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let Some(index) = self.matches.get(self.selected) else {
            return;
        };
        if area.is_empty() {
            return;
        }
        let skill = &self.skills[*index];
        let width = usize::from(area.width).saturating_sub(4);
        let description = format!(
            "About: {}",
            sanitize_terminal_text_inline(skill.description())
        );
        let mut lines = wrap_display_lines(&description, width);
        if lines.len() > 2 {
            let tail = format!("{}…", lines[1].trim_end());
            lines[1] = truncate_display(&tail, width);
        }
        let description_rows = area.height.saturating_sub(1).min(2);
        for (row, line) in lines.iter().take(usize::from(description_rows)).enumerate() {
            frame.render_widget(
                Paragraph::new(line.as_str()).style(Style::default().fg(theme.muted())),
                Rect::new(
                    area.x + 2.min(area.width),
                    area.y + row as u16,
                    width as u16,
                    1,
                ),
            );
        }
        frame.render_widget(
            Paragraph::new(truncate_display(
                &format!("Insert: ${}", skill.name()),
                width,
            ))
            .style(Style::default().fg(theme.muted())),
            Rect::new(
                area.x + 2.min(area.width),
                area.bottom() - 1,
                width as u16,
                1,
            ),
        );
    }
}

impl Component for SkillPicker {
    type Event = SkillPickerEvent;
    type Effect = SkillPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            SkillPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            SkillPickerEvent::Terminal(Event::Mouse(mouse)) => {
                self.update_mouse(mouse, Instant::now())
            }
            SkillPickerEvent::Terminal(_) => ComponentUpdate::none(),
            SkillPickerEvent::Query(query) => self.set_query(query),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.list_area = Rect::default();
        if area.is_empty() {
            return;
        }

        let layout = Floating::new("Skills", 72, 14, &KEY_BINDINGS).render(frame, area, theme);
        if layout.body.is_empty() {
            return;
        }
        let search_area = Rect {
            height: 1,
            ..layout.body
        };
        let detail_height = 3.min(layout.body.height.saturating_sub(2));
        let skills_area = Rect::new(
            layout.body.x,
            layout.body.y + 1,
            layout.body.width,
            layout.body.height.saturating_sub(1 + detail_height),
        );
        let details_area = Rect::new(
            layout.body.x,
            skills_area.bottom(),
            layout.body.width,
            detail_height,
        );
        self.render_search(frame, search_area, theme);
        self.render_skills(frame, skills_area, theme, area.width >= 54);
        self.render_details(frame, details_area, theme);
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, SkillPicker, SkillPickerEffect, SkillPickerEvent};
    use crate::core::extensions::Skill;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> SkillPickerEvent {
        SkillPickerEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    fn picker() -> SkillPicker {
        SkillPicker::new(
            vec![
                Skill::new("autofix", "Repair a pull request."),
                Skill::new("open-docs", "Open documentation."),
            ]
            .into(),
        )
    }

    #[test]
    fn query_filters_skills_and_enter_inserts_the_match() {
        let mut picker = picker();
        picker.update(SkillPickerEvent::Query("fix".to_owned()));

        let update = picker.update(key(KeyCode::Enter));

        assert_eq!(
            update.effects.as_slice(),
            [SkillPickerEffect::Insert("autofix".to_owned())]
        );
    }

    #[test]
    fn down_selects_the_next_skill() {
        let mut picker = picker();
        picker.update(key(KeyCode::Down));

        let update = picker.update(key(KeyCode::Tab));

        assert_eq!(
            update.effects.as_slice(),
            [SkillPickerEffect::Insert("open-docs".to_owned())]
        );
    }
    fn render(
        picker: &mut SkillPicker,
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
    fn description_is_display_only_and_query_keeps_the_shared_name_ranking() {
        let mut picker = SkillPicker::new(
            vec![
                Skill::new("z-last", "onlydescription"),
                Skill::new("a-first", "Other"),
            ]
            .into(),
        );
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SkillPickerEffect::Insert("z-last".into())]
        );
        picker.update(SkillPickerEvent::Query(String::new()));
        assert_eq!(
            picker.update(key(KeyCode::Enter)).effects,
            [SkillPickerEffect::Insert("a-first".into())]
        );
        picker.update(SkillPickerEvent::Query("onlydescription".into()));
        assert!(picker.update(key(KeyCode::Enter)).effects.is_empty());
        let terminal = render(&mut picker, 72, 14);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("No matching skills"));
    }

    #[test]
    fn fixed_details_do_not_move_the_list_and_mouse_returns_the_original_name() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let now = std::time::Instant::now();
        let name = "日本語-skill-with-a-very-long-original-name";
        let mut picker = SkillPicker::new(
            vec![
                Skill::new("short", "Brief"),
                Skill::new(name, "Long description ".repeat(20)),
            ]
            .into(),
        );
        render(&mut picker, 32, 14);
        let list = picker.list_area;
        let mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: list.x,
            row: list.y + 1,
            modifiers: KeyModifiers::NONE,
        };
        assert!(picker.update_mouse(mouse, now).effects.is_empty());
        let terminal = render(&mut picker, 32, 14);
        assert_eq!(picker.list_area, list);
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("About: Long description"));
        assert!(text.contains("Insert: $日"));
        assert!(text.contains('…'));
        assert_eq!(
            picker
                .update_mouse(mouse, now + std::time::Duration::from_millis(100))
                .effects,
            [SkillPickerEffect::Insert(name.into())]
        );
        assert_eq!(
            picker.update(key(KeyCode::Tab)).effects,
            [SkillPickerEffect::Insert(name.into())]
        );
    }

    #[test]
    fn display_sanitizes_catalog_controls_and_tiny_areas_are_safe() {
        let mut picker =
            SkillPicker::new(vec![Skill::new("control-skill", "first\x1bsecond\nthird")].into());
        let terminal = render(&mut picker, 72, 14);
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .all(|cell| !cell.symbol().contains(char::is_control))
        );
        for width in 0..18 {
            for height in 0..14 {
                render(&mut picker, width, height);
            }
        }
    }
    #[test]
    fn wheel_and_arrow_navigation_have_the_same_clamped_selection() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut wheel = picker();
        let mut arrows = picker();
        render(&mut wheel, 72, 14);
        for (kind, code) in [
            (MouseEventKind::ScrollDown, KeyCode::Down),
            (MouseEventKind::ScrollDown, KeyCode::Down),
            (MouseEventKind::ScrollUp, KeyCode::Up),
            (MouseEventKind::ScrollUp, KeyCode::Up),
        ] {
            wheel.update(SkillPickerEvent::Terminal(Event::Mouse(MouseEvent {
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
