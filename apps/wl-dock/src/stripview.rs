//! The strip on screen: an xdg_popup of the dock surface, anchored to the
//! clicked icon, with a card per window (its preview and shadow, from
//! `wl_shell_ui::card`) and the window's title under it, in the grid of
//! strip.rs.
//!
//! The compositor places the popup next to the icon (above a bottom dock,
//! beside a side dock), centered on it, and slides or flips it to keep it
//! on the output. The popup's surface is as large as the grid plus room
//! for the cards' shadows; its input region is the grid only, so the
//! shadows never take clicks meant for the dock. Its window geometry is
//! set to the surface, since the cards reach past it while they grow out
//! of the icon.
//!
//! The popup grabs the pointer and keyboard: a click on another client
//! dismisses it, while clicks on the dock still reach the dock. The arrow
//! keys move a highlight between the cards, Enter activates, `1`–`9` pick
//! a card and Escape closes. The pointer moves the same highlight, and the
//! highlighted card zooms a little. It starts on the card the dock picks
//! (the app's previous window).
//!
//! Opening, the cards grow out of the icon (a morph on a spring), the ones
//! nearest the icon first. Closing reverses it, and the dock drops the
//! strip once it's back in the icon. A strip too large for the output
//! scrolls with the wheel, and is cut to its viewport.
//!
//! The dock owns the captures (they start before the strip opens) and
//! passes them in to draw; the strip only has their windows' IDs. Its
//! springs advance on the dock's frame callbacks, and every render
//! commits the popup.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::seat::pointer::{AxisScroll, BTN_LEFT, PointerEventKind};
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::xdg::{XdgPositioner, popup::Popup};
use wayland_client::{
    Proxy, QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use wl_shell_core::Shell;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::popup::{self, Anchor, ConstraintAdjustment, Gravity, PopupConfigure};
use wl_shell_log::warn;
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_render::{shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::card::{self, Card, Content, Preview};
use wl_shell_ui::geom::{self, IRect, Rect};
use wl_shell_ui::{Shape, Spring, Sub};

use crate::app::WlDock;
use crate::config::{Position, Previews};
use crate::dock::Window;
use crate::strip::{Key, Layout, Params};

type Qh = QueueHandle<Shell<WlDock>>;

/// Space between the icon and the strip.
const STRIP_GAP: f64 = 8.0;
/// Space between cards.
const CARD_GAP: f64 = 14.0;
/// Space between a card and its title.
const TITLE_GAP: f64 = 6.0;
/// Narrowest slot a card gets, for its title.
const MIN_SLOT: f64 = 120.0;
/// Room kept free between the strip and the edges of the output.
const EDGE_ROOM: f64 = 24.0;
/// Pixels of touchpad scroll per wheel notch.
const NOTCH: f64 = 40.0;

/// Tags of title pill requests, unique in the process.
fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the strip takes from the config.
#[derive(Clone, Debug, PartialEq)]
pub struct StripStyle {
    /// False: clicks never open the strip.
    pub enabled: bool,
    /// Apps with fewer windows activate one on a click instead.
    pub min_windows: usize,
    /// Cards per row along the dock at most; 0: no limit.
    pub max_columns: usize,
    /// Rows away from the dock at most; 0: no limit.
    pub max_rows: usize,
    /// Fraction of the output height.
    pub card_height: f64,
    /// Fraction of the output's length along the dock (and of its width,
    /// for a card).
    pub max_width: f64,
    pub previews: Previews,
    pub zoom: f64,
    pub title: text::Style,
    pub placeholder: u32,
    pub shadow: Option<shadow::Settings>,
    /// ms: the morph, and the zoom and scroll.
    pub morph_response: f64,
    pub response: f64,
}

impl StripStyle {
    /// How far the cards' shadows reach past them.
    fn pad(&self) -> f64 {
        self.shadow.map_or(0.0, |s| {
            let reach = (1.5 * s.softness.max(0.0)).ceil() + s.spread.max(0) as f64;
            reach + s.offset.0.abs().max(s.offset.1.abs()) as f64 + 1.0
        })
    }
}

/// Where the strip goes and how much room it has.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Place {
    /// The dock's edge.
    pub position: Position,
    /// The icon it points at, on the dock surface.
    pub icon: Rect,
    /// The output's logical size.
    pub output: (f64, f64),
    /// How far the icon's far side is from the dock's edge.
    pub reach: f64,
    pub scale: i32,
}

/// What a pointer event or key on the strip asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StripInput {
    None,
    /// Activate this window (and close).
    Activate(String),
    /// Close the strip.
    Close,
}

/// A window's title pill.
struct Title {
    tag: u64,
    request: PillRequest,
    /// The rendered pill and its logical size.
    pill: Option<(WlBuffer, f64, f64)>,
}

impl Drop for Title {
    fn drop(&mut self) {
        if let Some((buffer, ..)) = self.pill.take() {
            buffer.destroy();
        }
    }
}

pub struct StripView {
    // Field order is drop order: children before their parent surface.
    cards: Vec<Card>,
    title_subs: Vec<Sub>,
    viewport: WpViewport,
    popup: Popup,
    no_input: Region,
    transparent: WlBuffer,
    placeholder: WlBuffer,
    g: Rc<Globals>,
    assets: Rc<Assets>,
    qh: Qh,

    /// The app whose windows these are.
    pub app_id: String,
    windows: Vec<Window>,
    aspects: Vec<f64>,
    style: StripStyle,
    place: Place,
    layout: Layout,
    shadows: Vec<[Option<Shape>; 4]>,
    titles: Vec<Title>,
    /// Room around the strip for the shadows.
    pad: f64,
    /// The popup's surface size asked for.
    size: (i32, i32),
    /// The popup's position on the dock surface, once configured.
    at: Option<(f64, f64)>,

    /// 0: in the icon, 1: open.
    morph: Spring,
    closing: bool,
    zooms: Vec<Spring>,
    scroll: Spring,
    /// The card the keys act on, zoomed; the pointer moves it too.
    highlight: Option<usize>,
    /// The card a button went down on.
    pressed: Option<usize>,
}

impl StripView {
    /// Open a strip of `windows` (their aspect ratios `aspects`) of
    /// `app_id`, a popup of `layer` grabbing `grab` (a seat and the serial
    /// of the click), with card `highlight` highlighted.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        g: Rc<Globals>,
        assets: Rc<Assets>,
        qh: &Qh,
        layer: &LayerSurface,
        grab: Option<(&WlSeat, u32)>,
        app_id: String,
        windows: Vec<Window>,
        aspects: Vec<f64>,
        highlight: usize,
        style: StripStyle,
        place: Place,
    ) -> Result<Self, String> {
        let io = |e: std::io::Error| e.to_string();
        let transparent = cache::color_buffer(&g, 0, qh).map_err(io)?;
        let placeholder = cache::color_buffer(&g, style.placeholder, qh).map_err(io)?;
        let no_input = Region::new(&g.compositor).map_err(|e| e.to_string())?;
        let pad = style.pad();
        let layout = Layout::new(&params(&style, &place, &aspects));
        let size = surface_size(&layout, pad);
        let positioner = positioner(&g, size, &place, pad).ok_or("xdg_wm_base missing")?;
        let popup = popup::layer_popup(&g, layer, &positioner, grab, qh)?;
        let viewport = g.viewporter.get_viewport(popup.wl_surface(), qh, Ignore);
        let response = style.response;
        let mut strip = Self {
            cards: Vec::new(),
            title_subs: Vec::new(),
            viewport,
            popup,
            no_input,
            transparent,
            placeholder,
            g,
            assets,
            qh: qh.clone(),
            app_id,
            windows,
            aspects,
            morph: Spring::new(0.0, style.morph_response),
            style,
            place,
            layout,
            shadows: Vec::new(),
            titles: Vec::new(),
            pad,
            size,
            at: None,
            closing: false,
            zooms: Vec::new(),
            scroll: Spring::new(0.0, response),
            highlight: None,
            pressed: None,
        };
        strip.morph.retarget(1.0);
        strip.relaid();
        if highlight < strip.layout.len() {
            strip.set_highlight(Some(highlight));
            let s = strip.layout.reveal(highlight, 0.0);
            strip.scroll.snap(s);
        }
        Ok(strip)
    }

    pub fn surface(&self) -> &WlSurface {
        self.popup.wl_surface()
    }

    pub fn is(&self, popup: &Popup) -> bool {
        self.popup == *popup
    }

    /// The windows shown, in strip order.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    pub fn closing(&self) -> bool {
        self.closing
    }

    /// Closed and back in the icon: it can go.
    pub fn gone(&self) -> bool {
        self.closing && self.morph.settled()
    }

    /// Shrink back into the icon; the dock drops it after.
    pub fn close(&mut self) {
        self.closing = true;
        self.morph.retarget(0.0);
        self.pressed = None;
        self.set_highlight(None);
    }

    pub fn moving(&self) -> bool {
        !self.morph.settled() || !self.scroll.settled() || self.zooms.iter().any(|z| !z.settled())
    }

    /// Advance the springs by `dt` seconds. Returns whether any moves.
    pub fn advance(&mut self, dt: f64) -> bool {
        let mut moving = self.morph.advance(dt);
        moving |= self.scroll.advance(dt);
        for z in &mut self.zooms {
            moving |= z.advance(dt);
        }
        moving
    }

    /// Show `windows` with `aspects`, `style` and `place` (lays out again
    /// if anything changed). The highlight stays on its window.
    pub fn update(
        &mut self,
        windows: &[Window],
        aspects: &[f64],
        style: &StripStyle,
        place: Place,
    ) {
        let relayout = self.aspects != aspects
            || self.windows.len() != windows.len()
            || self.style != *style
            || self.place != place;
        let retitle = self.windows != windows;
        let moved = self.place.icon != place.icon;
        let highlighted = (self.highlight)
            .and_then(|i| self.windows.get(i))
            .map(|w| w.id.clone());
        self.windows = windows.to_vec();
        self.aspects = aspects.to_vec();
        if self.style != *style {
            self.style = style.clone();
            self.pad = style.pad();
        }
        self.place = place;
        if relayout {
            self.layout = Layout::new(&params(&self.style, &self.place, &self.aspects));
            let size = surface_size(&self.layout, self.pad);
            if size != self.size || moved {
                self.size = size;
                self.reposition();
            }
            self.relaid();
        } else if retitle {
            self.request_titles();
        }
        if !self.closing
            && let Some(id) = highlighted
        {
            let i = self.windows.iter().position(|w| w.id == id);
            let i = i.or(self
                .highlight
                .map(|h| h.min(self.windows.len().saturating_sub(1))));
            self.set_highlight(i.filter(|&i| i < self.layout.len()));
        }
    }

    /// The cache was cleared: get the shapes and titles again.
    pub fn reload(&mut self) {
        self.shadows.clear();
        self.titles.clear();
        self.request_titles();
    }

    /// Ask the compositor to place the popup again, for a new size. Older
    /// compositors (xdg_popup v2) keep the first size.
    fn reposition(&self) {
        if self.popup.xdg_popup().version() < 3 {
            return;
        }
        if let Some(p) = positioner(&self.g, self.size, &self.place, self.pad) {
            self.popup.reposition(&p, 0);
        }
    }

    /// The layout changed: subsurfaces, shadows, springs and titles to
    /// match it.
    fn relaid(&mut self) {
        let n = self.layout.len();
        if self.cards.len() < n {
            let more = card::cards(
                &self.g,
                self.popup.wl_surface(),
                &self.no_input,
                n - self.cards.len(),
                &self.qh,
            );
            // Below the titles.
            if let Some(first) = self.title_subs.first() {
                for c in &more {
                    for s in c.shadow.iter().chain([&c.preview]) {
                        s.place_below(first.surface());
                    }
                }
            }
            self.cards.extend(more);
        }
        while self.title_subs.len() < n {
            let sub = Sub::new(&self.g, self.popup.wl_surface(), &self.no_input, &self.qh);
            self.title_subs.push(sub);
        }
        self.zooms.resize(n, Spring::new(0.0, self.style.response));
        if self.highlight.is_some_and(|h| h >= n) {
            self.highlight = None;
        }
        let max = self.layout.max_scroll();
        if self.scroll.target > max {
            self.scroll.retarget(max);
        }
        self.shadows.clear();
        self.request_titles();
    }

    /// The cards' shadows, from `cache`.
    pub fn fetch_shadows(&mut self, cache: &mut Cache) {
        let Some(shadow) = self.style.shadow else {
            self.shadows.clear();
            return;
        };
        if self.shadows.len() == self.layout.len() {
            return;
        }
        self.shadows = (0..self.layout.len())
            .map(|i| {
                let c = self.layout.card(i, 0.0);
                let size = (c.w.round() as i32, c.h.round() as i32);
                cache.card_shadow(&self.g, size, shadow, self.place.scale, &self.qh)
            })
            .collect();
    }

    /// The title pills to show: ask the worker for new ones. A title keeps
    /// showing its old pill until the new one arrives.
    fn request_titles(&mut self) {
        self.titles.truncate(self.windows.len());
        let mut wanted = Vec::new();
        for (i, w) in self.windows.iter().enumerate() {
            let slot = match i < self.layout.len() {
                true => self.layout.card(i, 0.0).w.max(MIN_SLOT),
                false => MIN_SLOT,
            };
            wanted.push(PillRequest {
                text: w.title.clone(),
                spans: Vec::new(),
                max_width: slot.round() as i32,
                scale: self.place.scale,
                style: self.style.title.clone(),
            });
        }
        for (i, request) in wanted.into_iter().enumerate() {
            match self.titles.get_mut(i) {
                Some(t) if t.request == request => {}
                Some(t) => {
                    t.tag = next_tag();
                    t.request = request;
                    self.assets.pill(t.tag, t.request.clone());
                }
                None => {
                    let tag = next_tag();
                    self.assets.pill(tag, request.clone());
                    self.titles.push(Title {
                        tag,
                        request,
                        pill: None,
                    });
                }
            }
        }
    }

    /// A title pill arrived; true if it is one of this strip's.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let Some(t) = self.titles.iter_mut().find(|t| t.tag == tag) else {
            return false;
        };
        if t.request != *request {
            return false;
        }
        let buffer = match cache::shm_buffer(&self.g, &pill.image, &self.qh) {
            Ok(b) => b,
            Err(e) => {
                warn!("title buffer: {e}");
                return false;
            }
        };
        if let Some((old, ..)) = t
            .pill
            .replace((buffer, pill.width as f64, pill.height as f64))
        {
            old.destroy();
        }
        true
    }

    /// The compositor placed the popup.
    pub fn configure(&mut self, configure: &PopupConfigure) {
        let (x, y) = configure.position;
        self.at = Some((x as f64, y as f64));
        let (w, h) = (configure.width.max(1), configure.height.max(1));
        let surface = self.popup.wl_surface();
        surface.attach(Some(&self.transparent), 0, 0);
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.viewport.set_destination(w, h);
        self.popup.xdg_surface().set_window_geometry(0, 0, w, h);
        // Input on the strip, not on the shadows around it.
        if let Ok(region) = Region::new(&self.g.compositor) {
            let (sw, sh) = self.layout.size();
            let p = self.pad.round() as i32;
            region.add(p, p, sw.round() as i32, sh.round() as i32);
            surface.set_input_region(Some(region.wl_region()));
        }
    }

    /// The icon on the strip's content (0, 0 being the strip's corner).
    fn icon(&self) -> Option<Rect> {
        let (x, y) = self.at?;
        let i = self.place.icon;
        Some(Rect::new(i.x - x - self.pad, i.y - y - self.pad, i.w, i.h))
    }

    /// Each card's rank by distance from the icon: the nearest comes out
    /// first.
    fn ranks(&self, icon: Rect) -> Vec<usize> {
        let (ix, iy) = icon.center();
        let d = |i: usize| {
            let (x, y) = self.layout.card(i, self.scroll.target).center();
            (x - ix).powi(2) + (y - iy).powi(2)
        };
        let mut order: Vec<usize> = (0..self.layout.len()).collect();
        order.sort_by(|&a, &b| d(a).total_cmp(&d(b)));
        let mut ranks = vec![0; order.len()];
        for (rank, i) in order.into_iter().enumerate() {
            ranks[i] = rank;
        }
        ranks
    }

    fn gate(&self, a: f64) -> f64 {
        let a = a.clamp(0.0, 1.0);
        match self.g.alpha_modifier {
            Some(_) => a,
            None if a >= 0.5 => 1.0,
            None => 0.0,
        }
    }

    /// Draw it and commit. `previews` are the windows' captures, `icon`
    /// the app's icon (its buffer, size and the part without its shadow)
    /// for cards without previews.
    pub fn render(&mut self, previews: &mut [Preview], icon: Option<(&WlBuffer, f64, [f64; 4])>) {
        let Some(icon_rect) = self.icon() else {
            return; // not configured yet
        };
        let morph = self.morph.value.clamp(0.0, 1.0);
        let scroll = self.scroll.value;
        let ranks = self.ranks(icon_rect);
        let pad = self.pad;
        let shift = |r: Rect| Rect::new(r.x + pad, r.y + pad, r.w, r.h);
        let (vw, vh) = self.layout.size();
        let viewport = Rect::new(0.0, 0.0, vw, vh);
        // A scrolling strip is cut to its viewport, once the cards are out
        // of the icon.
        let clip = self.layout.scrolls().then(|| {
            let x0 = viewport.x.min(icon_rect.x);
            let y0 = viewport.y.min(icon_rect.y);
            let x1 = viewport.right().max(icon_rect.right());
            let y1 = viewport.bottom().max(icon_rect.bottom());
            shift(Rect::new(x0, y0, x1 - x0, y1 - y0).lerp(viewport, morph))
        });
        let scale = self.place.scale as f64;
        let off = self.style.previews == Previews::Off;
        for i in 0..self.cards.len() {
            if i >= self.layout.len() {
                self.cards[i].clear();
                self.title_subs[i].clear();
                continue;
            }
            let local = card::stagger(morph, ranks.get(i).copied().unwrap_or(i));
            let zoom = self.zooms.get(i).map_or(0.0, |z| z.value);
            let target = self.layout.zoomed(i, scroll, zoom);
            let rect = icon_rect.lerp(target, local);
            // Cards pop in quickly out of the icon, which they cover.
            let alpha = self.gate((local * 4.0).min(1.0));
            let content = match off {
                true => icon.map(|(buffer, size, src)| Content::Image {
                    buffer,
                    size: (size, size),
                    src: Some(src),
                }),
                false => {
                    let id = self.windows.get(i).map(|w| w.id.as_str());
                    Some(Content::Preview {
                        preview: previews.iter_mut().find(|p| Some(p.window.as_str()) == id),
                        placeholder: &self.placeholder,
                    })
                }
            };
            let rest = self.layout.card(i, 0.0);
            let rendered = (rest.w.round(), rest.h.round());
            let shadow = self.shadows.get(i);
            (self.cards[i]).place(content, shift(rect), shadow, rendered, scale, alpha, clip);

            // The title, under the card once it's out.
            let title_alpha = self.gate(((local - 0.6) / 0.4).clamp(0.0, 1.0));
            let sub = &mut self.title_subs[i];
            match self.titles.get(i).and_then(|t| t.pill.as_ref()) {
                Some((buffer, w, h)) if title_alpha > 0.0 => {
                    let (cx, _) = rect.center();
                    let r = shift(Rect::new(cx - w / 2.0, rect.bottom() + TITLE_GAP, *w, *h));
                    let size = (w * scale, h * scale);
                    let part = match clip {
                        Some(c) => geom::clip(r, size, None, c),
                        None => Some((r, None)),
                    };
                    match part {
                        Some((r, src)) => {
                            sub.crop(src);
                            sub.show(buffer);
                            sub.place(IRect::round(r));
                            sub.set_alpha(title_alpha);
                        }
                        None => sub.hide(),
                    }
                    sub.commit();
                }
                _ => sub.clear(),
            }
        }
        self.popup.wl_surface().commit();
    }

    /// A pointer event on the strip's surface, at (`x`, `y`) on it.
    pub fn pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) -> StripInput {
        if self.closing {
            return StripInput::None;
        }
        let (cx, cy) = (x - self.pad, y - self.pad);
        let hit = self.layout.hit(cx, cy, self.scroll.value);
        match kind {
            // The highlight stays where it was when the pointer leaves the
            // cards, so the keys go on from there.
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if hit.is_some() {
                    self.set_highlight(hit);
                }
            }
            PointerEventKind::Leave { .. } => self.pressed = None,
            PointerEventKind::Press { button, .. } if *button == BTN_LEFT => self.pressed = hit,
            PointerEventKind::Release { button, .. } if *button == BTN_LEFT => {
                if let Some(i) = hit
                    && self.pressed.take() == Some(i)
                    && let Some(w) = self.windows.get(i)
                {
                    return StripInput::Activate(w.id.clone());
                }
            }
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => {
                // Wheels by the notch, touchpads by the pixel.
                let step = |a: &AxisScroll| match (a.value120, a.discrete) {
                    (0, 0) => a.absolute,
                    (0, d) => d as f64 * NOTCH,
                    (v, _) => v as f64 / 120.0 * NOTCH,
                };
                let delta = self.layout.scroll_delta(step(horizontal), step(vertical));
                let max = self.layout.max_scroll();
                self.scroll
                    .retarget((self.scroll.target + delta).clamp(0.0, max));
                let hit = self.layout.hit(cx, cy, self.scroll.target);
                if hit.is_some() {
                    self.set_highlight(hit);
                }
            }
            _ => {}
        }
        StripInput::None
    }

    /// A key on the strip.
    pub fn key(&mut self, key: Key) -> StripInput {
        if self.closing {
            return StripInput::None;
        }
        let activate = |i: Option<usize>| {
            (i.and_then(|i| self.windows.get(i)))
                .map_or(StripInput::None, |w| StripInput::Activate(w.id.clone()))
        };
        match key {
            Key::Close => StripInput::Close,
            Key::Activate => activate(self.highlight),
            Key::Pick(i) => activate(Some(i).filter(|&i| i < self.layout.len())),
            Key::Move(arrow) => {
                let next = match self.highlight {
                    Some(i) => self.layout.neighbor(i, arrow),
                    None => (self.layout.len() > 0).then_some(0),
                };
                if let Some(i) = next {
                    self.set_highlight(Some(i));
                    let s = self.layout.reveal(i, self.scroll.target);
                    self.scroll.retarget(s);
                }
                StripInput::None
            }
        }
    }

    /// The highlighted card's window.
    pub fn highlighted(&self) -> Option<&str> {
        let i = self.highlight?;
        self.windows.get(i).map(|w| w.id.as_str())
    }

    /// Highlight the card of window `id` (scrolled into view).
    pub fn highlight_window(&mut self, id: &str) {
        if self.closing {
            return;
        }
        let Some(i) = self.windows.iter().position(|w| w.id == id) else {
            return;
        };
        if i < self.layout.len() {
            self.set_highlight(Some(i));
            let s = self.layout.reveal(i, self.scroll.target);
            self.scroll.retarget(s);
        }
    }

    fn set_highlight(&mut self, highlight: Option<usize>) {
        if highlight == self.highlight {
            return;
        }
        self.highlight = highlight;
        for (i, z) in self.zooms.iter_mut().enumerate() {
            z.retarget(if Some(i) == highlight { 1.0 } else { 0.0 });
        }
    }
}

impl Drop for StripView {
    fn drop(&mut self) {
        self.viewport.destroy();
        self.transparent.destroy();
        self.placeholder.destroy();
    }
}

/// The layout for `aspects` at `place`.
fn params(style: &StripStyle, place: &Place, aspects: &[f64]) -> Params {
    let (ow, oh) = place.output;
    let (along, across) = match place.position {
        Position::Bottom => (ow, oh),
        Position::Left | Position::Right => (oh, ow),
    };
    // Away from the dock: from the icon's far side to the output's edge.
    let max_across = across - place.reach - STRIP_GAP - EDGE_ROOM;
    let max_along = (style.max_width * along).min(along - 2.0 * EDGE_ROOM);
    let title = text::pill_height(style.title.size) as f64 + TITLE_GAP;
    let aspects = match style.previews {
        Previews::Off => vec![1.0; aspects.len()],
        _ => aspects.to_vec(),
    };
    Params {
        aspects,
        position: place.position,
        max_columns: style.max_columns,
        max_rows: style.max_rows,
        card_height: (style.card_height * oh).max(1.0),
        max_card_width: (style.max_width * ow).max(MIN_SLOT),
        max_along: max_along.max(MIN_SLOT),
        max_across: max_across.max(1.0),
        min_slot: MIN_SLOT,
        gap: CARD_GAP,
        title,
        zoom: style.zoom,
    }
}

/// The popup's surface size: the strip and its shadows' room.
fn surface_size(layout: &Layout, pad: f64) -> (i32, i32) {
    let (w, h) = layout.size();
    (
        ((w + 2.0 * pad).ceil() as i32).max(1),
        ((h + 2.0 * pad).ceil() as i32).max(1),
    )
}

/// A positioner for a popup `size` large next to the icon at `place`,
/// centered on it.
fn positioner(g: &Globals, size: (i32, i32), place: &Place, pad: f64) -> Option<XdgPositioner> {
    let p = popup::positioner(g)?;
    p.set_size(size.0.max(1), size.1.max(1));
    let a = IRect::round(place.icon);
    p.set_anchor_rect(a.x, a.y, a.w.max(1), a.h.max(1));
    // The strip's content STRIP_GAP from the icon; its surface reaches
    // `pad` further.
    let gap = (STRIP_GAP - pad).round() as i32;
    let (anchor, gravity, offset) = match place.position {
        Position::Bottom => (Anchor::Top, Gravity::Top, (0, -gap)),
        Position::Left => (Anchor::Right, Gravity::Right, (gap, 0)),
        Position::Right => (Anchor::Left, Gravity::Left, (-gap, 0)),
    };
    p.set_anchor(anchor);
    p.set_gravity(gravity);
    p.set_offset(offset.0, offset.1);
    p.set_constraint_adjustment(
        ConstraintAdjustment::SlideX
            | ConstraintAdjustment::SlideY
            | ConstraintAdjustment::FlipX
            | ConstraintAdjustment::FlipY,
    );
    if p.version() >= 3 {
        p.set_reactive();
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style() -> StripStyle {
        StripStyle {
            enabled: true,
            min_windows: 2,
            max_columns: 3,
            max_rows: 3,
            card_height: 0.2,
            max_width: 0.5,
            previews: Previews::Live,
            zoom: 1.05,
            title: text::Style {
                font: "sans".into(),
                size: 13.0,
                color: 0,
                background: 0,
            },
            placeholder: 0,
            shadow: Some(shadow::Settings {
                color: 0x00000080,
                softness: 10.0,
                spread: 0,
                offset: (0, 4),
            }),
            morph_response: 250.0,
            response: 150.0,
        }
    }

    fn place(position: Position) -> Place {
        Place {
            position,
            icon: Rect::new(500.0, 20.0, 48.0, 48.0),
            output: (1920.0, 1080.0),
            reach: 100.0,
            scale: 1,
        }
    }

    #[test]
    fn strips_fit_the_output() {
        for position in [Position::Bottom, Position::Left, Position::Right] {
            for (columns, rows) in [(3, 3), (1, 0), (0, 1), (0, 0)] {
                let s = StripStyle {
                    max_columns: columns,
                    max_rows: rows,
                    ..style()
                };
                let p = place(position);
                let l = Layout::new(&params(&s, &p, &[1.6; 12]));
                let (w, h) = l.size();
                let (away, along) = match position {
                    Position::Bottom => (h, w),
                    _ => (w, h),
                };
                let (aw, al) = match position {
                    Position::Bottom => (1080.0 - 100.0, 1920.0),
                    _ => (1920.0 - 100.0, 1080.0),
                };
                let at = format!("{position:?} {columns}x{rows}");
                assert!(away <= aw - STRIP_GAP - EDGE_ROOM + 1e-6, "{at}");
                assert!(along <= al - 2.0 * EDGE_ROOM + 1e-6, "{at}");
            }
        }
    }

    #[test]
    fn four_windows_stay_close_to_the_icon() {
        // The exit criterion: on bottom and side docks, every card of an
        // app with four windows is a short move from the icon.
        for position in [Position::Bottom, Position::Left, Position::Right] {
            let l = Layout::new(&params(&style(), &place(position), &[1.6; 4]));
            assert_eq!((l.columns, l.rows), (3, 2), "{position:?}");
            let (w, h) = l.size();
            let away = match position {
                Position::Bottom => h,
                _ => w,
            };
            // Two rows of cards a fifth of the output tall, at most.
            assert!(
                away <= 2.0 * (0.2 * 1080.0 * 1.05 + 40.0) + CARD_GAP,
                "{position:?}"
            );
        }
    }

    #[test]
    fn shadows_get_room() {
        let s = style();
        // 15 px of blur, 4 px of offset, and a pixel to spare.
        assert_eq!(s.pad(), 20.0);
        assert_eq!(StripStyle { shadow: None, ..s }.pad(), 0.0);
        let l = Layout::new(&params(&style(), &place(Position::Bottom), &[1.0, 1.0]));
        let (w, h) = surface_size(&l, 20.0);
        assert_eq!(w, (l.size().0 + 40.0).ceil() as i32);
        assert_eq!(h, (l.size().1 + 40.0).ceil() as i32);
    }
}
