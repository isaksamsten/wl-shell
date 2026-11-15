//! The menu on screen: an xdg_popup of the dock surface, a speech bubble
//! (menu.rs, rendered by the worker) pointing at the icon it was opened
//! from, with a row per entry.
//!
//! Opening takes a few steps, all without blocking: the rows' texts are
//! rendered first (the bubble is as wide as the widest), then the popup is
//! created next to the icon and the bubble rendered for where the
//! compositor put it. The tail points at the icon's center even when the
//! compositor slid the popup to keep it on the output: a configure that
//! moves it gets a bubble with the tail moved. The menu shows once the
//! popup is configured and its bubble is there.
//!
//! The popup's surface is the bubble's image, shadow included; its input
//! region is the bubble only. On it, as subsurfaces: the bubble, the
//! highlight (a rounded rect that slides between rows on a fast spring),
//! and per row its text (in the highlight's text color on the highlighted
//! row) and its icon or the focused window's dot.
//!
//! The popup grabs the pointer and keyboard, so a click elsewhere
//! dismisses it. The pointer and Up/Down move the highlight, a click or
//! Enter picks a row, Escape closes. Opening, the bubble grows out of its
//! tail's tip (scaled from 0.9 and faded in); picking flashes the row's
//! highlight, then the menu fades out quickly, and the dock drops it once
//! it's gone. A row that asks first (Empty Trash) only shows its confirm
//! text when picked, and runs when picked again; its texts are measured
//! together, so the bubble doesn't change size. Springs advance on the
//! dock's frame callbacks.
//!
//! A folder's popup is a menu too, with file rows: a press on one that
//! moves `drag-threshold` drags the file out (`MenuInput::Drag`). The
//! menu then fades but is held (`hold`) until the drag ends, since the
//! drag started on its surface.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::seat::pointer::PointerEventKind;
use smithay_client_toolkit::shell::wlr_layer::LayerSurface;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_seat::WlSeat, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use wl_shell_core::Shell;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::popup::{self, ConstraintAdjustment, PopupConfigure};
use wl_shell_log::warn;
use wl_shell_render::bubble::Bubble;
use wl_shell_render::icons::NamedIcon;
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_render::{Image, shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::shapes::ShapeKey;
use wl_shell_ui::{IRect, Rect, Spring, Sub, lerp};

use crate::app::WlDock;
use crate::config::Position;
use crate::menu::{self, Action, Entry, Group, Layout, Mark, MenuKey, Metrics, Place};

type Qh = QueueHandle<Shell<WlDock>>;

/// Opening: the scale and fade, ms.
const OPEN_RESPONSE: f64 = 120.0;
/// Closing: the fade, ms.
const CLOSE_RESPONSE: f64 = 80.0;
/// The highlight sliding between rows, ms.
const HIGHLIGHT_RESPONSE: f64 = 70.0;
/// The picked row's flash, s.
const FLASH: f64 = 0.14;
/// The bubble grows from this scale.
const OPEN_SCALE: f64 = 0.9;
/// The focused window's dot.
const DOT: i32 = 6;

/// Tags of text and bubble requests, unique in the process.
fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 48);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the menu takes from the config.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuStyle {
    pub metrics: Metrics,
    /// The rows' text.
    pub text: text::Style,
    pub highlight_text: u32,
    pub background: u32,
    pub border: u32,
    pub separator: u32,
    pub highlight: u32,
    pub shadow: Option<shadow::Settings>,
    pub icon_theme: Option<String>,
    /// How far a press on a file's row moves before it drags the file.
    pub drag_threshold: f64,
}

/// What a pointer event or key on the menu asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuInput {
    None,
    /// Run this (the menu is closing).
    Pick(Action),
    Close,
    /// The file of row `row` is dragged out, by the press with `serial`.
    Drag {
        row: usize,
        serial: u32,
    },
}

/// A rendered line of text and its logical size.
struct Text {
    tag: u64,
    request: PillRequest,
    pill: Option<(WlBuffer, f64, f64)>,
}

impl Text {
    fn new(assets: &Assets, request: PillRequest) -> Self {
        let tag = next_tag();
        assets.line(tag, request.clone());
        Self {
            tag,
            request,
            pill: None,
        }
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        if let Some((buffer, ..)) = self.pill.take() {
            buffer.destroy();
        }
    }
}

/// The popup and its subsurfaces, once the rows are measured.
struct Surface {
    // Field order is drop order: children before their parent.
    bubble: Sub,
    highlight: Sub,
    texts: Vec<Sub>,
    marks: Vec<Sub>,
    viewport: WpViewport,
    popup: Popup,
    layout: Layout,
    place: Place,
    /// The popup's position on the dock surface, once configured.
    at: Option<(f64, f64)>,
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.viewport.destroy();
    }
}

pub struct MenuView {
    surface: Option<Surface>,
    no_input: Region,
    transparent: WlBuffer,
    /// The bubble shown, and the one asked for.
    bubble: Option<(Bubble, WlBuffer)>,
    wanted: Option<(u64, Bubble)>,
    /// Replaced buffers, destroyed once the popup no longer shows them.
    stale: Vec<WlBuffer>,
    g: Rc<Globals>,
    assets: Rc<Assets>,
    qh: Qh,
    layer: LayerSurface,
    grab: Option<(WlSeat, u32)>,

    /// The app whose menu this is.
    pub app_id: String,
    entries: Vec<Entry>,
    groups: Vec<Group>,
    style: MenuStyle,
    position: Position,
    /// The icon on the dock surface.
    icon: Rect,
    scale: i32,
    texts: Vec<Text>,
    /// In the highlight's text color; None if it's the same.
    highlighted: Vec<Option<Text>>,
    /// The confirm texts of rows that ask first, and highlighted.
    confirms: Vec<Option<(Text, Option<Text>)>>,
    /// The row that asked and waits for a second pick.
    armed: Option<usize>,

    /// 0: hidden, 1: open.
    open: Spring,
    shown: bool,
    closing: bool,
    highlight: Option<usize>,
    highlight_y: Spring,
    highlight_alpha: Spring,
    /// Seconds since a row was picked.
    flash: Option<f64>,
    /// A button went down on the menu: on this row (if any), at this
    /// point, with this serial.
    pressed: Option<Press>,
    /// A row was dragged out: the menu stays (faded) until the drag ends,
    /// since the drag started on its surface.
    held: bool,
}

/// A press on the menu.
#[derive(Clone, Copy, Debug)]
struct Press {
    row: Option<usize>,
    at: (f64, f64),
    serial: u32,
}

impl MenuView {
    /// A menu of `groups` for `app_id`'s icon at `icon` on the dock
    /// `layer` at `position`, grabbing `grab` (a seat and the serial of the
    /// click). It shows once its rows are rendered.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        g: Rc<Globals>,
        assets: Rc<Assets>,
        cache: &Cache,
        qh: &Qh,
        layer: &LayerSurface,
        grab: Option<(&WlSeat, u32)>,
        app_id: String,
        groups: Vec<Group>,
        style: MenuStyle,
        position: Position,
        icon: Rect,
        scale: i32,
    ) -> Result<Self, String> {
        let io = |e: std::io::Error| e.to_string();
        let transparent = cache::color_buffer(&g, 0, qh).map_err(io)?;
        let no_input = Region::new(&g.compositor).map_err(|e| e.to_string())?;
        let entries: Vec<Entry> = menu::entries(&groups).into_iter().cloned().collect();
        let column = menu::column(&entries);
        let max_width = style.metrics.text_max(column).floor() as i32;
        let request = |color: u32, label: &str| PillRequest {
            text: label.to_string(),
            max_width,
            scale,
            style: text::Style {
                color,
                ..style.text.clone()
            },
        };
        let texts = (entries.iter())
            .map(|e| Text::new(&assets, request(style.text.color, &e.label)))
            .collect();
        let highlighted = (entries.iter())
            .map(|e| {
                (style.highlight_text != style.text.color)
                    .then(|| Text::new(&assets, request(style.highlight_text, &e.label)))
            })
            .collect();
        let confirms = (entries.iter())
            .map(|e| {
                let label = e.confirm.as_ref()?;
                let lit = (style.highlight_text != style.text.color)
                    .then(|| Text::new(&assets, request(style.highlight_text, label)));
                Some((Text::new(&assets, request(style.text.color, label)), lit))
            })
            .collect();
        for e in &entries {
            if let Some((key, _)) = mark_key(&e.mark, scale, &style)
                && cache.named_icon(&key).is_none()
            {
                assets.named_icon(&key);
            }
        }
        Ok(Self {
            surface: None,
            no_input,
            transparent,
            bubble: None,
            wanted: None,
            stale: Vec::new(),
            g,
            assets,
            qh: qh.clone(),
            layer: layer.clone(),
            grab: grab.map(|(seat, serial)| (seat.clone(), serial)),
            app_id,
            entries,
            groups,
            style,
            position,
            icon,
            scale: scale.max(1),
            texts,
            highlighted,
            confirms,
            armed: None,
            open: Spring::new(0.0, OPEN_RESPONSE),
            shown: false,
            closing: false,
            highlight: None,
            highlight_y: Spring::new(0.0, HIGHLIGHT_RESPONSE),
            highlight_alpha: Spring::new(0.0, HIGHLIGHT_RESPONSE),
            flash: None,
            pressed: None,
            held: false,
        })
    }

    /// The popup's surface, once there is one.
    pub fn surface(&self) -> Option<&WlSurface> {
        self.surface.as_ref().map(|s| s.popup.wl_surface())
    }

    pub fn is(&self, popup: &Popup) -> bool {
        self.surface.as_ref().is_some_and(|s| s.popup == *popup)
    }

    /// Closing, or picked and about to.
    pub fn closing(&self) -> bool {
        self.closing || self.flash.is_some()
    }

    /// Faded out (or closed before it showed), and not held by a drag
    /// from it: it can go.
    pub fn gone(&self) -> bool {
        self.closing && !self.held && self.flash.is_none() && (!self.shown || self.open.settled())
    }

    /// Fade out; the dock drops it after.
    pub fn close(&mut self) {
        if self.closing {
            return;
        }
        self.closing = true;
        self.pressed = None;
        // Faster out than in.
        let mut fade = Spring::new(self.open.value, CLOSE_RESPONSE);
        fade.retarget(0.0);
        self.open = fade;
    }

    /// A row was dragged out: fade out, but stay until `release`.
    pub fn hold(&mut self) {
        self.held = true;
        self.close();
        if let Some(s) = &self.surface
            && let Ok(region) = Region::new(&self.g.compositor)
        {
            // Nothing to click on anymore.
            s.popup
                .wl_surface()
                .set_input_region(Some(region.wl_region()));
        }
    }

    /// The drag from it ended: it can go.
    pub fn release(&mut self) {
        self.held = false;
    }

    pub fn held(&self) -> bool {
        self.held
    }

    /// Row `i`.
    pub fn entry(&self, i: usize) -> Option<&Entry> {
        self.entries.get(i)
    }

    /// Row `i`'s icon, if it has one there, and its logical size.
    pub fn mark_buffer(&self, i: usize, cache: &Cache) -> Option<(WlBuffer, f64)> {
        let (key, size) = mark_key(&self.entries.get(i)?.mark, self.scale, &self.style)?;
        Some((cache.named_icon(&key).flatten()?, size))
    }

    pub fn moving(&self) -> bool {
        self.flash.is_some()
            || !self.open.settled()
            || !self.highlight_y.settled()
            || !self.highlight_alpha.settled()
    }

    /// Advance the springs by `dt` seconds. Returns whether any moves.
    pub fn advance(&mut self, dt: f64) -> bool {
        if let Some(t) = &mut self.flash {
            *t += dt;
            if *t >= FLASH {
                self.flash = None;
                self.close();
            }
        }
        let mut moving = self.flash.is_some();
        if self.shown {
            moving |= self.open.advance(dt);
        }
        moving |= self.highlight_y.advance(dt);
        moving |= self.highlight_alpha.advance(dt);
        moving
    }

    // -- the worker ----------------------------------------------------------

    /// A text arrived; true if it is one of this menu's. Once every row's
    /// text is there, the popup opens.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let g = self.g.clone();
        let qh = self.qh.clone();
        let confirms = (self.confirms.iter_mut().flatten())
            .flat_map(|(t, lit)| std::iter::once(t).chain(lit.iter_mut()));
        let text = (self.texts.iter_mut())
            .chain(self.highlighted.iter_mut().flatten())
            .chain(confirms)
            .find(|t| t.tag == tag);
        let Some(t) = text else { return false };
        if t.request != *request {
            return false;
        }
        match cache::shm_buffer(&g, &pill.image, &qh) {
            Ok(buffer) => {
                let size = (pill.width as f64, pill.height as f64);
                if let Some((old, ..)) = t.pill.replace((buffer, size.0, size.1)) {
                    self.stale.push(old);
                }
            }
            Err(e) => warn!("menu text buffer: {e}"),
        }
        let measured = self.texts.iter().all(|t| t.pill.is_some())
            && (self.confirms.iter().flatten()).all(|(t, _)| t.pill.is_some());
        if self.surface.is_none() && !self.closing && measured {
            self.realize();
        }
        true
    }

    /// A bubble arrived; true if it is this menu's.
    pub fn bubble_ready(&mut self, tag: u64, bubble: &Bubble, image: &Image) -> bool {
        let Some((wanted, _)) = &self.wanted else {
            return false;
        };
        if *wanted != tag {
            return false;
        }
        self.wanted = None;
        match cache::shm_buffer(&self.g, image, &self.qh) {
            Ok(buffer) => {
                if let Some((_, old)) = self.bubble.replace((bubble.clone(), buffer)) {
                    self.stale.push(old);
                }
            }
            Err(e) => warn!("menu bubble buffer: {e}"),
        }
        true
    }

    /// The rows are measured: lay them out and open the popup.
    fn realize(&mut self) {
        let width = |t: &Text| t.pill.as_ref().map_or(0.0, |p| p.1);
        // A row that asks first is as wide as the wider of its texts.
        let widths: Vec<f64> = (self.texts.iter().zip(&self.confirms))
            .map(|(t, c)| width(t).max(c.as_ref().map_or(0.0, |(c, _)| width(c))))
            .collect();
        let layout = menu::layout(&self.groups, &widths, &self.style.metrics);
        let body = (layout.w, layout.h);
        let bubble = self.bubble_for(&layout, None);
        let place = Place {
            position: self.position,
            icon: self.icon,
            bounds: bubble.bounds(),
            body,
            tail_height: self.style.metrics.tail.1,
        };
        let Some(positioner) = popup::positioner(&self.g) else {
            return self.close();
        };
        let (w, h) = place.size();
        positioner.set_size(w, h);
        let a = IRect::round(self.icon);
        positioner.set_anchor_rect(a.x, a.y, a.w.max(1), a.h.max(1));
        let (anchor, gravity, (dx, dy)) = place.positioner();
        positioner.set_anchor(anchor);
        positioner.set_gravity(gravity);
        positioner.set_offset(dx, dy);
        // Along the dock only: across it, the tail would leave the icon.
        positioner.set_constraint_adjustment(match self.position {
            Position::Bottom => ConstraintAdjustment::SlideX,
            Position::Left | Position::Right => ConstraintAdjustment::SlideY,
        });
        let grab = self.grab.as_ref().map(|(seat, serial)| (seat, *serial));
        let popup = match popup::layer_popup(&self.g, &self.layer, &positioner, grab, &self.qh) {
            Ok(popup) => popup,
            Err(e) => {
                warn!("cannot show the menu: {e}");
                return self.close();
            }
        };
        let parent = popup.wl_surface();
        let sub = || Sub::new(&self.g, parent, &self.no_input, &self.qh);
        let (bubble_sub, highlight) = (sub(), sub());
        let texts = self.entries.iter().map(|_| sub()).collect();
        let marks = self.entries.iter().map(|_| sub()).collect();
        let viewport = self.g.viewporter.get_viewport(parent, &self.qh, Ignore);
        self.surface = Some(Surface {
            bubble: bubble_sub,
            highlight,
            texts,
            marks,
            viewport,
            popup,
            layout,
            place,
            at: None,
        });
        self.request_bubble(bubble);
    }

    /// The bubble for `layout`, with its tail at `tail` along its edge
    /// (None: the middle).
    fn bubble_for(&self, layout: &Layout, tail: Option<f64>) -> Bubble {
        let m = &self.style.metrics;
        let side = menu::side(self.position);
        let edge = match self.position {
            Position::Bottom => layout.w,
            Position::Left | Position::Right => layout.h,
        };
        let radius = m.radius.min(layout.w.min(layout.h) / 2.0);
        Bubble {
            w: layout.w,
            h: layout.h,
            radius,
            side,
            tail_at: tail.unwrap_or_else(|| menu::tail_at(edge / 2.0, 0.0, edge, radius, m.tail.0)),
            tail_width: m.tail.0,
            tail_height: m.tail.1,
            fill: self.style.background,
            border: self.style.border,
            separators: layout.separators(),
            separator: self.style.separator,
            shadow: self.style.shadow,
            scale: self.scale,
        }
    }

    fn request_bubble(&mut self, bubble: Bubble) {
        let shown = self.bubble.as_ref().map(|(b, _)| b);
        let asked = self.wanted.as_ref().map(|(_, b)| b);
        if shown == Some(&bubble) || asked == Some(&bubble) {
            return;
        }
        let tag = next_tag();
        self.assets.bubble(tag, bubble.clone());
        self.wanted = Some((tag, bubble));
    }

    // -- the popup -----------------------------------------------------------

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
        let radius = m.radius.min(s.layout.w.min(s.layout.h) / 2.0);
        let tail = s
            .place
            .configured_tail((x as f64, y as f64), radius, m.tail.0);
        // Input on the bubble, not on its shadow (none once a drag left it).
        if !self.held
            && let Ok(region) = Region::new(&self.g.compositor)
        {
            let (ox, oy) = s.place.body_origin();
            for r in input_rects(&s.layout, self.position, tail, m) {
                let r = IRect::round(Rect::new(r.x + ox, r.y + oy, r.w, r.h));
                region.add(r.x, r.y, r.w, r.h);
            }
            surface.set_input_region(Some(region.wl_region()));
        }
        let layout = s.layout.clone();
        let bubble = self.bubble_for(&layout, Some(tail));
        self.request_bubble(bubble);
    }

    // -- input ---------------------------------------------------------------

    /// A pointer event on the popup, at (`x`, `y`) on it.
    pub fn pointer(&mut self, kind: &PointerEventKind, x: f64, y: f64) -> MenuInput {
        let Some(s) = &self.surface else {
            return MenuInput::None;
        };
        if self.closing || self.flash.is_some() {
            return MenuInput::None;
        }
        let (ox, oy) = s.place.body_origin();
        let hit = s.layout.hit(x - ox, y - oy);
        match kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                // A press on a file's row that moves far enough drags the
                // file out.
                if let Some(p) = self.pressed
                    && let Some(row) = p.row
                    && (self.entries.get(row)).is_some_and(|e| e.file.is_some())
                    && (x - p.at.0).hypot(y - p.at.1) > self.style.drag_threshold
                {
                    self.pressed = None;
                    return MenuInput::Drag {
                        row,
                        serial: p.serial,
                    };
                }
                self.set_highlight(hit);
            }
            PointerEventKind::Leave { .. } => {
                self.pressed = None;
                self.set_highlight(None);
            }
            PointerEventKind::Press { serial, .. } => {
                self.pressed = Some(Press {
                    row: hit,
                    at: (x, y),
                    serial: *serial,
                });
            }
            PointerEventKind::Release { .. } => {
                if self.pressed.take().is_some()
                    && let Some(i) = hit
                {
                    return self.pick(i);
                }
            }
            _ => {}
        }
        MenuInput::None
    }

    /// A key while the menu has the keyboard.
    pub fn key(&mut self, key: MenuKey) -> MenuInput {
        if self.closing || self.flash.is_some() {
            return MenuInput::None;
        }
        let n = self.entries.len();
        match key {
            MenuKey::Close => MenuInput::Close,
            MenuKey::Up | MenuKey::Down => {
                let next = menu::step(self.highlight, n, key == MenuKey::Down);
                self.set_highlight(next);
                MenuInput::None
            }
            MenuKey::Pick => match self.highlight {
                Some(i) => self.pick(i),
                None => MenuInput::None,
            },
        }
    }

    /// Row `i` was picked: it flashes, then the menu fades out. A row that
    /// asks first only shows its question the first time.
    fn pick(&mut self, i: usize) -> MenuInput {
        let Some(e) = self.entries.get(i) else {
            return MenuInput::None;
        };
        if e.confirm.is_some() && self.armed != Some(i) {
            self.armed = Some(i);
            self.set_highlight(Some(i));
            return MenuInput::None;
        }
        let action = e.action.clone();
        self.set_highlight(Some(i));
        self.highlight_y.snap(self.highlight_y.target);
        self.highlight_alpha.snap(1.0);
        self.flash = Some(0.0);
        MenuInput::Pick(action)
    }

    fn set_highlight(&mut self, highlight: Option<usize>) {
        if highlight == self.highlight {
            return;
        }
        self.highlight = highlight;
        let row = (self.surface.as_ref())
            .zip(highlight)
            .and_then(|(s, i)| s.layout.row(i));
        match row {
            Some(r) => {
                // From nothing it appears in place; between rows it slides.
                if self.highlight_alpha.value <= 0.0 {
                    self.highlight_y.snap(r.y);
                } else {
                    self.highlight_y.retarget(r.y);
                }
                self.highlight_alpha.retarget(1.0);
            }
            None => self.highlight_alpha.retarget(0.0),
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

    /// Draw it and commit, once it's configured and its bubble is there.
    /// The first time, it starts opening, with the tail where the
    /// configure put it.
    pub fn render(&mut self, cache: &mut Cache) {
        let configured = self.surface.as_ref().is_some_and(|s| s.at.is_some());
        let ready = configured && self.bubble.is_some() && (self.shown || self.wanted.is_none());
        if !ready {
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
        let flash = self
            .flash
            .map_or(1.0, |t| ((t / FLASH) * std::f64::consts::PI).cos().abs());
        let highlight_alpha = self.gate(self.highlight_alpha.value * flash) * alpha;
        let style = self.style.clone();
        let (Some(s), Some((bubble, buffer))) = (&mut self.surface, &self.bubble) else {
            return;
        };
        let (ox, oy) = s.place.body_origin();
        let (tx, ty) = bubble.tip();
        // Everything grows out of the tail's tip.
        let (tx, ty) = (ox + tx, oy + ty);
        let xf = |r: Rect| r.scaled(scale, tx, ty);
        let body = |r: Rect| xf(Rect::new(r.x + ox, r.y + oy, r.w, r.h));
        let (bw, bh) = s.place.size();
        (s.bubble).present(
            Some(buffer),
            xf(Rect::new(0.0, 0.0, bw as f64, bh as f64)),
            alpha,
        );

        let m = &style.metrics;
        let p = m.padding;
        let row = Rect::new(
            p,
            self.highlight_y.value,
            s.layout.w - 2.0 * p,
            s.layout.row_height,
        );
        let key = ShapeKey::Rect {
            w: row.w.round() as i32,
            h: row.h.round() as i32,
            radius: (m.radius - p).max(0.0).round() as i32,
            color: style.highlight,
            scale: self.scale,
        };
        let shape = cache.shape(&self.g, key, &self.qh);
        let shown = shape.as_ref().filter(|_| highlight_alpha > 0.0);
        (s.highlight).present(shown.map(|s| &s.buffer), body(row), highlight_alpha);

        let dot = ShapeKey::Dots {
            count: 1,
            size: DOT,
            color: style.text.color,
            scale: self.scale,
        };
        let dot = cache.shape(&self.g, dot, &self.qh);
        for (i, e) in self.entries.iter().enumerate() {
            let lit = self.highlight == Some(i) && highlight_alpha > 0.0;
            // An armed row asks its question instead of its label.
            let (plain, highlighted) = match (self.armed == Some(i), self.confirms.get(i)) {
                (true, Some(Some((t, t_lit)))) => (t, t_lit.as_ref()),
                _ => (
                    &self.texts[i],
                    self.highlighted.get(i).and_then(Option::as_ref),
                ),
            };
            let text = match (lit, highlighted) {
                (true, Some(t)) if t.pill.is_some() => t,
                _ => plain,
            };
            match &text.pill {
                Some((buffer, w, h)) => match s.layout.text(i, *w, *h) {
                    Some(r) => s.texts[i].present(Some(buffer), body(r), alpha),
                    None => s.texts[i].clear(),
                },
                None => s.texts[i].clear(),
            }
            let mark = match &e.mark {
                Mark::None => None,
                Mark::Focused => (dot.as_ref()).map(|d| (d.buffer.clone(), DOT as f64)),
                Mark::Icon(_) | Mark::File(_) => mark_key(&e.mark, self.scale, &style)
                    .and_then(|(key, size)| Some((cache.named_icon(&key).flatten()?, size))),
            };
            match mark.and_then(|(b, size)| Some((b, s.layout.mark(i, size)?))) {
                Some((buffer, r)) => s.marks[i].present(Some(&buffer), body(r), alpha),
                None => s.marks[i].clear(),
            }
        }
        s.popup.wl_surface().commit();
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
    }
}

impl Drop for MenuView {
    fn drop(&mut self) {
        // The popup and its subsurfaces go first.
        self.surface = None;
        if let Some((_, buffer)) = self.bubble.take() {
            buffer.destroy();
        }
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
        self.transparent.destroy();
    }
}

/// The key of a row's icon at `scale`, and its logical size; None for
/// marks that aren't icons.
fn mark_key(mark: &Mark, scale: i32, style: &MenuStyle) -> Option<(NamedIcon, f64)> {
    let (name, fallbacks, size) = match mark {
        Mark::Icon(name) => (name.clone(), Vec::new(), menu::ICON),
        Mark::File(names) => {
            let (first, rest) = names.split_first()?;
            (first.clone(), rest.to_vec(), menu::FILE_ICON)
        }
        Mark::None | Mark::Focused => return None,
    };
    let key = NamedIcon {
        name,
        fallbacks,
        size: (size as u32) * scale.max(1) as u32,
        theme: style.icon_theme.clone(),
    };
    Some((key, size))
}

/// The bubble's input region in body coordinates: the body without its
/// rounded corners' outsides, and the tail at `tail` along its edge.
fn input_rects(layout: &Layout, position: Position, tail: f64, m: &Metrics) -> Vec<Rect> {
    let (w, h) = (layout.w, layout.h);
    // Where a corner's arc is close to the rect's edge.
    let cut = (m.radius.min(w.min(h) / 2.0) * 0.3).round();
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
