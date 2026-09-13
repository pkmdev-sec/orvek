//! Confirmation before downloading and opening the matching review interface.

use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::{
    format::{truncate_display, wrap_display_lines},
    theme::Theme,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

const EXPLANATION: [&str; 2] = [
    "The browser review interface needs to be installed.",
    "Download the bundle for this Orvek version, verify it, then open the review in your browser.",
];
const ACTIONS: [(&str, &str); 2] = [("enter/y", "Download & open"), ("esc/n", "Cancel")];

pub(super) enum ReviewConfirmationEvent {
    Terminal(Event),
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum ReviewConfirmationEffect {
    Confirm,
    Dismiss,
}

#[derive(Default)]
pub(super) struct ReviewDownloadConfirmation {
    scroll: usize,
    max_scroll: usize,
    page_height: usize,
    targets: [Rect; 2],
    body: Rect,
    wrapped_width: u16,
    lines: Vec<(String, bool)>,
}

impl ReviewDownloadConfirmation {
    fn effect(confirm: bool) -> ComponentUpdate<ReviewConfirmationEffect> {
        ComponentUpdate {
            effects: vec![if confirm {
                ReviewConfirmationEffect::Confirm
            } else {
                ReviewConfirmationEffect::Dismiss
            }],
            render: RenderRequest::Immediate,
        }
    }
}

impl Component for ReviewDownloadConfirmation {
    type Event = ReviewConfirmationEvent;
    type Effect = ReviewConfirmationEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        match event {
            ReviewConfirmationEvent::Terminal(Event::Key(key))
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
            {
                match key.code {
                    KeyCode::Enter | KeyCode::Char('y' | 'Y') => return Self::effect(true),
                    KeyCode::Esc | KeyCode::Char('n' | 'N') => return Self::effect(false),
                    KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(self.page_height.max(1))
                    }
                    KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_add(self.page_height.max(1))
                    }
                    KeyCode::Home => self.scroll = 0,
                    KeyCode::End => self.scroll = self.max_scroll,
                    _ => return ComponentUpdate::none(),
                }
            }
            ReviewConfirmationEvent::Terminal(Event::Mouse(mouse)) => {
                let point = Position::new(mouse.column, mouse.row);
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        if let Some(index) = self
                            .targets
                            .iter()
                            .position(|target| target.contains(point))
                        {
                            return Self::effect(index == 0);
                        }
                        return ComponentUpdate::none();
                    }
                    MouseEventKind::ScrollUp if self.body.contains(point) => {
                        self.scroll = self.scroll.saturating_sub(1)
                    }
                    MouseEventKind::ScrollDown if self.body.contains(point) => {
                        self.scroll = self.scroll.saturating_add(1)
                    }
                    _ => return ComponentUpdate::none(),
                }
            }
            _ => return ComponentUpdate::none(),
        }
        self.scroll = self.scroll.min(self.max_scroll);
        ComponentUpdate::render(RenderRequest::Immediate)
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        self.targets = [Rect::default(); 2];
        let width = area.width.min(64).saturating_sub(6);
        if width != self.wrapped_width || self.lines.is_empty() {
            self.wrapped_width = width;
            self.lines = wrap_display_lines(EXPLANATION[0], usize::from(width))
                .into_iter()
                .map(|line| (line, false))
                .collect();
            self.lines.push((String::new(), false));
            self.lines.extend(
                wrap_display_lines(EXPLANATION[1], usize::from(width))
                    .into_iter()
                    .map(|line| (line, true)),
            );
        }
        let together = width >= 39;
        let action_rows = if together { 1 } else { 2 };
        let height = (self.lines.len() + action_rows + 4)
            .max(9)
            .min(usize::from(u16::MAX)) as u16;
        let body = Floating::new("Install review interface", 64, height, &[])
            .render(frame, area, theme)
            .body;
        self.body = body;
        if body.is_empty() {
            self.max_scroll = 0;
            return;
        }
        let action_height = (action_rows as u16).min(body.height);
        let actions_y = body.bottom() - action_height;
        let explanation_y = (body.y + 1).min(actions_y);
        let available = actions_y.saturating_sub(explanation_y).saturating_sub(1);
        let overflow = self.lines.len() > usize::from(available);
        let capacity = available.saturating_sub(u16::from(overflow));
        self.page_height = usize::from(capacity);
        self.max_scroll = self.lines.len().saturating_sub(self.page_height);
        self.scroll = self.scroll.min(self.max_scroll);
        let x = body.x + 2.min(body.width);
        for (row, (line, muted)) in self
            .lines
            .iter()
            .skip(self.scroll)
            .take(self.page_height)
            .enumerate()
        {
            frame.render_widget(
                Paragraph::new(line.as_str()).style(Style::default().fg(if *muted {
                    theme.muted()
                } else {
                    theme.text()
                })),
                Rect::new(x, explanation_y + row as u16, width, 1),
            );
        }
        if overflow && available > 0 {
            let hint = if self.scroll == 0 {
                "↓ More"
            } else if self.scroll == self.max_scroll {
                "↑ Back"
            } else {
                "↑ / ↓ More"
            };
            frame.render_widget(
                Paragraph::new(truncate_display(hint, usize::from(width)))
                    .style(Style::default().fg(theme.muted())),
                Rect::new(x, explanation_y + capacity, width, 1),
            );
        }
        let mut action_x = if together {
            body.x + body.width.saturating_sub(39) / 2
        } else {
            x
        };
        for (index, (key, label)) in ACTIONS.iter().enumerate() {
            let y = actions_y + if together { 0 } else { index as u16 };
            let target =
                Rect::new(action_x, y, (key.len() + label.len() + 1) as u16, 1).intersection(body);
            self.targets[index] = target;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(*key, Style::default().fg(theme.text())),
                    Span::styled(
                        format!(" {label}"),
                        Style::default().fg(if index == 0 {
                            theme.accent()
                        } else {
                            theme.muted()
                        }),
                    ),
                ])),
                target,
            );
            action_x += (key.len() + label.len() + 5) as u16;
            if !together {
                action_x = x;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn render(dialog: &mut ReviewDownloadConfirmation, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| dialog.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn download_copy_and_both_actions_remain_reachable_in_short_layouts() {
        let mut dialog = ReviewDownloadConfirmation::default();
        let text = render(&mut dialog, 64, 9);
        assert!(text.contains("needs to be installed"));
        assert!(text.contains("Download & open"));
        assert!(text.contains("Cancel"));
        render(&mut dialog, 32, 9);
        assert!(dialog.max_scroll > 0);
        let targets = dialog.targets;
        dialog.update(ReviewConfirmationEvent::Terminal(Event::Key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        )));
        let text = render(&mut dialog, 32, 9);
        assert!(text.contains("browser."));
        assert_eq!(dialog.targets, targets);
    }

    #[test]
    fn mouse_and_keyboard_share_confirm_and_dismiss_effects() {
        let mut dialog = ReviewDownloadConfirmation::default();
        render(&mut dialog, 64, 9);
        for (index, code) in [KeyCode::Enter, KeyCode::Esc].into_iter().enumerate() {
            let target = dialog.targets[index];
            let clicked = dialog.update(ReviewConfirmationEvent::Terminal(Event::Mouse(
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: target.x,
                    row: target.y,
                    modifiers: KeyModifiers::NONE,
                },
            )));
            let typed = dialog.update(ReviewConfirmationEvent::Terminal(Event::Key(
                KeyEvent::new(code, KeyModifiers::NONE),
            )));
            assert_eq!(clicked.effects, typed.effects);
        }
    }

    #[test]
    fn resize_and_tiny_rectangles_keep_all_targets_bounded() {
        let mut dialog = ReviewDownloadConfirmation::default();
        for width in 0..65 {
            for height in 0..14 {
                render(&mut dialog, width, height);
                assert!(dialog.targets.iter().all(
                    |target| target.is_empty() || dialog.body.intersection(*target) == *target
                ));
            }
        }
    }
    #[test]
    fn release_events_are_ignored_and_uppercase_repeats_keep_native_actions() {
        let mut dialog = ReviewDownloadConfirmation::default();
        for (code, expected) in [
            (KeyCode::Char('Y'), ReviewConfirmationEffect::Confirm),
            (KeyCode::Char('N'), ReviewConfirmationEffect::Dismiss),
        ] {
            let mut key = KeyEvent::new(code, KeyModifiers::NONE);
            key.kind = KeyEventKind::Release;
            assert!(
                dialog
                    .update(ReviewConfirmationEvent::Terminal(Event::Key(key)))
                    .effects
                    .is_empty()
            );
            key.kind = KeyEventKind::Repeat;
            assert_eq!(
                dialog
                    .update(ReviewConfirmationEvent::Terminal(Event::Key(key)))
                    .effects,
                [expected]
            );
        }
    }
}
