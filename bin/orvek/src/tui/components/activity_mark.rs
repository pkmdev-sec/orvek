//! State and timing shared by the activity-driven composer chrome.

use super::activity::{ActivityState, ActivityVisual};
use std::time::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_millis(90);

pub(crate) struct ActivityMark {
    state: ActivityState,
    started_at: Instant,
    next_frame: Option<Instant>,
    frame: usize,
    motion: bool,
}

impl ActivityMark {
    pub(crate) fn new(now: Instant) -> Self {
        Self {
            state: ActivityState::Idle,
            started_at: now,
            next_frame: None,
            frame: 0,
            motion: true,
        }
    }

    pub(crate) fn set_motion(&mut self, motion: bool) {
        if self.motion == motion {
            return;
        }
        self.motion = motion;
        self.frame = 0;
        self.started_at = Instant::now();
        self.next_frame =
            (motion && self.state.active()).then_some(self.started_at + FRAME_INTERVAL);
    }

    pub(crate) fn set_state(&mut self, state: ActivityState, now: Instant) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.started_at = now;
        self.frame = 0;
        self.next_frame = (self.motion && state.active()).then_some(now + FRAME_INTERVAL);
        true
    }

    pub(crate) const fn visual(&self) -> ActivityVisual {
        ActivityVisual::new(
            self.state,
            self.frame,
            self.state.active() && self.next_frame.is_some(),
        )
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
        self.next_frame = Some(now + FRAME_INTERVAL);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{ActivityMark, ActivityState, FRAME_INTERVAL};
    use std::time::{Duration, Instant};

    #[test]
    fn active_state_advances_the_shared_composer_frame() {
        let now = Instant::now();
        let mut activity = ActivityMark::new(now);
        activity.set_state(ActivityState::Working, now);
        let initial = activity.visual();

        assert!(activity.advance(now + FRAME_INTERVAL));
        assert_ne!(activity.visual(), initial);
    }

    #[test]
    fn final_states_settle_and_only_active_work_keeps_a_clock() {
        let now = Instant::now();
        for state in [
            ActivityState::Idle,
            ActivityState::Thinking,
            ActivityState::Working,
            ActivityState::Complete,
            ActivityState::Error,
            ActivityState::Cancelled,
        ] {
            let mut activity = ActivityMark::new(now);
            activity.set_state(state, now);
            assert_eq!(activity.deadline().is_some(), state.active());
            let deadline = activity.deadline();
            assert!(!activity.set_state(state, now + Duration::from_secs(1)));
            assert_eq!(activity.deadline(), deadline);
            activity.set_motion(false);
            assert!(activity.deadline().is_none());
        }
    }
}
