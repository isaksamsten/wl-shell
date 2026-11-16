//! Where the launcher's matches stand on the drum. Pure, unit tested.
//!
//! The dock is a drum lying along the bottom edge, and the matches ride
//! its face. A match at ring position `t` (0 the front, 1 the next, ...)
//! sits at the angle `φ = t · step`. Its depth is `d = (1 − cos φ) /
//! (1 − cos φmax)`, 0 at the front and 1 at the last slot, and it is
//! `1 − d · (1 − far)` times the front's size and a little dimmer. The
//! matches are stacked tightly above the front icon, in the same column:
//! the rise at `t` is the integral of the size from 0 to `t`, plus a small
//! gap, so neighbors are a gap apart however much they shrink. (The size
//! is concave in `t`, so the integral over a step is at least the mean of
//! its ends.) The slots don't depend on how many matches there are.
//!
//! The ring is closed: past the last used slot (`t` between n − 1 and n
//! for n matches), the drum's back runs over the top and down behind the
//! bar to the front. A match there fades out over the top, and the one
//! leaving the front sinks toward the bar as it fades. Both remain
//! stacked in front of the bar so reverse rolls slide up without a pop.
//!
//! Rolling is a spring on the roll `r`: match `i` is at `t = (i − r) mod
//! n`, and the selection is the one the roll is headed for.

/// The step between slots at most, and the angle of the last slot at
/// most (below 90°, so the size stays concave).
const STEP: f64 = 20.0 * std::f64::consts::PI / 180.0;
const MAX_ANGLE: f64 = 80.0 * std::f64::consts::PI / 180.0;
/// How much dimmer the farthest slot is.
const DIM: f64 = 0.3;
/// Space between neighbors, of the smaller one's size.
const GAP: f64 = 0.08;
/// The parts of the back stretch spent sinking into the bar and coming
/// over the top.
const SINK: f64 = 0.3;
const TOP: f64 = 0.3;
/// How deep a match sinks, and how high it comes over the top, in front
/// sizes.
const SINK_DEPTH: f64 = 0.6;
const RISE: f64 = 0.3;

/// The drum's geometry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drum {
    /// The front icon's size.
    pub front: f64,
    /// Slots on the drum, at least 1.
    pub slots: usize,
    /// The farthest slot's size, of the front's.
    pub far_scale: f64,
    step: f64,
    /// The size's shrink: `size = front · (1 − shrink · (1 − cos φ))`.
    shrink: f64,
}

/// Where a match is drawn, relative to the front icon.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placed {
    /// How far its center is above the front icon's.
    pub rise: f64,
    pub size: f64,
    pub alpha: f64,
    /// On the hidden return stretch, with no name beside the icon.
    pub behind: bool,
}

impl Drum {
    pub fn new(front: f64, slots: usize, far_scale: f64) -> Self {
        let slots = slots.max(1);
        let step = match slots {
            1 => STEP,
            n => STEP.min(MAX_ANGLE / (n - 1) as f64),
        };
        let max = 1.0 - ((slots - 1) as f64 * step).cos();
        let shrink = match max > 0.0 {
            true => (1.0 - far_scale) / max,
            false => 0.0,
        };
        Self {
            front,
            slots,
            far_scale,
            step,
            shrink,
        }
    }

    fn last(&self) -> f64 {
        (self.slots - 1) as f64
    }

    /// The depth at slot position `t`: 0 at the front, 1 at the last
    /// slot.
    pub fn depth(&self, t: f64) -> f64 {
        let max = 1.0 - (self.last() * self.step).cos();
        match max > 0.0 {
            true => (1.0 - (t * self.step).cos()) / max,
            false => 0.0,
        }
    }

    fn size(&self, t: f64) -> f64 {
        self.front * (1.0 - self.depth(t) * (1.0 - self.far_scale))
    }

    /// The rise at `t`: the size's integral from 0 to `t`, plus the gap.
    fn rise(&self, t: f64) -> f64 {
        let c = self.shrink;
        let integral = (1.0 - c) * t + c * (t * self.step).sin() / self.step;
        (1.0 + GAP) * self.front * integral
    }

    /// Slot position `t` (0 to `slots` − 1).
    pub fn slot(&self, t: f64) -> Placed {
        let t = t.clamp(0.0, self.last());
        Placed {
            rise: self.rise(t),
            size: self.size(t),
            alpha: 1.0 - DIM * self.depth(t),
            behind: false,
        }
    }

    // /// How far above the front icon's center the drum reaches.
    // pub fn height(&self) -> f64 {
    //     let last = self.slot(self.last());
    //     let over = self.front * RISE;
    //     last.rise + over + last.size / 2.0
    // }

    /// Ring position `t` (0 to `ring`) on a ring of `ring` matches (at
    /// most `slots`).
    pub fn place(&self, t: f64, ring: usize) -> Placed {
        let ring = ring.clamp(1, self.slots);
        if ring == 1 {
            // Only the front, whatever the roll.
            return self.slot(0.0);
        }
        let t = t.rem_euclid(ring as f64);
        let last = (ring - 1) as f64;
        if t <= last {
            return self.slot(t);
        }
        // The back: from the last slot (0) to the front (1).
        let b = t - last;
        if b >= 1.0 - SINK {
            // Sinking into the bar as it leaves the front.
            let u = (1.0 - b) / SINK;
            return Placed {
                rise: -u * SINK_DEPTH * self.front,
                size: self.front,
                alpha: 1.0 - u,
                behind: true,
            };
        }
        let top = self.slot(last);
        if b <= TOP {
            // Coming over the top into the last slot.
            let u = b / TOP;
            return Placed {
                rise: top.rise + u * RISE * self.front,
                size: top.size * (1.0 - 0.2 * u),
                alpha: top.alpha * (1.0 - u),
                behind: false,
            };
        }
        Placed {
            rise: 0.0,
            size: self.front,
            alpha: 0.0,
            behind: true,
        }
    }
}

/// The ring position of match `i` of `ring` with the drum rolled to
/// `roll`.
pub fn position(i: usize, roll: f64, ring: usize) -> f64 {
    (i as f64 - roll).rem_euclid(ring.max(1) as f64)
}

/// The match in front once the roll gets to `target`.
pub fn selected(target: f64, ring: usize) -> usize {
    (target.round() as i64).rem_euclid(ring.max(1) as i64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn front_and_far() {
        let d = Drum::new(60.0, 5, 0.75);
        let front = d.slot(0.0);
        assert_eq!((front.rise, front.size, front.alpha), (0.0, 60.0, 1.0));
        let far = d.slot(4.0);
        assert!(close(far.size, 45.0) && close(d.depth(4.0), 1.0));
        assert!(far.alpha < 1.0 && far.alpha > 0.5);
        // Higher, smaller and dimmer as they go around.
        for t in 0..4 {
            let (a, b) = (d.slot(t as f64), d.slot(t as f64 + 1.0));
            assert!(b.rise > a.rise && b.size < a.size && b.alpha < a.alpha);
        }
        // assert!(d.height() > far.rise);
    }

    #[test]
    fn neighbors_are_close_but_never_overlap() {
        for slots in 1..=12 {
            for far in [0.25, 0.5, 0.75, 1.0] {
                let d = Drum::new(48.0, slots, far);
                for t in 0..slots.saturating_sub(1) {
                    let (a, b) = (d.slot(t as f64), d.slot(t as f64 + 1.0));
                    let room = b.rise - a.rise - (a.size + b.size) / 2.0;
                    let gap = GAP * b.size;
                    assert!(room >= gap - 1e-9, "{slots} {far} {t}: {room}");
                    // The size is concave, so a little more over a big shrink.
                    assert!(room <= gap + 0.2 * 48.0, "{slots} {far} {t}: {room}");
                }
            }
        }
    }

    #[test]
    fn slots_dont_depend_on_the_matches() {
        let d = Drum::new(60.0, 5, 0.75);
        for t in [0.0, 0.5, 1.0, 2.0] {
            assert_eq!(d.place(t, 3), d.place(t, 5));
            assert_eq!(d.place(t, 5), d.slot(t));
        }
        // One match: only the front, whatever the roll.
        for t in [0.0, 0.3, 2.7] {
            assert_eq!(d.place(t, 1), d.slot(0.0));
        }
        // A ring can't use more slots than there are.
        assert_eq!(d.place(4.5, 9), d.place(4.5, 5));
    }

    #[test]
    fn the_ring_is_closed_and_continuous() {
        let d = Drum::new(60.0, 5, 0.75);
        for ring in 2..=5 {
            let steps = 10_000;
            let at = |k: usize| d.place(ring as f64 * k as f64 / steps as f64, ring);
            for k in 0..steps {
                let (a, b) = (at(k), at(k + 1));
                if a.alpha > 1e-6 && b.alpha > 1e-6 {
                    assert!((a.rise - b.rise).abs() < 1.0, "{ring} {k}: {a:?} {b:?}");
                    assert!((a.size - b.size).abs() < 1.0);
                }
                assert!((a.alpha - b.alpha).abs() < 0.01, "{ring} {k}: {a:?} {b:?}");
            }
            // Back around at the front.
            assert_eq!(at(steps), at(0));
            // Just before the front again, it is on the return stretch.
            let leaving = d.place(ring as f64 - 0.01, ring);
            assert!(leaving.behind && leaving.rise < 0.0);
            // Just past the last slot, it goes over the top.
            let over = d.place(ring as f64 - 1.0 + 0.01, ring);
            assert!(!over.behind && over.rise > d.slot(ring as f64 - 1.0).rise);
        }
    }

    #[test]
    fn rolling() {
        // Rolled one step toward you: the second is in front, the first
        // went around the back to the last slot.
        assert_eq!(position(1, 1.0, 5), 0.0);
        assert_eq!(position(0, 1.0, 5), 4.0);
        assert_eq!(position(3, -1.0, 5), 4.0);
        assert_eq!(position(0, 0.5, 3), 2.5);
        assert_eq!(selected(1.0, 5), 1);
        assert_eq!(selected(-1.0, 5), 4);
        assert_eq!(selected(7.0, 5), 2);
        assert_eq!(selected(3.0, 1), 0);
    }
}
