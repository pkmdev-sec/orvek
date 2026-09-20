//! Bounded, independently expiring transient notices.

use super::dialog::Dialog;
use crate::tui::{
    format::sanitize_terminal_text,
    theme::{FeedbackTone, Theme},
};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const TOAST_DURATION: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ToastTextRole {
    Tone,
    Primary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ToastSpan {
    text: String,
    role: ToastTextRole,
    bold: bool,
}

impl ToastSpan {
    pub(super) fn tone(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: ToastTextRole::Tone,
            bold: false,
        }
    }

    pub(super) fn strong(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: ToastTextRole::Tone,
            bold: true,
        }
    }

    pub(super) fn primary(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            role: ToastTextRole::Primary,
            bold: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ToastBody {
    Plain(String),
    Rich(Vec<ToastSpan>),
}

pub(super) struct Toast {
    body: ToastBody,
    tone: FeedbackTone,
    expires_at: Instant,
}

impl Toast {
    pub(super) fn plain(message: impl Into<String>, tone: FeedbackTone, now: Instant) -> Self {
        Self {
            body: ToastBody::Plain(sanitize_terminal_text(&message.into()).into_owned()),
            tone,
            expires_at: now + TOAST_DURATION,
        }
    }

    pub(super) fn rich(spans: Vec<ToastSpan>, tone: FeedbackTone, now: Instant) -> Self {
        let spans = spans
            .into_iter()
            .map(|span| ToastSpan {
                text: sanitize_terminal_text(&span.text).replace('\n', " "),
                ..span
            })
            .collect();
        Self {
            body: ToastBody::Rich(spans),
            tone,
            expires_at: now + TOAST_DURATION,
        }
    }

    pub(super) fn update_available(version: impl std::fmt::Display, now: Instant) -> Self {
        Self::rich(
            vec![
                ToastSpan::tone("Update available · "),
                ToastSpan::strong(format!("v{version}")),
                ToastSpan::tone(" · run "),
                ToastSpan::primary("`orvek update`"),
            ],
            FeedbackTone::Success,
            now,
        )
    }

    #[cfg(test)]
    pub(super) const fn tone(&self) -> FeedbackTone {
        self.tone
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.expires_at
    }

    pub(super) fn text(&self, theme: &Theme) -> Text<'static> {
        let tone = theme.feedback(self.tone);
        match &self.body {
            ToastBody::Plain(message) => Text::from(
                message
                    .split('\n')
                    .map(|line| {
                        Line::styled(
                            line.to_owned(),
                            Style::default().fg(tone).add_modifier(Modifier::BOLD),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            ToastBody::Rich(spans) => Text::from(Line::from(
                spans
                    .iter()
                    .map(|span| {
                        let color = match span.role {
                            ToastTextRole::Tone => tone,
                            ToastTextRole::Primary => theme.text(),
                        };
                        let style = if span.bold {
                            Style::default().fg(color).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(color)
                        };
                        Span::styled(span.text.clone(), style)
                    })
                    .collect::<Vec<_>>(),
            )),
        }
    }

    fn size(&self, available_width: u16, theme: &Theme) -> (u16, u16) {
        let text = self.text(theme);
        let longest = text.lines.iter().map(Line::width).max().unwrap_or(0);
        let width = u16::try_from(longest.saturating_add(4))
            .unwrap_or(u16::MAX)
            .clamp(12, 64)
            .min(available_width);
        let inset = u16::from(width >= 40);
        let text_width = width.saturating_sub(2 + inset * 2).max(1);
        let line_count = Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .line_count(text_width);
        let body_height = u16::try_from(line_count).unwrap_or(u16::MAX).max(1);
        (width, body_height.saturating_add(2))
    }

    fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) -> Option<u16> {
        if area.width < 3 || area.height < 3 {
            return None;
        }
        let text = self.text(theme);
        let (width, height) = self.size(area.width, theme);
        if height > area.height {
            return None;
        }
        let color = theme.feedback(self.tone);
        let layout = Dialog::new("", width, height, &[])
            .at_top()
            .colors(color, color)
            .render(frame, area, theme);
        let inset = u16::from(width >= 40);
        let text_area = Rect::new(
            layout.body.x + inset,
            layout.body.y,
            layout.body.width.saturating_sub(inset * 2),
            layout.body.height,
        );
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        let paragraph = if paragraph.line_count(text_area.width) == 1 {
            paragraph.centered()
        } else {
            paragraph.left_aligned()
        };
        frame.render_widget(paragraph, text_area);
        Some(height)
    }
}

#[derive(Default)]
pub(super) struct ToastStack {
    toasts: VecDeque<Toast>,
}

impl ToastStack {
    pub(super) const MAX_VISIBLE: usize = 3;

    pub(super) fn push(&mut self, toast: Toast) {
        if self.toasts.len() == Self::MAX_VISIBLE {
            self.toasts.pop_front();
        }
        self.toasts.push_back(toast);
    }

    pub(super) fn expire(&mut self, now: Instant) -> bool {
        let previous = self.toasts.len();
        self.toasts.retain(|toast| now < toast.expires_at);
        previous != self.toasts.len()
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.toasts.iter().map(Toast::deadline).min()
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let mut remaining = area;
        for toast in self.toasts.iter().rev() {
            let Some(height) = toast.render(frame, remaining, theme) else {
                break;
            };
            let consumed = height.saturating_add(1);
            remaining.y = remaining.y.saturating_add(consumed);
            remaining.height = remaining.height.saturating_sub(consumed);
        }
    }

    #[cfg(test)]
    pub(super) fn latest(&self) -> Option<&Toast> {
        self.toasts.back()
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.toasts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{Toast, ToastStack};
    use crate::tui::theme::{FeedbackTone, Theme};
    use ratatui::{Terminal, backend::TestBackend};
    use std::time::{Duration, Instant};

    #[test]
    fn stack_keeps_three_newest_toasts_and_expires_independently() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        for index in 0..4 {
            stack.push(Toast::plain(
                format!("notice {index}"),
                FeedbackTone::Info,
                now + Duration::from_secs(index),
            ));
        }

        assert_eq!(stack.len(), 3);
        assert_eq!(stack.deadline(), Some(now + Duration::from_secs(11)));
        assert!(stack.expire(now + Duration::from_secs(12)));
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn newest_toast_renders_first_with_semantic_color() {
        let now = Instant::now();
        let mut stack = ToastStack::default();
        stack.push(Toast::plain("older", FeedbackTone::Warning, now));
        stack.push(Toast::plain("newer", FeedbackTone::Error, now));
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();

        terminal
            .draw(|frame| stack.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let top = (0..40)
            .map(|column| buffer[(column, 1)].symbol())
            .collect::<String>();
        assert!(top.contains("newer"));
        let newer = (0..40)
            .find(|column| buffer[(*column, 1)].symbol() == "n")
            .unwrap();
        assert_eq!(buffer[(newer, 1)].fg, Theme::default().error());
    }

    #[test]
    fn plain_toasts_sanitize_control_characters() {
        let toast = Toast::plain("safe\u{1b}[31m\nnext", FeedbackTone::Info, Instant::now());
        let text = toast.text(&Theme::default());
        assert_eq!(text.lines.len(), 2);
        assert!(!text.lines[0].to_string().contains('\u{1b}'));
    }
}
