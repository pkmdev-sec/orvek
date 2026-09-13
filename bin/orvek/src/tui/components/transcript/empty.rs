//! Demand-driven empty transcript artwork.

use crate::{app::config::ReasoningEffort, tui::theme::Theme};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(220);
const FRAME_COUNT: usize = 16;
const MARK_WIDTH: u16 = 18;
const MARK: [&str; 8] = [
    r"        /\",
    r"      / /\ \",
    r"    /  /  \  \",
    r"  /___/____\___\",
    r"  \   \    /   /",
    r"    \  \  /  /",
    r"      \ \/ /",
    r"        \/",
];
const WORDMARK_WIDTH: u16 = 30;
const WORDMARK: [&str; 3] = [
    r" /--\  |--\  \   /  |---  |  /",
    r" |  |  |__/   \ /   |--   |<",
    r" \--/  |  \    V    |---  |  \",
];

pub(super) struct EmptyLogo {
    started_at: Instant,
    next_frame: Instant,
    frame: usize,
}

impl EmptyLogo {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            started_at: now,
            next_frame: now + FRAME_INTERVAL,
            frame: 0,
        }
    }

    pub(super) const fn deadline(&self) -> Instant {
        self.next_frame
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        if now < self.next_frame {
            return false;
        }

        let elapsed = now.saturating_duration_since(self.started_at).as_millis();
        let frame = usize::try_from(elapsed / FRAME_INTERVAL.as_millis()).unwrap_or(usize::MAX)
            % FRAME_COUNT;
        self.next_frame = now + FRAME_INTERVAL;
        if frame == self.frame {
            return false;
        }
        self.frame = frame;
        true
    }

    pub(super) fn render(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        theme: &Theme,
        effort: ReasoningEffort,
    ) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }

        let (wordmark, width): (&[&str], u16) = if area.width >= WORDMARK_WIDTH && area.height >= 3
        {
            (&WORDMARK, WORDMARK_WIDTH)
        } else if area.width >= 5 {
            (&["ORVEK"], 5)
        } else if area.width >= 2 {
            (&["<>"], 2)
        } else {
            (&["O"], 1)
        };
        let wordmark_height = wordmark.len() as u16;
        let show_mark = area.width >= MARK_WIDTH && area.height >= 9 + wordmark_height;
        let height = wordmark_height + if show_mark { 9 } else { 0 };
        let mut y = area.y + (area.height - height) / 2;

        if show_mark {
            let x = area.x + (area.width - MARK_WIDTH) / 2;
            let highlight = self.frame.min(FRAME_COUNT - 1 - self.frame);
            for (row, line) in MARK.iter().enumerate() {
                let mut style = Style::default().fg(theme.effort(effort));
                if row == highlight {
                    style = style.add_modifier(Modifier::BOLD);
                }
                frame
                    .buffer_mut()
                    .set_string(x, y + row as u16, line, style);
            }
            y += 9;
        }

        let x = area.x + (area.width - width) / 2;
        for (row, line) in wordmark.iter().enumerate() {
            frame.buffer_mut().set_string(
                x,
                y + row as u16,
                line,
                Style::default().fg(theme.code_text()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EmptyLogo, FRAME_INTERVAL};
    use crate::{
        app::config::ReasoningEffort,
        tui::theme::{Theme, ThemeMode},
    };
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
    use std::{collections::HashSet, time::Instant};

    fn render(logo: &EmptyLogo, width: u16, height: u16, theme: &Theme) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                logo.render(frame, frame.area(), theme, ReasoningEffort::Medium);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn rows(buffer: &Buffer) -> Vec<String> {
        buffer
            .content()
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect()
    }

    #[test]
    fn full_artwork_is_a_faceted_diamond_above_an_ascii_wordmark() {
        let buffer = render(&EmptyLogo::new(Instant::now()), 41, 14, &Theme::default());
        let rows = rows(&buffer);

        assert_eq!(rows[1].trim(), r"/\");
        assert_eq!(rows[4].trim(), r"/___/____\___\");
        assert_eq!(rows[8].trim(), r"\/");
        assert!(rows[9].trim().is_empty());
        assert_eq!(rows[10].trim(), r"/--\  |--\  \   /  |---  |  /");
        assert_eq!(rows[11].trim(), r"|  |  |__/   \ /   |--   |<");
        assert_eq!(rows[12].trim(), r"\--/  |  \    V    |---  |  \");
        assert!(rows.iter().all(|row| row.is_ascii()));
    }

    #[test]
    fn small_terminals_get_complete_readable_fallbacks() {
        let logo = EmptyLogo::new(Instant::now());
        let theme = Theme::default();

        assert_eq!(rows(&render(&logo, 5, 1, &theme)), ["ORVEK"]);
        assert_eq!(rows(&render(&logo, 2, 1, &theme)), ["<>"]);
        assert_eq!(rows(&render(&logo, 1, 1, &theme)), ["O"]);
        assert!(rows(&render(&logo, 20, 10, &theme))[9].contains("ORVEK"));
        assert!(rows(&render(&logo, 30, 3, &theme))[0].contains(r"/--\  |--\"));
    }

    #[test]
    fn artwork_stays_inside_offset_areas_at_every_small_size() {
        let logo = EmptyLogo::new(Instant::now());
        let theme = Theme::default();
        let mut terminal = Terminal::new(TestBackend::new(46, 19)).unwrap();
        for width in 0..=41 {
            for height in 0..=14 {
                let area = Rect::new(3, 2, width, height);
                terminal
                    .draw(|frame| {
                        logo.render(frame, area, &theme, ReasoningEffort::Medium);
                    })
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
    fn artwork_uses_theme_colors_in_light_and_dark_modes() {
        let logo = EmptyLogo::new(Instant::now());
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let mut theme = Theme::default();
            theme.set_mode(mode);
            let buffer = render(&logo, 41, 14, &theme);
            let colors = buffer
                .content()
                .iter()
                .filter(|cell| cell.symbol() != " ")
                .map(|cell| cell.fg)
                .collect::<HashSet<_>>();

            assert_eq!(colors, [theme.thinking_medium(), theme.code_text()].into());
        }
    }

    #[test]
    fn animation_moves_the_highlight_without_changing_the_lettering() {
        let start = Instant::now();
        let mut logo = EmptyLogo::new(start);
        let theme = Theme::default();
        let first = render(&logo, 41, 14, &theme);

        assert!(!logo.advance(start + FRAME_INTERVAL / 2));
        assert!(logo.advance(start + FRAME_INTERVAL));
        let second = render(&logo, 41, 14, &theme);
        assert_eq!(rows(&first), rows(&second));
        assert_ne!(first, second);
        assert!(logo.deadline() > start + FRAME_INTERVAL);
    }
}
