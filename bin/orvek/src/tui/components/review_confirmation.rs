use super::{
    floating::Floating,
    node::{Component, ComponentUpdate, RenderRequest},
};
use crate::tui::theme::Theme;
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    text::Line,
    widgets::{Paragraph, Wrap},
};

const KEY_BINDINGS: [(&str, &str); 2] = [("enter/y", "download"), ("esc/n", "cancel")];

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
    body: Rect,
    scroll: usize,
    max_scroll: usize,
}

impl Component for ReviewDownloadConfirmation {
    type Event = ReviewConfirmationEvent;
    type Effect = ReviewConfirmationEffect;

    fn update(&mut self, event: Self::Event) -> ComponentUpdate<Self::Effect> {
        let ReviewConfirmationEvent::Terminal(event) = event;
        let previous = self.scroll;
        match &event {
            Event::Mouse(mouse) if self.body.contains(Position::new(mouse.column, mouse.row)) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3),
                    _ => {}
                }
            }
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                match key.code {
                    KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                    KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_sub(usize::from(self.body.height))
                    }
                    KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_add(usize::from(self.body.height))
                    }
                    KeyCode::Home => self.scroll = 0,
                    KeyCode::End => self.scroll = self.max_scroll,
                    _ => {}
                }
            }
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll);
        if self.scroll != previous {
            return ComponentUpdate::render(RenderRequest::Immediate);
        }
        let Event::Key(key) = event else {
            return ComponentUpdate::none();
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return ComponentUpdate::none();
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y' | 'Y') => ComponentUpdate {
                effects: vec![ReviewConfirmationEffect::Confirm],
                render: RenderRequest::Immediate,
            },
            KeyCode::Esc | KeyCode::Char('n' | 'N') => ComponentUpdate {
                effects: vec![ReviewConfirmationEffect::Dismiss],
                render: RenderRequest::Immediate,
            },
            _ => ComponentUpdate::none(),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let layout = Floating::new("Install review interface", 64, 9, &KEY_BINDINGS)
            .render(frame, area, theme);
        let lines = vec![
            Line::from("The browser review interface is not installed."),
            Line::from(""),
            Line::styled(
                "Download the matching, checksummed bundle from this Orvek release?",
                Style::default().fg(theme.muted()),
            ),
        ];
        self.body = layout.body;
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        self.max_scroll = paragraph
            .line_count(self.body.width)
            .saturating_sub(usize::from(self.body.height));
        self.scroll = self.scroll.min(self.max_scroll);
        frame.render_widget(
            paragraph.scroll((u16::try_from(self.scroll).unwrap_or(u16::MAX), 0)),
            self.body,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Component, ReviewDownloadConfirmation};
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn short_confirmation_can_scroll_to_the_full_download_question() {
        use super::{ReviewConfirmationEffect, ReviewConfirmationEvent};
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut popup = ReviewDownloadConfirmation::default();
        let mut terminal = Terminal::new(TestBackend::new(34, 7)).unwrap();
        terminal
            .draw(|frame| popup.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = popup.update(ReviewConfirmationEvent::Terminal(Event::Key(
            KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        )));
        assert!(update.effects.is_empty());
        terminal
            .draw(|frame| popup.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("Orvek release?"), "{text}");
        let update = popup.update(ReviewConfirmationEvent::Terminal(Event::Key(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )));
        assert_eq!(update.effects, vec![ReviewConfirmationEffect::Confirm]);
    }

    #[test]
    fn download_message_fits_inside_the_popup() {
        let mut terminal = Terminal::new(TestBackend::new(64, 9)).unwrap();
        terminal
            .draw(|frame| {
                ReviewDownloadConfirmation::default().render(
                    frame,
                    frame.area(),
                    &Theme::default(),
                );
            })
            .unwrap();

        let buffer = terminal.backend().buffer();
        let text = (0..buffer.area.height)
            .map(|y| (1..63).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join(" ");
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            text.contains("Download the matching, checksummed bundle from this Orvek release?")
        );
        for y in 1..8 {
            assert_eq!(buffer[(63, y)].symbol(), "│");
        }
    }
}
