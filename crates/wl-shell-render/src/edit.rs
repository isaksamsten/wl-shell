//! Editable text: lines of styled source, laid out and drawn on the
//! program's own thread, so a keystroke shows in the next frame (wl-note's
//! open note).
//!
//! The program says, for each source line, which byte ranges show and in
//! what style ([`Span`]), and what goes in the marker column ([`Line`]'s
//! `marker` or `lead`). What no span covers is hidden: that is how
//! markdown's syntax disappears off the caret's line. Lines are shaped
//! with cosmic-text into an [`EditLayout`] that is kept between frames:
//! only lines that changed are shaped again. The layout answers where a
//! caret at a byte of a line is, which byte is at a point, and where the
//! checkboxes and links are; [`Text::edit_draw`] draws the part in view,
//! with the selection and the caret.
//!
//! Sizes in the API are logical pixels, from the document's top left;
//! everything inside is laid out at buffer scale.

use std::collections::HashMap;
use std::ops::Range;

use cosmic_text::{Attrs, Buffer, Color, Family, Metrics, Shaping, Style as Slant, Weight, Wrap};
use resvg::tiny_skia::{PathBuilder, Pixmap};

use crate::doc::{
    DIM, Hit, LINK_MASK, STRIKE, Target, bgra, checkbox, color, fade, fill, fill_rect, rounded,
};
use crate::image::Image;
use crate::text::{Text, blend, family, line_height};

/// How much syntax (markers shown on the caret's line) keeps of its alpha.
const FAINT: f64 = 0.4;

/// How editable text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct EditStyle {
    pub font: String,
    /// Headings', if not `font`.
    pub heading_font: Option<String>,
    /// Logical pixels.
    pub size: f64,
    /// 0xRRGGBBAA, not premultiplied.
    pub color: u32,
    pub link: u32,
    /// Behind selected text.
    pub selection: u32,
}

/// What is drawn in a line's marker column when it has no `lead`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LineMarker {
    #[default]
    None,
    Bullet,
    /// A checkbox, checked or not: `Target::Task(line)`.
    Task(bool),
}

/// A byte range of a line's text, and its style.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub range: Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strike: bool,
    /// Fainter, as a checked task's text.
    pub dim: bool,
    /// Fainter still: syntax shown on the caret's line.
    pub faint: bool,
    /// A link, by its index in the program's list.
    pub link: Option<u32>,
}

impl Span {
    pub fn plain(range: Range<usize>) -> Self {
        Self {
            range,
            ..Default::default()
        }
    }
}

/// A source line to lay out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Line {
    /// Without its line break.
    pub text: String,
    /// Indentation levels.
    pub depth: u8,
    pub marker: LineMarker,
    /// Spans drawn in the marker column (a list marker's source, a
    /// number); the text starts after them.
    pub lead: Vec<Span>,
    /// Spans drawn as the line's text, wrapped, in order.
    pub spans: Vec<Span>,
    /// A heading's level; 0: none.
    pub heading: u8,
    /// In a code block: monospace on a faint background.
    pub code: bool,
    /// Its syntax shows (the caret's line): its links and checkboxes
    /// aren't targets, so a click there places the caret.
    pub active: bool,
}

/// A caret: logical pixels from the document's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub x: f64,
    pub y: f64,
    pub h: f64,
}

/// A position in the text: a line, and a byte of it.
pub type Pos = (usize, usize);

/// Shaped spans: the buffer, and where its text is in the line's.
struct Shaped {
    buffer: Buffer,
    /// (index in the shaped text, index in the line, length), in order.
    map: Vec<(usize, usize, usize)>,
    /// Physical pixels.
    width: f32,
}

impl Shaped {
    /// The line's byte for the shaped text's byte `d`.
    fn to_src(&self, d: usize) -> usize {
        for &(disp, src, len) in &self.map {
            if d >= disp && d < disp + len {
                return src + (d - disp);
            }
        }
        self.map.last().map_or(0, |&(_, src, len)| src + len)
    }

    /// The shaped text's byte for the line's byte `s`: the next shown one
    /// if it is hidden.
    fn to_disp(&self, s: usize) -> usize {
        for &(disp, src, len) in &self.map {
            if s <= src {
                return disp;
            }
            if s <= src + len {
                return disp + (s - src);
            }
        }
        self.map.last().map_or(0, |&(disp, _, len)| disp + len)
    }

    /// The first line byte it shows, and the end of the last.
    fn src_bounds(&self) -> Option<(usize, usize)> {
        let first = self.map.first()?;
        let last = self.map.last()?;
        Some((first.1, last.1 + last.2))
    }
}

/// A line laid out: physical pixels, from the line's top left.
struct Laid {
    lead: Option<Shaped>,
    body: Option<Shaped>,
    x_lead: f32,
    x_body: f32,
    /// The marker column's width.
    indent: f32,
    height: f32,
    /// The first row's height.
    first: f32,
}

/// Lines laid out, kept between frames.
#[derive(Default)]
pub struct EditLayout {
    key: Option<(EditStyle, i32, i32)>,
    lines: Vec<(Line, Laid)>,
    /// Physical pixels.
    tops: Vec<f32>,
    height: f32,
    scale: f32,
    /// Logical.
    width: i32,
    hits: Vec<Hit>,
}

/// Where a caret at display byte `d` is on `buffer`: (x, row top, row
/// height), physical pixels.
fn caret_in(buffer: &Buffer, d: usize) -> Option<(f32, f32, f32)> {
    let runs: Vec<_> = buffer.layout_runs().collect();
    let last = runs.len().checked_sub(1)?;
    for (i, run) in runs.iter().enumerate() {
        let end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
        let start = run.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
        if d < end || i == last || (d == end && runs[i + 1].glyphs.is_empty()) {
            let x = if d <= start && !run.glyphs.is_empty() {
                run.glyphs
                    .iter()
                    .filter(|g| g.start == start)
                    .map(|g| g.x)
                    .fold(f32::MAX, f32::min)
            } else {
                edge_x(run.glyphs, d)
            };
            return Some((x, run.line_top, run.line_height));
        }
    }
    None
}

/// The x of a caret at `d` in a row's glyphs: before the first glyph from
/// it on, else after the last.
fn edge_x(glyphs: &[cosmic_text::LayoutGlyph], d: usize) -> f32 {
    let mut edges: Vec<(usize, f32)> = Vec::with_capacity(glyphs.len() + 1);
    let mut end = (0, 0.0f32);
    for g in glyphs {
        edges.push((g.start, g.x));
        if g.end >= end.0 {
            end = (g.end, g.x + g.w);
        }
    }
    edges.push(end);
    edges.sort_by_key(|e| e.0);
    (edges.iter())
        .find(|e| e.0 >= d)
        .or(edges.last())
        .map_or(0.0, |e| e.1)
}

/// The display byte nearest `x` in a row's glyphs.
fn nearest(glyphs: &[cosmic_text::LayoutGlyph], x: f32) -> usize {
    let mut best = (f32::MAX, 0);
    for g in glyphs {
        for (i, gx) in [(g.start, g.x), (g.end, g.x + g.w)] {
            let d = (gx - x).abs();
            if d < best.0 {
                best = (d, i);
            }
        }
    }
    best.1
}

impl EditLayout {
    /// The document's height, logical pixels.
    pub fn height(&self) -> f64 {
        (self.height / self.scale.max(1.0)) as f64
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The checkboxes (`Target::Task(line)`) and links of lines that
    /// aren't active.
    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    /// Where a caret at `pos` is.
    pub fn caret(&self, (line, at): Pos) -> Option<Caret> {
        let (_, laid) = self.lines.get(line)?;
        let s = self.scale;
        let top = self.tops[line];
        let out = |x: f32, y: f32, h: f32| Caret {
            x: (x / s) as f64,
            y: ((top + y) / s) as f64,
            h: (h / s) as f64,
        };
        if let Some(lead) = &laid.lead
            && let Some((_, end)) = lead.src_bounds()
            && at < end
        {
            let (x, y, h) =
                caret_in(&lead.buffer, lead.to_disp(at)).unwrap_or((0.0, 0.0, laid.first));
            return Some(out(laid.x_lead + x, y, h));
        }
        match &laid.body {
            Some(body) => {
                let (x, y, h) =
                    caret_in(&body.buffer, body.to_disp(at)).unwrap_or((0.0, 0.0, laid.first));
                Some(out(laid.x_body + x, y, h))
            }
            None => Some(out(laid.x_body, 0.0, laid.first)),
        }
    }

    /// The position nearest (`x`, `y`).
    pub fn index_at(&self, x: f64, y: f64) -> Pos {
        let s = self.scale;
        let (px, py) = (x as f32 * s, y as f32 * s);
        if self.lines.is_empty() {
            return (0, 0);
        }
        let line = match self.tops.iter().rposition(|t| *t <= py) {
            Some(i) => i,
            None => return (0, 0),
        };
        let (l, laid) = &self.lines[line];
        let ly = py - self.tops[line];
        if let Some(lead) = &laid.lead
            && px < laid.x_body
            && ly < laid.first
        {
            let run = lead.buffer.layout_runs().next();
            let d = run.map_or(0, |r| nearest(r.glyphs, px - laid.x_lead));
            return (line, lead.to_src(d));
        }
        let Some(body) = &laid.body else {
            return (line, l.text.len());
        };
        let runs: Vec<_> = body.buffer.layout_runs().collect();
        let run = (runs.iter())
            .find(|r| ly < r.line_top + r.line_height)
            .or(runs.last());
        let d = run.map_or(0, |r| nearest(r.glyphs, px - laid.x_body));
        (line, body.to_src(d))
    }

    /// The position a row up (`rows` < 0) or down from `pos`, nearest
    /// `goal_x`; or `dy` logical pixels away if given (a page).
    pub fn vertical(&self, pos: Pos, goal_x: f64, rows: i32, dy: Option<f64>) -> Pos {
        let Some(c) = self.caret(pos) else {
            return pos;
        };
        let y = match (dy, rows < 0) {
            (Some(d), _) => c.y + c.h / 2.0 + d,
            (None, true) => c.y - 1.0,
            (None, false) => c.y + c.h + 1.0,
        };
        if y < 0.0 {
            return (0, 0);
        }
        if y >= self.height() {
            let last = self.lines.len().saturating_sub(1);
            return (last, self.lines.get(last).map_or(0, |l| l.0.text.len()));
        }
        self.index_at(goal_x, y)
    }

    /// The width text wraps at, logical pixels.
    pub fn width(&self) -> i32 {
        self.width
    }
}

impl Text {
    /// Shape `spans` of `text` on one buffer, wrapped at `wrap` (physical
    /// pixels) if given.
    #[allow(clippy::too_many_arguments)]
    fn shape_spans_of(
        &mut self,
        text: &str,
        spans: &[Span],
        st: &EditStyle,
        heading: u8,
        code: bool,
        metrics: Metrics,
        wrap: Option<f32>,
    ) -> Option<Shaped> {
        let mut disp = String::new();
        let mut map = Vec::new();
        let mut used = Vec::new();
        for sp in spans {
            let Some(t) = text.get(sp.range.clone()).filter(|t| !t.is_empty()) else {
                continue;
            };
            map.push((disp.len(), sp.range.start, t.len()));
            disp.push_str(t);
            used.push(sp);
        }
        if used.is_empty() {
            return None;
        }
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        match wrap {
            Some(w) => {
                buffer.set_wrap(&mut self.fonts, Wrap::WordOrGlyph);
                buffer.set_size(&mut self.fonts, Some(w), None);
            }
            None => {
                buffer.set_wrap(&mut self.fonts, Wrap::None);
                buffer.set_size(&mut self.fonts, None, None);
            }
        }
        let mut base = Attrs::new().family(family(&st.font)).color(color(st.color));
        if let (true, Some(font)) = (heading > 0, &st.heading_font) {
            base = base.family(family(font));
        }
        let rich = (used.iter().zip(&map)).map(|(sp, &(d, _, len))| {
            let mut a = base.clone();
            if heading > 0 || sp.bold {
                a = a.weight(Weight::BOLD);
            }
            if sp.italic {
                a = a.style(Slant::Italic);
            }
            if sp.code || code {
                a = a.family(Family::Monospace);
            }
            let mut rgba = if sp.link.is_some() { st.link } else { st.color };
            if sp.dim {
                rgba = fade(rgba, DIM);
            }
            if sp.faint {
                rgba = fade(rgba, FAINT);
            }
            let link = sp.link.map_or(0, |l| (l as usize + 1) & LINK_MASK);
            let strike = if sp.strike { STRIKE } else { 0 };
            (
                &disp[d..d + len],
                a.color(color(rgba)).metadata(link | strike),
            )
        });
        buffer.set_rich_text(&mut self.fonts, rich, &base, Shaping::Advanced, None);
        let width = buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max);
        Some(Shaped { buffer, map, width })
    }

    fn lay_line(&mut self, line: &Line, st: &EditStyle, width: f32, s: f32) -> Laid {
        let size = st.size as f32 * s;
        let lh = line_height(st.size) as f32 * s;
        let indent = (size * 1.5).round();
        let (fsize, flh) = match line.heading {
            1 => (size * 1.3, (lh * 1.3).round()),
            2 => (size * 1.15, (lh * 1.15).round()),
            _ => (size, lh),
        };
        let metrics = Metrics::new(fsize, flh);
        let left = line.depth as f32 * indent;
        let lead = self.shape_spans_of(
            &line.text,
            &line.lead,
            st,
            line.heading,
            line.code,
            metrics,
            None,
        );
        let marked = line.marker != LineMarker::None || lead.is_some();
        let x_body = match (&lead, marked) {
            (Some(l), _) => left + indent.max(l.width + (size * 0.3).round()),
            (None, true) => left + indent,
            (None, false) => left,
        };
        let avail = (width - x_body).max(size);
        let body = self.shape_spans_of(
            &line.text,
            &line.spans,
            st,
            line.heading,
            line.code,
            metrics,
            Some(avail),
        );
        let (height, first) = match &body {
            Some(b) => {
                let runs: Vec<_> = b.buffer.layout_runs().collect();
                let h = (runs.iter())
                    .map(|r| r.line_top + r.line_height)
                    .fold(0.0, f32::max);
                let first = runs.first().map_or(flh, |r| r.line_height);
                (h.max(flh), first)
            }
            None => (flh, flh),
        };
        Laid {
            lead,
            body,
            x_lead: left,
            x_body,
            indent,
            height,
            first,
        }
    }

    /// Lay `lines` out at `width` (logical pixels) and `scale`, reusing
    /// what `layout` has of lines that didn't change.
    pub fn edit_layout(
        &mut self,
        layout: &mut EditLayout,
        lines: Vec<Line>,
        st: &EditStyle,
        width: i32,
        scale: i32,
    ) {
        let s = scale.max(1) as f32;
        let key = (st.clone(), width, scale);
        let mut pool: HashMap<Line, Vec<Laid>> = HashMap::new();
        if layout.key.as_ref() == Some(&key) {
            for (l, laid) in layout.lines.drain(..) {
                pool.entry(l).or_default().push(laid);
            }
        }
        layout.lines.clear();
        layout.key = Some(key);
        layout.scale = s;
        layout.width = width;
        let w = (width.max(1) as f32 * s).round();
        for line in lines {
            let laid = match pool.get_mut(&line).and_then(Vec::pop) {
                Some(laid) => laid,
                None => self.lay_line(&line, st, w, s),
            };
            layout.lines.push((line, laid));
        }
        layout.tops.clear();
        let mut y = 0.0;
        for (_, laid) in &layout.lines {
            layout.tops.push(y);
            y += laid.height;
        }
        layout.height = y;
        layout.hits = hits(layout);
    }

    /// Draw the part of `layout` from `scroll` down, `height` logical
    /// pixels tall, with the `selection` (from, to) and the `caret`.
    pub fn edit_draw(
        &mut self,
        layout: &EditLayout,
        st: &EditStyle,
        scroll: f64,
        height: i32,
        selection: Option<(Pos, Pos)>,
        caret: Option<Pos>,
    ) -> Image {
        let s = layout.scale.max(1.0);
        let size = st.size as f32 * s;
        let (bw, bh) = (
            (layout.width.max(1) as f32 * s).round() as u32,
            (height.max(1) as f32 * s).round() as u32,
        );
        let top = (scroll as f32 * s).round();
        let visible = |i: usize| {
            let y = layout.tops[i] - top;
            y + layout.lines[i].1.height > 0.0 && y < bh as f32
        };
        let mut image = match Pixmap::new(bw, bh) {
            Some(mut pixmap) => {
                for i in (0..layout.lines.len()).filter(|i| visible(*i)) {
                    let (line, laid) = &layout.lines[i];
                    let y = layout.tops[i] - top;
                    if line.code {
                        let r = rounded(0.0, y, bw as f32, laid.height, 0.0);
                        fill(&mut pixmap, r.as_ref(), fade(st.color, 0.08));
                    }
                    if let Some(((l0, a0), (l1, a1))) = selection
                        && (l0..=l1).contains(&i)
                    {
                        let from = if i == l0 { a0 } else { 0 };
                        let to = if i == l1 { a1 } else { line.text.len() };
                        let past = i < l1;
                        for (x0, y0, x1, y1) in select_rects(laid, from, to, past, size) {
                            let r = rounded(x0, y + y0, x1 - x0, y1 - y0, 0.0);
                            fill(&mut pixmap, r.as_ref(), st.selection);
                        }
                    }
                    if laid.lead.is_none() {
                        let center = y + laid.first / 2.0;
                        match line.marker {
                            LineMarker::Bullet => {
                                let r = (size * 0.14).max(1.5 * s);
                                let dot = PathBuilder::from_circle(
                                    laid.x_lead + laid.indent * 0.45,
                                    center,
                                    r,
                                );
                                fill(&mut pixmap, dot.as_ref(), st.color);
                            }
                            LineMarker::Task(checked) => {
                                let bs = (size * 0.85).round();
                                let bx =
                                    (laid.x_lead + (laid.indent - bs) / 2.0 - size * 0.1).round();
                                let by = (center - bs / 2.0).round();
                                checkbox(&mut pixmap, bx, by, bs, checked, st.color, size, s);
                            }
                            LineMarker::None => {}
                        }
                    }
                }
                bgra(&pixmap)
            }
            None => Image {
                width: bw,
                height: bh,
                data: vec![0; (bw * bh * 4) as usize],
            },
        };
        let base = color(st.color);
        for i in (0..layout.lines.len()).filter(|i| visible(*i)) {
            let (_, laid) = &layout.lines[i];
            let y = layout.tops[i] - top;
            for (shaped, x) in [(&laid.lead, laid.x_lead), (&laid.body, laid.x_body)] {
                if let Some(shaped) = shaped {
                    self.draw_rows(&shaped.buffer, &mut image, x, y, base);
                    underline(&shaped.buffer, &mut image, x, y, base, size, s);
                }
            }
        }
        if let Some(c) = caret.and_then(|p| layout.caret(p)) {
            let x = (c.x as f32 * s).round();
            let y0 = (c.y as f32 * s).round() - top;
            let h = (c.h as f32 * s).round();
            let pad = (h * 0.1).round();
            fill_rect(
                &mut image,
                x,
                y0 + pad,
                x + s.round().max(1.0),
                y0 + h - pad,
                base,
            );
        }
        image
    }

    /// Draw `buffer`'s rows that are in `img` at (`x`, `y`).
    fn draw_rows(&mut self, buffer: &Buffer, img: &mut Image, x: f32, y: f32, base: Color) {
        let (w, h) = (img.width as i32, img.height as i32);
        for run in buffer.layout_runs() {
            if y + run.line_top + run.line_height < 0.0 || y + run.line_top > h as f32 {
                continue;
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
}

/// The rects of a line's selection from byte `from` to `to` (and past
/// its end, if `past`): physical pixels from the line's top left.
fn select_rects(
    laid: &Laid,
    from: usize,
    to: usize,
    past: bool,
    size: f32,
) -> Vec<(f32, f32, f32, f32)> {
    let mut out = Vec::new();
    let mut end_x = laid.x_body;
    for (shaped, x) in [(&laid.lead, laid.x_lead), (&laid.body, laid.x_body)] {
        let Some(shaped) = shaped else { continue };
        let (d0, d1) = (shaped.to_disp(from), shaped.to_disp(to));
        for run in shaped.buffer.layout_runs() {
            let start = run.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
            let end = run.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
            end_x = x + run.line_w;
            let (a, b) = (d0.max(start), d1.min(end));
            if a < b {
                let xa = x + edge_x(run.glyphs, a);
                let xb = x + edge_x(run.glyphs, b);
                out.push((
                    xa.min(xb),
                    run.line_top,
                    xa.max(xb),
                    run.line_top + run.line_height,
                ));
            }
        }
    }
    if past {
        let y0 = match &laid.body {
            Some(b) => b.buffer.layout_runs().last().map_or(0.0, |r| r.line_top),
            None => 0.0,
        };
        let y1 = laid.height;
        out.push((end_x, y0, end_x + (size * 0.35).round(), y1));
    }
    out
}

/// Underlines for links and lines through struck text.
fn underline(buffer: &Buffer, img: &mut Image, x: f32, y: f32, base: Color, size: f32, s: f32) {
    let thick = s.round().max(1.0);
    for run in buffer.layout_runs() {
        if y + run.line_top + run.line_height < 0.0 || y + run.line_top > img.height as f32 {
            continue;
        }
        let baseline = y + run.line_y;
        for (meta, x0, x1, c) in stretches(run.glyphs, base) {
            let (x0, x1) = (x + x0, x + x1);
            if meta & LINK_MASK != 0 {
                let uy = (baseline + size * 0.12).round();
                fill_rect(img, x0, uy, x1, uy + thick, c);
            }
            if meta & STRIKE != 0 {
                let sy = (baseline - size * 0.28).round();
                fill_rect(img, x0, sy, x1, sy + thick, c);
            }
        }
    }
}

/// Stretches of glyphs with the same metadata: (meta, x0, x1, color).
fn stretches(glyphs: &[cosmic_text::LayoutGlyph], base: Color) -> Vec<(usize, f32, f32, Color)> {
    let mut segs: Vec<(usize, f32, f32, Color)> = Vec::new();
    for g in glyphs.iter().filter(|g| g.metadata != 0) {
        let c = g.color_opt.unwrap_or(base);
        match segs.last_mut() {
            Some((m, _, x1, _)) if *m == g.metadata && (g.x - *x1).abs() < 1.0 => {
                *x1 = g.x + g.w;
            }
            _ => segs.push((g.metadata, g.x, g.x + g.w, c)),
        }
    }
    segs
}

/// The checkboxes and links of the lines that aren't active, logical
/// pixels.
fn hits(layout: &EditLayout) -> Vec<Hit> {
    let s = layout.scale.max(1.0);
    let mut out = Vec::new();
    for (i, (line, laid)) in layout.lines.iter().enumerate() {
        if line.active {
            continue;
        }
        let top = layout.tops[i];
        if let (LineMarker::Task(_), None) = (line.marker, &laid.lead) {
            out.push(Hit {
                x: (laid.x_lead / s) as f64,
                y: (top / s) as f64,
                w: (laid.indent / s) as f64,
                h: (laid.first / s) as f64,
                target: Target::Task(i as u32),
            });
        }
        let Some(body) = &laid.body else { continue };
        for run in body.buffer.layout_runs() {
            for (meta, x0, x1, _) in stretches(run.glyphs, Color::rgb(0, 0, 0)) {
                if meta & LINK_MASK == 0 {
                    continue;
                }
                out.push(Hit {
                    x: ((laid.x_body + x0) / s) as f64,
                    y: ((top + run.line_top) / s) as f64,
                    w: ((x1 - x0) / s) as f64,
                    h: (run.line_height / s) as f64,
                    target: Target::Link(((meta & LINK_MASK) - 1) as u32),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> EditStyle {
        EditStyle {
            font: "sans-serif".into(),
            heading_font: None,
            size: 13.0,
            color: 0x202020ff,
            link: 0x1a5fb4ff,
            selection: 0x1a5fb440,
        }
    }

    fn plain(text: &str) -> Line {
        Line {
            text: text.into(),
            spans: vec![Span::plain(0..text.len())],
            ..Default::default()
        }
    }

    #[test]
    fn carets_and_hits() {
        let mut text = Text::new();
        let mut layout = EditLayout::default();
        // "**bold** and [link](x)" off the caret's line: syntax hidden.
        let src = "**bold** and [link](x)";
        let hidden = Line {
            text: src.into(),
            spans: vec![
                Span {
                    bold: true,
                    ..Span::plain(2..6)
                },
                Span::plain(8..13),
                Span {
                    link: Some(0),
                    ..Span::plain(14..18)
                },
            ],
            ..Default::default()
        };
        let task = Line {
            text: "- [ ] milk".into(),
            marker: LineMarker::Task(false),
            spans: vec![Span::plain(6..10)],
            ..Default::default()
        };
        let lines = vec![plain("first line"), hidden, task, plain("")];
        text.edit_layout(&mut layout, lines.clone(), &style(), 200, 2);
        assert_eq!(layout.len(), 4);
        let h = layout.height();
        assert!(h > 50.0 && h < 120.0, "{h}");
        // Carets go down the lines, and along them.
        let a = layout.caret((0, 0)).unwrap();
        let b = layout.caret((0, 5)).unwrap();
        let c = layout.caret((1, 2)).unwrap();
        assert_eq!(a.x, 0.0);
        assert!(b.x > a.x && b.y == a.y);
        assert!(c.y > a.y);
        // A caret in hidden syntax sits before the next shown text.
        assert_eq!(layout.caret((1, 0)).unwrap().x, c.x);
        // Clicks find the bytes back.
        assert_eq!(layout.index_at(b.x + 0.1, b.y + 1.0), (0, 5));
        assert_eq!(layout.index_at(-5.0, -5.0), (0, 0));
        let end = layout.index_at(1000.0, h + 10.0);
        assert_eq!(end, (3, 0));
        // The checkbox and the link are targets.
        let targets: Vec<_> = layout.hits().iter().map(|h| h.target).collect();
        assert_eq!(targets, [Target::Link(0), Target::Task(2)]);
        // Rows up and down.
        assert_eq!(layout.vertical((1, 2), c.x, -1, None).0, 0);
        assert_eq!(layout.vertical((0, 0), 0.0, -1, None), (0, 0));
        assert_eq!(layout.vertical((2, 6), 0.0, 1, None).0, 3);
        // Laid out again, unchanged lines are kept; the image is the view.
        text.edit_layout(&mut layout, lines, &style(), 200, 2);
        let img = text.edit_draw(
            &layout,
            &style(),
            0.0,
            40,
            Some(((0, 0), (1, 4))),
            Some((0, 3)),
        );
        assert_eq!((img.width, img.height), (400, 80));
        assert!(img.data.iter().any(|b| *b != 0));
    }
}
