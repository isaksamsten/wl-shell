//! Painting a laid-out tree with tiny-skia, at the output's scale, in the
//! theme's colors and font. Text is shaped and drawn with cosmic-text,
//! icons and images come from the caller (`Resources`: the worker's icon
//! and image caches).
//!
//! The look of each kind, beyond its `background`, `radius`, `border`
//! and `opacity`:
//!
//! - Text in its style (`title` larger and bold, `muted` dimmer,
//!   `caption` smaller), in the color it is given, else the one it is
//!   on (the theme's text; the accent's text on a filled button).
//! - Icons scaled into their box; `-symbolic` ones (and any given a
//!   `color`) take the text's color.
//! - Boxes in their variant's colors; buttons with hover, pressed,
//!   selected and disabled looks; sliders, toggles and progress bars as
//!   tracks, fills and knobs in the accent.
//! - A scroll area's content is cut to it, moved by its offset.
//!
//! The result is an image and, per node the host tracks (interactive
//! ones, and keyed ones with a `hover` look), its key and rect.

use std::sync::Arc;

use cosmic_text::{Align as TAlign, Attrs, Buffer, Metrics, Shaping, Weight, Wrap};
use resvg::tiny_skia::{
    FillRule, FilterQuality, IntSize, Mask, Paint, PathBuilder, Pattern, Pixmap, PixmapPaint,
    SpreadMode, Stroke, Transform,
};

use super::canvas;
use super::layout::{self, Bounds, Laid, Rect};
use super::tree::{
    BoxVariant, ButtonVariant, Color, Fit, Kind, Live, Node, TextAlign, TextStyle, Theme,
    ThemeColor,
};
use crate::image::Image;
use crate::text::{Text, family};

/// What the painter takes from outside: decoded icons and images.
pub trait Resources {
    /// `app`'s icon, about `px` pixels.
    fn app_icon(&mut self, app: &str, px: u32) -> Option<Arc<Image>>;
    /// A theme icon (or a file), about `px` pixels.
    fn icon(&mut self, name: &str, px: u32) -> Option<Arc<Image>>;
    /// An image file, as it is.
    fn image(&mut self, src: &str) -> Option<Arc<Image>>;
}

/// Resources that have nothing.
pub struct NoResources;

impl Resources for NoResources {
    fn app_icon(&mut self, _: &str, _: u32) -> Option<Arc<Image>> {
        None
    }
    fn icon(&mut self, _: &str, _: u32) -> Option<Arc<Image>> {
        None
    }
    fn image(&mut self, _: &str) -> Option<Arc<Image>> {
        None
    }
}

/// What the host tracks of the pointer, by node key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    pub hovered: Option<String>,
    pub pressed: Option<String>,
    /// Scroll areas' offsets, logical pixels.
    pub scroll: Vec<(String, f32)>,
    /// Sliders being dragged: their values.
    pub sliders: Vec<(String, f64)>,
    /// For live values (`Live::at`'s clock); 0: show them as given.
    pub now: f64,
}

/// What to paint.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub tree: Arc<Node>,
    pub bounds: Bounds,
    pub scale: i32,
    pub theme: Arc<Theme>,
    /// The widget's app, for `ui.app_icon{}`.
    pub app: Option<String>,
    pub state: State,
    /// The image's corners are cut round to this radius (a popup's
    /// bubble); 0: square.
    pub radius: f32,
    /// Live sliders and progress bars (outside scroll areas) are left
    /// out of the image and listed in `Rendered::live`, so the host can
    /// paint them on their own as they move (`Request::part`).
    pub apart: bool,
}

impl Request {
    /// A request for `tree` in `bounds`: every other field the default.
    pub fn new(tree: Arc<Node>, bounds: Bounds, scale: i32, theme: Arc<Theme>) -> Self {
        Request {
            tree,
            bounds,
            scale,
            theme,
            app: None,
            state: State::default(),
            radius: 0.0,
            apart: false,
        }
    }

    /// The request painting just `part` of this one's tree, as big as
    /// its rect, with this one's state.
    pub fn part(&self, part: &LivePart) -> Request {
        let mut node = part.node.clone();
        node.layout = Default::default();
        node.look.opacity = Some(part.alpha);
        Request {
            tree: Arc::new(node),
            bounds: Bounds::Exact(part.rect.w.max(1.0), part.rect.h.max(1.0)),
            radius: 0.0,
            apart: false,
            ..self.clone()
        }
    }
}

/// A live node left out of an image painted `apart`.
#[derive(Clone, Debug, PartialEq)]
pub struct LivePart {
    /// Its index in the tree's walk.
    pub index: usize,
    /// Where it goes on the image, logical pixels.
    pub rect: Rect,
    /// Its own and its ancestors' opacity.
    pub alpha: f32,
    /// The node, without children (live nodes have none).
    pub node: Node,
}

/// A node the host tracks: where it is, as painted.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub key: String,
    /// The kind's name (`button`, `slider`, ...).
    pub kind: &'static str,
    pub rect: Rect,
    pub enabled: bool,
    /// A scroll area's content height.
    pub content: f32,
}

/// A painted tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    /// Logical size.
    pub width: f32,
    pub height: f32,
    /// `width` x `height` times the scale (rounded up).
    pub image: Image,
    /// Front to back: a later hit is on top.
    pub hits: Vec<Hit>,
    /// The live nodes left out (`Request::apart`), in tree order.
    pub live: Vec<LivePart>,
}

/// Lay `req.tree` out and paint it.
pub fn render(req: &Request, text: &mut Text, res: &mut dyn Resources) -> Rendered {
    let theme = &req.theme;
    let laid = layout::layout(&req.tree, req.bounds, theme, &mut |n, max| {
        measure(text, theme, n, max)
    });
    let (w, h) = laid.size();
    let s = req.scale.max(1) as f32;
    let (pw, ph) = (
        ((w * s).ceil() as u32).max(1),
        ((h * s).ceil() as u32).max(1),
    );
    let mut pixmap = Pixmap::new(pw, ph).unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1"));
    let nodes = req.tree.walk();
    let mut p = Painter {
        s,
        theme,
        text,
        res,
        laid: &laid,
        state: &req.state,
        app: req.app.as_deref(),
        hits: Vec::new(),
        nodes: &nodes,
        apart: req.apart,
        scrolled: 0,
        live: Vec::new(),
    };
    let mut index = 0;
    let fg = theme.get(ThemeColor::Text);
    p.node(&mut pixmap, &mut index, 0.0, fg, 1.0, true);
    let hits = std::mem::take(&mut p.hits);
    let live = std::mem::take(&mut p.live);
    let radius = req.radius.min(w.min(h) / 2.0);
    if radius > 0.0
        && let Some(mut mask) = Mask::new(pw, ph)
        && let Some(path) = crate::doc::rounded(0.0, 0.0, w * s, h * s, radius * s)
    {
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
        pixmap.apply_mask(&mask);
    }
    Rendered {
        width: w,
        height: h,
        image: crate::doc::bgra(&pixmap),
        hits,
        live,
    }
}

// -- text ------------------------------------------------------------------

/// A text node's font size, weight and color (None: the color it is on).
fn text_spec(theme: &Theme, n: &Node) -> Option<(f32, u16, Option<u32>)> {
    let Kind::Text {
        style,
        weight,
        color,
        size,
        ..
    } = &n.kind
    else {
        return None;
    };
    let base = theme.font_size;
    let (s, w, c) = match style {
        TextStyle::Title => (base * 1.25, 700, None),
        TextStyle::Body => (base, 400, None),
        TextStyle::Muted => (base, 400, Some(theme.get(ThemeColor::Muted))),
        TextStyle::Caption => (base * 0.85, 400, None),
    };
    let c = color.map(|c| theme.rgba(c)).or(c);
    Some((size.unwrap_or(s).max(1.0), weight.unwrap_or(w), c))
}

/// One line's height for a font size.
fn line_height(size: f32) -> f32 {
    (size * 1.3).ceil()
}

/// `text` with only `\n` for line breaks and no other control characters.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\n' => '\n',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect()
}

/// `text` shaped at `size` (pixels), wrapped at `width`, and cut with an
/// ellipsis after `max_lines`.
#[allow(clippy::too_many_arguments)]
fn shape(
    t: &mut Text,
    text: &str,
    font: &str,
    size: f32,
    weight: u16,
    width: Option<f32>,
    align: TextAlign,
    max_lines: Option<u32>,
) -> Buffer {
    let line_h = line_height(size);
    let attrs = Attrs::new().family(family(font)).weight(Weight(weight));
    let align = match align {
        TextAlign::Start => None,
        TextAlign::Center => Some(TAlign::Center),
        TextAlign::End => Some(TAlign::End),
    };
    let build = |t: &mut Text, s: &str, width: Option<f32>| {
        let mut b = Buffer::new(&mut t.fonts, Metrics::new(size, line_h));
        b.set_wrap(&mut t.fonts, Wrap::WordOrGlyph);
        b.set_size(&mut t.fonts, width, None);
        b.set_text(&mut t.fonts, s, &attrs, Shaping::Advanced, align);
        b
    };
    let text = clean(text);
    let buffer = build(t, &text, width);
    let Some(max) = max_lines.filter(|&m| m > 0) else {
        return buffer;
    };
    let runs: Vec<(usize, usize)> = (buffer.layout_runs())
        .map(|r| {
            (
                r.line_i,
                r.glyphs.iter().map(|g| g.start).min().unwrap_or(0),
            )
        })
        .collect();
    if runs.len() <= max as usize {
        return buffer;
    }
    // Where each paragraph starts in `text`.
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let (line, start) = runs[max as usize - 1];
    let cut = starts.get(line).copied().unwrap_or(0) + start;
    let keep = &text[..cut.min(text.len())];
    let rest: String = text[cut.min(text.len())..].replace('\n', " ");
    let room = width.unwrap_or(f32::MAX);
    let mut measure = |s: &str| {
        let b = build(t, s, None);
        b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    };
    let last = crate::text::ellipsize(rest.trim(), room, &mut measure);
    build(t, &format!("{keep}{last}"), width)
}

/// A buffer's size: its widest line and its lines' height.
fn extent(b: &Buffer) -> (f32, f32) {
    let (mut w, mut h) = (0.0f32, 0.0f32);
    for r in b.layout_runs() {
        w = w.max(r.line_w);
        h = h.max(r.line_top + r.line_height);
    }
    (w, h)
}

/// The size of text node `n` wrapped at `max` (logical pixels).
fn measure(t: &mut Text, theme: &Theme, n: &Node, max: Option<f32>) -> (f32, f32) {
    let Kind::Text {
        text,
        align,
        max_lines,
        ..
    } = &n.kind
    else {
        return (0.0, 0.0);
    };
    let Some((size, weight, _)) = text_spec(theme, n) else {
        return (0.0, 0.0);
    };
    let b = shape(t, text, &theme.font, size, weight, max, *align, *max_lines);
    let (w, h) = extent(&b);
    (w.ceil(), h.max(line_height(size)).ceil())
}

/// Draw `buffer` with its top left at (`x`, `y`) pixels.
fn draw_text(t: &mut Text, pm: &mut Pixmap, buffer: &Buffer, x: f32, y: f32, rgba: u32) {
    let [r, g, b, a] = rgba.to_be_bytes();
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let (x, y) = (x.round() as i32, y.round() as i32);
    let data = pm.data_mut();
    buffer.draw(
        &mut t.fonts,
        &mut t.glyphs,
        cosmic_text::Color::rgba(r, g, b, a),
        |gx, gy, gw, gh, c| {
            let alpha = c.a() as f32 / 255.0;
            if alpha <= 0.0 {
                return;
            }
            let src = [c.r(), c.g(), c.b()].map(|v| v as f32 * alpha);
            for py in (y + gy).max(0)..(y + gy + gh as i32).min(h) {
                for px in (x + gx).max(0)..(x + gx + gw as i32).min(w) {
                    let i = ((py * w + px) * 4) as usize;
                    let d = &mut data[i..i + 4];
                    let keep = 1.0 - alpha;
                    for k in 0..3 {
                        d[k] = (src[k] + d[k] as f32 * keep).round().min(255.0) as u8;
                    }
                    d[3] = (alpha * 255.0 + d[3] as f32 * keep).round().min(255.0) as u8;
                }
            }
        },
    );
}

// -- shapes and images -----------------------------------------------------

/// `rgba` with its alpha times `f`.
fn fade(rgba: u32, f: f32) -> u32 {
    crate::doc::fade(rgba, f as f64)
}

fn fill_rounded(pm: &mut Pixmap, r: (f32, f32, f32, f32), radius: f32, rgba: u32) {
    if rgba & 0xff == 0 {
        return;
    }
    let path = crate::doc::rounded(r.0, r.1, r.2, r.3, radius);
    crate::doc::fill(pm, path.as_ref(), rgba);
}

fn stroke_rounded(pm: &mut Pixmap, r: (f32, f32, f32, f32), radius: f32, width: f32, rgba: u32) {
    if rgba & 0xff == 0 || width <= 0.0 {
        return;
    }
    let h = width / 2.0;
    let path = crate::doc::rounded(r.0 + h, r.1 + h, r.2 - width, r.3 - width, radius - h);
    if let Some(path) = path {
        let stroke = Stroke {
            width,
            ..Stroke::default()
        };
        let paint = crate::doc::solid(rgba);
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

/// `img` (premultiplied BGRA) as a pixmap, its color replaced by `tint`.
fn pixmap_of(img: &Image, tint: Option<u32>) -> Option<Pixmap> {
    let size = IntSize::from_wh(img.width, img.height)?;
    let mut data = Vec::with_capacity(img.data.len());
    let tint = tint.map(|c| c.to_be_bytes().map(|v| v as f32 / 255.0));
    for px in img.data.as_chunks::<4>().0 {
        match tint {
            Some([r, g, b, a]) => {
                let alpha = px[3] as f32 / 255.0 * a;
                let c = |v: f32| (v * alpha * 255.0).round() as u8;
                data.extend_from_slice(&[c(r), c(g), c(b), (alpha * 255.0).round() as u8]);
            }
            None => data.extend_from_slice(&[px[2], px[1], px[0], px[3]]),
        }
    }
    Pixmap::from_vec(data, size)
}

/// Draw `src` into the box `r` (pixels) as `fit` says, cut to a rounded
/// rect of `radius`.
fn draw_pixmap(
    pm: &mut Pixmap,
    src: &Pixmap,
    r: (f32, f32, f32, f32),
    fit: Fit,
    radius: f32,
    opacity: f32,
) {
    let (iw, ih) = (src.width() as f32, src.height() as f32);
    if iw <= 0.0 || ih <= 0.0 || r.2 <= 0.0 || r.3 <= 0.0 {
        return;
    }
    let (kx, ky) = match fit {
        Fit::Cover => {
            let k = (r.2 / iw).max(r.3 / ih);
            (k, k)
        }
        Fit::Contain => {
            let k = (r.2 / iw).min(r.3 / ih);
            (k, k)
        }
        Fit::Fill => (r.2 / iw, r.3 / ih),
    };
    let (dw, dh) = (iw * kx, ih * ky);
    let (dx, dy) = (r.0 + (r.2 - dw) / 2.0, r.1 + (r.3 - dh) / 2.0);
    // What shows: the box, or the image inside it.
    let shown = match fit {
        Fit::Contain => (dx, dy, dw, dh),
        _ => r,
    };
    let Some(path) = crate::doc::rounded(shown.0, shown.1, shown.2, shown.3, radius) else {
        return;
    };
    let shader = Pattern::new(
        src.as_ref(),
        SpreadMode::Pad,
        FilterQuality::Bicubic,
        opacity.clamp(0.0, 1.0),
        Transform::from_row(kx, 0.0, 0.0, ky, dx, dy),
    );
    let paint = Paint {
        shader,
        anti_alias: true,
        ..Paint::default()
    };
    pm.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

/// `src` drawn to cover a `w` x `h` pixmap, blurred with `sigma`. Drawn
/// larger first, so the edges don't fade.
fn blurred(src: &Pixmap, w: u32, h: u32, sigma: f32) -> Option<Pixmap> {
    let m = (3.0 * sigma).ceil() as u32;
    let (bw, bh) = (w + 2 * m, h + 2 * m);
    let mut big = Pixmap::new(bw, bh)?;
    draw_pixmap(
        &mut big,
        src,
        (0.0, 0.0, bw as f32, bh as f32),
        Fit::Cover,
        0.0,
        1.0,
    );
    let (bwu, bhu) = (bw as usize, bh as usize);
    let mut planes = vec![vec![0f32; bwu * bhu]; 4];
    for (i, px) in big.data().as_chunks::<4>().0.iter().enumerate() {
        for k in 0..4 {
            planes[k][i] = px[k] as f32;
        }
    }
    for plane in &mut planes {
        for r in crate::shadow::box_radii(sigma as f64) {
            crate::shadow::box_blur(plane, bwu, bhu, r, true);
            crate::shadow::box_blur(plane, bwu, bhu, r, false);
        }
    }
    let mut out = Pixmap::new(w, h)?;
    let data = out.data_mut();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y + m as usize) * bwu + x + m as usize;
            let o = (y * w as usize + x) * 4;
            let a = planes[3][i].round().clamp(0.0, 255.0);
            data[o + 3] = a as u8;
            for k in 0..3 {
                data[o + k] = planes[k][i].round().clamp(0.0, a) as u8;
            }
        }
    }
    Some(out)
}

// -- the painter -----------------------------------------------------------

struct Painter<'a> {
    s: f32,
    theme: &'a Theme,
    text: &'a mut Text,
    res: &'a mut dyn Resources,
    laid: &'a Laid,
    state: &'a State,
    app: Option<&'a str>,
    hits: Vec<Hit>,
    nodes: &'a [&'a Node],
    /// Leave live nodes out, into `live`.
    apart: bool,
    /// How many scroll areas the node being painted is in.
    scrolled: u32,
    live: Vec<LivePart>,
}

impl Painter<'_> {
    fn rgba(&self, c: Color) -> u32 {
        self.theme.rgba(c)
    }

    /// Node `index`'s rect, moved up by `dy` (scrolling), in pixels.
    fn px(&self, r: Rect, dy: f32) -> (f32, f32, f32, f32) {
        let s = self.s;
        (r.x * s, (r.y - dy) * s, r.w * s, r.h * s)
    }

    fn scroll_of(&self, key: Option<&str>) -> f32 {
        let key = key.unwrap_or("");
        (self.state.scroll.iter())
            .find(|(k, _)| k == key)
            .map_or(0.0, |(_, v)| *v)
    }

    /// Paint node `*index` (and its children) onto `pm`, scrolled up by
    /// `dy`, on a background whose text color is `fg`, with every alpha
    /// times `alpha`. `live` is false for nodes the pointer can't reach
    /// (cut off by a scroll area).
    fn node(
        &mut self,
        pm: &mut Pixmap,
        index: &mut usize,
        dy: f32,
        fg: u32,
        alpha: f32,
        live: bool,
    ) {
        let i = *index;
        let n = self.nodes[i];
        *index += 1;
        let rect = self.laid.rects[i];
        let r = self.px(rect, dy);
        let key = n.key.as_deref();
        let hovered = key.is_some() && self.state.hovered.as_deref() == key;
        let pressed = key.is_some() && self.state.pressed.as_deref() == key;
        let mut look = n.look;
        if hovered && let Some(h) = &n.hover {
            look = look.with(h);
        }
        let alpha = alpha * look.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
        if alpha <= 0.0 || rect.w <= 0.0 || rect.h <= 0.0 {
            return self.skip_children(index, n);
        }
        let enabled = match n.kind {
            Kind::Button { enabled, .. }
            | Kind::Slider { enabled, .. }
            | Kind::Toggle { enabled, .. } => enabled,
            _ => true,
        };
        if live
            && let Some(key) = key
            && (n.kind.interactive() || n.handlers.any() || n.hover.is_some())
        {
            let visible = Rect {
                y: rect.y - dy,
                ..rect
            };
            self.hits.push(Hit {
                key: key.to_string(),
                kind: n.kind.name(),
                rect: visible,
                enabled,
                content: self.laid.content[i],
            });
        }
        // Painted on its own by the host, as it moves.
        let moves = matches!(
            n.kind,
            Kind::Slider { live: Some(_), .. } | Kind::Progress { live: Some(_), .. }
        );
        if self.apart && self.scrolled == 0 && moves {
            self.live.push(LivePart {
                index: i,
                rect,
                alpha,
                node: Node {
                    children: Vec::new(),
                    ..n.clone()
                },
            });
            return self.skip_children(index, n);
        }
        let s = self.s;
        let theme = self.theme;
        let mut fg = fg;
        // Kinds with a look of their own.
        let mut radius = look.radius.map(|v| v * s);
        let mut background = look.background.map(|c| self.rgba(c));
        match &n.kind {
            Kind::Box {
                variant: Some(variant),
            } => {
                let (bg, rad) = match variant {
                    BoxVariant::Card => (ThemeColor::Card, theme.radius * s),
                    BoxVariant::Inset => (ThemeColor::Inset, theme.radius * s),
                    BoxVariant::Badge => (ThemeColor::Badge, r.3 / 2.0),
                };
                background = background.or(Some(theme.get(bg)));
                radius = radius.or(Some(rad));
                if *variant == BoxVariant::Badge {
                    fg = theme.get(ThemeColor::BadgeText);
                }
            }
            Kind::Button {
                variant, selected, ..
            } => {
                radius = radius.or(Some(theme.radius * s));
                let filled = *variant == ButtonVariant::Filled || *selected;
                let base = match (filled, variant) {
                    (true, _) => Some(theme.get(ThemeColor::Accent)),
                    (false, ButtonVariant::Ghost) => Some(theme.get(ThemeColor::Card)),
                    _ => None,
                };
                background = background.or(base);
                if filled {
                    fg = theme.get(ThemeColor::AccentText);
                }
            }
            _ => {}
        }
        let rad = radius.unwrap_or(0.0);
        if let Some(bg) = background {
            fill_rounded(pm, r, rad, fade(bg, alpha));
        }
        // Buttons light up under the pointer.
        if let Kind::Button { .. } = n.kind
            && enabled
            && (hovered || pressed)
        {
            let c = match pressed {
                true => ThemeColor::Pressed,
                false => ThemeColor::Hover,
            };
            fill_rounded(pm, r, rad, fade(theme.get(c), alpha));
        }
        if let Some(b) = look.border {
            stroke_rounded(pm, r, rad, b.width * s, fade(self.rgba(b.color), alpha));
        }
        let alpha = match enabled {
            true => alpha,
            false => alpha * 0.4,
        };
        match &n.kind {
            Kind::Divider => {
                fill_rounded(pm, r, 0.0, fade(theme.get(ThemeColor::Divider), alpha));
            }
            Kind::Text {
                text,
                align,
                max_lines,
                ..
            } => {
                if let Some((size, weight, color)) = text_spec(theme, n) {
                    let b = shape(
                        self.text,
                        text,
                        &theme.font,
                        size * s,
                        weight,
                        Some(r.2 + 0.5),
                        *align,
                        *max_lines,
                    );
                    let c = fade(color.unwrap_or(fg), alpha);
                    draw_text(self.text, pm, &b, r.0, r.1, c);
                }
            }
            Kind::Icon { name, color, .. } => {
                let px = r.2.max(r.3).round().max(1.0) as u32;
                let tint = match color {
                    Some(c) => Some(self.rgba(*c)),
                    None => name.ends_with("-symbolic").then_some(fg),
                };
                if let Some(img) = self.res.icon(name, px)
                    && let Some(src) = pixmap_of(&img, tint)
                {
                    draw_pixmap(pm, &src, r, Fit::Contain, 0.0, alpha);
                }
            }
            Kind::AppIcon { app, .. } => {
                let px = r.2.min(r.3).round().max(1.0) as u32;
                let app = app.as_deref().or(self.app);
                if let Some(img) = app.and_then(|a| self.res.app_icon(a, px))
                    && let Some(src) = pixmap_of(&img, None)
                {
                    draw_pixmap(pm, &src, r, Fit::Contain, 0.0, alpha);
                }
            }
            Kind::Image {
                src,
                fit,
                radius,
                blur,
                opacity,
            } => {
                let img = self.res.image(src);
                let src = img.as_deref().and_then(|i| pixmap_of(i, None));
                let a = alpha * opacity.clamp(0.0, 1.0);
                match src {
                    Some(src) if *blur > 0.0 => {
                        let (w, h) = (r.2.ceil() as u32, r.3.ceil() as u32);
                        if let Some(b) = blurred(&src, w.max(1), h.max(1), blur * s / 2.0) {
                            draw_pixmap(pm, &b, r, Fit::Fill, radius * s, a);
                        }
                    }
                    Some(src) => draw_pixmap(pm, &src, r, *fit, radius * s, a),
                    None => {}
                }
            }
            Kind::Canvas { viewbox, commands } => {
                let (vw, vh) = (viewbox.0.max(1e-3), viewbox.1.max(1e-3));
                let base = Transform::from_row(r.2 / vw, 0.0, 0.0, r.3 / vh, r.0, r.1);
                let t = &mut *self.text;
                let font = &theme.font;
                canvas::paint(commands, pm, base, theme, alpha, &mut |pm, l| {
                    let size = l.size.max(1.0);
                    let weight = if l.bold { 700 } else { 400 };
                    let b = shape(t, l.text, font, size, weight, None, TextAlign::Start, None);
                    let (w, _) = extent(&b);
                    let x = match l.align {
                        TextAlign::Start => l.x,
                        TextAlign::Center => l.x - w / 2.0,
                        TextAlign::End => l.x - w,
                    };
                    draw_text(t, pm, &b, x, l.y - line_height(size) / 2.0, l.rgba);
                });
            }
            Kind::Slider {
                value,
                min,
                max,
                live: lv,
                ..
            } => {
                let dragged = (self.state.sliders.iter())
                    .find(|(k, _)| Some(k.as_str()) == key)
                    .map(|(_, v)| *v);
                let v = dragged.unwrap_or_else(|| live_value(*value, *lv, self.state.now));
                let f = fraction(v, *min, *max);
                let knob = (r.3 / 2.0 - 2.0 * s).max(2.0 * s);
                let knob = if (hovered || pressed) && enabled {
                    knob + s
                } else {
                    knob
                };
                let th = 4.0 * s;
                let (x0, x1) = (r.0 + knob, r.0 + r.2 - knob);
                let cy = r.1 + r.3 / 2.0;
                let track = (x0, cy - th / 2.0, (x1 - x0).max(0.0), th);
                fill_rounded(
                    pm,
                    track,
                    th / 2.0,
                    fade(theme.get(ThemeColor::Track), alpha),
                );
                let at = x0 + (x1 - x0) * f;
                let done = (x0, cy - th / 2.0, at - x0, th);
                fill_rounded(
                    pm,
                    done,
                    th / 2.0,
                    fade(theme.get(ThemeColor::Accent), alpha),
                );
                let dot = PathBuilder::from_circle(at, cy, knob);
                crate::doc::fill(pm, dot.as_ref(), fade(0xffffffff, alpha));
            }
            Kind::Toggle { value, .. } => {
                let track = match value {
                    true => ThemeColor::Accent,
                    false => ThemeColor::Track,
                };
                fill_rounded(pm, r, r.3 / 2.0, fade(theme.get(track), alpha));
                let k = r.3 / 2.0 - 2.0 * s;
                let cx = match value {
                    true => r.0 + r.2 - r.3 / 2.0,
                    false => r.0 + r.3 / 2.0,
                };
                let dot = PathBuilder::from_circle(cx, r.1 + r.3 / 2.0, k.max(1.0));
                crate::doc::fill(pm, dot.as_ref(), fade(0xffffffff, alpha));
            }
            Kind::Progress {
                value,
                max,
                live: lv,
            } => {
                let v = live_value(*value, *lv, self.state.now);
                let f = fraction(v, 0.0, *max);
                fill_rounded(pm, r, r.3 / 2.0, fade(theme.get(ThemeColor::Track), alpha));
                if f > 0.0 {
                    let done = (r.0, r.1, (r.2 * f).max(r.3), r.3);
                    fill_rounded(
                        pm,
                        done,
                        r.3 / 2.0,
                        fade(theme.get(ThemeColor::Accent), alpha),
                    );
                }
            }
            _ => {}
        }
        // The children; a scroll area's on a layer of their own, cut to
        // it and moved by its offset.
        if let Kind::Scroll = n.kind {
            let content = self.laid.content[i];
            let offset = self.scroll_of(key).clamp(0.0, (content - rect.h).max(0.0));
            let mut layer = Pixmap::new(pm.width(), pm.height());
            let mask = clip(pm, r);
            match (&mut layer, mask) {
                (Some(layer), Some(mask)) => {
                    self.scrolled += 1;
                    for _ in 0..n.children.len() {
                        self.child(layer, index, dy + offset, fg, alpha, live, rect, dy);
                    }
                    self.scrolled -= 1;
                    pm.draw_pixmap(
                        0,
                        0,
                        layer.as_ref(),
                        &PixmapPaint::default(),
                        Transform::identity(),
                        Some(&mask),
                    );
                }
                _ => self.skip_children(index, n),
            }
        } else {
            for _ in 0..n.children.len() {
                self.node(pm, index, dy, fg, alpha, live);
            }
        }
    }

    /// A scroll area's child: hits only while it shows in the area
    /// (`area`, scrolled by `area_dy`).
    #[allow(clippy::too_many_arguments)]
    fn child(
        &mut self,
        pm: &mut Pixmap,
        index: &mut usize,
        dy: f32,
        fg: u32,
        alpha: f32,
        live: bool,
        area: Rect,
        area_dy: f32,
    ) {
        let before = self.hits.len();
        self.node(pm, index, dy, fg, alpha, live);
        // Cut what it put in the hits to the area.
        let (top, bottom) = (area.y - area_dy, area.y - area_dy + area.h);
        let mut k = before;
        while k < self.hits.len() {
            let h = &mut self.hits[k];
            let y0 = h.rect.y.max(top);
            let y1 = (h.rect.y + h.rect.h).min(bottom);
            if y1 <= y0 {
                self.hits.remove(k);
                continue;
            }
            h.rect.y = y0;
            h.rect.h = y1 - y0;
            k += 1;
        }
    }

    fn skip_children(&self, index: &mut usize, n: &Node) {
        *index += n.count() - 1;
    }
}

/// A mask of rect `r` (pixels) on `pm`.
fn clip(pm: &Pixmap, r: (f32, f32, f32, f32)) -> Option<Mask> {
    let mut mask = Mask::new(pm.width(), pm.height())?;
    let path = crate::doc::rounded(r.0, r.1, r.2, r.3, 0.0)?;
    mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
    Some(mask)
}

/// `v` between `min` and `max`, 0 to 1.
fn fraction(v: f64, min: f64, max: f64) -> f32 {
    if max <= min || max.is_nan() || min.is_nan() || !v.is_finite() {
        return 0.0;
    }
    ((v - min) / (max - min)).clamp(0.0, 1.0) as f32
}

/// A live value at `now` (0: as given).
pub fn live_value(value: f64, live: Option<Live>, now: f64) -> f64 {
    match live {
        Some(l) if now > 0.0 && l.rate.is_finite() => value + l.rate * (now - l.at).max(0.0),
        _ => value,
    }
}
