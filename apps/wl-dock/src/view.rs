//! One output's dock: its layer surface and the scene on it.
//!
//! A layer surface anchored to the dock's edge and both sides, spanning
//! the output, as thick as the fully magnified icons plus the label (see
//! layout.rs), so magnifying never resizes it. The surface itself shows a
//! transparent pixel; its input region is the panel, plus the band the
//! magnified icons cover while hovered, so clicks elsewhere go through.
//!
//! Everything else is a sync subsurface scaled with a viewport, in
//! stacking order: the panel (three slices, so it can grow along the dock
//! without stretching its corners), per slot an indicator and the icon,
//! and the hovered app's label on top. Each frame moves them all and then
//! commits the parent once.
//!
//! Movement runs on springs, advanced on frame callbacks by the time
//! measured since the last one: `hover` (magnification growing in and
//! out), each slot's presence (items appearing and disappearing), and the
//! label's fade, and launching apps' bounce. The pointer's position along
//! the dock is used as is (zoom.rs). Once everything has settled, no more
//! frames are requested.
//!
//! Indicators: a dot (or one per window) under apps with windows on this
//! dock, brighter for the focused app, and a ring under pinned apps whose
//! windows are all on other outputs.
//!
//! The strip (stripview.rs): hovering an app that gets one (enough
//! windows here) starts a static capture of each window, so a click finds
//! the frames ready. Clicking it opens the strip, a grabbing popup, with
//! the captures live; clicking the icon again closes it. While it is
//! open, the magnification holds still. Hovering only ever shows the
//! app's name.
//!
//! Keyboard: the surface is `on-demand`, only so that a click gives it
//! keyboard focus and with it the modifiers (Ctrl+click). After a click
//! that opens no popup, and when the strip closes, the dock gives the
//! focus back by turning its interactivity off, until the pointer next
//! moves on it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{FrameCallbackData, Region},
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
};
use wayland_client::{
    QueueHandle,
    protocol::{
        wl_buffer::WlBuffer,
        wl_output::{Transform, WlOutput},
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::shell::xdg::popup::Popup;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::popup::PopupConfigure;
use wl_shell_core::reexports::calloop::{LoopHandle, RegistrationToken};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::worker::PillRequest;
use wl_shell_render::worker::{Assets, LabelKey};
use wl_shell_render::{icons, shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::card::{self, Preview};
use wl_shell_ui::shapes::{self, Shape, ShapeKey};
use wl_shell_ui::{IRect, Spring, Sub};

use crate::app::WlDock;
use crate::config::{Config, DockLayer, DockLayout, Indicator, Position, Previews};
use crate::dock::{Item, Window};
use crate::layout::{Frame, LABEL_GAP, Metrics};
use crate::row::{self, Slot};
use crate::strip::{self, Key};
use crate::stripview::{Place, StripInput, StripStyle, StripView};
use crate::zoom;

type Qh = QueueHandle<Shell<WlDock>>;

/// Indicator dots at most.
const MAX_DOTS: usize = 3;
/// The label's fade, ms.
const LABEL_RESPONSE: f64 = 100.0;
/// How high a launching app's icon hops, in icon sizes.
const BOUNCE: f64 = 0.4;

/// Everything a dock takes from the config, for its output.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub layout: DockLayout,
    pub layer: DockLayer,
    gap: f64,
    padding: f64,
    range: f64,
    response: f64,
    hover_delay: Duration,
    indicator: Indicator,
    /// None: no labels.
    label: Option<text::Style>,
    label_max_width: i32,
    panel_color: u32,
    panel_radius: u32,
    indicator_color: u32,
    /// None: the focused app isn't marked.
    active_indicator_color: Option<u32>,
    shadow: Option<shadow::Settings>,
    icon_theme: Option<String>,
    icon_shadow: Option<shadow::IconShadow>,
    font: String,
    pub strip: StripStyle,
}

impl Style {
    pub fn new(config: &Config, layout: DockLayout) -> Self {
        let theme = config.theme();
        Self {
            layout,
            layer: config.layer,
            gap: config.icon_gap as f64,
            padding: config.padding as f64,
            range: config.magnification_range,
            response: config.response as f64,
            hover_delay: Duration::from_millis(config.hover_delay.into()),
            indicator: config.indicator,
            label: config.label_style(),
            label_max_width: (layout.icon_size as i32 * 3).max(200),
            panel_color: theme.panel_color,
            panel_radius: theme.panel_radius,
            indicator_color: theme.indicator_color,
            active_indicator_color: (config.active_indicator)
                .then_some(theme.active_indicator_color),
            shadow: config.shadow(),
            icon_theme: theme.icon_theme.clone(),
            icon_shadow: config.icon_shadow(),
            font: config.font.clone(),
            strip: StripStyle {
                enabled: config.strip,
                min_windows: config.strip_min_windows as usize,
                max_columns: layout.strip_max_columns as usize,
                max_rows: layout.strip_max_rows as usize,
                card_height: config.strip_card_height,
                max_width: config.strip_max_width,
                previews: config.previews,
                zoom: config.zoom,
                title: config.title_style(),
                placeholder: theme.placeholder,
                shadow: config.shadow(),
                morph_response: config.response as f64 * 1.7,
                response: config.response as f64,
            },
        }
    }

    /// Whether a dock with this style can't become one with `other`
    /// without a new layer surface.
    pub fn needs_new_surface(&self, other: &Style) -> bool {
        self.layout.position != other.layout.position || self.layer != other.layer
    }

    fn icon(&self) -> f64 {
        self.layout.icon_size as f64
    }

    /// Icons are rendered at their magnified size and scaled down.
    fn icon_key_size(&self) -> u32 {
        (self.icon() * self.layout.magnification).ceil() as u32
    }

    fn metrics(&self) -> Metrics {
        let label = match (&self.label, self.layout.position) {
            (None, _) => 0.0,
            (Some(style), Position::Bottom) => text::pill_height(style.size) as f64,
            (Some(_), _) => self.label_max_width as f64,
        };
        Metrics {
            icon: self.icon(),
            padding: self.padding,
            margin: self.layout.margin as f64,
            magnification: self.layout.magnification,
            label,
            icon_shadow: self.icon_shadow.map_or(0, |s| s.pad()) as f64,
        }
    }

    fn zoom(&self, hover: f64) -> zoom::Params {
        zoom::Params {
            icon: self.icon(),
            gap: self.gap,
            magnification: self.layout.magnification,
            range: self.range,
            hover,
        }
    }
}

/// The subsurfaces of one slot.
struct SlotSubs {
    indicator: Sub,
    icon: Sub,
}

pub struct DockView {
    // Field order is drop order: children before their parent surface.
    /// The strip, while open (or closing).
    strip: Option<StripView>,
    panel: [Sub; 3],
    /// Per slot, at least; extra ones stay hidden.
    slot_subs: Vec<SlotSubs>,
    label_sub: Sub,
    no_input: Region,
    viewport: WpViewport,
    layer: LayerSurface,
    transparent: WlBuffer,

    pub output: WlOutput,
    /// The output's name.
    pub name: String,
    style: Style,
    cache: Rc<RefCell<Cache>>,
    assets: Rc<Assets>,
    g: Rc<Globals>,
    qh: Qh,
    loop_handle: LoopHandle<'static, Shell<WlDock>>,

    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    slots: Vec<Slot>,
    panel_shape: Option<Shape>,
    /// The panel size the shape was rendered for, in surface orientation.
    panel_rendered: (f64, f64),
    /// Indicator rows of 1 to `MAX_DOTS` dots, and in the active color.
    dot_shapes: Vec<Option<Shape>>,
    active_dot_shapes: Vec<Option<Shape>>,
    /// The hollow indicator: windows only on other outputs.
    ring_shape: Option<Shape>,
    /// The apps launching: their icons bounce.
    launching: Vec<String>,
    /// The input region last set.
    input: Vec<IRect>,

    /// The pointer along the dock, kept after it leaves while the
    /// magnification shrinks away.
    pointer: f64,
    /// Magnification, 0 to 1.
    hover: Spring,
    /// The app under the pointer.
    hovered: Option<String>,
    /// The hover delay passed: the hovered app's label shows.
    label_on: bool,
    /// The label shown (or fading out). Looked up in the cache when
    /// drawn, since the cache may replace it.
    label: Option<LabelKey>,
    label_alpha: Spring,
    /// Where the label goes: along the dock, and its start across it.
    label_at: (f64, f64),
    label_timer: Option<RegistrationToken>,

    frame_pending: bool,
    last_frame: Option<Instant>,

    /// The output's logical size.
    output_size: Option<(f64, f64)>,
    /// Captures of the windows of the hovered app (if it gets a strip) and
    /// of the strip's.
    previews: Vec<Preview>,
    /// The magnification holds still while the strip is open.
    frozen: bool,
    /// The pointer is on the dock.
    pointer_in: bool,
    /// The surface's keyboard interactivity is `on-demand` (else `none`).
    interactive: bool,
    /// Warned that the strip can't open.
    warned_strip: bool,
}

impl DockView {
    /// An empty dock on `output` (named `name`) at `scale`.
    pub fn new(
        core: &Core<WlDock>,
        output: WlOutput,
        name: String,
        scale: i32,
        style: Style,
        cache: Rc<RefCell<Cache>>,
        assets: Rc<Assets>,
    ) -> std::io::Result<Self> {
        let g = core.g.clone();
        let qh = core.qh.clone();
        let transparent = cache::color_buffer(&g, 0, &qh)?;
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;

        let surface = g.compositor.create_surface(&qh);
        let viewport = g.viewporter.get_viewport(&surface, &qh, Ignore);
        let layer = match style.layer {
            DockLayer::Top => Layer::Top,
            DockLayer::Overlay => Layer::Overlay,
        };
        let layer = (g.layer_shell).create_layer_surface(
            &qh,
            surface,
            layer,
            Some("wl-dock"),
            Some(&output),
        );
        let anchor = match style.layout.position {
            Position::Bottom => Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            Position::Left => Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM,
            Position::Right => Anchor::RIGHT | Anchor::TOP | Anchor::BOTTOM,
        };
        layer.set_anchor(anchor);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        // Nothing takes input until there is something to click.
        (layer.wl_surface()).set_input_region(Some(no_input.wl_region()));

        let sub = || Sub::new(&g, layer.wl_surface(), &no_input, &qh);
        let panel = [sub(), sub(), sub()];
        let label_sub = sub();

        let response = style.response;
        let mut dock = Self {
            strip: None,
            panel,
            slot_subs: Vec::new(),
            label_sub,
            no_input,
            viewport,
            layer,
            transparent,
            output,
            name,
            style,
            cache,
            assets,
            g,
            qh,
            loop_handle: core.loop_handle.clone(),
            width: 0,
            height: 0,
            scale: scale.max(1),
            configured: false,
            slots: Vec::new(),
            panel_shape: None,
            panel_rendered: (1.0, 1.0),
            dot_shapes: Vec::new(),
            active_dot_shapes: Vec::new(),
            ring_shape: None,
            launching: Vec::new(),
            input: Vec::new(),
            pointer: 0.0,
            hover: Spring::new(0.0, response),
            hovered: None,
            label_on: false,
            label: None,
            label_alpha: Spring::new(0.0, LABEL_RESPONSE),
            label_at: (0.0, 0.0),
            label_timer: None,
            frame_pending: false,
            last_frame: None,
            output_size: None,
            previews: Vec::new(),
            frozen: false,
            pointer_in: false,
            interactive: true,
            warned_strip: false,
        };
        dock.apply_size();
        dock.relayout();
        // Initial commit without a buffer; mapped on configure.
        dock.layer.commit();
        Ok(dock)
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// The surface's size and exclusive zone for the style.
    fn apply_size(&self) {
        let m = self.style.metrics();
        let thickness = m.thickness();
        match self.style.layout.position {
            Position::Bottom => self.layer.set_size(0, thickness),
            Position::Left | Position::Right => self.layer.set_size(thickness, 0),
        }
        (self.layer).set_exclusive_zone(m.exclusive_zone(self.style.layout.reserve));
    }

    /// A new config (`needs_new_surface` is false for it). With `reload`,
    /// the cache was cleared: get the shapes again even if it is the same.
    pub fn set_style(&mut self, style: Style, reload: bool) {
        if style == self.style && !reload {
            return;
        }
        let resized = style.metrics() != self.style.metrics() || style.layout != self.style.layout;
        self.style = style;
        if resized {
            self.apply_size();
        }
        if self.label_on {
            self.update_label();
        }
        self.relayout();
        if reload && let Some(strip) = &mut self.strip {
            strip.reload();
        }
        self.refresh_strip();
        self.kick();
    }

    /// The output's logical size, for the strip's room.
    pub fn set_output_size(&mut self, size: Option<(i32, i32)>) {
        self.output_size = size
            .filter(|(w, h)| *w > 0 && *h > 0)
            .map(|(w, h)| (w as f64, h as f64));
    }

    pub fn configure(&mut self, width: u32, height: u32) {
        let first = !self.configured;
        self.configured = true;
        (self.width, self.height) = (width, height);
        if width > 0 && height > 0 {
            self.viewport.set_destination(width as i32, height as i32);
        }
        if first {
            let surface = self.layer.wl_surface();
            surface.attach(Some(&self.transparent), 0, 0);
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            self.pointer = self.frame().length() / 2.0;
        }
        self.relayout();
        self.render();
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.relayout();
        if self.label_on {
            self.update_label();
        }
        self.refresh_strip();
        self.kick();
    }

    /// Show `items`; changes animate once the dock is on screen.
    pub fn set_items(&mut self, core: &Core<WlDock>, items: &[Item]) {
        row::merge(&mut self.slots, items, self.style.response, self.configured);
        row::set_launching(&mut self.slots, &self.launching);
        let qh = &self.qh;
        let parent = self.layer.wl_surface();
        while self.slot_subs.len() < self.slots.len() {
            let sub = || {
                let sub = Sub::new(&self.g, parent, &self.no_input, qh);
                sub.place_below(self.label_sub.surface());
                sub
            };
            let indicator = sub();
            let icon = sub();
            self.slot_subs.push(SlotSubs { indicator, icon });
        }
        // The hovered app went: its label goes too.
        if let Some(app_id) = &self.hovered
            && !self
                .slots
                .iter()
                .any(|s| !s.leaving && s.item.app_id == *app_id)
        {
            self.hovered = None;
            self.label_on = false;
            self.cancel_label_timer();
            self.update_label();
        }
        self.request_assets(false);
        self.sync_strip(core);
        self.kick();
    }

    /// Bounce the icons of `apps`; the others land.
    pub fn set_launching(&mut self, apps: &[String]) {
        if apps == self.launching {
            return;
        }
        self.launching = apps.to_vec();
        row::set_launching(&mut self.slots, &self.launching);
        self.kick();
    }

    /// The item under the pointer.
    pub fn hovered_item(&self) -> Option<&Item> {
        let app_id = self.hovered.as_ref()?;
        (self.slots.iter())
            .find(|s| !s.leaving && s.item.app_id == *app_id)
            .map(|s| &s.item)
    }

    fn frame(&self) -> Frame {
        Frame {
            position: self.style.layout.position,
            width: self.width as f64,
            height: self.height as f64,
        }
    }

    fn icon_key(&self, app_id: &str) -> icons::Key {
        icons::Key::new(
            app_id,
            self.style.icon_key_size(),
            self.style.icon_theme.clone(),
            self.style.icon_shadow,
            self.scale,
        )
    }

    fn label_key(&self, app_id: &str) -> Option<LabelKey> {
        Some(LabelKey {
            app_id: app_id.to_string(),
            max_width: self.style.label_max_width,
            scale: self.scale,
            style: self.style.label.clone()?,
        })
    }

    /// Ask the worker for the icons and labels not in the cache, or all of
    /// them if `all` (they may have changed).
    pub fn request_assets(&self, all: bool) {
        let cache = self.cache.borrow();
        for slot in self.slots.iter().filter(|s| !s.leaving) {
            let key = self.icon_key(&slot.item.app_id);
            if all || cache.icon(&key).is_none() {
                self.assets.icon(&key, &self.style.font);
            }
            if let Some(key) = self.label_key(&slot.item.app_id)
                && (all || cache.label(&key).is_none())
            {
                self.assets.label(&key);
            }
        }
    }

    /// The worker rendered an icon (now in the cache).
    pub fn icon_ready(&mut self, key: &icons::Key) {
        if (self.slots.iter()).any(|s| self.icon_key(&s.item.app_id) == *key) {
            self.kick();
        }
    }

    /// The worker rendered a label (now in the cache).
    pub fn label_ready(&mut self, key: &LabelKey) {
        let wanted = (self.hovered.as_deref()).and_then(|a| self.label_key(a));
        if self.label_on && wanted.as_ref() == Some(key) {
            self.update_label();
        }
    }

    /// Get the shapes, icons and labels for the style and scale.
    fn relayout(&mut self) {
        self.request_assets(false);
        let m = self.style.metrics();
        let thickness = m.panel();
        let radius = (self.style.panel_radius as f64)
            .min(thickness / 2.0)
            .floor();
        // Short: the middle slice is stretched along the dock.
        let length = (2.0 * radius + 4.0).max(8.0);
        let (w, h) = match self.style.layout.position {
            Position::Bottom => (length, thickness),
            Position::Left | Position::Right => (thickness, length),
        };
        self.panel_rendered = (w, h);
        let scale = self.scale;
        let mut cache = self.cache.borrow_mut();
        self.panel_shape = cache.shape(
            &self.g,
            ShapeKey::Panel {
                w: w as i32,
                h: h as i32,
                radius: radius as i32,
                color: self.style.panel_color,
                shadow: self.style.shadow,
                scale,
            },
            &self.qh,
        );
        let mut dots = |color: Option<u32>| -> Vec<Option<Shape>> {
            let Some(color) = color else {
                return Vec::new();
            };
            (1..=MAX_DOTS)
                .map(|count| {
                    let key = ShapeKey::Dots {
                        count: count as u32,
                        size: m.dot() as i32,
                        color,
                        scale,
                    };
                    cache.shape(&self.g, key, &self.qh)
                })
                .collect()
        };
        self.dot_shapes = dots(Some(self.style.indicator_color));
        self.active_dot_shapes = dots(self.style.active_indicator_color);
        let ring = ShapeKey::Ring {
            size: m.dot() as i32,
            width: (m.dot() / 4.0).round().max(1.0) as i32,
            color: self.style.indicator_color,
            scale,
        };
        self.ring_shape = cache.shape(&self.g, ring, &self.qh);
    }

    // -- pointer -----------------------------------------------------------

    /// The pointer entered or moved, to (`x`, `y`) on the surface.
    pub fn pointer_motion(&mut self, core: &Core<WlDock>, x: f64, y: f64) {
        if !self.configured {
            return;
        }
        self.pointer_in = true;
        // Clicks give the surface keyboard focus again.
        self.set_interactive(true);
        let (u, _) = self.frame().dock_point(x, y);
        // Held still while the strip is open: the icons don't move under
        // the pointer, which may be on its way to another one.
        let at = if self.frozen { self.pointer } else { u };
        let weights = self.weights();
        let center = self.frame().length() / 2.0;
        let row = zoom::layout(&weights, center, at, &self.style.zoom(self.hover.value));
        let hovered = (row.hit(u))
            .map(|i| &self.slots[i])
            .filter(|s| !s.leaving)
            .map(|s| s.item.app_id.clone());
        if !self.frozen {
            self.pointer = u;
        }
        self.hover.retarget(1.0);
        self.set_hovered(core, hovered);
        self.kick();
    }

    /// The pointer left the dock. With the strip open, the magnification
    /// stays until it closes.
    pub fn pointer_leave(&mut self, core: &Core<WlDock>) {
        self.pointer_in = false;
        if self.strip_open() {
            self.set_hovered(core, None);
            return;
        }
        self.unhover(core);
    }

    /// Nothing is hovered anymore: the magnification shrinks away.
    fn unhover(&mut self, core: &Core<WlDock>) {
        self.frozen = false;
        self.hover.retarget(0.0);
        self.set_hovered(core, None);
        self.kick();
    }

    /// Turn keyboard focus on clicks on (`on-demand`) or off. Turning it
    /// off gives the focus back.
    fn set_interactive(&mut self, on: bool) {
        if on == self.interactive {
            return;
        }
        self.interactive = on;
        self.layer.set_keyboard_interactivity(match on {
            true => KeyboardInteractivity::OnDemand,
            false => KeyboardInteractivity::None,
        });
        if self.configured {
            self.layer.commit();
        }
    }

    /// A click was handled without opening a popup: give the keyboard
    /// focus it brought back.
    pub fn release_keyboard(&mut self) {
        if !self.strip_open() {
            self.set_interactive(false);
        }
    }

    fn set_hovered(&mut self, core: &Core<WlDock>, hovered: Option<String>) {
        if hovered == self.hovered {
            return;
        }
        self.hovered = hovered;
        if self.hovered.is_none() {
            self.cancel_label_timer();
            self.label_on = false;
        } else if !self.label_on && self.label_timer.is_none() {
            // The delay runs once per visit: moving on to the next icon
            // while waiting doesn't restart it.
            let output = self.output.clone();
            let token = core.timer(self.style.hover_delay, move |app: &mut WlDock, _| {
                if let Some(dock) = app.dock_mut(&output) {
                    dock.label_timer = None;
                    dock.label_due();
                }
            });
            match token {
                Ok(token) => self.label_timer = Some(token),
                Err(e) => {
                    warn!("{e}");
                    self.label_on = true;
                }
            }
        }
        self.sync_previews(core);
        self.update_label();
    }

    fn cancel_label_timer(&mut self) {
        if let Some(token) = self.label_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The hover delay passed.
    fn label_due(&mut self) {
        if self.hovered.is_some() {
            self.label_on = true;
            self.update_label();
        }
    }

    /// Show the hovered app's label if it is due, or fade it out.
    fn update_label(&mut self) {
        // The strip replaces the label.
        let stripped = |a: &str| (self.strip.as_ref()).is_some_and(|s| s.app_id == a);
        let want = (self.hovered.as_deref())
            .filter(|a| self.label_on && !stripped(a))
            .and_then(|a| self.label_key(a));
        let Some(key) = want else {
            self.label_alpha.retarget(0.0);
            self.kick();
            return;
        };
        let cached = self.cache.borrow().label(&key).is_some();
        match cached {
            true => {
                let fresh = self.label.as_ref().is_none_or(|l| l.app_id != key.app_id)
                    || self.label_alpha.value <= 0.0;
                self.label = Some(key);
                // Another app's name replaces the last one at once.
                if fresh && self.label_alpha.target > 0.0 {
                    self.label_alpha.snap(1.0);
                }
                self.label_alpha.retarget(1.0);
            }
            false => {
                self.assets.label(&key);
                self.label_alpha.retarget(0.0);
            }
        }
        self.kick();
    }

    // -- frames ------------------------------------------------------------

    fn request_frame(&mut self) {
        let surface = self.layer.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
    }

    /// Something changed: draw it on the next frame.
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

    fn moving(&self) -> bool {
        self.strip.as_ref().is_some_and(StripView::moving)
            || !self.hover.settled()
            || !self.label_alpha.settled()
            || self
                .slots
                .iter()
                .any(|s| !s.presence.settled() || s.bounce.is_some())
    }

    /// The frame callback: advance the springs and draw.
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        let now = Instant::now();
        let dt = (self.last_frame).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let mut moving = self.hover.advance(dt);
        moving |= self.label_alpha.advance(dt);
        moving |= row::advance(&mut self.slots, dt);
        if let Some(strip) = &mut self.strip {
            moving |= strip.advance(dt);
            if strip.gone() {
                self.strip = None;
                self.prune_previews();
                // Its app's name can show again.
                self.update_label();
            }
        }
        self.last_frame = moving.then_some(now);
        self.render();
    }

    /// Apply the current state to every subsurface and commit, asking for
    /// another frame while anything moves.
    pub fn render(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        self.place_all();
        if self.moving() && !self.frame_pending {
            self.request_frame();
        }
        self.layer.commit();
        self.render_strip();
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

    fn weights(&self) -> Vec<f64> {
        (self.slots.iter())
            .map(|s| s.presence.value.clamp(0.0, 1.0))
            .collect()
    }

    fn place_all(&mut self) {
        let frame = self.frame();
        let m = self.style.metrics();
        let scale = self.scale as f64;
        let weights = self.weights();
        let hover = self.hover.value.clamp(0.0, 1.0);
        let center = frame.length() / 2.0;
        let row = zoom::layout(&weights, center, self.pointer, &self.style.zoom(hover));
        let present: f64 = weights.iter().sum();

        // The panel: the row without its outer half gaps, plus padding,
        // never shorter than its shape.
        let half_gap = self.style.gap / 2.0;
        let (start, end) = (
            row.start + half_gap - m.padding,
            row.end - half_gap + m.padding,
        );
        let rendered = match frame.axis() {
            shapes::Axis::Horizontal => self.panel_rendered.0,
            shapes::Axis::Vertical => self.panel_rendered.1,
        };
        let length = (end - start).max(rendered);
        let mid = (start + end) / 2.0;
        let panel_u = mid - length / 2.0;
        let panel = frame.rect(panel_u, m.panel_v(), length, m.panel());
        let panel_alpha = self.gate(present.min(1.0));
        shapes::place_panel(
            &mut self.panel,
            self.panel_shape.as_ref(),
            panel,
            self.panel_rendered,
            frame.axis(),
            scale,
            &IRect::round,
            panel_alpha,
        );

        // Icons rise from the panel's inner edge; indicators sit between
        // them and the edge.
        let key_size = self.style.icon_key_size() as f64;
        let transform = match frame.axis() {
            shapes::Axis::Horizontal => Transform::Normal,
            shapes::Axis::Vertical => Transform::_90,
        };
        let keys: Vec<_> = (self.slots.iter())
            .map(|s| self.icon_key(&s.item.app_id))
            .collect();
        let cache = self.cache.borrow();
        for (i, subs) in self.slot_subs.iter_mut().enumerate() {
            let (Some(slot), Some(key), Some(cell)) =
                (self.slots.get(i), keys.get(i), row.cells.get(i))
            else {
                subs.icon.clear();
                subs.indicator.clear();
                continue;
            };
            let d = cell.size;
            // A bounce never takes the icon off the surface.
            let room = (frame.across() - m.icon_v() - d).max(0.0);
            let hop = (slot.bounce).map_or(0.0, |b| (b.height() * BOUNCE * m.icon).min(room));
            let r = frame.rect(cell.center - d / 2.0, m.icon_v() + hop, d, d);
            // The image reaches past the icon by its shadow, which scales
            // with the icon.
            let padded = r.outset(key.pad() as f64 * d / key_size);
            let alpha = if d >= 0.5 { 1.0 } else { 0.0 };
            subs.icon.present(cache.icon(key).as_ref(), padded, alpha);

            let item = &slot.item;
            let count = match self.style.indicator {
                Indicator::None => 0,
                Indicator::Dot => 1,
                Indicator::Dots => item.windows.len().min(MAX_DOTS),
            };
            let dots = match item.active && !self.active_dot_shapes.is_empty() {
                true => &self.active_dot_shapes,
                false => &self.dot_shapes,
            };
            let shape = match (item.windows.is_empty(), item.elsewhere.is_empty()) {
                (false, _) => count.checked_sub(1).and_then(|k| dots.get(k)),
                (true, false) if self.style.indicator != Indicator::None => Some(&self.ring_shape),
                _ => None,
            }
            .and_then(|s| s.as_ref());
            match shape {
                Some(s) => {
                    let (w, h) = (s.rect.w as f64, s.rect.h as f64);
                    let r = frame.rect(cell.center - w / 2.0, m.dot_v(), w, h);
                    let a = match self.g.alpha_modifier {
                        Some(_) => weights[i],
                        None if weights[i] >= 0.5 => 1.0,
                        None => 0.0,
                    };
                    subs.indicator.set_transform(transform);
                    subs.indicator.present(Some(&s.buffer), r, a);
                }
                None => subs.indicator.clear(),
            }
        }

        // The label above (or beside) the hovered icon, following it.
        if let Some(key) = &self.label {
            let slot = self.slots.iter().position(|s| s.item.app_id == key.app_id);
            if let Some(cell) = slot.and_then(|i| row.cells.get(i)) {
                self.label_at = (cell.center, m.icon_v() + cell.size + LABEL_GAP);
            }
        }
        let label_alpha = self.gate(self.label_alpha.value);
        match self.label.as_ref().and_then(|k| cache.label(k)) {
            Some(label) if label_alpha > 0.0 => {
                let (u, v) = self.label_at;
                let r = frame.upright(u, v, label.width as f64, label.height as f64);
                self.label_sub.present(Some(&label.buffer), r, label_alpha);
            }
            _ => self.label_sub.clear(),
        }
        drop(cache);

        // Input: the panel, and while magnified, the band the icons cover.
        let mut input = Vec::new();
        if present > 0.0 {
            input.push(IRect::round(panel));
            if hover > 0.0 {
                let top = m.icon_v() + row.largest();
                let band = frame.rect(
                    row.start,
                    m.panel_v(),
                    row.end - row.start,
                    top - m.panel_v(),
                );
                input.push(IRect::round(band));
            }
        }
        if input != self.input {
            self.set_input(&input);
            self.input = input;
        }
    }

    fn set_input(&self, rects: &[IRect]) {
        let region = match Region::new(&self.g.compositor) {
            Ok(region) => region,
            Err(e) => return warn!("input region: {e}"),
        };
        for r in rects {
            region.add(r.x, r.y, r.w, r.h);
        }
        // Copied on commit; the region can go right away.
        (self.layer.wl_surface()).set_input_region(Some(region.wl_region()));
    }

    // -- the strip -----------------------------------------------------------

    /// Whether `item` gets a strip: strips are on, it has enough windows
    /// here, and there are popups to show it in.
    pub fn can_strip(&self, item: &Item) -> bool {
        let strip = &self.style.strip;
        strip.enabled
            && item.windows.len() >= strip.min_windows.max(1)
            && self.g.xdg_wm_base.is_some()
    }

    /// The hovered app, if it gets a strip.
    fn strip_item(&self) -> Option<&Item> {
        self.hovered_item().filter(|item| self.can_strip(item))
    }

    /// Whether the strip is open (and not closing).
    fn strip_open(&self) -> bool {
        self.strip.as_ref().is_some_and(|s| !s.closing())
    }

    /// The app whose strip is open (and not closing).
    pub fn strip_app(&self) -> Option<&str> {
        (self.strip.as_ref())
            .filter(|s| !s.closing())
            .map(|s| s.app_id.as_str())
    }

    /// The icon of `app_id` on the dock surface, with the magnification
    /// where it is headed, and how far its far side is from the edge.
    fn icon_place(&self, app_id: &str) -> Option<(IRect, f64)> {
        let i = self.slots.iter().position(|s| s.item.app_id == app_id)?;
        let frame = self.frame();
        let m = self.style.metrics();
        let weights: Vec<f64> = (self.slots.iter())
            .map(|s| s.presence.target.clamp(0.0, 1.0))
            .collect();
        let zoom = self.style.zoom(self.hover.target.clamp(0.0, 1.0));
        let row = zoom::layout(&weights, frame.length() / 2.0, self.pointer, &zoom);
        let cell = row.cells.get(i)?;
        let d = cell.size.max(1.0);
        let r = frame.rect(cell.center - d / 2.0, m.icon_v(), d, d);
        Some((IRect::round(r), m.icon_v() + d))
    }

    /// Where the strip of `app_id` goes.
    fn strip_place(&self, app_id: &str) -> Option<Place> {
        let (icon, reach) = self.icon_place(app_id)?;
        let output = self
            .output_size
            .unwrap_or(match self.style.layout.position {
                Position::Bottom => (self.width as f64, 1080.0),
                Position::Left | Position::Right => (1920.0, self.height as f64),
            });
        Some(Place {
            position: self.style.layout.position,
            icon: wl_shell_ui::Rect::new(
                icon.x as f64,
                icon.y as f64,
                icon.w as f64,
                icon.h as f64,
            ),
            output,
            reach,
            scale: self.scale,
        })
    }

    /// The windows' aspect ratios, from their captures.
    fn aspects(&self, windows: &[Window]) -> Vec<f64> {
        (windows.iter())
            .map(|w| {
                (self.previews.iter())
                    .find(|p| p.window == w.id)
                    .and_then(Preview::aspect)
                    .unwrap_or(card::DEFAULT_ASPECT)
            })
            .collect()
    }

    /// The items changed: show the open strip's app's windows, or close
    /// it if none are left.
    fn sync_strip(&mut self, core: &Core<WlDock>) {
        if let Some(app_id) = self.strip_app().map(str::to_string) {
            let windows = (self.slots.iter())
                .find(|s| !s.leaving && s.item.app_id == app_id)
                .map(|s| s.item.windows.clone())
                .unwrap_or_default();
            if windows.is_empty() {
                self.close_strip(core);
            } else {
                let place = self.strip_place(&app_id);
                let aspects = self.aspects(&windows);
                if let (Some(strip), Some(place)) = (&mut self.strip, place) {
                    strip.update(&windows, &aspects, &self.style.strip, place);
                }
            }
        }
        self.sync_previews(core);
    }

    /// Start the captures the hovered app's strip would need (paused
    /// after their first frame, so a click finds them ready) and the open
    /// strip's (live), and drop the rest.
    fn sync_previews(&mut self, core: &Core<WlDock>) {
        if self.style.strip.previews != Previews::Off {
            let open: Vec<Window> = (self.strip.as_ref())
                .filter(|s| !s.closing())
                .map(|s| s.windows().to_vec())
                .unwrap_or_default();
            if let Some(item) = self.strip_item() {
                let hovered: Vec<Window> = (item.windows.iter())
                    .filter(|w| !open.contains(w))
                    .cloned()
                    .collect();
                self.capture(core, &hovered, false);
            }
            let live = self.style.strip.previews == Previews::Live;
            self.capture(core, &open, live);
        }
        self.prune_previews();
    }

    /// Open the strip of `app_id`, grabbing `grab` (a seat and the serial
    /// of the click). Any other strip goes at once. Returns whether it
    /// opened.
    pub fn open_strip(
        &mut self,
        core: &Core<WlDock>,
        app_id: &str,
        grab: Option<(&WlSeat, u32)>,
    ) -> bool {
        // A grabbing popup has to be the topmost: the old one goes first.
        self.strip = None;
        self.set_interactive(true);
        let Some(windows) = (self.slots.iter())
            .find(|s| !s.leaving && s.item.app_id == app_id)
            .map(|s| s.item.windows.clone())
        else {
            return false;
        };
        let Some(place) = self.strip_place(app_id) else {
            return false;
        };
        let aspects = self.aspects(&windows);
        // Start on the app's previous window.
        let ids: Vec<&str> = windows.iter().map(|w| w.id.as_str()).collect();
        let focused = (core.focus_stream.running())
            .then(|| core.mru.first())
            .flatten();
        let highlight = strip::first_highlight(&ids, &core.mru, focused);
        match StripView::new(
            self.g.clone(),
            self.assets.clone(),
            &self.qh,
            &self.layer,
            grab,
            app_id.to_string(),
            windows,
            aspects,
            highlight,
            self.style.strip.clone(),
            place,
        ) {
            Ok(strip) => {
                self.strip = Some(strip);
                // The icons hold still under the strip.
                self.frozen = true;
            }
            Err(e) => {
                if !std::mem::replace(&mut self.warned_strip, true) {
                    warn!("cannot show the strip: {e}");
                }
            }
        }
        self.sync_previews(core);
        self.update_label();
        self.kick();
        self.strip.is_some()
    }

    /// Shrink the strip back into the icon (a card was picked, say); it
    /// goes once it's there. Gives the keyboard back.
    pub fn close_strip(&mut self, core: &Core<WlDock>) {
        let Some(strip) = &mut self.strip else { return };
        if !strip.closing() {
            strip.close();
        }
        self.strip_closed(core);
    }

    /// Drop the strip at once.
    pub fn drop_strip(&mut self, core: &Core<WlDock>) {
        if self.strip.take().is_some() {
            self.strip_closed(core);
        }
    }

    fn strip_closed(&mut self, core: &Core<WlDock>) {
        self.frozen = false;
        self.set_interactive(false);
        if !self.pointer_in {
            self.unhover(core);
        }
        self.sync_previews(core);
        self.update_label();
        self.kick();
    }

    /// The style, scale or output changed: lay the strip out again.
    fn refresh_strip(&mut self) {
        let Some(strip) = &self.strip else { return };
        let (app_id, windows) = (strip.app_id.clone(), strip.windows().to_vec());
        let place = self.strip_place(&app_id);
        let aspects = self.aspects(&windows);
        if let (Some(strip), Some(place)) = (&mut self.strip, place) {
            strip.update(&windows, &aspects, &self.style.strip, place);
        }
    }

    /// Capture `windows` (those not captured yet), all `live` or paused.
    fn capture(&mut self, core: &Core<WlDock>, windows: &[Window], live: bool) {
        let missing: Vec<&Window> = (windows.iter())
            .filter(|w| !self.previews.iter().any(|p| p.window == w.id))
            .collect();
        if !missing.is_empty() {
            for (handle, t) in core.toplevels() {
                if missing.iter().any(|w| w.id == t.identifier) {
                    let p = Preview::new(t.identifier, &handle, &self.g, live, &self.qh);
                    self.previews.push(p);
                }
            }
        }
        for p in &mut self.previews {
            if windows.iter().any(|w| w.id == p.window) {
                p.set_live(live, &self.g, &self.qh);
            }
        }
    }

    /// Drop the captures neither the hovered app nor the strip needs, and
    /// pause the closing strip's.
    fn prune_previews(&mut self) {
        let mut keep: Vec<String> = Vec::new();
        if self.style.strip.previews != Previews::Off {
            if let Some(item) = self.strip_item() {
                keep.extend(item.windows.iter().map(|w| w.id.clone()));
            }
            if let Some(strip) = &self.strip {
                keep.extend(strip.windows().iter().map(|w| w.id.clone()));
            }
        }
        self.previews.retain(|p| keep.contains(&p.window));
        if let Some(strip) = &self.strip
            && strip.closing()
        {
            for p in &mut self.previews {
                if strip.windows().iter().any(|w| w.id == p.window) {
                    p.set_live(false, &self.g, &self.qh);
                }
            }
        }
    }

    /// Draw the strip, if any.
    fn render_strip(&mut self) {
        let Some(strip) = &mut self.strip else { return };
        let key = self.style.strip.previews.eq(&Previews::Off).then(|| {
            icons::Key::new(
                &strip.app_id,
                self.style.icon_key_size(),
                self.style.icon_theme.clone(),
                self.style.icon_shadow,
                self.scale,
            )
        });
        let mut cache = self.cache.borrow_mut();
        strip.fetch_shadows(&mut cache);
        let icon = key.as_ref().and_then(|k| Some((cache.icon(k)?, k)));
        let icon = icon.as_ref().map(|(buffer, k)| {
            let p = (k.pad() * k.scale) as f64;
            (
                buffer,
                k.image_size() as f64,
                [p, p, k.size as f64, k.size as f64],
            )
        });
        strip.render(&mut self.previews, icon);
    }

    /// Whether `surface` is this dock's strip.
    pub fn has_strip_surface(&self, surface: &WlSurface) -> bool {
        self.strip.as_ref().is_some_and(|s| s.surface() == surface)
    }

    pub fn has_popup(&self, popup: &Popup) -> bool {
        self.strip.as_ref().is_some_and(|s| s.is(popup))
    }

    /// The compositor placed the strip.
    pub fn popup_configure(&mut self, configure: &PopupConfigure) {
        if let Some(strip) = &mut self.strip {
            strip.configure(configure);
        }
        self.render();
    }

    /// The compositor dismissed the strip (a click on another client).
    pub fn popup_done(&mut self, core: &Core<WlDock>) {
        self.drop_strip(core);
    }

    /// A pointer event on the strip. Returns the window to activate, if a
    /// card was clicked.
    pub fn strip_pointer(&mut self, core: &Core<WlDock>, event: &PointerEvent) -> Option<String> {
        let strip = self.strip.as_mut()?;
        let (x, y) = event.position;
        let input = strip.pointer(&event.kind, x, y);
        self.kick();
        self.strip_input(core, input)
    }

    /// A key on the strip. Returns the window to activate, if any.
    pub fn strip_key(&mut self, core: &Core<WlDock>, key: Key) -> Option<String> {
        let input = self.strip.as_mut()?.key(key);
        self.kick();
        self.strip_input(core, input)
    }

    fn strip_input(&mut self, core: &Core<WlDock>, input: StripInput) -> Option<String> {
        match input {
            StripInput::None => None,
            StripInput::Close => {
                self.close_strip(core);
                None
            }
            StripInput::Activate(id) => {
                self.close_strip(core);
                Some(id)
            }
        }
    }

    /// A title pill from the worker; true if it was the strip's.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let ours = (self.strip.as_mut()).is_some_and(|s| s.pill_ready(tag, request, pill));
        if ours {
            self.kick();
        }
        ours
    }

    /// Whether capture `id` is one of this dock's.
    pub fn has_capture(&self, id: CaptureId) -> bool {
        self.previews.iter().any(|p| p.id == id)
    }

    /// An event of capture `id`.
    pub fn capture_update(
        &mut self,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        let Some(p) = self.previews.iter_mut().find(|p| p.id == id) else {
            return;
        };
        let update = p.update(&self.g, &self.qh, f);
        let window = p.window.clone();
        let shown =
            (self.strip.as_ref()).is_some_and(|s| s.windows().iter().any(|w| w.id == window));
        if update.resized {
            self.refresh_strip();
        }
        if shown && (update.shown || update.resized) {
            self.kick();
        }
    }
}

impl Drop for DockView {
    fn drop(&mut self) {
        self.cancel_label_timer();
        self.viewport.destroy();
        self.transparent.destroy();
    }
}
