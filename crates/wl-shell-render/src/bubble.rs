//! A speech bubble: a rounded rect with a tail on one side, pointing at
//! what it belongs to (the dock's menu points at its icon). Rendered with
//! tiny-skia into one image: the body, a 1 px inner border, hairline
//! separators that fade out at their ends, and a drop shadow of the whole
//! shape that is left out where the bubble covers it, so a translucent
//! bubble isn't darkened by its own shadow.
//!
//! The outline is one path: the tail's base joins the body with small
//! concave curves and its tip is slightly rounded, so it reads as one
//! smooth shape. It is built for a tail on the bottom edge and turned for
//! the other sides. Everything is in logical pixels relative to the body's
//! top left corner; `bounds` says where the image goes.

use resvg::tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Mask, Paint, Path, PathBuilder, Pixmap, Point,
    Rect as SkRect, SpreadMode, Stroke, Transform,
};

use crate::image::Image;
use crate::shadow::{self, Rect, Settings};

/// How far the concave curves joining the tail to the body reach along
/// the edge, past the tail's base, at most.
pub const JOIN: f64 = 3.0;
/// How far a separator fades out at each end.
pub const SEPARATOR_FADE: f64 = 12.0;

/// The side of the body the tail is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Pointing down.
    Bottom,
    /// Pointing left.
    Left,
    /// Pointing right.
    Right,
}

/// A horizontal hairline across the body at `y`, from `x0` to `x1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Separator {
    pub y: f64,
    pub x0: f64,
    pub x1: f64,
}

/// What to render.
#[derive(Clone, Debug, PartialEq)]
pub struct Bubble {
    /// The body's size.
    pub w: f64,
    pub h: f64,
    pub radius: f64,
    pub side: Side,
    /// The tail's center along its edge, from the edge's start (its left
    /// or top end).
    pub tail_at: f64,
    /// The tail's base and height; 0: no tail.
    pub tail_width: f64,
    pub tail_height: f64,
    /// 0xRRGGBBAA, not premultiplied; alpha 0: none.
    pub fill: u32,
    pub border: u32,
    pub separators: Vec<Separator>,
    pub separator: u32,
    pub shadow: Option<Settings>,
    pub scale: i32,
}

type P = (f64, f64);

/// A piece of an outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(P),
    Line(P),
    Quad(P, P),
    Cubic(P, P, P),
    Close,
}

/// A cubic's control distance for a quarter circle of radius 1.
const KAPPA: f64 = 0.552_284_75;

impl Bubble {
    fn radius(&self) -> f64 {
        self.radius.clamp(0.0, self.w.min(self.h).max(0.0) / 2.0)
    }

    /// The tail's edge's length and the body's size across it.
    fn edge(&self) -> (f64, f64) {
        match self.side {
            Side::Bottom => (self.w, self.h),
            Side::Left | Side::Right => (self.h, self.w),
        }
    }

    fn has_tail(&self) -> bool {
        self.tail_width > 0.0 && self.tail_height > 0.0
    }

    /// A point built for a tail on the bottom edge (`x` along it, `y`
    /// across from the opposite edge) on the body.
    fn orient(&self, (x, y): P) -> P {
        match self.side {
            Side::Bottom => (x, y),
            Side::Left => (self.w - y, x),
            Side::Right => (y, x),
        }
    }

    /// The tail's base half-width and center along its edge, kept on it.
    fn tail(&self) -> (f64, f64) {
        let (len, _) = self.edge();
        let half = (self.tail_width / 2.0).clamp(0.0, len / 2.0);
        (half, self.tail_at.clamp(half, (len - half).max(half)))
    }

    /// Where the tail points.
    pub fn tip(&self) -> P {
        let (_, across) = self.edge();
        let (_, at) = self.tail();
        let t = if self.has_tail() {
            self.tail_height
        } else {
            0.0
        };
        self.orient((at, across + t))
    }

    /// The outline, clockwise from the top left corner.
    pub fn outline(&self) -> Vec<Seg> {
        let (len, across) = self.edge();
        let r = self.radius();
        let k = r * KAPPA;
        let mut out = vec![
            Seg::Move((r, 0.0)),
            Seg::Line((len - r, 0.0)),
            Seg::Cubic((len - r + k, 0.0), (len, r - k), (len, r)),
            Seg::Line((len, across - r)),
            Seg::Cubic(
                (len, across - r + k),
                (len - r + k, across),
                (len - r, across),
            ),
        ];
        if self.has_tail() {
            let (half, at) = self.tail();
            let tip = (at, across + self.tail_height);
            let (right, left) = ((at + half, across), (at - half, across));
            let side = half.hypot(self.tail_height);
            let unit = |from: P| ((tip.0 - from.0) / side, (tip.1 - from.1) / side);
            let (ur, ul) = (unit(right), unit(left));
            let along = |p: P, u: P, d: f64| (p.0 + u.0 * d, p.1 + u.1 * d);
            let join = JOIN.min(side / 3.0);
            let round = (side / 4.0).min(1.5);
            out.extend([
                Seg::Line((right.0 + join, across)),
                Seg::Quad(right, along(right, ur, join)),
                Seg::Line(along(tip, ur, -round)),
                Seg::Quad(tip, along(tip, ul, -round)),
                Seg::Line(along(left, ul, join)),
                Seg::Quad(left, (left.0 - join, across)),
            ]);
        }
        out.extend([
            Seg::Line((r, across)),
            Seg::Cubic((r - k, across), (0.0, across - r + k), (0.0, across - r)),
            Seg::Line((0.0, r)),
            Seg::Cubic((0.0, r - k), (r - k, 0.0), (r, 0.0)),
            Seg::Close,
        ]);
        let o = |p| self.orient(p);
        (out.into_iter())
            .map(|s| match s {
                Seg::Move(a) => Seg::Move(o(a)),
                Seg::Line(a) => Seg::Line(o(a)),
                Seg::Quad(a, b) => Seg::Quad(o(a), o(b)),
                Seg::Cubic(a, b, c) => Seg::Cubic(o(a), o(b), o(c)),
                Seg::Close => Seg::Close,
            })
            .collect()
    }

    /// Where the image goes, relative to the body's top left corner: the
    /// body, the tail and the shadow, plus a pixel for antialiasing.
    pub fn bounds(&self) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1): (f64, f64, f64, f64) = (0.0, 0.0, self.w, self.h);
        if self.has_tail() {
            let t = self.tail_height;
            match self.side {
                Side::Bottom => y1 += t,
                Side::Left => x0 -= t,
                Side::Right => x1 += t,
            }
        }
        if let Some(s) = &self.shadow {
            let e = (1.5 * s.softness.max(0.0)).ceil();
            let (ox, oy) = (s.offset.0 as f64, s.offset.1 as f64);
            (x0, x1) = (x0.min(x0 + ox - e), x1.max(x1 + ox + e));
            (y0, y1) = (y0.min(y0 + oy - e), y1.max(y1 + oy + e));
        }
        let (x0, y0) = ((x0 - 1.0).floor() as i32, (y0 - 1.0).floor() as i32);
        let (x1, y1) = ((x1 + 1.0).ceil() as i32, (y1 + 1.0).ceil() as i32);
        Rect {
            x: x0,
            y: y0,
            w: x1 - x0,
            h: y1 - y0,
        }
    }
}

fn color(rgba: u32) -> Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

fn paint(rgba: u32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(rgba));
    p.anti_alias = true;
    p
}

/// `segs` as a path, each point mapped by `map`.
fn path(segs: &[Seg], map: &dyn Fn(P) -> (f32, f32)) -> Option<Path> {
    let mut pb = PathBuilder::new();
    for s in segs {
        match *s {
            Seg::Move(a) => {
                let a = map(a);
                pb.move_to(a.0, a.1);
            }
            Seg::Line(a) => {
                let a = map(a);
                pb.line_to(a.0, a.1);
            }
            Seg::Quad(a, b) => {
                let (a, b) = (map(a), map(b));
                pb.quad_to(a.0, a.1, b.0, b.1);
            }
            Seg::Cubic(a, b, c) => {
                let (a, b, c) = (map(a), map(b), map(c));
                pb.cubic_to(a.0, a.1, b.0, b.1, c.0, c.1);
            }
            Seg::Close => pb.close(),
        }
    }
    pb.finish()
}

/// Render `b`: an image of `b.bounds()` at its scale, premultiplied
/// ARGB8888.
pub fn render(b: &Bubble) -> Image {
    let rect = b.bounds();
    let s = b.scale.max(1) as f64;
    let (bw, bh) = ((rect.w as f64 * s) as u32, (rect.h as f64 * s) as u32);
    let blank = || Image {
        width: bw.max(1),
        height: bh.max(1),
        data: vec![0; (bw.max(1) * bh.max(1) * 4) as usize],
    };
    let map = |(x, y): P| {
        (
            ((x - rect.x as f64) * s) as f32,
            ((y - rect.y as f64) * s) as f32,
        )
    };
    let (Some(outline), Some(mut pixmap), Some(mut mask)) = (
        path(&b.outline(), &map),
        Pixmap::new(bw, bh),
        Mask::new(bw, bh),
    ) else {
        return blank();
    };
    let id = Transform::identity();
    mask.fill_path(&outline, FillRule::Winding, true, id);
    if b.fill & 0xff != 0 {
        pixmap.fill_path(&outline, &paint(b.fill), FillRule::Winding, id, None);
    }
    if b.separator & 0xff != 0 {
        for sep in &b.separators {
            separator(&mut pixmap, &mask, sep, b.separator, &map, s);
        }
    }
    if b.border & 0xff != 0 {
        // Twice as wide, half of it cut off outside: an inner border.
        let stroke = Stroke {
            width: (2.0 * s) as f32,
            ..Stroke::default()
        };
        pixmap.stroke_path(&outline, &paint(b.border), &stroke, id, Some(&mask));
    }
    let shadow = b.shadow.map(|sh| shadow_plane(mask.data(), bw, bh, &sh, s));
    let sc = b
        .shadow
        .map_or([0.0; 4], |sh| shadow::premultiplied(sh.color));
    let mut data = Vec::with_capacity((bw * bh * 4) as usize);
    for (i, px) in pixmap.data().as_chunks::<4>().0.iter().enumerate() {
        let [r, g, bl, a] = [px[0], px[1], px[2], px[3]].map(|c| c as f64 / 255.0);
        let mut out = [bl, g, r, a];
        if let Some(plane) = &shadow {
            let under = plane[i] as f64 * (1.0 - mask.data()[i] as f64 / 255.0);
            for k in 0..4 {
                out[k] += sc[k] * under;
            }
        }
        data.extend(out.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8));
    }
    Image {
        width: bw,
        height: bh,
        data,
    }
}

/// A separator: a hairline fading in and out at its ends, cut to the
/// bubble.
fn separator(
    pixmap: &mut Pixmap,
    mask: &Mask,
    sep: &Separator,
    rgba: u32,
    map: &dyn Fn(P) -> (f32, f32),
    s: f64,
) {
    let len = sep.x1 - sep.x0;
    if len <= 0.0 {
        return;
    }
    let fade = (SEPARATOR_FADE.min(len / 2.0) / len) as f32;
    let (solid, clear) = (color(rgba), color(rgba & 0xffffff00));
    let stops = vec![
        GradientStop::new(0.0, clear),
        GradientStop::new(fade, solid),
        GradientStop::new(1.0 - fade, solid),
        GradientStop::new(1.0, clear),
    ];
    let (a, b) = (map((sep.x0, sep.y)), map((sep.x1, sep.y)));
    let Some(shader) = LinearGradient::new(
        Point::from_xy(a.0, a.1),
        Point::from_xy(b.0, b.1),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    // One logical pixel thick, on whole buffer pixels.
    let thick = s.max(1.0) as f32;
    let y = (a.1 - thick / 2.0).round();
    if let Some(r) = SkRect::from_xywh(a.0, y, b.0 - a.0, thick) {
        let p = Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        };
        pixmap.fill_rect(r, &p, Transform::identity(), Some(mask));
    }
}

/// The shadow's alpha per pixel: the shape's coverage, moved by the
/// offset and blurred, times the color's alpha (in `premultiplied`).
fn shadow_plane(mask: &[u8], w: u32, h: u32, s: &Settings, scale: f64) -> Vec<f32> {
    let (w, h) = (w as usize, h as usize);
    let (ox, oy) = (
        (s.offset.0 as f64 * scale).round() as isize,
        (s.offset.1 as f64 * scale).round() as isize,
    );
    let mut plane = vec![0f32; w * h];
    for y in 0..h {
        let sy = y as isize - oy;
        if sy < 0 || sy >= h as isize {
            continue;
        }
        for x in 0..w {
            let sx = x as isize - ox;
            if sx >= 0 && sx < w as isize {
                plane[y * w + x] = mask[sy as usize * w + sx as usize] as f32 / 255.0;
            }
        }
    }
    let sigma = s.softness.max(0.0) / 2.0 * scale;
    for r in shadow::box_radii(sigma) {
        shadow::box_blur(&mut plane, w, h, r, true);
        shadow::box_blur(&mut plane, w, h, r, false);
    }
    plane
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bubble(side: Side) -> Bubble {
        Bubble {
            w: 200.0,
            h: 120.0,
            radius: 12.0,
            side,
            tail_at: 60.0,
            tail_width: 10.0,
            tail_height: 7.0,
            fill: 0x262626e6,
            border: 0,
            separators: Vec::new(),
            separator: 0,
            shadow: None,
            scale: 1,
        }
    }

    /// The pixel at body coordinates (`x`, `y`): B, G, R, A.
    fn pixel(b: &Bubble, img: &Image, x: f64, y: f64) -> [u8; 4] {
        let r = b.bounds();
        let s = b.scale as f64;
        let (i, j) = (((x - r.x as f64) * s) as u32, ((y - r.y as f64) * s) as u32);
        let k = ((j * img.width + i) * 4) as usize;
        img.data[k..k + 4].try_into().unwrap()
    }

    #[test]
    fn tips() {
        assert_eq!(bubble(Side::Bottom).tip(), (60.0, 127.0));
        assert_eq!(bubble(Side::Left).tip(), (-7.0, 60.0));
        assert_eq!(bubble(Side::Right).tip(), (207.0, 60.0));
        // Kept on its edge.
        let b = Bubble {
            tail_at: -50.0,
            ..bubble(Side::Bottom)
        };
        assert_eq!(b.tip(), (5.0, 127.0));
    }

    #[test]
    fn bounds_hold_the_tail_and_shadow() {
        let b = bubble(Side::Bottom);
        assert_eq!(
            b.bounds(),
            Rect {
                x: -1,
                y: -1,
                w: 202,
                h: 129
            }
        );
        let b = Bubble {
            shadow: Some(Settings {
                color: 0x00000080,
                softness: 10.0,
                spread: 0,
                offset: (0, 4),
            }),
            ..bubble(Side::Left)
        };
        // 15 px of blur, moved 4 px down; the tail reaches 7 px left.
        assert_eq!(
            b.bounds(),
            Rect {
                x: -7 - 15 - 1,
                y: -11 - 1,
                w: 7 + 200 + 30 + 2,
                h: 120 + 30 + 2
            }
        );
    }

    #[test]
    fn outline_is_closed_and_touches_the_tip() {
        for side in [Side::Bottom, Side::Left, Side::Right] {
            let b = bubble(side);
            let o = b.outline();
            assert!(matches!(o[0], Seg::Move(_)));
            assert_eq!(o.last(), Some(&Seg::Close));
            let tip = b.tip();
            let at_tip = |s: &Seg| matches!(s, Seg::Quad(c, _) if *c == tip);
            assert!(o.iter().any(at_tip), "{side:?}: no tip");
        }
    }

    #[test]
    fn rendered_shapes() {
        for (side, inside_tip) in [
            (Side::Bottom, (60.0, 124.0)),
            (Side::Left, (-4.0, 60.0)),
            (Side::Right, (204.0, 60.0)),
        ] {
            for scale in [1, 2] {
                let b = Bubble {
                    scale,
                    ..bubble(side)
                };
                let img = render(&b);
                let r = b.bounds();
                assert_eq!(
                    (img.width, img.height),
                    ((r.w * scale) as u32, (r.h * scale) as u32)
                );
                // The body is filled (premultiplied 0x262626 at 0xe6).
                assert_eq!(pixel(&b, &img, 100.0, 60.0), [34, 34, 34, 230]);
                // The tail too, and its corners are round.
                assert!(pixel(&b, &img, inside_tip.0, inside_tip.1)[3] > 100);
                assert_eq!(pixel(&b, &img, 0.5, 0.5)[3], 0);
            }
        }
        // Beside the tail, outside: nothing.
        let b = bubble(Side::Bottom);
        let img = render(&b);
        assert_eq!(pixel(&b, &img, 75.0, 124.0)[3], 0);
        assert_eq!(pixel(&b, &img, 45.0, 124.0)[3], 0);
    }

    #[test]
    fn shadow_stays_out_from_under_the_bubble() {
        let plain = bubble(Side::Bottom);
        let b = Bubble {
            shadow: Some(Settings {
                color: 0x000000ff,
                softness: 12.0,
                spread: 0,
                offset: (0, 4),
            }),
            ..plain.clone()
        };
        let img = render(&b);
        // Not darkened inside; shadow under the body, more below than
        // above, none past its reach.
        assert_eq!(pixel(&b, &img, 100.0, 60.0), [34, 34, 34, 230]);
        let below = pixel(&b, &img, 150.0, 123.0)[3];
        let above = pixel(&b, &img, 150.0, -3.0)[3];
        assert!(below > above && above > 0, "{below} {above}");
        let r = b.bounds();
        assert!(pixel(&b, &img, 100.0, (r.y + r.h) as f64 - 0.5)[3] <= 1);
    }

    #[test]
    fn border_and_separators() {
        let b = Bubble {
            border: 0xffffffff,
            separators: vec![Separator {
                y: 60.0,
                x0: 10.0,
                x1: 190.0,
            }],
            separator: 0xff0000ff,
            fill: 0x000000ff,
            ..bubble(Side::Bottom)
        };
        let img = render(&b);
        // The border is inside the edge.
        assert!(pixel(&b, &img, 100.0, 0.5)[0] > 200);
        assert!(pixel(&b, &img, 100.0, 1.5)[0] < 30);
        // The separator is red in the middle and fades out at its ends.
        let mid = pixel(&b, &img, 100.0, 60.0);
        assert!(mid[2] > 240 && mid[0] < 10, "{mid:?}");
        assert!(pixel(&b, &img, 11.0, 60.0)[2] < 40);
        assert_eq!(pixel(&b, &img, 100.0, 62.0), [0, 0, 0, 255]);
    }
}
