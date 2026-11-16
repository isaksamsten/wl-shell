//! The launcher on screen: a layer surface over the whole output (layer
//! `overlay`, exclusive keyboard), transparent but for the bar and the
//! drum of matches.
//!
//! It is created without an output, so the compositor puts it on the
//! focused one, and maps fully transparent. Once `wl_surface.enter` says
//! which output that is, the bottom dock there hands over (`attach`): the
//! bar starts as the dock's panel (drawn here from then on, in the panel's
//! color, cross-fading to the bar's) and its rect moves to the bar's on a
//! spring, while the dock's icons shrink into it. The icon and the query
//! fade in as it settles, and the matches slide up onto the drum in
//! front of the bar. The keyboard is the launcher's from its first frame,
//! so nothing typed early is lost.
//!
//! The bar holds the selected match's icon (the front of the drum, larger
//! than the dock's icons, so it rises out of the bar), the query with its
//! caret, and the rest of the selected app's name: inline after the query
//! if the query is the start of it, else dimmed at the bar's end. The
//! other matches stand on the drum (drum.rs) above the front icon, each
//! with its name beside it in a pill (the query's matches brighter),
//! scaled with the icon; the roll is a spring, and returning matches
//! fade in and out in front of the bar. A re-ranking shows the new
//! best match in front without rolling, cross-fading the icons of the
//! slots whose match changed. With an empty query, the front shows
//! `system-search`.
//!
//! Closing runs it backwards: the matches slide down in front of the bar and
//! the bar becomes the dock's panel again. If an app was opened, its icon
//! flies from the front of the drum to its place in the dock. Closing
//! gives up the keyboard and the input at once, so the app opened gets
//! them.
//!
//! Texts (the query, the completion, the name) are lines from the worker;
//! a line shows its last text until the new one arrives. Icons come from
//! the shared cache.

use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use smithay_client_toolkit::compositor::{FrameCallbackData, Region};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerSurface,
};
use wayland_client::QueueHandle;
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_surface::WlSurface};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::icons::{self, NamedIcon};
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_render::{shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::shapes::{self, Axis, Shape, ShapeKey};
use wl_shell_ui::{IRect, Rect, Spring, Sub};

use crate::app::WlDock;
use crate::config::{Config, DockLayout};
use crate::drum::{self, Drum};
use crate::launcher::{Completion, Query, completion};
use crate::view::Handover;

type Qh = QueueHandle<Shell<WlDock>>;

/// The icon cross-fade when a slot's match changes, ms.
const FADE_RESPONSE: f64 = 120.0;
/// Shown while nothing was typed.
const PLACEHOLDER: &str = "Search apps";
/// The front's icon with an empty query.
const SEARCH_ICON: &str = "system-search";
/// Pixels of touchpad scrolling per roll step.
const SCROLL_STEP: f64 = 50.0;
/// The matches' names on the drum, of the query's font size.
const LABEL_FONT: f64 = 0.75;

/// Tags of text requests, unique in the process.
fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 52);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Everything the launcher takes from the config, for its dock's output.
#[derive(Clone, Debug, PartialEq)]
pub struct LauncherStyle {
    /// The dock's icon size.
    pub icon: f64,
    pub padding: f64,
    pub margin: f64,
    pub radius: f64,
    pub panel_color: u32,
    pub background: u32,
    pub shadow: Option<shadow::Settings>,
    pub icon_scale: f64,
    pub far_scale: f64,
    /// Slots on the drum.
    pub matches: usize,
    /// The bar's width, of the output's.
    pub width: f64,
    pub response: f64,
    pub font: String,
    pub font_size: f64,
    pub text: u32,
    pub completion: u32,
    pub caret: u32,
    pub icon_theme: Option<String>,
    pub icon_shadow: Option<shadow::IconShadow>,
}

impl LauncherStyle {
    pub fn new(config: &Config, layout: &DockLayout) -> Self {
        let theme = config.theme();
        Self {
            icon: layout.icon_size as f64,
            padding: config.padding as f64,
            margin: layout.margin as f64,
            radius: theme.panel_radius as f64,
            panel_color: theme.panel_color,
            background: theme.launcher_background,
            shadow: config.shadow(),
            icon_scale: config.launcher.icon_scale,
            far_scale: config.launcher.far_scale,
            matches: (config.launcher.matches as usize).max(1),
            width: config.launcher.width,
            response: config.appearance.response as f64,
            font: config.appearance.font.clone(),
            font_size: config.launcher_font_size(),
            text: theme.launcher_text,
            completion: theme.launcher_completion,
            caret: theme.launcher_caret,
            icon_theme: theme.icon_theme.clone(),
            icon_shadow: config.icon_shadow(),
        }
    }

    fn thickness(&self) -> f64 {
        self.icon + 2.0 * self.padding
    }

    /// The front icon's size.
    fn front(&self) -> f64 {
        (self.icon * self.icon_scale).round()
    }

    fn line_style(&self, color: u32) -> text::Style {
        text::Style {
            font: self.font.clone(),
            size: self.font_size,
            color,
            background: 0,
        }
    }

    /// A match's name beside its icon on the drum: dim, in a pill of the
    /// bar's color.
    fn label_style(&self) -> text::Style {
        text::Style {
            font: self.font.clone(),
            size: (self.font_size * LABEL_FONT).round(),
            color: self.completion,
            background: self.background,
        }
    }
}

/// A match as the launcher shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The app's desktop ID (without `.desktop`).
    pub id: String,
    pub name: String,
    /// Its `Icon=`.
    pub icon: Option<String>,
    /// The parts of the name the query matched, as byte ranges.
    pub matched: Vec<Range<usize>>,
}

/// What the pointer did on the launcher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherInput {
    None,
    /// The match at this ring index was clicked.
    Open(usize),
    /// A click outside the bar and the matches.
    Close,
}

/// A line of text from the worker, on its subsurface.
struct Line {
    sub: Sub,
    /// What it shows: the request, its buffer and logical size.
    shown: Option<(PillRequest, WlBuffer, f64, f64)>,
    /// Asked for and not arrived yet.
    pending: Option<(u64, PillRequest)>,
    /// In a pill, not bare.
    pill: bool,
}

impl Line {
    fn new(sub: Sub) -> Self {
        Self {
            sub,
            shown: None,
            pending: None,
            pill: false,
        }
    }

    fn pill(sub: Sub) -> Self {
        let mut line = Self::new(sub);
        line.pill = true;
        line
    }

    /// Show `request` (None: nothing), once it is rendered.
    fn want(&mut self, assets: &Assets, request: Option<PillRequest>, stale: &mut Vec<WlBuffer>) {
        let Some(request) = request else {
            self.pending = None;
            if let Some((_, buffer, ..)) = self.shown.take() {
                stale.push(buffer);
            }
            return;
        };
        let shown = self.shown.as_ref().is_some_and(|(r, ..)| *r == request);
        if shown {
            self.pending = None;
            return;
        }
        if self.pending.as_ref().is_some_and(|(_, r)| *r == request) {
            return;
        }
        let tag = next_tag();
        match self.pill {
            true => assets.pill(tag, request.clone()),
            false => assets.line(tag, request.clone()),
        }
        self.pending = Some((tag, request));
    }

    /// A text arrived; true if it is this line's.
    fn ready(
        &mut self,
        g: &Globals,
        qh: &Qh,
        tag: u64,
        pill: &text::Pill,
        stale: &mut Vec<WlBuffer>,
    ) -> bool {
        let Some((_, request)) = self.pending.take_if(|(t, _)| *t == tag) else {
            return false;
        };
        match cache::shm_buffer(g, &pill.image, qh) {
            Ok(buffer) => {
                let size = (pill.width as f64, pill.height as f64);
                if let Some((_, old, ..)) = self.shown.replace((request, buffer, size.0, size.1)) {
                    stale.push(old);
                }
            }
            Err(e) => warn!("launcher text buffer: {e}"),
        }
        true
    }

    fn width(&self) -> f64 {
        self.shown.as_ref().map_or(0.0, |(_, _, w, _)| *w)
    }

    fn present(&mut self, x: f64, y: f64, alpha: f64) {
        self.present_scaled(x, y, 1.0, alpha);
    }

    /// Present it `scale` times its size, with its top left at (`x`, `y`).
    fn present_scaled(&mut self, x: f64, y: f64, scale: f64, alpha: f64) -> Option<Rect> {
        let Self { sub, shown, .. } = self;
        match shown {
            Some((_, buffer, w, h)) => {
                let r = Rect::new(x, y, *w * scale, *h * scale);
                sub.present(Some(buffer), r, alpha);
                Some(r)
            }
            None => {
                sub.clear();
                None
            }
        }
    }

    fn height(&self) -> f64 {
        self.shown.as_ref().map_or(0.0, |(_, _, _, h)| *h)
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        if let Some((_, buffer, ..)) = self.shown.take() {
            buffer.destroy();
        }
    }
}

/// A match on the drum: its icon, and the one it replaced while that
/// fades out.
struct Ringed {
    m: Match,
    key: icons::Key,
    old: Option<icons::Key>,
    /// 0 (the old icon) to 1 (this one).
    fade: Spring,
}

/// A ring slot's subsurfaces.
struct RingSubs {
    icon: Sub,
    old: Sub,
    /// The match's name, beside its icon.
    label: Line,
}

/// The opened app's icon on its way to the dock.
struct Fly {
    key: icons::Key,
    from: Rect,
    to: Rect,
    t: Spring,
}

/// Where the dock is, on the launcher's surface.
struct Attached {
    output: WlOutput,
    style: LauncherStyle,
    drum: Drum,
    /// The dock's panel when the bar left it (or is headed back to).
    from: Rect,
    /// The bar at rest, and the resting dock's length it is at least.
    bar: Rect,
    rest_length: f64,
    panel_shape: Option<Shape>,
    bar_shape: Option<Shape>,
    /// The size the panel shapes were rendered for.
    rendered: (f64, f64),
}

pub struct LauncherView {
    // Field order is drop order: children before their parent surface.
    ring_subs: Vec<RingSubs>,
    fly_sub: Sub,
    panel: [Sub; 3],
    bar: [Sub; 3],
    search: Sub,
    query_line: Line,
    inline: Line,
    after: Line,
    caret: Sub,
    no_input: Region,
    viewport: WpViewport,
    layer: LayerSurface,
    transparent: WlBuffer,
    caret_buffer: Option<WlBuffer>,

    g: Rc<Globals>,
    qh: Qh,
    assets: Rc<Assets>,
    cache: Rc<std::cell::RefCell<Cache>>,

    width: f64,
    height: f64,
    scale: i32,
    configured: bool,
    attached: Option<Attached>,

    query: Query,
    ring: Vec<Ringed>,
    /// The drum's roll; the selection is where it is headed.
    roll: Spring,
    /// Scrolling not yet rolled, in steps.
    scrolled: f64,
    /// 0 (the dock's panel) to 1 (the bar).
    morph: Spring,
    /// The search icon shows (an empty ring), 0 to 1.
    searching: Spring,
    closing: bool,
    fly: Option<Fly>,
    /// Where the matches were drawn, for clicks: ring index and rect.
    hits: Vec<(usize, Rect)>,
    /// The bar as drawn.
    bar_at: Rect,
    /// Buffers replaced by new texts, destroyed after the next commit.
    stale: Vec<WlBuffer>,
    frame_pending: bool,
    last_frame: Option<Instant>,
}

impl LauncherView {
    /// The launcher's surface, mapped fully transparent; the compositor
    /// puts it on the focused output.
    pub fn new(
        core: &Core<WlDock>,
        assets: Rc<Assets>,
        cache: Rc<std::cell::RefCell<Cache>>,
        response: f64,
    ) -> std::io::Result<Self> {
        let g = core.g.clone();
        let qh = core.qh.clone();
        let transparent = cache::color_buffer(&g, 0, &qh)?;
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;
        let surface = g.compositor.create_surface(&qh);
        let viewport = g.viewporter.get_viewport(&surface, &qh, Ignore);
        let layer = (g.layer_shell).create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("wl-dock-launcher"),
            None,
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        let sub = || Sub::new(&g, layer.wl_surface(), &no_input, &qh);
        let panel = [sub(), sub(), sub()];
        let bar = [sub(), sub(), sub()];
        let search = sub();
        let inline = Line::new(sub());
        let query_line = Line::new(sub());
        let after = Line::new(sub());
        let caret = sub();
        let fly_sub = sub();
        layer.commit();
        Ok(Self {
            ring_subs: Vec::new(),
            fly_sub,
            panel,
            bar,
            search,
            query_line,
            inline,
            after,
            caret,
            no_input,
            viewport,
            layer,
            transparent,
            caret_buffer: None,
            g,
            qh,
            assets,
            cache,
            width: 0.0,
            height: 0.0,
            scale: 1,
            configured: false,
            attached: None,
            query: Query::default(),
            ring: Vec::new(),
            roll: Spring::new(0.0, response),
            scrolled: 0.0,
            morph: Spring::new(0.0, response),
            searching: Spring::new(1.0, FADE_RESPONSE),
            closing: false,
            fly: None,
            hits: Vec::new(),
            bar_at: Rect::default(),
            stale: Vec::new(),
            frame_pending: false,
            last_frame: None,
        })
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn is(&self, layer: &LayerSurface) -> bool {
        self.layer.wl_surface() == layer.wl_surface()
    }

    /// The output the dock it took over is on, once attached.
    pub fn output(&self) -> Option<&WlOutput> {
        self.attached.as_ref().map(|a| &a.output)
    }

    pub fn configure(&mut self, width: u32, height: u32) {
        let first = !self.configured;
        self.configured = true;
        (self.width, self.height) = (width as f64, height as f64);
        if width > 0 && height > 0 {
            self.viewport.set_destination(width as i32, height as i32);
        }
        if first {
            let surface = self.layer.wl_surface();
            surface.attach(Some(&self.transparent), 0, 0);
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        }
        if let Some(a) = &mut self.attached {
            a.bar = bar_rect(&a.style, a.rest_length, self.width, self.height);
        }
        self.render();
    }

    /// The dock on `output` hands over (`handover`, on its surface): the
    /// bar grows out of its panel. `rest_length` is the resting dock's
    /// length, which the bar is at least.
    pub fn attach(
        &mut self,
        output: WlOutput,
        scale: i32,
        style: LauncherStyle,
        handover: Handover,
    ) {
        self.scale = scale.max(1);
        let dy = self.height - handover.height;
        let from = Rect::new(
            handover.panel.x,
            handover.panel.y + dy,
            handover.panel.w,
            handover.panel.h,
        );
        let bar = bar_rect(&style, handover.rest_length, self.width, self.height);
        let drum = Drum::new(style.front(), style.matches, style.far_scale);
        self.roll = Spring::new(0.0, style.response);
        self.morph = Spring::new(0.0, style.response);
        self.morph.retarget(1.0);
        self.attached = Some(Attached {
            output,
            style,
            drum,
            from,
            bar,
            rest_length: handover.rest_length,
            panel_shape: None,
            bar_shape: None,
            rendered: (1.0, 1.0),
        });
        self.relayout();
        self.sync_texts();
        self.render();
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.relayout();
        for r in &mut self.ring {
            r.key = Self::icon_key(self.attached.as_ref(), &r.m, scale);
            r.old = None;
        }
        self.request_icons();
        self.sync_texts();
        self.kick();
    }

    /// Get the shapes for the style and scale.
    fn relayout(&mut self) {
        let scale = self.scale;
        let Some(a) = &mut self.attached else { return };
        let thickness = a.style.thickness();
        let radius = a.style.radius.min(thickness / 2.0).floor();
        let length = (2.0 * radius + 4.0).max(8.0);
        a.rendered = (length, thickness);
        let mut cache = self.cache.borrow_mut();
        let key = |color| ShapeKey::Panel {
            w: length as i32,
            h: thickness as i32,
            radius: radius as i32,
            color,
            shadow: a.style.shadow,
            scale,
        };
        a.panel_shape = cache.shape(&self.g, key(a.style.panel_color), &self.qh);
        a.bar_shape = cache.shape(&self.g, key(a.style.background), &self.qh);
        drop(cache);
        let caret = a.style.caret;
        if let Some(old) = self.caret_buffer.take() {
            self.stale.push(old);
        }
        self.caret_buffer = cache::color_buffer(&self.g, caret, &self.qh).ok();
        self.search_key().inspect(|k| self.assets.named_icon(k));
    }

    fn icon_key(attached: Option<&Attached>, m: &Match, scale: i32) -> icons::Key {
        let (size, theme, shadow) = match attached {
            Some(a) => (
                a.style.front(),
                a.style.icon_theme.clone(),
                a.style.icon_shadow,
            ),
            None => (60.0, None, None),
        };
        icons::Key::new(&m.id, size.ceil() as u32, theme, shadow, scale).with_icon(m.icon.clone())
    }

    fn search_key(&self) -> Option<NamedIcon> {
        let a = self.attached.as_ref()?;
        Some(NamedIcon {
            name: SEARCH_ICON.into(),
            fallbacks: vec!["edit-find".into(), "search".into()],
            size: a.style.front().ceil() as u32 * self.scale as u32,
            theme: a.style.icon_theme.clone(),
            shadow: a.style.icon_shadow,
            scale: self.scale,
        })
    }

    fn request_icons(&self) {
        let cache = self.cache.borrow();
        let font = self
            .attached
            .as_ref()
            .map_or("sans-serif", |a| a.style.font.as_str());
        for r in &self.ring {
            if cache.icon(&r.key).is_none() {
                self.assets.icon(&r.key, font);
            }
        }
    }

    // -- the query and the matches -------------------------------------------

    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn query_mut(&mut self) -> &mut Query {
        &mut self.query
    }

    /// Show `matches`, best first: the best in front, without rolling.
    /// Slots whose match changed cross-fade their icons.
    pub fn set_matches(&mut self, matches: Vec<Match>) {
        let slots = self.attached.as_ref().map_or(1, |a| a.style.matches);
        // What each slot shows now: slot `i` holds the match `i` past the
        // one in front.
        let front = self.selected().unwrap_or(0);
        let n = self.ring.len();
        let old: Vec<icons::Key> = (0..n)
            .map(|i| self.ring[(i + front) % n].key.clone())
            .collect();
        let response = FADE_RESPONSE;
        let attached = self.attached.as_ref();
        let ring: Vec<Ringed> = (matches.into_iter().take(slots).enumerate())
            .map(|(i, m)| {
                let key = Self::icon_key(attached, &m, self.scale);
                let prev = old.get(i).cloned();
                let same = prev.as_ref() == Some(&key);
                let mut fade = Spring::new(if same { 1.0 } else { 0.0 }, response);
                fade.retarget(1.0);
                Ringed {
                    m,
                    key,
                    old: prev.filter(|_| !same),
                    fade,
                }
            })
            .collect();
        self.ring = ring;
        self.roll.snap(0.0);
        self.searching
            .retarget(if self.ring.is_empty() { 1.0 } else { 0.0 });
        self.request_icons();
        self.sync_texts();
        self.kick();
    }

    /// Roll the drum `by` steps (positive: the next match comes to the
    /// front).
    pub fn roll(&mut self, by: i32) {
        if self.ring.len() < 2 || self.closing {
            return;
        }
        let target = self.roll.target + by as f64;
        self.roll.retarget(target);
        self.sync_texts();
        self.kick();
    }

    /// The ring index of the selection.
    pub fn selected(&self) -> Option<usize> {
        (!self.ring.is_empty()).then(|| drum::selected(self.roll.target, self.ring.len()))
    }

    pub fn match_at(&self, i: usize) -> Option<&Match> {
        self.ring.get(i).map(|r| &r.m)
    }

    /// Ask for the texts the query and the selection need.
    pub fn sync_texts(&mut self) {
        let Some(a) = &self.attached else { return };
        let style = &a.style;
        let scale = self.scale;
        let max = (a.bar.w - text_x(style) - 2.0 * style.padding).max(1.0) as i32;
        let line = |text: String, color: u32| PillRequest {
            text,
            spans: Vec::new(),
            max_width: max,
            scale,
            style: style.line_style(color),
        };
        // The names on the drum, with what the query matched brighter.
        let labels: Vec<PillRequest> = (self.ring.iter())
            .map(|r| PillRequest {
                text: r.m.name.clone(),
                spans: r
                    .m
                    .matched
                    .iter()
                    .map(|m| (m.clone(), style.text))
                    .collect(),
                max_width: max,
                scale,
                style: style.label_style(),
            })
            .collect();
        let name = self
            .selected()
            .and_then(|i| self.ring.get(i))
            .map(|r| r.m.name.as_str());
        let q = self.query.text();
        // Zero-width spaces keep the query's own spaces at its ends.
        let query = (!q.is_empty()).then(|| line(format!("\u{200b}{q}\u{200b}"), style.text));
        let (inline, after) = match completion(q, name) {
            Completion::None if q.is_empty() => (Some(PLACEHOLDER.to_string()), None),
            Completion::None => (None, None),
            Completion::Inline(rest) if rest.is_empty() => (None, None),
            Completion::Inline(rest) => (Some(rest), None),
            Completion::After(name) => (None, Some(name)),
        };
        let inline = inline.map(|t| line(t, style.completion));
        let after = after.map(|t| line(t, style.completion));
        let assets = self.assets.clone();
        // The query first, so its width is there when the rest arrives.
        self.query_line.want(&assets, query, &mut self.stale);
        self.inline.want(&assets, inline, &mut self.stale);
        self.after.want(&assets, after, &mut self.stale);
        self.grow_ring_subs(labels.len());
        let mut labels = labels.into_iter();
        for subs in &mut self.ring_subs {
            subs.label.want(&assets, labels.next(), &mut self.stale);
        }
    }

    /// At least `n` ring slots' subsurfaces.
    fn grow_ring_subs(&mut self, n: usize) {
        while self.ring_subs.len() < n {
            let sub = || Sub::new(&self.g, self.layer.wl_surface(), &self.no_input, &self.qh);
            let subs = RingSubs {
                icon: sub(),
                old: sub(),
                label: Line::pill(sub()),
            };
            subs.icon.place_below(self.query_line.sub.surface());
            subs.old.place_below(self.query_line.sub.surface());
            subs.label.sub.place_below(self.query_line.sub.surface());
            self.ring_subs.push(subs);
        }
    }

    /// A text from the worker; true if it was the launcher's.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let _ = request;
        let (g, qh) = (self.g.clone(), self.qh.clone());
        let ours = [&mut self.query_line, &mut self.inline, &mut self.after]
            .into_iter()
            .chain(self.ring_subs.iter_mut().map(|s| &mut s.label))
            .any(|l| l.ready(&g, &qh, tag, pill, &mut self.stale));
        if ours {
            self.kick();
        }
        ours
    }

    /// An icon arrived in the cache.
    pub fn icon_ready(&mut self) {
        self.kick();
    }

    // -- closing ---------------------------------------------------------------

    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Run it backwards to `panel` (the dock's, on a dock surface `height`
    /// tall), sending the front icon to `fly_to` (on the dock's surface)
    /// if an app was opened. Gives up the keyboard and the input now.
    pub fn close(&mut self, back: Option<Handover>, fly_to: Option<Rect>) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        (self.layer.wl_surface()).set_input_region(Some(self.no_input.wl_region()));
        self.morph.retarget(0.0);
        let dy = |h: f64| self.height - h;
        if let (Some(a), Some(back)) = (&mut self.attached, back) {
            let d = dy(back.height);
            a.from = Rect::new(back.panel.x, back.panel.y + d, back.panel.w, back.panel.h);
        }
        let front = self.front_rect();
        let selected = self.selected().and_then(|i| self.ring.get(i));
        if let (Some(to), Some(front), Some(r), Some(back)) = (fly_to, front, selected, back) {
            let d = dy(back.height);
            let response = self
                .attached
                .as_ref()
                .map_or(150.0, |a| a.style.response * 1.5);
            let mut t = Spring::new(0.0, response);
            t.retarget(1.0);
            self.fly = Some(Fly {
                key: r.key.clone(),
                from: front,
                to: Rect::new(to.x, to.y + d, to.w, to.h),
                t,
            });
        }
        self.layer.commit();
        self.kick();
    }

    /// Closed and out of sight: it can go.
    pub fn gone(&self) -> bool {
        let done = self.morph.settled() && self.fly.is_none();
        self.closing && (done || self.attached.is_none())
    }

    // -- input -----------------------------------------------------------------

    /// A pointer event on the launcher.
    pub fn pointer(&mut self, event: &PointerEvent) -> LauncherInput {
        if self.closing {
            return LauncherInput::None;
        }
        let (x, y) = event.position;
        match event.kind {
            PointerEventKind::Release { .. } => {
                if let Some((i, _)) = self.hits.iter().find(|(_, r)| r.contains(x, y)) {
                    return LauncherInput::Open(*i);
                }
                if self.bar_at.contains(x, y) || self.attached.is_none() {
                    return LauncherInput::None;
                }
                LauncherInput::Close
            }
            PointerEventKind::Axis { vertical, .. } => {
                let steps = match (vertical.value120, vertical.discrete) {
                    (0, 0) => vertical.absolute / SCROLL_STEP,
                    (0, d) => d as f64,
                    (v, _) => v as f64 / 120.0,
                };
                if self.scrolled * steps < 0.0 {
                    self.scrolled = 0.0;
                }
                self.scrolled += steps;
                while self.scrolled.abs() >= 1.0 - 1e-6 {
                    // Scrolling down brings the next match forward.
                    let step = self.scrolled.signum();
                    self.roll(step as i32);
                    self.scrolled -= step;
                }
                LauncherInput::None
            }
            _ => LauncherInput::None,
        }
    }

    // -- frames ------------------------------------------------------------------

    pub fn kick(&mut self) {
        if !self.configured || self.frame_pending {
            return;
        }
        if self.last_frame.is_none() {
            self.last_frame = Some(Instant::now());
        }
        self.request_frame();
        self.layer.commit();
    }

    fn request_frame(&mut self) {
        let surface = self.layer.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
    }

    fn moving(&self) -> bool {
        !self.morph.settled()
            || !self.roll.settled()
            || !self.searching.settled()
            || self.ring.iter().any(|r| !r.fade.settled())
            || self.fly.is_some()
    }

    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        let now = Instant::now();
        let dt = (self.last_frame).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let mut moving = self.morph.advance(dt);
        moving |= self.roll.advance(dt);
        moving |= self.searching.advance(dt);
        for r in &mut self.ring {
            moving |= r.fade.advance(dt);
            if r.fade.settled() {
                r.old = None;
            }
        }
        if let Some(fly) = &mut self.fly {
            moving |= fly.t.advance(dt);
            if fly.t.settled() {
                self.fly = None;
            }
        }
        self.last_frame = moving.then_some(now);
        self.render();
    }

    /// An alpha as shown: without wp_alpha_modifier_v1, things are either
    /// there or not.
    fn gate(&self, a: f64) -> f64 {
        let a = a.clamp(0.0, 1.0);
        match self.g.alpha_modifier {
            Some(_) => a,
            None if a >= 0.5 => 1.0,
            None => 0.0,
        }
    }

    /// The front icon's rect, as drawn now.
    fn front_rect(&self) -> Option<Rect> {
        let a = self.attached.as_ref()?;
        let bar = a.from.lerp(a.bar, self.morph.value);
        Some(front_rect(&a.style, bar))
    }

    pub fn render(&mut self) {
        if !self.configured || self.width <= 0.0 || self.height <= 0.0 {
            return;
        }
        self.place_all();
        if self.moving() && !self.frame_pending {
            self.request_frame();
        }
        self.layer.commit();
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
    }

    fn place_all(&mut self) {
        let Some(a) = &self.attached else {
            self.hits.clear();
            return;
        };
        let style = a.style.clone();
        let drum = a.drum;
        let scale = self.scale as f64;
        let m = self.morph.value.clamp(0.0, 1.0);
        let bar = a.from.lerp(a.bar, self.morph.value);
        self.bar_at = bar;

        // The panel turns into the bar.
        let (panel_shape, bar_shape, rendered) =
            (a.panel_shape.clone(), a.bar_shape.clone(), a.rendered);
        let (pa, ba) = (self.gate(1.0 - m), self.gate(m));
        for (subs, shape, alpha) in [
            (&mut self.panel, panel_shape, pa),
            (&mut self.bar, bar_shape, ba),
        ] {
            let round = &IRect::round;
            shapes::place_panel(
                subs,
                shape.as_ref(),
                bar,
                rendered,
                Axis::Horizontal,
                scale,
                round,
                alpha,
            );
        }

        // The drum.
        let front = front_rect(&style, bar);
        let (cx, cy) = front.center();
        // The matches slide up in front of the bar as it opens.
        let open = m;
        let n = self.ring.len();
        self.grow_ring_subs(n.max(1));
        let cache = self.cache.borrow();
        let fly_key = self.fly.as_ref().map(|f| f.key.clone());
        self.hits.clear();
        let present = 1.0 - self.searching.value.clamp(0.0, 1.0);
        for (i, subs) in self.ring_subs.iter_mut().enumerate() {
            let Some(r) = self.ring.get(i) else {
                subs.icon.clear();
                subs.old.clear();
                subs.label.sub.clear();
                continue;
            };
            let t = drum::position(i, self.roll.value, n);
            let p = drum.place(t, n);
            let size = p.size;
            let center = (cx, cy - p.rise * open);
            let rect = Rect::new(center.0 - size / 2.0, center.1 - size / 2.0, size, size);
            // The front one is flying to the dock.
            let flying = fly_key.as_ref() == Some(&r.key) && t < 0.5;
            let gate = |a: f64| match self.g.alpha_modifier {
                Some(_) => a.clamp(0.0, 1.0),
                None if a >= 0.5 => 1.0,
                None => 0.0,
            };
            let alpha = gate(p.alpha * open * present);
            let fade = r.fade.value.clamp(0.0, 1.0);
            let place = |sub: &mut Sub, key: &icons::Key, a: f64| {
                let pad = key.pad() as f64 * size / (key.size as f64 / key.scale as f64);
                sub.present(cache.icon(key).as_ref(), rect.outset(pad), a);
            };
            match flying {
                true => subs.icon.clear(),
                false => place(&mut subs.icon, &r.key, alpha * fade),
            }
            match (&r.old, flying) {
                (Some(old), false) => place(&mut subs.old, old, alpha * (1.0 - fade)),
                _ => subs.old.clear(),
            }
            if alpha > 0.3 {
                self.hits.push((i, rect));
            }
            // Its name beside it, scaled with it. The front's is in the
            // bar, so it fades in as the match rolls up out of it.
            let k = size / drum.front;
            let label_in = if p.behind { 0.0 } else { t.min(1.0) };
            let label_alpha = gate(p.alpha * open * present * label_in);
            let (x, y) = (rect.right() + style.padding * k, center.1);
            let h = subs.label.height() * k;
            let label = subs.label.present_scaled(x, y - h / 2.0, k, label_alpha);
            if let Some(label) = label.filter(|_| label_alpha > 0.3) {
                self.hits.push((i, label));
            }
        }

        // The search icon, with nothing typed.
        let search = self
            .search_key()
            .and_then(|k| Some((cache.named_icon(&k)??, k)));
        let search_alpha = self.gate(self.searching.value * open);
        match &search {
            Some((buffer, key)) if search_alpha > 0.0 => {
                let pad = key.pad() as f64;
                self.search
                    .present(Some(buffer), front.outset(pad), search_alpha);
            }
            _ => self.search.clear(),
        }

        // The opened app's icon, on its way to the dock.
        match &self.fly {
            Some(fly) => {
                let t = fly.t.value;
                let r = fly.from.lerp(fly.to, t);
                let pad = fly.key.pad() as f64 * r.w / (fly.key.size as f64 / fly.key.scale as f64);
                // Into the dock's own icon at the end.
                let alpha = self.gate(((1.0 - t) / 0.15).clamp(0.0, 1.0));
                (self.fly_sub).present(cache.icon(&fly.key).as_ref(), r.outset(pad), alpha);
            }
            None => self.fly_sub.clear(),
        }
        drop(cache);

        // The query, the completion and the caret.
        let text_alpha = self.gate((m - 0.5) / 0.5);
        let line_h = text::line_height(style.font_size) as f64;
        let x0 = bar.x + text_x(&style);
        let ty = (bar.y + (bar.h - line_h) / 2.0).round();
        let query_w = match self.query.is_empty() {
            true => 0.0,
            false => self.query_line.width(),
        };
        match self.query.is_empty() {
            true => self.query_line.sub.clear(),
            false => self.query_line.present(x0, ty, text_alpha),
        }
        self.inline.present(x0 + query_w, ty, text_alpha);
        let after_x = bar.right() - 2.0 * style.padding - self.after.width();
        self.after
            .present(after_x.max(x0 + query_w + style.padding), ty, text_alpha);
        let caret_w = (style.font_size / 12.0).round().max(1.0);
        let caret = Rect::new(x0 + query_w, ty, caret_w, line_h);
        let caret_alpha = if self.closing { 0.0 } else { text_alpha };
        self.caret
            .present(self.caret_buffer.as_ref(), caret, caret_alpha);
    }
}

/// Where the query starts, from the bar's left edge.
fn text_x(style: &LauncherStyle) -> f64 {
    2.0 * style.padding + style.front() + 2.0 * style.padding
}

/// The front icon in the bar at `bar`: at its start, rising out of it
/// from the panel's inner edge, as a magnified icon does.
fn front_rect(style: &LauncherStyle, bar: Rect) -> Rect {
    let f = style.front();
    let bottom = bar.bottom() - style.padding;
    Rect::new(bar.x + 2.0 * style.padding, bottom - f, f, f)
}

/// The bar at rest on a `width` x `height` output: centered at the
/// bottom where the panel is, `launcher-width` of the output wide, but at
/// least `rest_length`, and inside the margins.
fn bar_rect(style: &LauncherStyle, rest_length: f64, width: f64, height: f64) -> Rect {
    let thickness = style.thickness();
    let room = (width - 2.0 * style.margin).max(thickness);
    let w = (style.width * width).max(rest_length).min(room).round();
    let x = ((width - w) / 2.0).round();
    Rect::new(x, height - style.margin - thickness, w, thickness)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> LauncherStyle {
        LauncherStyle::new(&Config::default(), &Config::default().dock("X").unwrap())
    }

    #[test]
    fn the_bar_sits_where_the_panel_is() {
        let s = style();
        let bar = bar_rect(&s, 300.0, 1920.0, 1080.0);
        assert_eq!(bar.w, 768.0);
        assert_eq!(bar.x, (1920.0 - 768.0) / 2.0);
        assert_eq!(bar.bottom(), 1080.0 - s.margin);
        assert_eq!(bar.h, s.icon + 2.0 * s.padding);
        // At least the resting dock, inside the margins.
        assert_eq!(bar_rect(&s, 1000.0, 1920.0, 1080.0).w, 1000.0);
        assert_eq!(
            bar_rect(&s, 5000.0, 1920.0, 1080.0).w,
            1920.0 - 2.0 * s.margin
        );
    }

    #[test]
    fn the_front_icon_rises_out_of_the_bar() {
        let s = style();
        let bar = bar_rect(&s, 0.0, 1920.0, 1080.0);
        let f = front_rect(&s, bar);
        assert_eq!(f.w, 60.0);
        assert_eq!(f.bottom(), bar.bottom() - s.padding);
        assert!(f.y < bar.y);
        assert!(text_x(&s) > f.right() - bar.x);
    }
}
