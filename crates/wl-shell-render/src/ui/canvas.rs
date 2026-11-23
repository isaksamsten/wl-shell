//! A canvas's drawing: the commands a plugin's `draw(c)` recorded, in
//! viewbox units (y down), and their painting with tiny-skia.
//!
//! Angles are radians, clockwise (y is down); `arc` starts at 3 o'clock
//! like an HTML canvas, while a `rotate(a)` turns a line drawn along -y
//! (12 o'clock) clockwise, as clock hands do. Stroke widths are in
//! viewbox units too, so they scale with the canvas.

use resvg::tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform,
};

use super::tree::{Color, TextAlign, Theme};

/// How a stroke's ends look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cap {
    #[default]
    Butt,
    Round,
    Square,
}

/// How a shape is filled and stroked.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ink {
    pub fill: Option<Color>,
    pub stroke: Option<Color>,
    /// The stroke's width; default 1.
    pub width: f32,
    pub cap: Cap,
}

/// A segment of a path, absolute.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Quad(f32, f32, f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Save,
    Restore,
    Translate(f32, f32),
    /// Radians, clockwise.
    Rotate(f32),
    Scale(f32, f32),
    Line {
        from: (f32, f32),
        to: (f32, f32),
        ink: Ink,
    },
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        ink: Ink,
    },
    Circle {
        x: f32,
        y: f32,
        r: f32,
        ink: Ink,
    },
    /// From `start` to `end` (radians from 3 o'clock, clockwise); with
    /// `pie`, closed through the center.
    Arc {
        x: f32,
        y: f32,
        r: f32,
        start: f32,
        end: f32,
        pie: bool,
        ink: Ink,
    },
    Path {
        segs: Vec<Seg>,
        ink: Ink,
    },
    /// One line of text whose `align` edge (and middle, down) is at
    /// (`x`, `y`). It moves and scales with the transform, but doesn't
    /// turn.
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        color: Option<Color>,
        bold: bool,
        align: TextAlign,
    },
}

/// The most commands a canvas keeps.
pub const MAX_COMMANDS: usize = 20_000;

/// SVG path data (`M 0 0 L 10 10 Z`, relative commands, arcs) as
/// absolute segments.
pub fn parse_path(d: &str) -> Result<Vec<Seg>, String> {
    use svgtypes::SimplePathSegment as S;
    let mut out = Vec::new();
    for seg in svgtypes::SimplifyingPathParser::from(d) {
        let seg = seg.map_err(|e| format!("path: {e}"))?;
        let f = |v: f64| v as f32;
        out.push(match seg {
            S::MoveTo { x, y } => Seg::Move(f(x), f(y)),
            S::LineTo { x, y } => Seg::Line(f(x), f(y)),
            S::Quadratic { x1, y1, x, y } => Seg::Quad(f(x1), f(y1), f(x), f(y)),
            S::CurveTo {
                x1,
                y1,
                x2,
                y2,
                x,
                y,
            } => Seg::Cubic(f(x1), f(y1), f(x2), f(y2), f(x), f(y)),
            S::ClosePath => Seg::Close,
        });
    }
    Ok(out)
}

/// A circular arc's path, in cubic pieces of at most a quarter turn.
fn arc(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, start: f32, end: f32, move_to: bool) {
    let sweep = (end - start).clamp(-std::f32::consts::TAU, std::f32::consts::TAU);
    let n = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = sweep / n as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let at = |a: f32| (cx + r * a.cos(), cy + r * a.sin());
    let (x0, y0) = at(start);
    match move_to {
        true => pb.move_to(x0, y0),
        false => pb.line_to(x0, y0),
    }
    for i in 0..n {
        let a0 = start + step * i as f32;
        let a1 = a0 + step;
        let (x0, y0) = at(a0);
        let (x1, y1) = at(a1);
        pb.cubic_to(
            x0 - k * r * a0.sin(),
            y0 + k * r * a0.cos(),
            x1 + k * r * a1.sin(),
            y1 - k * r * a1.cos(),
            x1,
            y1,
        );
    }
}

/// A text the painter draws for the canvas: where (in pixels on the
/// target), how big (pixels), and the rest as given.
pub(crate) struct Label<'a> {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub text: &'a str,
    pub rgba: u32,
    pub bold: bool,
    pub align: TextAlign,
}

/// Paint `cmds` on `pixmap` under `base` (viewbox to pixels), with every
/// alpha times `alpha`; texts go to `text`.
pub(crate) fn paint(
    cmds: &[Cmd],
    pixmap: &mut Pixmap,
    base: Transform,
    theme: &Theme,
    alpha: f32,
    text: &mut dyn FnMut(&mut Pixmap, Label),
) {
    let mut stack: Vec<Transform> = Vec::new();
    let mut t = base;
    let rgba = |c: Color| {
        let v = theme.rgba(c);
        let a = ((v & 0xff) as f32 * alpha).round().clamp(0.0, 255.0) as u32;
        (v & !0xff) | a
    };
    let draw = |pixmap: &mut Pixmap, path: Option<Path>, ink: &Ink, t: Transform, closed: bool| {
        let Some(path) = path else { return };
        if let Some(fill) = ink.fill.filter(|_| closed) {
            pixmap.fill_path(&path, &solid(rgba(fill)), FillRule::Winding, t, None);
        }
        if let Some(stroke) = ink.stroke {
            let s = Stroke {
                width: if ink.width > 0.0 { ink.width } else { 1.0 },
                line_cap: match ink.cap {
                    Cap::Butt => LineCap::Butt,
                    Cap::Round => LineCap::Round,
                    Cap::Square => LineCap::Square,
                },
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &solid(rgba(stroke)), &s, t, None);
        }
    };
    for cmd in cmds {
        match cmd {
            Cmd::Save => stack.push(t),
            Cmd::Restore => t = stack.pop().unwrap_or(base),
            Cmd::Translate(x, y) => t = t.pre_translate(*x, *y),
            Cmd::Rotate(a) => t = t.pre_rotate(a.to_degrees()),
            Cmd::Scale(x, y) => t = t.pre_scale(*x, *y),
            Cmd::Line { from, to, ink } => {
                let mut pb = PathBuilder::new();
                pb.move_to(from.0, from.1);
                pb.line_to(to.0, to.1);
                // A line has nothing to fill: its fill strokes it.
                let ink = Ink {
                    stroke: ink.stroke.or(ink.fill),
                    ..*ink
                };
                draw(pixmap, pb.finish(), &ink, t, false);
            }
            Cmd::Rect {
                x,
                y,
                w,
                h,
                radius,
                ink,
            } => {
                let path = crate::doc::rounded(*x, *y, *w, *h, *radius);
                draw(pixmap, path, ink, t, true);
            }
            Cmd::Circle { x, y, r, ink } => {
                draw(pixmap, PathBuilder::from_circle(*x, *y, *r), ink, t, true);
            }
            Cmd::Arc {
                x,
                y,
                r,
                start,
                end,
                pie,
                ink,
            } => {
                let mut pb = PathBuilder::new();
                if *pie {
                    pb.move_to(*x, *y);
                }
                arc(&mut pb, *x, *y, *r, *start, *end, !*pie);
                if *pie {
                    pb.close();
                }
                draw(pixmap, pb.finish(), ink, t, true);
            }
            Cmd::Path { segs, ink } => {
                let mut pb = PathBuilder::new();
                for s in segs {
                    match *s {
                        Seg::Move(x, y) => pb.move_to(x, y),
                        Seg::Line(x, y) => pb.line_to(x, y),
                        Seg::Quad(a, b, x, y) => pb.quad_to(a, b, x, y),
                        Seg::Cubic(a, b, c, d, x, y) => pb.cubic_to(a, b, c, d, x, y),
                        Seg::Close => pb.close(),
                    }
                }
                draw(pixmap, pb.finish(), ink, t, true);
            }
            Cmd::Text {
                x,
                y,
                text: s,
                size,
                color,
                bold,
                align,
            } => {
                let mut p = resvg::tiny_skia::Point::from_xy(*x, *y);
                t.map_point(&mut p);
                // The transform's scale, without its turn.
                let k = (t.sx * t.sy - t.kx * t.ky).abs().sqrt();
                let c = color.unwrap_or(Color::Theme(super::tree::ThemeColor::Text));
                text(
                    pixmap,
                    Label {
                        x: p.x,
                        y: p.y,
                        size: size * k,
                        text: s,
                        rgba: rgba(c),
                        bold: *bold,
                        align: *align,
                    },
                );
            }
        }
    }
}

pub(crate) fn solid(rgba: u32) -> Paint<'static> {
    crate::doc::solid(rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        let segs = parse_path("M 1 2 l 3 4 H 10 z").unwrap();
        assert_eq!(
            segs,
            [
                Seg::Move(1.0, 2.0),
                Seg::Line(4.0, 6.0),
                Seg::Line(10.0, 6.0),
                Seg::Close
            ]
        );
        assert!(parse_path("M 1").is_err());
        // Arcs become curves.
        let arc = parse_path("M 0 0 A 5 5 0 0 1 10 0").unwrap();
        assert!(arc.iter().any(|s| matches!(s, Seg::Cubic(..))));
    }

    #[test]
    fn arcs_reach_their_ends() {
        let mut pb = PathBuilder::new();
        arc(&mut pb, 10.0, 10.0, 5.0, 0.0, std::f32::consts::PI, true);
        let path = pb.finish().unwrap();
        let last = path.points().last().unwrap();
        assert!((last.x - 5.0).abs() < 1e-3 && (last.y - 10.0).abs() < 1e-3);
        // Through the bottom (y down: clockwise from 3 o'clock).
        let b = path.bounds();
        assert!((b.bottom() - 15.0).abs() < 0.1 && b.top() > 9.9);
    }
}
