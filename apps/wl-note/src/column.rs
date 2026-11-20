//! One output's column of notes: its layer surface, the scene on it, and
//! the pointer and keys on it.
//!
//! A layer surface anchored to the edge and both sides, as wide as a
//! sheet and its shadow (so revealing or opening never resizes it), with
//! an exclusive zone of 0, or of the edge band with `reserve-edge-band`.
//! The surface itself shows a transparent pixel, and its input region is
//! only what shows of the notes and the edge bands, so clicks anywhere
//! else go through.
//!
//! Each note is a sync subsurface showing its paper (papers.rs), placed by
//! the deck's springs (deck.rs). A paper is drawn at its natural size, its
//! side that comes out first `out` from the edge, and cut to the card's
//! length along the edge while that grows or shrinks; it reaches past the
//! surface and is cropped to it, so it never shows on a neighboring
//! output. Which paper follows the most the note is out for, now or
//! heading to (`Role`):
//!
//! - **Marker:** a small rounded strip, seen edge-on.
//! - **Header:** a tab: the note's margin with its title turned a
//!   quarter, a dashed crease; lifted under the pointer.
//! - **Sheet, revealed:** the whole sheet, tilted, with its title and
//!   text drawn on (cut with a fade), and its file bar.
//! - **Sheet, open:** straight, with the editable body drawn on the main
//!   thread, and the bar's icons above the paper.
//!
//! The file bar's icons are subsurfaces too (thumbnails or type icons,
//! monochrome in the note's text color until hovered), with the hovered
//! one's name beside them, and on an open sheet the archive and pin icons
//! at its end.
//!
//! At header level a margin can be dragged along the column to reorder
//! it (deck.rs, reorder.rs): a press on a margin opens it on release,
//! unless the pointer moved along the edge first. A hidden note crumples
//! into the edge: its paper shrinks toward it and fades.
//!
//! An open drawer (drawer.rs) is the deck's open card under the key
//! `drawer::KEY`, last in the column: a kraft sheet whose margin names it
//! and whose body (the filter, the scope, the rows, drawn as one image by
//! drawerview.rs on the main thread, drawn again only when something
//! changed) takes the pointer and the keys. It is closed by a note opening,
//! its margin, Escape or its button; picking a row asks the program to
//! show the note again (`Act::Show`).
//!
//! The keyboard is only taken by an open note: opening asks for it
//! (`exclusive`), and once it has it, `on-demand` lets a click elsewhere
//! take it away while the note stays open. Closing gives it back.
//!
//! Frames are only drawn while a spring moves: an idle column costs
//! nothing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{FrameCallbackData, Region},
    seat::{
        keyboard::{KeyEvent, Keysym, Modifiers},
        pointer::{AxisScroll, BTN_LEFT, PointerEvent, PointerEventKind},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wl_shell_core::dnd::DragIcon;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::reexports::calloop::RegistrationToken;
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_notes::due::{self, Local, Preset, TimeOfDay};
use wl_shell_notes::{Color, PRESETS};
use wl_shell_render::Image;
use wl_shell_render::doc::{self, Doc, DocStyle, Target};
use wl_shell_render::edit::{EditLayout, EditStyle, Pos};
use wl_shell_render::icons::NamedIcon;
use wl_shell_render::paper::{self, Paper};
use wl_shell_render::shadow::IconShadow;
use wl_shell_render::text::{self, Field, Text};
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_ui::anim::Spring;
use wl_shell_ui::cache::shm_buffer;
use wl_shell_ui::geom::clip;
use wl_shell_ui::{IRect, Rect, Sub, cache, lerp};

use crate::app::WlNote;
use crate::bar;
use crate::config::{ColumnLayout, Config, Edge, NoteLayer};
use crate::deck::{Card, Deck, Group, Item, LIFT, Look, Metrics, On, Role};
use crate::draft::Draft;
use crate::drawer::{self, Drawer};
use crate::drawerview::{self, Lines};
use crate::files;
use crate::input::Field as FilterField;
use crate::markdown::{self, Content};
use crate::papers::{Papers, Sheet};
use crate::row::{self, Available, Button, Mode};

/// The text's distance from a sheet's body's sides, and from its top.
const PAD_X: f64 = 12.0;
const PAD_Y: f64 = 10.0;
/// How far papers reach behind the edge past how far they are out, so
/// a tilt or a lift never shows their end.
const BEHIND: f64 = 10.0;
/// How much larger than its markers a group's backdrop is, on each side.
const BACKDROP_PAD: f64 = 3.0;
/// Corner radii of tabs and sheets.
const TAB_RADIUS: f64 = 7.0;
const SHEET_RADIUS: f64 = 8.0;
/// A label starts this far from its margin's top.
const LABEL_PAD: f64 = 8.0;
/// Between a label's letters, ems.
const LABEL_SPACING: f32 = 0.12;
/// The font titles, labels and names are set in.
const UI_FONT: &str = "sans-serif";
/// The notice's line size, of the font size (a doc's level 2 heading).
const NOTICE_SCALE: f64 = 1.15;
/// File bar icons and the gap between them.
const ICON: f64 = 16.0;
const ICON_GAP: f64 = 8.0;
const ROW_ICON: u32 = 14;
const ROW_COUNT_ICON: u32 = 10;
const TRASH_RED: u32 = 0xcb3535ff;
const PALETTE_TIME: Duration = Duration::from_millis(150);
/// Name labels kept.
const LINES: usize = 48;
/// A wheel click or an arrow key scrolls this far.
const LINE: f64 = 40.0;
/// How far the pointer moves along the edge with a margin pressed before
/// it is dragged rather than clicked.
const DRAG_START: f64 = 6.0;
/// What a crumpled note shrinks to.
const CRUMPLED: f64 = 0.2;
/// A header fades into its marker over this much of the last of its way
/// in (and out of it over the first of its way out).
const HEADER_FADE: f64 = 0.5;
/// Icons for files whose own aren't known (yet).
const FILE_ICONS: [&str; 3] = ["text-x-generic", "application-x-generic", "unknown"];
/// What the archive icon says it does.
const ARCHIVE_LABEL: &str = "Archive (hide this note)";

/// The fonts on the main thread, loaded in the background.
pub type Fonts = Rc<RefCell<Option<Text>>>;

/// What the column looks like and how it behaves, from the config.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub edge: Edge,
    pub layer: NoteLayer,
    pub metrics: Metrics,
    pub marker_radius: f64,
    /// Keep windows off the edge band.
    pub reserve_band: bool,
    pub file_bar: f64,
    pub reveal_delay: Duration,
    pub hide_delay: Duration,
    pub response: f64,
    row_buttons: Vec<Button>,
    pub shadow: Option<IconShadow>,
    /// Lifted headers' and sheets'.
    pub lifted: Option<IconShadow>,
    pub font: String,
    pub font_size: f64,
    pub link: u32,
    /// The `+` button's color and its plus's.
    pub plus_color: u32,
    pub plus_glyph: u32,
    /// Behind each group's markers (alpha 0: none).
    pub backdrop: u32,
    /// An overdue note's due time and clock.
    pub overdue: u32,
    /// The drawers' paper.
    pub drawer: u32,
    /// The due presets' times of day.
    pub morning: TimeOfDay,
    pub evening: TimeOfDay,
    palette: [u32; PRESETS.len()],
}

impl Style {
    pub fn new(config: &Config, layout: ColumnLayout) -> Self {
        let theme = config.theme();
        let (white, ink) = theme.preset("white");
        Self {
            edge: layout.edge,
            layer: layout.layer,
            metrics: Metrics {
                marker_width: config.column.marker_width as f64,
                marker_length: config.column.marker_length as f64,
                marker_gap: config.column.marker_gap as f64,
                band: config.column.edge_band as f64,
                padding: config.column.marker_padding as f64,
                header: config.column.header as f64,
                header_length: config.column.header_length as f64,
                sheet_width: config.sheet.width as f64,
                sheet_height: config.sheet.height as f64,
                plus: 22.0,
                group_gap: config.column.group_gap as f64,
                backdrop_headers: config.column.group_backdrop_headers,
            },
            marker_radius: config.column.marker_radius as f64,
            reserve_band: config.column.reserve_edge_band,
            file_bar: config.sheet.file_bar as f64,
            reveal_delay: Duration::from_millis(config.column.reveal_delay as u64),
            hide_delay: Duration::from_millis(config.column.hide_delay as u64),
            response: config.column.response as f64,
            row_buttons: config.row.buttons.clone(),
            shadow: config.shadow(),
            lifted: config.lifted_shadow(),
            font: config.sheet.font.clone(),
            font_size: config.sheet.font_size as f64,
            link: theme.link_color,
            plus_color: (white & !0xff) | 0xe6,
            plus_glyph: (ink & !0xff) | 0xb0,
            backdrop: theme.group_backdrop,
            overdue: theme.overdue,
            drawer: config.drawer_color(),
            morning: config.due.morning,
            evening: config.due.evening,
            palette: PRESETS.map(|name| config.paper_color(Some(&Color::Preset(name)))),
        }
    }

    /// Whether going from `self` to `new` takes a new surface.
    pub fn needs_new_surface(&self, new: &Style) -> bool {
        self.edge != new.edge || self.layer != new.layer
    }

    /// The surface's width: a sheet and its shadow.
    fn width(&self) -> u32 {
        let reach = (self.lifted.or(self.shadow)).map_or(0, |s| s.pad()) as f64;
        (self.metrics.sheet_width + 2.0 * reach + 16.0).ceil() as u32
    }

    fn doc_style(&self, color: u32) -> DocStyle {
        DocStyle {
            font: self.font.clone(),
            heading_font: Some(UI_FONT.into()),
            size: self.font_size,
            color,
            link: self.link,
        }
    }

    /// The labels' and names' text style.
    fn small(&self, color: u32) -> text::Style {
        text::Style {
            font: UI_FONT.into(),
            size: (self.font_size * 0.8).max(9.0),
            color,
            background: 0,
        }
    }

    /// A line of the notice above the open note's text.
    fn notice_line(&self) -> f64 {
        text::line_height(self.font_size * NOTICE_SCALE) as f64
    }

    /// A sheet's body's width: beside the margin.
    fn body_w(&self) -> f64 {
        self.metrics.sheet_width - self.metrics.header
    }

    /// The width text wraps at on a sheet.
    fn content_w(&self) -> f64 {
        (self.body_w() - 2.0 * PAD_X).max(20.0)
    }

    /// The exclusive zone: the edge band, if reserved.
    fn exclusive_zone(&self) -> i32 {
        match self.reserve_band {
            true => self.metrics.band.round() as i32,
            false => 0,
        }
    }
}

/// Whether the open note can be pinned to the context's window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pin {
    /// There is no window to pin to.
    None,
    /// Its frontmatter is broken: it isn't edited.
    Locked,
    /// Not pinned to it; the window's app ID.
    Unpinned(String),
    /// Pinned; unpinning leaves it global.
    Pinned { global_after: bool },
    /// Promote a scratch note to an ordinary note.
    Keep,
}

/// A file on a note.
#[derive(Clone, Debug, PartialEq)]
pub struct FileLink {
    pub path: PathBuf,
    pub exists: bool,
}

/// A note to show: its card, and what is on it.
#[derive(Clone)]
pub struct Shown {
    pub item: Item,
    pub content: Rc<Content>,
    /// Its text color.
    pub text: u32,
    pub files: Vec<FileLink>,
    pub pin: Pin,
    pub color: Color,
    pub due: Option<DueShown>,
}

/// A note's due time as its sheet shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DueShown {
    /// "Due Fri 20 Nov 09:00".
    pub label: String,
    pub overdue: bool,
}

/// What the column knows of a note shown.
struct NoteView {
    content: Rc<Content>,
    text: u32,
    files: Vec<FileLink>,
    pin: Pin,
    color: Color,
    paper: u32,
    scratch: bool,
    due: Option<DueShown>,
    /// The color of its marker and margin, if not the paper's.
    accent: Option<u32>,
}

/// What a pointer event or a key asks of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Open this note.
    Open(String),
    /// Close the open note.
    Close,
    /// Toggle this note's n-th checkbox.
    Task(String, u32),
    /// Open this note's n-th link.
    Link(String, u32),
    OpenLink(String, String),
    Save,
    QueueSave,
    Reload,
    KeepMine,
    Copy(String),
    Paste(u64),
    /// Open this note in the editor.
    Edit(String),
    /// Make a new note.
    New(bool),
    Row(Button),
    /// Keep as an app note (optionally matching its title).
    Keep(String, bool),
    /// Keep at this gap in the app or global group.
    KeepAt(String, Group, Vec<String>, usize),
    /// Open this file.
    File(PathBuf),
    /// Pin this note to the context's app (or, with true, its window).
    Pin(String, bool),
    /// Unpin it from the context's window.
    Unpin(String),
    /// Hide this note.
    Hide(String),
    /// Show this hidden note again, and open it (or not).
    Show(String, bool),
    /// Make a new note from this template (a path in `.templates`).
    Template(String),
    Color(String, Color),
    /// Set this note's due time (a TOML date or datetime), or with None
    /// clear it.
    Due(String, Option<String>),
    Trash(String),
    RemoveFile(String, PathBuf),
    /// This note was dropped at this slot of these notes (the group it
    /// lands in, in order).
    Reorder(String, Vec<String>, usize),
}

/// The column's timers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// The pointer rested on a margin: reveal it.
    Reveal,
    /// The pointer left the column: back to markers.
    Hide,
    Trash,
    RowRest,
    RowHide,
}

/// A card's subsurface, and the paper it shows.
struct Slot {
    key: String,
    sub: Sub,
    /// Its marker, under a header as that fades into it or out of it.
    under: Sub,
    /// The paper shown, and its sheet.
    shown: Option<(Paper, Sheet)>,
    /// Where its paper is on the surface (unclipped).
    paper: Rect,
    /// Where it takes the pointer, if it does.
    hit: Option<Rect>,
}

/// A body viewport already drawn; animation only repositions it.
struct BodyShown {
    image: Sized,
    scroll: f64,
}

/// A buffer and its logical size.
#[derive(Clone)]
struct Sized {
    buffer: WlBuffer,
    w: f64,
    h: f64,
}

/// What of a sheet's file bar is somewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BarHit {
    /// The n-th file's icon.
    File(usize),
    RemoveFile(usize),
    /// The `+n` for those that don't fit.
    More,
    Color,
    Swatch(Color),
    /// The clock that opens the due choices.
    Clock,
    /// One of the due choices (None: clear it).
    Due(Option<Preset>),
    Trash,
    Archive,
    Pin,
}

/// Where the parts of a sheet's file bar are, on the surface.
struct BarLayout {
    /// The file's index and its icon's rect, for those shown.
    icons: Vec<(usize, Rect)>,
    /// How many more, and where the `+n` is.
    more: Option<(usize, Rect)>,
    color: Option<Rect>,
    swatches: Vec<(Color, Rect)>,
    clock: Option<Rect>,
    due_row: Vec<(Option<Preset>, Rect)>,
    trash: Option<Rect>,
    archive: Option<Rect>,
    pin: Option<Rect>,
    hit_pad: f64,
    /// Where names go: from `name_x`, up to `name_end`, centered on
    /// `mid`.
    name_x: f64,
    name_end: f64,
    mid: f64,
}

/// A bar's subsurfaces.
struct BarSubs {
    icons: Vec<Sub>,
    more: Sub,
    name: Sub,
    archive: Sub,
    pin: Sub,
    labels: Vec<Sub>,
}

/// A press on a margin or a revealed sheet: a click, or the start of a
/// drag.
struct Press {
    key: String,
    /// Where it was.
    x: f64,
    y: f64,
    /// From the pointer to the card's center, along the edge, once
    /// dragged.
    offset: f64,
    /// On a revealed sheet: a click there does what it does on it.
    revealed: bool,
    dragging: bool,
}

struct FilePress {
    key: String,
    path: PathBuf,
    at: (f64, f64),
    serial: u32,
    ctrl: bool,
}

/// A drop is resolved against the sheet at button release, not when the
/// data (or a copy) arrives. Only reuse the offset in the same text.
pub enum FileTarget {
    New,
    NewScratch,
    NewGlobal,
    /// A template of the open Templates drawer: a new note from it, with
    /// the files linked.
    Template(String),
    Note {
        key: String,
        editor: Option<(usize, String)>,
    },
}

/// A name label: its text, color, width and whether it is struck
/// through.
#[derive(Clone, Debug, PartialEq)]
struct LineKey {
    text: String,
    color: u32,
    width: i32,
    strike: bool,
    tooltip: bool,
}

impl LineKey {
    fn style(&self, style: &Style) -> text::Style {
        let mut style = style.small(self.color);
        if self.tooltip {
            style.background = 0x202020e6;
        }
        style
    }
}

struct RowSlot {
    disc: Sub,
    glyph: Sub,
    count: Sub,
    out: Spring,
}

/// What of the drawer's body is somewhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DrawerHit {
    Filter,
    Scope,
    /// The n-th row in view.
    Row(usize),
    /// The button (the trash icon, or the ✓) on the n-th row in view.
    Action(usize),
}

/// The open drawer.
struct DrawerState {
    drawer: Drawer,
    /// The filter, being typed.
    filter: FilterField,
    /// How far the filter is scrolled sideways.
    field_scroll: f64,
    hover: Option<DrawerHit>,
    /// Wheel movement not yet a row.
    wheel: f64,
}

/// What picking the row of the note `key` in a drawer of `kind` asks for
/// (`ctrl` held): a hidden note is shown again, and opened unless Ctrl
/// leaves the drawer to bring back several; a due note is opened (shown
/// again if it is hidden); a template makes a new note.
fn pick(kind: drawer::Kind, key: String, ctrl: bool) -> Act {
    match kind {
        drawer::Kind::Hidden => Act::Show(key, !ctrl),
        drawer::Kind::Due => Act::Show(key, true),
        drawer::Kind::Templates => Act::Template(key),
    }
}

pub struct Column {
    pub output: WlOutput,
    /// The output's key.
    pub name: String,
    /// Its connector name, which the output stream's lines use.
    pub connector: Option<String>,
    layer: LayerSurface,
    viewport: WpViewport,
    transparent: WlBuffer,
    no_input: Region,
    g: Rc<Globals>,
    qh: QueueHandle<Shell<WlNote>>,
    style: Style,
    papers: Rc<RefCell<Papers>>,
    assets: Rc<Assets>,
    fonts: Fonts,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    deck: Deck,
    notes: HashMap<String, NoteView>,
    slots: Vec<Slot>,
    plus: Sub,
    /// Each group's backdrop, by `Group`: its top, middle and bottom.
    backdrops: [[Sub; 3]; Group::ALL.len()],
    /// The backdrops' buffer: a rounded rect cut in three, and what it
    /// was drawn for (scale, color, width, radius).
    backdrop_shown: Option<((i32, u32, i32, i32), WlBuffer)>,
    /// The `+` button's buffer, and the scale it was drawn at.
    plus_shown: Option<(i32, WlBuffer, paper::Rendered)>,
    /// Where the `+` button is, while it takes the pointer.
    plus_rect: Option<Rect>,
    row_slots: [RowSlot; Button::ALL.len()],
    row_label: Sub,
    row_label_for: Option<Button>,
    row_disc: Option<Sized>,
    row_hits: Vec<(Button, Rect)>,
    row_hold: Option<Rect>,
    row_available: Available,
    row_mode: Mode,
    row_last: Mode,
    row_drag: bool,
    row_timer: Option<(Timer, RegistrationToken)>,
    /// The open note's text and bar; the revealed note's bar.
    text: Sub,
    open_bar: BarSubs,
    reveal_bar: BarSubs,
    /// The buffer `text` shows.
    text_shown: Option<WlBuffer>,
    /// The notes to show, without the drawer.
    items: Vec<Item>,
    /// The drawer, while open (a card of the deck, `drawer::KEY`), the
    /// kind it last was (its margin shows it while it leaves), its body
    /// as drawn, and whether that is out of date.
    drawer: Option<DrawerState>,
    drawer_kind: drawer::Kind,
    drawer_shown: Option<Sized>,
    drawer_dirty: bool,
    drawer_lines: Lines,
    pub draft: Option<Draft>,
    layout: EditLayout,
    links: Vec<String>,
    body_shown: Option<BodyShown>,
    notice: Sub,
    notice_shown: Option<(String, Sized)>,
    body_dirty: bool,
    follow_caret: bool,
    focused: bool,
    selecting: bool,
    last_click: Option<(Instant, f64, f64, u8)>,
    goal_x: Option<f64>,
    paste_serial: u64,
    /// Replaced buffers, destroyed by `trim` once not shown.
    stale: Vec<WlBuffer>,
    /// How far the open note's text is scrolled.
    scroll: f64,
    /// Files' icon names (thumbnail first), and those asked for.
    file_icons: HashMap<PathBuf, Vec<String>>,
    asked_icons: HashSet<PathBuf>,
    /// Icons as decoded (None: there is none), and as buffers, plain
    /// (None) or tinted.
    icons: HashMap<NamedIcon, Option<Arc<Image>>>,
    icon_buffers: HashMap<(NamedIcon, Option<u32>), WlBuffer>,
    /// Name labels, most recently used first; None: asked for.
    lines: Vec<(LineKey, Option<Sized>)>,
    /// What the bars hover.
    bar_hover: Option<(String, BarHit)>,
    selected_files: Option<(String, HashSet<PathBuf>)>,
    file_press: Option<FilePress>,
    dragged_file_note: Option<String>,
    trash: bar::Trash,
    palette_since: Option<Instant>,
    /// The due choices are out, since then.
    due_since: Option<Instant>,
    dots: HashMap<(u32, u32, bool), WlBuffer>,
    /// All-files views on the open and revealed sheets, respectively.
    all_files: [Option<(String, usize)>; 2],
    /// The surfaces' order last applied, bottom first.
    stacking: Vec<WlSurface>,
    /// The input region last set.
    input: Option<Vec<IRect>>,
    /// Where the pointer is on the surface, while it is.
    pointer: Option<(f64, f64)>,
    /// The note waiting to be revealed, and its timer.
    reveal: Option<(String, RegistrationToken)>,
    hide: Option<RegistrationToken>,
    /// A press on a margin, until released.
    press: Option<Press>,
    keyboard: KeyboardInteractivity,
    frame_pending: bool,
    last_frame: Option<Instant>,
    /// Buffers attached by the last frame.
    attached: Vec<WlBuffer>,
    /// On its way out, e.g. to another output: its notes slide away, and
    /// the program drops it once they are gone (`retired`).
    pub retiring: bool,
}

impl BarSubs {
    fn new(
        g: &Globals,
        parent: &WlSurface,
        no_input: &Region,
        qh: &QueueHandle<Shell<WlNote>>,
    ) -> Self {
        Self {
            icons: Vec::new(),
            more: Sub::new(g, parent, no_input, qh),
            name: Sub::new(g, parent, no_input, qh),
            pin: Sub::new(g, parent, no_input, qh),
            archive: Sub::new(g, parent, no_input, qh),
            labels: Vec::new(),
        }
    }

    fn clear(&mut self) {
        for s in self.icons.iter_mut().chain(&mut self.labels).chain([
            &mut self.more,
            &mut self.name,
            &mut self.archive,
            &mut self.pin,
        ]) {
            s.clear();
        }
    }

    /// Show `plan` (or nothing), with `new` making icon subsurfaces as
    /// needed. Returns the label buffers it attached.
    fn apply(&mut self, plan: Option<BarPlan>, mut new: impl FnMut() -> Sub) -> Vec<WlBuffer> {
        let Some(plan) = plan.filter(|p| p.alpha > 0.0) else {
            self.clear();
            return Vec::new();
        };
        while self.icons.len() < plan.icons.len() {
            self.icons.push(new());
        }
        for (i, sub) in self.icons.iter_mut().enumerate() {
            match plan.icons.get(i) {
                Some((buffer, rect, a)) => sub.present(buffer.as_ref(), *rect, plan.alpha * a),
                None => sub.clear(),
            }
        }
        while self.labels.len() < plan.labels.len() {
            self.labels.push(new());
        }
        for (i, sub) in self.labels.iter_mut().enumerate() {
            match plan.labels.get(i) {
                Some((buffer, rect, a)) => sub.present(Some(buffer), *rect, plan.alpha * a),
                None => sub.clear(),
            }
        }
        let one = |sub: &mut Sub, part: &Option<(WlBuffer, Rect)>| match part {
            Some((buffer, rect)) => sub.present(Some(buffer), *rect, plan.alpha),
            None => sub.clear(),
        };
        one(&mut self.more, &plan.more);
        one(&mut self.name, &plan.name);
        let icon = |sub: &mut Sub, part: &Option<(Option<WlBuffer>, Rect, f64)>| match part {
            Some((buffer, rect, a)) => sub.present(buffer.as_ref(), *rect, plan.alpha * a),
            None => sub.clear(),
        };
        icon(&mut self.archive, &plan.archive);
        icon(&mut self.pin, &plan.pin);
        (plan.more.iter().chain(&plan.name))
            .map(|(b, _)| b.clone())
            .chain(plan.labels.iter().map(|(b, _, _)| b.clone()))
            .collect()
    }
}

impl Column {
    /// An empty column on `output` (named `name`, whose connector is
    /// `connector`, if it has one) at `scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: &Core<WlNote>,
        output: WlOutput,
        name: String,
        connector: Option<String>,
        scale: i32,
        style: Style,
        papers: Rc<RefCell<Papers>>,
        assets: Rc<Assets>,
        fonts: Fonts,
    ) -> std::io::Result<Self> {
        let g = core.g.clone();
        let qh = core.qh.clone();
        let transparent = cache::color_buffer(&g, 0, &qh)?;
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;
        let surface = g.compositor.create_surface(&qh);
        let viewport = g.viewporter.get_viewport(&surface, &qh, Ignore);
        let layer = match style.layer {
            NoteLayer::Top => Layer::Top,
            NoteLayer::Overlay => Layer::Overlay,
        };
        let layer = (g.layer_shell).create_layer_surface(
            &qh,
            surface,
            layer,
            Some("wl-note"),
            Some(&output),
        );
        let side = match style.edge {
            Edge::Left => Anchor::LEFT,
            Edge::Right => Anchor::RIGHT,
        };
        layer.set_anchor(side | Anchor::TOP | Anchor::BOTTOM);
        layer.set_size(style.width(), 0);
        layer.set_exclusive_zone(style.exclusive_zone());
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Nothing takes input until there are notes.
        (layer.wl_surface()).set_input_region(Some(no_input.wl_region()));
        // Mapped on configure.
        layer.commit();
        let parent = layer.wl_surface().clone();
        let sub = || Sub::new(&g, &parent, &no_input, &qh);
        let (plus, text, notice) = (sub(), sub(), sub());
        let row_slots = std::array::from_fn(|i| RowSlot {
            disc: sub(),
            glyph: sub(),
            count: sub(),
            out: Spring::new(0.0, style.response + i as f64 * 35.0),
        });
        let row_label = sub();
        let backdrops = std::array::from_fn(|_| std::array::from_fn(|_| sub()));
        let open_bar = BarSubs::new(&g, &parent, &no_input, &qh);
        let reveal_bar = BarSubs::new(&g, &parent, &no_input, &qh);
        let deck = Deck::new(style.metrics, style.response);
        let column = Self {
            output,
            name,
            connector,
            layer,
            viewport,
            transparent,
            no_input,
            g,
            qh,
            style,
            papers,
            assets,
            fonts,
            width: 0,
            height: 0,
            scale: scale.max(1),
            configured: false,
            deck,
            notes: HashMap::new(),
            slots: Vec::new(),
            plus,
            backdrops,
            backdrop_shown: None,
            plus_shown: None,
            plus_rect: None,
            row_slots,
            row_label,
            row_label_for: None,
            row_disc: None,
            row_hits: Vec::new(),
            row_hold: None,
            row_available: Available::default(),
            row_mode: Mode::Closed,
            row_last: Mode::Closed,
            row_drag: false,
            row_timer: None,
            text,
            open_bar,
            reveal_bar,
            text_shown: None,
            items: Vec::new(),
            drawer: None,
            drawer_kind: drawer::Kind::Hidden,
            drawer_shown: None,
            drawer_dirty: true,
            drawer_lines: Lines::default(),
            draft: None,
            layout: EditLayout::default(),
            links: Vec::new(),
            body_shown: None,
            notice,
            notice_shown: None,
            body_dirty: true,
            follow_caret: false,
            focused: false,
            selecting: false,
            last_click: None,
            goal_x: None,
            paste_serial: 0,
            stale: Vec::new(),
            scroll: 0.0,
            file_icons: HashMap::new(),
            asked_icons: HashSet::new(),
            icons: HashMap::new(),
            icon_buffers: HashMap::new(),
            lines: Vec::new(),
            bar_hover: None,
            selected_files: None,
            file_press: None,
            dragged_file_note: None,
            trash: bar::Trash::default(),
            palette_since: None,
            due_since: None,
            dots: HashMap::new(),
            all_files: [None, None],
            stacking: Vec::new(),
            input: None,
            pointer: None,
            reveal: None,
            hide: None,
            press: None,
            keyboard: KeyboardInteractivity::None,
            frame_pending: false,
            last_frame: None,
            attached: Vec::new(),
            retiring: false,
        };
        column.ask_standard_icons();
        Ok(column)
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn is(&self, layer: &LayerSurface) -> bool {
        self.layer.wl_surface() == layer.wl_surface()
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// The open note, if any.
    pub fn open_key(&self) -> Option<&str> {
        self.deck.open().filter(|k| *k != drawer::KEY)
    }

    /// Retiring, and its notes are gone (or were never shown): it can go.
    pub fn retired(&self) -> bool {
        self.retiring && (!self.configured || (self.deck.cards().is_empty() && !self.deck.moving()))
    }

    /// `key` is being hidden: it crumples into the edge as it leaves.
    pub fn crumple(&mut self, key: &str) {
        self.deck.crumple(key);
        self.kick();
    }

    /// The notes shown, in order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        (self.deck.cards().iter())
            .filter(|c| !c.leaving && c.key != drawer::KEY)
            .map(|c| c.key.as_str())
    }

    /// A new style (`needs_new_surface` is false for it).
    pub fn set_style(&mut self, style: Style) {
        if style == self.style {
            return;
        }
        self.layer.set_size(style.width(), 0);
        self.layer.set_exclusive_zone(style.exclusive_zone());
        self.deck.set_metrics(style.metrics, style.response);
        self.style = style;
        self.forget_drawn();
        // The drawer's paper.
        if self.drawer.is_some() {
            self.sync_deck();
        }
        self.kick();
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
        }
        self.deck.resize(height as f64);
        self.render();
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.forget_drawn();
        self.ask_standard_icons();
        self.kick();
    }

    /// Forget icons, labels and the button drawn for another scale or
    /// style; the subsurfaces showing them are cleared, so they can go.
    fn forget_drawn(&mut self) {
        self.open_bar.clear();
        self.reveal_bar.clear();
        self.plus.clear();
        self.row_label.clear();
        for slot in &mut self.row_slots {
            slot.disc.clear();
            slot.glyph.clear();
            slot.count.clear();
        }
        self.stale.extend(self.row_disc.take().map(|s| s.buffer));
        for sub in self.backdrops.iter_mut().flatten() {
            sub.clear();
        }
        self.stale
            .extend(self.backdrop_shown.take().map(|(_, b)| b));
        self.text.clear();
        self.notice.clear();
        self.stale
            .extend(self.drawer_shown.take().map(|s| s.buffer));
        self.drawer_lines.clear();
        self.drawer_dirty = true;
        self.stale
            .extend(self.body_shown.take().map(|b| b.image.buffer));
        self.stale
            .extend(self.notice_shown.take().map(|(_, s)| s.buffer));
        self.body_dirty = true;
        self.stale.extend(self.icon_buffers.drain().map(|(_, b)| b));
        self.stale.extend(self.dots.drain().map(|(_, b)| b));
        self.stale.extend(
            self.lines
                .drain(..)
                .filter_map(|(_, s)| s.map(|s| s.buffer)),
        );
        self.stale.extend(self.plus_shown.take().map(|(_, b, _)| b));
        self.icons.clear();
    }

    /// Show `notes`, animating the change (see deck.rs). The program keeps
    /// the open note among them.
    pub fn set_notes(&mut self, notes: Vec<Shown>) {
        // Notes that turned overdue since they were last shown pulse.
        let overdue = |due: &Option<DueShown>| due.as_ref().is_some_and(|d| d.overdue);
        let turned: Vec<String> = (notes.iter())
            .filter(|n| overdue(&n.due))
            .filter(|n| {
                self.notes
                    .get(&n.item.key)
                    .is_some_and(|old| !overdue(&old.due))
            })
            .map(|n| n.item.key.clone())
            .collect();
        self.notes = (notes.iter())
            .map(|n| {
                let view = NoteView {
                    content: n.content.clone(),
                    text: n.text,
                    files: n.files.clone(),
                    pin: n.pin.clone(),
                    color: n.color,
                    paper: n.item.look.color,
                    scratch: n.item.group == Group::Scratch,
                    due: n.due.clone(),
                    accent: n.item.look.accent,
                };
                (n.item.key.clone(), view)
            })
            .collect();
        self.items = notes.into_iter().map(|n| n.item).collect();
        self.sync_deck();
        for key in &turned {
            self.deck.pulse(key);
        }
        self.reset_file_views();
        // The open note went (removed, hidden): the keyboard goes back.
        if self.deck.open().is_none() {
            self.draft = None;
            self.focused = false;
            self.selecting = false;
            self.paste_serial = self.paste_serial.wrapping_add(1);
            self.palette_since = None;
            self.due_since = None;
            self.trash.clear();
            self.set_keyboard(KeyboardInteractivity::None);
        }
        let paths: Vec<PathBuf> = (self.notes.values())
            .flat_map(|n| n.files.iter().map(|f| f.path.clone()))
            .collect();
        for path in paths {
            self.ask_file_icons(&path);
        }
        self.body_dirty = true;
        self.kick();
    }

    pub fn rename_note(&mut self, from: &str, to: &str) {
        self.deck.rename(from, to);
        if let Some(view) = self.notes.remove(from) {
            self.notes.insert(to.to_owned(), view);
        }
        if let Some(slot) = self.slots.iter_mut().find(|s| s.key == from) {
            to.clone_into(&mut slot.key);
        }
        if let Some(d) = self.draft.as_mut().filter(|d| d.key == from) {
            to.clone_into(&mut d.key);
        }
        self.body_dirty = true;
    }

    /// A sheet arrived: show it.
    pub fn paper_ready(&mut self) {
        self.kick();
    }

    /// The paper buffers on screen, which must not be destroyed.
    pub fn shown_buffers(&self) -> Vec<WlBuffer> {
        (self.slots.iter())
            .filter_map(|s| s.shown.as_ref().map(|(_, s)| s.shape.buffer.clone()))
            .collect()
    }

    /// Destroy the replaced buffers no longer shown.
    pub fn trim(&mut self) {
        let shown: Vec<WlBuffer> = self
            .text_shown
            .iter()
            .chain(&self.attached)
            .cloned()
            .collect();
        self.stale.retain(|b| {
            let keep = shown.contains(b);
            if !keep {
                b.destroy();
            }
            keep
        });
    }

    // -- what the notes do -------------------------------------------------

    /// Open `key`: its sheet comes out and stays, and it asks for the
    /// keyboard. The program has already saved any previous draft.
    pub fn open(&mut self, core: &Core<WlNote>, key: &str) {
        // One drawer or note at a time.
        self.drop_drawer();
        let again = self.deck.open() == Some(key);
        self.cancel_reveal(core);
        self.deck.open_note(key);
        if !again {
            self.palette_since = None;
            self.due_since = None;
            self.trash.clear();
            self.reset_file_views();
            self.scroll = 0.0;
            self.layout = EditLayout::default();
            self.body_dirty = true;
            self.last_click = None;
            self.selecting = false;
            self.goal_x = None;
            self.paste_serial = self.paste_serial.wrapping_add(1);
        }
        self.set_keyboard(KeyboardInteractivity::Exclusive);
        self.kick();
    }

    /// Close the open note and give the keyboard back.
    pub fn close(&mut self) {
        self.drop_drawer();
        self.draft = None;
        self.body_dirty = true;
        self.selecting = false;
        self.focused = false;
        self.paste_serial = self.paste_serial.wrapping_add(1);
        self.deck.close();
        self.palette_since = None;
        self.due_since = None;
        self.trash.clear();
        self.reset_file_views();
        self.set_keyboard(KeyboardInteractivity::None);
        self.kick();
    }

    /// The open note's keyboard focus came: keep it until a click
    /// elsewhere takes it.
    pub fn keyboard_enter(&mut self) {
        self.focused = true;
        self.drawer_dirty = true;
        self.body_dirty = true;
        self.kick();
        if self.keyboard == KeyboardInteractivity::Exclusive {
            self.set_keyboard(KeyboardInteractivity::OnDemand);
        }
    }

    /// The keyboard went elsewhere, e.g. to a window clicked: the note
    /// stays open.
    pub fn keyboard_leave(&mut self) {
        self.focused = false;
        self.drawer_dirty = true;
        self.selecting = false;
        self.body_dirty = true;
        self.paste_serial = self.paste_serial.wrapping_add(1);
        self.kick();
    }

    fn set_keyboard(&mut self, k: KeyboardInteractivity) {
        if k == self.keyboard {
            return;
        }
        self.keyboard = k;
        self.layer.set_keyboard_interactivity(k);
        if self.configured {
            self.layer.commit();
        }
    }

    // -- input -------------------------------------------------------------

    pub fn pointer(
        &mut self,
        core: &mut Core<WlNote>,
        event: &PointerEvent,
        mods: &Modifiers,
    ) -> Vec<Act> {
        let (x, y) = event.position;
        match &event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                if self.dragged_file_note.is_some() {
                    return Vec::new();
                }
                if self.file_press.is_some() {
                    self.file_press_moved(core, x, y);
                    return Vec::new();
                }
                if self.selecting {
                    self.drag_selection(x, y);
                    return Vec::new();
                }
                match self.press.is_some() {
                    true => self.press_moved(core, x, y),
                    false => self.hover_at(core, x, y),
                }
                Vec::new()
            }
            PointerEventKind::Leave { .. } => {
                self.file_press = None;
                if self.dragged_file_note.is_none() {
                    self.pointer_left(core);
                }
                Vec::new()
            }
            PointerEventKind::Press { button, serial, .. } if *button == BTN_LEFT => {
                self.click(core, x, y, mods, *serial)
            }
            PointerEventKind::Release { button, .. } if *button == BTN_LEFT => {
                self.selecting = false;
                if let Some(p) = self.file_press.take() {
                    return self.file_release(core, p, x, y).into_iter().collect();
                }
                self.release(core, x, y)
            }
            PointerEventKind::Axis { vertical, .. } => {
                if self.drawer.is_some() && self.on_at(x, y) == On::Sheet(drawer::KEY.into()) {
                    self.drawer_wheel(axis(vertical));
                    return Vec::new();
                }
                if let On::Sheet(key) = self.on_at(x, y)
                    && let Some(area) = self.grid_area(&key)
                    && area.contains(x, y)
                {
                    let d = axis(vertical);
                    if self.showing_files(&key) {
                        if d != 0.0 {
                            self.scroll_files(&key, d.signum(), x, y);
                        }
                    } else if self.deck.open() == Some(&key) {
                        self.scroll_by(d);
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A key while the column has the keyboard.
    pub fn key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Vec<Act> {
        if self.drawer.is_some() {
            return self.drawer_key(event, mods);
        }
        let Some(key) = self.deck.open().map(String::from) else {
            return Vec::new();
        };
        self.paste_serial = self.paste_serial.wrapping_add(1);
        let sym = event.keysym;
        if mods.ctrl && !mods.alt {
            match sym {
                Keysym::v | Keysym::V => return vec![Act::Paste(self.paste_serial)],
                Keysym::c | Keysym::C | Keysym::x | Keysym::X => {
                    let cut = matches!(sym, Keysym::x | Keysym::X);
                    let (text, changed) = if let Some(d) = &mut self.draft {
                        let text = if cut {
                            d.editor.cut()
                        } else {
                            d.editor.selected().map(str::to_owned)
                        };
                        let changed = cut && text.is_some();
                        (text, changed)
                    } else {
                        (None, false)
                    };
                    self.follow_caret = changed;
                    self.draft_changed();
                    let mut acts: Vec<_> = text.into_iter().map(Act::Copy).collect();
                    if changed {
                        acts.push(Act::QueueSave);
                    }
                    return acts;
                }
                _ => {}
            }
        }
        match event.keysym {
            Keysym::Escape if self.palette_since.take().is_some() => self.kick(),
            Keysym::Escape if self.due_since.take().is_some() => self.kick(),
            Keysym::Escape if self.showing_files(&key) => {
                self.all_files[0] = None;
                self.kick();
            }
            Keysym::Escape => return vec![Act::Close],
            Keysym::Return | Keysym::KP_Enter if mods.ctrl => return vec![Act::Close],
            Keysym::e | Keysym::E if mods.ctrl => return vec![Act::Edit(key)],
            Keysym::s | Keysym::S if mods.ctrl => return vec![Act::Save],
            _ => return self.body_key(event, mods),
        }
        Vec::new()
    }

    fn body_key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Vec<Act> {
        if self.deck.open().is_some_and(|k| self.showing_files(k)) || mods.alt {
            return Vec::new();
        }
        let page = self.body_height() as f64 * 0.9;
        let Some(d) = &mut self.draft else {
            return Vec::new();
        };
        let e = &mut d.editor;
        let version = e.version();
        let (ctrl, shift) = (mods.ctrl, mods.shift);
        let goal = self.goal_x.take();
        match event.keysym {
            Keysym::Left | Keysym::KP_Left => e.left(ctrl, shift),
            Keysym::Right | Keysym::KP_Right => e.right(ctrl, shift),
            Keysym::Home | Keysym::KP_Home if ctrl => e.doc_start(shift),
            Keysym::End | Keysym::KP_End if ctrl => e.doc_end(shift),
            Keysym::Home | Keysym::KP_Home => e.home(shift),
            Keysym::End | Keysym::KP_End => e.end(shift),
            Keysym::Up
            | Keysym::KP_Up
            | Keysym::Down
            | Keysym::KP_Down
            | Keysym::Page_Up
            | Keysym::Page_Down => {
                let pos = e.line_col(e.caret());
                let up = matches!(event.keysym, Keysym::Up | Keysym::KP_Up | Keysym::Page_Up);
                let dir = if up { -1 } else { 1 };
                let dy = matches!(event.keysym, Keysym::Page_Up | Keysym::Page_Down)
                    .then_some(page * f64::from(dir));
                let x = goal
                    .or_else(|| self.layout.caret(pos).map(|c| c.x))
                    .unwrap_or(0.0);
                let (line, col) = self.layout.vertical(pos, x, dir, dy);
                e.set_caret(e.offset(line, col), shift);
                self.goal_x = Some(x);
            }
            Keysym::BackSpace => e.backspace(ctrl),
            Keysym::Delete | Keysym::KP_Delete => e.delete(ctrl),
            Keysym::Return | Keysym::KP_Enter => e.enter(),
            Keysym::Tab | Keysym::ISO_Left_Tab => {
                e.indent(shift || event.keysym == Keysym::ISO_Left_Tab)
            }
            Keysym::a | Keysym::A if ctrl => e.select_all(),
            Keysym::z | Keysym::Z if ctrl && !shift => {
                e.undo();
            }
            Keysym::z | Keysym::Z | Keysym::y | Keysym::Y if ctrl => {
                e.redo();
            }
            _ if !ctrl => {
                let Some(s) = &event.utf8 else {
                    return Vec::new();
                };
                e.insert(s);
            }
            _ => return Vec::new(),
        }
        let changed = e.version() != version;
        self.follow_caret = true;
        self.draft_changed();
        if changed {
            vec![Act::QueueSave]
        } else {
            Vec::new()
        }
    }

    pub fn paste(&mut self, serial: u64, text: &str) -> bool {
        if serial != self.paste_serial || self.deck.open().is_none() || text.is_empty() {
            return false;
        }
        if let Some(st) = &mut self.drawer {
            st.filter.insert(text);
            self.drawer_filter_changed();
            return false;
        }
        let Some(d) = &mut self.draft else {
            return false;
        };
        d.editor.paste(text);
        self.follow_caret = true;
        self.draft_changed();
        true
    }

    /// One of the column's timers fired.
    pub fn timer_due(&mut self, which: Timer) {
        match which {
            Timer::Reveal => {
                if let Some((key, _)) = self.reveal.take() {
                    self.deck.reveal_due(&key);
                }
            }
            Timer::Hide => {
                self.hide = None;
                if self.pointer.is_none() && self.dragged_file_note.is_none() {
                    self.deck.hide_due();
                }
            }
            Timer::Trash => self.drawer_dirty = true,
            Timer::RowRest => {
                self.row_timer = None;
                if self
                    .pointer
                    .is_some_and(|(x, y)| self.on_at(x, y) == On::Plus)
                {
                    self.set_row_mode(Mode::Rest);
                }
            }
            Timer::RowHide => {
                self.row_timer = None;
                self.set_row_mode(Mode::Closed);
            }
        }
        self.kick();
    }

    fn start_reveal(&mut self, core: &Core<WlNote>, key: String) {
        if self.reveal.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        self.cancel_reveal(core);
        if self.style.reveal_delay.is_zero() {
            self.reveal = None;
            self.deck.reveal_due(&key);
            return;
        }
        let surface = self.surface().clone();
        let timer = core.timer(self.style.reveal_delay, move |app: &mut WlNote, core| {
            app.column_timer(core, &surface, Timer::Reveal);
        });
        match timer {
            Ok(token) => self.reveal = Some((key, token)),
            Err(e) => warn!("{e}"),
        }
    }

    fn cancel_reveal(&mut self, core: &Core<WlNote>) {
        if let Some((_, token)) = self.reveal.take() {
            core.loop_handle.remove(token);
        }
    }

    fn start_hide(&mut self, core: &Core<WlNote>) {
        if self.hide.is_some() {
            return;
        }
        if self.style.hide_delay.is_zero() {
            self.deck.hide_due();
            return;
        }
        let surface = self.surface().clone();
        let timer = core.timer(self.style.hide_delay, move |app: &mut WlNote, core| {
            app.column_timer(core, &surface, Timer::Hide);
        });
        match timer {
            Ok(token) => self.hide = Some(token),
            Err(e) => warn!("{e}"),
        }
    }

    fn cancel_hide(&mut self, core: &Core<WlNote>) {
        if let Some(token) = self.hide.take() {
            core.loop_handle.remove(token);
        }
    }

    pub fn pointer_left(&mut self, core: &Core<WlNote>) {
        self.pointer = None;
        self.selecting = false;
        self.bar_hover = None;
        self.trash.clear();
        if let Some(st) = &mut self.drawer
            && st.hover.take().is_some()
        {
            self.drawer_dirty = true;
        }
        self.all_files = [None, None];
        if self.press.take().is_some() {
            self.deck.drag_cancel();
        }
        self.row_drag = false;
        self.update_row_hover(core);
        self.cancel_reveal(core);
        self.deck.point(On::Outside);
        self.start_hide(core);
        self.kick();
    }

    pub fn dragged_file_note(&self) -> Option<&str> {
        self.dragged_file_note.as_deref()
    }

    pub fn file_drag_done(&mut self, core: &Core<WlNote>) {
        self.dragged_file_note = None;
        if core.input.pointer_on(self.surface()) {
            let (x, y) = core.input.pointer_pos;
            self.hover_at(core, x, y);
        } else {
            self.pointer_left(core);
        }
    }

    pub fn file_hover(&mut self, core: &Core<WlNote>, x: f64, y: f64) {
        if self.dragged_file_note.is_none() {
            self.row_drag = true;
            self.hover_at(core, x, y);
        }
    }

    pub fn file_target(&self, x: f64, y: f64) -> Option<FileTarget> {
        if self.retiring {
            return None;
        }
        if let Some(button) = self.row_at(x, y) {
            return match button {
                Button::Window => Some(FileTarget::NewScratch),
                Button::Global => Some(FileTarget::NewGlobal),
                _ => None,
            };
        }
        if self.row_hold.is_some_and(|r| r.contains(x, y)) && self.on_at(x, y) != On::Plus {
            return None;
        }
        let key = match self.on_at(x, y) {
            // Only a template takes a drop (the drawer's other rows are notes).
            On::Sheet(key) if key == drawer::KEY => return self.drawer_file_target(x, y),
            On::Margin(key) if self.notes.get(&key).is_some_and(|n| n.scratch) => {
                return Some(FileTarget::NewScratch);
            }
            On::Sheet(key)
                if self.notes.get(&key).is_some_and(|n| n.scratch)
                    && self
                        .face(&key)
                        .is_some_and(|face| self.margin_rect(face).contains(x, y)) =>
            {
                return Some(FileTarget::NewScratch);
            }
            On::Sheet(key) | On::Margin(key) => key,
            On::Strip if self.deck.over_scratch(y) => return Some(FileTarget::NewScratch),
            On::Plus | On::Strip => return Some(FileTarget::New),
            On::Outside => return None,
        };
        let editor = (|| {
            let draft = self.draft.as_ref().filter(|d| d.key == key)?;
            let area = self.editor_rect(self.face(&key)?);
            if self.showing_files(&key) || !area.contains(x, y) {
                return None;
            }
            let (line, col) = self.layout.index_at(x - area.x, y - area.y + self.scroll);
            Some((
                draft.editor.offset(line, col),
                draft.editor.text().to_owned(),
            ))
        })();
        Some(FileTarget::Note { key, editor })
    }

    pub fn attach_files(
        &mut self,
        key: &str,
        editor: Option<(usize, String)>,
        paths: &[PathBuf],
    ) -> bool {
        let Some(draft) = self.draft.as_mut().filter(|d| d.key == key) else {
            return false;
        };
        let body = draft.editor.text();
        let (at, text) = match editor {
            Some((at, expected)) if expected == body => (at, files::insert(body, at, paths)),
            _ => (body.len(), files::append(body, paths, "\n")),
        };
        draft.editor.set_caret(at, false);
        draft.editor.paste(&text);
        self.paste_serial = self.paste_serial.wrapping_add(1);
        self.follow_caret = true;
        self.draft_changed();
        true
    }

    fn file_press_moved(&mut self, core: &mut Core<WlNote>, x: f64, y: f64) {
        let Some(p) = &self.file_press else { return };
        if (x - p.at.0).hypot(y - p.at.1) < DRAG_START {
            return;
        }
        let p = self.file_press.take().unwrap();
        let Some(note) = self.notes.get(&p.key).filter(|_| self.deck.held(&p.key)) else {
            return;
        };
        let paths: Vec<_> = match &self.selected_files {
            Some((key, set)) if *key == p.key && set.contains(&p.path) => note
                .files
                .iter()
                .filter(|f| set.contains(&f.path) && f.path.exists())
                .map(|f| f.path.clone())
                .collect(),
            _ if p.path.exists() => vec![p.path.clone()],
            _ => return,
        };
        if paths.is_empty() {
            return;
        }
        let buffer = self.file_buffer(&p.path, None);
        let icon = buffer.as_ref().map(|buffer| DragIcon {
            buffer,
            scale: self.scale,
            hotspot: (ICON as i32 / 2, ICON as i32 / 2),
        });
        match core.start_drag(
            self.surface(),
            p.serial,
            files::offer(&paths),
            DndAction::Copy,
            icon,
        ) {
            Ok(()) => {
                self.cancel_hide(core);
                self.dragged_file_note = Some(p.key);
            }
            Err(e) => warn!("cannot drag {}: {e}", p.path.display()),
        }
    }

    fn file_release(&mut self, core: &Core<WlNote>, p: FilePress, x: f64, y: f64) -> Option<Act> {
        let hit = self.bar_at(x, y);
        self.hover_at(core, x, y);
        let (key, BarHit::File(i)) = hit? else {
            return None;
        };
        if key != p.key || self.notes.get(&key)?.files.get(i)?.path != p.path {
            return None;
        }
        let act = if p.ctrl {
            if p.path.exists() {
                if !self.selected_files.as_ref().is_some_and(|(k, _)| *k == key) {
                    self.selected_files = Some((key, HashSet::new()));
                }
                let (_, set) = self.selected_files.as_mut().unwrap();
                if !set.remove(&p.path) {
                    set.insert(p.path);
                }
            }
            None
        } else {
            self.selected_files = None;
            Some(Act::File(p.path))
        };
        self.kick();
        act
    }

    /// The pointer is at (`x`, `y`): follow it with the levels, and the
    /// bars' hover.
    fn hover_at(&mut self, core: &Core<WlNote>, x: f64, y: f64) {
        let was_over_plus = self
            .pointer
            .is_some_and(|(x, y)| self.on_at(x, y) == On::Plus);
        self.pointer = Some((x, y));
        self.cancel_hide(core);
        let on = self.on_at(x, y);
        self.update_row_hover(core);
        let mut changed = self.leave_file_views(&on) || was_over_plus != (on == On::Plus);
        changed |= self.update_bar_hover(x, y);
        changed |= self.drawer.is_some() && self.update_drawer_hover(x, y);
        let before = (
            self.deck.level(),
            self.deck.revealed().map(String::from),
            self.deck.hover().map(String::from),
        );
        match self.deck.point(on) {
            Some(key) => self.start_reveal(core, key),
            None => self.cancel_reveal(core),
        }
        let after = (
            self.deck.level(),
            self.deck.revealed().map(String::from),
            self.deck.hover().map(String::from),
        );
        if before != after {
            changed = true;
        }
        if changed {
            self.kick();
        }
    }

    /// The pointer moved with a margin (or a revealed sheet) pressed: past
    /// `DRAG_START` along the edge, its margin is dragged.
    fn press_moved(&mut self, core: &Core<WlNote>, x: f64, y: f64) {
        self.pointer = Some((x, y));
        let Some(p) = &mut self.press else {
            return;
        };
        if !p.dragging {
            if (y - p.y).abs() < DRAG_START {
                return;
            }
            let (key, start) = (p.key.clone(), p.y + p.offset);
            p.dragging = true;
            if !self.deck.drag_start(&key, start) {
                // Not something to drag: neither a drag nor a click.
                self.press = None;
                return self.hover_at(core, x, y);
            }
            self.cancel_reveal(core);
            self.bar_hover = None;
        }
        if let Some(p) = &self.press {
            self.deck.drag_to(y + p.offset);
            self.kick();
        }
    }

    /// The button went up: a pressed margin opens (a revealed sheet does
    /// what a click on it does), a dragged one drops: into its place in
    /// the order, pinned to the context's app or unpinned if it went to
    /// the other group.
    fn release(&mut self, core: &Core<WlNote>, x: f64, y: f64) -> Vec<Act> {
        let Some(p) = self.press.take() else {
            return Vec::new();
        };
        let acts = match (p.dragging, p.revealed) {
            (false, false) => vec![Act::Open(p.key)],
            (false, true) => self.revealed_click(p.key, p.x, p.y).into_iter().collect(),
            (true, _) => match self.deck.drag_end() {
                Some(l) if matches!(l.group, Group::Scratch | Group::Drawer) => Vec::new(),
                Some(l) => {
                    // The slot is among the notes of the group it lands
                    // in, which the order is changed among.
                    let keys = (self.deck.cards().iter())
                        .filter(|c| !c.leaving && c.group == l.group)
                        .map(|c| c.key.clone())
                        .collect();
                    if self.notes.get(&l.key).is_some_and(|n| n.scratch) {
                        vec![Act::KeepAt(l.key, l.group, keys, l.gap)]
                    } else {
                        let mut acts = vec![Act::Reorder(l.key.clone(), keys, l.gap)];
                        if l.moved {
                            acts.push(match l.group {
                                Group::Window => Act::Pin(l.key, false),
                                Group::Global => Act::Unpin(l.key),
                                Group::Scratch | Group::Drawer => unreachable!(),
                            });
                        }
                        acts
                    }
                }
                None => Vec::new(),
            },
        };
        self.hover_at(core, x, y);
        self.kick();
        acts
    }

    fn click(
        &mut self,
        core: &Core<WlNote>,
        x: f64,
        y: f64,
        mods: &Modifiers,
        serial: u32,
    ) -> Vec<Act> {
        self.paste_serial = self.paste_serial.wrapping_add(1);
        let on = self.on_at(x, y);
        if let Some(button) = self.row_at(x, y) {
            return vec![Act::Row(button)];
        }
        if on == On::Strip && self.row_hold.is_some_and(|r| r.contains(x, y)) {
            return Vec::new();
        }
        if self.drawer.is_some() && on == On::Sheet(drawer::KEY.into()) {
            return self.drawer_click(core, x, y, mods);
        }
        let left_files = self.leave_file_views(&on);
        if self.update_bar_hover(x, y) || left_files {
            self.kick();
        }
        let hit = self.bar_hover.clone();
        if !matches!(hit, Some((_, BarHit::File(_) | BarHit::More)))
            && self.selected_files.take().is_some()
        {
            self.kick();
        }
        let keeps_palette = matches!(hit, Some((_, BarHit::Color | BarHit::Swatch(_))));
        let keeps_due = matches!(hit, Some((_, BarHit::Clock | BarHit::Due(_))));
        if (self.palette_since.is_some() && !keeps_palette)
            || (self.due_since.is_some() && !keeps_due)
        {
            self.palette_since = None;
            self.due_since = None;
            self.kick();
            return Vec::new();
        }
        let open = self.deck.open().map(String::from);
        let mut acts = Vec::new();
        if let Some((key, hit)) = hit {
            if let BarHit::File(i) = hit {
                if let Some(path) = self
                    .notes
                    .get(&key)
                    .and_then(|n| n.files.get(i))
                    .map(|f| f.path.clone())
                {
                    self.cancel_reveal(core);
                    self.file_press = Some(FilePress {
                        key,
                        path,
                        at: (x, y),
                        serial,
                        ctrl: mods.ctrl,
                    });
                }
            } else {
                acts.extend(self.bar_click(core, &key, hit, mods.shift));
            }
            return acts;
        }
        match on {
            On::Plus => acts.push(Act::New(mods.shift)),
            // Opened on release, unless it is dragged.
            On::Margin(k) => {
                self.cancel_reveal(core);
                let card_y = (self.deck.cards().iter())
                    .find(|c| c.key == k)
                    .map_or(y, |c| c.y.value);
                self.press = Some(Press {
                    key: k,
                    x,
                    y,
                    offset: card_y - y,
                    revealed: false,
                    dragging: false,
                });
            }
            On::Sheet(k) if open.as_deref() == Some(k.as_str()) => {
                acts.extend(self.open_click(&k, x, y, mods.shift));
            }
            On::Sheet(k) if self.showing_files(&k) => {}
            On::Sheet(k) => {
                self.cancel_reveal(core);
                self.press = Some(Press {
                    key: k,
                    x,
                    y,
                    offset: 0.0,
                    revealed: true,
                    dragging: false,
                });
            }
            On::Strip | On::Outside => {}
        }
        acts
    }

    /// What a click at (`x`, `y`) on the open note `key` does.
    fn open_click(&mut self, key: &str, x: f64, y: f64, shift: bool) -> Option<Act> {
        let face = self.face(key)?;
        if self.margin_rect(face).contains(x, y) {
            return Some(Act::Close);
        }
        let banner = self.notice_rect(face);
        if banner.contains(x, y) && self.draft.as_ref()?.conflict.is_some() {
            let (reload, keep) = self.notice_buttons(banner);
            return if reload.contains(x, y) {
                Some(Act::Reload)
            } else if keep.contains(x, y) {
                Some(Act::KeepMine)
            } else {
                None
            };
        }
        let area = self.editor_rect(face);
        if self.showing_files(key) || !area.contains(x, y) {
            return None;
        }
        let (dx, dy) = (x - area.x, y - area.y + self.scroll);
        let now = Instant::now();
        let count = self
            .last_click
            .filter(|(t, px, py, n)| {
                *n < 3
                    && now.duration_since(*t) < Duration::from_millis(400)
                    && (x - px).hypot(y - py) < 5.0
            })
            .map_or(1, |(_, _, _, n)| n + 1);
        self.last_click = Some((now, x, y, count));
        let draft = self.draft.as_mut()?;
        if !shift && count == 1 {
            match doc::hit(self.layout.hits(), dx, dy) {
                Some(Target::Task(n)) if draft.editor.toggle_task(n as usize) => {
                    self.draft_changed();
                    return Some(Act::QueueSave);
                }
                Some(Target::Link(n)) => {
                    return self
                        .links
                        .get(n as usize)
                        .map(|url| Act::OpenLink(key.to_owned(), url.clone()));
                }
                _ => {}
            }
        }
        let (line, col) = self.layout.index_at(dx, dy);
        let e = &mut draft.editor;
        let at = e.offset(line, col);
        match (shift, count) {
            (false, 2) => e.select_word(at),
            (false, 3) => e.select_line(at),
            _ => e.set_caret(at, shift),
        }
        self.selecting = true;
        self.goal_x = None;
        self.follow_caret = true;
        self.draft_changed();
        None
    }

    fn drag_selection(&mut self, x: f64, y: f64) {
        let Some(face) = self.deck.open().and_then(|k| self.face(k)) else {
            return;
        };
        let area = self.editor_rect(face);
        let (line, col): Pos = self.layout.index_at(x - area.x, y - area.y + self.scroll);
        if let Some(d) = &mut self.draft {
            let at = d.editor.offset(line, col);
            d.editor.set_caret(at, true);
            self.follow_caret = true;
            self.draft_changed();
        }
    }

    /// What a click at (`x`, `y`) on the revealed note `key` does: its
    /// files, checkboxes and links work; anywhere else opens it.
    fn revealed_click(&self, key: String, x: f64, y: f64) -> Option<Act> {
        if self.showing_files(&key) {
            return None;
        }
        let target = (self.slots.iter().find(|s| s.key == key))
            .and_then(|s| Some((s.paper, s.shown.as_ref()?)))
            .and_then(|(r, (_, sheet))| doc::hit(&sheet.hits, x - r.x, y - r.y));
        Some(match target {
            Some(Target::Task(n)) => Act::Task(key, n),
            Some(Target::Link(n)) => Act::Link(key, n),
            None => Act::Open(key),
        })
    }

    fn bar_click(
        &mut self,
        core: &Core<WlNote>,
        key: &str,
        hit: BarHit,
        shift: bool,
    ) -> Option<Act> {
        let note = self.notes.get(key)?;
        match hit {
            BarHit::File(_) => None,
            BarHit::RemoveFile(i) => {
                Some(Act::RemoveFile(key.into(), note.files.get(i)?.path.clone()))
            }
            BarHit::More => {
                let i = self.view_index(key);
                self.all_files[i] = (!self.showing_files(key)).then(|| (key.into(), 0));
                self.kick();
                None
            }
            BarHit::Color => {
                self.palette_since = if self.palette_since.is_some() || note.pin == Pin::Locked {
                    None
                } else {
                    Some(Instant::now())
                };
                self.due_since = None;
                self.kick();
                None
            }
            BarHit::Clock => {
                self.due_since = if self.due_since.is_some() || note.pin == Pin::Locked {
                    None
                } else {
                    Some(Instant::now())
                };
                self.palette_since = None;
                self.kick();
                None
            }
            BarHit::Due(choice) => {
                self.due_since = None;
                self.kick();
                match choice {
                    None => Some(Act::Due(key.into(), None)),
                    Some(preset) => {
                        let now = due::now();
                        let all = due::presets(now, &Local, self.style.morning, self.style.evening);
                        let (_, when) = all.into_iter().find(|(p, _)| *p == preset)?;
                        Some(Act::Due(
                            key.into(),
                            Some(when.datetime(&Local).to_string()),
                        ))
                    }
                }
            }
            BarHit::Swatch(color) => {
                let changed = note.color != color;
                self.palette_since = None;
                self.due_since = None;
                self.kick();
                changed.then(|| Act::Color(key.into(), color))
            }
            BarHit::Trash => {
                let confirmed = self.trash.click(Instant::now());
                if !confirmed {
                    let surface = self.surface().clone();
                    if let Err(e) = core.timer(bar::TRASH_TIMEOUT, move |app: &mut WlNote, core| {
                        app.column_timer(core, &surface, Timer::Trash);
                    }) {
                        warn!("trash confirmation: {e}");
                    }
                }
                self.kick();
                confirmed.then(|| Act::Trash(key.into()))
            }
            BarHit::Archive => Some(Act::Hide(key.to_string())),
            BarHit::Pin => match &note.pin {
                Pin::Unpinned(_) => Some(Act::Pin(key.to_string(), shift)),
                Pin::Pinned { .. } => Some(Act::Unpin(key.to_string())),
                Pin::Keep => Some(Act::Keep(key.to_string(), shift)),
                Pin::None | Pin::Locked => None,
            },
        }
    }

    fn update_bar_hover(&mut self, x: f64, y: f64) -> bool {
        let next = self.bar_at(x, y);
        if next == self.bar_hover {
            return false;
        }
        self.trash.clear();
        self.bar_hover = next;
        true
    }

    fn view_index(&self, key: &str) -> usize {
        usize::from(self.deck.open() != Some(key))
    }

    fn showing_files(&self, key: &str) -> bool {
        self.all_files[self.view_index(key)]
            .as_ref()
            .is_some_and(|(k, _)| k == key)
    }

    fn reset_file_views(&mut self) {
        for (view, key) in self
            .all_files
            .iter_mut()
            .zip([self.deck.open(), self.deck.revealed()])
        {
            if view.as_ref().is_some_and(|(k, _)| {
                Some(k.as_str()) != key || self.notes.get(k).is_none_or(|n| n.files.is_empty())
            }) {
                *view = None;
            }
        }
    }

    fn leave_file_views(&mut self, on: &On) -> bool {
        let mut changed = false;
        for view in &mut self.all_files {
            if view
                .as_ref()
                .is_some_and(|(k, _)| !matches!(on, On::Sheet(s) if s == k))
            {
                *view = None;
                changed = true;
            }
        }
        changed
    }

    fn grid_area(&self, key: &str) -> Option<Rect> {
        let face = self.face(key)?;
        let area = self.text_rect(face);
        let hit = self.slots.iter().find(|s| s.key == key)?.hit?;
        intersect(area, hit)
    }

    fn file_grid(&self, key: &str) -> Option<bar::Grid> {
        let (k, row) = self.all_files[self.view_index(key)].as_ref()?;
        (k == key).then_some(())?;
        Some(bar::grid(
            self.grid_area(key)?,
            self.notes.get(key)?.files.len(),
            *row,
        ))
    }

    fn scroll_files(&mut self, key: &str, direction: f64, x: f64, y: f64) {
        let Some(grid) = self.file_grid(key) else {
            return;
        };
        let i = self.view_index(key);
        if let Some((_, row)) = &mut self.all_files[i] {
            let old = (*row).min(grid.last_row);
            *row = old
                .saturating_add_signed(direction as isize)
                .min(grid.last_row);
            if *row != old {
                self.update_bar_hover(x, y);
                self.kick();
            }
        }
    }

    fn file_hit(&self, key: &str, i: usize, icon: Rect, x: f64, y: f64) -> BarHit {
        let over = matches!(self.bar_hover.as_ref(), Some((k, BarHit::File(j) | BarHit::RemoveFile(j))) if k == key && *j == i);
        if over && !self.notes[key].files[i].path.exists() && remove_rect(icon).contains(x, y) {
            BarHit::RemoveFile(i)
        } else {
            BarHit::File(i)
        }
    }

    /// What the pointer at (`x`, `y`) is on, topmost first.
    fn on_at(&self, x: f64, y: f64) -> On {
        if self.row_at(x, y).is_some() {
            return On::Strip;
        }
        for card in self.deck.paint_order().into_iter().rev() {
            let hit = (self.slots.iter().find(|s| s.key == card.key)).and_then(|s| s.hit);
            if card.present() && hit.is_some_and(|r| r.contains(x, y)) {
                return match self.deck.held(&card.key) {
                    true => On::Sheet(card.key.clone()),
                    false => On::Margin(card.key.clone()),
                };
            }
        }
        if self.plus_rect.is_some_and(|r| r.contains(x, y)) {
            return On::Plus;
        }
        if self.row_hold.is_some_and(|r| r.contains(x, y)) {
            return On::Strip;
        }
        match self.band_rects().iter().any(|r| r.contains(x, y)) {
            true => On::Strip,
            false => On::Outside,
        }
    }

    /// The file bar part at (`x`, `y`), of the topmost sheet there.
    fn bar_at(&self, x: f64, y: f64) -> Option<(String, BarHit)> {
        let On::Sheet(key) = self.on_at(x, y) else {
            return None;
        };
        if self.deck.open() == Some(&key) && self.open_alpha(&key) <= 0.0 {
            return None;
        }
        let layout = self.bar_layout(&key)?;
        let near = |r: &Rect| r.outset(layout.hit_pad).contains(x, y);
        if !layout.color.is_some_and(|r| r.contains(x, y))
            && let Some((color, _)) = layout.swatches.iter().rev().find(|(_, r)| r.contains(x, y))
        {
            return Some((key, BarHit::Swatch(*color)));
        }
        if !layout.clock.is_some_and(|r| r.contains(x, y))
            && let Some((choice, _)) = layout.due_row.iter().rev().find(|(_, r)| r.contains(x, y))
        {
            return Some((key, BarHit::Due(*choice)));
        }
        for (rect, hit) in [
            (layout.color, BarHit::Color),
            (layout.clock, BarHit::Clock),
            (layout.trash, BarHit::Trash),
            (layout.archive, BarHit::Archive),
            (layout.pin, BarHit::Pin),
            (layout.more.map(|(_, r)| r), BarHit::More),
        ] {
            if rect.is_some_and(|r| near(&r)) {
                return Some((key, hit));
            }
        }
        for (i, r) in layout.icons {
            if near(&r) {
                let hit = self.file_hit(&key, i, r, x, y);
                return Some((key, hit));
            }
        }
        if let Some(grid) = self.file_grid(&key)
            && let Some((i, cell)) = grid.cells.into_iter().find(|(_, cell)| cell.contains(x, y))
        {
            let hit = self.file_hit(&key, i, grid_icon(cell), x, y);
            return Some((key, hit));
        }
        None
    }

    fn scroll_by(&mut self, d: f64) {
        let max = self.max_scroll();
        let scroll = (self.scroll + d).clamp(0.0, max);
        if scroll != self.scroll {
            self.scroll = scroll;
            self.kick();
        }
    }

    fn notice_height(&self) -> f64 {
        if self.notice_text().is_some() {
            self.style.notice_line() * if self.has_conflict() { 2.0 } else { 1.0 }
        } else {
            0.0
        }
    }

    fn has_conflict(&self) -> bool {
        self.draft.as_ref().is_some_and(|d| d.conflict.is_some())
    }

    fn notice_text(&self) -> Option<&str> {
        let draft = self.draft.as_ref()?;
        if draft.conflict.is_some() {
            Some("Changed on disk")
        } else {
            draft.error.as_deref().or(draft.base().error.as_deref())
        }
    }

    fn body_height(&self) -> i32 {
        (self.style.metrics.sheet_height - PAD_Y - self.style.file_bar - self.notice_height())
            .floor()
            .max(1.0) as i32
    }

    fn max_scroll(&self) -> f64 {
        (self.layout.height() - self.body_height() as f64).max(0.0)
    }

    pub fn draft_changed(&mut self) {
        self.body_dirty = true;
        self.kick();
    }

    fn edit_style(&self) -> Option<EditStyle> {
        let note = self.notes.get(self.deck.open()?)?;
        Some(EditStyle {
            font: self.style.font.clone(),
            heading_font: Some(UI_FONT.into()),
            size: self.style.font_size,
            color: note.text,
            link: self.style.link,
            selection: (self.style.link & !0xff) | 0x48,
        })
    }

    fn layout_body(&mut self, style: &EditStyle) {
        let Some(draft) = &self.draft else { return };
        let mut fonts = self.fonts.borrow_mut();
        let Some(fonts) = fonts.as_mut() else { return };
        let editor = &draft.editor;
        let active = if self.focused {
            editor.active_lines()
        } else {
            0..0
        };
        let (lines, links) = markdown::source_lines(editor.text(), active);
        self.links = links;
        fonts.edit_layout(
            &mut self.layout,
            lines,
            style,
            self.style.content_w() as i32,
            self.scale,
        );
    }

    fn draw_body(&mut self) {
        let Some(style) = self.edit_style() else {
            return;
        };
        if self.body_dirty {
            self.layout_body(&style);
        }
        let Some(draft) = &self.draft else { return };
        let height = self.body_height();
        let editor = &draft.editor;
        let caret = self.focused.then(|| editor.line_col(editor.caret()));
        if self.follow_caret
            && let Some(c) = caret.and_then(|p| self.layout.caret(p))
        {
            self.scroll = self.scroll.min(c.y).max(c.y + c.h - height as f64);
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
        if !self.body_dirty
            && self
                .body_shown
                .as_ref()
                .is_some_and(|b| b.scroll == self.scroll)
        {
            return;
        }
        let selection = caret
            .and_then(|_| editor.selection())
            .map(|r| (editor.line_col(r.start), editor.line_col(r.end)));
        let drawn = {
            let mut fonts = self.fonts.borrow_mut();
            let Some(fonts) = fonts.as_mut() else { return };
            fonts.edit_draw(&self.layout, &style, self.scroll, height, selection, caret)
        };
        let buffer = match shm_buffer(&self.g, &drawn, &self.qh) {
            Ok(b) => b,
            Err(e) => return warn!("editor buffer: {e}"),
        };
        let s = self.scale as f64;
        let image = Sized {
            buffer,
            w: drawn.width as f64 / s,
            h: drawn.height as f64 / s,
        };
        self.stale.extend(
            self.body_shown
                .replace(BodyShown {
                    image,
                    scroll: self.scroll,
                })
                .map(|b| b.image.buffer),
        );
        self.body_dirty = false;
        self.follow_caret = false;
    }

    /// The fonts arrived: the editor and drawer can be drawn.
    pub fn fonts_ready(&mut self) {
        self.kick();
    }

    // -- icons and names -----------------------------------------------------

    /// The icon `names` (the first found) at the bar's size.
    fn named(&self, names: &[&str]) -> NamedIcon {
        NamedIcon {
            name: names.first().map_or(String::new(), |n| n.to_string()),
            fallbacks: names.iter().skip(1).map(|n| n.to_string()).collect(),
            size: ICON as u32 * self.scale as u32,
            theme: None,
            shadow: None,
            scale: self.scale,
        }
    }

    fn generic_icon(&self) -> NamedIcon {
        self.named(&FILE_ICONS)
    }

    fn pin_icon(&self, unpin: bool) -> NamedIcon {
        self.named(&[if unpin {
            "builtin:unpin"
        } else {
            "builtin:pin"
        }])
    }

    fn archive_icon(&self) -> NamedIcon {
        self.named(&["builtin:archive"])
    }

    fn trash_icon(&self) -> NamedIcon {
        self.named(&["builtin:trash"])
    }

    fn clock_icon(&self) -> NamedIcon {
        self.named(&["builtin:clock"])
    }

    /// The due choices to offer now, in the order they are laid out:
    /// the presets still to come, and to clear the due time if there is
    /// one.
    fn due_choices(&self, has_due: bool) -> Vec<Option<Preset>> {
        let now = due::now();
        let presets = due::presets(now, &Local, self.style.morning, self.style.evening);
        (presets.into_iter().map(|(p, _)| Some(p)))
            .chain(has_due.then_some(None))
            .collect()
    }

    /// A file's own icon, once its names are known.
    fn file_icon(&self, path: &PathBuf) -> Option<NamedIcon> {
        let names = self.file_icons.get(path)?;
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        (!names.is_empty()).then(|| self.named(&names))
    }

    fn ask_standard_icons(&self) {
        for key in [
            self.generic_icon(),
            self.pin_icon(false),
            self.pin_icon(true),
            self.archive_icon(),
            self.trash_icon(),
            self.clock_icon(),
            self.named(&["builtin:close"]),
        ] {
            self.assets.named_icon(&key);
        }
    }

    fn ask_file_icons(&mut self, path: &PathBuf) {
        if !self.file_icons.contains_key(path) && self.asked_icons.insert(path.clone()) {
            self.assets.file_icons(path, false);
        }
    }

    /// A file's icon names arrived (its thumbnail, else its type's).
    pub fn file_icons_ready(&mut self, path: PathBuf, names: Vec<String>) {
        self.asked_icons.remove(&path);
        self.file_icons.insert(path.clone(), names);
        if let Some(key) = self.file_icon(&path) {
            self.assets.named_icon(&key);
        }
        self.kick();
    }

    /// An icon arrived (None: there is none).
    pub fn icon_ready(&mut self, key: NamedIcon, image: Option<Arc<Image>>) {
        let scale = self.scale as u32;
        let bar_icon = key.size == ICON as u32 * scale;
        let row_icon = [ROW_ICON * scale, ROW_COUNT_ICON * scale].contains(&key.size)
            && Button::ALL.iter().any(|button| key.name == button.icon());
        if key.shadow.is_some() || !(bar_icon || row_icon) {
            return;
        }
        self.icons.insert(key, image);
        // The drawer's trash icon may be this one.
        self.drawer_dirty = true;
        self.kick();
    }

    /// The buffer of the icon `key`, `tint`ed (or as it is): None until it
    /// arrived, or if there is none.
    fn icon_buffer(&mut self, key: &NamedIcon, tint: Option<u32>) -> Option<WlBuffer> {
        // Thumbnails show as they are.
        let tint = tint.filter(|_| !key.name.contains("/thumbnails/"));
        if let Some(b) = self.icon_buffers.get(&(key.clone(), tint)) {
            return Some(b.clone());
        }
        let image = match self.icons.get(key) {
            Some(image) => image.clone()?,
            None => {
                self.assets.named_icon(key);
                return None;
            }
        };
        let image = match tint {
            Some(c) => tinted(&image, c),
            None => (*image).clone(),
        };
        let buffer = shm_buffer(&self.g, &image, &self.qh)
            .inspect_err(|e| warn!("icon buffer: {e}"))
            .ok()?;
        self.icon_buffers
            .insert((key.clone(), tint), buffer.clone());
        Some(buffer)
    }

    /// A file's icon: its own, else a generic one.
    fn file_buffer(&mut self, path: &PathBuf, tint: Option<u32>) -> Option<WlBuffer> {
        let own = self.file_icon(path);
        if let Some(own) = own
            && let Some(b) = self.icon_buffer(&own, tint)
        {
            return Some(b);
        }
        let generic = self.generic_icon();
        self.icon_buffer(&generic, tint)
    }

    fn dot(&mut self, paper: u32, ink: u32, selected: bool) -> Option<WlBuffer> {
        let key = (paper, ink, selected);
        if let Some(buffer) = self.dots.get(&key) {
            return Some(buffer.clone());
        }
        let size = ICON as u32 * self.scale as u32;
        let mut rgba = Vec::new();
        for y in 0..size {
            for x in 0..size {
                let coord = |i| ((i as f64 + 0.5) / size as f64 - 0.5) * ICON;
                let r = coord(x).hypot(coord(y));
                let (color, coverage) = if r <= 4.5 {
                    (paper, 1.0)
                } else if selected && r >= 6.0 {
                    (
                        ink,
                        ((r - 6.0).min(7.8 - r) * self.scale as f64 + 0.5).clamp(0.0, 1.0),
                    )
                } else {
                    (ink, (5.7 - r).clamp(0.0, 1.0))
                };
                let [r, g, b, a] = color.to_be_bytes();
                rgba.extend([r, g, b, (a as f64 * coverage).round() as u8]);
            }
        }
        let image = Image {
            width: size,
            height: size,
            data: wl_shell_render::image::to_argb8888(&rgba),
        };
        let buffer = shm_buffer(&self.g, &image, &self.qh)
            .inspect_err(|e| warn!("color dot: {e}"))
            .ok()?;
        if self.dots.len() >= 64 {
            self.stale.extend(self.dots.drain().map(|(_, b)| b));
        }
        self.dots.insert(key, buffer.clone());
        Some(buffer)
    }

    /// A name label: None until the worker drew it (it is asked for).
    fn line(&mut self, key: LineKey) -> Option<Sized> {
        if let Some(i) = self.lines.iter().position(|(k, _)| *k == key) {
            let entry = self.lines.remove(i);
            let sized = entry.1.clone();
            self.lines.insert(0, entry);
            return sized;
        }
        let request = PillRequest {
            text: key.text.clone(),
            spans: Vec::new(),
            max_width: key.width.max(1),
            scale: self.scale,
            style: key.style(&self.style),
        };
        if key.tooltip {
            self.assets.pill(0, request);
        } else {
            self.assets.line(0, request);
        }
        self.lines.insert(0, (key, None));
        while self.lines.len() > LINES {
            if let Some((_, Some(s))) = self.lines.pop() {
                self.stale.push(s.buffer);
            }
        }
        None
    }

    /// A line of text from the worker: a name label, if still wanted.
    pub fn line_ready(&mut self, request: &PillRequest, pill: &text::Pill) {
        if request.scale != self.scale {
            return;
        }
        let mut changed = false;
        for (key, sized) in &mut self.lines {
            let same = key.text == request.text
                && key.width.max(1) == request.max_width
                && key.style(&self.style) == request.style;
            if !same || sized.is_some() {
                continue;
            }
            let mut image = pill.image.clone();
            if key.strike {
                strike(&mut image, key.color, self.scale);
            }
            match shm_buffer(&self.g, &image, &self.qh) {
                Ok(buffer) => {
                    *sized = Some(Sized {
                        buffer,
                        w: pill.width as f64,
                        h: pill.height as f64,
                    });
                    changed = true;
                }
                Err(e) => warn!("label buffer: {e}"),
            }
        }
        if changed {
            self.kick();
        }
    }

    // -- the drawer --------------------------------------------------------

    /// What the open drawer lists, if one is open.
    pub fn drawer_kind(&self) -> Option<drawer::Kind> {
        self.drawer.as_ref().map(|st| st.drawer.kind())
    }

    /// The notes to show, and the drawer if it is open, as the deck's
    /// cards: the drawer is the last one, a sheet of its own.
    fn sync_deck(&mut self) {
        let mut items = self.items.clone();
        if self.drawer.is_some() {
            items.push(Item {
                key: drawer::KEY.into(),
                look: Look {
                    color: self.style.drawer,
                    tilt: 0.0,
                    accent: None,
                },
                group: Group::Drawer,
                cross: false,
            });
        }
        self.deck.set(items);
    }

    /// Open a drawer of `entries`: its sheet comes out and stays, and it
    /// asks for the keyboard. The program has already saved any draft.
    pub fn open_drawer(&mut self, kind: drawer::Kind, entries: Vec<drawer::Entry>) {
        // A note or another drawer goes.
        self.close();
        self.drawer_kind = kind;
        self.drawer = Some(DrawerState {
            drawer: Drawer::new(kind, entries),
            filter: FilterField::new(""),
            field_scroll: 0.0,
            hover: None,
            wheel: 0.0,
        });
        self.deck.open_note(drawer::KEY);
        self.sync_deck();
        self.drawer_dirty = true;
        self.set_keyboard(KeyboardInteractivity::Exclusive);
        self.kick();
    }

    /// The drawer `kind`, if open, lists `entries` now.
    pub fn set_drawer_entries(&mut self, kind: drawer::Kind, entries: Vec<drawer::Entry>) {
        if let Some(st) = self.drawer.as_mut().filter(|st| st.drawer.kind() == kind) {
            st.drawer.set_entries(entries);
            self.drawer_changed();
        }
    }

    /// Close the drawer, if open, without giving the keyboard back (the
    /// caller does, or asks for it again): its card leaves.
    fn drop_drawer(&mut self) {
        if self.drawer.take().is_none() {
            return;
        }
        self.stale
            .extend(self.drawer_shown.take().map(|s| s.buffer));
        self.trash.clear();
        if self.deck.open() == Some(drawer::KEY) {
            self.deck.close();
        }
        self.sync_deck();
    }

    /// The drawer's margin: its name, turned.
    fn drawer_margin(&self, role: Role) -> paper::Margin {
        let m = &self.style.metrics;
        let room = match role {
            Role::Sheet => m.sheet_height,
            _ => m.header_length * 0.7,
        };
        let ink = self.drawer_ink();
        paper::Margin {
            width: m.header as i32,
            ink,
            crease: true,
            label: Some(paper::Label {
                text: self.drawer_kind.label().to_uppercase(),
                style: self.style.small((ink & !0xff) | 0xc8),
                spacing: LABEL_SPACING,
                top: LABEL_PAD as i32,
                len: (room - 2.0 * LABEL_PAD).max(8.0) as i32,
            }),
        }
    }

    /// The text's color on the drawer's paper.
    fn drawer_ink(&self) -> u32 {
        Config::ink_on(self.style.drawer)
    }

    /// Something about the drawer changed: draw it again.
    fn drawer_changed(&mut self) {
        self.drawer_dirty = true;
        self.kick();
    }

    /// Where the drawer's body is on an open sheet.
    fn drawer_area(&self, face: Rect) -> Rect {
        let body = self.body_rect(face);
        Rect::new(
            body.x + PAD_X,
            face.y + PAD_Y,
            self.style.content_w(),
            (face.h - 2.0 * PAD_Y).max(0.0),
        )
    }

    /// The drawer's body and what is where on it (from its corner).
    fn drawer_layout(&self) -> Option<(Rect, drawer::Layout)> {
        let st = self.drawer.as_ref()?;
        let face = self.face(drawer::KEY)?;
        let area = self.drawer_area(face);
        let size = self.style.font_size;
        let (row, foot) = (drawerview::row_height(size), drawerview::foot_height(size));
        let layout = drawer::layout(area.w, area.h, row, foot, st.drawer.len());
        Some((area, layout))
    }

    /// The same, and the drawer is told how many rows that has room for.
    fn drawer_geometry(&mut self) -> Option<(Rect, drawer::Layout)> {
        let (area, layout) = self.drawer_layout()?;
        let st = self.drawer.as_mut()?;
        if st.drawer.set_capacity(layout.capacity()) {
            self.drawer_dirty = true;
        }
        Some((area, layout))
    }

    /// What a file dropped at (`x`, `y`) on the open drawer makes: a
    /// note from the template there, on the Templates drawer.
    fn drawer_file_target(&self, x: f64, y: f64) -> Option<FileTarget> {
        let st = self.drawer.as_ref()?;
        if st.drawer.kind() != drawer::Kind::Templates {
            return None;
        }
        let (area, layout) = self.drawer_layout()?;
        let (x, y) = (x - area.x, y - area.y);
        let shown = st.drawer.window().len();
        let n = (layout.rows.iter().take(shown)).position(|r| r.contains(x, y))?;
        Some(FileTarget::Template(st.drawer.row(n)?.key.clone()))
    }

    /// What of the drawer is at (`x`, `y`).
    fn drawer_hit(&mut self, x: f64, y: f64) -> Option<DrawerHit> {
        let (area, layout) = self.drawer_geometry()?;
        let (x, y) = (x - area.x, y - area.y);
        if !Rect::new(0.0, 0.0, area.w, area.h).contains(x, y) {
            return None;
        }
        if layout.scope.contains(x, y) {
            return Some(DrawerHit::Scope);
        }
        if layout.filter.contains(x, y) {
            return Some(DrawerHit::Filter);
        }
        let st = self.drawer.as_ref()?;
        let shown = st.drawer.window().len();
        let has_action = st.drawer.kind().row_action().is_some();
        for (n, row) in layout.rows.iter().enumerate().take(shown) {
            if row.contains(x, y) {
                let on_action =
                    has_action && drawer::trash_rect(*row, drawerview::ICON).contains(x, y);
                return Some(match on_action {
                    true => DrawerHit::Action(n),
                    false => DrawerHit::Row(n),
                });
            }
        }
        None
    }

    /// The pointer moved over the drawer (or it moved under it). Returns
    /// whether what is hovered changed.
    fn update_drawer_hover(&mut self, x: f64, y: f64) -> bool {
        let hit = self.drawer_hit(x, y);
        let Some(st) = &mut self.drawer else {
            return false;
        };
        if st.hover == hit {
            return false;
        }
        st.hover = hit;
        // An armed trash button is for the row it was armed on.
        self.trash.clear();
        self.drawer_dirty = true;
        true
    }

    /// A click at (`x`, `y`) on the drawer's sheet.
    fn drawer_click(&mut self, core: &Core<WlNote>, x: f64, y: f64, mods: &Modifiers) -> Vec<Act> {
        let Some(face) = self.face(drawer::KEY) else {
            return Vec::new();
        };
        if self.margin_rect(face).contains(x, y) {
            return vec![Act::Close];
        }
        let Some(hit) = self.drawer_hit(x, y) else {
            return Vec::new();
        };
        let row_key = |st: &DrawerState, n: usize| st.drawer.row(n).map(|e| e.key.clone());
        match hit {
            DrawerHit::Scope => {
                if let Some(st) = &mut self.drawer {
                    st.drawer.toggle_scope();
                }
                self.drawer_changed();
                Vec::new()
            }
            DrawerHit::Filter => {
                self.drawer_place_caret(x);
                Vec::new()
            }
            DrawerHit::Row(n) => {
                let key = self.drawer.as_mut().and_then(|st| {
                    st.drawer.select(n);
                    row_key(st, n)
                });
                let kind = self.drawer_kind();
                self.drawer_changed();
                // Ctrl, on a hidden note: show it again and leave the
                // drawer, to bring back several.
                (kind.zip(key))
                    .map(|(kind, k)| pick(kind, k, mods.ctrl))
                    .into_iter()
                    .collect()
            }
            DrawerHit::Action(n) => {
                let Some(key) = self.drawer.as_ref().and_then(|st| row_key(st, n)) else {
                    return Vec::new();
                };
                match self.drawer_kind().and_then(drawer::Kind::row_action) {
                    Some(drawer::RowAction::Clear) => vec![Act::Due(key, None)],
                    Some(drawer::RowAction::Trash) => {
                        let confirmed = self.trash.click(Instant::now());
                        if !confirmed {
                            let surface = self.surface().clone();
                            if let Err(e) =
                                core.timer(bar::TRASH_TIMEOUT, move |app: &mut WlNote, core| {
                                    app.column_timer(core, &surface, Timer::Trash);
                                })
                            {
                                warn!("trash confirmation: {e}");
                            }
                        }
                        self.drawer_changed();
                        confirmed.then_some(Act::Trash(key)).into_iter().collect()
                    }
                    None => Vec::new(),
                }
            }
        }
    }

    /// The wheel moved `d` over the drawer: whole rows scroll.
    fn drawer_wheel(&mut self, d: f64) {
        let row = drawerview::row_height(self.style.font_size);
        let Some(st) = &mut self.drawer else {
            return;
        };
        st.wheel += d;
        let rows = (st.wheel / row).trunc();
        if rows != 0.0 {
            st.wheel -= rows * row;
            st.drawer.scroll(rows as isize);
            self.drawer_changed();
        }
    }

    /// The filter's text changed: list what matches it.
    fn drawer_filter_changed(&mut self) {
        if let Some(st) = &mut self.drawer {
            let text = st.filter.text.clone();
            st.drawer.set_filter(&text);
        }
        self.drawer_changed();
    }

    /// A key while the drawer is open: typing filters, the arrows move,
    /// Enter picks (Ctrl: and the drawer stays), Tab switches the scope,
    /// Escape closes.
    fn drawer_key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Vec<Act> {
        self.paste_serial = self.paste_serial.wrapping_add(1);
        let serial = self.paste_serial;
        let sym = event.keysym;
        let Some(st) = &mut self.drawer else {
            return Vec::new();
        };
        let (ctrl, shift) = (mods.ctrl, mods.shift);
        let anchor = st.filter.anchor();
        let mut acts = Vec::new();
        let mut edited = false;
        let mut moved = true;
        match sym {
            Keysym::Escape => return vec![Act::Close],
            Keysym::Return | Keysym::KP_Enter => {
                let kind = st.drawer.kind();
                return (st.drawer.selected())
                    .map(|e| pick(kind, e.key.clone(), ctrl))
                    .into_iter()
                    .collect();
            }
            Keysym::v | Keysym::V if ctrl => return vec![Act::Paste(serial)],
            Keysym::c | Keysym::C | Keysym::x | Keysym::X if ctrl => {
                let cut = matches!(sym, Keysym::x | Keysym::X);
                let text = match cut {
                    true => st.filter.cut(),
                    false => st.filter.selected().map(str::to_owned),
                };
                edited = cut && text.is_some();
                acts.extend(text.map(Act::Copy));
            }
            Keysym::a | Keysym::A if ctrl => st.filter.select_all(),
            Keysym::u | Keysym::U if ctrl => {
                st.filter.select_all();
                edited = st.filter.cut().is_some();
            }
            Keysym::n | Keysym::N if ctrl => st.drawer.step(1),
            Keysym::p | Keysym::P if ctrl => st.drawer.step(-1),
            Keysym::Down | Keysym::KP_Down => st.drawer.step(1),
            Keysym::Up | Keysym::KP_Up => st.drawer.step(-1),
            Keysym::Page_Down => st.drawer.step(st.drawer.page()),
            Keysym::Page_Up => st.drawer.step(-st.drawer.page()),
            Keysym::Home | Keysym::KP_Home if ctrl => st.drawer.jump(false),
            Keysym::End | Keysym::KP_End if ctrl => st.drawer.jump(true),
            Keysym::Tab | Keysym::ISO_Left_Tab => st.drawer.toggle_scope(),
            Keysym::BackSpace => {
                st.filter.backspace(ctrl);
                edited = true;
            }
            Keysym::Delete | Keysym::KP_Delete => {
                st.filter.delete(ctrl);
                edited = true;
            }
            Keysym::Left | Keysym::KP_Left => st.filter.left(ctrl),
            Keysym::Right | Keysym::KP_Right => st.filter.right(ctrl),
            Keysym::Home | Keysym::KP_Home => st.filter.home(),
            Keysym::End | Keysym::KP_End => st.filter.end(),
            _ if !ctrl && !mods.alt => match &event.utf8 {
                Some(s) => {
                    st.filter.insert(s);
                    edited = true;
                }
                None => moved = false,
            },
            _ => moved = false,
        }
        if shift
            && matches!(
                sym,
                Keysym::Left
                    | Keysym::KP_Left
                    | Keysym::Right
                    | Keysym::KP_Right
                    | Keysym::Home
                    | Keysym::KP_Home
                    | Keysym::End
                    | Keysym::KP_End
            )
            && !ctrl
        {
            st.filter.extend_from(anchor);
        }
        match (edited, moved) {
            (true, _) => self.drawer_filter_changed(),
            (false, true) => self.drawer_changed(),
            (false, false) => {}
        }
        acts
    }

    /// The filter as a field to draw.
    fn drawer_filter_field(&self) -> Option<Field> {
        let st = self.drawer.as_ref()?;
        let width = drawer::field_width(self.style.content_w());
        Some(Field {
            text: st.filter.text.clone(),
            caret: self.focused.then_some(st.filter.caret),
            selection: st
                .filter
                .selection()
                .map(|r| (r, (self.style.link & !0xff) | 0x48)),
            style: text::Style {
                font: UI_FONT.into(),
                size: drawerview::title_size(self.style.font_size),
                color: self.drawer_ink(),
                background: 0,
            },
            bold: false,
            width: width.round() as i32,
            scroll: st.field_scroll,
            scale: self.scale,
        })
    }

    /// A click at `x` on the filter: the caret goes there.
    fn drawer_place_caret(&mut self, x: f64) {
        let Some((area, layout)) = self.drawer_geometry() else {
            return;
        };
        let Some(field) = self.drawer_filter_field() else {
            return;
        };
        let from = area.x + layout.filter.x + drawer::INSET;
        let at = {
            let mut fonts = self.fonts.borrow_mut();
            let Some(fonts) = fonts.as_mut() else {
                return;
            };
            fonts.field_index(&field, x - from)
        };
        if let Some(st) = &mut self.drawer {
            st.filter.set_caret(at);
        }
        self.drawer_changed();
    }

    /// Draw the drawer's body if anything about it changed (and the fonts
    /// are there).
    fn draw_drawer(&mut self) {
        let Some((area, layout)) = self.drawer_geometry() else {
            return;
        };
        if !self.drawer_dirty && self.drawer_shown.is_some() {
            return;
        }
        let Some(field) = self.drawer_filter_field() else {
            return;
        };
        let Some(st) = &self.drawer else {
            return;
        };
        let ink = self.drawer_ink();
        let armed = self.trash.armed(Instant::now());
        let hover = st.hover;
        let d = &st.drawer;
        let rows: Vec<drawerview::Row> = (0..d.window().len())
            .filter_map(|n| {
                let e = d.row(n)?;
                Some(drawerview::Row {
                    title: e.title.clone(),
                    sub: e.subtitle(),
                    color: e.color,
                    overdue: e.overdue,
                    dim: e.dim,
                    selected: d.is_selected(n),
                    hovered: matches!(
                        hover,
                        Some(DrawerHit::Row(i) | DrawerHit::Action(i)) if i == n
                    ),
                })
            })
            .collect();
        let action = match (hover, d.kind().row_action()) {
            (Some(DrawerHit::Row(n) | DrawerHit::Action(n)), Some(kind)) if n < rows.len() => {
                let lit = matches!(hover, Some(DrawerHit::Action(_)));
                match kind {
                    drawer::RowAction::Trash => {
                        let tint = match (lit, armed) {
                            (true, true) => TRASH_RED,
                            (true, false) => ink,
                            _ => (ink & !0xff) | 0xa0,
                        };
                        (self.icons.get(&self.trash_icon()))
                            .and_then(|i| i.as_ref())
                            .map(|image| (n, drawerview::Action::Icon(tinted(image, tint))))
                    }
                    drawer::RowAction::Clear => {
                        let color = if lit { ink } else { (ink & !0xff) | 0xa0 };
                        Some((n, drawerview::Action::Glyph("✓", color)))
                    }
                }
            }
            _ => None,
        };
        let empty = d.is_empty().then(|| {
            match (d.filter().trim().is_empty(), d.elsewhere()) {
                (true, elsewhere) => d.kind().empty(elsewhere),
                (false, false) => "No matches",
                (false, true) => "No matches here (Tab: all)",
            }
            .to_owned()
        });
        let view = drawerview::View {
            scale: self.scale,
            w: area.w,
            h: area.h,
            layout: &layout,
            font: UI_FONT,
            size: self.style.font_size,
            ink,
            overdue: self.style.overdue,
            filter: field,
            placeholder: "Filter",
            scope: match d.kind().scoped() {
                true => d.scope().label().to_owned(),
                false => String::new(),
            },
            scope_lit: hover == Some(DrawerHit::Scope),
            rows,
            action,
            empty,
            above: d.above(),
            more: d.below(),
        };
        let (image, scroll) = {
            let mut fonts = self.fonts.borrow_mut();
            let Some(fonts) = fonts.as_mut() else {
                return;
            };
            drawerview::compose(fonts, &mut self.drawer_lines, &view)
        };
        let buffer = match shm_buffer(&self.g, &image, &self.qh) {
            Ok(buffer) => buffer,
            Err(e) => return warn!("drawer buffer: {e}"),
        };
        let s = self.scale as f64;
        let sized = Sized {
            buffer,
            w: image.width as f64 / s,
            h: image.height as f64 / s,
        };
        self.stale
            .extend(self.drawer_shown.replace(sized).map(|s| s.buffer));
        if let Some(st) = &mut self.drawer {
            st.field_scroll = scroll;
        }
        self.drawer_dirty = false;
    }

    /// The drawer's body, on its sheet.
    fn place_drawer(&mut self, face: Option<Rect>, alpha: f64, surface: Rect) {
        self.notice.clear();
        self.show_bar(true, None);
        let Some(face) = face else {
            self.text.clear();
            return;
        };
        self.draw_drawer();
        let area = self.drawer_area(face);
        if let Some(image) = &self.drawer_shown {
            let s = self.scale as f64;
            self.text_shown = place_image(&mut self.text, image, area, surface, s, alpha);
        }
        if self.text_shown.is_none() {
            self.text.clear();
        }
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

    /// The frame callback: advance the springs and draw.
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        let now = Instant::now();
        let dt = (self.last_frame).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let moving = self.deck.advance(dt);
        for slot in &mut self.row_slots {
            slot.out.advance(dt);
        }
        let moving = moving || self.row_moving();
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
        let animating_palette = [self.palette_since, self.due_since]
            .into_iter()
            .flatten()
            .any(|t| t.elapsed() < PALETTE_TIME);
        if (self.deck.moving() || self.row_moving() || animating_palette) && !self.frame_pending {
            self.request_frame();
        }
        self.layer.commit();
    }

    // -- geometry ----------------------------------------------------------

    fn surface_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width as f64, self.height as f64)
    }

    /// How far behind the edge a card goes to be out of sight, shadow
    /// included.
    fn reach(&self) -> f64 {
        (self.style.lifted.or(self.style.shadow)).map_or(0, |s| s.pad()) as f64 + BEHIND
    }

    /// How far a card is out now, its presence included.
    fn out_now(&self, card: &Card) -> f64 {
        lerp(-self.reach(), card.out.value, card.presence.value)
    }

    /// A rect `w` wide whose side that comes out is `out` from the edge.
    fn out_rect(&self, out: f64, w: f64, y: f64, h: f64) -> Rect {
        match self.style.edge {
            Edge::Right => Rect::new(self.width as f64 - out, y, w, h),
            Edge::Left => Rect::new(out - w, y, w, h),
        }
    }

    /// Where a card's paper starts along the edge now: at the top of its
    /// length, so a paper growing or shrinking (a header into a sheet, or
    /// back, or into its marker) keeps its top, title and all, where the
    /// card's top is, rather than sliding past it from being centered, and
    /// one paper replacing another mid-way starts where it did.
    fn top(card: &Card) -> f64 {
        card.y.value - card.len.value / 2.0
    }

    /// Where the part of a card that is out takes the pointer: as far as
    /// it is out, `l` long from `top`.
    fn hit_rect(&self, card: &Card, top: f64, l: f64) -> Option<Rect> {
        let o = self.out_now(card);
        let r = match self.style.edge {
            Edge::Right => Rect::new(self.width as f64 - o, top, o, l),
            Edge::Left => Rect::new(0.0, top, o, l),
        };
        intersect(r, self.surface_rect())
    }

    /// Where a held note's sheet is, as on screen (without what reaches
    /// behind the edge).
    fn face(&self, key: &str) -> Option<Rect> {
        let card = self.deck.cards().iter().find(|c| c.key == key)?;
        let m = &self.style.metrics;
        let h = m.sheet_height;
        Some(self.out_rect(self.out_now(card), m.sheet_width, Self::top(card), h))
    }

    fn margin_rect(&self, face: Rect) -> Rect {
        let mw = self.style.metrics.header;
        match self.style.edge {
            Edge::Right => Rect::new(face.x, face.y, mw, face.h),
            Edge::Left => Rect::new(face.right() - mw, face.y, mw, face.h),
        }
    }

    fn body_rect(&self, face: Rect) -> Rect {
        let mw = self.style.metrics.header;
        match self.style.edge {
            Edge::Right => Rect::new(face.x + mw, face.y, face.w - mw, face.h),
            Edge::Left => Rect::new(face.x, face.y, face.w - mw, face.h),
        }
    }

    fn text_rect(&self, face: Rect) -> Rect {
        let body = self.body_rect(face);
        let h = (face.h - PAD_Y - self.style.file_bar).max(0.0);
        Rect::new(body.x + PAD_X, face.y + PAD_Y, self.style.content_w(), h)
    }

    fn notice_rect(&self, face: Rect) -> Rect {
        let area = self.text_rect(face);
        Rect::new(area.x, area.y, area.w, self.notice_height().min(area.h))
    }

    fn editor_rect(&self, face: Rect) -> Rect {
        let area = self.text_rect(face);
        let h = self.notice_rect(face).h;
        Rect::new(area.x, area.y + h, area.w, area.h - h)
    }

    fn notice_buttons(&self, area: Rect) -> (Rect, Rect) {
        let y = area.y + self.style.notice_line();
        let h = (area.bottom() - y).max(0.0);
        let w = (area.w / 2.0).floor();
        (Rect::new(area.x, y, w, h), Rect::new(area.x + w, y, w, h))
    }

    fn place_notice(&mut self, face: Rect, surface: Rect, alpha: f64) {
        let area = self.notice_rect(face);
        let Some(message) = self.notice_text() else {
            self.notice.clear();
            return;
        };
        let conflict = self.has_conflict();
        let label = if conflict {
            format!("{message}\nReload\nKeep mine")
        } else {
            message.to_owned()
        };
        if self.notice_shown.as_ref().is_none_or(|(s, _)| *s != label) {
            let drawn = {
                let mut fonts = self.fonts.borrow_mut();
                let Some(fonts) = fonts.as_mut() else { return };
                let mut field = Field {
                    text: message.to_owned(),
                    caret: None,
                    selection: None,
                    style: self.style.small(TRASH_RED),
                    bold: true,
                    width: area.w.floor().max(1.0) as i32,
                    scroll: 0.0,
                    scale: self.scale,
                };
                let mut image = fonts.field(&field).image;
                if conflict {
                    let s = self.scale as usize;
                    let row = self.style.notice_line() as usize * s;
                    let width = image.width as usize;
                    let height = row * 2;
                    image.height = height as u32;
                    image.data.resize(width * height * 4, 0);
                    field.width /= 2;
                    for (i, name) in ["Reload", "Keep mine"].into_iter().enumerate() {
                        field.text = name.into();
                        let button = fonts.field(&field).image;
                        let w = button.width as usize;
                        for (j, pixels) in button.data.chunks_exact(w * 4).take(row).enumerate() {
                            let start = ((row + j) * width + i * w) * 4;
                            image.data[start..start + w * 4].copy_from_slice(pixels);
                        }
                    }
                }
                image
            };
            if let Ok(buffer) = shm_buffer(&self.g, &drawn, &self.qh) {
                let sized = Sized {
                    buffer,
                    w: drawn.width as f64 / self.scale as f64,
                    h: drawn.height as f64 / self.scale as f64,
                };
                self.stale.extend(
                    self.notice_shown
                        .replace((label, sized))
                        .map(|(_, s)| s.buffer),
                );
            }
        }
        if let Some((_, image)) = &self.notice_shown
            && let Some(buffer) = place_image(
                &mut self.notice,
                image,
                area,
                surface,
                self.scale as f64,
                alpha,
            )
        {
            self.attached.push(buffer);
        } else {
            self.notice.clear();
        }
    }

    fn bar_rect(&self, face: Rect) -> Rect {
        let body = self.body_rect(face);
        let h = self.style.file_bar;
        Rect::new(body.x + PAD_X, face.bottom() - h, self.style.content_w(), h)
    }

    /// The edge bands (see `Deck::bands`).
    fn band_rects(&self) -> Vec<Rect> {
        (self.deck.bands().into_iter())
            .map(|(top, bottom, w)| self.out_rect(w, w, top, bottom - top))
            .collect()
    }

    /// Where the parts of `key`'s file bar are, if it shows one.
    fn bar_layout(&self, key: &str) -> Option<BarLayout> {
        let note = self.notes.get(key)?;
        let open = self.deck.open() == Some(key);
        if !self.deck.held(key) {
            return None;
        }
        let face = self.face(key)?;
        let bar = self.bar_rect(face);
        let mid = bar.y + bar.h / 2.0;
        let preview_palette = !open && note.pin != Pin::Locked;
        let has_details = !note.files.is_empty() || note.due.is_some();
        let slots = if open {
            7.0
        } else if preview_palette && has_details {
            3.0
        } else {
            1.0
        };
        let pitch = (ICON + ICON_GAP).min(bar.w / slots);
        let size = ICON.min(pitch * 0.85);
        let rect = |x| Rect::new(x, mid - size / 2.0, size, size);
        let slot = |i: usize| rect(bar.x + i as f64 * pitch);
        let control = |i: usize| open.then(|| rect(bar.right() - size - i as f64 * pitch));
        let color = control(if note.scratch { 2 } else { 3 });
        let palette_open = open && self.palette_since.is_some();
        // The color row slides over the clock, which is out of its way.
        let clock = control(if note.scratch { 3 } else { 4 }).filter(|_| !palette_open);
        // Leave room for attachments or the due label beside the preview palette.
        let palette_left = if preview_palette && has_details {
            bar.x + bar.w / 3.0
        } else {
            bar.x
        };
        let file_end = clock.or(color).map_or_else(
            || {
                if preview_palette {
                    palette_left
                } else {
                    bar.right()
                }
            },
            |r| r.x - (pitch - size),
        );
        let (n, more) = bar::fit(note.files.len(), file_end - bar.x, size, pitch - size);
        let grid = self.showing_files(key);
        let swatches = match (color, self.palette_since) {
            (Some(dot), Some(start)) => {
                let p = (start.elapsed().as_secs_f64() / PALETTE_TIME.as_secs_f64()).min(1.0);
                let colors: Vec<_> = bar::colors(note.color).collect();
                let rects = bar::palette(bar.x, dot, colors.len(), 1.0 - (1.0 - p).powi(3));
                colors.into_iter().zip(rects).collect()
            }
            _ if preview_palette => {
                let anchor = rect(bar.right());
                let colors: Vec<_> = bar::colors(note.color).collect();
                let rects = bar::palette(palette_left, anchor, colors.len(), 1.0);
                colors.into_iter().zip(rects).collect()
            }
            _ => Vec::new(),
        };
        let due_row = match (clock, self.due_since) {
            (Some(clock), Some(start)) if !grid => {
                let p = (start.elapsed().as_secs_f64() / PALETTE_TIME.as_secs_f64()).min(1.0);
                let choices = self.due_choices(note.due.is_some());
                let widths: Vec<f64> = (choices.iter())
                    .map(|c| bar::cell_width(due_label(*c)))
                    .collect();
                let cells = bar::row(
                    bar.x,
                    bar.right(),
                    clock,
                    &widths,
                    4.0,
                    1.0 - (1.0 - p).powi(3),
                );
                choices.into_iter().zip(cells).collect()
            }
            _ => Vec::new(),
        };
        let picking = !due_row.is_empty();
        let used = n + usize::from(more.is_some());
        let mut layout = BarLayout {
            icons: if grid || palette_open || !due_row.is_empty() {
                Vec::new()
            } else {
                (0..n).map(|i| (i, slot(i))).collect()
            },
            more: if palette_open || !due_row.is_empty() {
                None
            } else if grid {
                Some((0, slot(more.map_or(0, |_| n))))
            } else {
                more.map(|count| (count, slot(n)))
            },
            // The due row takes the whole bar: the controls step aside.
            pin: control(0).filter(|_| !picking),
            archive: if note.scratch { None } else { control(1) }.filter(|_| !picking),
            trash: control(if note.scratch { 1 } else { 2 }).filter(|_| !picking),
            color: color.filter(|_| !picking),
            swatches,
            clock: clock.filter(|_| !picking),
            due_row,
            hit_pad: (pitch - size) / 2.0,
            name_x: bar.x + used.max(usize::from(grid)) as f64 * pitch,
            name_end: file_end,
            mid,
        };
        // On a tilted sheet, the icons follow the tilt.
        let slot = self.slots.iter().find(|s| s.key == key)?;
        let tilt = slot.shown.as_ref().map_or(0.0, |(p, _)| p.tilt);
        if tilt != 0.0 {
            let (cx, cy) = slot.paper.center();
            let turn = |r: Rect| {
                let (x, y) = rotate(r.center(), (cx, cy), tilt);
                Rect::new(x - r.w / 2.0, y - r.h / 2.0, r.w, r.h)
            };
            for (_, r) in &mut layout.icons {
                *r = turn(*r);
            }
            if let Some((_, r)) = &mut layout.more {
                *r = turn(*r);
            }
            for (_, r) in &mut layout.swatches {
                *r = turn(*r);
            }
            let (x, mid) = rotate((layout.name_x, layout.mid), (cx, cy), tilt);
            layout.name_end += x - layout.name_x;
            (layout.name_x, layout.mid) = (x, mid);
        }
        Some(layout)
    }

    // -- papers --------------------------------------------------------------

    /// A note's margin: its title turned, and the crease. A sheet's
    /// title may take the whole margin, so as the sheet comes down from
    /// its header the title comes down with it.
    fn margin(&self, key: &str, role: Role) -> Option<paper::Margin> {
        if key == drawer::KEY {
            return Some(self.drawer_margin(role));
        }
        let note = self.notes.get(key)?;
        let m = &self.style.metrics;
        let room = match role {
            Role::Sheet => m.sheet_height,
            _ => m.header_length * 0.7,
        };
        // On an accent color the text is whatever reads on that.
        let text = note.accent.map_or(note.text, Config::ink_on);
        let ink = (text & !0xff) | 0xc8;
        Some(paper::Margin {
            width: m.header as i32,
            ink: text,
            crease: true,
            label: Some(paper::Label {
                text: note.content.title.to_uppercase(),
                style: self.style.small(ink),
                spacing: LABEL_SPACING,
                top: LABEL_PAD as i32,
                len: (room - 2.0 * LABEL_PAD).max(8.0) as i32,
            }),
        })
    }

    /// The paper a card is drawn with in `role`.
    fn paper(&self, card: &Card, role: Role) -> Paper {
        let m = &self.style.metrics;
        let base = Paper {
            w: (m.marker_width + BEHIND).round() as i32,
            h: self.deck.marker_length().round() as i32,
            color: card.look.color,
            edge: self.style.edge.into(),
            tilt: card.look.tilt,
            sliver: true,
            accent: card.look.accent,
            radius: self.style.marker_radius,
            curl: false,
            margin: None,
            bar: 0,
            shadow: None,
            scale: self.scale,
            content: None,
        };
        match role {
            Role::Marker => base,
            Role::Header | Role::Lifted => Paper {
                w: (m.header + LIFT + BEHIND).round() as i32,
                h: m.header_length.round() as i32,
                sliver: false,
                radius: TAB_RADIUS,
                margin: self.margin(&card.key, role),
                shadow: match role {
                    Role::Lifted => self.style.lifted,
                    _ => self.style.shadow,
                },
                ..base
            },
            Role::Sheet => {
                let note = self.notes.get(&card.key);
                let open = self.deck.open() == Some(card.key.as_str());
                let footer = note.is_some_and(|n| {
                    n.pin != Pin::Locked || !n.files.is_empty() || n.due.is_some()
                });
                let drawer = card.key == drawer::KEY;
                let bar = if drawer {
                    0.0
                } else if open || footer {
                    self.style.file_bar
                } else {
                    0.0
                };
                let content = note
                    .filter(|_| !open && !self.showing_files(&card.key))
                    .map(|n| {
                        let clip = m.sheet_height - PAD_Y - bar.max(PAD_Y);
                        let line = text::line_height(self.style.font_size) as f64;
                        let x = match self.style.edge {
                            Edge::Right => m.header + PAD_X,
                            Edge::Left => BEHIND + PAD_X,
                        };
                        paper::Content {
                            doc: Doc {
                                blocks: n.content.blocks.clone(),
                                style: self.style.doc_style(n.text),
                                width: self.style.content_w() as i32,
                                max_height: (clip + 2.0 * line) as i32,
                                scale: self.scale,
                            },
                            x: x as i32,
                            y: PAD_Y as i32,
                            clip: Some(clip as i32),
                        }
                    });
                Paper {
                    w: (m.sheet_width + BEHIND).round() as i32,
                    h: m.sheet_height.round() as i32,
                    tilt: if open { 0.0 } else { card.look.tilt },
                    sliver: false,
                    radius: SHEET_RADIUS,
                    margin: self.margin(&card.key, Role::Sheet),
                    bar: bar as i32,
                    shadow: self.style.lifted,
                    content,
                    ..base
                }
            }
        }
    }

    // -- the scene ---------------------------------------------------------

    fn place_all(&mut self) {
        self.attached.clear();
        let surface = self.surface_rect();
        let scale = self.scale as f64;
        let cards: Vec<Card> = self.deck.cards().to_vec();
        self.slots.retain(|s| cards.iter().any(|c| c.key == s.key));
        for card in &cards {
            if !self.slots.iter().any(|s| s.key == card.key) {
                let (g, parent) = (&self.g, self.layer.wl_surface());
                let under = Sub::new(g, parent, &self.no_input, &self.qh);
                let sub = Sub::new(g, parent, &self.no_input, &self.qh);
                self.slots.push(Slot {
                    key: card.key.clone(),
                    sub,
                    under,
                    shown: None,
                    paper: Rect::default(),
                    hit: None,
                });
            }
            let role = self.deck.role(&card.key).max(self.deck.role_now(card));
            let want = self.paper(card, role);
            let ready = match card.crumpling() {
                true => None,
                false => self.papers.borrow_mut().get(&want),
            };
            // Have the next level ready: headers (for the labels to show
            // at once), and the sheet of the note likely to be revealed.
            if role < Role::Header {
                let header = self.paper(card, Role::Header);
                self.papers.borrow_mut().get(&header);
            }
            if role < Role::Sheet && self.deck.hover() == Some(card.key.as_str()) {
                let sheet = self.paper(card, Role::Sheet);
                self.papers.borrow_mut().get(&sheet);
            }
            let reach = self.reach();
            let width = self.width as f64;
            let edge = self.style.edge;
            let Some(i) = self.slots.iter().position(|s| s.key == card.key) else {
                continue;
            };
            // A crumpling note keeps the paper it had.
            let crumpling = card.crumpling() && self.slots[i].shown.is_some();
            if let Some(sheet) = ready {
                self.slots[i].shown = Some((want, sheet));
            }
            let headers = [Role::Header, Role::Lifted].map(|r| self.paper(card, r));
            let marker = self.paper(card, Role::Marker);
            let Some((p, sheet)) = &self.slots[i].shown else {
                self.slots[i].sub.clear();
                self.slots[i].under.clear();
                self.slots[i].hit = None;
                continue;
            };
            let (pw, ph) = (p.w as f64, p.h as f64);
            let r = sheet.shape.rect;
            // A header folding into its marker fades into it (drawn under
            // it) over the last of the way, and one coming out of it fades
            // in over it, so the paper doesn't change at once as the card
            // passes a marker's size.
            let (mw, hw) = (self.style.metrics.marker_width, self.style.metrics.header);
            let from = mw + 0.5;
            let header = headers.contains(p) && !crumpling;
            let fade = match header && hw > from {
                true => ((card.out.value - from) / ((hw - from) * HEADER_FADE)).clamp(0.0, 1.0),
                false => 1.0,
            };
            // Asked for while it is a header, so it is ready to fade into.
            let under = match header {
                true => self.papers.borrow_mut().get(&marker).filter(|_| fade < 1.0),
                false => None,
            };
            let fade = if under.is_some() { fade } else { 1.0 };
            let under = under.and_then(|s| {
                let mo = lerp(-reach, mw, card.presence.value);
                let (mpw, mph) = (marker.w as f64, marker.h as f64);
                let top = Self::top(card);
                let at = match edge {
                    Edge::Right => Rect::new(width - mo, top, mpw, mph),
                    Edge::Left => Rect::new(mo - mpw, top, mpw, mph),
                };
                let r = s.shape.rect;
                let image = Rect::new(at.x + r.x as f64, at.y + r.y as f64, r.w as f64, r.h as f64);
                let size = (r.w as f64 * scale, r.h as f64 * scale);
                Some((s, clip(image, size, None, surface)?))
            });
            // Crumpling, it stays out and shrinks toward the edge instead.
            let o = match crumpling {
                true => card.out.value,
                false => lerp(-reach, card.out.value, card.presence.value),
            };
            let top = Self::top(card);
            let paper = match edge {
                Edge::Right => Rect::new(width - o, top, pw, ph),
                Edge::Left => Rect::new(o - pw, top, pw, ph),
            };
            // Cut to its length while it grows or shrinks between markers
            // and headers. The cut keeps the shadow's reach past the
            // paper's ends, so the shadow grows along with the paper
            // instead of showing once it settles. A sheet isn't cut while
            // it comes out or goes back in: it slides whole from and to its
            // header's top rather than unrolling or rolling up.
            let len = card.len.value;
            let whole = self.deck.role(&card.key) == Role::Sheet
                || self.deck.role_now(card) == Role::Sheet;
            let cut = match len < ph - 0.5 && !crumpling && !whole {
                true => {
                    let above = (-r.y as f64).max(0.0);
                    let below = (r.y as f64 + r.h as f64 - ph).max(0.0);
                    let top = card.y.value - len / 2.0 - above;
                    intersect(surface, Rect::new(0.0, top, width, len + above + below))
                }
                false => Some(surface),
            };
            let hit_len = if whole { ph } else { len.min(ph) };
            let hit = self.hit_rect(card, top, hit_len).filter(|_| card.present());
            let slot = &mut self.slots[i];
            slot.paper = paper;
            slot.hit = hit;
            let Some((_, sheet)) = &slot.shown else {
                continue;
            };
            let r = sheet.shape.rect;
            let mut image = Rect::new(
                paper.x + r.x as f64,
                paper.y + r.y as f64,
                r.w as f64,
                r.h as f64,
            );
            let p = card.presence.value.clamp(0.0, 1.0);
            let alpha = match crumpling {
                true => {
                    let edge_x = match edge {
                        Edge::Right => width,
                        Edge::Left => 0.0,
                    };
                    image = image.scaled(lerp(CRUMPLED, 1.0, p), edge_x, card.y.value);
                    p * p
                }
                false => fade,
            };
            let size = (r.w as f64 * scale, r.h as f64 * scale);
            let placed = cut.and_then(|cut| clip(image, size, None, cut));
            match placed {
                Some((rect, src)) if card.presence.value > 0.0 && alpha > 0.0 => {
                    slot.sub.show(&sheet.shape.buffer);
                    slot.sub.crop(src);
                    slot.sub.place(IRect::round(rect));
                    slot.sub.set_alpha(alpha);
                    slot.sub.commit();
                }
                _ => slot.sub.clear(),
            }
            match under {
                Some((s, (rect, src))) if card.presence.value > 0.0 => {
                    slot.under.show(&s.shape.buffer);
                    slot.under.crop(src);
                    slot.under.place(IRect::round(rect));
                    slot.under.commit();
                }
                _ => slot.under.clear(),
            }
        }

        self.place_plus();
        self.place_row();
        self.place_backdrops();
        if self.press.is_none()
            && let Some((x, y)) = self.pointer
        {
            // Geometry can move under a stationary pointer, including
            // away from an armed trash button.
            let on = self.on_at(x, y);
            if self.leave_file_views(&on) {
                self.kick();
            }
            self.update_bar_hover(x, y);
            if self.drawer.is_some() {
                self.update_drawer_hover(x, y);
            }
        }
        self.place_open();
        let revealed = self.deck.revealed().map(String::from);
        let plan = revealed.as_deref().and_then(|k| self.bar_plan(k, false));
        self.show_bar(false, plan);
        self.restack();

        // Input: what shows of the notes, the button and the bands.
        let mut input: Vec<IRect> = (self.slots.iter())
            .filter_map(|s| s.hit)
            .chain(self.plus_rect)
            .chain(self.row_hold)
            .chain(
                self.band_rects()
                    .into_iter()
                    .filter_map(|r| intersect(r, surface)),
            )
            .map(IRect::round)
            .collect();
        input.dedup();
        if self.input.as_ref() != Some(&input) {
            self.set_input(&input);
            self.input = Some(input);
        }
    }

    /// The groups' backdrops: a rounded rect, cut in three so the middle
    /// stretches, `BACKDROP_PAD` larger than the markers along the edge
    /// and out of it, and reaching behind it like them.
    fn place_backdrops(&mut self) {
        let m = self.style.metrics;
        let color = self.style.backdrop;
        let r = (self.style.marker_radius + BACKDROP_PAD).round().max(1.0) as i32;
        let out = m.marker_width + BACKDROP_PAD;
        let w = (out + BEHIND).round() as i32;
        let key = (self.scale, color, w, r);
        let visible = color & 0xff != 0;
        if visible && self.backdrop_shown.as_ref().is_none_or(|(k, _)| *k != key) {
            let image = paper::rounded(w, 2 * r + 1, r as f64, color, self.scale);
            match shm_buffer(&self.g, &image, &self.qh) {
                Ok(buffer) => {
                    let old = self.backdrop_shown.replace((key, buffer));
                    self.stale.extend(old.map(|(_, b)| b));
                }
                Err(e) => warn!("backdrop buffer: {e}"),
            }
        }
        let surface = self.surface_rect();
        let s = self.scale as f64;
        let (wf, rf) = (w as f64, r as f64);
        let x = match self.style.edge {
            Edge::Right => self.width as f64 - out,
            Edge::Left => out - wf,
        };
        let backdrops = *self.deck.backdrops();
        for (subs, b) in self.backdrops.iter_mut().zip(backdrops) {
            let alpha = b.alpha.value.clamp(0.0, 1.0);
            let shown = self
                .backdrop_shown
                .as_ref()
                .filter(|_| visible && alpha > 0.0);
            let Some((_, buffer)) = shown else {
                for sub in subs.iter_mut() {
                    sub.clear();
                }
                continue;
            };
            let (top, bottom) = (b.top.value - BACKDROP_PAD, b.bottom.value + BACKDROP_PAD);
            let len = (bottom - top).max(0.0);
            let cap = rf.min(len / 2.0);
            let (bw, rp) = (wf * s, rf * s);
            let parts = [
                (Rect::new(x, top, wf, cap), [0.0, 0.0, bw, rp]),
                (
                    Rect::new(x, top + cap, wf, len - 2.0 * cap),
                    [0.0, rp, bw, s],
                ),
                (Rect::new(x, bottom - cap, wf, cap), [0.0, rp + s, bw, rp]),
            ];
            for (sub, (rect, src)) in subs.iter_mut().zip(parts) {
                match clip(rect, (bw, rp * 2.0 + s), Some(src), surface) {
                    Some((rect, src)) => {
                        sub.show(buffer);
                        sub.crop(src);
                        sub.place(IRect::round(rect));
                        sub.set_alpha(alpha);
                        sub.commit();
                    }
                    None => sub.clear(),
                }
            }
            self.attached.push(buffer.clone());
        }
    }
    /// The `+` button: out from header level on, in the screen's bottom
    /// corner, centered on the headers' width.
    fn place_plus(&mut self) {
        let m = self.style.metrics;
        if self
            .plus_shown
            .as_ref()
            .is_none_or(|(s, _, _)| *s != self.scale)
        {
            let r = paper::button(
                m.plus as i32,
                self.style.plus_color,
                self.style.plus_glyph,
                self.style.shadow,
                self.scale,
            );
            match shm_buffer(&self.g, &r.image, &self.qh) {
                Ok(buffer) => {
                    let old = self.plus_shown.replace((self.scale, buffer, r));
                    self.stale.extend(old.map(|(_, b, _)| b));
                }
                Err(e) => warn!("button buffer: {e}"),
            }
        }
        let p = self.deck.plus.value;
        let dist = lerp(-m.plus, m.header / 2.0, p);
        let cx = match self.style.edge {
            Edge::Right => self.width as f64 - dist,
            Edge::Left => dist,
        };
        let cy = self.deck.plus_center();
        let rect = Rect::new(cx - m.plus / 2.0, cy - m.plus / 2.0, m.plus, m.plus);
        self.plus_rect = (p > 0.5).then_some(rect);
        let surface = self.surface_rect();
        let Some((_, buffer, r)) = &self.plus_shown else {
            return;
        };
        let sr = r.rect;
        let image = Rect::new(
            rect.x + sr.x as f64,
            rect.y + sr.y as f64,
            sr.w as f64,
            sr.h as f64,
        );
        let s = self.scale as f64;
        match clip(image, (sr.w as f64 * s, sr.h as f64 * s), None, surface) {
            Some((rect, src)) if p > 0.0 => {
                self.plus.show(buffer);
                self.plus.crop(src);
                self.plus.place(IRect::round(rect));
                self.plus.set_alpha(p.clamp(0.0, 1.0));
                self.plus.commit();
            }
            _ => self.plus.clear(),
        }
    }

    pub fn set_row_available(&mut self, available: Available) {
        if self.row_available != available {
            self.row_available = available;
            self.kick();
        }
    }

    fn row_moving(&self) -> bool {
        self.row_slots.iter().any(|slot| !slot.out.settled())
    }

    fn row_at(&self, x: f64, y: f64) -> Option<Button> {
        if self.plus_rect.is_some_and(|plus| plus.contains(x, y)) {
            return None;
        }
        self.row_hits
            .iter()
            .rev()
            .find_map(|&(button, rect)| rect.contains(x, y).then_some(button))
    }

    fn set_row_mode(&mut self, mode: Mode) {
        if self.row_mode != mode {
            self.row_mode = mode;
            if mode != Mode::Closed {
                self.row_last = mode;
            }
            self.kick();
        }
    }

    pub fn update_row_hover(&mut self, core: &Core<WlNote>) {
        let over_plus = self
            .pointer
            .is_some_and(|(x, y)| self.on_at(x, y) == On::Plus);
        let in_row = self
            .pointer
            .is_some_and(|(x, y)| self.row_hold.is_some_and(|r| r.contains(x, y)));
        let timer = if self.retiring {
            None
        } else if (over_plus || in_row) && self.row_drag {
            self.set_row_mode(Mode::Drag);
            None
        } else if self.row_mode == Mode::Drag && !self.row_drag && (over_plus || in_row) {
            self.set_row_mode(Mode::Rest);
            None
        } else if self.row_mode != Mode::Closed {
            (!(over_plus || in_row)).then_some((Timer::RowHide, self.style.hide_delay))
        } else {
            (over_plus && !self.row_drag && self.press.is_none())
                .then_some((Timer::RowRest, self.style.reveal_delay))
        };
        if self.row_timer.map(|(kind, _)| kind) != timer.map(|(kind, _)| kind) {
            self.cancel_row_timer(core);
            if let Some((kind, delay)) = timer {
                let surface = self.surface().clone();
                if delay.is_zero() {
                    self.timer_due(kind);
                } else {
                    match core.timer(delay, move |app: &mut WlNote, core| {
                        app.column_timer(core, &surface, kind);
                    }) {
                        Ok(token) => self.row_timer = Some((kind, token)),
                        Err(e) => warn!("row timer: {e}"),
                    }
                }
            }
        }
        let label = if self.row_drag || self.retiring {
            None
        } else {
            self.pointer.and_then(|(x, y)| self.row_at(x, y))
        };
        if label != self.row_label_for {
            self.row_label_for = label;
            self.kick();
        }
    }

    pub fn cancel_row_timer(&mut self, core: &Core<WlNote>) {
        if let Some((_, token)) = self.row_timer.take() {
            core.loop_handle.remove(token);
        }
    }

    fn place_row(&mut self) {
        self.row_hits.clear();
        self.row_hold = None;
        let m = self.style.metrics;
        let width = self.width as f64;
        let edge = self.style.edge;
        let cx = if edge == Edge::Left {
            m.header / 2.0
        } else {
            width - m.header / 2.0
        };
        let plus = Rect::new(
            cx - m.plus / 2.0,
            self.deck.plus_center() - m.plus / 2.0,
            m.plus,
            m.plus,
        );
        let available = self.row_available;
        let buttons = row::buttons(&self.style.row_buttons, available, self.row_mode);
        let previous = row::buttons(&self.style.row_buttons, available, self.row_last);
        let layout = |buttons: &[Button]| {
            row::places(buttons, plus, edge, width, m.sheet_width).collect::<Vec<_>>()
        };
        let targets = layout(&buttons);
        let previous = layout(&previous);
        let surface = self.surface_rect();
        let scale = self.scale as f64;
        let alpha = self.deck.plus.value.clamp(0.0, 1.0);
        let mut label = None;
        if alpha > 0.0 && !targets.is_empty() && self.row_disc.is_none() && !self.retiring {
            let size = m.plus as i32;
            let image = paper::rounded(size, size, m.plus / 2.0, self.style.plus_color, self.scale);
            if let Ok(buffer) =
                shm_buffer(&self.g, &image, &self.qh).inspect_err(|e| warn!("row button: {e}"))
            {
                self.row_disc = Some(Sized {
                    buffer,
                    w: m.plus,
                    h: m.plus,
                });
            }
        }
        for (i, button) in Button::ALL.into_iter().enumerate() {
            let target = targets.iter().find(|(b, _)| *b == button).map(|(_, r)| *r);
            let active = target.is_some() && !self.retiring;
            let immediate = active && (button == Button::Undo || self.row_mode == Mode::Drag);
            let out = &mut self.row_slots[i].out;
            let goal = if active { 1.0 } else { 0.0 };
            if immediate || alpha == 0.0 || self.retiring {
                out.snap(goal);
            } else {
                out.retarget(goal);
            }
            let p = out.value.clamp(0.0, 1.0);
            let rect =
                target.or_else(|| previous.iter().find(|(b, _)| *b == button).map(|(_, r)| *r));
            let shown = rect.filter(|_| alpha * p > 0.0 && !self.retiring);
            let mut disc = None;
            let mut glyph = None;
            let mut badge = None;
            if let Some(rect) = shown {
                let start = self.plus_rect.map_or(plus.x, |r| r.x);
                let area = Rect::new(lerp(start, rect.x, p), rect.y, rect.w, rect.h);
                if let Some(image) = &self.row_disc {
                    disc = place_image(
                        &mut self.row_slots[i].disc,
                        image,
                        area,
                        surface,
                        scale,
                        alpha * p,
                    );
                }
                let count = row::count(button, available).and_then(|text| {
                    self.line(LineKey {
                        text,
                        color: self.style.plus_glyph,
                        width: m.plus as i32,
                        strike: false,
                        tooltip: false,
                    })
                });
                let size = if count.is_some() {
                    ROW_COUNT_ICON
                } else {
                    ROW_ICON
                } as f64;
                let mut key = self.named(&[button.icon()]);
                key.size = (size * scale) as u32;
                let count_width = count.as_ref().map_or(0.0, |image| image.w);
                if let Some(buffer) = self.icon_buffer(&key, Some(self.style.plus_glyph)) {
                    let image = Sized {
                        buffer,
                        w: size,
                        h: size,
                    };
                    let x = area.x + (area.w - size - count_width) / 2.0;
                    let y = area.y + (area.h - size) / 2.0;
                    glyph = place_image(
                        &mut self.row_slots[i].glyph,
                        &image,
                        Rect::new(x, y, image.w, image.h),
                        surface,
                        scale,
                        alpha * p,
                    );
                    if let Some(count) = count {
                        let rect = Rect::new(
                            x + size,
                            area.y + (area.h - count.h) / 2.0,
                            count.w,
                            count.h,
                        );
                        badge = place_image(
                            &mut self.row_slots[i].count,
                            &count,
                            rect,
                            surface,
                            scale,
                            alpha * p,
                        );
                    }
                }
                if active && alpha > 0.5 && p > 0.5 && disc.is_some() && glyph.is_some() {
                    self.row_hits.push((button, area));
                    if self.row_label_for == Some(button) {
                        label = Some((button, area));
                    }
                }
            }
            if disc.is_none() {
                self.row_slots[i].disc.clear();
            }
            if glyph.is_none() {
                self.row_slots[i].glyph.clear();
            }
            if badge.is_none() {
                self.row_slots[i].count.clear();
            }
            self.attached
                .extend(disc.into_iter().chain(glyph).chain(badge));
        }
        if let (Some(plus), Some((_, end))) = (self.plus_rect, targets.last()) {
            let x = plus.x.min(end.x);
            let right = plus.right().max(end.right());
            self.row_hold = intersect(Rect::new(x, plus.y, right - x, plus.h), surface);
        }
        let label = if !self.row_drag
            && !self.retiring
            && self.press.is_none()
            && self
                .pointer
                .is_some_and(|(x, y)| self.on_at(x, y) == On::Plus)
        {
            self.plus_rect.map(|rect| ("New note".into(), rect))
        } else {
            label.map(|(button, rect)| (row::label(button, available), rect))
        };
        let placed = label.and_then(|(text, rect)| {
            let (image, area) = self.tooltip(text, false, rect)?;
            place_image(&mut self.row_label, &image, area, surface, scale, alpha)
        });
        if placed.is_none() {
            self.row_label.clear();
        }
        self.attached.extend(placed);
    }

    fn tooltip(&mut self, text: String, strike: bool, anchor: Rect) -> Option<(Sized, Rect)> {
        let surface = self.surface_rect();
        let image = self.line(LineKey {
            text,
            color: 0xffffffff,
            width: surface.w.min(320.0) as i32,
            strike,
            tooltip: true,
        })?;
        let x = (anchor.x + (anchor.w - image.w) / 2.0)
            .clamp(surface.x, (surface.right() - image.w).max(surface.x));
        let y = (anchor.y - image.h - 4.0).max(surface.y);
        let area = Rect::new(x, y, image.w, image.h);
        Some((image, area))
    }

    /// How much the open note's parts on top of its paper show: once its
    /// straight sheet is shown, as it comes out.
    fn open_alpha(&self, key: &str) -> f64 {
        let Some(card) = self.deck.cards().iter().find(|c| c.key == key) else {
            return 0.0;
        };
        let straight = (self.slots.iter().find(|s| s.key == key))
            .and_then(|s| s.shown.as_ref())
            .is_some_and(|(p, _)| *p == self.paper(card, Role::Sheet));
        if !straight {
            return 0.0;
        }
        let m = &self.style.metrics;
        let t = ((card.out.value - m.header) / (m.sheet_width - m.header)).clamp(0.0, 1.0);
        t * t * card.presence.value.clamp(0.0, 1.0)
    }

    /// The open note's text, notice and bar.
    fn place_open(&mut self) {
        let surface = self.surface_rect();
        let s = self.scale as f64;
        let key = self.deck.open().map(String::from);
        let alpha = key.as_deref().map_or(0.0, |k| self.open_alpha(k));
        let face = key
            .as_deref()
            .and_then(|k| self.face(k))
            .filter(|_| alpha > 0.0);
        self.text_shown = None;
        if key.as_deref() == Some(drawer::KEY) {
            return self.place_drawer(face, alpha, surface);
        }
        if let Some(f) = face {
            self.place_notice(f, surface, alpha);
            if key.as_deref().is_some_and(|k| !self.showing_files(k)) {
                self.draw_body();
                if let Some(b) = &self.body_shown {
                    let area = self.editor_rect(f);
                    self.text_shown =
                        place_image(&mut self.text, &b.image, area, surface, s, alpha);
                }
            }
        } else {
            self.notice.clear();
        }
        if self.text_shown.is_none() {
            self.text.clear();
        }
        // The bar.
        let plan = (key.as_deref().filter(|_| face.is_some())).and_then(|k| self.bar_plan(k, true));
        let plan = plan.map(|p| BarPlan { alpha, ..p });
        self.show_bar(true, plan);
    }

    /// What `key`'s bar shows: buffers and where (`open`: its open
    /// sheet's, else its revealed sheet's).
    fn bar_plan(&mut self, key: &str, open: bool) -> Option<BarPlan> {
        let layout = self.bar_layout(key)?;
        let alpha = match open {
            true => 1.0,
            false => {
                let card = self.deck.cards().iter().find(|c| c.key == key)?;
                let shown = (self.slots.iter().find(|s| s.key == key))
                    .and_then(|s| s.shown.as_ref())
                    .is_some_and(|(p, _)| *p == self.paper(card, Role::Sheet));
                let m = &self.style.metrics;
                let t = ((card.out.value - m.header) / (m.sheet_width - m.header)).clamp(0.0, 1.0);
                if shown { t * t } else { 0.0 }
            }
        };
        let note = self.notes.get(key)?;
        let (ink, files, pin) = (note.text, note.files.clone(), note.pin.clone());
        let (current, paper) = (note.color, note.paper);
        let due = note.due.clone();
        let grid = self.file_grid(key);
        let armed = open && self.trash.armed(Instant::now());
        let hover = (self.bar_hover.as_ref())
            .filter(|(k, _)| k == key)
            .map(|(_, h)| *h);
        let mut plan = BarPlan {
            alpha,
            ..BarPlan::default()
        };
        if alpha <= 0.0 {
            return Some(plan);
        }
        let entries = grid.as_ref().map_or_else(
            || {
                layout
                    .icons
                    .iter()
                    .map(|(i, r)| (*i, *r, None))
                    .collect::<Vec<_>>()
            },
            |g| {
                g.cells
                    .iter()
                    .map(|(i, cell)| (*i, grid_icon(*cell), Some(*cell)))
                    .collect()
            },
        );
        for (i, rect, cell) in entries {
            let f = &files[i];
            let selected = self
                .selected_files
                .as_ref()
                .is_some_and(|(k, set)| k == key && set.contains(&f.path));
            let lit = matches!(hover, Some(BarHit::File(j) | BarHit::RemoveFile(j)) if i == j);
            let exists = if lit { f.path.exists() } else { f.exists };
            if selected {
                plan.icons.push((
                    self.dot(self.style.link, ink, false),
                    rect.outset(3.0),
                    0.55,
                ));
            }
            let buffer = self.file_buffer(&f.path, (!(lit || selected)).then_some(ink));
            plan.icons
                .push((buffer, rect, if exists { 1.0 } else { 0.4 }));
            if !exists && lit {
                let icon = self.named(&["builtin:close"]);
                plan.icons
                    .push((self.icon_buffer(&icon, None), remove_rect(rect), 1.0));
            }
            if let Some(cell) = cell
                && let Some(l) = self.line(LineKey {
                    text: bar::name(&f.path),
                    color: ink,
                    width: (cell.w - 4.0) as i32,
                    strike: !exists,
                    tooltip: false,
                })
            {
                let r = Rect::new(cell.x + (cell.w - l.w) / 2.0, rect.bottom() + 2.0, l.w, l.h);
                if r.bottom() <= cell.bottom() {
                    plan.labels.push((l.buffer, r, 1.0));
                }
            }
        }
        if let Some((count, rect)) = layout.more {
            if count == 0 {
                let icon = self.named(&["builtin:close"]);
                plan.icons.push((self.icon_buffer(&icon, None), rect, 1.0));
            } else if let Some(l) = self.line(LineKey {
                text: format!("+{count}"),
                color: ink,
                width: rect.w.ceil() as i32 + 6,
                strike: false,
                tooltip: false,
            }) {
                let r = Rect::new(rect.x, rect.y + (rect.h - l.h) / 2.0, l.w, l.h);
                plan.more = Some((l.buffer, r));
            }
        }
        for &(color, rect) in &layout.swatches {
            let fill = match color {
                Color::Preset(name) => PRESETS
                    .iter()
                    .position(|p| *p == name)
                    .map_or(paper, |i| self.style.palette[i]),
                Color::Rgb(_) => paper,
            };
            plan.icons
                .push((self.dot(fill, ink, current == color), rect, 1.0));
        }
        if let Some(rect) = layout.color {
            let a = if pin == Pin::Locked { 0.4 } else { 1.0 };
            plan.icons.push((self.dot(paper, ink, false), rect, a));
        }
        if let Some(rect) = layout.clock {
            let (tint, a) = match (&due, pin == Pin::Locked) {
                (_, true) => (ink, 0.4),
                (Some(d), _) if d.overdue => (self.style.overdue, 1.0),
                (Some(_), _) => (ink, 1.0),
                (None, _) if hover == Some(BarHit::Clock) => (ink, 1.0),
                (None, _) => (ink, 0.75),
            };
            let icon = self.clock_icon();
            plan.icons
                .push((self.icon_buffer(&icon, Some(tint)), rect, a));
        }
        for &(choice, rect) in &layout.due_row {
            let lit = hover == Some(BarHit::Due(choice));
            let key = LineKey {
                text: due_label(choice).into(),
                color: ink,
                width: rect.w.ceil() as i32 + 8,
                strike: false,
                tooltip: false,
            };
            if let Some(l) = self.line(key) {
                let r = Rect::new(
                    rect.x + (rect.w - l.w) / 2.0,
                    rect.y + (rect.h - l.h) / 2.0,
                    l.w,
                    l.h,
                );
                plan.labels.push((l.buffer, r, if lit { 1.0 } else { 0.7 }));
            }
        }
        if let Some(rect) = layout.trash {
            let tint = if armed { TRASH_RED } else { ink };
            let a = if hover == Some(BarHit::Trash) {
                1.0
            } else {
                0.75
            };
            plan.icons
                .push((self.icon_buffer(&self.trash_icon(), Some(tint)), rect, a));
        }
        if let Some(rect) = layout.pin {
            let lit = hover == Some(BarHit::Pin);
            let (icon, tint, a) = match &pin {
                Pin::Pinned { .. } => (self.pin_icon(lit), None, 1.0),
                Pin::Unpinned(_) if lit => (self.pin_icon(false), None, 1.0),
                Pin::Unpinned(_) => (self.pin_icon(false), Some(ink), 0.75),
                Pin::Keep => (self.pin_icon(false), (!lit).then_some(ink), 1.0),
                Pin::None | Pin::Locked => (self.pin_icon(false), Some(ink), 0.3),
            };
            plan.pin = Some((self.icon_buffer(&icon, tint), rect, a));
        }
        if let Some(rect) = layout.archive {
            let lit = hover == Some(BarHit::Archive);
            let (tint, a) = if lit { (None, 1.0) } else { (Some(ink), 0.75) };
            let icon = self.archive_icon();
            plan.archive = Some((self.icon_buffer(&icon, tint), rect, a));
        }
        // The name of what is hovered.
        let name = match hover {
            Some(BarHit::File(i)) if grid.is_none() => {
                files.get(i).map(|f| (bar::name(&f.path), !f.path.exists()))
            }
            Some(BarHit::RemoveFile(_)) => Some(("Remove missing link".into(), false)),
            Some(BarHit::More) => Some((
                if grid.is_some() {
                    "Back to note"
                } else {
                    "Show all files"
                }
                .into(),
                false,
            )),
            Some(BarHit::Color) => Some((
                if pin == Pin::Locked {
                    "The frontmatter doesn't parse"
                } else {
                    "Change color"
                }
                .into(),
                false,
            )),
            Some(BarHit::Swatch(color)) => Some((bar::color_name(color), false)),
            Some(BarHit::Clock) => Some((
                if pin == Pin::Locked {
                    "The frontmatter doesn't parse"
                } else if due.is_some() {
                    "Change the due time"
                } else {
                    "Set a due time"
                }
                .into(),
                false,
            )),
            Some(BarHit::Due(None)) => Some(("Clear the due time".into(), false)),
            Some(BarHit::Due(Some(preset))) => {
                let now = due::now();
                let all = due::presets(now, &Local, self.style.morning, self.style.evening);
                all.into_iter()
                    .find(|(p, _)| *p == preset)
                    .map(|(p, d)| (format!("{}: {}", p.name(), d.when(&Local, now)), false))
            }
            Some(BarHit::Trash) => Some((
                if armed {
                    "Move to Trash?"
                } else {
                    "Move to Trash"
                }
                .into(),
                false,
            )),
            Some(BarHit::Archive) => Some((ARCHIVE_LABEL.to_string(), false)),
            Some(BarHit::Pin) => Some((pin_label(&pin), false)),
            _ => None,
        };
        if let Some((text, strike)) = name {
            let control = matches!(
                hover,
                Some(
                    BarHit::Pin
                        | BarHit::Archive
                        | BarHit::Trash
                        | BarHit::Color
                        | BarHit::Swatch(_)
                        | BarHit::Clock
                        | BarHit::Due(_)
                )
            );
            let area = self.bar_rect(self.face(key)?);
            let start = if control { area.x } else { layout.name_x };
            let end = layout.name_end;
            let above =
                end - start < 90.0 || !layout.swatches.is_empty() || !layout.due_row.is_empty();
            let width = if above { area.w } else { end - start };
            if let Some(l) = self.line(LineKey {
                text,
                color: ink,
                width: width as i32,
                strike,
                tooltip: false,
            }) {
                let (x, y) = if above {
                    (area.right() - l.w, area.y - l.h - 3.0)
                } else {
                    (
                        if control { end - l.w } else { start },
                        layout.mid - l.h / 2.0,
                    )
                };
                plan.name = Some((l.buffer, Rect::new(x, y, l.w, l.h)));
            }
        }
        // With nothing hovered, the due time, beside the controls.
        if plan.name.is_none()
            && let Some(d) = &due
            && grid.is_none()
            && (!open || layout.swatches.is_empty())
            && layout.due_row.is_empty()
        {
            let room = layout.name_end - layout.name_x;
            let wanted = d.label.chars().count() as f64 * 6.0;
            if room >= wanted
                && let Some(l) = self.line(LineKey {
                    text: d.label.clone(),
                    color: if d.overdue { self.style.overdue } else { ink },
                    width: room as i32,
                    strike: false,
                    tooltip: false,
                })
            {
                let r = Rect::new(layout.name_end - l.w, layout.mid - l.h / 2.0, l.w, l.h);
                plan.name = Some((l.buffer, r));
            }
        }
        Some(plan)
    }

    /// Show the plan for the revealed (or open) note's bar.
    fn show_bar(&mut self, open: bool, plan: Option<BarPlan>) {
        let (g, no_input, qh) = (&self.g, &self.no_input, &self.qh);
        let parent = self.layer.wl_surface();
        let bar = match open {
            true => &mut self.open_bar,
            false => &mut self.reveal_bar,
        };
        let attached = bar.apply(plan, || Sub::new(g, parent, no_input, qh));
        self.attached.extend(attached);
    }

    /// Stack the subsurfaces: the `+` button at the bottom, then the
    /// notes in their paint order, each held one with its parts on top
    /// of it.
    fn restack(&mut self) {
        let mut order: Vec<&Sub> = self.backdrops.iter().flatten().collect();
        order.push(&self.plus);
        for card in self.deck.paint_order() {
            if let Some(slot) = self.slots.iter().find(|s| s.key == card.key) {
                order.extend([&slot.under, &slot.sub]);
            }
            if self.deck.open() == Some(card.key.as_str()) {
                order.extend([&self.text, &self.notice]);
                order.extend(self.open_bar.subs());
            }
            if self.deck.revealed() == Some(card.key.as_str()) {
                order.extend(self.reveal_bar.subs());
            }
        }
        for slot in &self.row_slots {
            order.extend([&slot.disc, &slot.glyph, &slot.count]);
        }
        order.push(&self.row_label);
        let surfaces: Vec<WlSurface> = order.iter().map(|s| s.surface().clone()).collect();
        if surfaces == self.stacking {
            return;
        }
        for pair in order.windows(2) {
            pair[1].place_above(pair[0].surface());
        }
        self.stacking = surfaces;
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
}

/// What a file bar shows this frame.
#[derive(Default)]
struct BarPlan {
    /// The bar's, times each part's.
    alpha: f64,
    icons: Vec<(Option<WlBuffer>, Rect, f64)>,
    /// Name labels and the due choices, with each one's alpha.
    labels: Vec<(WlBuffer, Rect, f64)>,
    more: Option<(WlBuffer, Rect)>,
    archive: Option<(Option<WlBuffer>, Rect, f64)>,
    pin: Option<(Option<WlBuffer>, Rect, f64)>,
    name: Option<(WlBuffer, Rect)>,
}

impl BarSubs {
    /// Bottom first.
    fn subs(&self) -> impl Iterator<Item = &Sub> {
        self.icons.iter().chain(&self.labels).chain([
            &self.more,
            &self.archive,
            &self.pin,
            &self.name,
        ])
    }
}

fn place_image(
    sub: &mut Sub,
    image: &Sized,
    area: Rect,
    surface: Rect,
    scale: f64,
    alpha: f64,
) -> Option<WlBuffer> {
    let rect = Rect::new(area.x, area.y, image.w, image.h);
    let (rect, src) = clip(
        rect,
        (image.w * scale, image.h * scale),
        None,
        intersect(area, surface)?,
    )?;
    sub.show(&image.buffer);
    sub.crop(src);
    sub.place(IRect::round(rect));
    sub.set_alpha(alpha);
    sub.commit();
    Some(image.buffer.clone())
}

fn grid_icon(cell: Rect) -> Rect {
    let size = 24.0_f64.min(cell.w).min(cell.h);
    Rect::new(cell.x + (cell.w - size) / 2.0, cell.y, size, size)
}

fn remove_rect(icon: Rect) -> Rect {
    let size = (icon.w * 0.65).min(12.0);
    Rect::new(icon.right() - size, icon.y, size, size)
}

/// What a due choice says in the row.
fn due_label(choice: Option<Preset>) -> &'static str {
    choice.map_or("×", Preset::short)
}

/// What the pin icon says it does.
fn pin_label(pin: &Pin) -> String {
    match pin {
        Pin::None => "No window to pin to".into(),
        Pin::Locked => "The frontmatter doesn't parse".into(),
        Pin::Keep => "Keep as app note (⇧: match window title)".into(),
        Pin::Unpinned(app) => format!("Pin to {app} (⇧: this window)"),
        Pin::Pinned { global_after: true } => "Unpin (show everywhere)".into(),
        Pin::Pinned {
            global_after: false,
        } => "Unpin".into(),
    }
}

/// (`x`, `y`) turned `deg` degrees clockwise around `c`.
fn rotate((x, y): (f64, f64), c: (f64, f64), deg: f64) -> (f64, f64) {
    let (s, co) = deg.to_radians().sin_cos();
    let (dx, dy) = (x - c.0, y - c.1);
    (c.0 + dx * co - dy * s, c.1 + dx * s + dy * co)
}

/// `img` in one color, `rgba`, keeping its shape.
fn tinted(img: &Image, rgba: u32) -> Image {
    let [r, g, b, a] = rgba.to_be_bytes();
    let ink = a as f64 / 255.0 * 0.85;
    let mut data = Vec::with_capacity(img.data.len());
    for px in img.data.as_chunks::<4>().0 {
        let alpha = px[3] as f64 / 255.0 * ink;
        let c = |v: u8| (v as f64 * alpha).round() as u8;
        data.extend_from_slice(&[c(b), c(g), c(r), (alpha * 255.0).round() as u8]);
    }
    Image { data, ..*img }
}

/// A line through `img` (a line of text), in `rgba`.
fn strike(img: &mut Image, rgba: u32, scale: i32) {
    let [r, g, b, a] = rgba.to_be_bytes();
    let alpha = a as f64 / 255.0;
    let src = [b, g, r].map(|v| v as f64 * alpha);
    let w = img.width as usize;
    let y0 = (img.height as f64 * 0.55).round() as usize;
    for y in y0..(y0 + scale.max(1) as usize).min(img.height as usize) {
        for x in 0..w {
            let i = (y * w + x) * 4;
            let px = &mut img.data[i..i + 4];
            for k in 0..3 {
                px[k] = (src[k] + px[k] as f64 * (1.0 - alpha)).round().min(255.0) as u8;
            }
            px[3] = (alpha * 255.0 + px[3] as f64 * (1.0 - alpha))
                .round()
                .min(255.0) as u8;
        }
    }
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// How far a wheel event scrolls, logical pixels.
fn axis(a: &AxisScroll) -> f64 {
    match (a.absolute, a.discrete) {
        (v, _) if v != 0.0 => v,
        (_, d) => d as f64 * LINE,
    }
}

impl Drop for Column {
    fn drop(&mut self) {
        self.slots.clear();
        self.forget_drawn();
        for b in self.stale.drain(..) {
            b.destroy();
        }
        self.viewport.destroy();
        self.transparent.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            intersect(a, Rect::new(5.0, -5.0, 10.0, 10.0)),
            Some(Rect::new(5.0, 0.0, 5.0, 5.0))
        );
        assert_eq!(intersect(a, Rect::new(10.0, 0.0, 5.0, 5.0)), None);
    }

    #[test]
    fn rotating() {
        let (x, y) = rotate((10.0, 0.0), (0.0, 0.0), 90.0);
        assert!((x - 0.0).abs() < 1e-9 && (y - 10.0).abs() < 1e-9);
        assert_eq!(rotate((3.0, 4.0), (3.0, 4.0), 12.0), (3.0, 4.0));
    }

    #[test]
    fn tinting_keeps_the_shape() {
        let img = Image {
            width: 2,
            height: 1,
            data: vec![10, 200, 30, 255, 0, 0, 0, 0],
        };
        let t = tinted(&img, 0x204060ff);
        assert_eq!(t.data[4..], [0, 0, 0, 0]);
        let a = t.data[3];
        assert!(a > 200 && a < 255);
        // Premultiplied blue, green, red of the ink.
        assert_eq!(t.data[0], (0x60 as f64 * a as f64 / 255.0).round() as u8);
    }

    #[test]
    fn pin_labels() {
        assert_eq!(
            pin_label(&Pin::Pinned { global_after: true }),
            "Unpin (show everywhere)"
        );
        assert!(pin_label(&Pin::Unpinned("firefox".into())).starts_with("Pin to firefox"));
    }
}
