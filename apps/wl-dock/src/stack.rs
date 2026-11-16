//! A folder's stack (`view = "stack"`): its entries as large icons with
//! their names in pills, in a column out of the folder's icon. Pure, unit
//! tested; menuview.rs shows it, with the folder popup's rows.
//!
//! Above a bottom dock the column rises from the icon, the newest entry
//! nearest the dock and Open in File Manager on top, and fans out: it
//! bends to the right along a circle of `FAN_RADIUS`, the names on the
//! icons' left. Beside a left or right dock it is a straight column
//! centered on the icon, newest first, with the names on the side away
//! from the dock. As many entries are shown as fit in the room the output
//! leaves (`fits`); Open in File Manager is always there.
//!
//! The layout is in logical pixels on the popup's surface, whole pixels
//! for the icons and pills. Each item's `hit` reaches halfway to its
//! neighbors, so moving along the stack never passes over nothing.

use wl_shell_core::popup::{Anchor, Gravity};
use wl_shell_ui::Rect;

use crate::config::Position;

/// The circle the fan bends along.
pub const FAN_RADIUS: f64 = 1500.0;
/// Between the stack and the folder's icon.
pub const DOCK_GAP: f64 = 6.0;
/// Between items.
pub const ITEM_GAP: f64 = 6.0;
/// Between an icon and its name.
pub const LABEL_GAP: f64 = 8.0;
/// Around a name in its pill.
pub const LABEL_PAD_X: f64 = 9.0;
pub const LABEL_PAD_Y: f64 = 3.0;
/// The widest a name's text gets.
pub const TEXT_MAX: f64 = 220.0;
/// Around everything, so nothing is cut at the surface's edge.
pub const MARGIN: f64 = 2.0;

/// Sizes the stack is laid out with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// An entry's icon.
    pub icon: f64,
    /// A line of text.
    pub text_h: f64,
    /// How far an icon's shadow reaches past it.
    pub shadow: f64,
}

impl Metrics {
    /// A name's pill height.
    pub fn label_h(&self) -> f64 {
        (self.text_h + 2.0 * LABEL_PAD_Y).ceil()
    }

    /// An item's height (across the column).
    fn row(&self) -> f64 {
        self.icon.max(self.label_h())
    }

    /// From one item to the next.
    fn pitch(&self) -> f64 {
        self.row() + ITEM_GAP
    }

    /// Around everything: room for the icons' shadows.
    fn margin(&self) -> f64 {
        MARGIN.max(self.shadow.ceil())
    }
}

/// How many items fit in `room` logical pixels along the column; at least
/// one.
pub fn fits(m: &Metrics, room: f64) -> usize {
    let n = (room - 2.0 * m.margin() + ITEM_GAP) / m.pitch();
    n.floor().max(1.0) as usize
}

/// How far the fan has bent `d` along the column.
fn bend(d: f64) -> f64 {
    let d = d.clamp(0.0, FAN_RADIUS);
    FAN_RADIUS - (FAN_RADIUS * FAN_RADIUS - d * d).sqrt()
}

/// One entry on the surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    pub icon: Rect,
    /// The name's pill.
    pub label: Rect,
    /// Where the pointer is on it.
    pub hit: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub position: Position,
    /// The surface's size (whole pixels).
    pub w: f64,
    pub h: f64,
    /// Between the surface's edges and the items.
    pub margin: f64,
    /// In entry order: the first nearest the dock (bottom), or on top
    /// (sides).
    pub items: Vec<Item>,
}

/// Lay out items whose names' texts are `widths` wide, for a dock at
/// `position`.
pub fn layout(position: Position, widths: &[f64], m: &Metrics) -> Layout {
    let (s, row, pitch, lh) = (m.icon, m.row(), m.pitch(), m.label_h());
    let margin = m.margin();
    let n = widths.len();
    let label_w = |i: usize| (widths[i].min(TEXT_MAX).ceil() + 2.0 * LABEL_PAD_X).max(lh);
    let column = (n as f64 * pitch - ITEM_GAP).max(0.0);
    let h = (column + 2.0 * margin).ceil();
    let mut items = Vec::with_capacity(n);
    let item = |icon_x: f64, cy: f64, label_x: f64, lw: f64| {
        let icon = Rect::new(icon_x, (cy - s / 2.0).round(), s, s);
        let label = Rect::new(label_x, (cy - lh / 2.0).round(), lw, lh);
        let x0 = icon.x.min(label.x);
        let x1 = icon.right().max(label.right());
        let hit = Rect::new(x0, cy - pitch / 2.0, x1 - x0, pitch);
        Item { icon, label, hit }
    };
    let w = match position {
        Position::Bottom => {
            // The icons' centers, bent to the right as the column rises,
            // with the first one straight above the folder.
            let d = |i: usize| row / 2.0 + i as f64 * pitch;
            let cx: Vec<f64> = (0..n).map(|i| bend(d(i)) - bend(d(0))).collect();
            let left = (0..n).fold(0.0_f64, |a, i| {
                a.min(cx[i] - s / 2.0 - LABEL_GAP - label_w(i))
            });
            let right = (0..n).fold(s / 2.0, |a, i| a.max(cx[i] + s / 2.0));
            let shift = margin - left;
            for (i, &x) in cx.iter().enumerate() {
                let icon_x = (x - s / 2.0 + shift).round();
                let lw = label_w(i);
                let cy = h - margin - d(i);
                items.push(item(icon_x, cy, icon_x - LABEL_GAP - lw, lw));
            }
            (right - left + 2.0 * margin).ceil()
        }
        Position::Left | Position::Right => {
            let widest = (0..n).fold(0.0_f64, |a, i| a.max(label_w(i)));
            let w = (s + LABEL_GAP + widest + 2.0 * margin).ceil();
            for i in 0..n {
                let cy = margin + row / 2.0 + i as f64 * pitch;
                let lw = label_w(i);
                items.push(match position {
                    Position::Left => item(margin, cy, margin + s + LABEL_GAP, lw),
                    _ => {
                        let icon_x = w - margin - s;
                        item(icon_x, cy, icon_x - LABEL_GAP - lw, lw)
                    }
                });
            }
            w
        }
    };
    Layout {
        position,
        w: w.max(1.0),
        h: h.max(1.0),
        margin,
        items,
    }
}

impl Layout {
    /// The item at (`x`, `y`), if any.
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        self.items.iter().position(|it| it.hit.contains(x, y))
    }

    /// The surface's size.
    pub fn size(&self) -> (i32, i32) {
        (self.w as i32, self.h as i32)
    }

    /// The positioner's anchor (on the folder's icon), gravity and offset:
    /// above a bottom dock, the first icon centered over the folder's;
    /// beside a side dock, the column centered on it. The icons are
    /// `DOCK_GAP` from it (the margin's shadow room may reach past that).
    pub fn positioner(&self) -> (Anchor, Gravity, (i32, i32)) {
        let gap = (DOCK_GAP - self.margin).round() as i32;
        match self.position {
            Position::Bottom => {
                let first = self
                    .items
                    .first()
                    .map_or(self.w / 2.0, |it| it.icon.center().0);
                let dx = (self.w / 2.0 - first).round() as i32;
                (Anchor::Top, Gravity::Top, (dx, -gap))
            }
            Position::Left => (Anchor::Right, Gravity::Right, (gap, 0)),
            Position::Right => (Anchor::Left, Gravity::Left, (-gap, 0)),
        }
    }
}

/// How far item `i` of `n` is out of the icon while the stack opens, at
/// `t` (0: closed, 1: open): the ones nearer the dock lead, the farthest
/// trailing by `STAGGER`.
pub fn progress(t: f64, i: usize, n: usize) -> f64 {
    const STAGGER: f64 = 0.35;
    let f = match n {
        0 | 1 => 0.0,
        _ => i as f64 / (n - 1) as f64,
    };
    ((t - STAGGER * f) / (1.0 - STAGGER)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            icon: 48.0,
            text_h: 20.0,
            shadow: 0.0,
        }
    }

    fn whole(r: Rect) -> bool {
        [r.x, r.y, r.w, r.h].iter().all(|v| v.fract() == 0.0)
    }

    #[test]
    fn bottom_fans_out_to_the_right() {
        let m = metrics();
        let l = layout(Position::Bottom, &[80.0, 200.0, 40.0, 120.0, 60.0], &m);
        assert_eq!(l.items.len(), 5);
        // The first is at the bottom, the rest above it in order.
        let first = l.items[0];
        assert_eq!(first.icon.bottom(), l.h - MARGIN);
        for w in l.items.windows(2) {
            assert!(w[1].icon.bottom() + ITEM_GAP <= w[0].icon.y);
            // Bending right, more the higher it gets.
            assert!(w[1].icon.x >= w[0].icon.x);
            // Hits meet without a hole or an overlap.
            assert_eq!(w[1].hit.bottom(), w[0].hit.y);
        }
        assert!(l.items[4].icon.x > first.icon.x);
        for it in &l.items {
            // Names on the left of their icons, centered on them.
            assert_eq!(it.label.right() + LABEL_GAP, it.icon.x);
            assert!((it.label.center().1 - it.icon.center().1).abs() <= 1.0);
            // On the surface, in whole pixels.
            assert!(it.label.x >= MARGIN && it.icon.right() <= l.w - MARGIN);
            assert!(it.icon.y >= MARGIN - 0.5);
            assert!(whole(it.icon) && whole(it.label));
        }
        // The widest name reaches the left margin.
        assert_eq!(l.items[1].label.x, MARGIN);
        assert_eq!((l.w.fract(), l.h.fract()), (0.0, 0.0));
    }

    #[test]
    fn names_are_capped() {
        let m = metrics();
        let l = layout(Position::Bottom, &[5000.0], &m);
        assert_eq!(l.items[0].label.w, TEXT_MAX + 2.0 * LABEL_PAD_X);
        // A short name still gets a round pill.
        let l = layout(Position::Bottom, &[1.0], &m);
        assert_eq!(l.items[0].label.w, m.label_h());
    }

    #[test]
    fn the_first_icon_sits_over_the_folder() {
        let m = metrics();
        let l = layout(Position::Bottom, &[80.0, 200.0, 40.0], &m);
        let (anchor, gravity, (dx, dy)) = l.positioner();
        // The icons DOCK_GAP from the folder's, the surface's margin closer.
        assert_eq!((anchor, gravity, dy), (Anchor::Top, Gravity::Top, -4));
        // Centered on the anchor point, then moved by the offset.
        let x = -l.w / 2.0 + dx as f64;
        assert!((x + l.items[0].icon.center().0).abs() <= 0.5);
    }

    #[test]
    fn sides_are_straight_columns() {
        let m = metrics();
        let widths = [80.0, 200.0, 40.0];
        for position in [Position::Left, Position::Right] {
            let l = layout(position, &widths, &m);
            // Top to bottom, first first, straight.
            assert_eq!(l.items[0].icon.y, MARGIN);
            for w in l.items.windows(2) {
                assert!(w[0].icon.bottom() < w[1].icon.y);
                assert_eq!(w[0].icon.x, w[1].icon.x);
            }
            // The icons face the dock, the names the other way.
            for it in &l.items {
                match position {
                    Position::Left => {
                        assert_eq!(it.icon.x, MARGIN);
                        assert_eq!(it.label.x, it.icon.right() + LABEL_GAP);
                    }
                    _ => {
                        assert_eq!(it.icon.right(), l.w - MARGIN);
                        assert_eq!(it.label.right() + LABEL_GAP, it.icon.x);
                    }
                }
                assert!(it.label.x >= MARGIN && it.label.right() <= l.w - MARGIN);
            }
            let (_, _, (dx, dy)) = l.positioner();
            assert_eq!((dx.abs(), dy), (4, 0));
        }
    }

    #[test]
    fn hits() {
        let m = metrics();
        let l = layout(Position::Left, &[80.0, 80.0], &m);
        let a = l.items[0].icon.center();
        let b = l.items[1].label.center();
        assert_eq!(l.hit(a.0, a.1), Some(0));
        assert_eq!(l.hit(b.0, b.1), Some(1));
        // Between them: one of them.
        let between = (l.items[0].icon.bottom() + l.items[1].icon.y) / 2.0;
        assert!(l.hit(a.0, between).is_some());
        assert_eq!(l.hit(l.w + 1.0, a.1), None);
    }

    #[test]
    fn fitting() {
        let m = metrics();
        // 54 per item, less the last gap, plus the margins.
        assert_eq!(fits(&m, 2.0 * 54.0 - 6.0 + 4.0), 2);
        assert_eq!(fits(&m, 2.0 * 54.0 - 6.0 + 3.0), 1);
        assert_eq!(fits(&m, 0.0), 1);
        let l = layout(Position::Bottom, &[50.0; 7], &m);
        assert_eq!(fits(&m, l.h), 7);
    }

    #[test]
    fn room_for_shadows() {
        let m = Metrics {
            shadow: 10.4,
            ..metrics()
        };
        for position in [Position::Bottom, Position::Left, Position::Right] {
            let l = layout(position, &[80.0, 200.0, 40.0], &m);
            assert_eq!(l.margin, 11.0);
            for it in &l.items {
                let shadow = it.icon.outset(m.shadow);
                assert!(shadow.x >= 0.0 && shadow.y >= 0.0);
                assert!(shadow.right() <= l.w && shadow.bottom() <= l.h);
            }
            // The icons stay DOCK_GAP from the folder's: the surface
            // reaches over it by the rest of the margin.
            let (_, _, (dx, dy)) = l.positioner();
            let gap = (DOCK_GAP - 11.0) as i32;
            match position {
                Position::Bottom => assert_eq!(dy, -gap),
                Position::Left => assert_eq!(dx, gap),
                Position::Right => assert_eq!(dx, -gap),
            }
        }
        // Fewer fit.
        assert_eq!(fits(&metrics(), 3.0 * 54.0), 3);
        assert_eq!(fits(&m, 3.0 * 54.0), 2);
    }

    #[test]
    fn opening_staggers() {
        assert_eq!(progress(0.0, 0, 5), 0.0);
        assert_eq!(progress(1.0, 4, 5), 1.0);
        // The first leads, the last trails.
        assert!(progress(0.5, 0, 5) > progress(0.5, 4, 5));
        assert_eq!(progress(0.35, 4, 5), 0.0);
        assert_eq!(progress(0.65, 0, 5), 1.0);
        // Overshooting springs stay in range.
        assert_eq!(progress(1.2, 2, 5), 1.0);
        assert_eq!(progress(0.5, 0, 1), progress(0.5, 0, 2));
    }
}
