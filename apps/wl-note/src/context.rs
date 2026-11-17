//! The context: the window the notes follow. Pure, unit tested.
//!
//! It is the most recently focused open window, as the `mru-event-stream`
//! says (see app.rs). A change only shows once it has held for `settle`,
//! so cycling through windows with Alt+Tab, or skimming through a
//! browser's tabs, doesn't make the notes flicker: [`Settle`] holds a
//! proposed value back until then, and drops it if the old one comes
//! back first.

use std::time::{Duration, Instant};

/// What a proposal (or a deadline passing) did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proposal {
    /// Nothing changes (any pending value is dropped).
    Same,
    /// The value changed.
    Changed,
    /// A value is pending until then: call `due` at that time.
    Wait(Instant),
}

/// A value that only changes once a new one has held for a while.
#[derive(Clone, Debug)]
pub struct Settle<T> {
    value: T,
    pending: Option<(T, Instant)>,
}

impl<T: PartialEq> Settle<T> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            pending: None,
        }
    }

    /// The value in effect.
    pub fn value(&self) -> &T {
        &self.value
    }

    /// `value` is what it should be from `now` on: it takes effect once
    /// it held for `settle` (at once if that's 0).
    pub fn propose(&mut self, value: T, now: Instant, settle: Duration) -> Proposal {
        if value == self.value {
            self.pending = None;
            return Proposal::Same;
        }
        if settle.is_zero() {
            self.value = value;
            self.pending = None;
            return Proposal::Changed;
        }
        if let Some((pending, at)) = &self.pending
            && *pending == value
        {
            return Proposal::Wait(*at);
        }
        let at = now + settle;
        self.pending = Some((value, at));
        Proposal::Wait(at)
    }

    /// At `now`: the pending value takes effect if it held long enough.
    pub fn due(&mut self, now: Instant) -> Proposal {
        match self.pending.take() {
            None => Proposal::Same,
            Some((value, at)) if now >= at => {
                self.value = value;
                Proposal::Changed
            }
            Some((value, at)) => {
                self.pending = Some((value, at));
                Proposal::Wait(at)
            }
        }
    }

    /// Take `value` at once, dropping anything pending.
    pub fn set(&mut self, value: T) {
        self.value = value;
        self.pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTLE: Duration = Duration::from_millis(120);

    fn ms(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn changes_once_held() {
        let t0 = Instant::now();
        let mut s = Settle::new("a");
        assert_eq!(s.propose("a", t0, SETTLE), Proposal::Same);
        assert_eq!(s.propose("b", t0, SETTLE), Proposal::Wait(ms(t0, 120)));
        assert_eq!(*s.value(), "a");
        // Proposed again, it keeps its deadline.
        assert_eq!(
            s.propose("b", ms(t0, 50), SETTLE),
            Proposal::Wait(ms(t0, 120))
        );
        assert_eq!(s.due(ms(t0, 100)), Proposal::Wait(ms(t0, 120)));
        assert_eq!(s.due(ms(t0, 120)), Proposal::Changed);
        assert_eq!(*s.value(), "b");
        assert_eq!(s.due(ms(t0, 200)), Proposal::Same);
    }

    #[test]
    fn cycling_back_changes_nothing() {
        // Alt+Tab from a over b and c, back to a.
        let t0 = Instant::now();
        let mut s = Settle::new("a");
        s.propose("b", t0, SETTLE);
        assert_eq!(
            s.propose("c", ms(t0, 40), SETTLE),
            Proposal::Wait(ms(t0, 160))
        );
        assert_eq!(s.propose("a", ms(t0, 80), SETTLE), Proposal::Same);
        assert_eq!(s.due(ms(t0, 200)), Proposal::Same);
        assert_eq!(*s.value(), "a");
    }

    #[test]
    fn skimming_restarts_the_wait() {
        let t0 = Instant::now();
        let mut s = Settle::new("tab 1");
        s.propose("tab 2", t0, SETTLE);
        s.propose("tab 3", ms(t0, 100), SETTLE);
        assert_eq!(s.due(ms(t0, 130)), Proposal::Wait(ms(t0, 220)));
        assert_eq!(s.due(ms(t0, 220)), Proposal::Changed);
        assert_eq!(*s.value(), "tab 3");
    }

    #[test]
    fn without_settling() {
        let mut s = Settle::new(1);
        assert_eq!(
            s.propose(2, Instant::now(), Duration::ZERO),
            Proposal::Changed
        );
        assert_eq!(*s.value(), 2);
        s.propose(3, Instant::now(), SETTLE);
        s.set(4);
        assert_eq!(s.due(Instant::now() + SETTLE), Proposal::Same);
        assert_eq!(*s.value(), 4);
    }
}
