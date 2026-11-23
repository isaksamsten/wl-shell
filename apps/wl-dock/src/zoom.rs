//! Magnification: the icons near the pointer grow together, on a smooth
//! curve. Pure, unit tested; everything is along the dock (`u`).
//!
//! For the item `i` whose center rests at `cᵢ`, with the pointer at `p`:
//!
//! ```text
//! d    = |cᵢ − p| / icon
//! f(d) = cos²(π·d / (2·range))      for d < range, else 0
//! sᵢ   = icon · (1 + (magnification − 1) · f(d) · hover)
//! ```
//!
//! Every slot has a cell: an item's icon with half a gap on either side,
//! or a fixed length that doesn't magnify (the separator). Cells are back
//! to back, so icons never overlap. A slot's `weight` (0 to 1) scales its
//! cell and icon, so items appearing or disappearing grow in or shrink
//! away while their neighbors slide.
//!
//! A widget's tile can be longer along the dock than an icon: `span`
//! icons (1, 1.5 or 2), as thick as an icon. Its cell is `size · span`
//! plus the gap, and its distance to the pointer is measured from the
//! nearer end of its middle stretch (where a square icon would have its
//! center): the whole tile is fully grown while the pointer is on that
//! stretch, and its neighbors grow as they would beside an icon.
//!
//! The magnified row is shifted so the point under the pointer stays under
//! the pointer: `p` falls in a resting cell at some fraction of its length,
//! and the same fraction of the magnified cell is put at `p`. Outside the
//! row the nearer end stays put. The layout is a function of `p` alone,
//! so it follows the pointer without lag; only `hover` animates.

use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    /// Icon size at rest.
    pub icon: f64,
    pub gap: f64,
    /// The hovered icon's scale.
    pub magnification: f64,
    /// Icons on each side that grow.
    pub range: f64,
    /// 0 (at rest) to 1 (magnified).
    pub hover: f64,
}

/// One slot of the row.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// 0 (not there) to 1 (fully there): scales the cell and the icon.
    pub weight: f64,
    /// The cell's length along the dock, instead of an icon's: it
    /// doesn't magnify, and has no icon.
    pub fixed: Option<f64>,
    /// The tile's length in icons (1 for an icon).
    pub span: f64,
}

impl Slot {
    /// A slot `len` long that doesn't magnify.
    pub fn fixed(len: f64, weight: f64) -> Self {
        Self {
            weight,
            fixed: Some(len),
            span: 1.0,
        }
    }

    /// A tile of `span` icons, of `weight`.
    pub fn wide(span: f64, weight: f64) -> Self {
        Self {
            weight,
            fixed: None,
            span: span.max(1.0),
        }
    }
}

impl From<f64> for Slot {
    /// An icon's slot of `weight`.
    fn from(weight: f64) -> Self {
        Self::wide(1.0, weight)
    }
}

/// One slot as laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// The icon's center.
    pub center: f64,
    /// The icon's size (across the dock, and along it for a square
    /// icon), weight included; 0 for a fixed slot.
    pub size: f64,
    /// Its length along the dock: `size` times the span.
    pub len: f64,
    /// The cell: the icon and half a gap on either side.
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The first cell's start and the last one's end.
    pub start: f64,
    pub end: f64,
}

impl Row {
    /// The item whose cell is at `u`.
    pub fn hit(&self, u: f64) -> Option<usize> {
        (self.cells.iter()).position(|c| c.end > c.start && u >= c.start && u < c.end)
    }

    /// The largest icon (across the dock).
    pub fn largest(&self) -> f64 {
        self.cells.iter().map(|c| c.size).fold(0.0, f64::max)
    }
}

/// How much an icon `d` icons from the pointer grows, 0 to 1.
pub fn falloff(d: f64, range: f64) -> f64 {
    if d >= range {
        return 0.0;
    }
    (PI * d / (2.0 * range)).cos().powi(2)
}

/// The resting row of `slots`, centered on `center`.
#[cfg(test)]
pub fn rest<S: Into<Slot> + Copy>(slots: &[S], center: f64, p: &Params) -> Row {
    layout(slots, center, center, &Params { hover: 0.0, ..*p })
}

/// The row of `slots` (icons' weights, or `Slot`s), centered on `center`
/// at rest, with the pointer at `pointer`.
pub fn layout<S: Into<Slot> + Copy>(slots: &[S], center: f64, pointer: f64, p: &Params) -> Row {
    let slots: Vec<Slot> = slots.iter().map(|&s| s.into()).collect();
    let rest: Vec<f64> = (slots.iter())
        .map(|s| {
            let len = s.fixed.unwrap_or(p.icon * s.span.max(1.0) + p.gap);
            s.weight.max(0.0) * len.max(0.0)
        })
        .collect();
    let total: f64 = rest.iter().sum();
    let start = center - total / 2.0;
    // Each slot's icon size (0 for fixed ones), tile length and
    // magnified cell.
    let mut sizes = Vec::with_capacity(slots.len());
    let mut lens = Vec::with_capacity(slots.len());
    let mut cells = Vec::with_capacity(slots.len());
    let mut at = start;
    for (&len, s) in rest.iter().zip(&slots) {
        let c = at + len / 2.0;
        at += len;
        let w = s.weight.max(0.0);
        if s.fixed.is_some() {
            sizes.push(0.0);
            lens.push(0.0);
            cells.push(len);
            continue;
        }
        let span = s.span.max(1.0);
        // From the nearer end of the tile's middle stretch.
        let stretch = (span - 1.0) * p.icon / 2.0;
        let d = ((c - pointer).abs() - stretch * w).max(0.0) / p.icon;
        let grow = (p.magnification - 1.0) * falloff(d, p.range) * p.hover;
        let size = p.icon * (1.0 + grow);
        sizes.push(size * w);
        lens.push(size * span * w);
        cells.push(w * (size * span + p.gap));
    }
    let magnified: f64 = cells.iter().sum();

    // Where the pointer's resting point lands in the magnified row,
    // relative to the row's start.
    let rel = pointer - start;
    let mapped = if rel <= 0.0 {
        rel
    } else if rel >= total {
        magnified + (rel - total)
    } else {
        let (mut r0, mut m0) = (0.0, 0.0);
        let mut mapped = magnified;
        for (&len, &cell) in rest.iter().zip(&cells) {
            if len > 0.0 && rel < r0 + len {
                mapped = m0 + (rel - r0) / len * cell;
                break;
            }
            r0 += len;
            m0 += cell;
        }
        mapped
    };
    let start = pointer - mapped;

    let mut at = start;
    let cells = (sizes.iter().zip(&lens).zip(&cells))
        .map(|((&size, &tile), &len)| {
            let cell = Cell {
                center: at + len / 2.0,
                size,
                len: tile,
                start: at,
                end: at + len,
            };
            at += len;
            cell
        })
        .collect();
    Row {
        cells,
        start,
        end: at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Params = Params {
        icon: 48.0,
        gap: 6.0,
        magnification: 2.0,
        range: 3.0,
        hover: 1.0,
    };

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn falls_off_smoothly() {
        assert_eq!(falloff(0.0, 3.0), 1.0);
        assert!(close(falloff(1.5, 3.0), 0.5));
        assert_eq!(falloff(3.0, 3.0), 0.0);
        assert_eq!(falloff(5.0, 3.0), 0.0);
    }

    #[test]
    fn rests_when_not_hovered() {
        let w = [1.0; 5];
        let rest = rest(&w, 500.0, &P);
        assert!(close(rest.start, 500.0 - 2.5 * 54.0));
        assert!(close(rest.cells[2].center, 500.0));
        for c in &rest.cells {
            assert_eq!(c.size, 48.0);
        }
        for p in [0.0, 400.0, 517.0, 2000.0] {
            let row = layout(&w, 500.0, p, &Params { hover: 0.0, ..P });
            assert_eq!(row, rest);
        }
    }

    #[test]
    fn magnification_one_changes_nothing() {
        let w = [1.0; 4];
        let rest = rest(&w, 300.0, &P);
        for p in [100.0, 250.0, 301.5] {
            let row = layout(
                &w,
                300.0,
                p,
                &Params {
                    magnification: 1.0,
                    ..P
                },
            );
            for (a, b) in row.cells.iter().zip(&rest.cells) {
                assert!(close(a.center, b.center) && close(a.size, b.size));
            }
        }
    }

    #[test]
    fn hovered_icon_stays_under_the_pointer() {
        let w = [1.0; 6];
        let rest = rest(&w, 500.0, &P);
        for (i, c) in rest.cells.iter().enumerate() {
            let row = layout(&w, 500.0, c.center, &P);
            assert!(close(row.cells[i].center, c.center), "{i}");
            assert!(close(row.cells[i].size, 96.0));
            assert_eq!(row.hit(c.center), Some(i));
        }
    }

    #[test]
    fn continuous_in_the_pointer() {
        let w = [1.0, 0.5, 1.0, 1.0, 0.0, 1.0];
        let mut last = layout(&w, 500.0, 0.0, &P);
        let mut p = 0.0;
        while p < 1000.0 {
            p += 0.25;
            let row = layout(&w, 500.0, p, &P);
            for (a, b) in row.cells.iter().zip(&last.cells) {
                assert!((a.center - b.center).abs() < 1.0, "p = {p}");
                assert!((a.size - b.size).abs() < 1.0, "p = {p}");
            }
            last = row;
        }
    }

    #[test]
    fn icons_never_overlap() {
        let w = [1.0, 1.0, 0.3, 1.0, 1.0, 1.0, 1.0];
        let mut p = 0.0;
        while p < 1000.0 {
            let row = layout(&w, 500.0, p, &P);
            for pair in row.cells.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                assert!(a.center + a.size / 2.0 <= b.center - b.size / 2.0 + 1e-9);
                assert!(close(a.end, b.start));
            }
            p += 7.0;
        }
    }

    #[test]
    fn symmetric_for_a_centered_pointer() {
        for n in [4, 5] {
            let w = vec![1.0; n];
            let row = layout(&w, 500.0, 500.0, &P);
            assert!(close(row.start + row.end, 1000.0), "{n}");
            for i in 0..n {
                let (a, b) = (row.cells[i], row.cells[n - 1 - i]);
                assert!(close(a.center - 500.0, 500.0 - b.center));
                assert!(close(a.size, b.size));
            }
        }
    }

    #[test]
    fn grows_outward_at_the_ends() {
        let w = [1.0; 4];
        let rest = rest(&w, 500.0, &P);
        // Beyond the end, the end stays put and the row grows inward of
        // the pointer, outward of its resting extent.
        let row = layout(&w, 500.0, rest.end + 10.0, &P);
        assert!(close(row.end, rest.end));
        assert!(row.start < rest.start);
        let row = layout(&w, 500.0, rest.start - 10.0, &P);
        assert!(close(row.start, rest.start));
        assert!(row.end > rest.end);
    }

    #[test]
    fn weights_scale_cells() {
        let row = rest(&[1.0, 0.0, 1.0], 0.0, &P);
        assert_eq!(row.cells[1].size, 0.0);
        assert!(close(row.end - row.start, 2.0 * 54.0));
        assert_eq!(row.hit(row.cells[1].center), Some(2));
        let half = rest(&[1.0, 0.5], 0.0, &P);
        assert_eq!(half.cells[1].size, 24.0);
        assert!(close(half.cells[1].end - half.cells[1].start, 27.0));
    }

    /// Four icons with a 20 px fixed slot (weight `w`) after the second.
    fn with_separator(w: f64) -> Vec<Slot> {
        let mut s: Vec<Slot> = [1.0; 4].iter().map(|&w| Slot::from(w)).collect();
        s.insert(2, Slot::fixed(20.0, w));
        s
    }

    #[test]
    fn fixed_slots_keep_their_length() {
        let s = with_separator(1.0);
        let rest = rest(&s, 500.0, &P);
        assert!(close(rest.end - rest.start, 4.0 * 54.0 + 20.0));
        assert!(close(rest.cells[2].center, 500.0));
        // Hovered right on it, or next to it: it doesn't grow, its
        // neighbors do.
        for p in [500.0, rest.cells[1].center, rest.cells[3].center] {
            let row = layout(&s, 500.0, p, &P);
            let sep = row.cells[2];
            assert!(close(sep.end - sep.start, 20.0), "{p}");
            assert_eq!(sep.size, 0.0);
            assert!(row.cells[1].size > 48.0 && row.cells[3].size > 48.0);
        }
        // Its weight scales it like any other.
        let half = super::rest(&with_separator(0.5), 0.0, &P);
        assert!(close(half.cells[2].end - half.cells[2].start, 10.0));
    }

    #[test]
    fn continuous_with_a_fixed_slot() {
        let s = with_separator(1.0);
        let mut last = layout(&s, 500.0, 0.0, &P);
        let mut p = 0.0;
        while p < 1000.0 {
            p += 0.25;
            let row = layout(&s, 500.0, p, &P);
            for (a, b) in row.cells.iter().zip(&last.cells) {
                assert!((a.center - b.center).abs() < 1.0, "p = {p}");
            }
            for pair in row.cells.windows(2) {
                assert!(close(pair[0].end, pair[1].start));
            }
            last = row;
        }
    }

    /// Icons, a 2x tile and a 1.5x tile.
    fn wide() -> Vec<Slot> {
        vec![
            Slot::from(1.0),
            Slot::wide(2.0, 1.0),
            Slot::from(1.0),
            Slot::wide(1.5, 1.0),
            Slot::from(1.0),
        ]
    }

    #[test]
    fn wide_tiles_are_longer() {
        let rest = rest(&wide(), 500.0, &P);
        assert!(close(rest.end - rest.start, 5.0 * 54.0 + 48.0 + 24.0));
        let tile = rest.cells[1];
        assert!(close(tile.len, 96.0) && close(tile.size, 48.0));
        assert!(close(tile.end - tile.start, 102.0));
        assert!(close(rest.cells[3].len, 72.0));
        // Square icons are as long as they are thick.
        assert!(close(rest.cells[0].len, rest.cells[0].size));
    }

    #[test]
    fn wide_tiles_grow_whole_along_their_middle() {
        let rest = rest(&wide(), 500.0, &P);
        let tile = rest.cells[1];
        // Anywhere on its middle stretch, it is fully magnified and stays
        // under the pointer.
        for f in [-0.5, -0.25, 0.0, 0.25, 0.5] {
            let p = tile.center + f * 48.0;
            let row = layout(&wide(), 500.0, p, &P);
            assert!(close(row.cells[1].size, 96.0), "{f}");
            assert!(close(row.cells[1].len, 192.0), "{f}");
            assert_eq!(row.hit(p), Some(1));
        }
        // Its neighbor next to its end grows as it would beside an icon
        // whose center is there.
        let at_end = tile.center + 24.0;
        let row = layout(&wide(), 500.0, at_end, &P);
        let icons = layout(&[1.0; 5], 500.0, 500.0, &P);
        assert!(close(row.cells[2].size, icons.cells[3].size));
    }

    #[test]
    fn continuous_and_apart_with_wide_tiles() {
        let s = wide();
        let mut last = layout(&s, 500.0, 0.0, &P);
        let mut p = 0.0;
        while p < 1000.0 {
            p += 0.25;
            let row = layout(&s, 500.0, p, &P);
            for (a, b) in row.cells.iter().zip(&last.cells) {
                assert!((a.center - b.center).abs() < 1.0, "p = {p}");
                assert!((a.len - b.len).abs() < 1.0, "p = {p}");
            }
            for pair in row.cells.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                assert!(a.center + a.len / 2.0 <= b.center - b.len / 2.0 + 1e-9);
                assert!(close(a.end, b.start));
            }
            last = row;
        }
    }

    #[test]
    fn spans_change_smoothly_with_their_weight() {
        // A span spring halfway from 1 to 2: in between.
        let row = |span| rest(&[Slot::from(1.0), Slot::wide(span, 1.0)], 0.0, &P);
        let (a, b, c) = (row(1.0), row(1.5), row(2.0));
        assert!(a.cells[1].len < b.cells[1].len && b.cells[1].len < c.cells[1].len);
        assert!(close(b.end - b.start, 2.0 * 54.0 + 24.0));
    }
}
