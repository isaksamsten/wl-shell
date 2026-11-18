//! Documents: wrapped paragraphs of styled text, as wl-note shows a note's
//! markdown (its preview, and the focused note). The program parses the
//! markdown into [`Block`]s; this lays them out with cosmic-text and draws
//! them: list markers, checkboxes, code backgrounds, quote bars, rules and
//! underlined links. It also says where the checkboxes and links are
//! ([`Hit`]), so clicks on them can be told apart.
//!
//! Everything is laid out at buffer scale; sizes in the API are logical
//! pixels. Lines that would end below `max_height` are left out, and the
//! result says it was `cut`.

use cosmic_text::{
    Attrs, Buffer, Color, Family, Metrics, Shaping, Style as Slant, Weight, Wrap,
};
use resvg::tiny_skia::{
    BlendMode, FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform,
};

use crate::image::Image;
use crate::text::{Text, blend, family, line_height};

/// Glyph metadata: struck through.
const STRIKE: usize = 1 << 31;
/// Glyph metadata: the link's index plus one (0: none).
const LINK_MASK: usize = (1 << 30) - 1;
/// How much dimmed text keeps of its alpha.
const DIM: f64 = 0.55;

/// How a document looks.
#[derive(Clone, Debug, PartialEq)]
pub struct DocStyle {
    pub font: String,
    /// Logical pixels.
    pub size: f64,
    /// 0xRRGGBBAA, not premultiplied.
    pub color: u32,
    pub link: u32,
}

/// What to render.
#[derive(Clone, Debug, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub style: DocStyle,
    /// Logical pixels: the width lines wrap at.
    pub width: i32,
    /// Logical pixels: lines ending below are left out.
    pub max_height: i32,
    pub scale: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Paragraph,
    /// 1 to 6.
    Heading(u8),
    /// A code block: monospace, on a faint background.
    Code,
    /// A horizontal rule (its runs are ignored).
    Rule,
}

/// What stands before a block: a list item's marker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marker {
    None,
    Bullet,
    Number(u64),
    /// A checkbox, checked or not. Clicks on it are `Target::Task(n)` for
    /// the n-th task of the document.
    Task(bool),
}

/// A paragraph-like piece of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub kind: Kind,
    pub runs: Vec<Run>,
    /// Indentation levels: list nesting and quotes.
    pub depth: u8,
    /// In a block quote: a bar in the margin.
    pub quote: bool,
    pub marker: Marker,
    /// Space above it, as between paragraphs (not between tight list
    /// items).
    pub gap: bool,
}

impl Block {
    /// A plain paragraph of `runs`.
    pub fn new(kind: Kind, runs: Vec<Run>) -> Self {
        Self {
            kind,
            runs,
            depth: 0,
            quote: false,
            marker: Marker::None,
            gap: true,
        }
    }
}

/// A piece of text in one style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// Monospace.
    pub code: bool,
    pub strike: bool,
    /// Fainter, as a checked task's text.
    pub dim: bool,
    /// A link, by its index in the program's list: drawn in the link
    /// color, underlined, and a `Target::Link`.
    pub link: Option<u32>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }
}

/// What a click on a document can be on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// The n-th checkbox.
    Task(u32),
    /// The link with this index.
    Link(u32),
}

/// Where a target is: logical pixels from the document's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub target: Target,
}

impl Hit {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    /// Moved by (`dx`, `dy`).
    pub fn offset(self, dx: f64, dy: f64) -> Self {
        Self {
            x: self.x + dx,
            y: self.y + dy,
            ..self
        }
    }
}

/// The target at (`x`, `y`), if any.
pub fn hit(hits: &[Hit], x: f64, y: f64) -> Option<Target> {
    hits.iter().find(|h| h.contains(x, y)).map(|h| h.target)
}

/// A rendered document.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// `width` x `height` times the scale.
    pub image: Image,
    /// Logical size.
    pub width: i32,
    pub height: i32,
    pub hits: Vec<Hit>,
    /// Lines were left out for `max_height`.
    pub cut: bool,
}

/// A block laid out: physical pixels.
struct Laid {
    block: usize,
    /// Its indentation and top.
    left: f32,
    top: f32,
    height: f32,
    /// The text's buffer and where it is drawn.
    text: Option<(Buffer, f32, f32)>,
    /// The first line's height, for the marker.
    first: f32,
    /// An ordered list's number, and where it is drawn.
    number: Option<(Buffer, f32)>,
}

impl Text {
    /// Lay out and draw `doc`.
    pub fn doc(&mut self, doc: &Doc) -> Rendered {
        let s = doc.scale.max(1) as f32;
        let st = &doc.style;
        let size = st.size as f32 * s;
        let lh = line_height(st.size) as f32 * s;
        let width = (doc.width.max(1) as f32 * s).round();
        let max_h = doc.max_height.max(1) as f32 * s;
        let indent = (size * 1.5).round();
        let gap = (size * 0.6).round();
        let code_pad = (size * 0.35).round();

        let mut laid: Vec<Laid> = Vec::new();
        let mut y = 0.0f32;
        for (i, b) in doc.blocks.iter().enumerate() {
            if y >= max_h {
                break;
            }
            if b.gap && i > 0 {
                y += gap;
            }
            let left = b.depth as f32 * indent;
            if b.kind == Kind::Rule {
                let h = (lh * 0.8).round();
                laid.push(Laid {
                    block: i,
                    left,
                    top: y,
                    height: h,
                    text: None,
                    first: h,
                    number: None,
                });
                y += h;
                continue;
            }
            let (fsize, flh, bold) = match b.kind {
                Kind::Heading(1) => (size * 1.3, (lh * 1.3).round(), true),
                Kind::Heading(2) => (size * 1.15, (lh * 1.15).round(), true),
                Kind::Heading(_) => (size, lh, true),
                _ => (size, lh, false),
            };
            let marker = if b.marker == Marker::None { 0.0 } else { indent };
            let pad = if b.kind == Kind::Code { code_pad } else { 0.0 };
            let tx = left + marker + pad;
            let avail = (width - tx - pad).max(size);
            let buffer = self.paragraph(b, st, fsize, flh, bold, avail);
            let h = (buffer.layout_runs())
                .map(|r| r.line_top + r.line_height)
                .fold(0.0, f32::max)
                .max(flh);
            let number = match &b.marker {
                Marker::Number(n) => {
                    let mut nb = Buffer::new(&mut self.fonts, Metrics::new(fsize, flh));
                    nb.set_wrap(&mut self.fonts, Wrap::None);
                    nb.set_size(&mut self.fonts, None, None);
                    let attrs = Attrs::new().family(family(&st.font)).color(color(st.color));
                    let text = format!("{n}.");
                    nb.set_rich_text(
                        &mut self.fonts,
                        [(text.as_str(), attrs.clone())],
                        &attrs,
                        Shaping::Advanced,
                        None,
                    );
                    let w = nb.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
                    Some((nb, left + marker - w - (size * 0.35).round()))
                }
                _ => None,
            };
            laid.push(Laid {
                block: i,
                left,
                top: y,
                height: h + 2.0 * pad,
                text: Some((buffer, tx, y + pad)),
                first: flh,
                number,
            });
            y += h + 2.0 * pad;
        }
        let mut cut = y > max_h + 0.5 || laid.len() < doc.blocks.len();
        // Whole logical pixels, so the image maps onto them exactly.
        let total = ((y.min(max_h) / s).ceil() * s).max(s);
        let (bw, bh) = (width.max(1.0) as u32, total as u32);

        // Shapes first, with tiny-skia; the text goes on top.
        let mut hits = Vec::new();
        let mut image = match Pixmap::new(bw, bh) {
            Some(mut pixmap) => {
                self.shapes(doc, &laid, &mut pixmap, &mut hits, indent, size, s);
                bgra(&pixmap)
            }
            None => Image {
                width: bw,
                height: bh,
                data: vec![0; (bw * bh * 4) as usize],
            },
        };
        let base = color(st.color);
        for l in &laid {
            if let Some((nb, nx)) = &l.number
                && let Some((_, _, ty)) = &l.text
                && ty + l.first <= max_h + 0.5
            {
                self.glyphs_at(nb, &mut image, *nx, *ty, base, max_h, &mut cut);
            }
            let Some((buffer, tx, ty)) = &l.text else {
                continue;
            };
            self.glyphs_at(buffer, &mut image, *tx, *ty, base, max_h, &mut cut);
            decorate(buffer, &mut image, *tx, *ty, base, size, s, max_h, &mut hits);
        }
        Rendered {
            image,
            width: doc.width.max(1),
            height: (total / s).ceil() as i32,
            hits,
            cut,
        }
    }

    /// A block's text, shaped and wrapped at `width`.
    fn paragraph(
        &mut self,
        b: &Block,
        st: &DocStyle,
        size: f32,
        line_h: f32,
        bold: bool,
        width: f32,
    ) -> Buffer {
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(size, line_h));
        buffer.set_wrap(&mut self.fonts, Wrap::WordOrGlyph);
        buffer.set_size(&mut self.fonts, Some(width), None);
        let base = Attrs::new().family(family(&st.font)).color(color(st.color));
        let code_block = b.kind == Kind::Code;
        let mut spans: Vec<(&str, Attrs)> = (b.runs.iter())
            .filter(|r| !r.text.is_empty())
            .map(|r| {
                let mut a = base.clone();
                if bold || r.bold {
                    a = a.weight(Weight::BOLD);
                }
                if r.italic {
                    a = a.style(Slant::Italic);
                }
                if r.code || code_block {
                    a = a.family(Family::Monospace);
                }
                let mut rgba = if r.link.is_some() { st.link } else { st.color };
                if r.dim {
                    rgba = fade(rgba, DIM);
                }
                let link = r.link.map_or(0, |l| (l as usize + 1) & LINK_MASK);
                let strike = if r.strike { STRIKE } else { 0 };
                (r.text.as_str(), a.color(color(rgba)).metadata(link | strike))
            })
            .collect();
        if spans.is_empty() {
            spans.push((" ", base.clone()));
        }
        buffer.set_rich_text(&mut self.fonts, spans, &base, Shaping::Advanced, None);
        buffer
    }

    /// Draw the glyphs of `buffer` at (`x`, `y`), leaving out lines that
    /// end below `max_h` (setting `cut`).
    #[allow(clippy::too_many_arguments)]
    fn glyphs_at(
        &mut self,
        buffer: &Buffer,
        img: &mut Image,
        x: f32,
        y: f32,
        base: Color,
        max_h: f32,
        cut: &mut bool,
    ) {
        let (w, h) = (img.width as i32, img.height as i32);
        for run in buffer.layout_runs() {
            if y + run.line_top + run.line_height > max_h + 0.5 {
                *cut = true;
                break;
            }
            for g in run.glyphs {
                let pg = g.physical((x, y + run.line_y), 1.0);
                let c = g.color_opt.unwrap_or(base);
                let a = c.a() as f64 / 255.0;
                let data = &mut img.data;
                self.glyphs
                    .with_pixels(&mut self.fonts, pg.cache_key, c, |gx, gy, px| {
                        let alpha = px.a() as f64 / 255.0 * a;
                        let (px_x, px_y) = (pg.x + gx, pg.y + gy);
                        if alpha <= 0.0 || px_x < 0 || px_y < 0 || px_x >= w || px_y >= h {
                            return;
                        }
                        let i = ((px_y * w + px_x) * 4) as usize;
                        let src = [px.b(), px.g(), px.r()].map(|v| v as f64 * alpha);
                        blend(&mut data[i..i + 4], src, alpha);
                    });
            }
        }
    }

    /// Code backgrounds, quote bars, rules, bullets and checkboxes, and
    /// the checkboxes' hits.
    #[allow(clippy::too_many_arguments)]
    fn shapes(
        &self,
        doc: &Doc,
        laid: &[Laid],
        pixmap: &mut Pixmap,
        hits: &mut Vec<Hit>,
        indent: f32,
        size: f32,
        s: f32,
    ) {
        let st = &doc.style;
        let width = pixmap.width() as f32;
        let max_h = pixmap.height() as f32;
        let mut task = 0;
        for l in laid {
            let b = &doc.blocks[l.block];
            let text_top = l.text.as_ref().map_or(l.top, |t| t.2);
            let center = text_top + l.first / 2.0;
            if b.quote && l.left >= indent {
                let x = l.left - indent * 0.6;
                let bar = rounded(x, l.top, (2.0 * s).round(), l.height, s);
                fill(pixmap, bar.as_ref(), fade(st.color, 0.3));
            }
            match b.kind {
                Kind::Code => {
                    let r = rounded(l.left, l.top, width - l.left, l.height, 3.0 * s);
                    fill(pixmap, r.as_ref(), fade(st.color, 0.08));
                }
                Kind::Rule => {
                    let t = s.round().max(1.0);
                    let r = rounded(l.left, (center - t / 2.0).round(), width - l.left, t, 0.0);
                    fill(pixmap, r.as_ref(), fade(st.color, 0.3));
                }
                _ => {}
            }
            let shown = text_top + l.first <= max_h + 0.5;
            match b.marker {
                Marker::Bullet if shown => {
                    let r = (size * 0.14).max(1.5 * s);
                    let dot = PathBuilder::from_circle(l.left + indent * 0.45, center, r);
                    fill(pixmap, dot.as_ref(), st.color);
                }
                Marker::Task(checked) => {
                    let n = task;
                    task += 1;
                    if !shown {
                        continue;
                    }
                    let bs = (size * 0.85).round();
                    let bx = (l.left + (indent - bs) / 2.0 - size * 0.1).round();
                    let by = (center - bs / 2.0).round();
                    let rr = rounded(bx, by, bs, bs, bs * 0.22);
                    if checked {
                        fill(pixmap, rr.as_ref(), fade(st.color, 0.75));
                        let mut pb = PathBuilder::new();
                        pb.move_to(bx + 0.24 * bs, by + 0.52 * bs);
                        pb.line_to(bx + 0.43 * bs, by + 0.71 * bs);
                        pb.line_to(bx + 0.77 * bs, by + 0.31 * bs);
                        if let Some(check) = pb.finish() {
                            let paint = Paint {
                                anti_alias: true,
                                blend_mode: BlendMode::Clear,
                                ..Paint::default()
                            };
                            let stroke = Stroke {
                                width: (bs * 0.15).max(1.5),
                                line_cap: LineCap::Round,
                                line_join: LineJoin::Round,
                                ..Stroke::default()
                            };
                            pixmap.stroke_path(&check, &paint, &stroke, Transform::identity(), None);
                        }
                    } else if let Some(rr) = &rr {
                        let stroke = Stroke {
                            width: (size * 0.08).max(1.2 * s),
                            ..Stroke::default()
                        };
                        let paint = solid(fade(st.color, 0.7));
                        pixmap.stroke_path(rr, &paint, &stroke, Transform::identity(), None);
                    }
                    // Generous: the whole marker column of the first line.
                    hits.push(Hit {
                        x: (l.left / s) as f64,
                        y: (l.top / s) as f64,
                        w: (indent / s) as f64,
                        h: ((text_top + l.first - l.top) / s) as f64,
                        target: Target::Task(n),
                    });
                }
                _ => {}
            }
        }
    }
}

/// Underlines for links, lines through struck text, and the links' hits.
#[allow(clippy::too_many_arguments)]
fn decorate(
    buffer: &Buffer,
    img: &mut Image,
    x: f32,
    y: f32,
    base: Color,
    size: f32,
    s: f32,
    max_h: f32,
    hits: &mut Vec<Hit>,
) {
    let thick = s.round().max(1.0);
    for run in buffer.layout_runs() {
        if y + run.line_top + run.line_height > max_h + 0.5 {
            break;
        }
        // Stretches of glyphs with the same metadata: (meta, x0, x1, color).
        let mut segs: Vec<(usize, f32, f32, Color)> = Vec::new();
        for g in run.glyphs.iter().filter(|g| g.metadata != 0) {
            let c = g.color_opt.unwrap_or(base);
            match segs.last_mut() {
                Some((m, _, x1, _)) if *m == g.metadata && (g.x - *x1).abs() < 1.0 => {
                    *x1 = g.x + g.w;
                }
                _ => segs.push((g.metadata, g.x, g.x + g.w, c)),
            }
        }
        let baseline = y + run.line_y;
        for (meta, x0, x1, c) in segs {
            let (x0, x1) = (x + x0, x + x1);
            if meta & LINK_MASK != 0 {
                let uy = (baseline + size * 0.12).round();
                fill_rect(img, x0, uy, x1, uy + thick, c);
                hits.push(Hit {
                    x: (x0 / s) as f64,
                    y: ((y + run.line_top) / s) as f64,
                    w: ((x1 - x0) / s) as f64,
                    h: (run.line_height / s) as f64,
                    target: Target::Link(((meta & LINK_MASK) - 1) as u32),
                });
            }
            if meta & STRIKE != 0 {
                let sy = (baseline - size * 0.28).round();
                fill_rect(img, x0, sy, x1, sy + thick, c);
            }
        }
    }
}

/// `rgba` with its alpha times `f`.
fn fade(rgba: u32, f: f64) -> u32 {
    let a = ((rgba & 0xff) as f64 * f).round().clamp(0.0, 255.0) as u32;
    (rgba & !0xff) | a
}

fn color(rgba: u32) -> Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    Color::rgba(r, g, b, a)
}

fn solid(rgba: u32) -> Paint<'static> {
    let [r, g, b, a] = rgba.to_be_bytes();
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, a);
    p.anti_alias = true;
    p
}

fn fill(pixmap: &mut Pixmap, path: Option<&Path>, rgba: u32) {
    if let Some(path) = path {
        pixmap.fill_path(
            path,
            &solid(rgba),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

/// A rounded rect's outline.
fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Blend a rect of `c` over `img` (physical pixels, rounded out).
fn fill_rect(img: &mut Image, x0: f32, y0: f32, x1: f32, y1: f32, c: Color) {
    let (w, h) = (img.width as i32, img.height as i32);
    let alpha = c.a() as f64 / 255.0;
    let src = [c.b(), c.g(), c.r()].map(|v| v as f64 * alpha);
    for py in (y0.floor() as i32).max(0)..(y1.ceil() as i32).min(h) {
        for px in (x0.floor() as i32).max(0)..(x1.ceil() as i32).min(w) {
            let i = ((py * w + px) * 4) as usize;
            blend(&mut img.data[i..i + 4], src, alpha);
        }
    }
}

/// A pixmap (premultiplied RGBA) as an image (premultiplied BGRA).
fn bgra(pixmap: &Pixmap) -> Image {
    let mut data = Vec::with_capacity(pixmap.data().len());
    for px in pixmap.data().as_chunks::<4>().0 {
        data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Image {
        width: pixmap.width(),
        height: pixmap.height(),
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(blocks: Vec<Block>, max_height: i32) -> Doc {
        Doc {
            blocks,
            style: DocStyle {
                font: "sans-serif".into(),
                size: 13.0,
                color: 0x202020ff,
                link: 0x1a5fb4ff,
            },
            width: 200,
            max_height,
            scale: 2,
        }
    }

    fn task(checked: bool, text: &str) -> Block {
        Block {
            marker: Marker::Task(checked),
            gap: false,
            ..Block::new(Kind::Paragraph, vec![Run::plain(text)])
        }
    }

    #[test]
    fn hits() {
        let hits = [Hit {
            x: 10.0,
            y: 10.0,
            w: 5.0,
            h: 5.0,
            target: Target::Link(3),
        }];
        assert_eq!(hit(&hits, 12.0, 14.9), Some(Target::Link(3)));
        assert_eq!(hit(&hits, 15.0, 12.0), None);
        assert_eq!(hits[0].offset(1.0, 2.0).x, 11.0);
        assert_eq!(fade(0x112233ff, 0.5), 0x11223380);
    }

    #[test]
    fn lays_out_and_finds_the_checkboxes() {
        let mut text = Text::new();
        let blocks = vec![
            Block::new(Kind::Heading(1), vec![Run::plain("Title")]),
            task(false, "one"),
            task(true, "two"),
            Block::new(Kind::Rule, vec![]),
        ];
        let r = text.doc(&doc(blocks.clone(), 1000));
        assert_eq!(r.width, 200);
        assert_eq!(r.image.width, 400);
        assert_eq!(r.image.height as i32, r.height * 2);
        assert!(!r.cut);
        let tasks: Vec<_> = r.hits.iter().filter(|h| matches!(h.target, Target::Task(_))).collect();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].target, Target::Task(0));
        assert!(tasks[1].y > tasks[0].y);
        // Cut short: the second task and the rule are left out.
        let short = text.doc(&doc(blocks, (tasks[1].y as i32) + 2));
        assert!(short.cut);
        assert_eq!(
            short.hits.iter().filter(|h| matches!(h.target, Target::Task(_))).count(),
            1
        );
    }
}

