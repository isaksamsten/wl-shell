//! What time a frame animates to.
//!
//! Frame callbacks' timestamps step evenly from frame to frame; when a
//! callback gets handled doesn't (after whatever else the event loop had
//! to do first: capture frames, worker results, input). Animating by the
//! timestamps keeps a movement's steps even. Their base is undefined, so
//! they only count as steps, on the clock's timeline: a frame's time is
//! the last one's plus the step between their timestamps.
//!
//! The first frame of a movement, one after a gap, or one with an odd
//! timestamp goes by the clock instead. A frame's time is never later
//! than the clock, so things started by the clock (an animation's start
//! `Instant`) stay comparable with it.

use std::time::{Duration, Instant};

/// Timestamps further apart than this, ms, are a gap (or odd).
const MAX_STEP_MS: u32 = 250;

/// The time of the last frame while something moves.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameClock {
    /// The last frame's time, and its callback's timestamp (None when
    /// started rather than drawn).
    last: Option<(Instant, Option<u32>)>,
}

impl FrameClock {
    /// Something starts moving: the next frame measures from now. Nothing
    /// changes if it is running already.
    pub fn start(&mut self) {
        if self.last.is_none() {
            self.last = Some((Instant::now(), None));
        }
    }

    /// The next frame measures from now, running or not.
    pub fn restart(&mut self) {
        self.last = Some((Instant::now(), None));
    }

    /// Nothing moves anymore.
    pub fn stop(&mut self) {
        self.last = None;
    }

    pub fn running(&self) -> bool {
        self.last.is_some()
    }

    /// The time of the frame whose callback has `timestamp`.
    pub fn frame(&mut self, timestamp: u32) -> Instant {
        self.frame_at(timestamp, Instant::now())
    }

    /// Seconds since the last frame (or the start) for the frame whose
    /// callback has `timestamp`; 0 if not running. Call `stop` once
    /// nothing moves anymore.
    pub fn tick(&mut self, timestamp: u32) -> f64 {
        self.tick_at(timestamp, Instant::now())
    }

    fn frame_at(&mut self, timestamp: u32, now: Instant) -> Instant {
        let at = match self.last {
            Some((last, Some(prev))) => match timestamp.wrapping_sub(prev) {
                ms @ 1..=MAX_STEP_MS => (last + Duration::from_millis(ms.into())).min(now),
                _ => now,
            },
            _ => now,
        };
        self.last = Some((at, Some(timestamp)));
        at
    }

    fn tick_at(&mut self, timestamp: u32, now: Instant) -> f64 {
        let last = self.last.map(|(t, _)| t);
        let at = self.frame_at(timestamp, now);
        last.map_or(0.0, |l| at.saturating_duration_since(l).as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn started(at: Instant) -> FrameClock {
        FrameClock {
            last: Some((at, None)),
        }
    }

    #[test]
    fn not_running_ticks_zero() {
        let mut c = FrameClock::default();
        assert_eq!(c.tick_at(1000, Instant::now()), 0.0);
    }

    #[test]
    fn first_frame_goes_by_the_clock() {
        let t0 = Instant::now();
        let mut c = started(t0);
        assert_eq!(c.tick_at(5000, t0 + ms(20)), 0.020);
    }

    #[test]
    fn late_handling_keeps_steps_even() {
        let t0 = Instant::now();
        let mut c = started(t0);
        c.tick_at(1000, t0 + ms(16));
        // Handled 9 ms late, then on time again: still 16 ms steps.
        assert_eq!(c.tick_at(1016, t0 + ms(41)), 0.016);
        assert_eq!(c.tick_at(1032, t0 + ms(48)), 0.016);
    }

    #[test]
    fn never_later_than_the_clock() {
        let t0 = Instant::now();
        let mut c = started(t0);
        c.tick_at(1000, t0 + ms(16));
        // A step of 16 ms, handled 10 ms after the last frame.
        let at = c.frame_at(1016, t0 + ms(26));
        assert_eq!(at, t0 + ms(26));
    }

    #[test]
    fn gaps_and_odd_timestamps_go_by_the_clock() {
        let t0 = Instant::now();
        let mut c = started(t0);
        c.tick_at(1000, t0 + ms(16));
        assert_eq!(c.tick_at(1000, t0 + ms(30)), 0.014); // no step
        assert_eq!(c.tick_at(900, t0 + ms(40)), 0.010); // backwards
        assert_eq!(c.tick_at(2000, t0 + ms(60)), 0.020); // a gap
    }

    #[test]
    fn timestamps_wrap() {
        let t0 = Instant::now();
        let mut c = started(t0);
        c.tick_at(u32::MAX - 5, t0 + ms(16));
        assert_eq!(c.tick_at(10, t0 + ms(40)), 0.016);
    }

    #[test]
    fn stop_and_start() {
        let mut c = FrameClock::default();
        assert!(!c.running());
        c.start();
        assert!(c.running());
        c.stop();
        assert!(!c.running());
    }
}

