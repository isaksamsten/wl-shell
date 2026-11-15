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
//! The menu (menuview.rs): a right click opens the app's menu, another
//! grabbing popup, a speech bubble pointing at the icon. While it is open
//! the magnification holds still and no label shows. Only one of the strip
//! and the menu is open at a time.
//!
//! Keyboard: the surface is `on-demand`, only so that a click gives it
//! keyboard focus and with it the modifiers (Ctrl+click). After a click
//! that opens no popup, and when the strip or menu closes, the dock gives
//! the focus back by turning its interactivity off, until the pointer
//! next moves on it.
//!
//! Autohide (autohide.rs): a dock with `autohide` starts hidden, slid off
//! its edge on the `shown` spring, with all of it cleared once it is out
//! of sight. Its input region is then a thin band along the edge, as long
//! as the resting panel; the pointer resting there for `reveal-delay`
//! brings it back, and it hides `hide-delay` after the pointer leaves,
//! unless its strip or menu is open. While shown, the input region
//! reaches down to the edge, so moving the pointer into the margin
//! doesn't count as leaving. It reserves no space.

use std::cell::RefCell;
use std::collections::BTreeMap;
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
use wl_shell_core::reexports::calloop::timer::{TimeoutAction, Timer};
use wl_shell_core::reexports::calloop::{LoopHandle, RegistrationToken};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::bubble::Bubble;
use wl_shell_render::worker::{Assets, LabelKey, PillRequest};
use wl_shell_render::{Image, icons, shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::card::{self, Preview};
use wl_shell_ui::shapes::{self, Shape, ShapeKey};
use wl_shell_ui::{IRect, Spring, Sub};

use crate::app::WlDock;
use crate::autohide;
use crate::config::{Config, DockLayer, DockLayout, Indicator, Position, Previews};
use crate::dock::{Item, Window};
use crate::layout::{Frame, LABEL_GAP, Metrics};
use crate::menu::{self, Group, MenuKey};
use crate::menuview::{MenuInput, MenuStyle, MenuView};
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
/// The autohide slide's response, in spring responses.
const SLIDE_RESPONSE: f64 = 1.5;

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
    pub menu: MenuStyle,
    autohide: autohide::Settings,
    /// How far the panel's shadow reaches past it.
    shadow_reach: f64,
    /// Icons (names or paths) and names from `[app.<id>]` tables.
    icons: BTreeMap<String, String>,
    names: BTreeMap<String, String>,
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
            menu: MenuStyle {
                metrics: menu::Metrics {
                    row_height: theme.menu_row_height as f64,
                    padding: theme.menu_padding as f64,
                    group_gap: theme.menu_group_gap as f64,
                    max_width: theme.menu_max_width as f64,
                    radius: theme.menu_radius as f64,
                    tail: (theme.menu_tail[0] as f64, theme.menu_tail[1] as f64),
                },
                text: config.menu_text_style(),
                highlight_text: theme.menu_highlight_text,
                background: theme.menu_background,
                border: theme.menu_border,
                separator: theme.menu_separator,
                highlight: theme.menu_highlight,
                shadow: config.menu_shadow(),
                icon_theme: theme.icon_theme.clone(),
            },
            autohide: autohide::Settings {
                enabled: layout.autohide,
                reveal_delay: Duration::from_millis(config.reveal_delay.into()),
                hide_delay: Duration::from_millis(config.hide_delay.into()),
                trigger: config.autohide_trigger as f64,
            },
            shadow_reach: autohide::shadow_reach(config.shadow().as_ref()),
            icons: (config.app.iter())
                .filter_map(|(id, a)| Some((id.clone(), a.icon.clone()?)))
                .collect(),
            names: (config.app.iter())
                .filter_map(|(id, a)| Some((id.clone(), a.name.clone()?)))
                .collect(),
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

    /// How far the resting dock reaches from its edge (autohide slides it
    /// that far).
    fn reach(&self) -> f64 {
        autohide::reach(&self.metrics(), self.shadow_reach)
    }
}

/// The subsurfaces of one slot.
struct SlotSubs {
    indicator: Sub,
    icon: Sub,
}

pub struct DockView {
    // Field order is drop order: children before their parent surface.
    /// The menu, while open (or closing).
    menu: Option<MenuView>,
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
    /// Where the pointer last was on the surface, while it is on it.
    pointer_at: Option<(f64, f64)>,
    /// Autohide: 0 hidden (slid off the edge) to 1 shown. Its target says
    /// where the dock is headed.
    shown: Spring,
    /// Shows (true) or hides the dock when it fires.
    autohide_timer: Option<(bool, RegistrationToken)>,
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
        // An autohiding dock starts out of sight.
        let shown = match style.autohide.enabled {
            true => 0.0,
            false => 1.0,
        };
        let mut dock = Self {
            menu: None,
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
            pointer_at: None,
            shown: Spring::new(shown, response * SLIDE_RESPONSE),
            autohide_timer: None,
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
        // A dock that hides reserves nothing: windows would go under it
        // anyway once it slides in.
        let reserve = self.style.layout.reserve && !self.style.autohide.enabled;
        (self.layer).set_exclusive_zone(m.exclusive_zone(reserve));
    }

    /// A new config (`needs_new_surface` is false for it). With `reload`,
    /// the cache was cleared: get the shapes again even if it is the same.
    pub fn set_style(&mut self, style: Style, reload: bool) {
        if style == self.style && !reload {
            return;
        }
        let resized = style.metrics() != self.style.metrics() || style.layout != self.style.layout;
        // The menu was built for the old look.
        if style.menu != self.style.menu || style.layout.position != self.style.layout.position {
            self.menu = None;
            self.frozen = self.strip_open();
        }
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
        // Autohide may have been turned on or off, or its delays changed.
        self.cancel_autohide_timer();
        self.sync_autohide();
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
        .with_icon(self.style.icons.get(app_id).cloned())
    }

    fn label_key(&self, app_id: &str) -> Option<LabelKey> {
        Some(LabelKey {
            app_id: app_id.to_string(),
            name: self.style.names.get(app_id).cloned(),
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
        self.pointer_at = Some((x, y));
        // Clicks give the surface keyboard focus again.
        self.set_interactive(true);
        // Hidden, the pointer is on the trigger band: it only brings the
        // dock back, after the delay.
        self.sync_autohide();
        if !self.showing() {
            return;
        }
        let (u, _) = self.frame().dock_point(x, y);
        // Held still while the strip or menu is open: the icons don't move
        // under the pointer, which may be on its way to another one.
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

    /// The pointer left the dock. With the strip or menu open, the
    /// magnification stays until it closes.
    pub fn pointer_leave(&mut self, core: &Core<WlDock>) {
        self.pointer_in = false;
        self.pointer_at = None;
        self.sync_autohide();
        if self.strip_open() || self.menu_open() {
            self.set_hovered(core, None);
            return;
        }
        self.unhover(core);
    }

    // -- autohide ------------------------------------------------------------

    /// Whether the dock is shown or on its way back (else hidden or
    /// hiding).
    fn showing(&self) -> bool {
        self.shown.target > 0.5
    }

    /// Start (or keep, or cancel) the timer for where autohide wants the
    /// dock, or show or hide it at once if there is no delay.
    fn sync_autohide(&mut self) {
        let held = self.strip_open() || self.menu_open();
        let next = autohide::next(&self.style.autohide, self.showing(), self.pointer_in, held);
        let Some((show, delay)) = next else {
            return self.cancel_autohide_timer();
        };
        if (self.autohide_timer).is_some_and(|(s, _)| s == show) {
            return;
        }
        self.cancel_autohide_timer();
        if delay.is_zero() {
            return self.set_shown(show);
        }
        let output = self.output.clone();
        let timer = Timer::from_duration(delay);
        let due = move |_, _: &mut (), shell: &mut Shell<WlDock>| {
            if let Some(dock) = shell.program.dock_mut(&output) {
                dock.autohide_due(&shell.core, show);
            }
            TimeoutAction::Drop
        };
        let token = self.loop_handle.insert_source(timer, due);
        match token {
            Ok(token) => self.autohide_timer = Some((show, token)),
            Err(e) => {
                warn!("failed to start a timer: {e}");
                self.set_shown(show);
            }
        }
    }

    fn cancel_autohide_timer(&mut self) {
        if let Some((_, token)) = self.autohide_timer.take() {
            self.loop_handle.remove(token);
        }
    }

    /// The autohide delay passed: slide in (`show`) or out.
    fn autohide_due(&mut self, core: &Core<WlDock>, show: bool) {
        self.autohide_timer = None;
        self.set_shown(show);
        // Back under a resting pointer: hover where it is.
        if show && let Some((x, y)) = self.pointer_at {
            self.pointer_motion(core, x, y);
        }
        self.sync_autohide();
    }

    fn set_shown(&mut self, show: bool) {
        self.shown.retarget(if show { 1.0 } else { 0.0 });
        self.kick();
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
        if !self.strip_open() && !self.menu_open() {
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
        // The strip replaces its app's label; no label shows over a menu.
        let stripped = |a: &str| (self.strip.as_ref()).is_some_and(|s| s.app_id == a);
        let want = (self.hovered.as_deref())
            .filter(|a| self.label_on && !stripped(a) && self.menu.is_none())
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
            || self.menu.as_ref().is_some_and(MenuView::moving)
            || !self.hover.settled()
            || !self.shown.settled()
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
        moving |= self.shown.advance(dt);
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
        if let Some(menu) = &mut self.menu {
            moving |= menu.advance(dt);
            if menu.gone() {
                self.menu = None;
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
        self.render_menu();
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
        // Autohide slides everything toward the edge, and once it is all
        // out of sight, clears it.
        let slide = autohide::slide(self.shown.value, self.style.reach());
        let seen = if self.shown.value > 0.0 { 1.0 } else { 0.0 };

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
        let panel = frame.rect(panel_u, m.panel_v() - slide, length, m.panel());
        let panel_alpha = self.gate(present.min(1.0)) * seen;
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
            let r = frame.rect(cell.center - d / 2.0, m.icon_v() + hop - slide, d, d);
            // The image reaches past the icon by its shadow, which scales
            // with the icon.
            let padded = r.outset(key.pad() as f64 * d / key_size);
            let alpha = if d >= 0.5 { seen } else { 0.0 };
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
                    let r = frame.rect(cell.center - w / 2.0, m.dot_v() - slide, w, h);
                    let a = match self.g.alpha_modifier {
                        Some(_) => weights[i],
                        None if weights[i] >= 0.5 => 1.0,
                        None => 0.0,
                    } * seen;
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
        let label_alpha = self.gate(self.label_alpha.value) * seen;
        match self.label.as_ref().and_then(|k| cache.label(k)) {
            Some(label) if label_alpha > 0.0 => {
                let (u, v) = self.label_at;
                let v = v - slide;
                let r = frame.upright(u, v, label.width as f64, label.height as f64);
                self.label_sub.present(Some(&label.buffer), r, label_alpha);
            }
            _ => self.label_sub.clear(),
        }
        drop(cache);

        // Input: the panel, and while magnified, the band the icons cover.
        // An autohiding dock reaches down to its edge, so the pointer
        // going into the margin doesn't leave it; hidden, it keeps only
        // the trigger band there.
        let mut input = Vec::new();
        let autohide = self.style.autohide;
        if present > 0.0 && !self.showing() {
            let trigger = frame.rect(panel_u, 0.0, length, autohide.trigger);
            input.push(IRect::round(trigger));
        } else if present > 0.0 {
            let reach = m.panel_v() + m.panel() - slide;
            input.push(IRect::round(match autohide.enabled {
                true => frame.rect(panel_u, 0.0, length, reach.max(autohide.trigger)),
                false => panel,
            }));
            if hover > 0.0 {
                let top = m.icon_v() + row.largest() - slide;
                let band = frame.rect(
                    row.start,
                    m.panel_v() - slide,
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
        // A grabbing popup has to be the topmost: the old ones go first.
        self.menu = None;
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
        self.frozen = self.menu_open();
        self.set_interactive(false);
        if !self.pointer_in {
            self.unhover(core);
        }
        self.sync_previews(core);
        // Nothing holds the dock anymore.
        self.sync_autohide();
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
        let Some(app_id) = self.strip.as_ref().map(|s| s.app_id.clone()) else {
            return;
        };
        let key = (self.style.strip.previews == Previews::Off).then(|| self.icon_key(&app_id));
        let Some(strip) = &mut self.strip else { return };
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
            || self.menu.as_ref().is_some_and(|m| m.is(popup))
    }

    /// The compositor placed the strip or the menu.
    pub fn popup_configure(&mut self, popup: &Popup, configure: &PopupConfigure) {
        if let Some(strip) = &mut self.strip
            && strip.is(popup)
        {
            strip.configure(configure);
        }
        if let Some(menu) = &mut self.menu
            && menu.is(popup)
        {
            menu.configure(configure);
        }
        self.render();
    }

    /// The compositor dismissed the strip or the menu (a click on another
    /// client).
    pub fn popup_done(&mut self, core: &Core<WlDock>, popup: &Popup) {
        if self.strip.as_ref().is_some_and(|s| s.is(popup)) {
            self.drop_strip(core);
        }
        if self.menu.as_ref().is_some_and(|m| m.is(popup)) {
            self.drop_menu(core);
        }
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

    /// A text from the worker; true if it was the strip's or the menu's.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let ours = (self.strip.as_mut()).is_some_and(|s| s.pill_ready(tag, request, pill))
            || (self.menu.as_mut()).is_some_and(|m| m.pill_ready(tag, request, pill));
        if ours {
            self.kick();
        }
        ours
    }

    /// The item of `app_id`, if it is on this dock.
    pub fn item(&self, app_id: &str) -> Option<&Item> {
        (self.slots.iter())
            .find(|s| !s.leaving && s.item.app_id == app_id)
            .map(|s| &s.item)
    }

    // -- the menu ------------------------------------------------------------

    /// Whether the menu is open (and not closing).
    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(|m| !m.closing())
    }

    /// The app whose menu is open (and not closing).
    pub fn menu_app(&self) -> Option<&str> {
        (self.menu.as_ref())
            .filter(|m| !m.closing())
            .map(|m| m.app_id.as_str())
    }

    /// Open `groups` as the menu of `app_id`, grabbing `grab` (a seat and
    /// the serial of the click). The strip and any other menu go at once.
    /// It shows once its rows are rendered. Returns whether it opened.
    pub fn open_menu(
        &mut self,
        core: &Core<WlDock>,
        app_id: &str,
        groups: Vec<Group>,
        grab: Option<(&WlSeat, u32)>,
    ) -> bool {
        // A grabbing popup has to be the topmost: the old ones go first.
        self.menu = None;
        if self.strip.take().is_some() {
            self.sync_previews(core);
        }
        let icon = self
            .icon_place(app_id)
            .map(|(r, _)| wl_shell_ui::Rect::new(r.x as f64, r.y as f64, r.w as f64, r.h as f64));
        if let (Some(icon), false) = (icon, groups.is_empty()) {
            let menu = MenuView::new(
                self.g.clone(),
                self.assets.clone(),
                &self.cache.borrow(),
                &self.qh,
                &self.layer,
                grab,
                app_id.to_string(),
                groups,
                self.style.menu.clone(),
                self.style.layout.position,
                icon,
                self.scale,
            );
            match menu {
                Ok(menu) => self.menu = Some(menu),
                Err(e) => warn!("cannot show the menu: {e}"),
            }
        }
        // The icons hold still under the menu.
        self.frozen = self.menu.is_some();
        self.set_interactive(true);
        self.update_label();
        self.kick();
        self.menu.is_some()
    }

    /// Fade the menu out; it goes once it's gone. Gives the keyboard back.
    pub fn close_menu(&mut self, core: &Core<WlDock>) {
        let Some(menu) = &mut self.menu else { return };
        if !menu.closing() {
            menu.close();
            self.menu_closed(core);
        }
    }

    /// Drop the menu at once.
    pub fn drop_menu(&mut self, core: &Core<WlDock>) {
        if self.menu.take().is_some() {
            self.menu_closed(core);
        }
    }

    fn menu_closed(&mut self, core: &Core<WlDock>) {
        self.frozen = self.strip_open();
        if !self.strip_open() {
            self.set_interactive(false);
        }
        if !self.pointer_in {
            self.unhover(core);
        }
        self.sync_autohide();
        self.update_label();
        self.kick();
    }

    /// Whether `surface` is this dock's menu.
    pub fn has_menu_surface(&self, surface: &WlSurface) -> bool {
        (self.menu.as_ref()).is_some_and(|m| m.surface() == Some(surface))
    }

    /// Whether keys on `surface` go to this dock's menu: it is the menu's,
    /// or the dock's while the menu isn't shown yet.
    pub fn menu_has_keys(&self, surface: &WlSurface) -> bool {
        self.menu_open() && (self.has_menu_surface(surface) || self.surface() == surface)
    }

    /// A pointer event on the menu. Returns the app and what to do, if a
    /// row was picked.
    pub fn menu_pointer(
        &mut self,
        core: &Core<WlDock>,
        event: &PointerEvent,
    ) -> Option<(String, menu::Action)> {
        let menu = self.menu.as_mut()?;
        let (x, y) = event.position;
        let input = menu.pointer(&event.kind, x, y);
        self.kick();
        self.menu_input(core, input)
    }

    /// A key on the menu. Returns the app and what to do, if a row was
    /// picked.
    pub fn menu_key(
        &mut self,
        core: &Core<WlDock>,
        key: MenuKey,
    ) -> Option<(String, menu::Action)> {
        let input = self.menu.as_mut()?.key(key);
        self.kick();
        self.menu_input(core, input)
    }

    fn menu_input(
        &mut self,
        core: &Core<WlDock>,
        input: MenuInput,
    ) -> Option<(String, menu::Action)> {
        match input {
            MenuInput::None => None,
            MenuInput::Close => {
                self.close_menu(core);
                None
            }
            // The row flashes, then the menu fades by itself.
            MenuInput::Pick(action) => {
                let app_id = self.menu.as_ref()?.app_id.clone();
                self.menu_closed(core);
                Some((app_id, action))
            }
        }
    }

    /// A bubble from the worker; true if it was the menu's.
    pub fn bubble_ready(&mut self, tag: u64, bubble: &Bubble, image: &Image) -> bool {
        let ours = (self.menu.as_mut()).is_some_and(|m| m.bubble_ready(tag, bubble, image));
        if ours {
            self.kick();
        }
        ours
    }

    /// An icon by name arrived (now in the cache): the menu may show it.
    pub fn named_icon_ready(&mut self) {
        if self.menu.is_some() {
            self.kick();
        }
    }

    /// Draw the menu, if any.
    fn render_menu(&mut self) {
        if let Some(menu) = &mut self.menu {
            menu.render(&mut self.cache.borrow_mut());
        }
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
        self.cancel_autohide_timer();
        self.viewport.destroy();
        self.transparent.destroy();
    }
}
