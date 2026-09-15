//! A small state-driven mark with a bounded transition and no idle clock.

use super::activity::{ActivityState, ActivityVisual};
use crate::tui::theme::Theme;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(90);
const TRANSITION: Duration = Duration::from_millis(180);

pub(crate) struct ActivityMark {
    state: ActivityState,
    previous: ActivityState,
    started_at: Instant,
    next_frame: Option<Instant>,
    frame: usize,
    motion: bool,
    ascii: bool,
}

impl ActivityMark {
    pub(crate) const WIDTH: u16 = 5;
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            state: ActivityState::Idle,
            previous: ActivityState::Idle,
            started_at: now,
            next_frame: None,
            frame: 0,
            motion: true,
            ascii: false,
        }
    }
    pub(crate) fn set_preferences(&mut self, motion: bool, ascii: bool) {
        if self.motion == motion && self.ascii == ascii {
            return;
        }
        self.motion = motion;
        self.ascii = ascii;
        self.previous = self.state;
        self.frame = 0;
        self.started_at = Instant::now();
        self.next_frame =
            (motion && !ascii && self.state.active()).then_some(self.started_at + FRAME_INTERVAL);
    }
    pub(crate) fn set_state(&mut self, state: ActivityState, now: Instant) -> bool {
        if self.state == state {
            return false;
        }
        self.previous = self.state;
        self.state = state;
        self.started_at = now;
        self.frame = 0;
        self.next_frame = (self.motion && !self.ascii).then_some(now + FRAME_INTERVAL);
        true
    }
    pub(crate) const fn visual(&self) -> ActivityVisual {
        ActivityVisual::new(self.state, self.frame, self.next_frame.is_some())
    }
    pub(crate) const fn deadline(&self) -> Option<Instant> {
        self.next_frame
    }
    pub(crate) fn advance(&mut self, now: Instant) -> bool {
        let Some(deadline) = self.next_frame else {
            return false;
        };
        if now < deadline {
            return false;
        }
        let elapsed = now.saturating_duration_since(self.started_at);
        self.frame =
            usize::try_from(elapsed.as_millis() / FRAME_INTERVAL.as_millis()).unwrap_or(usize::MAX);
        self.next_frame =
            (self.state.active() || elapsed < TRANSITION).then_some(now + FRAME_INTERVAL);
        true
    }
    pub(crate) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }
        let visual = self.visual();
        let color = visual.color(theme);
        if area.width < Self::WIDTH || area.height < 2 || self.ascii {
            frame.buffer_mut().set_string(
                area.x,
                area.y,
                self.state.compact(self.ascii),
                Style::default().fg(color),
            );
            return;
        }
        let current = self.state.pixels(self.frame, self.motion);
        let previous = self.previous.pixels(0, false);
        let changed = if self.motion {
            self.frame.saturating_mul(10).min(20)
        } else {
            20
        };
        let pixel = |x: usize, y: usize| {
            if self.previous != self.state && y * 5 + x >= changed {
                previous[y][x]
            } else {
                current[y][x]
            }
        };
        for y in 0..2 {
            for x in 0..5 {
                let symbol = match (pixel(x, y * 2), pixel(x, y * 2 + 1)) {
                    (true, true) => "█",
                    (true, false) => "▀",
                    (false, true) => "▄",
                    (false, false) => " ",
                };
                frame.buffer_mut().set_string(
                    area.x + x as u16,
                    area.y + y as u16,
                    symbol,
                    Style::default().fg(color).bg(Color::Reset),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityMark, ActivityState, TRANSITION};
    use crate::tui::theme::Theme;
    use ratatui::{Terminal, backend::TestBackend, layout::Rect, style::Color};
    use std::{
        collections::HashSet,
        time::{Duration, Instant},
    };
    const STATES: [ActivityState; 6] = [
        ActivityState::Idle,
        ActivityState::Thinking,
        ActivityState::Working,
        ActivityState::Complete,
        ActivityState::Error,
        ActivityState::Cancelled,
    ];
    #[test]
    fn settled_shapes_are_distinct_and_do_not_require_a_background_color() {
        let mut terminal = Terminal::new(TestBackend::new(5, 2)).unwrap();
        let now = Instant::now();
        let mut shapes = HashSet::new();
        for state in STATES {
            let mut mark = ActivityMark::new(now);
            mark.set_state(state, now);
            mark.set_preferences(false, false);
            terminal
                .draw(|frame| mark.render(frame, frame.area(), &Theme::default()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert!(buffer.content().iter().all(|cell| cell.bg == Color::Reset));
            shapes.insert(
                buffer
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>(),
            );
        }
        assert_eq!(shapes.len(), STATES.len());
    }
    #[test]
    fn final_states_settle_and_only_active_work_keeps_a_clock() {
        let now = Instant::now();
        for state in STATES {
            let mut mark = ActivityMark::new(now);
            mark.set_state(state, now);
            mark.advance(now + TRANSITION);
            assert_eq!(mark.deadline().is_some(), state.active());
            let deadline = mark.deadline();
            assert!(!mark.set_state(state, now + Duration::from_secs(1)));
            assert_eq!(mark.deadline(), deadline);
            mark.set_preferences(false, false);
            assert!(mark.deadline().is_none());
        }
    }
    #[test]
    fn every_frame_and_compact_fallback_stays_inside_its_area() {
        let now = Instant::now();
        let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();
        for state in STATES {
            for width in 0..=5 {
                for height in 0..=2 {
                    for tick in 0..12 {
                        let mut mark = ActivityMark::new(now);
                        mark.set_state(state, now);
                        mark.advance(now + Duration::from_millis(tick * 90));
                        let area = Rect::new(3, 2, width, height);
                        terminal
                            .draw(|frame| mark.render(frame, area, &Theme::default()))
                            .unwrap();
                        for y in 0..6 {
                            for x in 0..12 {
                                if !area.contains((x, y).into()) {
                                    assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn ascii_mode_has_no_decorative_deadline() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        mark.set_state(ActivityState::Working, now);
        mark.set_preferences(true, true);
        assert!(mark.deadline().is_none());
        let mut terminal = Terminal::new(TestBackend::new(5, 2)).unwrap();
        terminal
            .draw(|frame| mark.render(frame, frame.area(), &Theme::default()))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .all(|cell| cell.symbol().is_ascii())
        );
    }
}
