//! Selector for automatic, light, and dark color modes.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::{Theme, ThemeMode};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState},
};

const KEY_BINDINGS: [(&str, &str); 3] = [("↑↓", "change"), ("enter", "apply"), ("esc", "cancel")];

pub(super) enum ThemeSelectorEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ThemeSelectorEffect {
    Apply(ThemeMode),
    Dismiss,
}

pub(super) struct ThemeSelector {
    selected: usize,
    navigation_area: Rect,
}

impl ThemeSelector {
    pub(super) fn new(initial: ThemeMode) -> Self {
        let selected = ThemeMode::ALL
            .iter()
            .position(|mode| *mode == initial)
            .expect("all theme modes are selectable");
        Self {
            selected,
            navigation_area: Rect::default(),
        }
    }

    fn select_bounded(&mut self, delta: isize) -> ComponentUpdate<ThemeSelectorEffect> {
        let next = self
            .selected
            .saturating_add_signed(delta)
            .min(ThemeMode::ALL.len().saturating_sub(1));
        if next == self.selected {
            return ComponentUpdate::none();
        }
        self.selected = next;
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<ThemeSelectorEffect> {
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
            KeyCode::Up | KeyCode::Left => {
                self.selected = self.selected.saturating_sub(1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Down | KeyCode::Right => {
                self.selected = (self.selected + 1).min(ThemeMode::ALL.len() - 1);
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            KeyCode::Enter => ComponentUpdate {
                effects: vec![ThemeSelectorEffect::Apply(ThemeMode::ALL[self.selected])],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Backspace => ComponentUpdate {
                effects: vec![ThemeSelectorEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }
}

impl Component for ThemeSelector {
    type Event = ThemeSelectorEvent;
    type Effect = ThemeSelectorEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ThemeSelectorEvent::Terminal(Event::Key(key)) => self.update_key(key),
            ThemeSelectorEvent::Terminal(Event::Mouse(mouse))
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
            ThemeSelectorEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.navigation_area = Rect::default();
        if area.is_empty() {
            return;
        }
        let layout = Floating::new("Theme", 38, 7, &KEY_BINDINGS).render(frame, area, theme);
        self.navigation_area = layout.body;
        let items = ThemeMode::ALL.into_iter().map(|mode| {
            let detail = match mode {
                ThemeMode::Auto => "Follow the operating system",
                ThemeMode::Light => "Always use the light palette",
                ThemeMode::Dark => "Always use the dark palette",
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{:<6}", mode.as_str()),
                    Style::default().fg(theme.text()),
                ),
                Span::styled(detail, Style::default().fg(theme.muted())),
            ]))
        });
        let list = List::new(items).highlight_symbol("› ").highlight_style(
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        );
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(list, layout.body, &mut state);
        let cursor_y = layout.body.y + u16::try_from(self.selected).unwrap_or(u16::MAX);
        frame.set_cursor_position(Position::new(
            layout.body.x,
            cursor_y.min(layout.body.bottom().saturating_sub(1)),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, ThemeSelector, ThemeSelectorEffect, ThemeSelectorEvent};
    use crate::tui::theme::ThemeMode;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> ThemeSelectorEvent {
        ThemeSelectorEvent::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
    }

    #[test]
    fn selects_each_theme_mode() {
        let mut selector = ThemeSelector::new(ThemeMode::Auto);

        assert_eq!(
            selector.update(key(KeyCode::Enter)).effects,
            [ThemeSelectorEffect::Apply(ThemeMode::Auto)]
        );
        selector.update(key(KeyCode::Down));
        assert_eq!(
            selector.update(key(KeyCode::Enter)).effects,
            [ThemeSelectorEffect::Apply(ThemeMode::Light)]
        );
        selector.update(key(KeyCode::Down));
        assert_eq!(
            selector.update(key(KeyCode::Enter)).effects,
            [ThemeSelectorEffect::Apply(ThemeMode::Dark)]
        );
    }
    #[test]
    fn rendered_picker_bounds_wheel_and_page_navigation() {
        let mut picker = ThemeSelector::new(ThemeMode::Auto);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 12)).unwrap();
        terminal
            .draw(|frame| picker.render(frame, frame.area(), &crate::tui::theme::Theme::default()))
            .unwrap();
        let body = picker.navigation_area;
        assert!(!body.is_empty());
        let mouse = |kind, column, row| {
            ThemeSelectorEvent::Terminal(Event::Mouse(crossterm::event::MouseEvent {
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
        picker.update(ThemeSelectorEvent::Terminal(Event::Key(KeyEvent::new(
            KeyCode::PageDown,
            KeyModifiers::NONE,
        ))));
        assert_eq!(
            picker.selected,
            crate::tui::theme::ThemeMode::ALL
                .len()
                .saturating_sub(1)
                .min(usize::from(body.height).max(1))
        );
        for _ in 0..40 {
            picker.update(ThemeSelectorEvent::Terminal(Event::Key(KeyEvent::new(
                KeyCode::PageDown,
                KeyModifiers::NONE,
            ))));
        }
        let last = crate::tui::theme::ThemeMode::ALL.len().saturating_sub(1);
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
            text.contains("› ") && text.contains("dark")
        }));
        for _ in 0..40 {
            picker.update(ThemeSelectorEvent::Terminal(Event::Key(KeyEvent::new(
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
