//! Rects. Pure, unit tested.
//!
//! Layouts work in `Rect`s of logical pixels (f64), so springs can move
//! them smoothly; `IRect` is what the protocol takes, rounded at the last
//! moment.

/// A rect in surface-local logical pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    /// From `self` (`t` = 0) to `to` (`t` = 1).
    pub fn lerp(self, to: Rect, t: f64) -> Rect {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Rect::new(
            l(self.x, to.x),
            l(self.y, to.y),
            l(self.w, to.w),
            l(self.h, to.h),
        )
    }

    /// Scaled by `s` around the point (`cx`, `cy`).
    pub fn scaled(self, s: f64, cx: f64, cy: f64) -> Rect {
        Rect::new(
            cx + (self.x - cx) * s,
            cy + (self.y - cy) * s,
            self.w * s,
            self.h * s,
        )
    }

    /// Scaled by `s` around its center.
    pub fn scaled_center(self, s: f64) -> Rect {
        let (cx, cy) = self.center();
        self.scaled(s, cx, cy)
    }

    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// The largest square centered in `self`, at most `max` wide.
    pub fn centered_square(&self, max: f64) -> Rect {
        let s = self.w.min(self.h).min(max);
        let (cx, cy) = self.center();
        Rect::new(cx - s / 2.0, cy - s / 2.0, s, s)
    }

    /// Grown by `d` on every side.
    pub fn outset(&self, d: f64) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
}

/// A rect in surface-local logical pixels, as the protocol takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    /// Round the edges, not the size, so neighbors never overlap or part.
    /// Never empty: a viewport destination must be at least 1x1.
    pub fn round(r: Rect) -> Self {
        let (x0, y0) = (r.x.round() as i32, r.y.round() as i32);
        let (x1, y1) = (r.right().round() as i32, r.bottom().round() as i32);
        Self {
            x: x0,
            y: y0,
            w: (x1 - x0).max(1),
            h: (y1 - y0).max(1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounding_keeps_neighbors_touching() {
        let a = Rect::new(0.4, 0.0, 10.3, 5.0);
        let b = Rect::new(a.right(), 0.0, 10.3, 5.0);
        let (ia, ib) = (IRect::round(a), IRect::round(b));
        assert_eq!(ia.x + ia.w, ib.x);
        assert_eq!(IRect::round(Rect::new(3.0, 3.0, 0.1, 0.0)).w, 1);
    }

    #[test]
    fn scaling_and_lerping() {
        let r = Rect::new(10.0, 10.0, 20.0, 40.0);
        assert_eq!(r.scaled_center(0.5), Rect::new(15.0, 20.0, 10.0, 20.0));
        assert_eq!(r.scaled(2.0, 0.0, 0.0), Rect::new(20.0, 20.0, 40.0, 80.0));
        assert_eq!(
            r.lerp(Rect::default(), 0.5),
            Rect::new(5.0, 5.0, 10.0, 20.0)
        );
        assert_eq!(r.centered_square(100.0), Rect::new(10.0, 20.0, 20.0, 20.0));
        assert_eq!(r.outset(1.0), Rect::new(9.0, 9.0, 22.0, 42.0));
        assert!(r.contains(10.0, 49.9) && !r.contains(30.0, 20.0));
    }
}
