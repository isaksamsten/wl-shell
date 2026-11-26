//! A widget's popup on screen: an xdg_popup of the dock surface, a speech
//! bubble like the menu's (menuview.rs) pointing at the widget's icon,
//! with the plugin's tree painted inside.
//!
//! Opening takes a few steps, none blocking: the tree is painted first
//! (its size is the bubble's body), then the popup is created next to the
//! icon, and the bubble rendered for where the compositor put it, its
//! tail pointing at the icon's center even when the popup was slid along
//! the dock. It shows once it is configured and both are there, growing
//! out of the tail's tip like the menu; closing, it fades out quickly and
//! the dock drops it once it's gone.
//!
//! On the popup, as subsurfaces: the bubble (its shadow included), the
//! tree, and per live or animated node one of its own. The tree is
//! painted `apart`: without those nodes, which are shown on their own,
//! at their rects. Live sliders and progress bars (`live = {rate, at}`),
//! indeterminate ones and values easing in (`transition`) are painted
//! alone again and again as they move (as often as that moves them a
//! pixel, at most 30 times a second), so a playing track's seek bar
//! advances without repainting the popup. Animated canvases (`frames`)
//! are painted once into a frame sheet and played from it (anim.rs), at
//! most 60 frames a second. One timer paces them all, set again once the
//! compositor shows the last frame. Live nodes inside a scroll area are
//! painted with the tree instead, as they are at the time.
//!
//! Interaction state (hover, pressed, slider drags, scroll offsets) is
//! kept here and painted without asking the plugin; what it changes is
//! repainted: only the live layers if nothing else is touched. A popup
//! whose tree changes size is moved (xdg_popup v3), or opened again.
//! Its input region is the bubble, not its shadow.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use smithay_client_toolkit::compositor::{FrameCallbackData, Region};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::xdg::{XdgPositioner, popup::Popup};
use wayland_client::{
    Proxy, QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use wl_shell_core::Shell;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::popup::{self, ConstraintAdjustment, PopupConfigure};
use wl_shell_log::warn;
use wl_shell_lua::channel::PopupEvent;
use wl_shell_render::Image;
use wl_shell_render::bubble::{self, Bubble};
use wl_shell_render::ui::{self, Kind};
use wl_shell_render::worker::Assets;
use wl_shell_ui::cache;
use wl_shell_ui::{IRect, Rect, Spring, Sub, lerp};

use crate::anim::{self, Pacer, Sheets};
use crate::app::WlDock;
use crate::config::Position;
use crate::menu::{self, Metrics, Place};
use crate::menuview::MenuStyle;

type Qh = QueueHandle<Shell<WlDock>>;

/// Opening: the scale and fade, ms.
const OPEN_RESPONSE: f64 = 120.0;
/// Closing: the fade, ms.
const CLOSE_RESPONSE: f64 = 80.0;
/// The bubble grows from this scale.
const OPEN_SCALE: f64 = 0.9;
/// The largest a tree is laid out, logical pixels.
const MAX_W: f32 = 640.0;
const MAX_H: f32 = 620.0;
/// A dragged slider's `on_change` at most this often.
const CHANGE_EVERY: Duration = Duration::from_millis(33);
/// A live layer is repainted at most and at least this often, seconds.
const LIVE_FASTEST: f64 = 1.0 / 30.0;
const LIVE_SLOWEST: f64 = 1.0;
/// Scrolling by a wheel's click, logical pixels.
const WHEEL_STEP: f32 = 32.0;
const BTN_LEFT: u32 = 0x110;

/// Tags of tree and bubble requests, unique in the process.
fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 52);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Seconds since the epoch: the clock of `live.at` (Lua's `os.time()`).
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// The painted tree.
struct Painted {
    request: ui::Request,
    hits: Vec<ui::Hit>,
    live: Vec<ui::LivePart>,
    /// Logical size, whole pixels.
    size: (f64, f64),
    buffer: WlBuffer,
}

/// A live node on its own subsurface.
struct LiveLayer {
    sub: Sub,
    part: ui::LivePart,
    /// The paint in flight.
    wanted: Option<u64>,
    /// Another paint is due once it's in.
    again: bool,
    buffer: Option<WlBuffer>,
}

impl LiveLayer {
    fn key(&self) -> Option<&str> {
        self.part.node.key.as_deref()
    }

    fn interval(&self, scale: i32) -> Option<f64> {
        interval(&self.part, scale)
    }
}

/// How long a live `part`'s value takes to move a pixel at `scale`, kept
/// between `LIVE_FASTEST` and `LIVE_SLOWEST`; None if it doesn't move.
fn interval(part: &ui::LivePart, scale: i32) -> Option<f64> {
    let (lo, hi, live) = match part.node.kind {
        Kind::Slider { min, max, live, .. } => (min, max, live?),
        Kind::Progress { max, live, .. } => (0.0, max, live?),
        _ => return None,
    };
    if live.rate == 0.0 || !live.rate.is_finite() {
        return None;
    }
    let px = (part.rect.w as f64 * scale as f64).max(1.0);
    let per_px = (hi - lo).abs() / px;
    Some((per_px / live.rate.abs()).clamp(LIVE_FASTEST, LIVE_SLOWEST))
}

/// The popup and its subsurfaces, once the tree is painted.
struct Surface {
    // Field order is drop order: children before their parent.
    lives: Vec<LiveLayer>,
    /// Per animated canvas (`WidgetView::sheets`).
    sheet_subs: Vec<Sub>,
    content: Sub,
    bubble: Sub,
    viewport: WpViewport,
    popup: Popup,
    place: Place,
    /// The popup's position on the dock surface, once configured.
    at: Option<(f64, f64)>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.viewport.destroy();
    }
}

pub struct WidgetView {
    surface: Option<Surface>,
    content: Option<Painted>,
    /// The tree's paint in flight, and whether another is due after it.
    wanted: Option<u64>,
    again: bool,
    /// The bubble shown, and the one asked for.
    bubble: Option<(Bubble, WlBuffer)>,
    bubble_wanted: Option<(u64, Bubble)>,
    /// Replaced buffers, destroyed once the popup no longer shows them.
    stale: Vec<WlBuffer>,
    no_input: Region,
    transparent: WlBuffer,
    g: Rc<Globals>,
    assets: Rc<Assets>,
    qh: Qh,
    layer: LayerSurface,

    /// The app whose widget this is.
    pub app: String,
    /// Tells this popup from the next one (for its live layers' timer).
    pub id: u64,
    style: MenuStyle,
    position: Position,
    /// The icon on the dock surface.
    icon: Rect,
    scale: i32,
    theme: Arc<ui::Theme>,
    tree: Arc<ui::Node>,
    state: ui::State,

    /// 0: hidden, 1: open.
    open: Spring,
    shown: bool,
    closing: bool,
    /// When a dragged slider last told the plugin.
    last_change: Option<Instant>,
    /// The timer for what moves.
    pacer: Pacer,
    /// Plays the animated nodes.
    player: ui::Player,
    /// The animated canvases, and their frame sheets.
    sheets: Sheets<WlBuffer>,
}

impl WidgetView {
    /// The popup of `app`'s widget showing `tree`, for its icon at `icon`
    /// on the dock `layer` at `position`. It shows once the tree is
    /// painted and the popup configured.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        g: Rc<Globals>,
        assets: Rc<Assets>,
        qh: &Qh,
        layer: &LayerSurface,
        app: String,
        tree: Arc<ui::Node>,
        style: MenuStyle,
        theme: Arc<ui::Theme>,
        position: Position,
        icon: Rect,
        scale: i32,
        animations: bool,
    ) -> Result<Self, String> {
        let transparent = cache::color_buffer(&g, 0, qh).map_err(|e| e.to_string())?;
        let no_input = Region::new(&g.compositor).map_err(|e| e.to_string())?;
        let mut w = Self {
            surface: None,
            content: None,
            wanted: None,
            again: false,
            bubble: None,
            bubble_wanted: None,
            stale: Vec::new(),
            no_input,
            transparent,
            g,
            assets,
            qh: qh.clone(),
            layer: layer.clone(),
            app,
            id: next_tag(),
            style,
            position,
            icon,
            scale: scale.max(1),
            theme,
            tree,
            state: ui::State::default(),
            open: Spring::new(0.0, OPEN_RESPONSE),
            shown: false,
            closing: false,
            last_change: None,
            pacer: Pacer::default(),
            player: ui::Player::new(animations),
            sheets: Sheets::default(),
        };
        w.paint();
        Ok(w)
    }

    pub fn is(&self, popup: &Popup) -> bool {
        self.surface.as_ref().is_some_and(|s| s.popup == *popup)
    }

    pub fn has_surface(&self, surface: &WlSurface) -> bool {
        self.surface
            .as_ref()
            .is_some_and(|s| s.popup.wl_surface() == surface)
    }

    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Faded out (or closed before it showed): it can go.
    pub fn gone(&self) -> bool {
        self.closing && (!self.shown || self.open.settled())
    }

    /// Fade out; the dock drops it after. The pointer goes through it
    /// meanwhile.
    pub fn close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        let mut fade = Spring::new(self.open.value, CLOSE_RESPONSE);
        fade.retarget(0.0);
        self.open = fade;
        if let Some(s) = &self.surface
            && let Ok(region) = Region::new(&self.g.compositor)
        {
            s.popup
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
    }

    pub fn moving(&self) -> bool {
        self.shown && !self.open.settled()
    }

    /// Advance the opening or closing by `dt` seconds. Returns whether
    /// it moves.
    pub fn advance(&mut self, dt: f64) -> bool {
        self.shown && self.open.advance(dt)
    }

    /// The plugin drew a new tree.
    pub fn set_tree(&mut self, tree: Arc<ui::Node>) {
        if self.tree != tree {
            self.tree = tree;
            self.paint();
        }
    }

    /// The remote image `src` arrived: paint again if the tree shows it.
    pub fn image_arrived(&mut self, src: &str) {
        if self.tree.shows_image(src) {
            self.paint();
        }
    }

    // -- the worker ----------------------------------------------------------

    /// The pointer state, at this time.
    fn state_now(&self) -> ui::State {
        ui::State {
            now: now(),
            ..self.state.clone()
        }
    }

    /// Paint the tree with the current state (after the paint in flight).
    fn paint(&mut self) {
        if self.closing {
            return;
        }
        if self.wanted.is_some() {
            self.again = true;
            return;
        }
        let request = ui::Request {
            // A standalone widget has no app of its own.
            app: crate::dock::widget_name(&self.app)
                .is_none()
                .then(|| self.app.clone()),
            state: self.state_now(),
            radius: self.style.metrics.radius as f32,
            apart: true,
            ..ui::Request::new(
                self.tree.clone(),
                ui::Bounds::Content {
                    max_w: MAX_W,
                    max_h: MAX_H,
                },
                self.scale,
                self.theme.clone(),
            )
        };
        let tag = next_tag();
        self.assets.ui(tag, request);
        self.wanted = Some(tag);
    }

    /// Paint live layer `i` with the current state (after its paint in
    /// flight).
    fn paint_live(&mut self, i: usize) {
        let mut state = self.state_now();
        let (Some(s), Some(c)) = (&mut self.surface, &self.content) else {
            return;
        };
        let Some(l) = s.lives.get_mut(i) else { return };
        if l.wanted.is_some() {
            l.again = true;
            return;
        }
        // Where an easing value or a sweeping bar is now.
        state.values = self.player.values([&l.part.node], state.now);
        let request = ui::Request {
            state,
            ..c.request.part(&l.part)
        };
        let tag = next_tag();
        self.assets.ui(tag, request);
        l.wanted = Some(tag);
    }

    /// A painted tree or live layer arrived; true if it is this popup's.
    pub fn ui_ready(&mut self, tag: u64, request: &ui::Request, r: &ui::Rendered) -> bool {
        if self.wanted == Some(tag) {
            self.wanted = None;
            self.content_ready(request, r);
            if std::mem::take(&mut self.again) {
                self.paint();
            }
            return true;
        }
        let Some(s) = &mut self.surface else {
            return false;
        };
        let Some(i) = s.lives.iter().position(|l| l.wanted == Some(tag)) else {
            return false;
        };
        let l = &mut s.lives[i];
        l.wanted = None;
        match cache::shm_buffer(&self.g, &r.image, &self.qh) {
            Ok(buffer) => {
                if let Some(old) = l.buffer.replace(buffer) {
                    self.stale.push(old);
                }
            }
            Err(e) => warn!("widget buffer: {e}"),
        }
        if std::mem::take(&mut l.again) {
            self.paint_live(i);
        }
        true
    }

    fn content_ready(&mut self, request: &ui::Request, r: &ui::Rendered) {
        let buffer = match cache::shm_buffer(&self.g, &r.image, &self.qh) {
            Ok(buffer) => buffer,
            Err(e) => return warn!("widget buffer: {e}"),
        };
        let painted = Painted {
            request: request.clone(),
            hits: r.hits.clone(),
            live: r.live.clone(),
            size: (
                r.width.ceil().max(1.0) as f64,
                r.height.ceil().max(1.0) as f64,
            ),
            buffer,
        };
        if let Some(old) = self.content.replace(painted) {
            self.stale.push(old.buffer);
        }
        // Keep what revealing scrolled to, unless the wheel moved the area
        // since; and which nodes asked, so they scroll only once.
        for (key, offset) in &r.scroll {
            let at = |s: &ui::State| s.scroll.iter().position(|(k, _)| k == key);
            let sent = at(&request.state).map(|i| request.state.scroll[i].1);
            match at(&self.state) {
                Some(i) if Some(self.state.scroll[i].1) == sent => self.state.scroll[i].1 = *offset,
                Some(_) => {}
                None => self.state.scroll.push((key.clone(), *offset)),
            }
        }
        self.state.revealed = r.revealed.clone();
        if self.closing {
            return;
        }
        match &self.surface {
            None => self.realize(),
            Some(_) => self.resize(),
        }
        self.sync_lives();
    }

    /// Give each live part of the tree a layer, and paint them all; and
    /// each animated canvas one, playing its frame sheet.
    fn sync_lives(&mut self) {
        let (canvases, parts): (Vec<ui::LivePart>, Vec<ui::LivePart>) =
            (self.content.as_ref().map_or(Vec::new(), |c| c.live.clone()))
                .into_iter()
                .partition(|p| matches!(p.node.kind, Kind::Canvas { anim: Some(_), .. }));
        let Some(s) = &mut self.surface else { return };
        let now = now();
        (self.player).sync(parts.iter().chain(&canvases).map(|p| &p.node), now);
        // Layers go on top, in tree order.
        while s.lives.len() > parts.len() {
            if let Some(mut l) = s.lives.pop() {
                l.sub.hide();
                self.stale.extend(l.buffer.take());
            }
        }
        for (i, part) in parts.into_iter().enumerate() {
            match s.lives.get_mut(i) {
                Some(l) => l.part = part,
                None => {
                    let sub = Sub::new(&self.g, s.popup.wl_surface(), &self.no_input, &self.qh);
                    s.lives.push(LiveLayer {
                        sub,
                        part,
                        wanted: None,
                        again: false,
                        buffer: None,
                    });
                }
            }
        }
        // The canvases' sheets: painted only if they paint differently.
        if let Some(c) = &self.content {
            let (paint, stale) = self.sheets.sync(canvases, &c.request);
            self.stale.extend(stale);
            for (i, request) in paint {
                let tag = next_tag();
                self.assets.sheet(tag, request);
                self.sheets.asked(i, tag);
            }
        }
        while s.sheet_subs.len() > self.sheets.layers.len() {
            if let Some(mut sub) = s.sheet_subs.pop() {
                sub.hide();
            }
        }
        while s.sheet_subs.len() < self.sheets.layers.len() {
            let sub = Sub::new(&self.g, s.popup.wl_surface(), &self.no_input, &self.qh);
            s.sheet_subs.push(sub);
        }
        for i in 0..s.lives.len() {
            self.paint_live(i);
        }
    }

    /// A frame sheet arrived; true if it is this popup's.
    pub fn sheet_ready(&mut self, tag: u64, sheet: &ui::Sheet) -> bool {
        if !self.sheets.wants(tag) {
            return false;
        }
        let buffers = (sheet.images.iter())
            .filter_map(|image| match cache::shm_buffer(&self.g, image, &self.qh) {
                Ok(buffer) => Some(buffer),
                Err(e) => {
                    warn!("widget frame buffer: {e}");
                    None
                }
            })
            .collect();
        let old = self.sheets.arrived(tag, buffers, sheet.step);
        self.stale.extend(old);
        true
    }

    /// How soon live layer `l` moves, if it does.
    fn live_due(&self, l: &LiveLayer, now: f64) -> Option<f64> {
        let dragged = (self.state.sliders.iter()).any(|(k, _)| Some(k.as_str()) == l.key());
        if dragged {
            return None;
        }
        let live = l.interval(self.scale);
        let anim = self.player.due(&l.part.node, now, LIVE_FASTEST);
        live.into_iter().chain(anim).reduce(f64::min)
    }

    /// Whether anything on it moves.
    fn moves(&self, now: f64) -> bool {
        let lives = (self.surface.iter()).flat_map(|s| &s.lives);
        (lives.into_iter()).any(|l| self.live_due(l, now).is_some())
            || self.sheets.due(&self.player, now, anim::POPUP_FPS).is_some()
    }

    /// If the live layers or animated canvases move and none is being
    /// painted, set the timer: when they are due again (and this popup's
    /// ID). Once per tick, and only once the last one is on screen.
    pub fn tick_due(&mut self) -> Option<(u64, Duration)> {
        if self.closing {
            return None;
        }
        let s = self.surface.as_ref()?;
        if s.lives.iter().any(|l| l.wanted.is_some()) {
            return None;
        }
        let now = now();
        let lives = s.lives.iter().filter_map(|l| self.live_due(l, now));
        let sheets = self.sheets.due(&self.player, now, anim::POPUP_FPS);
        let due = lives.chain(sheets).reduce(f64::min);
        let after = self.pacer.arm(due)?;
        Some((self.id, after))
    }

    /// The timer: paint the moving live layers at this time, and show
    /// the canvases' frames for it.
    pub fn tick(&mut self) {
        self.pacer.fired();
        let now = now();
        let n = self.surface.as_ref().map_or(0, |s| s.lives.len());
        for i in 0..n {
            let moves = (self.surface.as_ref())
                .and_then(|s| s.lives.get(i))
                .is_some_and(|l| self.live_due(l, now).is_some());
            if moves {
                self.paint_live(i);
            }
        }
        self.render();
    }

    /// The popup's frame callback: what it drew is on screen.
    pub fn frame_done(&mut self) {
        self.pacer.shown();
    }

    /// A bubble arrived; true if it is this popup's.
    pub fn bubble_ready(&mut self, tag: u64, bubble: &Bubble, image: &Image) -> bool {
        if self.bubble_wanted.as_ref().is_none_or(|(t, _)| *t != tag) {
            return false;
        }
        self.bubble_wanted = None;
        match cache::shm_buffer(&self.g, image, &self.qh) {
            Ok(buffer) => {
                if let Some((_, old)) = self.bubble.replace((bubble.clone(), buffer)) {
                    self.stale.push(old);
                }
            }
            Err(e) => warn!("widget bubble buffer: {e}"),
        }
        true
    }

    fn request_bubble(&mut self, bubble: Bubble) {
        let shown = self.bubble.as_ref().map(|(b, _)| b);
        let asked = self.bubble_wanted.as_ref().map(|(_, b)| b);
        if shown == Some(&bubble) || asked == Some(&bubble) {
            return;
        }
        let tag = next_tag();
        self.assets.bubble(tag, bubble.clone());
        self.bubble_wanted = Some((tag, bubble));
    }

    // -- the popup -----------------------------------------------------------

    /// The bubble's body for a tree `size` big: at least long enough
    /// along the dock for the tail between its corners.
    fn body(&self, size: (f64, f64)) -> (f64, f64) {
        let m = &self.style.metrics;
        let room = (2.0 * (m.radius + menu::CORNER_ROOM) + m.tail.0 + 2.0 * bubble::JOIN).ceil();
        match self.position {
            Position::Bottom => (size.0.max(room), size.1),
            Position::Left | Position::Right => (size.0, size.1.max(room)),
        }
    }

    /// The bubble for a `body`, with its tail at `tail` along its edge
    /// (None: the middle).
    fn bubble_for(&self, body: (f64, f64), tail: Option<f64>) -> Bubble {
        let m = &self.style.metrics;
        let (w, h) = body;
        let edge = match self.position {
            Position::Bottom => w,
            Position::Left | Position::Right => h,
        };
        let radius = m.radius.min(w.min(h) / 2.0);
        Bubble {
            w,
            h,
            radius,
            side: menu::side(self.position),
            tail_at: tail.unwrap_or_else(|| menu::tail_at(edge / 2.0, 0.0, edge, radius, m.tail.0)),
            tail_width: m.tail.0,
            tail_height: m.tail.1,
            fill: self.style.background,
            border: self.style.border,
            separators: Vec::new(),
            separator: 0,
            shadow: self.style.shadow,
            scale: self.scale,
        }
    }

    /// Where the popup goes for a tree `size` big.
    fn place_for(&self, size: (f64, f64)) -> Place {
        let body = self.body(size);
        Place {
            position: self.position,
            icon: self.icon,
            bounds: self.bubble_for(body, None).bounds(),
            body,
            tail_height: self.style.metrics.tail.1,
        }
    }

    fn positioner(&self, place: &Place) -> Option<XdgPositioner> {
        let p = popup::positioner(&self.g)?;
        let (w, h) = place.size();
        let (anchor, gravity, (dx, dy)) = place.positioner();
        p.set_size(w, h);
        let a = IRect::round(self.icon);
        p.set_anchor_rect(a.x, a.y, a.w.max(1), a.h.max(1));
        p.set_anchor(anchor);
        p.set_gravity(gravity);
        p.set_offset(dx, dy);
        // Along the dock only: across it, the tail would leave the icon.
        p.set_constraint_adjustment(match self.position {
            Position::Bottom => ConstraintAdjustment::SlideX,
            Position::Left | Position::Right => ConstraintAdjustment::SlideY,
        });
        Some(p)
    }

    /// The tree is painted: open the popup for it.
    fn realize(&mut self) {
        let Some(size) = self.content.as_ref().map(|c| c.size) else {
            return;
        };
        let place = self.place_for(size);
        let Some(positioner) = self.positioner(&place) else {
            return self.close();
        };
        let popup = match popup::layer_popup(&self.g, &self.layer, &positioner, None, &self.qh) {
            Ok(popup) => popup,
            Err(e) => {
                warn!("cannot show the widget's popup: {e}");
                return self.close();
            }
        };
        let parent = popup.wl_surface();
        let bubble = Sub::new(&self.g, parent, &self.no_input, &self.qh);
        let content = Sub::new(&self.g, parent, &self.no_input, &self.qh);
        let viewport = self.g.viewporter.get_viewport(parent, &self.qh, Ignore);
        self.surface = Some(Surface {
            lives: Vec::new(),
            sheet_subs: Vec::new(),
            content,
            bubble,
            viewport,
            popup,
            place,
            at: None,
        });
        self.request_bubble(self.bubble_for(place.body, None));
    }

    /// The tree was painted again: if its size changed, move the popup
    /// for it (or open it again, without xdg_popup v3).
    fn resize(&mut self) {
        let Some(size) = self.content.as_ref().map(|c| c.size) else {
            return;
        };
        let place = self.place_for(size);
        let Some(s) = &mut self.surface else { return };
        if s.place.body == place.body {
            return;
        }
        if s.popup.xdg_popup().version() < 3 {
            // A new popup, with new layers.
            if let Some(mut old) = self.surface.take() {
                for l in &mut old.lives {
                    self.stale.extend(l.buffer.take());
                }
            }
            return self.realize();
        }
        s.place = place;
        if let Some(p) = self.positioner(&place)
            && let Some(s) = &self.surface
        {
            s.popup.reposition(&p, 0);
        }
        // The configure that follows points the tail.
        self.request_bubble(self.bubble_for(place.body, None));
    }

    /// The compositor placed the popup: point the tail at the icon from
    /// there.
    pub fn configure(&mut self, configure: &PopupConfigure) {
        let Some(s) = &mut self.surface else { return };
        let (x, y) = configure.position;
        s.at = Some((x as f64, y as f64));
        let (w, h) = (configure.width.max(1), configure.height.max(1));
        let surface = s.popup.wl_surface();
        surface.attach(Some(&self.transparent), 0, 0);
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        s.viewport.set_destination(w, h);
        s.popup.xdg_surface().set_window_geometry(0, 0, w, h);
        let m = &self.style.metrics;
        let place = s.place;
        let (bw, bh) = place.body;
        let radius = m.radius.min(bw.min(bh) / 2.0);
        let tail = place.configured_tail((x as f64, y as f64), radius, m.tail.0);
        // Input on the bubble, not on its shadow.
        if !self.closing
            && let Ok(region) = Region::new(&self.g.compositor)
        {
            let (ox, oy) = place.body_origin();
            for r in input_rects(place.body, self.position, tail, radius, m) {
                let r = IRect::round(Rect::new(r.x + ox, r.y + oy, r.w, r.h));
                region.add(r.x, r.y, r.w, r.h);
            }
            surface.set_input_region(Some(region.wl_region()));
        }
        self.request_bubble(self.bubble_for(place.body, Some(tail)));
        self.render();
    }

    // -- input ---------------------------------------------------------------

    /// The tree's top left corner on the popup: centered in the body.
    fn content_origin(&self) -> Option<(f64, f64)> {
        let s = self.surface.as_ref()?;
        let c = self.content.as_ref()?;
        let (ox, oy) = s.place.body_origin();
        let (bw, bh) = s.place.body;
        Some((
            ox + ((bw - c.size.0) / 2.0).round(),
            oy + ((bh - c.size.1) / 2.0).round(),
        ))
    }

    /// A pointer event on the popup. Repaints what it changes; returns
    /// what to tell the plugin: a node's key and what happened to it.
    pub fn pointer(&mut self, e: &PointerEvent) -> Option<(String, PopupEvent)> {
        if self.closing {
            return None;
        }
        let (cx, cy) = self.content_origin()?;
        let (x, y) = ((e.position.0 - cx) as f32, (e.position.1 - cy) as f32);
        let before = self.state.clone();
        let action = self.input(&e.kind, x, y);
        if before != self.state {
            self.repaint(&before);
        }
        action
    }

    fn input(&mut self, kind: &PointerEventKind, x: f32, y: f32) -> Option<(String, PopupEvent)> {
        let c = self.content.as_ref()?;
        let hit = c.hits.iter().rev().find(|h| h.rect.contains(x, y));
        let state = &mut self.state;
        state.hovered = hit.filter(|h| h.enabled).map(|h| h.key.clone());
        match kind {
            PointerEventKind::Leave { .. } => {
                state.hovered = None;
                return None;
            }
            PointerEventKind::Axis { vertical, .. } => {
                let area =
                    (c.hits.iter().rev()).find(|h| h.kind == "scroll" && h.rect.contains(x, y));
                if let Some(h) = area {
                    let i = (state.scroll.iter())
                        .position(|(k, _)| k == &h.key)
                        .unwrap_or_else(|| {
                            state.scroll.push((h.key.clone(), 0.0));
                            state.scroll.len() - 1
                        });
                    let dy = if vertical.absolute != 0.0 {
                        vertical.absolute as f32
                    } else {
                        vertical.discrete as f32 * WHEEL_STEP
                    };
                    let max = (h.content - h.rect.h).max(0.0);
                    state.scroll[i].1 = (state.scroll[i].1 + dy).clamp(0.0, max);
                }
                return None;
            }
            PointerEventKind::Press {
                button: BTN_LEFT, ..
            } => {
                state.pressed = state.hovered.clone();
                self.last_change = None;
            }
            PointerEventKind::Release {
                button: BTN_LEFT, ..
            }
            | PointerEventKind::Enter { .. }
            | PointerEventKind::Motion { .. } => {}
            _ => return None,
        }
        let release = matches!(kind, PointerEventKind::Release { .. });
        let key = match release {
            true => state.pressed.take(),
            false => state.pressed.clone(),
        }?;
        let h = c.hits.iter().find(|h| h.key == key && h.enabled)?;
        let nodes = c.request.tree.walk();
        let n = nodes.into_iter().find(|n| n.key.as_ref() == Some(&key))?;
        let over = state.hovered.as_ref() == Some(&key);
        let event = match n.kind {
            Kind::Slider { min, max, .. } => {
                let f = ((x - h.rect.x) / h.rect.w.max(1.0)).clamp(0.0, 1.0) as f64;
                let v = min + (max - min) * f;
                state.sliders.retain(|(k, _)| *k != key);
                if release {
                    PopupEvent::Commit(v)
                } else {
                    state.sliders.push((key.clone(), v));
                    let now = Instant::now();
                    if self.last_change.is_some_and(|t| now - t < CHANGE_EVERY) {
                        return None;
                    }
                    self.last_change = Some(now);
                    PopupEvent::Change(v)
                }
            }
            Kind::Toggle { value, .. } if release && over => PopupEvent::Toggle(!value),
            _ if release && over => PopupEvent::Click,
            _ => return None,
        };
        Some((key, event))
    }

    /// The state changed from `before`: repaint the tree if that shows
    /// in it, else only the live layers it touches.
    fn repaint(&mut self, before: &ui::State) {
        let after = &self.state;
        let mut touched: Vec<Option<String>> = Vec::new();
        if before.hovered != after.hovered {
            touched.extend([before.hovered.clone(), after.hovered.clone()]);
        }
        if before.pressed != after.pressed {
            touched.extend([before.pressed.clone(), after.pressed.clone()]);
        }
        if before.sliders != after.sliders {
            let keys = before.sliders.iter().chain(&after.sliders);
            touched.extend(keys.map(|(k, _)| Some(k.clone())));
        }
        let lives: Vec<Option<String>> = (self.surface.iter())
            .flat_map(|s| &s.lives)
            .map(|l| l.key().map(str::to_owned))
            .collect();
        let whole = before.scroll != after.scroll
            || (touched.iter()).any(|k| k.is_some() && !lives.contains(k));
        if whole {
            // The live layers follow once it's in.
            return self.paint();
        }
        for (i, key) in lives.iter().enumerate() {
            if key.is_some() && touched.contains(key) {
                self.paint_live(i);
            }
        }
    }

    // -- drawing -------------------------------------------------------------

    fn gate(&self, a: f64) -> f64 {
        let a = a.clamp(0.0, 1.0);
        match self.g.alpha_modifier {
            Some(_) => a,
            None if a >= 0.5 => 1.0,
            None => 0.0,
        }
    }

    /// Draw it and commit, once it's configured and its bubble and tree
    /// are there. The first time, it starts opening.
    pub fn render(&mut self) {
        let configured = self.surface.as_ref().is_some_and(|s| s.at.is_some());
        let drawn = self.content.is_some()
            && self.bubble.is_some()
            && (self.shown || self.bubble_wanted.is_none());
        if !configured || !drawn {
            return;
        }
        if !self.shown && !self.closing {
            self.shown = true;
            self.open.retarget(1.0);
        }
        let alpha = self.gate(self.open.value);
        let scale = match self.closing {
            true => 1.0,
            false => lerp(OPEN_SCALE, 1.0, self.open.value.clamp(0.0, 1.0)),
        };
        let Some((cx, cy)) = self.content_origin() else {
            return;
        };
        let (Some(s), Some((bubble, bubble_buffer)), Some(c)) =
            (&mut self.surface, &self.bubble, &self.content)
        else {
            return;
        };
        let (ox, oy) = s.place.body_origin();
        let (tx, ty) = bubble.tip();
        // Everything grows out of the tail's tip.
        let (tx, ty) = (ox + tx, oy + ty);
        let xf = |r: Rect| r.scaled(scale, tx, ty);
        let (bw, bh) = s.place.size();
        let whole = Rect::new(0.0, 0.0, bw as f64, bh as f64);
        s.bubble.present(Some(bubble_buffer), xf(whole), alpha);
        let tree = Rect::new(cx, cy, c.size.0, c.size.1);
        s.content.present(Some(&c.buffer), xf(tree), alpha);
        let at = |r: ui::Rect| {
            Rect::new(
                (cx + r.x as f64).round(),
                (cy + r.y as f64).round(),
                (r.w as f64).round().max(1.0),
                (r.h as f64).round().max(1.0),
            )
        };
        for l in &mut s.lives {
            l.sub.present(l.buffer.as_ref(), xf(at(l.part.rect)), alpha);
        }
        // The canvases' frames for now.
        let now = now();
        for (i, sub) in s.sheet_subs.iter_mut().enumerate() {
            let Some(l) = self.sheets.layers.get(i) else {
                sub.clear();
                continue;
            };
            let buffer = self.sheets.shown(i, &self.player, now);
            sub.present(buffer, xf(at(l.part.rect)), alpha);
        }
        // While anything moves, the next tick waits for this frame to show.
        let surface = s.popup.wl_surface().clone();
        if !self.closing && !self.pacer.waiting() && self.moves(now) {
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            self.pacer.drew();
        }
        surface.commit();
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
    }
}

impl Drop for WidgetView {
    fn drop(&mut self) {
        // The popup and its subsurfaces go first.
        if let Some(mut s) = self.surface.take() {
            for l in &mut s.lives {
                self.stale.extend(l.buffer.take());
            }
        }
        self.stale.extend(self.sheets.drain());
        if let Some(c) = self.content.take() {
            c.buffer.destroy();
        }
        if let Some((_, buffer)) = self.bubble.take() {
            buffer.destroy();
        }
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
        self.transparent.destroy();
    }
}

/// The bubble's input region in body coordinates: the `body` without its
/// rounded corners' outsides, and the tail at `tail` along its edge.
fn input_rects(
    body: (f64, f64),
    position: Position,
    tail: f64,
    radius: f64,
    m: &Metrics,
) -> Vec<Rect> {
    let (w, h) = body;
    // Where a corner's arc is close to the rect's edge.
    let cut = (radius * 0.3).round();
    let (tw, th) = m.tail;
    let mut rects = vec![
        Rect::new(0.0, cut, w, h - 2.0 * cut),
        Rect::new(cut, 0.0, w - 2.0 * cut, h),
    ];
    if tw > 0.0 && th > 0.0 {
        rects.push(match position {
            Position::Bottom => Rect::new(tail - tw / 2.0, h, tw, th),
            Position::Left => Rect::new(-th, tail - tw / 2.0, th, tw),
            Position::Right => Rect::new(w, tail - tw / 2.0, th, tw),
        });
    }
    rects
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Metrics {
        Metrics {
            row_height: 28.0,
            padding: 6.0,
            group_gap: 8.0,
            max_width: 320.0,
            radius: 12.0,
            tail: (18.0, 9.0),
        }
    }

    #[test]
    fn input_covers_the_body_and_tail() {
        let rects = input_rects((200.0, 100.0), Position::Bottom, 60.0, 12.0, &metrics());
        let inside = |x, y| rects.iter().any(|r| r.contains(x, y));
        assert!(inside(100.0, 50.0));
        assert!(inside(60.0, 104.0), "the tail");
        assert!(!inside(0.5, 0.5), "a corner");
        assert!(!inside(150.0, 104.0), "beside the tail");
        let rects = input_rects((200.0, 100.0), Position::Left, 50.0, 12.0, &metrics());
        assert!(rects.iter().any(|r| r.contains(-4.0, 50.0)));
    }

    fn seek(w: f32, rate: f64) -> ui::LivePart {
        ui::LivePart {
            index: 0,
            rect: ui::Rect {
                x: 0.0,
                y: 0.0,
                w,
                h: 10.0,
            },
            alpha: 1.0,
            node: ui::Node::new(Kind::Slider {
                value: 0.0,
                min: 0.0,
                max: 200.0,
                enabled: true,
                live: Some(ui::Live { rate, at: 0.0 }),
                transition: 0.0,
            }),
        }
    }

    #[test]
    fn live_layers_repaint_as_often_as_they_move_a_pixel() {
        // A 200 s track over 200 px: once a second, twice at scale 2.
        assert_eq!(interval(&seek(200.0, 1.0), 1), Some(1.0));
        assert_eq!(interval(&seek(200.0, 1.0), 2), Some(0.5));
        // Never faster than 30 times a second, and paused is still.
        assert_eq!(interval(&seek(200.0, 1000.0), 1), Some(LIVE_FASTEST));
        assert_eq!(interval(&seek(200.0, 0.0), 1), None);
        // Not live: not a layer that moves.
        let mut still = seek(200.0, 1.0);
        still.node.kind = Kind::Progress {
            value: 0.0,
            max: 1.0,
            live: None,
            indeterminate: false,
            transition: 0.0,
        };
        assert_eq!(interval(&still, 1), None);
    }
}
