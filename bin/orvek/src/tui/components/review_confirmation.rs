//! Review-specific confirmation dialog content.

use super::dialog::{ConfirmationDialog, KeyBinding};
use crate::tui::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::{
    style::Style,
    text::{Line, Text},
};

const KEY_BINDINGS: [KeyBinding; 2] = [("enter/y", "download"), ("esc/n", "cancel")];
const CONFIRM_KEYS: [KeyCode; 3] = [KeyCode::Enter, KeyCode::Char('y'), KeyCode::Char('Y')];
const DISMISS_KEYS: [KeyCode; 3] = [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('N')];

pub(super) fn review_download_confirmation() -> ConfirmationDialog {
    ConfirmationDialog::new(
        "Install review interface",
        64,
        9,
        &KEY_BINDINGS,
        review_body,
        &CONFIRM_KEYS,
        &DISMISS_KEYS,
    )
}

fn review_body(theme: &Theme) -> Text<'static> {
    Text::from(vec![
        Line::from("The browser review interface is not installed."),
        Line::from(""),
        Line::styled(
            "Download the matching, checksummed bundle from this Orvek release?",
            Style::default().fg(theme.muted()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::review_download_confirmation;
    use crate::tui::{components::node::Component, theme::Theme};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn short_confirmation_can_scroll_to_the_full_download_question() {
        let mut popup = review_download_confirmation();
        let mut terminal = Terminal::new(TestBackend::new(34, 7)).unwrap();
        terminal
            .draw(|frame| popup.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        let update = popup.update(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)));
        assert!(update.effects.is_empty());
        assert!(popup.scroll() > 0);
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
    }

    #[test]
    fn download_message_fits_inside_the_popup() {
        let mut terminal = Terminal::new(TestBackend::new(64, 9)).unwrap();
        terminal
            .draw(|frame| {
                review_download_confirmation().render(frame, frame.area(), &Theme::default());
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

    #[test]
    fn confirmation_maps_only_press_events_to_choices() {
        use super::super::dialog::ConfirmationChoice;
        let mut popup = review_download_confirmation();
        let confirm = popup.update(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(confirm.effects, [ConfirmationChoice::Confirm]);

        let mut popup = review_download_confirmation();
        let dismiss = popup.update(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert_eq!(dismiss.effects, [ConfirmationChoice::Dismiss]);
    }
}
