//! Sheets of paper, as wl-note shows its notes: tucked behind a screen
//! edge, coming out of it as far as the note is out.
//!
//! A sheet is its color with a slight vertical gradient (lighter at the
//! top, as if lit from above) and a grain: a tileable value noise at a few
//! percent of contrast, made once, that makes it read as paper rather than
//! a rectangle. Its corners are rounded by `radius`, and the bottom corner
//! on the side that comes out of the edge may curl: a lighter triangle
//! folded over, with the corner cut away under it. The shadow (shadow.rs's
//! `icon`, of the sheet's own shape) falls down and away from the edge.
//!
//! A `sliver` is the sheet seen edge-on, as a note's marker is: a little
//! darker, with a 1 px light line along the side that sticks out, where
//! the paper's thickness catches the light. An `accent` is another color
//! for the sliver and the margin (the body keeps the paper's, so the text
//! on it keeps its contrast).
//!
//! A sheet may have a `margin`: a strip on the side that comes out first
//! (the side away from the edge), with the note's title set in it turned
//! a quarter (reading up on the right edge, down on the left), cut with a
//! short fade if it is too long, and a faint dashed crease between it and
//! the body. A `bar` along the bottom is set off by a faint line. And it
//! may carry `content`: a document (doc.rs) drawn onto it, as a note's
//! text is, cut with a fade at `clip`.
//!
//! Subsurfaces can't rotate, so a sheet's small tilt is drawn into it,
//! its content and label included.

use std::sync::OnceLock;

use resvg::tiny_skia::{
    Color, FillRule, FilterQuality, GradientStop, IntSize, LineCap, LinearGradient, Mask, Paint,
    PathBuilder, Pixmap, PixmapPaint, Point, Rect as SkRect, SpreadMode, Stroke, StrokeDash,
    Transform,
};

use crate::doc::{self, Doc, Hit};
use crate::image::Image;
use crate::shadow::{self, IconShadow};
use crate::text;

/// The screen edge a sheet is tucked behind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    #[default]
    Right,
}

/// What to draw. Sizes are logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Paper {
    pub w: i32,
    pub h: i32,
    /// 0xRRGGBBAA, not premultiplied.
    pub color: u32,
    pub edge: Edge,
    /// Degrees, clockwise.
    pub tilt: f64,
    /// Drawn edge-on: darker, with a light line.
    pub sliver: bool,
    /// The color of a sliver and of the margin instead of `color`, which
    /// the body keeps (an overdue note's marker and margin).
    pub accent: Option<u32>,
    /// The corners' radius.
    pub radius: f64,
    /// The bottom corner on the side that comes out curls.
    pub curl: bool,
    /// A strip on the side that comes out, with a label.
    pub margin: Option<Margin>,
    /// A bar this high along the bottom (beside the margin), set off by a
    /// faint line; 0 for none.
    pub bar: i32,
    /// The shadow; its x offset is away from the edge (positive moves it
    /// onto the screen).
    pub shadow: Option<IconShadow>,
    pub scale: i32,
    /// Drawn onto the sheet.
    pub content: Option<Content>,
}

/// A sheet's margin.
#[derive(Clone, Debug, PartialEq)]
pub struct Margin {
    /// Across the sheet, from the side that comes out.
    pub width: i32,
    /// The crease's color (0xRRGGBBAA), drawn faint.
    pub ink: u32,
    /// A dashed crease between the margin and the body.
    pub crease: bool,
    pub label: Option<Label>,
}

/// A label set in a margin, turned a quarter.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    /// As shown (uppercase it first, say).
    pub text: String,
    /// Its background is left out.
    pub style: text::Style,
    /// Between letters, ems.
    pub spacing: f32,
    /// Where it may be along the margin: from `top`, `len` long. A label
    /// that fits ends at `top` on the right edge (it reads up) and starts
    /// there on the left; a longer one is cut at `top + len` (right: at
    /// `top`) with a fade.
    pub top: i32,
    pub len: i32,
}

/// A document on a sheet, its top left corner at (`x`, `y`) logical
/// pixels from the sheet's (before the tilt).
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub doc: Doc,
    pub x: i32,
    pub y: i32,
    /// Cut this far below `y`, with a fade, if it is longer.
    pub clip: Option<i32>,
}

/// A rendered sheet, and where its image goes relative to the sheet's top
/// left corner (it reaches past it for the tilt and the shadow).
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub image: Image,
    pub rect: shadow::Rect,
    /// The content's checkboxes and links, in sheet coordinates (the tilt
    /// left out).
    pub hits: Vec<Hit>,
}

/// The grain's contrast: pixels are lightened or darkened by up to this.
const GRAIN: f32 = 0.035;
/// The grain's tile, in buffer pixels.
const TILE: usize = 128;
/// Light from above: the top this much lighter, the bottom darker.
const LIGHT: f64 = 0.06;
/// How much darker a sliver is.
const EDGE_ON: f64 = 0.1;
/// The fades that cut content and labels, logical pixels.
const CONTENT_FADE: u32 = 28;
const LABEL_FADE: u32 = 12;
/// A quarter circle as a cubic Bézier: the controls' distance, of the
/// radius.
const KAPPA: f32 = 0.5523;

impl Paper {
    /// The curled corner's size.
    fn curl_size(&self) -> f32 {
        (self.w.min(self.h) as f32 * 0.12).clamp(5.0, 12.0)
    }

    /// The margins the tilt needs around the sheet, logical pixels.
    fn tilt_pad(&self) -> (i32, i32) {
        let t = self.tilt.to_radians().abs();
        let (w, h) = (self.w as f64, self.h as f64);
        let bw = w * t.cos() + h * t.sin();
        let bh = w * t.sin() + h * t.cos();
        let pad = |b: f64, s: f64| ((b - s) / 2.0).max(0.0).ceil() as i32 + 1;
        (pad(bw, w), pad(bh, h))
    }

    /// The shadow as `shadow::icon` takes it: moved away from the edge.
    fn placed_shadow(&self) -> Option<IconShadow> {
        self.shadow.map(|s| IconShadow {
            offset: match self.edge {
                Edge::Right => (-s.offset.0, s.offset.1),
                Edge::Left => s.offset,
            },
            ..s
        })
    }

    /// Where the margin is across the sheet: (from, to).
    pub fn margin_x(&self) -> Option<(f32, f32)> {
        let m = self.margin.as_ref()?;
        let (w, mw) = (self.w as f32, m.width.clamp(0, self.w) as f32);
        Some(match self.edge {
            Edge::Right => (0.0, mw),
            Edge::Left => (w - mw, w),
        })
    }

    /// Where the body is across the sheet: beside the margin.
    pub fn body_x(&self) -> (f32, f32) {
        let w = self.w as f32;
        match (self.margin_x(), self.edge) {
            (None, _) => (0.0, w),
            (Some((_, to)), Edge::Right) => (to, w),
            (Some((from, _)), Edge::Left) => (0.0, from),
        }
    }
}

/// Render a sheet without its content or label.
pub fn render(p: &Paper) -> Rendered {
    render_with(p, None, None)
}

/// Render a sheet with its content and its margin's label, rendered
/// already (`p.content`'s doc, and `p.margin`'s label with
/// `text::Text::label`).
pub fn render_with(
    p: &Paper,
    content: Option<&doc::Rendered>,
    label: Option<&text::Pill>,
) -> Rendered {
    let (w, h) = (p.w.max(1), p.h.max(1));
    let scale = p.scale.max(1);
    let (px, py) = p.tilt_pad();
    let (bw, bh) = (((w + 2 * px) * scale) as u32, ((h + 2 * py) * scale) as u32);
    let placed = p.content.as_ref().zip(content);
    let sheet = draw(p, (px, py), scale, (bw, bh), placed, label);
    let (image, pad) = match p.placed_shadow() {
        Some(s) => (shadow::icon(&sheet, &s, scale), s.pad()),
        None => (sheet, 0),
    };
    let hits = placed.map_or(Vec::new(), |(c, r)| {
        let clip = c.clip.map_or(f64::INFINITY, |clip| clip as f64);
        (r.hits.iter())
            .filter(|hit| hit.y + hit.h <= clip)
            .map(|hit| hit.offset(c.x as f64, c.y as f64))
            .collect()
    });
    Rendered {
        image,
        rect: shadow::Rect {
            x: -(px + pad),
            y: -(py + pad),
            w: w + 2 * (px + pad),
            h: h + 2 * (py + pad),
        },
        hits,
    }
}

/// The sheet alone, `bw`x`bh` buffer pixels, its top left corner at
/// `pad` logical pixels.
fn draw(
    p: &Paper,
    pad: (i32, i32),
    scale: i32,
    (bw, bh): (u32, u32),
    content: Option<(&Content, &doc::Rendered)>,
    label: Option<&text::Pill>,
) -> Image {
    let blank = || Image {
        width: bw,
        height: bh,
        data: vec![0; (bw * bh * 4) as usize],
    };
    let Some(mut pixmap) = Pixmap::new(bw, bh) else {
        return blank();
    };
    let (w, h, s) = (p.w.max(1) as f32, p.h.max(1) as f32, scale as f32);
    // Sheet coordinates: logical pixels from its top left corner, drawn
    // as if tucked behind the right edge (coming out to the left), and
    // mirrored for the left edge.
    let upright = Transform::from_scale(s, s)
        .pre_translate(pad.0 as f32 + w / 2.0, pad.1 as f32 + h / 2.0)
        .pre_rotate(p.tilt as f32)
        .pre_translate(-w / 2.0, -h / 2.0);
    let mut xf = upright;
    if p.edge == Edge::Left {
        xf = xf.pre_translate(w, 0.0).pre_scale(-1.0, 1.0);
    }
    let r = (p.radius as f32).clamp(0.0, w.min(h) / 2.0);
    let c = match p.curl {
        true => p.curl_size().min(w / 2.0).min(h / 2.0),
        false => 0.0,
    };
    let Some(outline) = outline(w, h, r, c) else {
        return blank();
    };

    let base = match p.sliver {
        true => shade(p.accent.unwrap_or(p.color), -EDGE_ON),
        false => p.color,
    };
    let paint = sheet_paint(base, h);
    pixmap.fill_path(&outline, &paint, FillRule::Winding, xf, None);

    // The margin in the accent color, over the paper's. (Sheet
    // coordinates, mirrored by `xf`: the margin is on the left here.)
    if let (Some(accent), false, Some(m)) = (p.accent, p.sliver, &p.margin)
        && let Some(strip) = SkRect::from_ltrb(0.0, -1.0, m.width.clamp(0, p.w) as f32, h + 1.0)
        && let Some(mut mask) = Mask::new(bw, bh)
    {
        mask.fill_path(&PathBuilder::from_rect(strip), FillRule::Winding, true, xf);
        let paint = sheet_paint(accent, h);
        pixmap.fill_path(&outline, &paint, FillRule::Winding, xf, Some(&mask));
    }

    // The curl: the cut corner folded over, its back catching the light
    // along the fold.
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, h - c);
    pb.line_to(c, h);
    pb.line_to(c, h - c);
    pb.close();
    if let Some(flap) = pb.finish().filter(|_| c > 0.0) {
        let stops = vec![
            GradientStop::new(0.0, color(shade(base, 0.3))),
            GradientStop::new(1.0, color(shade(base, 0.05))),
        ];
        // From the fold's middle toward the flap's tip.
        let fold = Point::from_xy(c / 2.0, h - c / 2.0);
        let tip = Point::from_xy(c, h - c);
        let paint =
            match LinearGradient::new(fold, tip, stops, SpreadMode::Pad, Transform::identity()) {
                Some(shader) => Paint {
                    shader,
                    anti_alias: true,
                    ..Paint::default()
                },
                None => solid(shade(base, 0.2)),
            };
        pixmap.fill_path(&flap, &paint, FillRule::Winding, xf, None);
    }

    if p.sliver
        && let Some(line) = SkRect::from_xywh(0.0, r, 1.0, (h - c.max(r) - r).max(0.0))
    {
        pixmap.fill_rect(line, &solid(0xffffff70), xf, None);
    }

    grain(&mut pixmap);

    // The rest on top of the grain, never mirrored.
    if let Some(m) = &p.margin
        && let Some((from, to)) = p.margin_x()
    {
        if m.crease {
            let x = match p.edge {
                Edge::Right => to,
                Edge::Left => from,
            };
            let mut pb = PathBuilder::new();
            pb.move_to(x, r.max(4.0));
            pb.line_to(x, h - r.max(c).max(4.0));
            if let Some(path) = pb.finish() {
                let stroke = Stroke {
                    width: 1.0,
                    dash: StrokeDash::new(vec![3.0, 3.0], 0.0),
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &solid(faint(m.ink, 0.35)), &stroke, upright, None);
            }
        }
        if let (Some(l), Some(img)) = (&m.label, label) {
            draw_label(&mut pixmap, l, img, p.edge, (from + to) / 2.0, upright, s);
        }
    }
    if p.bar > 0 {
        let (from, to) = p.body_x();
        let y = h - p.bar as f32;
        let ink = p.margin.as_ref().map_or(0x00000040, |m| faint(m.ink, 0.18));
        if let Some(line) = SkRect::from_xywh(from + 8.0, y, (to - from - 16.0).max(0.0), 1.0) {
            pixmap.fill_rect(line, &solid(ink), upright, None);
        }
    }
    if let Some((c, r)) = content {
        draw_content(&mut pixmap, c, r, upright, s, p.tilt != 0.0);
    }
    let mut data = Vec::with_capacity((bw * bh * 4) as usize);
    for px in pixmap.data().as_chunks::<4>().0 {
        // Premultiplied RGBA to premultiplied BGRA.
        data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Image {
        width: bw,
        height: bh,
        data,
    }
}

/// The fill of a sheet `h` high in `base`: a slight vertical gradient,
/// lighter at the top.
fn sheet_paint(base: u32, h: f32) -> Paint<'static> {
    let stops = vec![
        GradientStop::new(0.0, color(shade(base, LIGHT))),
        GradientStop::new(1.0, color(shade(base, -LIGHT * 0.8))),
    ];
    let fill = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(0.0, h),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    );
    match fill {
        Some(shader) => Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        },
        None => solid(base),
    }
}

/// A sheet's outline, `w`x`h`, with corners of radius `r`, the bottom
/// left one (on the side that comes out) cut by a curl of `c` if any.
fn outline(w: f32, h: f32, r: f32, c: f32) -> Option<resvg::tiny_skia::Path> {
    let k = r * (1.0 - KAPPA);
    let mut pb = PathBuilder::new();
    pb.move_to(r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.cubic_to(w - k, 0.0, w, k, w, r);
    pb.line_to(w, h - r);
    pb.cubic_to(w, h - k, w - k, h, w - r, h);
    if c > 0.0 {
        pb.line_to(c, h);
        pb.line_to(0.0, h - c);
    } else {
        pb.line_to(r, h);
        pb.cubic_to(k, h, 0.0, h - k, 0.0, h - r);
    }
    pb.line_to(0.0, r);
    pb.cubic_to(0.0, k, k, 0.0, r, 0.0);
    pb.close();
    pb.finish()
}

/// Set a label turned a quarter in the margin centered on `cx`: reading
/// up on the right edge, down on the left (see `Label`).
fn draw_label(
    pixmap: &mut Pixmap,
    l: &Label,
    img: &text::Pill,
    edge: Edge,
    cx: f32,
    upright: Transform,
    s: f32,
) {
    let keep = (l.len.max(1) as f32 * s) as u32;
    let cut = cut(&img.image, keep, (LABEL_FADE as f32 * s) as u32, false);
    let Some(image) = pixmap_of(&cut) else {
        return;
    };
    let (len, thick) = (cut.width as f32 / s, cut.height as f32 / s);
    let y0 = l.top as f32;
    // Image (u along the text, v down its line) to sheet coordinates.
    let turn = match edge {
        // A quarter counterclockwise: the text's start at the bottom.
        Edge::Right => Transform::from_row(0.0, -1.0, 1.0, 0.0, cx - thick / 2.0, y0 + len),
        // A quarter clockwise: its start at the top.
        Edge::Left => Transform::from_row(0.0, 1.0, -1.0, 0.0, cx + thick / 2.0, y0),
    };
    let xf = upright.pre_concat(turn).pre_scale(1.0 / s, 1.0 / s);
    let paint = PixmapPaint {
        quality: FilterQuality::Bilinear,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, xf, None);
}

/// Draw a rendered document onto the sheet: `upright` maps sheet
/// coordinates to the buffer's (scale `s`, tilt included).
fn draw_content(
    pixmap: &mut Pixmap,
    c: &Content,
    r: &doc::Rendered,
    upright: Transform,
    s: f32,
    tilted: bool,
) {
    let clipped;
    let img = match c.clip {
        Some(clip) => {
            let keep = (clip.max(1) as f32 * s) as u32;
            clipped = cut(&r.image, keep, (CONTENT_FADE as f32 * s) as u32, true);
            &clipped
        }
        None => &r.image,
    };
    let Some(image) = pixmap_of(img) else {
        return;
    };
    let paint = PixmapPaint {
        // Straight, pixels land on pixels: keep the text crisp.
        quality: match tilted {
            true => FilterQuality::Bilinear,
            false => FilterQuality::Nearest,
        },
        ..PixmapPaint::default()
    };
    let xf = upright
        .pre_translate(c.x as f32, c.y as f32)
        .pre_scale(1.0 / s, 1.0 / s);
    pixmap.draw_pixmap(0, 0, image.as_ref(), &paint, xf, None);
}

/// A BGRA image as a pixmap.
fn pixmap_of(img: &Image) -> Option<Pixmap> {
    let size = IntSize::from_wh(img.width, img.height)?;
    let mut rgba = Vec::with_capacity(img.data.len());
    for px in img.data.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Pixmap::from_vec(rgba, size)
}

/// `img` cut to `keep` pixels (rows if `vertical`, else columns) from its
/// start, the last `fade` of them fading out, if it is longer; else as
/// it is.
pub fn cut(img: &Image, keep: u32, fade: u32, vertical: bool) -> Image {
    let len = if vertical { img.height } else { img.width };
    if len <= keep || keep == 0 {
        return img.clone();
    }
    let (w, h) = match vertical {
        true => (img.width, keep),
        false => (keep, img.height),
    };
    let fade = fade.min(keep).max(1);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let at = if vertical { y } else { x };
            let f = match keep - at {
                left if left <= fade => (left as f32 - 0.5) / fade as f32,
                _ => 1.0,
            };
            let i = ((y * img.width + x) * 4) as usize;
            for c in &img.data[i..i + 4] {
                data.push((*c as f32 * f).round() as u8);
            }
        }
    }
    Image {
        width: w,
        height: h,
        data,
    }
}

/// A round button `size` logical pixels across in `color`, with a plus
/// in `glyph`, over its `shadow`. Its rect is relative to the button's
/// top left corner.
pub fn button(
    size: i32,
    color: u32,
    glyph: u32,
    shadow: Option<IconShadow>,
    scale: i32,
) -> Rendered {
    let s = scale.max(1);
    let side = (size.max(1) * s) as u32;
    let mut image = Image {
        width: side,
        height: side,
        data: vec![0; (side * side * 4) as usize],
    };
    if let Some(mut pixmap) = Pixmap::new(side, side) {
        let r = side as f32 / 2.0;
        if let Some(disc) = PathBuilder::from_circle(r, r, r - 0.5) {
            pixmap.fill_path(
                &disc,
                &solid(color),
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        let arm = side as f32 * 0.21;
        let mut pb = PathBuilder::new();
        pb.move_to(r - arm, r);
        pb.line_to(r + arm, r);
        pb.move_to(r, r - arm);
        pb.line_to(r, r + arm);
        if let Some(plus) = pb.finish() {
            let stroke = Stroke {
                width: (side as f32 * 0.085).max(1.5),
                line_cap: LineCap::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(&plus, &solid(glyph), &stroke, Transform::identity(), None);
        }
        let mut data = Vec::with_capacity(image.data.len());
        for px in pixmap.data().as_chunks::<4>().0 {
            data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        image.data = data;
    }
    let (image, pad) = match shadow {
        Some(sh) => (shadow::icon(&image, &sh, s), sh.pad()),
        None => (image, 0),
    };
    Rendered {
        image,
        rect: shadow::Rect {
            x: -pad,
            y: -pad,
            w: size + 2 * pad,
            h: size + 2 * pad,
        },
        hits: Vec::new(),
    }
}

/// A rounded rectangle `w`x`h` logical pixels in `color` (0xRRGGBBAA),
/// with corners of radius `r`, at `scale`: say, a group's backdrop.
pub fn rounded(w: i32, h: i32, r: f64, color: u32, scale: i32) -> Image {
    let s = scale.max(1);
    let (bw, bh) = ((w.max(1) * s) as u32, (h.max(1) * s) as u32);
    let mut image = Image {
        width: bw,
        height: bh,
        data: vec![0; (bw * bh * 4) as usize],
    };
    let Some(mut pixmap) = Pixmap::new(bw, bh) else {
        return image;
    };
    let r = (r as f32 * s as f32).min(bw.min(bh) as f32 / 2.0);
    if let Some(path) = outline(bw as f32, bh as f32, r, 0.0) {
        pixmap.fill_path(
            &path,
            &solid(color),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    let mut data = Vec::with_capacity(image.data.len());
    for px in pixmap.data().as_chunks::<4>().0 {
        data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    image.data = data;
    image
}

/// A shelf's board is this thick: a lighter top face, then its front.
pub const BOARD: i32 = 6;
const BOARD_TOP: f32 = 2.0;
/// The shadow a board casts on the paper under it.
const BOARD_SHADOW: f32 = 5.0;

/// A shelf's board, `w` logical pixels long, of wood (or kraft) in
/// `color`, turned `tilt` degrees about its middle as the sheet it is on
/// is, at `scale`: a top face catching the light, a darker front edge
/// and a soft shadow under it. Returns the image and where it goes from
/// the board's top left corner (before the tilt).
pub fn board(w: i32, color: u32, tilt: f64, scale: i32) -> (Image, shadow::Rect) {
    let s = scale.max(1);
    let (wf, hf) = (w.max(1) as f32, BOARD as f32);
    let sin = tilt.to_radians().sin().abs() as f32;
    let pad_x = (BOARD_SHADOW * sin).ceil() as i32 + 1;
    let pad_y = (wf / 2.0 * sin + BOARD_SHADOW).ceil() as i32 + 1;
    let rect = shadow::Rect {
        x: -pad_x,
        y: -pad_y,
        w: w.max(1) + 2 * pad_x,
        h: BOARD + 2 * pad_y,
    };
    let (bw, bh) = ((rect.w * s) as u32, (rect.h * s) as u32);
    let blank = Image {
        width: bw,
        height: bh,
        data: vec![0; (bw * bh * 4) as usize],
    };
    let Some(mut pixmap) = Pixmap::new(bw, bh) else {
        return (blank, rect);
    };
    let xf = Transform::from_scale(s as f32, s as f32)
        .pre_translate(pad_x as f32 + wf / 2.0, pad_y as f32 + hf / 2.0)
        .pre_rotate(tilt as f32)
        .pre_translate(-wf / 2.0, -hf / 2.0);
    let gradient = |y0: f32, y1: f32, from: u32, to: u32| {
        let stops = vec![
            GradientStop::new(0.0, self::color(from)),
            GradientStop::new(1.0, self::color(to)),
        ];
        let (a, b) = (Point::from_xy(0.0, y0), Point::from_xy(0.0, y1));
        match LinearGradient::new(a, b, stops, SpreadMode::Pad, Transform::identity()) {
            Some(shader) => Paint {
                shader,
                anti_alias: true,
                ..Paint::default()
            },
            None => solid(from),
        }
    };
    let parts = [
        // The shadow, fading down the paper.
        (
            SkRect::from_xywh(1.0, hf, wf - 2.0, BOARD_SHADOW),
            gradient(hf, hf + BOARD_SHADOW, 0x00000048, 0x00000000),
        ),
        (
            SkRect::from_xywh(0.0, 0.0, wf, BOARD_TOP),
            solid(shade(color, 0.14)),
        ),
        (
            SkRect::from_xywh(0.0, BOARD_TOP, wf, hf - BOARD_TOP),
            gradient(BOARD_TOP, hf, shade(color, -0.06), shade(color, -0.2)),
        ),
    ];
    for (r, paint) in parts {
        if let Some(r) = r {
            pixmap.fill_rect(r, &paint, xf, None);
        }
    }
    let mut data = Vec::with_capacity(blank.data.len());
    for px in pixmap.data().as_chunks::<4>().0 {
        data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    (
        Image {
            width: bw,
            height: bh,
            data,
        },
        rect,
    )
}

/// `rgba` with its alpha times `f`.
fn faint(rgba: u32, f: f64) -> u32 {
    let a = ((rgba & 0xff) as f64 * f).round().clamp(0.0, 255.0) as u32;
    (rgba & !0xff) | a
}

/// Lighten (`amount` > 0, toward white) or darken (toward black) an
/// 0xRRGGBBAA color, keeping its alpha.
fn shade(rgba: u32, amount: f64) -> u32 {
    let [r, g, b, a] = rgba.to_be_bytes();
    let f = |c: u8| {
        let c = c as f64;
        let v = match amount >= 0.0 {
            true => c + (255.0 - c) * amount,
            false => c * (1.0 + amount),
        };
        v.round().clamp(0.0, 255.0) as u8
    };
    u32::from_be_bytes([f(r), f(g), f(b), a])
}

fn color(rgba: u32) -> Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

fn solid(rgba: u32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(rgba));
    p.anti_alias = true;
    p
}

/// Lighten and darken the drawn pixels by the grain.
fn grain(pixmap: &mut Pixmap) {
    let tile = grain_tile();
    let w = pixmap.width() as usize;
    for (i, px) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let a = px[3];
        if a == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let f = 1.0 + tile[(y % TILE) * TILE + x % TILE] * GRAIN;
        for c in &mut px[..3] {
            *c = (*c as f32 * f).round().clamp(0.0, a as f32) as u8;
        }
    }
}

/// The grain: -1 to 1 per pixel of a `TILE`-square tile that repeats
/// seamlessly. Coarse value noise (fibers) under fine noise (tooth).
fn grain_tile() -> &'static [f32] {
    static TILE_DATA: OnceLock<Vec<f32>> = OnceLock::new();
    TILE_DATA.get_or_init(|| {
        const CELL: usize = 8;
        let cells = TILE / CELL;
        let lattice = |x: usize, y: usize| unit(hash((x % cells) as u32, (y % cells) as u32, 1));
        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
        let mut out = Vec::with_capacity(TILE * TILE);
        for y in 0..TILE {
            for x in 0..TILE {
                let (cx, cy) = (x / CELL, y / CELL);
                let tx = smooth((x % CELL) as f32 / CELL as f32);
                let ty = smooth((y % CELL) as f32 / CELL as f32);
                let top = lattice(cx, cy) + (lattice(cx + 1, cy) - lattice(cx, cy)) * tx;
                let bottom =
                    lattice(cx, cy + 1) + (lattice(cx + 1, cy + 1) - lattice(cx, cy + 1)) * tx;
                let coarse = top + (bottom - top) * ty;
                let fine = unit(hash(x as u32, y as u32, 2));
                out.push(0.4 * coarse + 0.6 * fine);
            }
        }
        out
    })
}

/// A hash of a lattice point, for noise.
fn hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// A hash as -1 to 1.
fn unit(h: u32) -> f32 {
    h as f32 / u32::MAX as f32 * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper() -> Paper {
        Paper {
            w: 120,
            h: 60,
            color: 0xfff3a0ff,
            edge: Edge::Right,
            tilt: 0.0,
            sliver: false,
            accent: None,
            radius: 1.5,
            curl: true,
            margin: None,
            bar: 0,
            shadow: None,
            scale: 1,
            content: None,
        }
    }

    /// The BGRA pixel at sheet coordinates (`x`, `y`), logical pixels.
    fn at(r: &Rendered, scale: i32, x: i32, y: i32) -> [u8; 4] {
        let (i, j) = ((x - r.rect.x) * scale, (y - r.rect.y) * scale);
        let k = ((j as u32 * r.image.width + i as u32) * 4) as usize;
        r.image.data[k..k + 4].try_into().unwrap()
    }

    /// A white label image, `w`x`h` buffer pixels.
    fn label_image(w: u32, h: u32) -> text::Pill {
        text::Pill {
            width: w as i32,
            height: h as i32,
            image: Image {
                width: w,
                height: h,
                data: vec![255; (w * h * 4) as usize],
            },
        }
    }

    #[test]
    fn sizes_match_the_rect() {
        for (tilt, shadow, scale) in [
            (0.0, None, 1),
            (1.5, None, 2),
            (-1.5, Some((8, (3, 4))), 2),
            (0.0, Some((0, (0, 0))), 3),
        ] {
            let p = Paper {
                tilt,
                shadow: shadow.map(|(softness, offset)| IconShadow {
                    color: 0x00000060,
                    softness,
                    offset,
                }),
                scale,
                ..paper()
            };
            let r = render(&p);
            assert_eq!(r.image.width, (r.rect.w * scale) as u32);
            assert_eq!(r.image.height, (r.rect.h * scale) as u32);
            assert_eq!(
                r.image.data.len(),
                (r.image.width * r.image.height * 4) as usize
            );
            assert!(r.rect.x < 0 && r.rect.y < 0);
            assert_eq!(r.rect.w + 2 * r.rect.x, 120);
            assert_eq!(r.rect.h + 2 * r.rect.y, 60);
        }
        // Tilted sheets need more room.
        let flat = render(&paper()).rect;
        let tilted = render(&Paper {
            tilt: 3.0,
            ..paper()
        })
        .rect;
        assert!(tilted.y < flat.y, "{tilted:?} {flat:?}");
    }

    #[test]
    fn the_color_with_a_grain_and_light_from_above() {
        let r = render(&paper());
        let [b, g, red, a] = at(&r, 1, 60, 30);
        assert_eq!(a, 255);
        let near = |v: u8, want: i32| (v as i32 - want).abs() <= 16;
        assert!(
            near(red, 0xff) && near(g, 0xf3) && near(b, 0xa0),
            "{:?}",
            [red, g, b]
        );
        // Lighter at the top than at the bottom, on average over a row.
        let row = |y: i32| (10..110).map(|x| at(&r, 1, x, y)[0] as u32).sum::<u32>();
        assert!(row(2) > row(57));
        // Grain: not every pixel the same.
        let mid: Vec<[u8; 4]> = (10..110).map(|x| at(&r, 1, x, 30)).collect();
        assert!(mid.iter().any(|p| p != &mid[0]));
    }

    #[test]
    fn the_curl_cuts_the_corner_that_comes_out() {
        let right = render(&paper());
        // Bottom left is cut, bottom right is paper.
        assert_eq!(at(&right, 1, 1, 59)[3], 0);
        assert_eq!(at(&right, 1, 118, 58)[3], 255);
        let left = render(&Paper {
            edge: Edge::Left,
            ..paper()
        });
        assert_eq!(at(&left, 1, 118, 59)[3], 0);
        assert_eq!(at(&left, 1, 1, 58)[3], 255);
        // The shapes mirror each other.
        for y in 0..60 {
            for x in 0..120 {
                let (a, b) = (at(&right, 1, x, y)[3], at(&left, 1, 119 - x, y)[3]);
                // Antialiasing may differ a little at the curves.
                assert!((a as i32 - b as i32).abs() <= 24, "({x}, {y}): {a} {b}");
            }
        }
    }

    #[test]
    fn rounded_corners() {
        let r = render(&Paper {
            radius: 8.0,
            curl: false,
            ..paper()
        });
        for (x, y) in [(0, 0), (119, 0), (0, 59), (119, 59)] {
            assert_eq!(at(&r, 1, x, y)[3], 0, "({x}, {y})");
        }
        assert_eq!(at(&r, 1, 8, 1)[3], 255);
        assert_eq!(at(&r, 1, 1, 30)[3], 255);
    }

    #[test]
    fn slivers_are_darker_with_a_light_line() {
        let sheet = render(&paper());
        let sliver = render(&Paper {
            sliver: true,
            ..paper()
        });
        let sum = |r: &Rendered, x: i32| (20..40).map(|y| at(r, 1, x, y)[1] as u32).sum::<u32>();
        assert!(sum(&sliver, 60) < sum(&sheet, 60));
        // The line along the side that comes out.
        assert!(sum(&sliver, 0) > sum(&sliver, 3));
    }

    #[test]
    fn the_shadow_falls_away_from_the_edge() {
        let s = IconShadow {
            color: 0x000000ff,
            softness: 6,
            offset: (4, 4),
        };
        let r = render(&Paper {
            shadow: Some(s),
            ..paper()
        });
        // Right edge: the shadow is on the left, toward the screen.
        let (left, right) = (at(&r, 1, -3, 30)[3], at(&r, 1, 122, 30)[3]);
        assert!(left > right, "{left} {right}");
        let r = render(&Paper {
            shadow: Some(s),
            edge: Edge::Left,
            ..paper()
        });
        let (left, right) = (at(&r, 1, -3, 30)[3], at(&r, 1, 122, 30)[3]);
        assert!(right > left, "{left} {right}");
    }

    #[test]
    fn labels_turn_into_the_margin() {
        let label = Label {
            text: "X".into(),
            style: text::Style {
                font: "sans-serif".into(),
                size: 10.0,
                color: 0xffffffff,
                background: 0,
            },
            spacing: 0.1,
            top: 6,
            len: 30,
        };
        let margin = Margin {
            width: 20,
            ink: 0x000000ff,
            crease: true,
            label: Some(label),
        };
        let p = Paper {
            color: 0x000000ff,
            margin: Some(margin),
            curl: false,
            ..paper()
        };
        assert_eq!(p.margin_x(), Some((0.0, 20.0)));
        assert_eq!(p.body_x(), (20.0, 120.0));
        // A white label 50 long (cut to 30) and 10 thick, turned.
        let r = render_with(&p, None, Some(&label_image(50, 10)));
        let white = |x, y| at(&r, 1, x, y)[2] > 200;
        // In the margin from the top, 30 long, 10 wide around its middle.
        assert!(white(10, 25) && white(6, 25) && white(14, 25));
        assert!(!white(10, 40) && !white(30, 20) && !white(2, 20));
        // Faded toward the top, where it is cut (it reads up).
        assert!(at(&r, 1, 10, 7)[2] < at(&r, 1, 10, 30)[2]);
        // On the left edge, in the margin on the right, faded at the bottom.
        let left = Paper {
            edge: Edge::Left,
            ..p.clone()
        };
        assert_eq!(left.margin_x(), Some((100.0, 120.0)));
        assert_eq!(left.body_x(), (0.0, 100.0));
        let r = render_with(&left, None, Some(&label_image(50, 10)));
        let white = |x, y| at(&r, 1, x, y)[2] > 200;
        assert!(white(110, 10) && white(110, 20) && !white(10, 20));
        assert!(at(&r, 1, 110, 35)[2] < at(&r, 1, 110, 10)[2]);
    }

    #[test]
    fn cutting_with_a_fade() {
        let img = Image {
            width: 4,
            height: 10,
            data: vec![200; 4 * 10 * 4],
        };
        let c = cut(&img, 6, 3, true);
        assert_eq!((c.width, c.height), (4, 6));
        let row = |y: u32| c.data[(y * 4 * 4) as usize];
        assert_eq!(row(2), 200);
        assert!(row(3) > row(4) && row(4) > row(5) && row(5) > 0);
        // Short enough: as it is.
        assert_eq!(cut(&img, 10, 3, true), img);
        let c = cut(&img, 2, 1, false);
        assert_eq!((c.width, c.height), (2, 10));
    }

    #[test]
    fn buttons() {
        let b = button(20, 0xffffffff, 0x000000ff, None, 2);
        assert_eq!((b.image.width, b.image.height), (40, 40));
        let px = |x: u32, y: u32| &b.image.data[((y * 40 + x) * 4) as usize..][..4];
        // Clear corners, the plus in the middle, the disc around it.
        assert_eq!(px(0, 0)[3], 0);
        assert_eq!(px(20, 20)[..3], [0, 0, 0]);
        assert_eq!(px(20, 4), [255, 255, 255, 255]);
        let shadowed = button(
            20,
            0xffffffff,
            0x000000ff,
            Some(IconShadow {
                color: 0x00000080,
                softness: 4,
                offset: (0, 2),
            }),
            1,
        );
        assert!(shadowed.rect.x < 0 && shadowed.rect.w > 20);
        assert_eq!(shadowed.image.width as i32, shadowed.rect.w);
    }

    #[test]
    fn grain_tiles() {
        let t = grain_tile();
        assert_eq!(t.len(), TILE * TILE);
        assert!(t.iter().all(|v| (-1.0..=1.0).contains(v)));
        let mean = t.iter().sum::<f32>() / t.len() as f32;
        assert!(mean.abs() < 0.1, "{mean}");
    }

    #[test]
    fn shades() {
        assert_eq!(shade(0x808080ff, 0.0), 0x808080ff);
        assert_eq!(shade(0x000000ff, 0.5), 0x808080ff);
        assert_eq!(shade(0xffffff40, -0.5), 0x80808040);
    }

    #[test]
    fn accent_colors_the_sliver_and_the_margin() {
        let accent = 0xd64541ff;
        // Red: little green, where the paper is yellow.
        let red = |r: &Rendered, x, y| at(r, 1, x, y)[1] < 120;
        // Edge-on, all of it.
        let plain = render(&Paper {
            sliver: true,
            ..paper()
        });
        let sliver = render(&Paper {
            sliver: true,
            accent: Some(accent),
            ..paper()
        });
        assert!(!red(&plain, 60, 30) && red(&sliver, 60, 30));
        // With a margin: it, and not the body.
        let margin = Margin {
            width: 20,
            ink: 0x000000ff,
            crease: false,
            label: None,
        };
        let with = |edge, accent| Paper {
            edge,
            accent,
            margin: Some(margin.clone()),
            curl: false,
            ..paper()
        };
        let r = render(&with(Edge::Right, Some(accent)));
        assert!(red(&r, 10, 30) && !red(&r, 60, 30) && !red(&r, 100, 30));
        let r = render(&with(Edge::Left, Some(accent)));
        assert!(red(&r, 110, 30) && !red(&r, 60, 30) && !red(&r, 10, 30));
        // No margin and no sliver: nothing to color.
        let r = render(&Paper {
            accent: Some(accent),
            ..paper()
        });
        assert!(!red(&r, 10, 30) && !red(&r, 60, 30));
        // The accent is part of what a sheet is drawn from.
        assert_ne!(with(Edge::Right, Some(accent)), with(Edge::Right, None));
    }

    #[test]
    fn boards_turn_with_the_sheet() {
        let (img, rect) = board(200, 0xd4b483ff, 0.0, 2);
        assert_eq!((img.width as i32, img.height as i32), (rect.w * 2, rect.h * 2));
        let alpha = |img: &Image, x: i32, y: i32| img.data[((y * img.width as i32 + x) * 4 + 3) as usize];
        // Straight: the board is opaque from its top, its shadow fades under it.
        let (x0, y0) = (-rect.x * 2, -rect.y * 2);
        assert_eq!(alpha(&img, x0 + 200, y0 + 1), 255);
        assert_eq!(alpha(&img, x0 + 200, y0 + BOARD * 2 - 1), 255);
        assert!((1..255).contains(&alpha(&img, x0 + 200, y0 + BOARD * 2 + 1)));
        assert_eq!(alpha(&img, x0 + 200, y0 - 2), 0);
        // Turned, it needs more room, and its ends are higher or lower.
        let (turned, r) = board(200, 0xd4b483ff, 3.0, 1);
        assert!(r.h > rect.h);
        let col = |x: i32| (0..turned.height as i32).find(|&y| alpha(&turned, x, y) == 255);
        assert!(col(-r.x + 5).unwrap() < col(-r.x + 195).unwrap());
    }
}
