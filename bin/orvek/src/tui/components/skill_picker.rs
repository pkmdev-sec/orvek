//! Searchable picker for skills available to the active session.

use super::{
    file_finder::{fuzzy_score, visible_query_tail},
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::{core::extensions::Skill, tui::theme::Theme};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use std::{cmp::Reverse, sync::Arc};
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
    navigation_area: Rect,
    matches: Vec<usize>,
}

impl SkillPicker {
    pub(super) fn new(skills: Arc<[Skill]>) -> Self {
        let matches = (0..skills.len()).collect();
        Self {
            skills,
            query: String::new(),
            selected: 0,
            navigation_area: Rect::default(),
            matches,
        }
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<SkillPickerEffect> {
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

    fn update_key(&mut self, key: KeyEvent) -> ComponentUpdate<SkillPickerEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }

        match key.code {
            KeyCode::PageUp => self.select_bounded(
                -(isize::try_from(self.navigation_area.height)
                    .unwrap_or(1)
                    .max(1)),
            ),
            KeyCode::PageDown => self.select_bounded(
                isize::try_from(self.navigation_area.height)
                    .unwrap_or(1)
                    .max(1),
            ),
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

    fn render_skills(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.is_empty() {
            return;
        }

        let items = self.matches.iter().map(|index| {
            let skill = &self.skills[*index];
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("${}", skill.name()),
                    Style::default()
                        .fg(theme.text())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {}", skill.description()),
                    Style::default().fg(theme.muted()),
                ),
            ]))
        });
        let list = List::new(items)
            .highlight_style(Style::default().fg(theme.accent()))
            .highlight_symbol(FOCUS_MARKER);
        let selected = (!self.matches.is_empty()).then_some(self.selected);
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, area, &mut state);
    }
}

impl Component for SkillPicker {
    type Event = SkillPickerEvent;
    type Effect = SkillPickerEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            SkillPickerEvent::Terminal(Event::Key(key)) => self.update_key(key),
            SkillPickerEvent::Terminal(Event::Mouse(mouse))
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
            SkillPickerEvent::Terminal(_) => ComponentUpdate::none(),
            SkillPickerEvent::Query(query) => self.set_query(query),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.navigation_area = Rect::default();
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
        let skills_area = Rect {
            y: layout.body.y + 1,
            height: layout.body.height.saturating_sub(1),
            ..layout.body
        };
        self.navigation_area = skills_area;
        self.render_search(frame, search_area, theme);
        self.render_skills(frame, skills_area, theme);
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
    #[test]
    fn rendered_picker_bounds_wheel_and_page_navigation() {
        let mut picker = SkillPicker::new(
            (0..30)
                .map(|index| Skill::new(format!("skill-{index:02}"), "description"))
                .collect::<Vec<_>>()
                .into(),
        );
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        let body = picker.navigation_area;
        assert!(!body.is_empty());
        let mouse = |kind, column, row| {
            SkillPickerEvent::Terminal(Event::Mouse(crossterm::event::MouseEvent {
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
        picker.update(SkillPickerEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.selected,
            picker
                .matches
                .len()
                .saturating_sub(1)
                .min(usize::from(body.height).max(1))
        );
        for _ in 0..40 {
            picker.update(SkillPickerEvent::Terminal(Event::Key(KeyEvent::new(
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
            text.contains("› ") && text.contains("$skill-29")
        }));
        for _ in 0..40 {
            picker.update(SkillPickerEvent::Terminal(Event::Key(KeyEvent::new(
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
