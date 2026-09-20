//! Immediate startup surface while the durable session is assembled.

use super::brand::BrandMark;
use crate::tui::{spinner::Spinner, theme::Theme};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use std::time::Instant;

pub(crate) struct StartupScreen {
    brand: BrandMark,
    spinner: Spinner,
    message: &'static str,
}

impl StartupScreen {
    pub(crate) fn new(now: Instant, message: &'static str) -> Self {
        Self {
            brand: BrandMark::new(now),
            spinner: Spinner::new(now),
            message,
        }
    }

    pub(crate) fn animation_deadline(&self) -> Instant {
        self.brand.deadline().map_or_else(
            || self.spinner.deadline(),
            |brand| brand.min(self.spinner.deadline()),
        )
    }

    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        self.brand.advance(now) | self.spinner.advance(now)
    }

    pub(crate) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }
        frame.buffer_mut().set_style(area, Style::reset());

        let full = area.width >= 29 && area.height >= 9;
        let large = area.width >= 58 && area.height >= 14;
        let status_y = if full {
            let brand_height = if large { 10 } else { 5 };
            let content_height = brand_height + 4;
            let content_y = area.y + area.height.saturating_sub(content_height) / 2;
            let brand_area = Rect::new(area.x, content_y, area.width, brand_height);
            if large {
                self.brand.render_large(frame, brand_area, theme);
            } else {
                self.brand.render(frame, brand_area, theme);
            }
            content_y + brand_height + 1
        } else {
            area.y + area.height / 2
        };
        let message = if area.width >= 24 {
            self.message
        } else {
            "Starting Orvek"
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    self.spinner.symbol(),
                    Style::default()
                        .fg(theme.accent())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(message, Style::default().fg(theme.text())),
            ]))
            .alignment(Alignment::Center),
            Rect::new(area.x, status_y, area.width, 1),
        );

        if full && status_y + 2 < area.bottom() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "Ctrl+C to cancel",
                    Style::default().fg(theme.muted()),
                ))
                .alignment(Alignment::Center),
                Rect::new(area.x, status_y + 2, area.width, 1),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StartupScreen;
    use crate::tui::{spinner::SPINNER_INTERVAL, theme::Theme};
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    use std::time::Instant;

    fn text(screen: &StartupScreen, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| screen.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn standard_surface_has_brand_status_and_cancellation_hint() {
        let screen = StartupScreen::new(Instant::now(), "Restoring session");
        let rendered = text(&screen, 80, 24);

        assert!(rendered.contains("Restoring session"));
        assert!(rendered.contains("Ctrl+C to cancel"));
        assert!(rendered.matches('█').count() > 200);
    }

    #[test]
    fn startup_preserves_the_terminal_background() {
        let screen = StartupScreen::new(Instant::now(), "Restoring session");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| screen.render(frame, frame.area(), &Theme::default()))
            .unwrap();

        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| cell.bg == ratatui::style::Color::Reset)
        );
    }

    #[test]
    fn compact_surface_uses_a_short_status() {
        let screen = StartupScreen::new(Instant::now(), "Restoring session");

        assert!(text(&screen, 18, 3).contains("Starting Orvek"));
    }

    #[test]
    fn every_compact_size_stays_inside_its_area() {
        let screen = StartupScreen::new(Instant::now(), "Loading session");
        let mut terminal = Terminal::new(TestBackend::new(44, 18)).unwrap();
        for width in 0..=39 {
            for height in 0..=13 {
                let area = Rect::new(3, 2, width, height);
                terminal
                    .draw(|frame| screen.render(frame, area, &Theme::default()))
                    .unwrap();
                for y in 0..18 {
                    for x in 0..44 {
                        if !area.contains((x, y).into()) {
                            assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn animation_uses_component_deadlines() {
        let now = Instant::now();
        let mut screen = StartupScreen::new(now, "Loading session");

        assert!(!screen.advance(now + SPINNER_INTERVAL / 2));
        assert!(screen.advance(now + SPINNER_INTERVAL));
        assert!(screen.animation_deadline() > now + SPINNER_INTERVAL);
    }
}
