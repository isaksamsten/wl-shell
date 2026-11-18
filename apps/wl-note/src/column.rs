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
//! - **Sheet, open:** straight, without text: the title (a field drawn on
//!   the main thread, editable), the text (a document from the worker,
//!   scrolled) and the bar's icons are subsurfaces above it.
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
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::reexports::calloop::RegistrationToken;
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::Image;
use wl_shell_render::doc::{self, Doc, DocStyle, Hit, Target};
use wl_shell_render::icons::NamedIcon;
use wl_shell_render::paper::{self, Paper};
use wl_shell_render::shadow::IconShadow;
use wl_shell_render::text::{self, Field, Text};
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_ui::cache::shm_buffer;
use wl_shell_ui::geom::clip;
use wl_shell_ui::{IRect, Rect, Sub, cache, lerp};

use crate::app::WlNote;
use crate::bar;
use crate::config::{Config, Edge, NoteLayer};
use crate::deck::{Card, Deck, Item, LIFT, Metrics, On, Role};
use crate::markdown::Content;
use crate::papers::{Papers, Sheet};
use crate::title::Title;

/// The text's distance from a sheet's body's sides, and from its top.
const PAD_X: f64 = 12.0;
const PAD_Y: f64 = 10.0;
/// How far papers reach behind the edge past how far they are out, so
/// a tilt or a lift never shows their end.
const BEHIND: f64 = 10.0;
/// Corner radii of tabs and sheets.
const TAB_RADIUS: f64 = 7.0;
const SHEET_RADIUS: f64 = 8.0;
/// A label starts this far from its margin's top.
const LABEL_PAD: f64 = 8.0;
/// Between a label's letters, ems.
const LABEL_SPACING: f32 = 0.12;
/// The font titles, labels and names are set in.
const UI_FONT: &str = "sans-serif";
/// A title's size, of the font size (a doc's level 2 heading).
const TITLE_SCALE: f64 = 1.15;
/// File bar icons and the gap between them.
const ICON: f64 = 16.0;
const ICON_GAP: f64 = 8.0;
/// The most of an open note's text that is rendered, logical pixels.
const DOC_MAX: i32 = 3000;
/// Documents kept rendered (the open one, and the last revealed).
const DOCS: usize = 4;
/// Name labels kept.
const LINES: usize = 48;
/// A wheel click or an arrow key scrolls this far.
const LINE: f64 = 40.0;
/// How far the pointer moves along the edge with a margin pressed before
/// it is dragged rather than clicked.
const DRAG_START: f64 = 6.0;
/// What a crumpled note shrinks to.
const CRUMPLED: f64 = 0.2;
/// Icons for files whose own aren't known (yet).
const FILE_ICONS: [&str; 3] = ["text-x-generic", "application-x-generic", "unknown"];
/// What the archive icon says it does.
const ARCHIVE_LABEL: &str = "Archive (hide this note)";

/// The fonts on the main thread, for the open note's title: loaded in the
/// background, None until then.
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
    pub shadow: Option<IconShadow>,
    /// Lifted headers' and sheets'.
    pub lifted: Option<IconShadow>,
    pub font: String,
    pub font_size: f64,
    pub link: u32,
    /// The `+` button's color and its plus's.
    pub plus_color: u32,
    pub plus_glyph: u32,
}

impl Style {
    pub fn new(config: &Config) -> Self {
        let theme = config.theme();
        let (white, ink) = theme.preset("white");
        Self {
            edge: config.edge,
            layer: config.layer,
            metrics: Metrics {
                marker_width: config.marker_width as f64,
                marker_length: config.marker_length as f64,
                marker_gap: config.marker_gap as f64,
                band: config.edge_band as f64,
                padding: config.marker_padding as f64,
                header: config.header as f64,
                header_length: config.header_length as f64,
                sheet_width: config.sheet_width as f64,
                sheet_height: config.sheet_height as f64,
                plus: 22.0,
            },
            marker_radius: config.marker_radius as f64,
            reserve_band: config.reserve_edge_band,
            file_bar: config.file_bar as f64,
            reveal_delay: Duration::from_millis(config.reveal_delay as u64),
            hide_delay: Duration::from_millis(config.hide_delay as u64),
            response: config.response as f64,
            shadow: config.shadow(),
            lifted: config.lifted_shadow(),
            font: config.font.clone(),
            font_size: config.font_size as f64,
            link: theme.link_color,
            plus_color: (white & !0xff) | 0xe6,
            plus_glyph: (ink & !0xff) | 0xb0,
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

    /// The open title's.
    fn title(&self, color: u32) -> text::Style {
        text::Style {
            font: UI_FONT.into(),
            size: self.font_size * TITLE_SCALE,
            color,
            background: 0,
        }
    }

    /// The open title's height.
    fn title_h(&self) -> f64 {
        text::line_height(self.font_size * TITLE_SCALE) as f64
    }

    /// Where an open note's text starts below its title.
    fn text_top(&self) -> f64 {
        PAD_Y + self.title_h() + (self.font_size * 0.6).round()
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
}

/// What the column knows of a note shown.
struct NoteView {
    content: Rc<Content>,
    text: u32,
    files: Vec<FileLink>,
    pin: Pin,
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
    /// Open this note in the editor.
    Edit(String),
    /// Make a new note.
    New,
    /// Open this file.
    File(PathBuf),
    /// Pin this note to the context's app (or, with true, its window).
    Pin(String, bool),
    /// Unpin it from the context's window.
    Unpin(String),
    /// Save this note's title (empty: drop it).
    Title(String, String),
    /// Hide this note.
    Hide(String),
    /// This note was dropped at this slot of the column (its keys, in
    /// order).
    Reorder(String, Vec<String>, usize),
}

/// The column's timers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// The pointer rested on a margin: reveal it.
    Reveal,
    /// The pointer left the column: back to markers.
    Hide,
}

/// A card's subsurface, and the paper it shows.
struct Slot {
    key: String,
    sub: Sub,
    /// The paper shown, and its sheet.
    shown: Option<(Paper, Sheet)>,
    /// Where its paper is on the surface (unclipped).
    paper: Rect,
    /// Where it takes the pointer, if it does.
    hit: Option<Rect>,
}

/// An open (or about to be) note's text, rendered.
struct DocEntry {
    key: String,
    /// What was asked of the worker last, and its tag.
    want: Doc,
    tag: u64,
    shown: Option<RenderedDoc>,
}

struct RenderedDoc {
    buffer: WlBuffer,
    /// Logical size.
    w: f64,
    h: f64,
    hits: Vec<Hit>,
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
    /// The `+n` for those that don't fit.
    More,
    Archive,
    Pin,
}

/// Where the parts of a sheet's file bar are, on the surface.
struct BarLayout {
    /// The file's index and its icon's rect, for those shown.
    icons: Vec<(usize, Rect)>,
    /// How many more, and where the `+n` is.
    more: Option<(usize, Rect)>,
    archive: Option<Rect>,
    pin: Option<Rect>,
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

/// A name label: its text, color, width and whether it is struck
/// through.
#[derive(Clone, Debug, PartialEq)]
struct LineKey {
    text: String,
    color: u32,
    width: i32,
    strike: bool,
}

/// The title as shown: what was drawn, and its buffer.
struct TitleShown {
    field: Field,
    buffer: WlBuffer,
    w: f64,
    h: f64,
}

pub struct Column {
    pub output: WlOutput,
    pub name: String,
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
    /// The `+` button's buffer, and the scale it was drawn at.
    plus_shown: Option<(i32, WlBuffer, paper::Rendered)>,
    /// Where the `+` button is, while it takes the pointer.
    plus_rect: Option<Rect>,
    /// The open note's text, title and bar; the revealed note's bar.
    text: Sub,
    title: Sub,
    open_bar: BarSubs,
    reveal_bar: BarSubs,
    /// The buffer `text` shows.
    text_shown: Option<WlBuffer>,
    title_shown: Option<TitleShown>,
    /// The title being edited.
    editing: Option<Title>,
    docs: Vec<DocEntry>,
    next_tag: u64,
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
    /// Label and title buffers attached by the last frame.
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
        }
    }

    fn clear(&mut self) {
        for s in self.icons.iter_mut().chain([
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
            .collect()
    }
}

impl Column {
    /// An empty column on `output` (named `name`) at `scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: &Core<WlNote>,
        output: WlOutput,
        name: String,
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
        let (plus, text, title) = (sub(), sub(), sub());
        let open_bar = BarSubs::new(&g, &parent, &no_input, &qh);
        let reveal_bar = BarSubs::new(&g, &parent, &no_input, &qh);
        let deck = Deck::new(style.metrics, style.response);
        let column = Self {
            output,
            name,
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
            plus_shown: None,
            plus_rect: None,
            text,
            title,
            open_bar,
            reveal_bar,
            text_shown: None,
            title_shown: None,
            editing: None,
            docs: Vec::new(),
            next_tag: 0,
            stale: Vec::new(),
            scroll: 0.0,
            file_icons: HashMap::new(),
            asked_icons: HashSet::new(),
            icons: HashMap::new(),
            icon_buffers: HashMap::new(),
            lines: Vec::new(),
            bar_hover: None,
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
        self.deck.open()
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
            .filter(|c| !c.leaving)
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
        self.refresh_docs();
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
        self.refresh_docs();
        self.kick();
    }

    /// Forget icons, labels and the button drawn for another scale or
    /// style; the subsurfaces showing them are cleared, so they can go.
    fn forget_drawn(&mut self) {
        self.open_bar.clear();
        self.reveal_bar.clear();
        self.plus.clear();
        self.title.clear();
        self.stale.extend(self.icon_buffers.drain().map(|(_, b)| b));
        self.stale.extend(
            self.lines
                .drain(..)
                .filter_map(|(_, s)| s.map(|s| s.buffer)),
        );
        self.stale.extend(self.plus_shown.take().map(|(_, b, _)| b));
        self.stale.extend(self.title_shown.take().map(|t| t.buffer));
        self.icons.clear();
    }

    /// Show `notes`, animating the change (see deck.rs). The program keeps
    /// the open note among them.
    pub fn set_notes(&mut self, notes: Vec<Shown>) {
        self.notes = (notes.iter())
            .map(|n| {
                let view = NoteView {
                    content: n.content.clone(),
                    text: n.text,
                    files: n.files.clone(),
                    pin: n.pin.clone(),
                };
                (n.item.key.clone(), view)
            })
            .collect();
        self.deck.set(notes.into_iter().map(|n| n.item).collect());
        // The open note went (removed, hidden): the keyboard goes back.
        if self.deck.open().is_none() {
            self.editing = None;
            self.set_keyboard(KeyboardInteractivity::None);
        }
        // Forget the documents of notes gone; render the others again if
        // they changed.
        let gone: Vec<usize> = (0..self.docs.len())
            .filter(|i| !self.notes.contains_key(&self.docs[*i].key))
            .collect();
        for i in gone.into_iter().rev() {
            let e = self.docs.remove(i);
            self.stale.extend(e.shown.map(|s| s.buffer));
        }
        let paths: Vec<PathBuf> = (self.notes.values())
            .flat_map(|n| n.files.iter().map(|f| f.path.clone()))
            .collect();
        for path in paths {
            self.ask_file_icons(&path);
        }
        self.refresh_docs();
        self.kick();
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
    /// keyboard. Returns a title edit of another note to save.
    pub fn open(&mut self, core: &Core<WlNote>, key: &str) -> Vec<Act> {
        let again = self.deck.open() == Some(key);
        let acts = match again {
            true => Vec::new(),
            false => self.commit_title(),
        };
        self.cancel_reveal(core);
        self.deck.open_note(key);
        if !again {
            self.scroll = 0.0;
            self.ensure_doc(key);
        }
        self.set_keyboard(KeyboardInteractivity::Exclusive);
        self.kick();
        acts
    }

    /// Close the open note and give the keyboard back. Returns its title
    /// edit to save.
    pub fn close(&mut self) -> Vec<Act> {
        let acts = self.commit_title();
        self.deck.close();
        self.set_keyboard(KeyboardInteractivity::None);
        self.kick();
        acts
    }

    /// End the title's edit: the save it asks for, if it changed.
    fn commit_title(&mut self) -> Vec<Act> {
        let Some(title) = self.editing.take() else {
            return Vec::new();
        };
        self.kick();
        match (self.deck.open(), title.changed()) {
            (Some(key), true) => vec![Act::Title(key.to_string(), title.text.trim().to_string())],
            _ => Vec::new(),
        }
    }

    /// Start editing the open note's title, the caret at `x` on the
    /// surface (None: at its end).
    fn edit_title(&mut self, x: Option<f64>) {
        let Some(key) = self.deck.open() else {
            return;
        };
        if self.editing.is_none() {
            let text = self
                .notes
                .get(key)
                .map_or(String::new(), |n| n.content.title.clone());
            self.editing = Some(Title::new(&text));
        }
        if let (Some(x), Some(face)) = (x, self.face(key)) {
            let area = self.title_rect(face);
            let field = self.title_field();
            let mut fonts = self.fonts.borrow_mut();
            if let (Some(fonts), Some(field), Some(title)) =
                (fonts.as_mut(), field, &mut self.editing)
            {
                let at = fonts.field_index(&field, x - area.x);
                title.set_caret(at);
            }
        }
        self.kick();
    }

    /// The open note's keyboard focus came: keep it until a click
    /// elsewhere takes it.
    pub fn keyboard_enter(&mut self) {
        if self.keyboard == KeyboardInteractivity::Exclusive {
            self.set_keyboard(KeyboardInteractivity::OnDemand);
        }
    }

    /// The keyboard went elsewhere, e.g. to a window clicked: the note
    /// stays open, a title edit is kept.
    pub fn keyboard_leave(&mut self) -> Vec<Act> {
        self.commit_title()
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
        core: &Core<WlNote>,
        event: &PointerEvent,
        mods: &Modifiers,
    ) -> Vec<Act> {
        let (x, y) = event.position;
        match &event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                match self.press.is_some() {
                    true => self.press_moved(core, x, y),
                    false => self.hover_at(core, x, y),
                }
                Vec::new()
            }
            PointerEventKind::Leave { .. } => {
                self.pointer = None;
                self.bar_hover = None;
                if self.press.take().is_some() {
                    self.deck.drag_cancel();
                }
                self.cancel_reveal(core);
                self.deck.point(On::Outside);
                self.start_hide(core);
                self.kick();
                Vec::new()
            }
            PointerEventKind::Press { button, .. } if *button == BTN_LEFT => {
                self.click(core, x, y, mods.shift)
            }
            PointerEventKind::Release { button, .. } if *button == BTN_LEFT => {
                self.release(core, x, y)
            }
            PointerEventKind::Axis { vertical, .. } => {
                if let Some(key) = self.deck.open()
                    && let Some(face) = self.face(key)
                    && self.text_rect(face).contains(x, y)
                {
                    self.scroll_by(axis(vertical));
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A key while the column has the keyboard.
    pub fn key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Vec<Act> {
        let Some(key) = self.deck.open().map(String::from) else {
            return Vec::new();
        };
        if let Some(t) = &mut self.editing {
            match event.keysym {
                Keysym::Return | Keysym::KP_Enter | Keysym::Tab => return self.commit_title(),
                Keysym::Escape => self.editing = None,
                Keysym::BackSpace => t.backspace(mods.ctrl),
                Keysym::Delete | Keysym::KP_Delete => t.delete(mods.ctrl),
                Keysym::Left | Keysym::KP_Left => t.left(mods.ctrl),
                Keysym::Right | Keysym::KP_Right => t.right(mods.ctrl),
                Keysym::Home | Keysym::KP_Home => t.home(),
                Keysym::End | Keysym::KP_End => t.end(),
                _ if !mods.ctrl && !mods.alt => {
                    if let Some(s) = &event.utf8 {
                        t.insert(s);
                    }
                }
                _ => {}
            }
            self.kick();
            return Vec::new();
        }
        let page =
            (self.style.metrics.sheet_height - self.style.text_top() - self.style.file_bar) * 0.9;
        match event.keysym {
            Keysym::Escape => return vec![Act::Close],
            Keysym::Return | Keysym::KP_Enter if mods.ctrl => return vec![Act::Close],
            Keysym::e | Keysym::E if mods.ctrl => return vec![Act::Edit(key)],
            Keysym::F2 => self.edit_title(None),
            Keysym::Up => self.scroll_by(-LINE),
            Keysym::Down => self.scroll_by(LINE),
            Keysym::Page_Up => self.scroll_by(-page),
            Keysym::Page_Down | Keysym::space => self.scroll_by(page),
            Keysym::Home => self.scroll_by(-f64::INFINITY),
            Keysym::End => self.scroll_by(f64::INFINITY),
            _ => {}
        }
        Vec::new()
    }

    /// One of the column's timers fired.
    pub fn timer_due(&mut self, which: Timer) {
        match which {
            Timer::Reveal => {
                if let Some((key, _)) = self.reveal.take() {
                    self.deck.reveal_due(&key);
                    if self.deck.revealed() == Some(key.as_str()) {
                        // Likely to be opened next.
                        self.ensure_doc(&key);
                    }
                }
            }
            Timer::Hide => {
                self.hide = None;
                if self.pointer.is_none() {
                    self.deck.hide_due();
                }
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

    /// The pointer is at (`x`, `y`): follow it with the levels, and the
    /// bars' hover.
    fn hover_at(&mut self, core: &Core<WlNote>, x: f64, y: f64) {
        self.pointer = Some((x, y));
        self.cancel_hide(core);
        let bar = self.bar_at(x, y);
        let mut changed = bar != self.bar_hover;
        self.bar_hover = bar;
        let before = (
            self.deck.level(),
            self.deck.revealed().map(String::from),
            self.deck.hover().map(String::from),
        );
        let on = self.on_at(x, y);
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
            if let Some(k) = &after.1 {
                self.ensure_doc(&k.clone());
            }
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
    /// what a click on it does), a dragged one drops.
    fn release(&mut self, core: &Core<WlNote>, x: f64, y: f64) -> Vec<Act> {
        let Some(p) = self.press.take() else {
            return Vec::new();
        };
        let act = match (p.dragging, p.revealed) {
            (false, false) => Act::Open(p.key),
            (false, true) => self.revealed_click(p.key, p.x, p.y),
            (true, _) => match self.deck.drag_end() {
                Some((key, gap)) => Act::Reorder(key, self.keys().map(String::from).collect(), gap),
                None => return Vec::new(),
            },
        };
        self.hover_at(core, x, y);
        self.kick();
        vec![act]
    }

    fn click(&mut self, core: &Core<WlNote>, x: f64, y: f64, shift: bool) -> Vec<Act> {
        let on = self.on_at(x, y);
        let open = self.deck.open().map(String::from);
        let on_title = (open.as_deref())
            .and_then(|k| self.face(k))
            .is_some_and(|f| self.title_rect(f).contains(x, y))
            && on == On::Sheet(open.clone().unwrap_or_default());
        let mut acts = match on_title {
            true => Vec::new(),
            false => self.commit_title(),
        };
        match on {
            On::Plus => acts.push(Act::New),
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
                acts.extend(self.open_click(&k, x, y, shift))
            }
            // The file bar acts at once; the rest of a revealed sheet on
            // release, unless it is dragged (folding back to its margin,
            // under the pointer).
            On::Sheet(k) => match self.bar_at(x, y) {
                Some((b, _)) if b == k => acts.push(self.revealed_click(k, x, y)),
                _ => {
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
            },
            On::Strip | On::Outside => {}
        }
        acts
    }

    /// What a click at (`x`, `y`) on the open note `key` does.
    fn open_click(&mut self, key: &str, x: f64, y: f64, shift: bool) -> Option<Act> {
        if let Some((k, hit)) = self.bar_at(x, y)
            && k == key
        {
            return self.bar_click(key, hit, shift);
        }
        let face = self.face(key)?;
        if self.margin_rect(face).contains(x, y) {
            return Some(Act::Close);
        }
        if self.title_rect(face).contains(x, y) {
            self.edit_title(Some(x));
            return None;
        }
        let area = self.text_rect(face);
        if !area.contains(x, y) {
            return None;
        }
        let d = self.docs.iter().find(|d| d.key == key)?.shown.as_ref()?;
        match doc::hit(&d.hits, x - area.x, y - area.y + self.scroll)? {
            Target::Task(n) => Some(Act::Task(key.to_string(), n)),
            Target::Link(n) => Some(Act::Link(key.to_string(), n)),
        }
    }

    /// What a click at (`x`, `y`) on the revealed note `key` does: its
    /// files, checkboxes and links work; anywhere else opens it.
    fn revealed_click(&mut self, key: String, x: f64, y: f64) -> Act {
        if let Some((k, hit)) = self.bar_at(x, y)
            && k == key
            && let Some(act) = self.bar_click(&key, hit, false)
        {
            return act;
        }
        let target = (self.slots.iter().find(|s| s.key == key))
            .and_then(|s| Some((s.paper, s.shown.as_ref()?)))
            .and_then(|(r, (_, sheet))| doc::hit(&sheet.hits, x - r.x, y - r.y));
        match target {
            Some(Target::Task(n)) => Act::Task(key, n),
            Some(Target::Link(n)) => Act::Link(key, n),
            None => Act::Open(key),
        }
    }

    fn bar_click(&self, key: &str, hit: BarHit, shift: bool) -> Option<Act> {
        let note = self.notes.get(key)?;
        match hit {
            BarHit::File(i) => Some(Act::File(note.files.get(i)?.path.clone())),
            BarHit::More => None,
            BarHit::Archive => Some(Act::Hide(key.to_string())),
            BarHit::Pin => match &note.pin {
                Pin::Unpinned(_) => Some(Act::Pin(key.to_string(), shift)),
                Pin::Pinned { .. } => Some(Act::Unpin(key.to_string())),
                Pin::None | Pin::Locked => None,
            },
        }
    }

    /// What the pointer at (`x`, `y`) is on, topmost first.
    fn on_at(&self, x: f64, y: f64) -> On {
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
        let layout = self.bar_layout(&key)?;
        let near = |r: &Rect| r.outset(ICON_GAP / 2.0).contains(x, y);
        if let Some((i, _)) = layout.icons.iter().find(|(_, r)| near(r)) {
            return Some((key, BarHit::File(*i)));
        }
        if layout.more.is_some_and(|(_, r)| near(&r)) {
            return Some((key, BarHit::More));
        }
        if layout.archive.is_some_and(|r| near(&r)) {
            return Some((key, BarHit::Archive));
        }
        if layout.pin.is_some_and(|r| near(&r)) {
            return Some((key, BarHit::Pin));
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

    /// How far the open note's text scrolls.
    fn max_scroll(&self) -> f64 {
        let Some(key) = self.deck.open() else {
            return 0.0;
        };
        let h = (self.docs.iter().find(|d| d.key == key))
            .and_then(|d| d.shown.as_ref())
            .map_or(0.0, |d| d.h);
        let m = &self.style.metrics;
        (h - (m.sheet_height - self.style.text_top() - self.style.file_bar)).max(0.0)
    }

    // -- documents ---------------------------------------------------------

    /// What an open `key` renders: its body (the title is a field).
    fn want_doc(&self, key: &str) -> Option<Doc> {
        let n = self.notes.get(key)?;
        Some(Doc {
            blocks: n.content.body(),
            style: self.style.doc_style(n.text),
            width: self.style.content_w() as i32,
            max_height: DOC_MAX,
            scale: self.scale,
        })
    }

    /// Have the open rendering of `key` drawn, unless it is already.
    fn ensure_doc(&mut self, key: &str) {
        let Some(want) = self.want_doc(key) else {
            return;
        };
        let i = match self.docs.iter().position(|d| d.key == key) {
            Some(i) if self.docs[i].want == want => i,
            found => {
                self.next_tag += 1;
                let tag = self.next_tag;
                self.assets.doc(tag, want.clone());
                match found {
                    Some(i) => {
                        (self.docs[i].want, self.docs[i].tag) = (want, tag);
                        i
                    }
                    None => {
                        self.docs.push(DocEntry {
                            key: key.to_string(),
                            want,
                            tag,
                            shown: None,
                        });
                        self.docs.len() - 1
                    }
                }
            }
        };
        // Most recently wanted first; the oldest go.
        let entry = self.docs.remove(i);
        self.docs.insert(0, entry);
        while self.docs.len() > DOCS {
            if let Some(e) = self.docs.pop() {
                self.stale.extend(e.shown.map(|s| s.buffer));
            }
        }
    }

    /// The notes or the scale changed: the open note (and the revealed
    /// one) may need drawing again.
    fn refresh_docs(&mut self) {
        let keys: Vec<String> = (self.deck.open().into_iter())
            .chain(self.deck.revealed())
            .map(String::from)
            .collect();
        for k in keys {
            self.ensure_doc(&k);
        }
    }

    /// A rendered document from the worker.
    pub fn doc_ready(&mut self, tag: u64, doc: &Doc, rendered: &doc::Rendered) {
        let Some(e) = (self.docs.iter_mut()).find(|e| e.tag == tag && e.want == *doc) else {
            return;
        };
        let buffer = match shm_buffer(&self.g, &rendered.image, &self.qh) {
            Ok(buffer) => buffer,
            Err(e) => return warn!("text buffer: {e}"),
        };
        let new = RenderedDoc {
            buffer,
            w: rendered.width as f64,
            h: rendered.height as f64,
            hits: rendered.hits.clone(),
        };
        if let Some(old) = e.shown.replace(new) {
            self.stale.push(old.buffer);
        }
        self.scroll = self.scroll.min(self.max_scroll());
        self.kick();
    }

    // -- the title -----------------------------------------------------------

    /// The fonts arrived: the title can be drawn.
    pub fn fonts_ready(&mut self) {
        self.kick();
    }

    /// The open note's title as a field to draw.
    fn title_field(&self) -> Option<Field> {
        let key = self.deck.open()?;
        let note = self.notes.get(key)?;
        let face = self.face(key)?;
        let area = self.title_rect(face);
        let (text, caret) = match &self.editing {
            Some(t) => (t.text.clone(), Some(t.caret)),
            None => (note.content.title.clone(), None),
        };
        let scroll = match (&self.editing, &self.title_shown) {
            (Some(_), Some(shown)) => shown.field.scroll,
            _ => 0.0,
        };
        Some(Field {
            text,
            caret,
            style: self.style.title(note.text),
            bold: true,
            width: area.w.round().max(1.0) as i32,
            scroll,
            scale: self.scale,
        })
    }

    /// Draw the open note's title if it changed (once the fonts are
    /// there).
    fn draw_title(&mut self) {
        let Some(field) = self.title_field() else {
            return;
        };
        if self.title_shown.as_ref().is_some_and(|s| s.field == field) {
            return;
        }
        let drawn = {
            let mut fonts = self.fonts.borrow_mut();
            let Some(fonts) = fonts.as_mut() else {
                return;
            };
            fonts.field(&field)
        };
        let buffer = match shm_buffer(&self.g, &drawn.image, &self.qh) {
            Ok(buffer) => buffer,
            Err(e) => return warn!("title buffer: {e}"),
        };
        let shown = TitleShown {
            field: Field {
                scroll: drawn.scroll,
                ..field
            },
            buffer,
            w: drawn.width as f64,
            h: drawn.height as f64,
        };
        if let Some(old) = self.title_shown.replace(shown) {
            self.stale.push(old.buffer);
        }
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
        if key.size != ICON as u32 * self.scale as u32 || key.shadow.is_some() {
            return;
        }
        self.icons.insert(key, image);
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
            style: self.style.small(key.color),
        };
        self.assets.line(0, request);
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
                && self.style.small(key.color) == request.style;
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
        if self.deck.moving() && !self.frame_pending {
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

    /// Where the part of a card that is out takes the pointer: as far as
    /// it is out, as long as it is (at most its paper's `ph`).
    fn hit_rect(&self, card: &Card, ph: f64) -> Option<Rect> {
        let o = self.out_now(card);
        let l = card.len.value.min(ph);
        let r = match self.style.edge {
            Edge::Right => Rect::new(self.width as f64 - o, card.y.value - l / 2.0, o, l),
            Edge::Left => Rect::new(0.0, card.y.value - l / 2.0, o, l),
        };
        intersect(r, self.surface_rect())
    }

    /// Where a held note's sheet is, as on screen (without what reaches
    /// behind the edge).
    fn face(&self, key: &str) -> Option<Rect> {
        let card = self.deck.cards().iter().find(|c| c.key == key)?;
        let m = &self.style.metrics;
        let h = m.sheet_height;
        Some(self.out_rect(self.out_now(card), m.sheet_width, card.y.value - h / 2.0, h))
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

    fn title_rect(&self, face: Rect) -> Rect {
        let body = self.body_rect(face);
        let w = self.style.content_w();
        Rect::new(body.x + PAD_X, face.y + PAD_Y, w, self.style.title_h())
    }

    fn text_rect(&self, face: Rect) -> Rect {
        let body = self.body_rect(face);
        let top = self.style.text_top();
        let h = (face.h - top - self.style.file_bar).max(0.0);
        Rect::new(body.x + PAD_X, face.y + top, self.style.content_w(), h)
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
        if !self.deck.held(key) || (!open && note.files.is_empty()) {
            return None;
        }
        let face = self.face(key)?;
        let bar = self.bar_rect(face);
        let mid = bar.y + bar.h / 2.0;
        // The archive and pin icons at the end of an open sheet's.
        let controls_w = if open { 2.0 * (ICON + ICON_GAP) } else { 0.0 };
        let (n, more) = bar::fit(note.files.len(), bar.w - controls_w, ICON, ICON_GAP);
        let slot = |i: usize| {
            Rect::new(
                bar.x + i as f64 * (ICON + ICON_GAP),
                mid - ICON / 2.0,
                ICON,
                ICON,
            )
        };
        let used = n + more.map_or(0, |_| 1);
        let mut layout = BarLayout {
            icons: (0..n).map(|i| (i, slot(i))).collect(),
            more: more.map(|count| (count, slot(n))),
            pin: open.then(|| Rect::new(bar.right() - ICON, mid - ICON / 2.0, ICON, ICON)),
            archive: open.then(|| {
                let x = bar.right() - 2.0 * ICON - ICON_GAP;
                Rect::new(x, mid - ICON / 2.0, ICON, ICON)
            }),
            name_x: bar.x + used as f64 * (ICON + ICON_GAP),
            name_end: bar.right() - controls_w,
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
            let (x, mid) = rotate((layout.name_x, layout.mid), (cx, cy), tilt);
            layout.name_end += x - layout.name_x;
            (layout.name_x, layout.mid) = (x, mid);
        }
        Some(layout)
    }

    // -- papers --------------------------------------------------------------

    /// A note's margin: its title turned, and the crease.
    fn margin(&self, key: &str) -> Option<paper::Margin> {
        let note = self.notes.get(key)?;
        let m = &self.style.metrics;
        let ink = (note.text & !0xff) | 0xc8;
        Some(paper::Margin {
            width: m.header as i32,
            ink: note.text,
            crease: true,
            label: Some(paper::Label {
                text: note.content.title.to_uppercase(),
                style: self.style.small(ink),
                spacing: LABEL_SPACING,
                top: LABEL_PAD as i32,
                len: (m.header_length * 0.7 - 2.0 * LABEL_PAD).max(8.0) as i32,
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
                margin: self.margin(&card.key),
                shadow: match role {
                    Role::Lifted => self.style.lifted,
                    _ => self.style.shadow,
                },
                ..base
            },
            Role::Sheet => {
                let note = self.notes.get(&card.key);
                let open = self.deck.open() == Some(card.key.as_str());
                let files = note.is_some_and(|n| !n.files.is_empty());
                let bar = if open || files {
                    self.style.file_bar
                } else {
                    0.0
                };
                let content = note.filter(|_| !open).map(|n| {
                    let clip = m.sheet_height - PAD_Y - bar.max(PAD_Y);
                    let line = text::line_height(self.style.font_size) as f64;
                    let x = match self.style.edge {
                        Edge::Right => m.header + PAD_X,
                        Edge::Left => BEHIND + PAD_X,
                    };
                    paper::Content {
                        doc: Doc {
                            blocks: n.content.sheet(),
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
                    margin: self.margin(&card.key),
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
        self.attached
            .extend(self.title_shown.as_ref().map(|t| t.buffer.clone()));
        let surface = self.surface_rect();
        let scale = self.scale as f64;
        let cards: Vec<Card> = self.deck.cards().to_vec();
        self.slots.retain(|s| cards.iter().any(|c| c.key == s.key));
        for card in &cards {
            if !self.slots.iter().any(|s| s.key == card.key) {
                let sub = Sub::new(&self.g, self.layer.wl_surface(), &self.no_input, &self.qh);
                self.slots.push(Slot {
                    key: card.key.clone(),
                    sub,
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
            let Some((p, _)) = &self.slots[i].shown else {
                self.slots[i].sub.clear();
                self.slots[i].hit = None;
                continue;
            };
            let (pw, ph) = (p.w as f64, p.h as f64);
            // Crumpling, it stays out and shrinks toward the edge instead.
            let o = match crumpling {
                true => card.out.value,
                false => lerp(-reach, card.out.value, card.presence.value),
            };
            let paper = match edge {
                Edge::Right => Rect::new(width - o, card.y.value - ph / 2.0, pw, ph),
                Edge::Left => Rect::new(o - pw, card.y.value - ph / 2.0, pw, ph),
            };
            // Cut to its length while it grows or shrinks.
            let len = card.len.value;
            let cut = match len < ph - 0.5 && !crumpling {
                true => intersect(
                    surface,
                    Rect::new(0.0, card.y.value - len / 2.0, width, len),
                ),
                false => Some(surface),
            };
            let hit = self.hit_rect(card, ph).filter(|_| card.present());
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
                false => 1.0,
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
        }

        self.place_plus();
        self.place_open();
        let revealed = self.deck.revealed().map(String::from);
        let plan = revealed.as_deref().and_then(|k| self.bar_plan(k, false));
        self.show_bar(false, plan);
        self.restack();

        // Input: what shows of the notes, the button and the bands.
        let mut input: Vec<IRect> = (self.slots.iter())
            .filter_map(|s| s.hit)
            .chain(self.plus_rect)
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

    /// The `+` button: out from header level on, past the last note.
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
        let dist = lerp(-m.plus, m.header / 2.0 + LIFT, p);
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

    /// The open note's text, title and bar.
    fn place_open(&mut self) {
        let surface = self.surface_rect();
        let s = self.scale as f64;
        let key = self.deck.open().map(String::from);
        let alpha = key.as_deref().map_or(0.0, |k| self.open_alpha(k));
        let face = key
            .as_deref()
            .and_then(|k| self.face(k))
            .filter(|_| alpha > 0.0);
        // The text, scrolled.
        let mut shown_text = None;
        let doc = (key.as_deref().zip(face))
            .and_then(|(k, f)| Some((self.docs.iter().find(|d| d.key == k)?.shown.as_ref()?, f)));
        match doc {
            Some((d, f)) => {
                let area = self.text_rect(f);
                let image = Rect::new(area.x, area.y - self.scroll, d.w, d.h);
                match intersect(area, surface)
                    .and_then(|a| clip(image, (d.w * s, d.h * s), None, a))
                {
                    Some((rect, src)) => {
                        self.text.show(&d.buffer);
                        self.text.crop(src);
                        self.text.place(IRect::round(rect));
                        self.text.set_alpha(alpha);
                        self.text.commit();
                        shown_text = Some(d.buffer.clone());
                    }
                    None => self.text.clear(),
                }
            }
            None => self.text.clear(),
        }
        self.text_shown = shown_text;
        // The title.
        if face.is_some() {
            self.draw_title();
        }
        let title = (face.zip(self.title_shown.as_ref())).and_then(|(f, t)| {
            let area = self.title_rect(f);
            let image = Rect::new(area.x, area.y, t.w, t.h);
            Some((
                clip(image, (t.w * s, t.h * s), None, surface)?,
                t.buffer.clone(),
            ))
        });
        match title {
            Some(((rect, src), buffer)) => {
                self.title.show(&buffer);
                self.title.crop(src);
                self.title.place(IRect::round(rect));
                self.title.set_alpha(alpha);
                self.title.commit();
            }
            None => self.title.clear(),
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
                    .is_some_and(|(p, _)| {
                        p.content.is_some() && *p == self.paper(card, Role::Sheet)
                    });
                let m = &self.style.metrics;
                let t = ((card.out.value - m.header) / (m.sheet_width - m.header)).clamp(0.0, 1.0);
                if shown { t * t } else { 0.0 }
            }
        };
        let note = self.notes.get(key)?;
        let (ink, files, pin) = (note.text, note.files.clone(), note.pin.clone());
        let hover = (self.bar_hover.as_ref())
            .filter(|(k, _)| k == key)
            .map(|(_, h)| *h);
        let mut plan = BarPlan {
            alpha,
            ..BarPlan::default()
        };
        for (i, rect) in &layout.icons {
            let f = &files[*i];
            let lit = hover == Some(BarHit::File(*i));
            let buffer = self.file_buffer(&f.path, (!lit).then_some(ink));
            let a = if f.exists { 1.0 } else { 0.4 };
            plan.icons.push((buffer, *rect, a));
        }
        if let Some((count, rect)) = layout.more {
            let key = LineKey {
                text: format!("+{count}"),
                color: ink,
                width: 40,
                strike: false,
            };
            if let Some(l) = self.line(key) {
                let r = Rect::new(rect.x, rect.y + (rect.h - l.h) / 2.0, l.w, l.h);
                plan.more = Some((l.buffer, r));
            }
        }
        if let Some(rect) = layout.pin {
            let lit = hover == Some(BarHit::Pin);
            let (icon, tint, a) = match &pin {
                Pin::Pinned { .. } => (self.pin_icon(lit), None, 1.0),
                Pin::Unpinned(_) if lit => (self.pin_icon(false), None, 1.0),
                Pin::Unpinned(_) => (self.pin_icon(false), Some(ink), 0.75),
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
            Some(BarHit::File(i)) => files.get(i).map(|f| (bar::name(&f.path), !f.exists)),
            Some(BarHit::More) => {
                let rest: Vec<String> = (files.iter().skip(layout.icons.len()))
                    .map(|f| bar::name(&f.path))
                    .collect();
                Some((rest.join(", "), false))
            }
            Some(BarHit::Archive) => Some((ARCHIVE_LABEL.to_string(), false)),
            Some(BarHit::Pin) => Some((pin_label(&pin), false)),
            None => None,
        };
        if let Some((text, strike)) = name.filter(|(t, _)| !t.is_empty()) {
            // The controls' names end before the first of them.
            let right_aligned = matches!(hover, Some(BarHit::Pin | BarHit::Archive));
            let first = layout.archive.or(layout.pin);
            let end = match (right_aligned, first) {
                (true, Some(r)) => r.x - ICON_GAP,
                _ => layout.name_end,
            };
            let width = (end - layout.name_x).max(1.0) as i32;
            let key = LineKey {
                text,
                color: ink,
                width,
                strike,
            };
            if let Some(l) = self.line(key) {
                let x = if right_aligned {
                    end - l.w
                } else {
                    layout.name_x
                };
                plan.name = Some((l.buffer, Rect::new(x, layout.mid - l.h / 2.0, l.w, l.h)));
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
        let mut order: Vec<&Sub> = vec![&self.plus];
        for card in self.deck.paint_order() {
            if let Some(slot) = self.slots.iter().find(|s| s.key == card.key) {
                order.push(&slot.sub);
            }
            if self.deck.open() == Some(card.key.as_str()) {
                order.extend([&self.text, &self.title]);
                order.extend(self.open_bar.subs());
            }
            if self.deck.revealed() == Some(card.key.as_str()) {
                order.extend(self.reveal_bar.subs());
            }
        }
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
    more: Option<(WlBuffer, Rect)>,
    archive: Option<(Option<WlBuffer>, Rect, f64)>,
    pin: Option<(Option<WlBuffer>, Rect, f64)>,
    name: Option<(WlBuffer, Rect)>,
}

impl BarSubs {
    /// Bottom first.
    fn subs(&self) -> impl Iterator<Item = &Sub> {
        (self.icons.iter()).chain([&self.more, &self.archive, &self.pin, &self.name])
    }
}

/// What the pin icon says it does.
fn pin_label(pin: &Pin) -> String {
    match pin {
        Pin::None => "No window to pin to".into(),
        Pin::Locked => "The frontmatter doesn't parse".into(),
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
        for e in self.docs.drain(..) {
            self.stale.extend(e.shown.map(|s| s.buffer));
        }
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
