//! Shared timing for finite, demand-driven component animations.

use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TimelineSample {
    pub(super) progress: f64,
    pub(super) finished: bool,
}

impl TimelineSample {
    pub(super) fn ease_out_cubic(self) -> f64 {
        1.0 - (1.0 - self.progress).powi(3)
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct AnimationTimeline {
    duration: Duration,
    frame_interval: Duration,
    started_at: Option<Instant>,
    next_frame: Option<Instant>,
}

impl AnimationTimeline {
    pub(super) const fn new(duration: Duration, frame_interval: Duration) -> Self {
        Self {
            duration,
            frame_interval,
            started_at: None,
            next_frame: None,
        }
    }

    pub(super) fn start(&mut self, now: Instant) {
        self.started_at = Some(now);
        self.next_frame = Some(now + self.frame_interval);
    }

    pub(super) fn stop(&mut self) {
        self.started_at = None;
        self.next_frame = None;
    }

    pub(super) const fn deadline(&self) -> Option<Instant> {
        self.next_frame
    }

    pub(super) fn sample(&self, now: Instant) -> Option<TimelineSample> {
        let started_at = self.started_at?;
        let elapsed = now.saturating_duration_since(started_at);
        let progress = if self.duration.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f64() / self.duration.as_secs_f64()).min(1.0)
        };
        Some(TimelineSample {
            progress,
            finished: progress >= 1.0,
        })
    }

    pub(super) fn advance(&mut self, now: Instant) -> Option<TimelineSample> {
        let sample = self.sample(now)?;
        if sample.finished {
            self.stop();
        } else {
            self.next_frame = Some(now + self.frame_interval);
        }
        Some(sample)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DeadlineSet(Option<Instant>);

impl DeadlineSet {
    pub(super) const fn new() -> Self {
        Self(None)
    }

    pub(super) fn include(&mut self, deadline: Option<Instant>) -> &mut Self {
        if let Some(deadline) = deadline {
            self.0 = Some(self.0.map_or(deadline, |current| current.min(deadline)));
        }
        self
    }

    pub(super) const fn earliest(self) -> Option<Instant> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::{AnimationTimeline, DeadlineSet};
    use std::time::{Duration, Instant};

    #[test]
    fn timeline_schedules_frames_and_finishes_without_catch_up() {
        let start = Instant::now();
        let mut timeline =
            AnimationTimeline::new(Duration::from_millis(100), Duration::from_millis(16));

        assert_eq!(timeline.deadline(), None);
        timeline.start(start);
        assert_eq!(timeline.deadline(), Some(start + Duration::from_millis(16)));

        let middle = timeline.advance(start + Duration::from_millis(50)).unwrap();
        assert_eq!(middle.progress, 0.5);
        assert!(!middle.finished);
        assert_eq!(timeline.deadline(), Some(start + Duration::from_millis(66)));

        let end = timeline
            .advance(start + Duration::from_millis(150))
            .unwrap();
        assert_eq!(end.progress, 1.0);
        assert!(end.finished);
        assert_eq!(timeline.deadline(), None);
    }

    #[test]
    fn sampling_does_not_change_the_deadline() {
        let start = Instant::now();
        let mut timeline =
            AnimationTimeline::new(Duration::from_millis(100), Duration::from_millis(16));
        timeline.start(start);
        let deadline = timeline.deadline();

        let sample = timeline.sample(start + Duration::from_millis(25)).unwrap();

        assert_eq!(sample.progress, 0.25);
        assert_eq!(timeline.deadline(), deadline);
    }

    #[test]
    fn deadline_set_returns_the_earliest_value() {
        let now = Instant::now();
        let mut deadlines = DeadlineSet::new();
        deadlines
            .include(Some(now + Duration::from_secs(2)))
            .include(None)
            .include(Some(now + Duration::from_secs(1)));

        assert_eq!(deadlines.earliest(), Some(now + Duration::from_secs(1)));
    }
}
