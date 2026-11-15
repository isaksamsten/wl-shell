//! The dock's geometry, independent of the edge it sits on. Pure, unit
//! tested.
//!
//! Layouts work in dock coordinates: `u` along the dock (left to right for
//! a bottom dock, top to bottom for a side dock) and `v` away from the
//! screen edge. A `Frame` maps them to the surface. `Metrics` has the
//! resting sizes: how thick the surface is, the exclusive zone, and where
//! the panel, icons and indicators sit across the dock.

use wl_shell_ui::Rect;
use wl_shell_ui::shapes::Axis;

use crate::config::Position;

/// Space between an icon and its label.
pub const LABEL_GAP: f64 = 8.0;

/// The dock surface: its edge and size (logical pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub position: Position,
    pub width: f64,
    pub height: f64,
}

impl Frame {
    /// The surface's length along the dock.
    pub fn length(&self) -> f64 {
        match self.position {
            Position::Bottom => self.width,
            Position::Left | Position::Right => self.height,
        }
    }

    /// The surface's thickness across the dock.
    pub fn across(&self) -> f64 {
        match self.position {
            Position::Bottom => self.height,
            Position::Left | Position::Right => self.width,
        }
    }

    /// The direction the dock grows in.
    pub fn axis(&self) -> Axis {
        match self.position {
            Position::Bottom => Axis::Horizontal,
            Position::Left | Position::Right => Axis::Vertical,
        }
    }

    /// The surface rect of the dock rect from (`u`, `v`), `along` long and
    /// `across` thick.
    pub fn rect(&self, u: f64, v: f64, along: f64, across: f64) -> Rect {
        match self.position {
            Position::Bottom => Rect::new(u, self.height - v - across, along, across),
            Position::Left => Rect::new(v, u, across, along),
            Position::Right => Rect::new(self.width - v - across, u, across, along),
        }
    }

    /// A rect that isn't turned with the dock (text, say): `w` x `h`,
    /// centered on `u` along the dock but kept on the surface, starting
    /// at `v` across it.
    pub fn upright(&self, u: f64, v: f64, w: f64, h: f64) -> Rect {
        let (along, across) = match self.position {
            Position::Bottom => (w, h),
            Position::Left | Position::Right => (h, w),
        };
        let u0 = (u - along / 2.0).clamp(0.0, (self.length() - along).max(0.0));
        self.rect(u0, v, along, across)
    }

    /// The dock coordinates (`u`, `v`) of the surface point (`x`, `y`).
    pub fn dock_point(&self, x: f64, y: f64) -> (f64, f64) {
        match self.position {
            Position::Bottom => (x, self.height - y),
            Position::Left => (y, x),
            Position::Right => (y, self.width - x),
        }
    }
}

/// Resting sizes across the dock, logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    pub icon: f64,
    pub padding: f64,
    pub margin: f64,
    pub magnification: f64,
    /// Room for the label beyond the magnified icons: its height for a
    /// bottom dock, its widest for a side dock; 0 without labels.
    pub label: f64,
    /// How far an icon's shadow reaches past it.
    pub icon_shadow: f64,
}

impl Metrics {
    /// The panel's thickness.
    pub fn panel(&self) -> f64 {
        self.icon + 2.0 * self.padding
    }

    /// Where the panel starts across the dock.
    pub fn panel_v(&self) -> f64 {
        self.margin
    }

    /// Where icons start across the dock; they grow away from the edge.
    pub fn icon_v(&self) -> f64 {
        self.margin + self.padding
    }

    /// The indicator dot's size.
    pub fn dot(&self) -> f64 {
        (self.icon / 12.0).round().max(3.0)
    }

    /// Where indicators start across the dock: centered in the padding
    /// between the panel's edge and the icons, if there is room.
    pub fn dot_v(&self) -> f64 {
        self.margin + ((self.padding - self.dot()) / 2.0).max(1.0).floor()
    }

    /// The surface's thickness: room for the fully magnified icons and
    /// the label (or the icons' shadow), so magnifying never resizes it.
    pub fn thickness(&self) -> u32 {
        let icons = self.icon_v() + self.icon * self.magnification;
        let above = match self.label > 0.0 {
            true => LABEL_GAP + self.label,
            false => 0.0,
        };
        (icons + above.max(self.icon_shadow)).ceil() as u32
    }

    /// The exclusive zone: the resting panel and its margin, if reserved.
    pub fn exclusive_zone(&self, reserve: bool) -> i32 {
        match reserve {
            true => (self.margin + self.panel()).ceil() as i32,
            false => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(position: Position) -> Frame {
        Frame {
            position,
            width: 1000.0,
            height: 100.0,
        }
    }

    #[test]
    fn frames_map_dock_rects_to_their_edge() {
        let f = frame(Position::Bottom);
        assert_eq!(f.length(), 1000.0);
        assert_eq!(
            f.rect(10.0, 5.0, 40.0, 20.0),
            Rect::new(10.0, 75.0, 40.0, 20.0)
        );
        assert_eq!(f.dock_point(10.0, 75.0), (10.0, 25.0));

        let f = Frame {
            position: Position::Left,
            width: 100.0,
            height: 1000.0,
        };
        assert_eq!(f.length(), 1000.0);
        assert_eq!(
            f.rect(10.0, 5.0, 40.0, 20.0),
            Rect::new(5.0, 10.0, 20.0, 40.0)
        );
        assert_eq!(f.dock_point(5.0, 10.0), (10.0, 5.0));

        let f = Frame {
            position: Position::Right,
            width: 100.0,
            height: 1000.0,
        };
        assert_eq!(
            f.rect(10.0, 5.0, 40.0, 20.0),
            Rect::new(75.0, 10.0, 20.0, 40.0)
        );
        assert_eq!(f.dock_point(75.0, 10.0), (10.0, 25.0));
    }

    #[test]
    fn dock_coordinates_round_trip() {
        for position in [Position::Bottom, Position::Left, Position::Right] {
            let f = Frame {
                position,
                width: 300.0,
                height: 200.0,
            };
            let r = f.rect(30.0, 12.0, 1.0, 1.0);
            // The rect's corner nearest the edge and the dock's start.
            let (x, y) = match position {
                Position::Bottom => (r.x, r.bottom()),
                Position::Left => (r.x, r.y),
                Position::Right => (r.right(), r.y),
            };
            assert_eq!(f.dock_point(x, y), (30.0, 12.0), "{position:?}");
        }
    }

    #[test]
    fn upright_rects_stay_on_the_surface() {
        let f = frame(Position::Bottom);
        assert_eq!(
            f.upright(500.0, 10.0, 100.0, 20.0),
            Rect::new(450.0, 70.0, 100.0, 20.0)
        );
        assert_eq!(f.upright(10.0, 10.0, 100.0, 20.0).x, 0.0);
        assert_eq!(f.upright(990.0, 10.0, 100.0, 20.0).right(), 1000.0);
        let f = Frame {
            position: Position::Left,
            width: 300.0,
            height: 1000.0,
        };
        // Not turned: as wide as it is, across the dock.
        assert_eq!(
            f.upright(500.0, 10.0, 100.0, 20.0),
            Rect::new(10.0, 490.0, 100.0, 20.0)
        );
    }

    #[test]
    fn metrics() {
        let m = Metrics {
            icon: 48.0,
            padding: 6.0,
            margin: 6.0,
            magnification: 2.0,
            label: 30.0,
            icon_shadow: 11.0,
        };
        assert_eq!(m.panel(), 60.0);
        assert_eq!(m.icon_v(), 12.0);
        assert_eq!(m.dot(), 4.0);
        assert_eq!(m.dot_v(), 7.0);
        assert!(m.dot_v() + m.dot() <= m.icon_v());
        assert_eq!(m.thickness(), 12 + 96 + 8 + 30);
        assert_eq!(m.exclusive_zone(true), 66);
        assert_eq!(m.exclusive_zone(false), 0);
        let no_label = Metrics { label: 0.0, ..m };
        assert_eq!(no_label.thickness(), 12 + 96 + 11);
    }
}
