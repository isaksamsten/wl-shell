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

/// Cut `rect`, which shows `src` (x, y, w, h in buffer pixels; None: all
/// of a buffer `size` pixels large), down to `clip`: the rect left and the
/// part of the buffer it shows (None if nothing was cut and `src` was
/// None). None if nothing is left.
pub fn clip(
    rect: Rect,
    size: (f64, f64),
    src: Option<[f64; 4]>,
    clip: Rect,
) -> Option<(Rect, Option<[f64; 4]>)> {
    let (x0, x1) = (rect.x.max(clip.x), rect.right().min(clip.right()));
    let (y0, y1) = (rect.y.max(clip.y), rect.bottom().min(clip.bottom()));
    if x1 <= x0 || y1 <= y0 || rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    if (x0, y0, x1, y1) == (rect.x, rect.y, rect.right(), rect.bottom()) {
        return Some((rect, src));
    }
    let [sx, sy, sw, sh] = src.unwrap_or([0.0, 0.0, size.0, size.1]);
    // wl_fixed has 1/256 precision; stay inside the buffer part.
    let floor = |v: f64| (v * 256.0).floor() / 256.0;
    let cut = |a0: f64, a1: f64, r0: f64, rl: f64, s0: f64, sl: f64| {
        let start = floor(s0 + (a0 - r0) / rl * sl);
        let len = floor((a1 - a0) / rl * sl).clamp(1.0 / 256.0, (s0 + sl - start).max(1.0 / 256.0));
        (start, len)
    };
    let (cx, cw) = cut(x0, x1, rect.x, rect.w, sx, sw);
    let (cy, ch) = cut(y0, y1, rect.y, rect.h, sy, sh);
    Some((Rect::new(x0, y0, x1 - x0, y1 - y0), Some([cx, cy, cw, ch])))
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

    #[test]
    fn clipping_cuts_the_buffer_with_the_rect() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        let all = Rect::new(-10.0, -10.0, 200.0, 200.0);
        assert_eq!(clip(r, (200.0, 100.0), None, all), Some((r, None)));
        let src = Some([10.0, 0.0, 100.0, 50.0]);
        assert_eq!(clip(r, (200.0, 100.0), src, all), Some((r, src)));
        // The right half and the top 10 px go.
        let (rect, src) =
            clip(r, (200.0, 100.0), None, Rect::new(-5.0, 10.0, 55.0, 100.0)).unwrap();
        assert_eq!(rect, Rect::new(0.0, 10.0, 50.0, 40.0));
        assert_eq!(src, Some([0.0, 20.0, 100.0, 80.0]));
        // Within a part of the buffer.
        let (_, src) = clip(
            r,
            (200.0, 100.0),
            Some([100.0, 0.0, 100.0, 50.0]),
            Rect::new(50.0, 0.0, 100.0, 100.0),
        )
        .unwrap();
        assert_eq!(src, Some([150.0, 0.0, 50.0, 50.0]));
        assert_eq!(
            clip(r, (1.0, 1.0), None, Rect::new(100.0, 0.0, 10.0, 10.0)),
            None
        );
    }
}
