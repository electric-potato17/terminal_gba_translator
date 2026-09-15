//! Drift-corrected native frame pacing.

use std::time::{Duration, Instant};

pub struct FramePacer {
    next_tick: Instant,
    interval: Duration,
}

impl FramePacer {
    pub fn new(hz: f64) -> Self {
        assert!(
            hz.is_finite() && hz > 0.0,
            "frame rate must be finite and positive"
        );
        let interval = Duration::from_secs_f64(1.0 / hz);
        Self {
            next_tick: Instant::now() + interval,
            interval,
        }
    }

    /// Sleep until the next scheduled frame, correcting for occasional slow
    /// frames without accumulating delay forever.
    pub fn sleep_until_next_frame(&mut self) {
        let now = Instant::now();
        if self.next_tick > now {
            std::thread::sleep(self.next_tick - now);
        }

        self.next_tick += self.interval;
        let now = Instant::now();
        if self.next_tick <= now {
            self.next_tick = now + self.interval;
        }
    }

    /// Drop the normal frame-rate limit while a frontend turbo modifier is
    /// held, without leaving a stale deadline to delay the next normal frame.
    pub fn skip_frame(&mut self) {
        self.next_tick = Instant::now() + self.interval;
    }
}

#[cfg(test)]
mod tests {
    use super::FramePacer;
    use std::time::Instant;

    #[test]
    fn rejects_invalid_rates() {
        for rate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(std::panic::catch_unwind(|| FramePacer::new(rate)).is_err());
        }
    }

    #[test]
    fn schedules_the_first_tick_one_interval_ahead() {
        let before = Instant::now();
        let pacer = FramePacer::new(60.0);
        let after = Instant::now();

        assert!(pacer.next_tick >= before);
        assert!(pacer.next_tick <= after + std::time::Duration::from_millis(20));
        assert!(pacer.interval > std::time::Duration::ZERO);
    }

    #[test]
    fn recovers_from_a_late_frame_without_sleeping_for_old_deadlines() {
        let mut pacer = FramePacer::new(60.0);
        pacer.next_tick = Instant::now() - std::time::Duration::from_secs(1);
        pacer.sleep_until_next_frame();

        assert!(pacer.next_tick > Instant::now());
    }
}
