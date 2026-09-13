//! One-shot welcome artwork for an empty transcript.

use crate::tui::theme::{ColorScheme, Theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(50);
const FRAME_COUNT: usize = 16;
const WIDTH: u16 = 29;
const HEIGHT: u16 = 5;
const GLYPHS: [[u8; 5]; 5] = [
    [14, 27, 27, 27, 14],
    [30, 27, 30, 26, 27],
    [27, 27, 27, 10, 4],
    [31, 24, 30, 24, 31],
    [27, 26, 28, 26, 27],
];

pub(super) struct EmptyLogo {
    started_at: Instant,
    next_frame: Option<Instant>,
    frame: usize,
    ascii: bool,
}

impl EmptyLogo {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            next_frame: Some(now + FRAME_INTERVAL),
            frame: 0,
            ascii: false,
        }
    }

    pub(super) fn set_preferences(&mut self, motion: bool, ascii: bool) {
        self.ascii = ascii;
        if !motion || ascii {
            self.frame = FRAME_COUNT;
            self.next_frame = None;
        }
    }

    pub(super) const fn deadline(&self) -> Option<Instant> {
        self.next_frame
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.next_frame else {
            return false;
        };
        if now < deadline {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.started_at).as_millis();
        let next = usize::try_from(elapsed / FRAME_INTERVAL.as_millis())
            .unwrap_or(usize::MAX)
            .min(FRAME_COUNT);
        self.next_frame = (next < FRAME_COUNT).then_some(now + FRAME_INTERVAL);
        let changed = next != self.frame;
        self.frame = next;
        changed
    }

    pub(super) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }
        if area.width < WIDTH || area.height < HEIGHT {
            let text = if area.width >= 5 {
                "ORVEK"
            } else if area.width >= 2 {
                "<>"
            } else {
                "O"
            };
            let x = area.x + (area.width - text.len() as u16) / 2;
            frame.buffer_mut().set_string(
                x,
                area.y + area.height / 2,
                text,
                Style::default().fg(theme.code_text()),
            );
            return;
        }
        let x = area.x + (area.width - WIDTH) / 2;
        let y = area.y + (area.height - HEIGHT) / 2;
        for (letter, glyph) in GLYPHS.iter().enumerate() {
            let color = match (theme.scheme(), letter < 3) {
                (ColorScheme::Dark, true) => Color::Rgb(182, 160, 247),
                (ColorScheme::Dark, false) => Color::Rgb(120, 201, 208),
                (ColorScheme::Light, true) => Color::Rgb(107, 65, 168),
                (ColorScheme::Light, false) => Color::Rgb(34, 108, 118),
            };
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..5 {
                    if bits & (1 << (4 - column)) == 0 {
                        continue;
                    }
                    let offset = letter * 6 + column;
                    let mut style = Style::default().fg(color);
                    if self.frame < FRAME_COUNT && offset.abs_diff(self.frame * 2) > 4 {
                        style = style.add_modifier(Modifier::DIM);
                    }
                    frame.buffer_mut().set_string(
                        x + offset as u16,
                        y + row as u16,
                        if self.ascii { "#" } else { "█" },
                        style,
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EmptyLogo, FRAME_INTERVAL};
    use crate::tui::theme::{Theme, ThemeMode};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect, style::Color};
    use std::{
        collections::HashSet,
        time::{Duration, Instant},
    };

    fn render(logo: &EmptyLogo, width: u16, height: u16, theme: &Theme) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| logo.render(frame, frame.area(), theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn welcome_matches_the_approved_five_row_wordmark() {
        let logo = EmptyLogo::new(Instant::now());
        let buffer = render(&logo, 29, 5, &Theme::default());
        let rows = buffer
            .content()
            .chunks(29)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert_eq!(rows[0], " ███  ████  ██ ██ █████ ██ ██");
        assert_eq!(rows[4], " ███  ██ ██   █   █████ ██ ██");
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn small_and_ascii_fallbacks_are_readable() {
        let mut logo = EmptyLogo::new(Instant::now());
        assert_eq!(render(&logo, 1, 1, &Theme::default())[(0, 0)].symbol(), "O");
        let word = render(&logo, 5, 1, &Theme::default());
        assert_eq!(
            word.content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>(),
            "ORVEK"
        );
        logo.set_preferences(true, true);
        assert!(logo.deadline().is_none());
        assert!(
            render(&logo, 29, 5, &Theme::default())
                .content()
                .iter()
                .all(|cell| cell.symbol().is_ascii())
        );
    }

    #[test]
    fn artwork_stays_inside_offset_areas_at_every_small_size() {
        let logo = EmptyLogo::new(Instant::now());
        let mut terminal = Terminal::new(TestBackend::new(46, 19)).unwrap();
        for width in 0..=41 {
            for height in 0..=14 {
                let area = Rect::new(3, 2, width, height);
                terminal
                    .draw(|frame| logo.render(frame, area, &Theme::default()))
                    .unwrap();
                for y in 0..19 {
                    for x in 0..46 {
                        if !area.contains((x, y).into()) {
                            assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn approved_colors_follow_the_background_scheme() {
        for (mode, expected) in [
            (
                ThemeMode::Dark,
                [Color::Rgb(182, 160, 247), Color::Rgb(120, 201, 208)],
            ),
            (
                ThemeMode::Light,
                [Color::Rgb(107, 65, 168), Color::Rgb(34, 108, 118)],
            ),
        ] {
            let mut theme = Theme::default();
            theme.set_mode(mode);
            let buffer = render(&EmptyLogo::new(Instant::now()), 29, 5, &theme);
            let colors = buffer
                .content()
                .iter()
                .filter(|cell| cell.symbol() == "█")
                .map(|cell| cell.fg)
                .collect::<HashSet<_>>();
            assert_eq!(colors, expected.into());
        }
    }

    #[test]
    fn entrance_settles_and_reduced_motion_never_schedules_frames() {
        let now = Instant::now();
        let mut logo = EmptyLogo::new(now);
        assert!(!logo.advance(now + FRAME_INTERVAL / 2));
        assert!(logo.advance(now + FRAME_INTERVAL));
        assert!(logo.advance(now + Duration::from_secs(1)));
        assert!(logo.deadline().is_none());
        assert!(!logo.advance(now + Duration::from_secs(2)));
        let mut reduced = EmptyLogo::new(now);
        reduced.set_preferences(false, false);
        assert!(reduced.deadline().is_none());
    }
}
