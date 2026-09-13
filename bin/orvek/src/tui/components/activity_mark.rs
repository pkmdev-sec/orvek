//! A persistent, demand-driven ASCII facet for the current activity.

use crate::tui::theme::Theme;
use ratatui::{Frame, layout::Rect, style::Style};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(240);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ActivityState {
    #[default]
    Idle,
    Thinking,
    Working,
    Compacting,
    Complete,
    Error,
    Cancelled,
}

impl ActivityState {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Idle => "Ready",
            Self::Thinking => "Thinking",
            Self::Working => "Working",
            Self::Compacting => "Compacting",
            Self::Complete => "Complete",
            Self::Error => "Error",
            Self::Cancelled => "Cancelled",
        }
    }

    const fn frames(self) -> &'static [&'static str] {
        match self {
            Self::Idle => &["<   *   >"],
            Self::Thinking => &["<  . .  >", "< . . . >", "<   :   >", "< .   . >"],
            Self::Working => &[r"< / | \ >", "< - + - >", r"< \ | / >", "< - + - >"],
            Self::Compacting => &["<  > <  >", " < > < > ", "  >   <  ", "   > <   "],
            Self::Complete => &[r"<  /+\  >"],
            Self::Error => &[r"<  /!\  >"],
            Self::Cancelled => &[r"<  /x\  >"],
        }
    }

    const fn compact(self) -> &'static str {
        match self {
            Self::Idle => "*",
            Self::Thinking => ".",
            Self::Working => "/",
            Self::Compacting => ">",
            Self::Complete => "+",
            Self::Error => "!",
            Self::Cancelled => "x",
        }
    }
}

pub(crate) struct ActivityMark {
    state: ActivityState,
    started_at: Instant,
    next_frame: Option<Instant>,
    frame: usize,
}

impl ActivityMark {
    pub(crate) const WIDTH: u16 = 9;

    pub(crate) fn new(now: Instant) -> Self {
        Self {
            state: ActivityState::Idle,
            started_at: now,
            next_frame: None,
            frame: 0,
        }
    }

    pub(crate) fn set_state(&mut self, state: ActivityState, now: Instant) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.started_at = now;
        self.frame = 0;
        self.next_frame = (state.frames().len() > 1).then_some(now + FRAME_INTERVAL);
        true
    }

    pub(crate) const fn state(&self) -> ActivityState {
        self.state
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

        let elapsed = now.saturating_duration_since(self.started_at).as_millis();
        let frame = usize::try_from(elapsed / FRAME_INTERVAL.as_millis()).unwrap_or(usize::MAX)
            % self.state.frames().len();
        self.next_frame = Some(now + FRAME_INTERVAL);
        if frame == self.frame {
            return false;
        }
        self.frame = frame;
        true
    }

    pub(crate) fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        let area = area.intersection(frame.area());
        if area.is_empty() {
            return;
        }
        let color = match self.state {
            ActivityState::Idle | ActivityState::Cancelled => theme.muted(),
            ActivityState::Thinking => theme.thinking_medium(),
            ActivityState::Working | ActivityState::Complete => theme.accent(),
            ActivityState::Compacting => theme.thinking_high(),
            ActivityState::Error => theme.thinking_xhigh(),
        };
        let art = if area.width >= Self::WIDTH {
            self.state.frames()[self.frame]
        } else {
            self.state.compact()
        };
        frame
            .buffer_mut()
            .set_string(area.x, area.y, art, Style::default().fg(color));
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityMark, ActivityState, FRAME_INTERVAL};
    use crate::tui::theme::{Theme, ThemeMode};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
    use std::{collections::HashSet, time::Instant};

    const STATES: [ActivityState; 7] = [
        ActivityState::Idle,
        ActivityState::Thinking,
        ActivityState::Working,
        ActivityState::Compacting,
        ActivityState::Complete,
        ActivityState::Error,
        ActivityState::Cancelled,
    ];

    fn render(mark: &ActivityMark, theme: &Theme) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(ActivityMark::WIDTH, 1)).unwrap();
        terminal
            .draw(|frame| mark.render(frame, frame.area(), theme))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn symbols(buffer: &Buffer) -> String {
        buffer.content().iter().map(|cell| cell.symbol()).collect()
    }

    #[test]
    fn activity_changes_the_ascii_shape_immediately() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        let theme = Theme::default();
        let mut shapes = HashSet::new();
        for state in STATES {
            mark.set_state(state, now);
            let shape = symbols(&render(&mark, &theme));
            assert!(shape.is_ascii());
            assert_eq!(shape.len(), usize::from(ActivityMark::WIDTH));
            assert_eq!(mark.state(), state);
            assert!(!state.label().is_empty());
            assert!(shapes.insert(shape));
        }
        assert_eq!(symbols(&render(&mark, &theme)), r"<  /x\  >");
    }

    #[test]
    fn only_active_work_schedules_animation() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        for state in STATES {
            mark.set_state(state, now);
            let active = matches!(
                state,
                ActivityState::Thinking | ActivityState::Working | ActivityState::Compacting
            );
            assert_eq!(mark.deadline().is_some(), active);
            assert!(!mark.advance(now + FRAME_INTERVAL / 2));
            let first = render(&mark, &Theme::default());
            assert_eq!(mark.advance(now + FRAME_INTERVAL), active);
            assert_eq!(first != render(&mark, &Theme::default()), active);
        }
    }

    #[test]
    fn repeated_events_preserve_phase_and_terminal_states_stop_the_timer() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        assert!(mark.set_state(ActivityState::Working, now));
        assert!(mark.advance(now + FRAME_INTERVAL));
        let deadline = mark.deadline();
        let working = render(&mark, &Theme::default());
        assert!(!mark.set_state(ActivityState::Working, now + FRAME_INTERVAL));
        assert_eq!(mark.deadline(), deadline);
        assert_eq!(render(&mark, &Theme::default()), working);

        assert!(mark.set_state(ActivityState::Complete, now + FRAME_INTERVAL));
        assert_eq!(mark.deadline(), None);
        assert!(!mark.advance(now + FRAME_INTERVAL * 100));
        assert_eq!(symbols(&render(&mark, &Theme::default())), r"<  /+\  >");
    }

    #[test]
    fn every_animation_frame_stays_ascii_and_fixed_width() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        for state in [
            ActivityState::Thinking,
            ActivityState::Working,
            ActivityState::Compacting,
        ] {
            mark.set_state(state, now);
            for step in 0..=8 {
                mark.advance(now + FRAME_INTERVAL * step);
                let art = symbols(&render(&mark, &Theme::default()));
                assert!(art.is_ascii());
                assert_eq!(art.len(), usize::from(ActivityMark::WIDTH));
            }
        }
    }

    #[test]
    fn narrow_or_empty_areas_do_not_overwrite_neighbors() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        let mut terminal = Terminal::new(TestBackend::new(16, 3)).unwrap();
        for state in STATES {
            mark.set_state(state, now);
            for width in 0..=11 {
                for height in 0..=2 {
                    let area = Rect::new(2, 1, width, height);
                    terminal
                        .draw(|frame| mark.render(frame, area, &Theme::default()))
                        .unwrap();
                    for y in 0..3 {
                        for x in 0..16 {
                            if !area.contains((x, y).into()) {
                                assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
                            }
                        }
                    }
                    if width > 0 && width < ActivityMark::WIDTH && height > 0 {
                        assert_ne!(terminal.backend().buffer()[(2, 1)].symbol(), " ");
                        assert_eq!(terminal.backend().buffer()[(3, 1)].symbol(), " ");
                    }
                }
            }
        }
    }

    #[test]
    fn active_mark_follows_the_selected_theme() {
        let now = Instant::now();
        let mut mark = ActivityMark::new(now);
        mark.set_state(ActivityState::Thinking, now);
        for mode in [ThemeMode::Light, ThemeMode::Dark] {
            let mut theme = Theme::default();
            theme.set_mode(mode);
            let buffer = render(&mark, &theme);
            assert!(
                buffer
                    .content()
                    .iter()
                    .all(|cell| cell.fg == theme.thinking_medium())
            );
        }
    }
}
