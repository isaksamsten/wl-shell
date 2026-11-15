//! Dragging icons: where the gap opens for the pointer, and what a drop
//! does. Pure, unit tested; view.rs shows the drag and app.rs applies the
//! drop to the pins.
//!
//! - Only pinned apps have an order to change. A pinned app moves the gap
//!   among the other pinned apps; past the last one, the gap stays at
//!   their end.
//! - A running app that isn't pinned, dragged among the pinned apps, gets
//!   pinned there. Past them (half an icon after the last), it is back
//!   among the running apps, and dropping it there changes nothing.
//! - On the Trash, a pinned app is unpinned. A running app that isn't
//!   pinned has nothing to remove, so the Trash doesn't take it.
//! - Off the dock, the item goes back to its place, and a drop there
//!   changes nothing: dragging off the dock never unpins.
//!
//! Positions are along the dock in the magnified layout as shown, gap
//! included. The gap opens where the pointer is, so the items around it
//! move away from the pointer and the choice holds still.

use crate::pins::{self, Pin};

/// The item being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dragged {
    /// A pinned app: the `i`th pinned app (from 0).
    Pinned(usize),
    /// A running app that isn't pinned.
    Running,
}

/// Where a drop would put the dragged item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Its own place: the drop changes nothing.
    Home,
    /// Among the pinned apps, before the `k`th of the others (from 0).
    Gap(usize),
    /// The Trash, which takes it: it is removed from the dock.
    Trash,
}

/// What a drop does to the pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// Pin the app as the `k`th pinned app (moving it if it is pinned).
    Pin(usize),
    Unpin,
    Nothing,
}

/// Where the pointer is relative to the dock, as shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scene<'a> {
    /// The centers of the pinned apps other than the dragged one, in
    /// order.
    pub pinned: &'a [f64],
    /// Where those end (the gap included, if it is after them), or where
    /// the row starts if there are none.
    pub pinned_end: f64,
    /// An icon and a gap, at rest.
    pub step: f64,
    /// The pointer is over the dock (else a drop cancels).
    pub on_dock: bool,
    /// The pointer is over the Trash.
    pub on_trash: bool,
}

/// Where a drop of `dragged` with the pointer at `u` would put it.
pub fn target(dragged: Dragged, s: &Scene, u: f64) -> Target {
    if !s.on_dock {
        return Target::Home;
    }
    if s.on_trash {
        return match dragged {
            Dragged::Pinned(_) => Target::Trash,
            Dragged::Running => Target::Home,
        };
    }
    let k = s.pinned.iter().filter(|&&c| c < u).count();
    match dragged {
        Dragged::Pinned(i) if i == k => Target::Home,
        Dragged::Pinned(_) => Target::Gap(k),
        Dragged::Running if k < s.pinned.len() || u < s.pinned_end + s.step / 2.0 => Target::Gap(k),
        Dragged::Running => Target::Home,
    }
}

/// Whether the Trash takes `dragged`.
pub fn trash_takes(dragged: Dragged) -> bool {
    matches!(dragged, Dragged::Pinned(_))
}

/// What dropping `dragged` on `target` does.
pub fn drop(dragged: Dragged, target: Target) -> Drop {
    match target {
        Target::Home => Drop::Nothing,
        Target::Gap(k) => Drop::Pin(k),
        Target::Trash if trash_takes(dragged) => Drop::Unpin,
        Target::Trash => Drop::Nothing,
    }
}

/// Apply `drop` of `app_id` to `pins`. Returns whether they changed.
pub fn apply(pins: &mut Vec<Pin>, app_id: &str, drop: Drop) -> bool {
    match drop {
        Drop::Pin(k) => pins::place_app(pins, app_id, k),
        Drop::Unpin => pins::unpin(pins, &Pin::App(app_id.to_string())),
        Drop::Nothing => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three other pinned apps centered at 100, 160 and 220, 60 apart.
    const PINNED: [f64; 3] = [100.0, 160.0, 220.0];

    fn scene() -> Scene<'static> {
        Scene {
            pinned: &PINNED,
            pinned_end: 250.0,
            step: 60.0,
            on_dock: true,
            on_trash: false,
        }
    }

    #[test]
    fn the_gap_follows_the_pointer_among_the_pinned() {
        let s = scene();
        let d = Dragged::Pinned(3);
        assert_eq!(target(d, &s, 50.0), Target::Gap(0));
        assert_eq!(target(d, &s, 130.0), Target::Gap(1));
        assert_eq!(target(d, &s, 190.0), Target::Gap(2));
        // Its own place: nothing moves.
        assert_eq!(target(d, &s, 240.0), Target::Home);
        assert_eq!(target(Dragged::Pinned(0), &s, 50.0), Target::Home);
        // Past the last one, the gap stays at their end.
        assert_eq!(target(Dragged::Pinned(0), &s, 900.0), Target::Gap(3));
    }

    #[test]
    fn running_apps_get_pinned_among_the_pinned() {
        let s = scene();
        let d = Dragged::Running;
        assert_eq!(target(d, &s, 50.0), Target::Gap(0));
        assert_eq!(target(d, &s, 230.0), Target::Gap(3));
        // Up to half an icon after the pinned apps.
        assert_eq!(target(d, &s, 270.0), Target::Gap(3));
        // Further, back among the running apps.
        assert_eq!(target(d, &s, 290.0), Target::Home);
        assert_eq!(drop(d, Target::Gap(3)), Drop::Pin(3));
        // Without pinned apps, the start of the row pins it.
        let none = Scene {
            pinned: &[],
            pinned_end: 40.0,
            ..s
        };
        assert_eq!(target(d, &none, 50.0), Target::Gap(0));
        assert_eq!(target(d, &none, 80.0), Target::Home);
    }

    #[test]
    fn the_trash_takes_pinned_apps_only() {
        let s = Scene {
            on_trash: true,
            ..scene()
        };
        assert_eq!(target(Dragged::Pinned(1), &s, 900.0), Target::Trash);
        assert_eq!(drop(Dragged::Pinned(1), Target::Trash), Drop::Unpin);
        assert_eq!(target(Dragged::Running, &s, 900.0), Target::Home);
        assert_eq!(drop(Dragged::Running, Target::Trash), Drop::Nothing);
        assert!(trash_takes(Dragged::Pinned(0)) && !trash_takes(Dragged::Running));
    }

    #[test]
    fn off_the_dock_it_goes_home() {
        let s = Scene {
            on_dock: false,
            on_trash: true,
            ..scene()
        };
        for d in [Dragged::Pinned(1), Dragged::Running] {
            for u in [0.0, 130.0, 900.0] {
                assert_eq!(target(d, &s, u), Target::Home);
            }
            assert_eq!(drop(d, Target::Home), Drop::Nothing);
        }
    }

    fn apps(text: &str) -> Vec<Pin> {
        pins::parse(text)
    }

    #[test]
    fn drops() {
        // Moving the third pinned app first.
        let mut p = apps("a\nb\nc\n");
        let k = match target(Dragged::Pinned(2), &scene(), 50.0) {
            Target::Gap(k) => k,
            t => panic!("{t:?}"),
        };
        assert!(apply(&mut p, "c", drop(Dragged::Pinned(2), Target::Gap(k))));
        assert_eq!(pins::apps(&p), ["c", "a", "b"]);
        // Pinning a running app between the first two.
        assert!(apply(&mut p, "d", Drop::Pin(1)));
        assert_eq!(pins::apps(&p), ["c", "d", "a", "b"]);
        // Removing one.
        assert!(apply(&mut p, "a", Drop::Unpin));
        assert_eq!(pins::apps(&p), ["c", "d", "b"]);
    }

    #[test]
    fn cancelling_leaves_the_pins() {
        let before = apps("a\n~/x\nb\n");
        let mut p = before.clone();
        for d in [Dragged::Pinned(0), Dragged::Running] {
            let off = Scene {
                on_dock: false,
                ..scene()
            };
            let t = target(d, &off, 130.0);
            assert!(!apply(&mut p, "a", drop(d, t)));
        }
        // A running app on the Trash: refused.
        assert!(!apply(&mut p, "z", drop(Dragged::Running, Target::Trash)));
        // A pinned app dropped where it was.
        let t = target(Dragged::Pinned(0), &scene(), 50.0);
        assert!(!apply(&mut p, "a", drop(Dragged::Pinned(0), t)));
        assert_eq!(p, before);
    }
}
