//! The dock's row as shown: the items, plus the ones on their way out, each
//! with a spring for how present it is. Pure, unit tested.
//!
//! When the items change, `merge` keeps the slots of items that stay (so
//! their springs carry on), adds new ones growing in from nothing, and
//! keeps the removed ones in place, shrinking away, until they are gone.

use wl_shell_ui::Spring;

use crate::dock::Item;

#[derive(Clone, Debug)]
pub struct Slot {
    pub item: Item,
    /// 0 (not there) to 1 (fully there).
    pub presence: Spring,
    /// No longer an item: it shrinks away and then goes.
    pub leaving: bool,
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

/// Advance the springs by `dt` seconds and drop the slots that are gone.
/// Returns whether any still moves.
pub fn advance(slots: &mut Vec<Slot>, dt: f64) -> bool {
    let mut moving = false;
    for s in slots.iter_mut() {
        moving |= s.presence.advance(dt);
    }
    slots.retain(|s| !(s.leaving && s.presence.settled()));
    moving
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::Window;

    fn item(app_id: &str) -> Item {
        Item {
            app_id: app_id.into(),
            windows: vec![Window {
                id: format!("{app_id}-1"),
                title: String::new(),
            }],
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
}
