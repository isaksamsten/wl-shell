//! Reordering the column by dragging a margin along it. Pure, unit tested.
//!
//! While a margin is dragged, a gap opens in the column where it would
//! land (`gap`): the slot nearest the pointer, among the slots of the
//! group it lands in (deck.rs picks the group). The other notes move
//! apart to leave that slot free. A drop moves the note in the whole
//! order (`land`) among the notes of that group: just before the next
//! one shown, or right after the previous one at the group's end, so the
//! notes this context doesn't show, and the other group's, keep their
//! places. A note from the other group isn't among them yet.

use std::path::{Path, PathBuf};

/// Where a note dragged to `y` along the edge lands: the index of the
/// slot nearest it among `slots` (the column's slot centers, in order).
pub fn gap(slots: &[f64], y: f64) -> usize {
    (slots.iter().enumerate())
        .min_by(|(_, a), (_, b)| (*a - y).abs().total_cmp(&(*b - y).abs()))
        .map_or(0, |(i, _)| i)
}

/// The whole order `full` after `key` was dropped at slot `gap` of the
/// group `shown` (in order; `key` included unless it comes from the other
/// group).
pub fn land(full: &[PathBuf], shown: &[&Path], key: &Path, gap: usize) -> Vec<PathBuf> {
    let others: Vec<&Path> = shown.iter().copied().filter(|p| *p != key).collect();
    // Where it was (or alone): nothing moves.
    let mut after = others.clone();
    after.insert(gap.min(others.len()), key);
    if after == shown && full.iter().any(|p| p == key) {
        return full.to_vec();
    }
    let mut out: Vec<PathBuf> = full.iter().filter(|p| *p != key).cloned().collect();
    let at = |p: &Path, out: &[PathBuf]| out.iter().position(|q| q == p);
    let i = match others.get(gap) {
        Some(next) => at(next, &out),
        None => others.last().and_then(|prev| at(prev, &out)).map(|i| i + 1),
    };
    let i = i.unwrap_or(out.len());
    out.insert(i, key.to_path_buf());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    fn refs(v: &[PathBuf]) -> Vec<&Path> {
        v.iter().map(PathBuf::as_path).collect()
    }

    #[test]
    fn the_gap_follows_the_pointer() {
        let slots = [100.0, 170.0, 240.0, 310.0];
        assert_eq!(gap(&slots, 0.0), 0);
        assert_eq!(gap(&slots, 130.0), 0);
        assert_eq!(gap(&slots, 140.0), 1);
        assert_eq!(gap(&slots, 250.0), 2);
        assert_eq!(gap(&slots, 1000.0), 3);
        assert_eq!(gap(&[], 10.0), 0);
    }

    #[test]
    fn dropping_keeps_the_notes_not_shown_in_place() {
        // x and y aren't shown in this context.
        let full = paths(&["a", "x", "b", "c", "y", "d"]);
        let shown = paths(&["a", "b", "c", "d"]);
        let shown = refs(&shown);
        let key = Path::new("d");
        // To the top: before a.
        assert_eq!(
            land(&full, &shown, key, 0),
            paths(&["d", "a", "x", "b", "c", "y"])
        );
        // Between a and b: just before b, so after x.
        assert_eq!(
            land(&full, &shown, key, 1),
            paths(&["a", "x", "d", "b", "c", "y"])
        );
        // Where it was: nothing moves.
        assert_eq!(land(&full, &shown, key, 3), full);
        // A down to the end: right after d, before nothing else.
        let key = Path::new("a");
        assert_eq!(
            land(&full, &shown, key, 3),
            paths(&["x", "b", "c", "y", "d", "a"])
        );
        // Down one: after b.
        assert_eq!(
            land(&full, &shown, key, 1),
            paths(&["x", "b", "a", "c", "y", "d"])
        );
    }

    #[test]
    fn dropping_alone_or_unknown() {
        let full = paths(&["a", "b"]);
        let one = paths(&["a"]);
        assert_eq!(land(&full, &refs(&one), Path::new("a"), 0), full);
        // A note not in the order yet goes in.
        let shown = paths(&["a", "n"]);
        assert_eq!(
            land(&full, &refs(&shown), Path::new("n"), 0),
            paths(&["n", "a", "b"])
        );
    }

    #[test]
    fn dropping_within_a_group_or_into_it() {
        // The window's w1 comes first in the order, the global g1 and g2
        // after it; the column shows g1, g2, w1.
        let full = paths(&["w1", "g1", "g2", "w2"]);
        let globals = paths(&["g1", "g2"]);
        // g1 to the end of its group: after g2, not before w1.
        assert_eq!(
            land(&full, &refs(&globals), Path::new("g1"), 1),
            paths(&["w1", "g2", "g1", "w2"])
        );
        // w1 into the global group, between them.
        assert_eq!(
            land(&full, &refs(&globals), Path::new("w1"), 1),
            paths(&["g1", "w1", "g2", "w2"])
        );
        // Or at its end: after g2.
        assert_eq!(
            land(&full, &refs(&globals), Path::new("w1"), 2),
            paths(&["g1", "g2", "w1", "w2"])
        );
    }
}
