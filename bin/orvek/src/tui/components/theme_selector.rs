//! Selector for automatic, light, and dark color modes.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    format::truncate_display,
    theme::{Theme, ThemeMode},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
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
    current: ThemeMode,
    targets: [Rect; 3],
}

impl ThemeSelector {
    pub(super) fn new(initial: ThemeMode) -> Self {
        let selected = ThemeMode::ALL
            .iter()
            .position(|mode| *mode == initial)
            .expect("all theme modes are selectable");
        Self {
            selected,
            current: initial,
            targets: [Rect::default(); 3],
        }
    }

    fn update_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> ComponentUpdate<ThemeSelectorEffect> {
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
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
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) =>
            {
                let Some(index) = self
                    .targets
                    .iter()
                    .position(|target| target.contains(Position::new(mouse.column, mouse.row)))
                else {
                    return ComponentUpdate::none();
                };
                self.selected = index;
                ComponentUpdate::render(RenderRequest::Immediate)
            }
            ThemeSelectorEvent::Terminal(_) => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.targets = [Rect::default(); 3];
        let body = Floating::new("Theme", 48, 11, &KEY_BINDINGS)
            .render(frame, area, theme)
            .body;
        if body.is_empty() {
            return;
        }
        let row = |offset| Rect::new(body.x, body.y + offset, body.width, 1).intersection(body);
        frame.render_widget(
            Paragraph::new(format!(
                "  Selected: {}",
                ThemeMode::ALL[self.selected].as_str()
            ))
            .style(Style::default().fg(theme.accent())),
            row(0),
        );
        frame.render_widget(
            Paragraph::new(format!("  Current: {}", self.current.as_str()))
                .style(Style::default().fg(theme.muted())),
            row(1),
        );
        let capacity = body.height.saturating_sub(3).min(3);
        let offset = self
            .selected
            .saturating_sub(usize::from(capacity).saturating_sub(1));
        for visible in 0..capacity {
            let index = offset + usize::from(visible);
            if index >= ThemeMode::ALL.len() {
                break;
            }
            let mode = ThemeMode::ALL[index];
            let selected = index == self.selected;
            let detail = match mode {
                ThemeMode::Auto => "Follow the operating system",
                ThemeMode::Light => "Use the light palette",
                ThemeMode::Dark => "Use the dark palette",
            };
            let area = row(3 + visible);
            self.targets[index] = area;
            let label = format!("{}{:<6}", if selected { "› " } else { "  " }, mode.as_str());
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        label,
                        Style::default()
                            .fg(if selected {
                                theme.accent()
                            } else {
                                theme.text()
                            })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_display(detail, usize::from(body.width).saturating_sub(8)),
                        Style::default().fg(theme.muted()),
                    ),
                ])),
                area,
            );
        }
        if body.height >= 8 {
            let mut sample = theme.clone();
            sample.set_mode(ThemeMode::ALL[self.selected]);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  Palette preview: ", Style::default().fg(theme.muted())),
                    Span::styled("Text ", Style::default().fg(sample.text())),
                    Span::styled("Accent ", Style::default().fg(sample.accent())),
                    Span::styled("Muted", Style::default().fg(sample.muted())),
                ])),
                row(7),
            );
        }
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
    fn mouse_changes_only_the_pending_theme_and_tiny_layouts_are_safe() {
        use crate::tui::theme::Theme;
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend};
        let mut selector = ThemeSelector::new(ThemeMode::Auto);
        let mut terminal = Terminal::new(TestBackend::new(48, 11)).unwrap();
        terminal
            .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let target = selector.targets[2];
        let update = selector.update(ThemeSelectorEvent::Terminal(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: target.x,
            row: target.y,
            modifiers: KeyModifiers::NONE,
        })));
        assert!(update.effects.is_empty());
        assert_eq!(selector.current, ThemeMode::Auto);
        assert_eq!(
            selector.update(key(KeyCode::Enter)).effects,
            [ThemeSelectorEffect::Apply(ThemeMode::Dark)]
        );
        for width in 0..20 {
            for height in 0..12 {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| selector.render(frame, frame.area(), &Theme::default()))
                    .unwrap();
            }
        }
        assert_eq!(
            selector.update(key(KeyCode::Esc)).effects,
            [ThemeSelectorEffect::Dismiss]
        );
    }
}
