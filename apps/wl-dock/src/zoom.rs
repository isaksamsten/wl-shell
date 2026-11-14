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
//! Every item has a cell: its icon with half a gap on either side. Cells
//! are back to back, so icons never overlap. An item's `weight` (0 to 1)
//! scales its cell and icon, so items appearing or disappearing grow in
//! or shrink away while their neighbors slide.
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

/// One item as laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// The icon's center.
    pub center: f64,
    /// The icon's size, weight included.
    pub size: f64,
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

    /// The largest icon.
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

/// The resting row of items with `weights`, centered on `center`.
#[cfg(test)]
pub fn rest(weights: &[f64], center: f64, p: &Params) -> Row {
    layout(weights, center, center, &Params { hover: 0.0, ..*p })
}

/// The row of items with `weights`, centered on `center` at rest, with
/// the pointer at `pointer`.
pub fn layout(weights: &[f64], center: f64, pointer: f64, p: &Params) -> Row {
    let step = p.icon + p.gap;
    let rest: Vec<f64> = weights.iter().map(|w| w.max(0.0) * step).collect();
    let total: f64 = rest.iter().sum();
    let start = center - total / 2.0;
    let mut sizes = Vec::with_capacity(weights.len());
    let mut at = start;
    for (&len, &w) in rest.iter().zip(weights) {
        let c = at + len / 2.0;
        at += len;
        let d = (c - pointer).abs() / p.icon;
        let grow = (p.magnification - 1.0) * falloff(d, p.range) * p.hover;
        sizes.push((p.icon * (1.0 + grow), w.max(0.0)));
    }
    let cells: Vec<f64> = sizes.iter().map(|(s, w)| w * (s + p.gap)).collect();
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
    let cells = (sizes.iter().zip(&cells))
        .map(|(&(s, w), &len)| {
            let cell = Cell {
                center: at + len / 2.0,
                size: s * w,
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
}
