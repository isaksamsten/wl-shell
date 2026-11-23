//! The dock's row as shown: the items, plus the ones on their way out, each
//! with a spring for how present it is. Pure, unit tested.
//!
//! When the items change, `merge` keeps the slots of items that stay (so
//! their springs carry on), adds new ones growing in from nothing, and
//! keeps the removed ones in place, shrinking away, until they are gone.
//!
//! Each slot also has a spring for its span (a widget's tile's length
//! along the dock), so a span change slides its neighbors instead of
//! jumping.
//!
//! A slot whose app is launching bounces (`Bounce`): hop after hop until
//! the launch ends, then it finishes the hop it's in and lands.

use wl_shell_ui::Spring;

use crate::dock::Item;

#[derive(Clone, Debug)]
pub struct Slot {
    pub item: Item,
    /// 0 (not there) to 1 (fully there).
    pub presence: Spring,
    /// No longer an item: it shrinks away and then goes.
    pub leaving: bool,
    pub bounce: Option<Bounce>,
    /// Its span (1 to 2 icons), headed for the item's.
    pub span: Spring,
}

/// One hop, seconds.
const HOP: f64 = 0.6;

/// A launching app's icon hopping up and down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounce {
    /// Hops so far, fractional.
    t: f64,
    /// Lands when `t` gets here: the end of the hop it was in when the
    /// launch ended.
    land: Option<f64>,
}

impl Default for Bounce {
    fn default() -> Self {
        Self::new()
    }
}

impl Bounce {
    pub fn new() -> Self {
        Self { t: 0.0, land: None }
    }

    /// Land at the end of this hop.
    pub fn stop(&mut self) {
        if self.land.is_none() {
            self.land = Some(self.t.ceil());
        }
    }

    /// Keep hopping after all.
    pub fn resume(&mut self) {
        self.land = None;
    }

    #[cfg(test)]
    pub fn stopping(&self) -> bool {
        self.land.is_some()
    }

    /// Advance by `dt` seconds. Returns whether it is still in the air.
    pub fn advance(&mut self, dt: f64) -> bool {
        self.t += dt.max(0.0) / HOP;
        match self.land {
            Some(land) if self.t >= land => {
                self.t = land;
                false
            }
            _ => true,
        }
    }

    /// How high it is, 0 (on the dock) to 1 (the top of a hop): a
    /// parabola per hop, so it falls like a thrown thing.
    pub fn height(&self) -> f64 {
        let f = self.t.fract();
        4.0 * f * (1.0 - f)
    }
}

/// Update `slots` to show `items`. New slots grow in with springs of
/// `response` ms if `animate`, else they are there at once.
pub fn merge(slots: &mut Vec<Slot>, items: &[Item], response: f64, animate: bool) {
    let old = std::mem::take(slots);
    let mut out: Vec<Slot> = (items.iter())
        .map(
            |item| match old.iter().find(|s| s.item.app_id == item.app_id) {
                Some(s) => {
                    let mut s = s.clone();
                    s.item = item.clone();
                    s.leaving = false;
                    s.presence.retarget(1.0);
                    match animate {
                        true => s.span.retarget(item.span.get()),
                        false => s.span.snap(item.span.get()),
                    }
                    s
                }
                None => {
                    let mut presence = Spring::new(0.0, response);
                    match animate {
                        true => presence.retarget(1.0),
                        false => presence.snap(1.0),
                    }
                    Slot {
                        item: item.clone(),
                        presence,
                        leaving: false,
                        bounce: None,
                        span: Spring::new(item.span.get(), response),
                    }
                }
            },
        )
        .collect();
    // Removed items stay after the slot they followed.
    for (k, slot) in old.iter().enumerate() {
        if items.iter().any(|i| i.app_id == slot.item.app_id) || !animate {
            continue;
        }
        let at = (old[..k].iter().rev())
            .find_map(|prev| out.iter().position(|o| o.item.app_id == prev.item.app_id))
            .map_or(0, |i| i + 1);
        let mut slot = slot.clone();
        slot.leaving = true;
        slot.presence.retarget(0.0);
        out.insert(at, slot);
    }
    *slots = out;
}

/// Bounce the slots of the apps in `launching`; the others land.
pub fn set_launching(slots: &mut [Slot], launching: &[String]) {
    for s in slots.iter_mut() {
        let wanted = !s.leaving && launching.contains(&s.item.app_id);
        match (&mut s.bounce, wanted) {
            (None, true) => s.bounce = Some(Bounce::new()),
            (Some(b), true) => b.resume(),
            (Some(b), false) => b.stop(),
            (None, false) => {}
        }
    }
}

/// Advance the springs and bounces by `dt` seconds and drop the slots
/// that are gone. Returns whether any still moves.
pub fn advance(slots: &mut Vec<Slot>, dt: f64) -> bool {
    let mut moving = false;
    for s in slots.iter_mut() {
        moving |= s.presence.advance(dt);
        moving |= s.span.advance(dt);
        if let Some(b) = &mut s.bounce {
            match b.advance(dt) {
                true => moving = true,
                false => s.bounce = None,
            }
        }
    }
    slots.retain(|s| !(s.leaving && s.presence.settled()));
    moving
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{Kind, Span, Window};

    fn item(app_id: &str) -> Item {
        Item {
            app_id: app_id.into(),
            kind: Kind::App,
            pinned: false,
            windows: vec![Window {
                id: format!("{app_id}-1"),
                title: String::new(),
            }],
            elsewhere: Vec::new(),
            active: false,
            span: Span::ONE,
        }
    }

    fn ids(slots: &[Slot]) -> Vec<(&str, bool)> {
        (slots.iter())
            .map(|s| (s.item.app_id.as_str(), s.leaving))
            .collect()
    }

    #[test]
    fn first_fill_is_at_rest() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a"), item("b")], 100.0, false);
        assert_eq!(ids(&slots), [("a", false), ("b", false)]);
        assert!(
            slots
                .iter()
                .all(|s| s.presence.settled() && s.presence.value == 1.0)
        );
    }

    #[test]
    fn new_items_grow_in() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a")], 100.0, false);
        merge(&mut slots, &[item("a"), item("b")], 100.0, true);
        assert_eq!(slots[1].presence.value, 0.0);
        assert_eq!(slots[1].presence.target, 1.0);
        assert!(advance(&mut slots, 0.05));
        assert!(!advance(&mut slots, 1.0));
        assert_eq!(slots[1].presence.value, 1.0);
    }

    #[test]
    fn removed_items_shrink_in_place_then_go() {
        let mut slots = Vec::new();
        let all = [item("a"), item("b"), item("c"), item("d")];
        merge(&mut slots, &all, 100.0, false);
        merge(&mut slots, &[item("a"), item("d")], 100.0, true);
        assert_eq!(
            ids(&slots),
            [("a", false), ("b", true), ("c", true), ("d", false)]
        );
        advance(&mut slots, 0.05);
        assert_eq!(slots.len(), 4);
        advance(&mut slots, 1.0);
        assert_eq!(ids(&slots), [("a", false), ("d", false)]);
    }

    #[test]
    fn removing_the_first_keeps_it_first() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a"), item("b")], 100.0, false);
        merge(&mut slots, &[item("b")], 100.0, true);
        assert_eq!(ids(&slots), [("a", true), ("b", false)]);
    }

    #[test]
    fn coming_back_while_leaving() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a"), item("b")], 100.0, false);
        merge(&mut slots, &[item("a")], 100.0, true);
        advance(&mut slots, 0.03);
        let value = slots[1].presence.value;
        assert!(value < 1.0);
        merge(&mut slots, &[item("a"), item("b")], 100.0, true);
        assert_eq!(ids(&slots), [("a", false), ("b", false)]);
        // It turns around from where it was.
        assert_eq!(slots[1].presence.value, value);
        assert_eq!(slots[1].presence.target, 1.0);
    }

    #[test]
    fn windows_update_in_place() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a")], 100.0, false);
        let mut a = item("a");
        a.windows.push(Window {
            id: "a-2".into(),
            title: String::new(),
        });
        merge(&mut slots, &[a], 100.0, true);
        assert_eq!(slots[0].item.windows.len(), 2);
        assert!(slots[0].presence.settled());
    }

    #[test]
    fn spans_slide() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a")], 100.0, false);
        assert_eq!(slots[0].span.value, 1.0);
        let mut wide = item("a");
        wide.span = Span::of(2.0);
        merge(&mut slots, &[wide.clone()], 100.0, true);
        assert_eq!((slots[0].span.value, slots[0].span.target), (1.0, 2.0));
        assert!(advance(&mut slots, 0.05));
        let mid = slots[0].span.value;
        assert!(mid > 1.0 && mid < 2.0);
        assert!(!advance(&mut slots, 1.0));
        assert_eq!(slots[0].span.value, 2.0);
        // New wide items come in at their span.
        merge(
            &mut slots,
            &[wide, {
                let mut b = item("b");
                b.span = Span::of(1.5);
                b
            }],
            100.0,
            true,
        );
        assert_eq!(slots[1].span.value, 1.5);
    }

    #[test]
    fn bounces_until_stopped_then_lands() {
        let mut b = Bounce::new();
        assert_eq!(b.height(), 0.0);
        assert!(b.advance(HOP / 2.0));
        assert!((b.height() - 1.0).abs() < 1e-9);
        // Many hops later, still going.
        assert!(b.advance(HOP * 3.0));
        b.stop();
        assert!(b.stopping());
        // It finishes the hop it's in.
        assert!(b.advance(HOP / 4.0));
        assert!(b.height() > 0.0);
        assert!(!b.advance(HOP));
        assert_eq!(b.height(), 0.0);
    }

    #[test]
    fn launching_slots_bounce() {
        let mut slots = Vec::new();
        merge(&mut slots, &[item("a"), item("b")], 100.0, false);
        set_launching(&mut slots, &["b".into()]);
        assert!(slots[0].bounce.is_none() && slots[1].bounce.is_some());
        assert!(advance(&mut slots, HOP / 2.0));
        set_launching(&mut slots, &[]);
        assert!(slots[1].bounce.unwrap().stopping());
        assert!(!advance(&mut slots, HOP));
        assert!(slots[1].bounce.is_none());
    }
}
