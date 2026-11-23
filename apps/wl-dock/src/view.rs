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
//! label's fade, and launching apps' bounce. The zoom focus normally
//! follows the pointer without lag (zoom.rs), but glides to the clicked
//! icon when switching away from a popup. Once everything has settled,
//! no more frames are requested.
//!
//! Indicators: a dot (or one per window) under apps with windows on this
//! dock, brighter for the focused app, and a ring under pinned apps whose
//! windows are all on other outputs.
//!
//! The strip (stripview.rs): hovering an app that gets one (enough
//! windows here) starts a static capture of each window, so a click finds
//! the frames ready. Clicking it opens the strip, a grabbing popup, with
//! the captures live; clicking the icon again closes it. While it is
//! open, moving the pointer doesn't move the magnification. Clicking
//! another icon transfers it to that icon.
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
//!
//! The Trash (dock.rs) comes last, after a separator: a thin line that
//! takes a fixed length along the dock and doesn't magnify. Its icon
//! follows whether the Trash is empty. Pinned folders sit between the
//! separator and the Trash, with their theme icon (folder.rs), faded if
//! they don't exist; a folder's popup is a menu (`open_menu` with
//! `popup`) whose file rows can be dragged out (`MenuEvent::DragOut`).
//!
//! Dragging (drag.rs): an app's icon lifts out of the dock and follows
//! the pointer on its own subsurface, on top of everything, kept on the
//! surface (the press's implicit grab sends the motion here even off
//! it). Its slot closes, and a gap opens where it would land, as a cell
//! of its own with a spring, anchored before the pinned app it would
//! precede so it stays put while the items change. Back home, its own
//! slot opens again instead. Over the Trash, if the Trash takes it, the
//! Trash grows a little and its label says what a drop does. On a drop
//! the gap's spring becomes the slot's, so the pins' new order (applied
//! by app.rs right away) takes over without a jump; the icon then flies
//! to its slot, as it does back home when the drag is cancelled. Into
//! the Trash, it just goes. Folders reorder among the folders the same
//! way.
//!
//! Drops from outside (drag.rs): a `wl_data_device` drag over the dock
//! magnifies it as the pointer does. app.rs says which items take it and
//! what their labels say (`Offer`); the target under it grows and its
//! label says what a drop does, and folders that can be pinned open a
//! gap among the folders, labelled over the gap. A hidden dock comes back
//! for a drag on its trigger band and stays while the drag is over it.
//!
//! Keyboard activation (activate.rs, driven by app.rs): while it is on,
//! the numbered items show their number in a badge at the icon's corner
//! (a dot and a worker line, on subsurfaces above the icons), the dock
//! comes out if it is hidden, and the item acted on magnifies as if
//! hovered. Its strip and its folders' popups open without a grab; the
//! highlight steps through the strip's windows in MRU order.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
use wl_shell_lua::channel::PopupEvent;
use wl_shell_render::bubble::Bubble;
use wl_shell_render::files::FileEntry;
use wl_shell_render::worker::{Assets, LabelKey, PillRequest};
use wl_shell_render::{Image, icons, shadow, text, ui};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::card::{self, Preview};
use wl_shell_ui::shapes::{self, Shape, ShapeKey};
use wl_shell_ui::{IRect, Rect, Spring, Sub, lerp};

use crate::activate;
use crate::app::WlDock;
use crate::autohide;
use crate::config::{Config, DockLayer, DockLayout, FolderView, Indicator, Position, Previews};
use crate::dock::{self, Item, Kind, Window};
use crate::drag::{self, Target};
use crate::folder::{self, UserDirs};
use crate::layout::{Frame, LABEL_GAP, Metrics};
use crate::menu::{self, MenuKey};
use crate::menuview::{Look, MenuInput, MenuStyle, MenuView};
use crate::pins::{self, Zone};
use crate::row::{self, Slot};
use crate::stack;
use crate::strip::{self, Key};
use crate::stripview::{Place, StripInput, StripStyle, StripView};
use crate::trash;
use crate::widgets::{TileBase, WidgetIcon};
use crate::widgetview::WidgetView;
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
/// How much the target of a drag grows while it takes it.
const TARGET_GROW: f64 = 0.15;
/// The Trash's label while it takes the dragged icon.
const REMOVE_LABEL: &str = "Remove from Dock";
/// The label's key while a drop from outside would pin folders: it
/// follows the gap.
const GAP_LABEL: &str = "\0gap";
/// How visible a folder that doesn't exist is.
const MISSING_ALPHA: f64 = 0.4;
/// What a stack leaves free at the output's far edge (or both ends).
const STACK_ROOM: f64 = 32.0;
/// How far a badge's center is in from the icon's corner, in badge sizes.
const BADGE_INSET: f64 = 0.3;
/// The icon a standalone widget shows until its plugin draws one.
const WIDGET_ICON: &str = "application-x-addon";

/// Tags of badge requests, unique in the process.
fn next_tag() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1 << 50);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// How the badges look.
#[derive(Clone, Debug, PartialEq)]
struct BadgeStyle {
    /// The digit's.
    text: text::Style,
    /// The dot's diameter.
    size: f64,
    background: u32,
}

/// One badge's digit, rendered by the worker.
struct BadgeText {
    tag: u64,
    request: PillRequest,
    /// The rendered line and its logical size.
    pill: Option<(WlBuffer, f64, f64)>,
}

impl Drop for BadgeText {
    fn drop(&mut self) {
        if let Some((buffer, ..)) = self.pill.take() {
            buffer.destroy();
        }
    }
}

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
    separator_color: u32,
    /// Icons (names or paths) and names from `[app.<id>]` tables.
    icons: BTreeMap<String, String>,
    names: BTreeMap<String, String>,
    /// Icons from `[folder.<path>]` tables, by path.
    folder_icons: Vec<(PathBuf, String)>,
    /// The XDG user directories, for their folders' icons.
    user_dirs: UserDirs,
    /// None: no badges during keyboard activation.
    badge: Option<BadgeStyle>,
    /// How widgets' trees look.
    pub ui_theme: Arc<ui::Theme>,
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
            response: config.appearance.response as f64,
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
            font: config.appearance.font.clone(),
            strip: StripStyle {
                enabled: config.windows.strip,
                min_windows: config.windows.min_windows as usize,
                max_columns: layout.strip_max_columns as usize,
                max_rows: layout.strip_max_rows as usize,
                card_height: config.windows.card_height,
                max_width: config.windows.max_width,
                previews: config.windows.previews,
                zoom: config.windows.zoom,
                title: config.title_style(),
                placeholder: theme.placeholder,
                shadow: config.shadow(),
                morph_response: config.appearance.response as f64 * 1.7,
                response: config.appearance.response as f64,
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
                drag_threshold: config.drag_threshold as f64,
                stack_icon: layout.icon_size as f64,
                stack_shadow: config.icon_shadow(),
            },
            autohide: autohide::Settings {
                enabled: layout.autohide,
                reveal_delay: Duration::from_millis(config.autohide.reveal_delay.into()),
                hide_delay: Duration::from_millis(config.autohide.hide_delay.into()),
                trigger: config.autohide.trigger as f64,
            },
            shadow_reach: autohide::shadow_reach(config.shadow().as_ref()),
            separator_color: theme.separator_color,
            icons: (config.app.iter())
                .filter_map(|(id, a)| Some((id.clone(), a.icon.clone()?)))
                .collect(),
            // Standalone widgets' names: their tables' `name`, else the
            // tables' names.
            names: (config.app.iter())
                .filter_map(|(id, a)| Some((id.clone(), a.name.clone()?)))
                .chain((config.widget.iter()).map(|(name, w)| {
                    let shown = w.name.clone().unwrap_or_else(|| name.clone());
                    (dock::widget_key(name), shown)
                }))
                .collect(),
            folder_icons: (config.folder.iter())
                .filter_map(|(path, f)| Some((pins::expand(path), f.icon.clone()?)))
                .collect(),
            user_dirs: UserDirs::load(),
            badge: config.badge_style().map(|text| BadgeStyle {
                size: (text::line_height(text.size) + 2) as f64,
                background: theme.badge_background,
                text,
            }),
            ui_theme: Arc::new(config.ui_theme()),
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

    /// The separator's length along the dock (it doesn't magnify).
    fn separator_len(&self) -> f64 {
        (self.icon() / 4.0).round().max(2.0 * self.gap).max(4.0)
    }

    /// The theme icon (or icon path) of the folder at `path`.
    fn folder_icon(&self, path: &Path) -> String {
        let own = (self.folder_icons.iter()).find(|(p, _)| p == path);
        match own {
            Some((_, icon)) => icon.clone(),
            None => self.user_dirs.icon(path).unwrap_or(folder::ICON).into(),
        }
    }
}

/// The subsurfaces of one slot.
struct SlotSubs {
    indicator: Sub,
    icon: Sub,
}

/// The pinned items that have an order among themselves: the zones on
/// either side of the separator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Group {
    Apps,
    Folders,
}

impl Group {
    fn of(item: &Item) -> Option<Self> {
        match item.kind {
            Kind::App | Kind::Widget { tail: false } => Some(Group::Apps),
            Kind::Folder { .. } | Kind::Widget { tail: true } => Some(Group::Folders),
            Kind::Separator | Kind::Trash { .. } => None,
        }
    }

    fn zone(self) -> Zone {
        match self {
            Group::Apps => Zone::Apps,
            Group::Folders => Zone::Folders,
        }
    }

    fn from_zone(zone: Zone) -> Self {
        match zone {
            Zone::Apps => Group::Apps,
            Zone::Folders => Group::Folders,
        }
    }
}

/// An icon being dragged.
struct Drag {
    app_id: String,
    /// The group it is headed for: its own, but a widget's may be the
    /// other one.
    group: Group,
    /// Its own group.
    home: Group,
    /// It is a widget, so it may go to the other group.
    crosses: bool,
    /// Where the pointer is on the surface.
    at: (f64, f64),
    /// From the pointer to the icon's center, as it was picked up.
    offset: (f64, f64),
    /// The icon's size on the surface as it was picked up.
    size: (f64, f64),
    target: Target,
}

/// A gap among the pinned apps or folders where a dragged icon (or
/// folders from outside) would land, opening or closing.
struct Gap {
    group: Group,
    /// Before the slot of this item; None: after the last pinned one.
    before: Option<String>,
    presence: Spring,
    /// The span of what lands there (a widget's tile may be longer).
    span: f64,
}

/// A dropped (or cancelled) icon flying to its slot.
struct Fly {
    app_id: String,
    /// Where it was let go: the icon on the surface.
    from: Rect,
    /// 0 (let go) to 1 (in its slot).
    t: Spring,
}

/// What a drag from outside over the dock may do, from app.rs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Offer {
    /// The items that take it, and the label saying what a drop does.
    pub takers: Vec<(String, String)>,
    /// It holds folders only: the label for pinning them, if they can be.
    pub pin: Option<String>,
}

/// What happened on the menu.
pub enum MenuEvent {
    /// A row of the menu of this item was picked.
    Pick(String, menu::Action),
    /// A file's row was dragged out, by the press with `serial` on
    /// `surface`; start a drag of it, with `icon` (a buffer at `scale`
    /// and its logical size) under the pointer, then `hold_menu`.
    DragOut {
        path: PathBuf,
        serial: u32,
        surface: WlSurface,
        icon: Option<(WlBuffer, f64)>,
        scale: i32,
    },
}

/// A drag from outside over the dock.
struct Incoming {
    /// Where it is on the surface.
    at: (f64, f64),
    /// None until its data is there.
    offer: Option<Offer>,
    target: drag::DropTarget,
}

/// The row as laid out: a cell per slot, and per gap.
struct Laid {
    row: zoom::Row,
    /// Per cell, its slot (None: a gap).
    slots: Vec<Option<usize>>,
    /// Per cell, its gap (None: a slot).
    gaps: Vec<Option<usize>>,
}

impl Laid {
    /// Slot `i`'s cell.
    fn cell(&self, i: usize) -> Option<&zoom::Cell> {
        let k = self.slots.iter().position(|s| *s == Some(i))?;
        self.row.cells.get(k)
    }

    /// Gap `i`'s cell.
    fn gap_cell(&self, i: usize) -> Option<&zoom::Cell> {
        let k = self.gaps.iter().position(|g| *g == Some(i))?;
        self.row.cells.get(k)
    }

    /// The slot whose cell is at `u` (None: none, or a gap).
    fn hit(&self, u: f64) -> Option<usize> {
        self.slots.get(self.row.hit(u)?).copied().flatten()
    }
}

/// The dock while the launcher is open on its output.
struct Launcher {
    /// 0 (the dock) to 1 (the launcher's bar): the icons shrink and fade
    /// into the bar's center. The launcher draws the panel meanwhile.
    morph: Spring,
    /// The app the launcher opened: its icon flies here, so the slot
    /// shows none until the launcher is gone.
    landing: Option<String>,
}

/// What the launcher takes over from the dock, on the dock's surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Handover {
    /// The panel as it is now.
    pub panel: Rect,
    /// The resting panel's length.
    pub rest_length: f64,
    /// The surface's height.
    pub height: f64,
}

pub struct DockView {
    // Field order is drop order: children before their parent surface.
    /// The menu, while open (or closing).
    menu: Option<MenuView>,
    /// A widget's popup, while open (or closing).
    widget: Option<WidgetView>,
    /// The strip, while open (or closing).
    strip: Option<StripView>,
    panel: [Sub; 3],
    /// Per slot, at least; extra ones stay hidden.
    slot_subs: Vec<SlotSubs>,
    /// Per number, a badge's dot and digit, above the icons; made the
    /// first time keyboard activation shows them.
    badge_subs: Vec<(Sub, Sub)>,
    label_sub: Sub,
    /// The dragged (or flying) icon, on top.
    drag_sub: Sub,
    no_input: Region,
    viewport: WpViewport,
    layer: LayerSurface,
    transparent: WlBuffer,

    pub output: WlOutput,
    /// The output's key (see `wl_shell_core::output::Output::key`).
    pub name: String,
    /// The output's connector name, which `output-event-stream` refers to;
    /// None if the compositor says none.
    pub connector: Option<String>,
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
    /// The separator's line.
    separator_shape: Option<Shape>,
    /// A badge's dot.
    badge_shape: Option<Shape>,
    /// The badges' digits, per number.
    badge_texts: Vec<BadgeText>,
    /// The apps launching: their icons bounce.
    launching: Vec<String>,
    /// The input region last set.
    input: Vec<IRect>,

    /// The zoom focus along the dock. It tracks the pointer directly,
    /// except when gliding to another icon clicked under an open popup.
    pointer: Spring,
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
    /// The labels widgets give their apps, by app ID (over the names).
    widget_names: BTreeMap<String, String>,
    /// The trees widgets draw as their apps' icons, by app ID, with
    /// their versions.
    widget_icons: BTreeMap<String, WidgetIcon>,

    frame_pending: bool,
    last_frame: Option<Instant>,

    /// The output's logical size.
    output_size: Option<(f64, f64)>,
    /// Captures of the windows of the hovered app (if it gets a strip) and
    /// of the strip's.
    previews: Vec<Preview>,
    /// Pointer motion doesn't move the zoom focus while a popup is open.
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

    /// The icon being dragged.
    drag: Option<Drag>,
    /// Gaps opening or closing where it would land.
    gaps: Vec<Gap>,
    /// A dropped icon on its way to its slot.
    fly: Option<Fly>,
    /// The target of a drag (the Trash, or what a drop from outside goes
    /// to) grows while it takes it, 0 to 1.
    grow: Spring,
    /// The item growing.
    grow_key: Option<String>,
    /// A drag from outside over the dock.
    incoming: Option<Incoming>,
    /// The open menu is a folder's popup.
    popup: bool,
    /// The launcher is open on this dock's output.
    launcher: Option<Launcher>,
    /// Keyboard activation is on, from this dock: the badges show.
    keys: bool,
    /// The badges' fade, 0 to 1.
    badge_alpha: Spring,
}

impl DockView {
    /// An empty dock on `output` (keyed `name`, with the connector
    /// `connector`) at `scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: &Core<WlDock>,
        output: WlOutput,
        name: String,
        connector: Option<String>,
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
        let drag_sub = sub();

        let response = style.response;
        // An autohiding dock starts out of sight.
        let shown = match style.autohide.enabled {
            true => 0.0,
            false => 1.0,
        };
        let mut dock = Self {
            menu: None,
            widget: None,
            strip: None,
            panel,
            slot_subs: Vec::new(),
            badge_subs: Vec::new(),
            label_sub,
            drag_sub,
            no_input,
            viewport,
            layer,
            transparent,
            output,
            name,
            connector,
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
            separator_shape: None,
            badge_shape: None,
            badge_texts: Vec::new(),
            launching: Vec::new(),
            input: Vec::new(),
            pointer: Spring::new(0.0, response),
            hover: Spring::new(0.0, response),
            hovered: None,
            label_on: false,
            label: None,
            label_alpha: Spring::new(0.0, LABEL_RESPONSE),
            label_at: (0.0, 0.0),
            label_timer: None,
            widget_names: BTreeMap::new(),
            widget_icons: BTreeMap::new(),
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
            drag: None,
            gaps: Vec::new(),
            fly: None,
            grow: Spring::new(0.0, response),
            grow_key: None,
            incoming: None,
            popup: false,
            launcher: None,
            keys: false,
            badge_alpha: Spring::new(0.0, LABEL_RESPONSE),
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
        // So was the widget's popup.
        if style.menu != self.style.menu
            || style.ui_theme != self.style.ui_theme
            || style.layout.position != self.style.layout.position
        {
            self.widget = None;
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
            self.pointer.snap(self.frame().length() / 2.0);
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
        self.sync_drag();
        if let Some((x, y)) = self.incoming.as_ref().map(|i| i.at) {
            self.dnd_motion(x, y);
        }
        let qh = &self.qh;
        let parent = self.layer.wl_surface();
        // Below the badges (if made yet) and the label.
        let above = match self.badge_subs.first() {
            Some((dot, _)) => dot.surface(),
            None => self.label_sub.surface(),
        };
        while self.slot_subs.len() < self.slots.len() {
            let sub = || {
                let sub = Sub::new(&self.g, parent, &self.no_input, qh);
                sub.place_below(above);
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

    /// The icon of an app.
    fn app_icon_key(&self, app_id: &str) -> icons::Key {
        icons::Key::new(
            app_id,
            self.style.icon_key_size(),
            self.style.icon_theme.clone(),
            self.style.icon_shadow,
            self.scale,
        )
        .with_icon(self.style.icons.get(app_id).cloned())
    }

    /// A widget's tile for `item`, `span` long: its tree's version and
    /// its size in buffer pixels, upright on the surface.
    fn tile(&self, item: &Item) -> Option<icons::Tile> {
        let version = self.widget_icons.get(&item.app_id)?.version;
        let thick = self.style.icon_key_size() * self.scale.max(1) as u32;
        let long = (thick as f64 * item.span.get()).round() as u32;
        let (w, h) = match self.frame().axis() {
            shapes::Axis::Horizontal => (long, thick),
            shapes::Axis::Vertical => (thick, long),
        };
        Some(icons::Tile { version, w, h })
    }

    /// What widgets' icons are drawn for on this dock, but their spans.
    pub fn tile_base(&self) -> TileBase {
        TileBase {
            size: self.style.icon_key_size() as f64,
            axis: match self.frame().axis() {
                shapes::Axis::Horizontal => wl_shell_lua::Axis::Horizontal,
                shapes::Axis::Vertical => wl_shell_lua::Axis::Vertical,
            },
            scale: self.scale,
        }
    }

    /// What paints `key`'s tile, if it is one.
    fn tile_request(&self, key: &icons::Key) -> Option<ui::Request> {
        let tile = key.tile?;
        let tree = &self.widget_icons.get(&key.app_id)?.tree;
        let s = key.scale.max(1) as f32;
        Some(ui::Request {
            // A standalone widget has no app of its own.
            app: dock::widget_name(&key.app_id)
                .is_none()
                .then(|| key.app_id.clone()),
            ..ui::Request::new(
                tree.clone(),
                ui::Bounds::Exact(tile.w as f32 / s, tile.h as f32 / s),
                key.scale,
                self.style.ui_theme.clone(),
            )
        })
    }

    /// Ask the worker for `key`'s image: a widget's tile, or an icon.
    fn request_icon(&self, key: &icons::Key) {
        match self.tile_request(key) {
            Some(request) => self.assets.ui_icon(key, &request),
            None => self.assets.icon(key, &self.style.font),
        }
    }

    /// The trees widgets draw for their apps now (with their versions).
    pub fn set_widget_icons(&mut self, icons: &BTreeMap<String, WidgetIcon>) {
        if *icons == self.widget_icons {
            return;
        }
        self.widget_icons = icons.clone();
        self.request_assets(false);
        self.kick();
    }

    /// The icon of `item`; None for the separator.
    fn icon_key(&self, item: &Item) -> Option<icons::Key> {
        match item.kind {
            Kind::App => Some(self.app_icon_key(&item.app_id).with_tile(self.tile(item))),
            // Its tree once the plugin draws one, a placeholder before
            // (or while it fails).
            Kind::Widget { .. } => {
                let key = self.app_icon_key(&item.app_id).with_tile(self.tile(item));
                Some(key.with_icon(Some(WIDGET_ICON.into())))
            }
            Kind::Separator => None,
            // Named after the icon, so a missing one gets a letter tile.
            Kind::Trash { full } => {
                let key = self.app_icon_key(trash::icon(full));
                Some(key.with_icon(Some(trash::icon(full).into())))
            }
            // Named `folder`, so a theme without the folder's icon falls
            // back to it (then to a letter tile).
            Kind::Folder { .. } => {
                let path = pins::expand(item.folder()?);
                let key = icons::Key::new(
                    folder::ICON,
                    self.style.icon_key_size(),
                    self.style.icon_theme.clone(),
                    self.style.icon_shadow,
                    self.scale,
                );
                Some(key.with_icon(Some(self.style.folder_icon(&path))))
            }
        }
    }

    /// The labels widgets give their apps now.
    pub fn set_widget_names(&mut self, names: &BTreeMap<String, String>) {
        if *names == self.widget_names {
            return;
        }
        let changed: Vec<String> = (names.iter())
            .filter(|(app, name)| self.widget_names.get(*app) != Some(name))
            .map(|(app, _)| app.clone())
            .chain(
                (self.widget_names.keys())
                    .filter(|app| !names.contains_key(*app))
                    .cloned(),
            )
            .collect();
        self.widget_names = names.clone();
        // Have the new labels ready for a hover.
        let cache = self.cache.borrow();
        for app in &changed {
            let shown = (self.slots.iter()).any(|s| !s.leaving && s.item.app_id == *app);
            if let Some(key) = self.label_key(app).filter(|_| shown)
                && cache.label(&key).is_none()
            {
                self.assets.label(&key);
            }
        }
        drop(cache);
        if (self.hovered.as_ref()).is_some_and(|h| changed.contains(h)) {
            self.update_label();
        }
    }

    fn label_key(&self, app_id: &str) -> Option<LabelKey> {
        let name = match app_id {
            dock::SEPARATOR => return None,
            dock::TRASH => Some("Trash".to_string()),
            _ => match dock::folder_pin(app_id) {
                Some(pin) => Some(folder::name(pin, &pins::expand(pin))),
                None => (self.widget_names.get(app_id))
                    .or_else(|| self.style.names.get(app_id))
                    .cloned(),
            },
        };
        Some(LabelKey {
            app_id: app_id.to_string(),
            name,
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
            if let Some(key) = self.icon_key(&slot.item)
                && (all || cache.icon(&key).is_none())
            {
                self.request_icon(&key);
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
        if (self.slots.iter()).any(|s| self.icon_key(&s.item).as_ref() == Some(key)) {
            self.kick();
        }
    }

    /// The worker rendered a label (now in the cache).
    pub fn label_ready(&mut self, key: &LabelKey) {
        if self.wanted_label().as_ref() == Some(key) {
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
        // One pixel, stretched into a line.
        let line = ShapeKey::Rect {
            w: 1,
            h: 1,
            radius: 0,
            color: self.style.separator_color,
            scale,
        };
        self.separator_shape = cache.shape(&self.g, line, &self.qh);
        self.badge_shape = (self.style.badge.as_ref()).and_then(|b| {
            let dot = ShapeKey::Dots {
                count: 1,
                size: b.size as i32,
                color: b.background,
                scale,
            };
            cache.shape(&self.g, dot, &self.qh)
        });
        drop(cache);
        if self.keys || !self.badge_texts.is_empty() {
            self.request_badges();
        }
    }

    // -- keyboard activation -------------------------------------------------

    /// Ask the worker for the badges' digits not rendered for the style
    /// and scale yet. A badge shows its old digit until the new one is
    /// there.
    fn request_badges(&mut self) {
        let Some(style) = self.style.badge.clone() else {
            self.badge_texts.clear();
            return;
        };
        for n in 1..=activate::MAX {
            let request = PillRequest {
                text: activate::label(n),
                spans: Vec::new(),
                max_width: (2.0 * style.size).ceil() as i32,
                scale: self.scale,
                style: style.text.clone(),
            };
            match self.badge_texts.get_mut(n - 1) {
                Some(t) if t.request == request => {}
                Some(t) => {
                    t.tag = next_tag();
                    t.request = request;
                    self.assets.line(t.tag, t.request.clone());
                }
                None => {
                    let tag = next_tag();
                    self.assets.line(tag, request.clone());
                    self.badge_texts.push(BadgeText {
                        tag,
                        request,
                        pill: None,
                    });
                }
            }
        }
    }

    /// Keyboard activation began on this dock: number the items, and come
    /// out if hidden.
    pub fn begin_keys(&mut self) {
        self.keys = true;
        if self.badge_subs.is_empty() {
            // Above the icons, below the label.
            let parent = self.layer.wl_surface();
            for _ in 0..activate::MAX {
                let sub = || {
                    let sub = Sub::new(&self.g, parent, &self.no_input, &self.qh);
                    sub.place_below(self.label_sub.surface());
                    sub
                };
                let dot = sub();
                let digit = sub();
                self.badge_subs.push((dot, digit));
            }
        }
        self.request_badges();
        self.badge_alpha.retarget(1.0);
        self.cancel_autohide_timer();
        self.set_shown(true);
        self.kick();
    }

    /// Keyboard activation ended: the badges go, and so does the
    /// magnification unless the pointer (or a popup) holds it.
    pub fn end_keys(&mut self, core: &Core<WlDock>) {
        if !std::mem::replace(&mut self.keys, false) {
            return;
        }
        self.badge_alpha.retarget(0.0);
        if !self.pointer_in && !self.strip_open() && !self.menu_open() {
            self.unhover(core);
        }
        self.sync_autohide();
        self.kick();
    }

    /// Magnify the item `key` as if the pointer were on it.
    pub fn point_at(&mut self, core: &Core<WlDock>, key: &str) {
        if !self.configured || self.drag.is_some() || self.launcher.is_some() {
            return;
        }
        let Some(center) = self.resting_center(key) else {
            return;
        };
        self.frozen = false;
        self.pointer.snap(center);
        self.hover.retarget(1.0);
        self.set_hovered(core, Some(key.to_string()));
        self.kick();
    }

    /// A click on a different icon under a popup moves the zoom focus to
    /// that icon, without changing the hit test that picked it.
    pub fn transfer_zoom(&mut self, key: &str) {
        let popup = self.strip_app().or_else(|| self.menu_app());
        if popup.is_some_and(|app| app != key)
            && let Some(center) = self.resting_center(key)
        {
            self.pointer.retarget(center);
            self.hover.retarget(1.0);
            self.kick();
        }
    }

    fn resting_center(&self, key: &str) -> Option<f64> {
        let i = self
            .slots
            .iter()
            .position(|s| !s.leaving && s.item.app_id == key)?;
        // At rest, the layout doesn't depend on the pointer.
        self.lay(self.pointer.target, 0.0, true)
            .cell(i)
            .map(|c| c.center)
    }

    /// The open strip's highlighted window.
    pub fn strip_highlighted(&self) -> Option<&str> {
        let strip = self.strip.as_ref().filter(|s| !s.closing())?;
        strip.highlighted()
    }

    /// Highlight the open strip's card of window `id`.
    pub fn strip_highlight(&mut self, id: &str) {
        if let Some(strip) = self.strip.as_mut().filter(|s| !s.closing()) {
            strip.highlight_window(id);
            self.kick();
        }
    }

    /// Highlight the open menu's (or folder popup's) next row (the
    /// previous one, `back`).
    pub fn menu_step(&mut self, back: bool) {
        if let Some(menu) = self.menu.as_mut().filter(|m| !m.closing()) {
            menu.step(back);
            self.kick();
        }
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
        // The launcher covers the output; nothing magnifies meanwhile.
        if !self.showing() || self.launcher.is_some() {
            return;
        }
        if self.drag.is_some() {
            return self.drag_motion(x, y);
        }
        let (u, _) = self.frame().dock_point(x, y);
        // With a popup open, moving the pointer can pick another icon
        // without moving the zoom focus.
        let at = if self.frozen { self.pointer.value } else { u };
        let hovered = self.item_at(u, at).map(|item| item.app_id.clone());
        if !self.frozen {
            self.pointer.snap(u);
        }
        self.hover.retarget(1.0);
        self.set_hovered(core, hovered);
        self.kick();
    }

    /// The icon at a button event's position. The row can move during a
    /// zoom transfer even when no motion event updates the hovered item.
    pub fn click_item(&self, x: f64, y: f64) -> Option<&Item> {
        if !self.configured || !self.showing() || self.launcher.is_some() {
            return None;
        }
        let (u, _) = self.frame().dock_point(x, y);
        self.item_at(u, self.pointer.value)
    }

    fn item_at(&self, u: f64, focus: f64) -> Option<&Item> {
        let laid = self.lay(focus, self.hover.value.clamp(0.0, 1.0), false);
        let slot = &self.slots[laid.hit(u)?];
        (!slot.leaving && slot.item.kind != Kind::Separator).then_some(&slot.item)
    }

    /// The pointer left the dock. With the strip or menu open, the
    /// magnification stays until it closes.
    pub fn pointer_leave(&mut self, core: &Core<WlDock>) {
        self.pointer_in = false;
        self.pointer_at = None;
        self.sync_autohide();
        if self.strip_open() || self.menu_open() || self.widget_open() {
            self.set_hovered(core, None);
            return;
        }
        self.unhover(core);
    }

    // -- dragging --------------------------------------------------------------

    /// Whether an icon is being dragged.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The app whose icon is dragged or flying back: its slot shows no
    /// icon of its own.
    fn lifted(&self) -> Option<&str> {
        (self.drag.as_ref().map(|d| d.app_id.as_str()))
            .or(self.fly.as_ref().map(|f| f.app_id.as_str()))
    }

    /// Where `app_id` is in the dock's order, for drag.rs.
    fn dragged(&self, app_id: &str) -> Option<drag::Dragged> {
        let slot = (self.slots.iter()).find(|s| !s.leaving && s.item.app_id == app_id)?;
        let group = Group::of(&slot.item)?;
        if !slot.item.pinned {
            return Some(drag::Dragged::Running);
        }
        let i = (self.pinned_slots(group, None)).position(|(_, s)| s.item.app_id == app_id)?;
        Some(drag::Dragged::Pinned(i))
    }

    /// The pinned slots of `group` (and their indices) in order, but
    /// `except`.
    fn pinned_slots<'a>(
        &'a self,
        group: Group,
        except: Option<&'a str>,
    ) -> impl Iterator<Item = (usize, &'a Slot)> + 'a {
        (self.slots.iter().enumerate()).filter(move |(_, s)| {
            !s.leaving
                && s.item.pinned
                && Group::of(&s.item) == Some(group)
                && Some(s.item.app_id.as_str()) != except
        })
    }

    /// Where a gap of `group` anchored `before` a slot goes among the
    /// slots.
    fn gap_index(&self, group: Group, before: Option<&str>) -> usize {
        if let Some(key) = before
            && let Some(i) = (self.slots.iter()).position(|s| !s.leaving && s.item.app_id == key)
        {
            return i;
        }
        // After the last pinned one (other than the lifted one).
        if let Some((i, _)) = self.pinned_slots(group, self.lifted()).last() {
            return i + 1;
        }
        match group {
            Group::Apps => 0,
            // The folders' place: after the separator, else before the
            // Trash, else at the end.
            Group::Folders => {
                let at = |kind: fn(&Item) -> bool| {
                    (self.slots.iter()).position(|s| !s.leaving && kind(&s.item))
                };
                at(|i| i.kind == Kind::Separator)
                    .map(|i| i + 1)
                    .or_else(|| at(Item::is_trash))
                    .unwrap_or(self.slots.len())
            }
        }
    }

    /// Lay out the slots and gaps with the pointer at `pointer` and the
    /// magnification at `hover`, with every spring as it is, or where it
    /// is headed if `targets`.
    fn lay(&self, pointer: f64, hover: f64, targets: bool) -> Laid {
        let weight = |s: &Spring| match targets {
            true => s.target,
            false => s.value,
        };
        let gaps: Vec<(usize, f64, f64)> = (self.gaps.iter())
            .map(|g| {
                (
                    self.gap_index(g.group, g.before.as_deref()),
                    weight(&g.presence),
                    g.span,
                )
            })
            .collect();
        let separator = self.style.separator_len();
        let n = self.slots.len();
        let mut slots = Vec::with_capacity(n + gaps.len());
        let mut cells = Vec::with_capacity(n + gaps.len());
        let mut gap_cells = Vec::with_capacity(n + gaps.len());
        for i in 0..=n {
            for (k, &(at, w, span)) in gaps.iter().enumerate() {
                if at.min(n) == i {
                    slots.push(zoom::Slot::wide(span, w.clamp(0.0, 1.0)));
                    cells.push(None);
                    gap_cells.push(Some(k));
                }
            }
            if let Some(s) = self.slots.get(i) {
                let w = weight(&s.presence).clamp(0.0, 1.0);
                slots.push(match s.item.kind {
                    Kind::Separator => zoom::Slot::fixed(separator, w),
                    _ => zoom::Slot::wide(weight(&s.span), w),
                });
                cells.push(Some(i));
                gap_cells.push(None);
            }
        }
        let center = self.frame().length() / 2.0;
        let row = zoom::layout(
            &slots,
            center,
            pointer,
            &self.style.zoom(hover.clamp(0.0, 1.0)),
        );
        Laid {
            row,
            slots: cells,
            gaps: gap_cells,
        }
    }

    /// Whether the point (`u`, `v`) along and across the dock is over it,
    /// as laid out.
    fn on_dock(&self, laid: &Laid, u: f64, v: f64) -> bool {
        let m = self.style.metrics();
        let step = m.icon + self.style.gap;
        v <= m.icon_v() + laid.row.largest().max(m.icon) + LABEL_GAP
            && u >= laid.row.start - step / 2.0
            && u <= laid.row.end + step / 2.0
    }

    /// A press on `app_id`'s icon moved far enough: pick the icon up, with
    /// the pointer at (`x`, `y`). Returns whether it was.
    pub fn start_drag(&mut self, core: &Core<WlDock>, app_id: &str, x: f64, y: f64) -> bool {
        let item = (self.slots.iter())
            .find(|s| !s.leaving && s.item.app_id == app_id && s.item.draggable())
            .map(|s| &s.item);
        let Some(group) = item.and_then(Group::of) else {
            return false;
        };
        if !self.configured || self.drag.is_some() || self.incoming.is_some() {
            return false;
        }
        let Some(i) = self.slots.iter().position(|s| s.item.app_id == app_id) else {
            return false;
        };
        let laid = self.lay(self.pointer.value, self.hover.value, false);
        let Some(cell) = laid.cell(i) else {
            return false;
        };
        let m = self.style.metrics();
        let size = cell.size.max(m.icon);
        let len = cell.len.max(size);
        let icon = self
            .frame()
            .rect(cell.center - len / 2.0, m.icon_v(), len, size);
        let (cx, cy) = icon.center();
        // A flight still on its way lands at once.
        self.fly = None;
        let crosses = item.is_some_and(Item::is_widget);
        self.drag = Some(Drag {
            app_id: app_id.to_string(),
            group,
            home: group,
            crosses,
            at: (x, y),
            offset: (cx - x, cy - y),
            size: (icon.w, icon.h),
            target: Target::Home,
        });
        // No label during a drag (but the Trash's).
        self.cancel_label_timer();
        self.label_on = false;
        self.set_hovered(core, None);
        self.sync_autohide();
        self.drag_motion(x, y);
        true
    }

    /// The pointer moved to (`x`, `y`) while dragging.
    fn drag_motion(&mut self, x: f64, y: f64) {
        let Some((app_id, home, crosses, was)) =
            (self.drag.as_ref()).map(|d| (d.app_id.clone(), d.home, d.crosses, d.group))
        else {
            return;
        };
        let frame = self.frame();
        let m = self.style.metrics();
        let (u, v) = frame.dock_point(x, y);
        // As it will be drawn, if the pointer is on the dock.
        let laid = self.lay(u, self.hover.value, false);
        let step = m.icon + self.style.gap;
        let on_dock = self.on_dock(&laid, u, v);
        // A widget goes to the side of the separator the pointer is on.
        let group = match crosses {
            true => Group::from_zone(drag::zone(u, self.separator_center(&laid))),
            false => home,
        };
        let dragged = match group == home {
            true => self.dragged(&app_id),
            false => Some(drag::Dragged::Across),
        };
        let Some(dragged) = dragged else {
            return self.cancel_drag();
        };
        // The other pinned items' centers of its group, and where they
        // (and the gap among them) end.
        let mut pinned = Vec::new();
        let mut pinned_end = None;
        for (cell, slot) in laid.row.cells.iter().zip(&laid.slots) {
            let counts = match slot {
                None => true,
                Some(i) => {
                    let s = &self.slots[*i];
                    let other = !s.leaving && s.item.pinned && Group::of(&s.item) == Some(group);
                    let other = other && s.item.app_id != app_id;
                    if other {
                        pinned.push(cell.center);
                    }
                    other
                }
            };
            if counts && cell.end > cell.start {
                pinned_end = Some(cell.end);
            }
        }
        let on_trash = (laid.hit(u)).is_some_and(|i| self.slots[i].item.is_trash());
        let scene = drag::Scene {
            pinned: &pinned,
            pinned_end: pinned_end.unwrap_or(laid.row.start),
            step,
            on_dock,
            on_trash,
        };
        let target = drag::target(dragged, &scene, u);
        // Magnified over the dock, at rest off it.
        if on_dock {
            self.pointer.snap(u);
            self.hover.retarget(1.0);
        } else {
            self.hover.retarget(0.0);
        }
        let changed = self
            .drag
            .as_ref()
            .is_some_and(|d| d.target != target || was != group);
        if let Some(drag) = &mut self.drag {
            drag.group = group;
            drag.at = (x, y);
            drag.target = target;
        }
        if changed {
            self.set_gap(match target {
                Target::Gap(k) => Some((group, self.nth_pinned(group, k, Some(&app_id)))),
                Target::Home | Target::Trash => None,
            });
            self.set_grow(match target {
                Target::Trash => Some(dock::TRASH.to_string()),
                _ => None,
            });
            self.update_label();
        }
        self.sync_drag_slot();
        self.kick();
    }

    /// The key of the `k`th pinned item of `group` but `except` (None:
    /// past the last), which a gap before it is anchored to.
    fn nth_pinned(&self, group: Group, k: usize, except: Option<&str>) -> Option<String> {
        (self.pinned_slots(group, except))
            .nth(k)
            .map(|(_, s)| s.item.app_id.clone())
    }

    /// Where the separator's middle is, as `laid`; None if there is none.
    fn separator_center(&self, laid: &Laid) -> Option<f64> {
        (laid.row.cells.iter().zip(&laid.slots))
            .find(|(_, slot)| {
                slot.is_some_and(|i| {
                    let s = &self.slots[i];
                    !s.leaving && s.item.kind == Kind::Separator
                })
            })
            .map(|(cell, _)| cell.center)
    }

    /// Make `key` the item that grows (None: none).
    fn set_grow(&mut self, key: Option<String>) {
        match key {
            Some(key) => {
                if self.grow_key.as_ref() != Some(&key) {
                    // Another target: it grows from its rest.
                    self.grow.snap(0.0);
                    self.grow_key = Some(key);
                }
                self.grow.retarget(1.0);
            }
            // The last one shrinks back.
            None => self.grow.retarget(0.0),
        }
    }

    /// Open a gap of a group before an item (None: after the group's
    /// pinned items), or none at all; the others close.
    fn set_gap(&mut self, gap: Option<(Group, Option<String>)>) {
        for g in &mut self.gaps {
            let wanted = gap.as_ref() == Some(&(g.group, g.before.clone()));
            g.presence.retarget(if wanted { 1.0 } else { 0.0 });
        }
        if let Some((group, before)) = gap
            && !(self.gaps.iter()).any(|g| g.group == group && g.before == before)
        {
            let mut presence = Spring::new(0.0, self.style.response);
            presence.retarget(1.0);
            // As long as the dragged item.
            let span = (self.drag.as_ref())
                .and_then(|d| self.slots.iter().find(|s| s.item.app_id == d.app_id))
                .map_or(1.0, |s| s.item.span.get());
            self.gaps.push(Gap {
                group,
                before,
                presence,
                span,
            });
        }
    }

    /// The dragged icon's slot is open at home, else closed.
    fn sync_drag_slot(&mut self) {
        let Some(drag) = &self.drag else { return };
        let open = drag.target == Target::Home;
        for s in self
            .slots
            .iter_mut()
            .filter(|s| s.item.app_id == drag.app_id)
        {
            if !s.leaving {
                s.presence.retarget(if open { 1.0 } else { 0.0 });
            }
        }
    }

    /// The items changed: keep the dragged slot closed, or cancel the
    /// drag if its app went; a flight to a slot that went lands.
    fn sync_drag(&mut self) {
        let present = |slots: &[Slot], app_id: &str| {
            (slots.iter()).any(|s| !s.leaving && s.item.app_id == app_id)
        };
        if let Some(drag) = &self.drag
            && !present(&self.slots, &drag.app_id)
        {
            self.cancel_drag();
        }
        if let Some(fly) = &self.fly
            && !present(&self.slots, &fly.app_id)
        {
            self.fly = None;
        }
        self.sync_drag_slot();
    }

    /// Escape, or the pointer left: the icon flies back home.
    pub fn cancel_drag(&mut self) {
        if let Some(drag) = &mut self.drag {
            drag.target = Target::Home;
            self.end_drag();
        }
    }

    /// The button came up: drop the icon. Returns the app, what the drop
    /// does to the pins, which the caller applies (and rebuilds the
    /// items) right away, and the zone it goes to (a widget's may be the
    /// other one).
    pub fn end_drag(&mut self) -> Option<(String, drag::Drop, Zone)> {
        let drag = self.drag.take()?;
        let dragged = match drag.group == drag.home {
            true => self.dragged(&drag.app_id),
            false => Some(drag::Dragged::Across),
        };
        let drop = dragged.map_or(drag::Drop::Nothing, |d| drag::drop(d, drag.target));
        let from = self.drag_rect(&drag);
        // Landing in the gap: its spring becomes the slot's, which the
        // new order moves there.
        let open = (self.gaps.iter()).position(|g| g.presence.target > 0.5);
        let slot = (self.slots.iter_mut()).find(|s| !s.leaving && s.item.app_id == drag.app_id);
        match (drop, open, slot) {
            (drag::Drop::Pin(_), Some(k), Some(slot)) => {
                slot.presence = self.gaps.remove(k).presence;
                slot.presence.retarget(1.0);
            }
            (drag::Drop::Unpin, _, _) => {}
            (_, _, Some(slot)) => slot.presence.retarget(1.0),
            (_, _, None) => {}
        }
        for g in &mut self.gaps {
            g.presence.retarget(0.0);
        }
        // Into the Trash it just goes; anywhere else it flies to its slot.
        if drop != drag::Drop::Unpin {
            let mut t = Spring::new(0.0, self.style.response);
            t.retarget(1.0);
            self.fly = Some(Fly {
                app_id: drag.app_id.clone(),
                from,
                t,
            });
        }
        self.set_grow(None);
        self.sync_autohide();
        self.update_label();
        self.kick();
        Some((drag.app_id, drop, drag.group.zone()))
    }

    /// Where the dragged icon is drawn: under the pointer as it was picked
    /// up, kept on the surface.
    fn drag_rect(&self, drag: &Drag) -> Rect {
        let (w, h) = drag.size;
        let keep = |c: f64, len: f64, half: f64| c.min(len - half).max(half);
        let cx = keep(drag.at.0 + drag.offset.0, self.width as f64, w / 2.0);
        let cy = keep(drag.at.1 + drag.offset.1, self.height as f64, h / 2.0);
        Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
    }

    // -- drops from outside ----------------------------------------------------

    /// A drag from outside entered the dock at (`x`, `y`); what it may do
    /// follows with `set_offer` once its data is read.
    pub fn dnd_enter(&mut self, core: &Core<WlDock>, x: f64, y: f64) {
        self.incoming = Some(Incoming {
            at: (x, y),
            offer: None,
            target: drag::DropTarget::Nothing,
        });
        self.cancel_label_timer();
        self.label_on = false;
        self.set_hovered(core, None);
        self.sync_autohide();
        self.dnd_motion(x, y);
    }

    /// What the drag from outside may do (None: nothing).
    pub fn set_offer(&mut self, offer: Option<Offer>) {
        let Some(incoming) = &mut self.incoming else {
            return;
        };
        if incoming.offer == offer {
            return;
        }
        incoming.offer = offer;
        let (x, y) = incoming.at;
        self.dnd_motion(x, y);
        // The label may say something else now.
        self.update_label();
    }

    /// What dropping the drag from outside would do now.
    pub fn incoming_target(&self) -> Option<&drag::DropTarget> {
        self.incoming.as_ref().map(|i| &i.target)
    }

    /// The drag from outside moved to (`x`, `y`).
    pub fn dnd_motion(&mut self, x: f64, y: f64) {
        let Some(incoming) = &mut self.incoming else {
            return;
        };
        incoming.at = (x, y);
        let offer = incoming.offer.clone();
        if !self.configured {
            return;
        }
        let (u, v) = self.frame().dock_point(x, y);
        let laid = self.lay(u, self.hover.value, false);
        // Hidden, it only brings the dock back (after the delay).
        let on_dock = self.showing() && self.on_dock(&laid, u, v);
        let target = match &offer {
            Some(offer) => self.drop_target(&laid, offer, on_dock, u),
            None => drag::DropTarget::Nothing,
        };
        if on_dock {
            self.pointer.snap(u);
            self.hover.retarget(1.0);
        } else {
            self.hover.retarget(0.0);
        }
        let changed = self.incoming.as_ref().is_some_and(|i| i.target != target);
        if changed {
            self.set_gap(match target {
                drag::DropTarget::Pin(k) => {
                    Some((Group::Folders, self.nth_pinned(Group::Folders, k, None)))
                }
                _ => None,
            });
            self.set_grow(match &target {
                drag::DropTarget::Open(key) | drag::DropTarget::Into(key) => Some(key.clone()),
                drag::DropTarget::Trash => Some(dock::TRASH.to_string()),
                drag::DropTarget::Pin(_) | drag::DropTarget::Nothing => None,
            });
            if let Some(incoming) = &mut self.incoming {
                incoming.target = target;
            }
            self.update_label();
        }
        self.kick();
    }

    /// What a drop with `offer` at `u` would do, on the dock as `laid`.
    fn drop_target(&self, laid: &Laid, offer: &Offer, on_dock: bool, u: f64) -> drag::DropTarget {
        let hit = laid.hit(u).and_then(|i| {
            let s = &self.slots[i];
            let cell = laid.cell(i)?;
            let kind = match s.item.kind {
                // A widget takes nothing (it isn't among the takers).
                Kind::App | Kind::Widget { .. } => drag::HitKind::App,
                Kind::Folder { .. } => drag::HitKind::Folder,
                Kind::Trash { .. } => drag::HitKind::Trash,
                Kind::Separator => drag::HitKind::Separator,
            };
            let len = (cell.end - cell.start).max(1e-6);
            Some(drag::Hit {
                key: &s.item.app_id,
                kind,
                takes: !s.leaving && offer.takers.iter().any(|(k, _)| *k == s.item.app_id),
                at: ((u - cell.start) / len).clamp(0.0, 1.0),
            })
        });
        let mut folders = Vec::new();
        let mut zone = None;
        for (cell, slot) in laid.row.cells.iter().zip(&laid.slots) {
            let Some(s) = slot.map(|i| &self.slots[i]) else {
                continue;
            };
            // The folders' zone holds its widgets too, for where pinned
            // folders go.
            if !s.leaving && s.item.pinned && Group::of(&s.item) == Some(Group::Folders) {
                folders.push(cell.center);
            } else if matches!(s.item.kind, Kind::Separator | Kind::Trash { .. }) && zone.is_none()
            {
                zone = Some(cell.start);
            }
        }
        let step = self.style.icon() + self.style.gap;
        let scene = drag::DropScene {
            on_dock,
            hit,
            folders: &folders,
            zone: zone.unwrap_or(laid.row.end - step / 2.0),
            pinnable: offer.pin.is_some(),
        };
        drag::drop_target(&scene, u)
    }

    /// The drag from outside left (or was dropped and handled).
    pub fn dnd_leave(&mut self, core: &Core<WlDock>) {
        if self.incoming.take().is_none() {
            return;
        }
        self.set_gap(None);
        self.set_grow(None);
        if !self.pointer_in {
            self.unhover(core);
        }
        self.sync_autohide();
        self.update_label();
        self.kick();
    }

    /// The label for the drag from outside, if its target takes it.
    fn incoming_label(&self, incoming: &Incoming) -> Option<LabelKey> {
        let offer = incoming.offer.as_ref()?;
        let (key, name) = match &incoming.target {
            drag::DropTarget::Nothing => return None,
            drag::DropTarget::Pin(_) => (GAP_LABEL, offer.pin.clone()?),
            drag::DropTarget::Open(key) | drag::DropTarget::Into(key) => {
                let label = offer.takers.iter().find(|(k, _)| k == key)?;
                (key.as_str(), label.1.clone())
            }
            drag::DropTarget::Trash => {
                let label = offer.takers.iter().find(|(k, _)| k == dock::TRASH)?;
                (dock::TRASH, label.1.clone())
            }
        };
        Some(LabelKey {
            app_id: key.to_string(),
            name: Some(name),
            ..self.label_key(dock::TRASH)?
        })
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
        let held = self.strip_open()
            || self.menu_open()
            || self.widget_open()
            || self.drag.is_some()
            || self.launcher.is_some()
            || self.keys;
        // A drag from outside counts as the pointer.
        let pointer_in = self.pointer_in || self.incoming.is_some();
        let next = autohide::next(&self.style.autohide, self.showing(), pointer_in, held);
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
        if show && let Some((x, y)) = self.incoming.as_ref().map(|i| i.at) {
            // Back under a drag from outside: it can drop now.
            self.dnd_motion(x, y);
        } else if show && let Some((x, y)) = self.pointer_at {
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

    /// The label that should show: the hovered app's once the delay
    /// passed, or while dragging, what a drop does.
    fn wanted_label(&self) -> Option<LabelKey> {
        if self.launcher.is_some() {
            return None;
        }
        if let Some(drag) = &self.drag {
            let key = self.label_key(dock::TRASH)?;
            return (drag.target == Target::Trash).then(|| LabelKey {
                name: Some(REMOVE_LABEL.into()),
                ..key
            });
        }
        if let Some(incoming) = &self.incoming {
            return self.incoming_label(incoming);
        }
        // The strip replaces its app's label; no label shows over a menu.
        let stripped = |a: &str| (self.strip.as_ref()).is_some_and(|s| s.app_id == a);
        (self.hovered.as_deref())
            .filter(|a| self.label_on && !stripped(a) && self.menu.is_none())
            .and_then(|a| self.label_key(a))
    }

    /// Show the label that should show, or fade it out.
    fn update_label(&mut self) {
        let Some(key) = self.wanted_label() else {
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
            || self.widget.as_ref().is_some_and(WidgetView::moving)
            || !self.pointer.settled()
            || !self.hover.settled()
            || !self.shown.settled()
            || !self.label_alpha.settled()
            || self
                .slots
                .iter()
                .any(|s| !s.presence.settled() || s.bounce.is_some())
            || self.gaps.iter().any(|g| !g.presence.settled())
            || self.fly.is_some()
            || !self.grow.settled()
            || (self.launcher.as_ref()).is_some_and(|l| !l.morph.settled())
            || !self.badge_alpha.settled()
    }

    /// The frame callback: advance the springs and draw.
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        let now = Instant::now();
        let dt = (self.last_frame).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let mut moving = self.hover.advance(dt);
        moving |= self.pointer.advance(dt);
        moving |= self.label_alpha.advance(dt);
        moving |= self.shown.advance(dt);
        moving |= row::advance(&mut self.slots, dt);
        moving |= self.grow.advance(dt);
        moving |= self.badge_alpha.advance(dt);
        if let Some(l) = &mut self.launcher {
            moving |= l.morph.advance(dt);
        }
        for g in &mut self.gaps {
            moving |= g.presence.advance(dt);
        }
        // Closed gaps go.
        (self.gaps).retain(|g| !(g.presence.target == 0.0 && g.presence.settled()));
        if let Some(fly) = &mut self.fly {
            moving |= fly.t.advance(dt);
            if fly.t.settled() {
                self.fly = None;
            }
        }
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
        if let Some(w) = &mut self.widget {
            moving |= w.advance(dt);
            if w.gone() {
                self.widget = None;
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
        if let Some(w) = &mut self.widget {
            w.render();
        }
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

    fn place_all(&mut self) {
        let frame = self.frame();
        let m = self.style.metrics();
        let scale = self.scale as f64;
        let hover = self.hover.value.clamp(0.0, 1.0);
        let laid = self.lay(self.pointer.value, hover, false);
        let row = &laid.row;
        let present: f64 = (self.slots.iter())
            .map(|s| s.presence.value.clamp(0.0, 1.0))
            .sum();
        // Autohide slides everything toward the edge, and once it is all
        // out of sight, clears it.
        let slide = autohide::slide(self.shown.value, self.style.reach());
        let seen = if self.shown.value > 0.0 { 1.0 } else { 0.0 };

        // The panel; the launcher draws it while it is open.
        let (panel, panel_u, length) = self.panel_at(row, slide);
        let mid = panel_u + length / 2.0;
        let panel_alpha = match self.launcher {
            Some(_) => 0.0,
            None => self.gate(present.min(1.0)) * seen,
        };
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
        // Into the launcher's bar: the icons shrink into its center.
        let morph = (self.launcher.as_ref()).map_or(0.0, |l| l.morph.value.clamp(0.0, 1.0));
        let landing_app = (self.launcher.as_ref()).and_then(|l| l.landing.clone());
        let into = |u: f64| lerp(u, mid, morph);

        // Icons rise from the panel's inner edge; indicators sit between
        // them and the edge.
        let key_size = self.style.icon_key_size() as f64;
        let transform = match frame.axis() {
            shapes::Axis::Horizontal => Transform::Normal,
            shapes::Axis::Vertical => Transform::_90,
        };
        let keys: Vec<_> = (self.slots.iter())
            .map(|s| self.icon_key(&s.item))
            .collect();
        let lifted = self.lifted().map(str::to_string);
        // Where the flying icon's slot has its icon.
        let mut landing = None;
        // Each slot's icon as drawn, for the badges.
        let mut icons: Vec<Option<Rect>> = vec![None; self.slots.len()];
        let cache = self.cache.borrow();
        for (i, subs) in self.slot_subs.iter_mut().enumerate() {
            let (Some(slot), Some(cell)) = (self.slots.get(i), laid.cell(i)) else {
                subs.icon.clear();
                subs.indicator.clear();
                continue;
            };
            let weight = slot.presence.value.clamp(0.0, 1.0) * (1.0 - morph);
            // As `gate`, which can't borrow all of `self` here.
            let gated = match self.g.alpha_modifier {
                Some(_) => weight,
                None if weight >= 0.5 => 1.0,
                None => 0.0,
            };
            let item = &slot.item;
            if item.kind == Kind::Separator {
                // A line across the panel, where the icons are.
                let r = frame.rect(into(cell.center) - 0.5, m.icon_v() - slide, 1.0, m.icon);
                let shape = self.separator_shape.as_ref().map(|s| &s.buffer);
                subs.icon.present(shape, r, gated * seen);
                subs.indicator.clear();
                continue;
            }
            // The Trash grows while it takes the dragged icon.
            // The target of a drag grows while it takes it.
            let grow = match self.grow_key.as_deref() == Some(item.app_id.as_str()) {
                true => 1.0 + TARGET_GROW * self.grow.value.max(0.0),
                false => 1.0,
            };
            let d = cell.size * grow * (1.0 - morph);
            // A widget's tile may be longer along the dock.
            let len = cell.len * grow * (1.0 - morph);
            // A bounce never takes the icon off the surface.
            let room = (frame.across() - m.icon_v() - d).max(0.0);
            let hop = (slot.bounce).map_or(0.0, |b| (b.height() * BOUNCE * m.icon).min(room));
            let r = frame.rect(
                into(cell.center) - len / 2.0,
                m.icon_v() + hop - slide,
                len,
                d,
            );
            if let Some(at) = icons.get_mut(i) {
                *at = Some(r);
            }
            // The dragged or flying icon is drawn on its own, and so is the
            // one the launcher sends here.
            let landing_here = landing_app.as_deref() == Some(item.app_id.as_str());
            let lifted = lifted.as_deref() == Some(item.app_id.as_str()) || landing_here;
            if lifted && !landing_here && self.fly.is_some() {
                landing = Some(r);
            }
            match keys.get(i).cloned().flatten() {
                Some(key) if !lifted => {
                    // The image reaches past the icon by its shadow, which
                    // scales with the icon.
                    let padded = r.outset(key.pad() as f64 * d / key_size);
                    let faded = match item.kind {
                        Kind::Folder { exists: false } => MISSING_ALPHA,
                        _ => 1.0,
                    };
                    let alpha = if d >= 0.5 {
                        seen * faded * (1.0 - morph)
                    } else {
                        0.0
                    };
                    subs.icon
                        .present(cache.icon_or_older(&key).as_ref(), padded, alpha);
                }
                _ => subs.icon.clear(),
            }

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
                    let r = frame.rect(into(cell.center) - w / 2.0, m.dot_v() - slide, w, h);
                    let a = gated * seen;
                    subs.indicator.set_transform(transform);
                    subs.indicator.present(Some(&s.buffer), r, a);
                }
                None => subs.indicator.clear(),
            }
        }

        // The dragged icon under the pointer, or flying to its slot.
        let lifted_rect = match (&self.drag, &self.fly) {
            (Some(drag), _) => Some(self.drag_rect(drag)),
            (None, Some(fly)) => landing.map(|to| {
                let t = fly.t.value;
                let f = fly.from;
                Rect::new(
                    lerp(f.x, to.x, t),
                    lerp(f.y, to.y, t),
                    lerp(f.w, to.w, t),
                    lerp(f.h, to.h, t),
                )
            }),
            (None, None) => None,
        };
        let lifted_key = (lifted.as_deref())
            .and_then(|a| self.slots.iter().find(|s| s.item.app_id == a))
            .and_then(|s| self.icon_key(&s.item));
        match (lifted_rect, lifted_key) {
            (Some(r), Some(key)) => {
                let padded = r.outset(key.pad() as f64 * r.w / key_size);
                (self.drag_sub).present(cache.icon_or_older(&key).as_ref(), padded, seen);
            }
            _ => self.drag_sub.clear(),
        }

        // The numbers at the icons' corners, during keyboard activation:
        // toward the output's middle from a right dock.
        let badge_alpha = self.gate(self.badge_alpha.value) * seen * (1.0 - morph);
        let numbered = match (badge_alpha > 0.0, &self.style.badge) {
            (true, Some(_)) => activate::numbered(self.items()),
            _ => Vec::new(),
        };
        let size = self.style.badge.as_ref().map_or(0.0, |b| b.size);
        let inset = size * BADGE_INSET;
        let left = self.style.layout.position == Position::Right;
        for (k, (dot, digit)) in self.badge_subs.iter_mut().enumerate() {
            let icon = (numbered.get(k))
                .and_then(|key| {
                    (self.slots.iter()).position(|s| !s.leaving && s.item.app_id == *key)
                })
                .and_then(|i| icons.get(i).copied().flatten());
            let Some(r) = icon else {
                dot.clear();
                digit.clear();
                continue;
            };
            let cx = if left { r.x + inset } else { r.right() - inset };
            let cy = r.y + inset;
            let back = Rect::new(cx - size / 2.0, cy - size / 2.0, size, size);
            dot.present(
                self.badge_shape.as_ref().map(|s| &s.buffer),
                back,
                badge_alpha,
            );
            match self.badge_texts.get(k).and_then(|t| t.pill.as_ref()) {
                Some((buffer, w, h)) => {
                    let at = Rect::new((cx - w / 2.0).round(), (cy - h / 2.0).round(), *w, *h);
                    digit.present(Some(buffer), at, badge_alpha);
                }
                None => digit.clear(),
            }
        }

        // The label above (or beside) the hovered icon, following it.
        if let Some(key) = &self.label {
            let slot = self.slots.iter().position(|s| s.item.app_id == key.app_id);
            // Pinning folders: over the open gap.
            let gap = (key.app_id == GAP_LABEL)
                .then(|| self.gaps.iter().position(|g| g.presence.target > 0.5))
                .flatten();
            let cell = match gap {
                Some(k) => laid.gap_cell(k),
                None => slot.and_then(|i| laid.cell(i)),
            };
            if let Some(cell) = cell {
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
        // the trigger band there. The launcher takes the input while it
        // is open.
        let mut input = Vec::new();
        let autohide = self.style.autohide;
        let present = if self.launcher.is_some() {
            0.0
        } else {
            present
        };
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

    /// The panel for `row` slid by `slide`: its rect on the surface, where
    /// it starts along the dock, and its length. It is the row without
    /// its outer half gaps, plus padding, never shorter than its shape.
    fn panel_at(&self, row: &zoom::Row, slide: f64) -> (Rect, f64, f64) {
        let frame = self.frame();
        let m = self.style.metrics();
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
        let panel_u = (start + end) / 2.0 - length / 2.0;
        let panel = frame.rect(panel_u, m.panel_v() - slide, length, m.panel());
        (panel, panel_u, length)
    }

    // -- the launcher --------------------------------------------------------

    /// Whether the launcher can open on this dock: it is at the bottom and
    /// on screen.
    pub fn takes_launcher(&self) -> bool {
        self.style.layout.position == Position::Bottom && self.configured && self.height > 0
    }

    /// What the launcher takes over: where the panel is now, and how long
    /// it is at rest.
    pub fn handover(&self) -> Handover {
        let laid = self.lay(self.pointer.value, self.hover.value.clamp(0.0, 1.0), false);
        let slide = autohide::slide(self.shown.value, self.style.reach());
        let (panel, ..) = self.panel_at(&laid.row, slide);
        let rest = self.lay(self.pointer.target, 0.0, true);
        let (_, _, rest_length) = self.panel_at(&rest.row, 0.0);
        Handover {
            panel,
            rest_length,
            height: self.height as f64,
        }
    }

    /// The launcher opened over this dock (its first frame is drawn): the
    /// panel is the launcher's now, and the icons shrink into it. Nothing
    /// magnifies, no label shows, and a hidden dock comes back.
    pub fn begin_launcher(&mut self, core: &Core<WlDock>) {
        let mut morph = Spring::new(0.0, self.style.response);
        morph.retarget(1.0);
        self.launcher = Some(Launcher {
            morph,
            landing: None,
        });
        self.cancel_label_timer();
        self.label_on = false;
        self.unhover(core);
        self.cancel_autohide_timer();
        self.set_shown(true);
        self.update_label();
        self.render();
    }

    /// The launcher is closing: the icons come back, and if it opened an
    /// app, that app's icon is the one flying in.
    pub fn end_launcher(&mut self, landing: Option<String>) {
        if let Some(l) = &mut self.launcher {
            l.morph.retarget(0.0);
            l.landing = landing;
        }
        self.kick();
    }

    /// The launcher is gone: the dock has its panel back.
    pub fn launcher_gone(&mut self) {
        if self.launcher.take().is_some() {
            self.sync_autohide();
            self.render();
        }
    }

    /// Where the launcher sends the icon of `app_id` it opened: its slot at
    /// rest, or where a new app's slot appears (after the last app).
    pub fn landing_rect(&self, app_id: &str) -> Rect {
        let frame = self.frame();
        let m = self.style.metrics();
        let laid = self.lay(self.pointer.target, 0.0, true);
        let at = |u: f64| frame.rect(u - m.icon / 2.0, m.icon_v(), m.icon, m.icon);
        if let Some(i) = (self.slots.iter()).position(|s| !s.leaving && s.item.app_id == app_id)
            && let Some(cell) = laid.cell(i)
        {
            return at(cell.center);
        }
        let step = m.icon + self.style.gap;
        let last_app = (self.slots.iter().enumerate().rev())
            .filter(|(_, s)| !s.leaving && s.item.is_app())
            .find_map(|(i, _)| laid.cell(i))
            .map(|c| c.center + step);
        at(last_app.unwrap_or(frame.length() / 2.0))
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
    pub fn strip_open(&self) -> bool {
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
        let laid = self.lay(self.pointer.target, self.hover.target, true);
        let cell = laid.cell(i)?;
        let d = cell.size.max(1.0);
        let len = cell.len.max(d);
        let r = frame.rect(cell.center - len / 2.0, m.icon_v(), len, d);
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
        self.frozen = false;
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
        let key = (self.style.strip.previews == Previews::Off).then(|| self.app_icon_key(&app_id));
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

    // -- the widget's popup ------------------------------------------------

    /// Whether a widget's popup is open (and not closing).
    pub fn widget_open(&self) -> bool {
        self.widget.as_ref().is_some_and(|w| !w.closing())
    }

    /// The key of the item whose widget's popup is open (and not
    /// closing).
    pub fn widget_item(&self) -> Option<&str> {
        (self.widget.as_ref())
            .filter(|w| !w.closing())
            .map(|w| w.app.as_str())
    }

    /// Fade the widget's popup out; it goes once it's gone. Without the
    /// pointer on the dock, the magnification it held goes too.
    pub fn close_widget(&mut self, core: &Core<WlDock>) {
        let Some(w) = &mut self.widget else { return };
        if w.closing() {
            return;
        }
        w.close();
        self.sync_autohide();
        if !self.pointer_in && !self.strip_open() && !self.menu_open() {
            self.unhover(core);
        }
        self.kick();
    }

    /// Show `tree` in `app`'s widget's popup, opening it if it isn't.
    pub fn paint_widget(&mut self, app: &str, tree: Arc<ui::Node>) {
        if let Some(w) = self
            .widget
            .as_mut()
            .filter(|w| w.app == app && !w.closing())
        {
            return w.set_tree(tree);
        }
        let Some((icon, _)) = self.icon_place(app) else {
            return;
        };
        let icon =
            wl_shell_ui::Rect::new(icon.x as f64, icon.y as f64, icon.w as f64, icon.h as f64);
        let widget = WidgetView::new(
            self.g.clone(),
            self.assets.clone(),
            &self.qh,
            &self.layer,
            app.to_string(),
            tree,
            self.style.menu.clone(),
            self.style.ui_theme.clone(),
            self.style.layout.position,
            icon,
            self.scale,
        );
        match widget {
            Ok(w) => {
                self.widget = Some(w);
                self.sync_autohide();
            }
            Err(e) => warn!("cannot show the widget's popup: {e}"),
        }
    }

    /// A painted tree from the worker; true if it was the widget's.
    pub fn widget_ready(&mut self, tag: u64, req: &ui::Request, r: &ui::Rendered) -> bool {
        let Some(w) = &mut self.widget else {
            return false;
        };
        let ours = w.ui_ready(tag, req, r);
        if ours {
            w.render();
            if w.moving() {
                self.kick();
            }
        }
        ours
    }

    /// When the widget's live layers are due again, and the popup's ID.
    pub fn widget_tick_due(&mut self) -> Option<(u64, std::time::Duration)> {
        self.widget.as_mut()?.tick_due()
    }

    /// They are due, if popup `id` is still there.
    pub fn widget_tick(&mut self, id: u64) {
        if let Some(w) = self.widget.as_mut().filter(|w| w.id == id) {
            w.tick();
        }
    }

    pub fn has_widget_popup(&self, popup: &Popup) -> bool {
        self.widget.as_ref().is_some_and(|w| w.is(popup))
    }

    pub fn has_widget_surface(&self, surface: &WlSurface) -> bool {
        self.widget.as_ref().is_some_and(|w| w.has_surface(surface))
    }

    /// The compositor placed the widget's popup.
    pub fn widget_configure(&mut self, configure: &PopupConfigure) {
        if let Some(w) = &mut self.widget {
            w.configure(configure);
        }
        self.kick();
    }

    /// A pointer event on the widget's popup: its app, and a node's key
    /// and what happened to it, for the plugin.
    pub fn widget_pointer(&mut self, e: &PointerEvent) -> Option<(String, String, PopupEvent)> {
        let w = self.widget.as_mut()?;
        let (key, event) = w.pointer(e)?;
        Some((w.app.clone(), key, event))
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
        if let Some(menu) = &mut self.menu
            && menu.is(popup)
        {
            // A drag from the menu still needs its surface.
            match menu.held() {
                true => menu.close(),
                false => self.drop_menu(core),
            }
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
        if let Some(t) = (self.badge_texts.iter_mut()).find(|t| t.tag == tag) {
            if t.request != *request {
                return false;
            }
            match cache::shm_buffer(&self.g, &pill.image, &self.qh) {
                Ok(buffer) => {
                    let size = (pill.width as f64, pill.height as f64);
                    if let Some((old, ..)) = t.pill.replace((buffer, size.0, size.1)) {
                        old.destroy();
                    }
                    self.kick();
                }
                Err(e) => warn!("badge buffer: {e}"),
            }
            return true;
        }
        let ours = (self.strip.as_mut()).is_some_and(|s| s.pill_ready(tag, request, pill))
            || (self.menu.as_mut()).is_some_and(|m| m.pill_ready(tag, request, pill));
        if ours {
            self.kick();
        }
        ours
    }

    /// The items on this dock (not those on their way out).
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        (self.slots.iter().filter(|s| !s.leaving)).map(|s| &s.item)
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

    /// The folder whose popup is open (and not closing), by its key.
    pub fn popup_folder(&self) -> Option<&str> {
        self.menu_app().filter(|_| self.popup)
    }

    /// Open `groups` as the menu of `app_id`, grabbing `grab` (a seat and
    /// the serial of the click). The strip and any other menu go at once.
    /// It shows once its rows are rendered. Returns whether it opened.
    pub fn open_menu(
        &mut self,
        core: &Core<WlDock>,
        app_id: &str,
        groups: Vec<menu::Group>,
        grab: Option<(&WlSeat, u32)>,
    ) -> bool {
        self.open_popup(core, app_id, groups, Look::Menu, false, grab)
    }

    /// Open the popup of the folder `key` (at `dir`) with its `files`, as
    /// `view` says: a menu, or a stack of as many files as fit. Like
    /// `open_menu` otherwise.
    pub fn open_folder(
        &mut self,
        core: &Core<WlDock>,
        key: &str,
        dir: &Path,
        files: &[FileEntry],
        view: FolderView,
        grab: Option<(&WlSeat, u32)>,
    ) -> bool {
        let (groups, look) = match view {
            FolderView::List => (menu::folder_popup(dir, files), Look::Menu),
            FolderView::Stack => {
                // Open in File Manager shows the folder's icon.
                let icons = vec![self.style.folder_icon(dir), folder::ICON.to_string()];
                let fits = self.stack_fits(key);
                (menu::folder_stack(dir, files, fits, icons), Look::Stack)
            }
        };
        self.open_popup(core, key, groups, look, true, grab)
    }

    /// How many items a stack out of `key`'s icon has room for.
    fn stack_fits(&self, key: &str) -> usize {
        let m = self.style.menu.stack_metrics();
        let (_, out_h) = self.output_size.unwrap_or((1920.0, 1080.0));
        let room = match self.style.layout.position {
            // Above the icon, which is this far from the edge.
            Position::Bottom => {
                let reach = self.icon_place(key).map_or(self.style.reach(), |(_, r)| r);
                out_h - reach - stack::DOCK_GAP - STACK_ROOM
            }
            // Centered on it, sliding along the edge to stay on the output.
            Position::Left | Position::Right => out_h - 2.0 * STACK_ROOM,
        };
        stack::fits(&m, room)
    }

    /// Open `groups` as the menu of `app_id`, shown as `look` (a folder's
    /// popup if `popup`).
    fn open_popup(
        &mut self,
        core: &Core<WlDock>,
        app_id: &str,
        groups: Vec<menu::Group>,
        look: Look,
        popup: bool,
        grab: Option<(&WlSeat, u32)>,
    ) -> bool {
        self.popup = popup;
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
                look,
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

    /// A pointer event on the menu: a row picked, or a file dragged out.
    pub fn menu_pointer(&mut self, core: &Core<WlDock>, event: &PointerEvent) -> Option<MenuEvent> {
        let menu = self.menu.as_mut()?;
        let (x, y) = event.position;
        let input = menu.pointer(&event.kind, x, y);
        self.kick();
        if let MenuInput::Drag { row, serial } = input {
            let menu = self.menu.as_ref()?;
            let path = menu.entry(row)?.file.clone()?;
            return Some(MenuEvent::DragOut {
                path,
                serial,
                surface: menu.surface()?.clone(),
                icon: menu.mark_buffer(row, &self.cache.borrow()),
                scale: self.scale,
            });
        }
        let (app_id, action) = self.menu_input(core, input)?;
        Some(MenuEvent::Pick(app_id, action))
    }

    /// A file was dragged out of the menu: it fades, but stays until the
    /// drag ends (`release_menu`).
    pub fn hold_menu(&mut self, core: &Core<WlDock>) {
        if let Some(menu) = &mut self.menu {
            menu.hold();
            self.menu_closed(core);
        }
    }

    /// The drag out of the menu ended.
    pub fn release_menu(&mut self) {
        if let Some(menu) = &mut self.menu {
            menu.release();
            self.kick();
        }
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
            MenuInput::None | MenuInput::Drag { .. } => None,
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

    /// A bubble from the worker; true if it was the menu's or the
    /// widget's popup's.
    pub fn bubble_ready(&mut self, tag: u64, bubble: &Bubble, image: &Image) -> bool {
        let ours = (self.menu.as_mut()).is_some_and(|m| m.bubble_ready(tag, bubble, image))
            || (self.widget.as_mut()).is_some_and(|w| w.bubble_ready(tag, bubble, image));
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
