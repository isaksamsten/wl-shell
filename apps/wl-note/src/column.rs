//! One output's column of notes: its layer surface, the scene on it, and
//! the pointer and keys on it.
//!
//! A layer surface anchored to the edge and both sides, as wide as a
//! focused note and its shadow (so focusing never resizes it), with an
//! exclusive zone of 0. The surface itself shows a transparent pixel, and
//! its input region is only what shows of the notes, so clicks anywhere
//! else go through.
//!
//! Each note is a sync subsurface showing its sheet of paper (papers.rs),
//! placed by the deck's springs (deck.rs). The sheet reaches past the
//! surface, so it is cropped to it with the viewport, and never shows on a
//! neighboring output. What the sheet is follows what the note does:
//!
//! - **Tucked:** a sliver, `tuck` of it showing.
//! - **Peeking:** the sheet with its preview (title and first lines)
//!   drawn on, tilted; the note under the pointer comes out a little
//!   farther, with a stronger shadow. A `+` sliver after the last note
//!   makes a new one.
//! - **Focused:** a straight sheet, grown to `focus-width` and up to
//!   `focus-height`, fully on screen, with the note rendered on another
//!   subsurface above it: laid out at the focused size from the start and
//!   cropped while the sheet grows, and scrolled with the wheel or keys.
//!   Sheets only come in a few sizes, so while one grows, the sheet at
//!   hand is stretched to it.
//!
//! Hovering a sliver for `reveal-delay` peeks; leaving the column for
//! `hide-delay` tucks back. A click focuses a note; Escape tucks it back.
//! The keyboard is `on-demand` while the pointer is on the column, so a
//! click gives it the keyboard, and is turned off when tucking back, which
//! gives it back to the window. A note focused from the command line takes
//! the keyboard (`exclusive`) until it is tucked back.
//!
//! Frames are only drawn while a spring moves: an idle column costs
//! nothing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
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
use wl_shell_render::doc::{self, Block, Doc, DocStyle, Hit, Kind, Run, Target};
use wl_shell_render::paper::{self, Paper};
use wl_shell_render::shadow::IconShadow;
use wl_shell_render::worker::Assets;
use wl_shell_ui::cache::shm_buffer;
use wl_shell_ui::geom::clip;
use wl_shell_ui::{IRect, Rect, Spring, Sub, cache, lerp};

use crate::app::WlNote;
use crate::config::{Config, Edge, NoteLayer};
use crate::deck::{Card, Deck, Item, LIFT, Metrics, Mode};
use crate::markdown::Content;
use crate::papers::{Papers, Sheet};

/// The text's distance from a sheet's sides, and from its top and bottom.
const PAD_X: f64 = 12.0;
const PAD_Y: f64 = 9.0;
/// A peeking sheet reaches this far behind the edge, so it still does
/// when lifted.
const BEHIND: f64 = LIFT + 10.0;
/// A focused note's distance from the edge and the column's ends.
const FOCUS_MARGIN: f64 = 14.0;
/// The most of a focused note that is rendered, logical pixels.
const DOC_MAX: i32 = 3000;
/// Focused notes kept rendered (the focused one, and the last hovered).
const DOCS: usize = 4;
/// A wheel click or an arrow key scrolls this far.
const LINE: f64 = 40.0;
/// Keys for the `+` sliver and the focused note's text in the stacking order.
const PLUS: &str = "\u{0}+";
const TEXT: &str = "\u{0}text";

/// What the column looks like and how it behaves, from the config.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub edge: Edge,
    pub layer: NoteLayer,
    pub metrics: Metrics,
    pub focus_width: f64,
    /// Of the column's height, at most.
    pub focus_height: f64,
    pub reveal_delay: Duration,
    pub hide_delay: Duration,
    pub response: f64,
    pub shadow: Option<IconShadow>,
    /// Lifted and focused notes'.
    pub lifted: Option<IconShadow>,
    pub font: String,
    pub font_size: f64,
    pub link: u32,
    /// The `+` sliver's paper and text.
    pub plus_paper: u32,
    pub plus_text: u32,
}

impl Style {
    pub fn new(config: &Config) -> Self {
        let theme = config.theme();
        let (_, plus_text) = theme.preset("white");
        Self {
            edge: config.edge,
            layer: config.layer,
            metrics: Metrics {
                tab: config.tab_height as f64,
                gap: config.tab_gap as f64,
                tuck: config.tuck as f64,
                peek: config.peek as f64,
                plus: 30.0,
            },
            focus_width: config.focus_width as f64,
            focus_height: config.focus_height,
            reveal_delay: Duration::from_millis(config.reveal_delay as u64),
            hide_delay: Duration::from_millis(config.hide_delay as u64),
            response: config.response as f64,
            shadow: config.shadow(),
            lifted: config.lifted_shadow(),
            font: config.font.clone(),
            font_size: config.font_size as f64,
            link: theme.link_color,
            plus_paper: config.paper_color(Some(&wl_shell_notes::Color::Preset("white"))),
            plus_text,
        }
    }

    /// Whether going from `self` to `new` takes a new surface.
    pub fn needs_new_surface(&self, new: &Style) -> bool {
        self.edge != new.edge || self.layer != new.layer
    }

    /// The surface's width: a focused note and its shadow.
    fn width(&self) -> u32 {
        let reach = (self.lifted.or(self.shadow)).map_or(0, |s| s.pad()) as f64;
        let widest = (self.focus_width + FOCUS_MARGIN).max(self.metrics.peek + LIFT);
        (widest + 2.0 * reach + 16.0).ceil() as u32
    }

    /// A sheet's width in the column: it reaches behind the edge.
    fn sheet_w(&self) -> f64 {
        self.metrics.peek + BEHIND
    }

    /// Where text starts on a sheet in the column: past what is behind
    /// the edge.
    fn content_x(&self) -> f64 {
        match self.edge {
            Edge::Right => PAD_X,
            Edge::Left => BEHIND + PAD_X,
        }
    }

    fn doc_style(&self, color: u32) -> DocStyle {
        DocStyle {
            font: self.font.clone(),
            size: self.font_size,
            color,
            link: self.link,
        }
    }
}

/// A note to show: its card, and what is on it.
pub struct Shown {
    pub item: Item,
    pub content: Rc<Content>,
    /// Its text color.
    pub text: u32,
}

/// What the column knows of a note shown.
struct NoteView {
    content: Rc<Content>,
    text: u32,
    preview: Vec<Block>,
}

/// What a pointer event or a key asks of the program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    None,
    /// Focus this note.
    Focus(String),
    /// Tuck the focused note back.
    Tuck,
    /// Toggle this note's n-th checkbox.
    Task(String, u32),
    /// Open this note's n-th link.
    Link(String, u32),
    /// Open this note in the editor.
    Edit(String),
    /// Make a new note.
    New,
}

/// The column's timers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timer {
    /// The pointer rested on a sliver: peek.
    Reveal,
    /// The pointer left the column: tuck back.
    Hide,
}

/// A card's subsurface (or the `+` sliver's), and the sheet it shows.
struct Slot {
    key: String,
    sub: Sub,
    /// The sheet shown, and the size it was drawn at: it may be shown
    /// stretched while the one wanted is drawn.
    shown: Option<(f64, f64, Sheet)>,
    /// Where it shows on the surface, if it does.
    visible: Option<Rect>,
    /// Where its sheet is (unclipped).
    sheet: Rect,
}

impl Slot {
    fn new(key: &str, sub: Sub) -> Self {
        Self {
            key: key.to_string(),
            sub,
            shown: None,
            visible: None,
            sheet: Rect::default(),
        }
    }
}

/// A focused note's rendering.
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
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    deck: Deck,
    notes: HashMap<String, NoteView>,
    slots: Vec<Slot>,
    plus: Slot,
    /// The focused note's text.
    text: Sub,
    /// The buffer `text` shows.
    text_shown: Option<WlBuffer>,
    docs: Vec<DocEntry>,
    next_tag: u64,
    /// Replaced documents' buffers, destroyed by `trim`.
    stale: Vec<WlBuffer>,
    /// The focused note's height.
    focus_h: Spring,
    /// How far its text is scrolled.
    scroll: f64,
    /// The subsurfaces' order last applied, bottom first.
    stacking: Vec<String>,
    /// The input region last set.
    input: Option<Vec<IRect>>,
    /// Where the pointer is on the surface, while it is.
    pointer: Option<(f64, f64)>,
    reveal: Option<RegistrationToken>,
    hide: Option<RegistrationToken>,
    keyboard: KeyboardInteractivity,
    frame_pending: bool,
    last_frame: Option<Instant>,
}

impl Column {
    /// An empty column on `output` (named `name`) at `scale`.
    pub fn new(
        core: &Core<WlNote>,
        output: WlOutput,
        name: String,
        scale: i32,
        style: Style,
        papers: Rc<RefCell<Papers>>,
        assets: Rc<Assets>,
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
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Nothing takes input until there are slivers.
        (layer.wl_surface()).set_input_region(Some(no_input.wl_region()));
        // Mapped on configure.
        layer.commit();
        let deck = Deck::new(style.metrics, style.response);
        let plus = Slot::new(PLUS, Sub::new(&g, layer.wl_surface(), &no_input, &qh));
        let text = Sub::new(&g, layer.wl_surface(), &no_input, &qh);
        let focus_h = Spring::new(style.metrics.tab, style.response);
        Ok(Self {
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
            width: 0,
            height: 0,
            scale: scale.max(1),
            configured: false,
            deck,
            notes: HashMap::new(),
            slots: Vec::new(),
            plus,
            text,
            text_shown: None,
            docs: Vec::new(),
            next_tag: 0,
            stale: Vec::new(),
            focus_h,
            scroll: 0.0,
            stacking: Vec::new(),
            input: None,
            pointer: None,
            reveal: None,
            hide: None,
            keyboard: KeyboardInteractivity::None,
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

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// The focused note, if any.
    pub fn focused(&self) -> Option<&str> {
        match self.deck.mode() {
            Mode::Focused(k) => Some(k),
            _ => None,
        }
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
        self.deck.set_metrics(style.metrics, style.response);
        self.focus_h = Spring::new(self.focus_h.value, style.response);
        self.style = style;
        self.refresh_docs();
        self.update_focus_h(true);
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
        self.update_focus_h(false);
        self.render();
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.refresh_docs();
        self.kick();
    }

    /// Show `notes`, animating the change. A focused note that isn't
    /// among them tucks back.
    pub fn set_notes(&mut self, core: &Core<WlNote>, notes: Vec<Shown>) {
        if let Some(k) = self.focused()
            && !notes.iter().any(|n| n.item.key == k)
        {
            self.tuck(core);
        }
        self.notes = (notes.iter())
            .map(|n| {
                let view = NoteView {
                    preview: n.content.preview(),
                    content: n.content.clone(),
                    text: n.text,
                };
                (n.item.key.clone(), view)
            })
            .collect();
        self.deck.set(notes.into_iter().map(|n| n.item).collect());
        // Forget the documents of notes gone; render the others again if
        // they changed.
        let gone: Vec<usize> = (0..self.docs.len())
            .filter(|i| !self.notes.contains_key(&self.docs[*i].key))
            .collect();
        for i in gone.into_iter().rev() {
            let e = self.docs.remove(i);
            self.stale.extend(e.shown.map(|s| s.buffer));
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
        (self.slots.iter().chain([&self.plus]))
            .filter_map(|s| s.shown.as_ref().map(|(_, _, s)| s.shape.buffer.clone()))
            .collect()
    }

    /// Destroy the documents' buffers no longer shown.
    pub fn trim(&mut self) {
        let shown = self.text_shown.clone();
        self.stale.retain(|b| {
            let keep = shown.as_ref() == Some(b);
            if !keep {
                b.destroy();
            }
            keep
        });
    }

    // -- what the notes do -------------------------------------------------

    /// Focus `key`: it comes forward, grown. With `keyboard`, it takes the
    /// keyboard (it wasn't clicked).
    pub fn focus(&mut self, core: &Core<WlNote>, key: &str, keyboard: bool) {
        self.cancel_timers(core);
        let again = self.focused() == Some(key);
        self.deck.set_hover(None);
        self.deck.set_mode(Mode::Focused(key.to_string()));
        if !again {
            self.scroll = 0.0;
            self.ensure_doc(key);
            self.update_focus_h(true);
        }
        if keyboard {
            self.set_keyboard(KeyboardInteractivity::Exclusive);
        }
        self.kick();
    }

    /// Tuck every note back, and give the keyboard back.
    pub fn tuck(&mut self, core: &Core<WlNote>) {
        self.cancel_timers(core);
        self.deck.set_hover(None);
        self.deck.set_mode(Mode::Tucked);
        self.set_keyboard(KeyboardInteractivity::None);
        self.kick();
    }

    /// Give the keyboard back that a click took, unless a note is focused.
    pub fn release_keyboard(&mut self) {
        if self.focused().is_none() {
            self.set_keyboard(KeyboardInteractivity::None);
        }
    }

    /// The keyboard went elsewhere, e.g. to a window clicked: a focused
    /// note tucks back.
    pub fn keyboard_leave(&mut self, core: &Core<WlNote>) {
        if self.focused().is_some() {
            self.tuck(core);
        }
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

    pub fn pointer(&mut self, core: &Core<WlNote>, event: &PointerEvent) -> Act {
        let (x, y) = event.position;
        match &event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                // A click on the column takes the keyboard.
                if self.keyboard == KeyboardInteractivity::None {
                    self.set_keyboard(KeyboardInteractivity::OnDemand);
                }
                self.hover_at(core, x, y);
                Act::None
            }
            PointerEventKind::Leave { .. } => {
                self.pointer = None;
                self.cancel(core, Timer::Reveal);
                if *self.deck.mode() == Mode::Peeking {
                    self.deck.set_hover(None);
                    self.start(core, Timer::Hide);
                }
                self.kick();
                Act::None
            }
            PointerEventKind::Press { button, .. } if *button == BTN_LEFT => self.click(x, y),
            PointerEventKind::Axis { vertical, .. } => {
                if self.on_focused(x, y) {
                    self.scroll_by(axis(vertical));
                }
                Act::None
            }
            _ => Act::None,
        }
    }

    /// A key while the column has the keyboard.
    pub fn key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Act {
        let Some(key) = self.focused().map(String::from) else {
            return match event.keysym {
                Keysym::Escape => Act::Tuck,
                _ => Act::None,
            };
        };
        let page = (self.focus_h.target - 2.0 * PAD_Y) * 0.9;
        match event.keysym {
            Keysym::Escape => return Act::Tuck,
            Keysym::Return | Keysym::KP_Enter if mods.ctrl => return Act::Tuck,
            Keysym::e | Keysym::E if mods.ctrl => return Act::Edit(key),
            Keysym::Up => self.scroll_by(-LINE),
            Keysym::Down => self.scroll_by(LINE),
            Keysym::Page_Up => self.scroll_by(-page),
            Keysym::Page_Down | Keysym::space => self.scroll_by(page),
            Keysym::Home => self.scroll_by(-f64::INFINITY),
            Keysym::End => self.scroll_by(f64::INFINITY),
            _ => {}
        }
        Act::None
    }

    /// One of the column's timers fired.
    pub fn timer_due(&mut self, core: &Core<WlNote>, which: Timer) {
        match which {
            Timer::Reveal => {
                self.reveal = None;
                if *self.deck.mode() == Mode::Tucked
                    && let Some((x, y)) = self.pointer
                    && self.card_at(x, y).is_some()
                {
                    self.deck.set_mode(Mode::Peeking);
                    self.hover_at(core, x, y);
                }
            }
            Timer::Hide => {
                self.hide = None;
                if *self.deck.mode() == Mode::Peeking && self.pointer.is_none() {
                    self.tuck(core);
                }
            }
        }
        self.kick();
    }

    fn start(&mut self, core: &Core<WlNote>, which: Timer) {
        let (running, delay) = match which {
            Timer::Reveal => (self.reveal.is_some(), self.style.reveal_delay),
            Timer::Hide => (self.hide.is_some(), self.style.hide_delay),
        };
        if running {
            return;
        }
        if delay.is_zero() {
            return self.timer_due(core, which);
        }
        let timer = core.timer(delay, move |app: &mut WlNote, core| {
            app.column_timer(core, which);
        });
        match (timer, which) {
            (Ok(token), Timer::Reveal) => self.reveal = Some(token),
            (Ok(token), Timer::Hide) => self.hide = Some(token),
            (Err(e), _) => warn!("{e}"),
        }
    }

    fn cancel(&mut self, core: &Core<WlNote>, which: Timer) {
        let slot = match which {
            Timer::Reveal => &mut self.reveal,
            Timer::Hide => &mut self.hide,
        };
        if let Some(token) = slot.take() {
            core.loop_handle.remove(token);
        }
    }

    fn cancel_timers(&mut self, core: &Core<WlNote>) {
        self.cancel(core, Timer::Reveal);
        self.cancel(core, Timer::Hide);
    }

    /// The pointer is at (`x`, `y`): peek once it rests on a sliver, and
    /// lift the note under it while peeking.
    fn hover_at(&mut self, core: &Core<WlNote>, x: f64, y: f64) {
        self.pointer = Some((x, y));
        let card = self.card_at(x, y);
        match self.deck.mode() {
            Mode::Tucked => match card {
                Some(_) => self.start(core, Timer::Reveal),
                None => self.cancel(core, Timer::Reveal),
            },
            Mode::Peeking => {
                self.cancel(core, Timer::Hide);
                if card.as_deref() != self.deck.hover() {
                    if let Some(k) = &card {
                        // Likely to be clicked next.
                        self.ensure_doc(k);
                    }
                    self.deck.set_hover(card);
                    self.kick();
                }
            }
            Mode::Focused(_) => {}
        }
    }

    fn click(&mut self, x: f64, y: f64) -> Act {
        if self.plus_at(x, y) {
            return Act::New;
        }
        let Some(key) = self.card_at(x, y) else {
            return match self.focused() {
                Some(_) => Act::Tuck,
                None => Act::None,
            };
        };
        match self.deck.mode() {
            Mode::Focused(f) if *f == key => match self.doc_target(&key, x, y) {
                Some(Target::Task(n)) => Act::Task(key, n),
                Some(Target::Link(n)) => Act::Link(key, n),
                None => Act::None,
            },
            // Checkboxes work while peeking too; the rest focuses.
            Mode::Peeking => match self.preview_target(&key, x, y) {
                Some(Target::Task(n)) => Act::Task(key, n),
                _ => Act::Focus(key),
            },
            _ => Act::Focus(key),
        }
    }

    /// The note shown at (`x`, `y`), topmost first.
    fn card_at(&self, x: f64, y: f64) -> Option<String> {
        (self.stacking.iter().rev())
            .filter_map(|k| self.slots.iter().find(|s| s.key == *k))
            .find(|s| s.visible.is_some_and(|r| r.contains(x, y)))
            .map(|s| s.key.clone())
    }

    fn plus_at(&self, x: f64, y: f64) -> bool {
        self.deck.plus.value > 0.5 && self.plus.visible.is_some_and(|r| r.contains(x, y))
    }

    fn on_focused(&self, x: f64, y: f64) -> bool {
        self.focused().is_some() && self.card_at(x, y).as_deref() == self.focused()
    }

    /// What of the preview of `key` is at (`x`, `y`).
    fn preview_target(&self, key: &str, x: f64, y: f64) -> Option<Target> {
        let slot = self.slots.iter().find(|s| s.key == key)?;
        let (pw, ph, sheet) = slot.shown.as_ref()?;
        let r = slot.sheet;
        let (sx, sy) = ((x - r.x) * pw / r.w, (y - r.y) * ph / r.h);
        doc::hit(&sheet.hits, sx, sy)
    }

    /// What of the focused note's text is at (`x`, `y`).
    fn doc_target(&self, key: &str, x: f64, y: f64) -> Option<Target> {
        let slot = self.slots.iter().find(|s| s.key == key)?;
        let d = self.docs.iter().find(|d| d.key == key)?.shown.as_ref()?;
        let r = slot.sheet;
        if !text_area(r).contains(x, y) {
            return None;
        }
        doc::hit(&d.hits, x - r.x - PAD_X, y - r.y - PAD_Y + self.scroll)
    }

    fn scroll_by(&mut self, d: f64) {
        let max = self.max_scroll();
        let scroll = (self.scroll + d).clamp(0.0, max);
        if scroll != self.scroll {
            self.scroll = scroll;
            self.kick();
        }
    }

    /// How far the focused note's text scrolls.
    fn max_scroll(&self) -> f64 {
        let Some(key) = self.focused() else {
            return 0.0;
        };
        let h = (self.docs.iter().find(|d| d.key == key))
            .and_then(|d| d.shown.as_ref())
            .map_or(0.0, |d| d.h);
        (h - (self.focus_h.target - 2.0 * PAD_Y)).max(0.0)
    }

    // -- documents ---------------------------------------------------------

    /// What a focused `key` renders.
    fn want_doc(&self, key: &str) -> Option<Doc> {
        let n = self.notes.get(key)?;
        Some(Doc {
            blocks: n.content.blocks.clone(),
            style: self.style.doc_style(n.text),
            width: (self.style.focus_width - 2.0 * PAD_X) as i32,
            max_height: DOC_MAX,
            scale: self.scale,
        })
    }

    /// Have the focused rendering of `key` drawn, unless it is already.
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

    /// The notes or the scale changed: the focused note may need drawing
    /// again.
    fn refresh_docs(&mut self) {
        let keys: Vec<String> = (self.focused().into_iter())
            .chain(self.deck.hover())
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
        let key = e.key.clone();
        if self.focused() == Some(&key) {
            self.update_focus_h(false);
        }
        self.kick();
    }

    /// The focused note's height: its text's, up to `focus-height` of the
    /// column. Until its text is drawn, a guess.
    fn update_focus_h(&mut self, snap: bool) {
        let Some(key) = self.focused() else {
            return;
        };
        let m = &self.style.metrics;
        let max = (self.height as f64 * self.style.focus_height)
            .min(self.height as f64 - 2.0 * FOCUS_MARGIN)
            .max(m.tab);
        let text = (self.docs.iter().find(|d| d.key == key)).and_then(|d| d.shown.as_ref());
        let h = match text {
            Some(d) => (d.h + 2.0 * PAD_Y).clamp(m.tab, max),
            None => (m.tab * 3.0).min(max),
        };
        match snap {
            true => self.focus_h.snap(h),
            false => self.focus_h.retarget(h),
        }
        self.scroll = self.scroll.min(self.max_scroll());
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
        let deck = self.deck.advance(dt);
        let grow = self.focus_h.advance(dt);
        self.last_frame = (deck || grow).then_some(now);
        self.render();
    }

    fn moving(&self) -> bool {
        self.deck.moving() || !self.focus_h.settled()
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
    }

    // -- the scene ---------------------------------------------------------

    fn surface_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.width as f64, self.height as f64)
    }

    fn focusing(card: &Card) -> bool {
        card.focus.target > 0.0 || card.focus.value > 0.0
    }

    /// Whether a card shows its face (peeking or lifted) rather than its
    /// edge.
    fn peeking(&self, card: &Card) -> bool {
        let tuck = self.style.metrics.tuck;
        card.out.target > tuck || card.out.value > tuck + 0.5
    }

    fn lifted(&self, card: &Card) -> bool {
        *self.deck.mode() == Mode::Peeking && self.deck.hover() == Some(card.key.as_str())
    }

    /// The paper a card is drawn with.
    fn paper(&self, card: &Card) -> Paper {
        let m = &self.style.metrics;
        let base = Paper {
            w: self.style.sheet_w() as i32,
            h: m.tab as i32,
            color: card.look.color,
            edge: self.style.edge.into(),
            tilt: card.look.tilt,
            sliver: true,
            shadow: self.style.shadow,
            scale: self.scale,
            content: None,
        };
        if Self::focusing(card) {
            return Paper {
                w: self.style.focus_width as i32,
                h: self.focus_h.target.round() as i32,
                tilt: 0.0,
                sliver: false,
                shadow: self.style.lifted,
                ..base
            };
        }
        if !self.peeking(card) {
            return base;
        }
        let content = self.notes.get(&card.key).map(|n| paper::Content {
            doc: Doc {
                blocks: n.preview.clone(),
                style: self.style.doc_style(n.text),
                width: (m.peek - 2.0 * PAD_X) as i32,
                max_height: (m.tab - 2.0 * PAD_Y) as i32,
                scale: self.scale,
            },
            x: self.style.content_x() as i32,
            y: PAD_Y as i32,
        });
        Paper {
            sliver: false,
            shadow: match self.lifted(card) {
                true => self.style.lifted,
                false => self.style.shadow,
            },
            content,
            ..base
        }
    }

    /// The `+` sliver's paper.
    fn plus_paper(&self) -> Paper {
        let m = &self.style.metrics;
        let run = Run {
            bold: true,
            ..Run::plain("+  New note")
        };
        Paper {
            w: self.style.sheet_w() as i32,
            h: m.plus as i32,
            color: self.style.plus_paper,
            edge: self.style.edge.into(),
            tilt: 0.0,
            sliver: false,
            shadow: self.style.shadow,
            scale: self.scale,
            content: Some(paper::Content {
                doc: Doc {
                    blocks: vec![Block::new(Kind::Paragraph, vec![run])],
                    style: self.style.doc_style(self.style.plus_text),
                    width: (m.peek - 2.0 * PAD_X) as i32,
                    max_height: m.plus as i32,
                    scale: self.scale,
                },
                x: self.style.content_x() as i32,
                y: ((m.plus - doc_line(self.style.font_size)) / 2.0).max(0.0) as i32,
            }),
        }
    }

    /// Where a sheet `out` of the edge (`presence` 0: out of sight, its
    /// shadow `reach` too) is, at `top`, `h` long.
    fn column_rect(&self, out: f64, presence: f64, reach: f64, top: f64, h: f64) -> Rect {
        let shown = lerp(-reach, out, presence);
        let w = self.style.sheet_w();
        let x = match self.style.edge {
            Edge::Right => self.width as f64 - shown,
            Edge::Left => shown - w,
        };
        Rect::new(x, top, w, h)
    }

    /// Where a focused card is: fully on screen, next to its place in the
    /// column.
    fn focused_rect(&self, card: &Card) -> Rect {
        let w = self.style.focus_width;
        let h = self.focus_h.value;
        let x = match self.style.edge {
            Edge::Right => self.width as f64 - FOCUS_MARGIN - w,
            Edge::Left => FOCUS_MARGIN,
        };
        let center = card.y.value + self.style.metrics.tab / 2.0;
        let bottom = (self.height as f64 - FOCUS_MARGIN - h).max(FOCUS_MARGIN);
        let y = (center - h / 2.0).clamp(FOCUS_MARGIN, bottom);
        Rect::new(x, y, w, h)
    }

    /// Where a card's sheet is.
    fn sheet_rect(&self, card: &Card, reach: f64) -> Rect {
        let tab = self.style.metrics.tab;
        let r = self.column_rect(card.out.value, card.presence.value, reach, card.y.value, tab);
        match card.focus.value > 0.0 {
            true => r.lerp(self.focused_rect(card), card.focus.value),
            false => r,
        }
    }

    fn place_all(&mut self) {
        let surface = self.surface_rect();
        let scale = self.scale as f64;
        // One slot per card, in the cards' order.
        let cards: Vec<Card> = self.deck.cards().to_vec();
        self.slots.retain(|s| cards.iter().any(|c| c.key == s.key));
        for card in &cards {
            if !self.slots.iter().any(|s| s.key == card.key) {
                let sub = Sub::new(&self.g, self.layer.wl_surface(), &self.no_input, &self.qh);
                self.slots.push(Slot::new(&card.key, sub));
            }
            let want = self.paper(card);
            let ready = self.papers.borrow_mut().get(&want);
            // Have the face ready before peeking.
            if !self.peeking(card) && !Self::focusing(card) {
                let mut face = card.clone();
                face.out.target = self.style.metrics.peek;
                let face = self.paper(&face);
                self.papers.borrow_mut().get(&face);
            }
            let Some(i) = self.slots.iter().position(|s| s.key == card.key) else {
                continue;
            };
            if let Some(sheet) = ready {
                self.slots[i].shown = Some((want.w as f64, want.h as f64, sheet));
            }
            let reach = (self.slots[i].shown.as_ref()).map_or(0.0, |(_, _, s)| -s.shape.rect.x as f64);
            let rect = self.sheet_rect(card, reach);
            let slot = &mut self.slots[i];
            slot.sheet = rect;
            let visible = card.presence.value > 0.0;
            slot.visible = present(slot, rect, surface, scale, visible, 1.0);
            if !card.present() {
                slot.visible = None;
            }
        }

        // The + sliver.
        let want = self.plus_paper();
        if let Some(sheet) = self.papers.borrow_mut().get(&want) {
            self.plus.shown = Some((want.w as f64, want.h as f64, sheet));
        }
        let m = self.style.metrics;
        let reach = (self.plus.shown.as_ref()).map_or(0.0, |(_, _, s)| -s.shape.rect.x as f64);
        let rect = self.column_rect(m.peek, self.deck.plus.value, reach, self.deck.plus_top(), m.plus);
        self.plus.sheet = rect;
        let plus = self.deck.plus.value;
        self.plus.visible = present(&mut self.plus, rect, surface, scale, plus > 0.0, 1.0);

        // The focused note's text: the most focused card's.
        let focused = (cards.iter())
            .filter(|c| c.focus.value > 0.0)
            .max_by(|a, b| a.focus.value.total_cmp(&b.focus.value));
        let text = focused.and_then(|c| {
            let d = self.docs.iter().find(|d| d.key == c.key)?.shown.as_ref()?;
            let slot = self.slots.iter().find(|s| s.key == c.key)?;
            Some((d, slot.sheet, c.focus.value))
        });
        let mut shown_text = None;
        match text {
            Some((d, r, focus)) => {
                let area = text_area(r);
                let image = Rect::new(r.x + PAD_X, r.y + PAD_Y - self.scroll, d.w, d.h);
                let area = intersect(area, surface);
                match area.and_then(|a| clip(image, (d.w * scale, d.h * scale), None, a)) {
                    Some((rect, src)) => {
                        self.text.show(&d.buffer);
                        self.text.crop(src);
                        self.text.place(IRect::round(rect));
                        self.text.set_alpha(focus * focus);
                        self.text.commit();
                        shown_text = Some(d.buffer.clone());
                    }
                    None => self.text.clear(),
                }
            }
            None => self.text.clear(),
        }
        self.text_shown = shown_text;

        self.restack(&cards);

        // Input: what shows of the notes.
        let mut input: Vec<IRect> = (self.slots.iter())
            .filter_map(|s| s.visible)
            .map(IRect::round)
            .collect();
        if plus > 0.5
            && let Some(r) = self.plus.visible
        {
            input.push(IRect::round(r));
        }
        if self.input.as_ref() != Some(&input) {
            self.set_input(&input);
            self.input = Some(input);
        }
    }

    /// Stack the subsurfaces: the `+` sliver at the bottom, then the notes
    /// in order, the lifted one above them, the focused one (and its
    /// text) on top.
    fn restack(&mut self, cards: &[Card]) {
        let hover = self.deck.hover();
        let rank = |c: &Card| match () {
            _ if c.focus.value > 0.0 || c.focus.target > 0.0 => 2,
            _ if Some(c.key.as_str()) == hover => 1,
            _ => 0,
        };
        let mut order: Vec<&Card> = cards.iter().collect();
        // Stable: the column's order within each rank.
        order.sort_by_key(|c| rank(c));
        let keys: Vec<String> = std::iter::once(PLUS.to_string())
            .chain(order.iter().map(|c| c.key.clone()))
            .chain(std::iter::once(TEXT.to_string()))
            .collect();
        if keys == self.stacking {
            return;
        }
        let surface = |k: &str| -> Option<&Sub> {
            match k {
                PLUS => Some(&self.plus.sub),
                TEXT => Some(&self.text),
                _ => self.slots.iter().find(|s| s.key == k).map(|s| &s.sub),
            }
        };
        for pair in keys.windows(2) {
            if let (Some(below), Some(above)) = (surface(&pair[0]), surface(&pair[1])) {
                above.place_above(below.surface());
            }
        }
        self.stacking = keys;
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

/// Show a slot's sheet at `sheet` (stretched to it if drawn at another
/// size), cropped to `surface`. Returns where it shows.
fn present(
    slot: &mut Slot,
    sheet: Rect,
    surface: Rect,
    scale: f64,
    visible: bool,
    alpha: f64,
) -> Option<Rect> {
    let Some((pw, ph, s)) = &slot.shown else {
        slot.sub.clear();
        return None;
    };
    let (sx, sy) = (sheet.w / pw, sheet.h / ph);
    let r = s.shape.rect;
    let image = Rect::new(
        sheet.x + r.x as f64 * sx,
        sheet.y + r.y as f64 * sy,
        r.w as f64 * sx,
        r.h as f64 * sy,
    );
    let size = (r.w as f64 * scale, r.h as f64 * scale);
    match clip(image, size, None, surface) {
        Some((rect, src)) if visible => {
            slot.sub.show(&s.shape.buffer);
            slot.sub.crop(src);
            slot.sub.place(IRect::round(rect));
            slot.sub.set_alpha(alpha);
            slot.sub.commit();
            intersect(sheet, surface)
        }
        _ => {
            slot.sub.clear();
            None
        }
    }
}

/// Where a focused sheet's text shows.
fn text_area(r: Rect) -> Rect {
    Rect::new(
        r.x + PAD_X,
        r.y + PAD_Y,
        (r.w - 2.0 * PAD_X).max(0.0),
        (r.h - 2.0 * PAD_Y).max(0.0),
    )
}

fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let (x0, y0) = (a.x.max(b.x), a.y.max(b.y));
    let (x1, y1) = (a.right().min(b.right()), a.bottom().min(b.bottom()));
    (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
}

/// A line of text's height, logical pixels.
fn doc_line(font_size: f64) -> f64 {
    wl_shell_render::text::line_height(font_size) as f64
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
        assert_eq!(
            text_area(Rect::new(0.0, 0.0, 100.0, 50.0)),
            Rect::new(PAD_X, PAD_Y, 100.0 - 2.0 * PAD_X, 50.0 - 2.0 * PAD_Y)
        );
    }
}

