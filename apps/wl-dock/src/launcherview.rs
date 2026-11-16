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
//! Files (the query's scope is files once the trigger was typed): the bar
//! holds two lines. The first is the query and the selected file's name:
//! inline after the caret if the query is the start of it, else after an
//! arrow, with what the query matched brighter. The second, smaller and
//! dimmer, is the file's full path (`~` for home) with what the query
//! matched brighter and its name brighter than its directories;
//! directories the query didn't match are elided to fit. While nothing
//! is selected, it shows the search's progress. With an empty file query,
//! the front is a folder icon and the first line is a placeholder. A
//! file's icon is its thumbnail or its type's (looked up by the worker);
//! a generic icon stands in until it arrives. Its pill on the drum has
//! its name, then its parent directory's dimmed. Results stream in
//! (`stream_matches`): until the drum is rolled the best is in front;
//! after, the selection stays in front while it is among them.
//!
//! Texts (the query, the completion, the name) are lines from the worker;
//! a line shows its last text until the new one arrives. Icons come from
//! the shared cache.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;
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
use wl_shell_render::{files, shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::shapes::{self, Axis, Shape, ShapeKey};
use wl_shell_ui::{IRect, Rect, Spring, Sub};

use crate::app::WlDock;
use crate::config::{Config, DockLayout};
use crate::drum::{self, Drum};
use crate::filesearch;
use crate::launcher::{Completion, Query, Scope, completion};
use crate::view::Handover;

type Qh = QueueHandle<Shell<WlDock>>;

/// The icon cross-fade when a slot's match changes, ms.
const FADE_RESPONSE: f64 = 120.0;
/// Shown while nothing was typed.
const PLACEHOLDER: &str = "Search apps";
const FILES_PLACEHOLDER: &str = "Search files";
/// The front's icon with an empty query.
const SEARCH_ICON: &str = "system-search";
/// Stand-ins for a file's icon, and a folder's.
const FILE_ICONS: [&str; 3] = ["text-x-generic", "application-x-generic", "unknown"];
/// Before a file's name that doesn't continue the query.
const NAME_MARK: &str = "→";
/// A character's width, of the font size, at most (to fit paths).
const CHAR_WIDTH: f64 = 0.55;
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

    /// The file path's font size under the query: the drum labels'.
    fn path_size(&self) -> f64 {
        (self.font_size * LABEL_FONT).round()
    }

    /// The file path under the query: bare, in the drum labels' size.
    fn path_style(&self, color: u32) -> text::Style {
        text::Style {
            font: self.font.clone(),
            size: self.path_size(),
            color,
            background: 0,
        }
    }
}

/// A match as the launcher shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Match {
    /// The app's desktop ID (without `.desktop`), or the file's path
    /// relative to the search's root.
    pub id: String,
    /// The app's name, or the file's.
    pub name: String,
    /// Its `Icon=`.
    pub icon: Option<String>,
    /// The parts of the name the query matched, as byte ranges.
    pub matched: Vec<Range<usize>>,
    /// What a file match has besides.
    pub file: Option<FileMatch>,
}

/// A file match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    pub path: PathBuf,
    pub dir: bool,
    /// The parent directory's name (empty at the root), and the parts the
    /// query matched in it.
    pub parent: String,
    pub parent_matched: Vec<Range<usize>>,
    /// Its full path for showing (`~` for home), and the parts the query
    /// matched in it.
    pub shown: String,
    pub shown_matched: Vec<Range<usize>>,
}

/// The file search, as the bar shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesStatus {
    /// Under the query while nothing is selected: how far the search got.
    pub progress: Option<String>,
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

/// An icon on the drum: an app's, or one by name (a file's).
#[derive(Clone, Debug, PartialEq, Eq)]
enum IconKey {
    App(icons::Key),
    Named(NamedIcon),
}

impl IconKey {
    /// Its buffer, if it is there.
    fn buffer(&self, cache: &Cache) -> Option<WlBuffer> {
        match self {
            IconKey::App(k) => cache.icon(k),
            IconKey::Named(k) => cache.named_icon(k).flatten(),
        }
    }

    /// How far the image reaches past the icon, of its size.
    fn pad(&self) -> f64 {
        let (pad, size, scale) = match self {
            IconKey::App(k) => (k.pad(), k.size, k.scale),
            IconKey::Named(k) => (k.pad(), k.size, k.scale),
        };
        pad as f64 / (size as f64 / scale.max(1) as f64)
    }

    /// Have the worker render it, unless it is cached.
    fn request(&self, cache: &Cache, assets: &Assets, font: &str) {
        match self {
            IconKey::App(k) if cache.icon(k).is_none() => assets.icon(k, font),
            IconKey::Named(k) if cache.named_icon(k).is_none() => assets.named_icon(k),
            _ => {}
        }
    }
}

/// A match on the drum: its icon, and the one it replaced while that
/// fades out.
struct Ringed {
    m: Match,
    key: IconKey,
    /// Shown while `key` isn't there (a file's generic icon).
    standin: Option<IconKey>,
    old: Option<IconKey>,
    /// 0 (the old icon) to 1 (this one).
    fade: Spring,
}

impl Ringed {
    /// The icon to draw, and its pad of its size.
    fn icon(&self, cache: &Cache) -> Option<(WlBuffer, f64)> {
        let shown = |k: &IconKey| k.buffer(cache).map(|b| (b, k.pad()));
        shown(&self.key).or_else(|| self.standin.as_ref().and_then(shown))
    }
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
    key: IconKey,
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
    /// A file search's second line: the selected file's path.
    path_line: Line,
    inline: Line,
    /// The inline text stands apart from the query (a file's name that
    /// doesn't continue it).
    inline_apart: bool,
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
    /// Rolled since the matches were last ranked anew.
    rolled: bool,
    /// Scrolling not yet rolled, in steps.
    scrolled: f64,
    files: FilesStatus,
    /// Files' icons from the worker (thumbnails or type icons, best
    /// first), and the files asked for.
    file_icons: HashMap<PathBuf, Vec<String>>,
    asked_icons: HashSet<PathBuf>,
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
        let path_line = Line::new(sub());
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
            path_line,
            inline,
            inline_apart: false,
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
            rolled: false,
            scrolled: 0.0,
            files: FilesStatus::default(),
            file_icons: HashMap::new(),
            asked_icons: HashSet::new(),
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
        let attached = self.attached.as_ref();
        for r in &mut self.ring {
            (r.key, r.standin) = Self::icon_keys(attached, &r.m, scale, &self.file_icons);
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
        for scope in [Scope::Apps, Scope::Files] {
            self.search_key(scope)
                .inspect(|k| self.assets.named_icon(k));
        }
    }

    /// The icon `names` (the first found) at the front's size.
    fn front_named(a: &Attached, scale: i32, names: Vec<String>) -> Option<NamedIcon> {
        let mut names = names.into_iter();
        Some(NamedIcon {
            name: names.next()?,
            fallbacks: names.collect(),
            size: a.style.front().ceil() as u32 * scale as u32,
            theme: a.style.icon_theme.clone(),
            shadow: a.style.icon_shadow,
            scale,
        })
    }

    /// The icon of `m`, and what stands in for it until it is there: a
    /// file's thumbnail or type icon (once `file_icons` has it), else a
    /// generic one.
    fn icon_keys(
        attached: Option<&Attached>,
        m: &Match,
        scale: i32,
        file_icons: &HashMap<PathBuf, Vec<String>>,
    ) -> (IconKey, Option<IconKey>) {
        let (size, theme, shadow) = match attached {
            Some(a) => (
                a.style.front(),
                a.style.icon_theme.clone(),
                a.style.icon_shadow,
            ),
            None => (60.0, None, None),
        };
        let app = || {
            let key = icons::Key::new(&m.id, size.ceil() as u32, theme.clone(), shadow, scale);
            IconKey::App(key.with_icon(m.icon.clone()))
        };
        let (Some(f), Some(a)) = (&m.file, attached) else {
            return (app(), None);
        };
        let generic = match f.dir {
            true => &files::FOLDER_ICONS[..],
            false => &FILE_ICONS[..],
        };
        let generic = generic.iter().map(|s| s.to_string()).collect();
        let standin = Self::front_named(a, scale, generic).map(IconKey::Named);
        let own = (file_icons.get(&f.path).cloned())
            .and_then(|names| Self::front_named(a, scale, names))
            .map(IconKey::Named);
        match (own, standin) {
            (Some(own), standin) => (own, standin),
            (None, Some(standin)) => (standin, None),
            (None, None) => (app(), None),
        }
    }

    /// The front's icon while there is no match: a search icon, or a
    /// folder for files.
    fn search_key(&self, scope: Scope) -> Option<NamedIcon> {
        let a = self.attached.as_ref()?;
        let names: &[&str] = match scope {
            Scope::Apps => &[SEARCH_ICON, "edit-find", "search"],
            Scope::Files => &files::FOLDER_ICONS,
        };
        let names = names.iter().map(|s| s.to_string()).collect();
        Self::front_named(a, self.scale, names)
    }

    /// Have the worker render the ring's icons, and look up those of its
    /// files.
    fn request_icons(&mut self) {
        let cache = self.cache.borrow();
        let font = self
            .attached
            .as_ref()
            .map_or("sans-serif", |a| a.style.font.as_str());
        for r in &self.ring {
            r.key.request(&cache, &self.assets, font);
            if let Some(standin) = &r.standin {
                standin.request(&cache, &self.assets, font);
            }
            if let Some(f) = &r.m.file
                && !self.file_icons.contains_key(&f.path)
                && self.asked_icons.insert(f.path.clone())
            {
                self.assets.file_icons(&f.path, f.dir);
            }
        }
    }

    /// A file's icons arrived (thumbnail or type icons, best first).
    pub fn file_icons_ready(&mut self, path: PathBuf, names: Vec<String>) {
        self.asked_icons.remove(&path);
        let mut changed = false;
        self.file_icons.insert(path.clone(), names);
        let attached = self.attached.as_ref();
        for r in &mut self.ring {
            if r.m.file.as_ref().is_some_and(|f| f.path == path) {
                (r.key, r.standin) = Self::icon_keys(attached, &r.m, self.scale, &self.file_icons);
                changed = true;
            }
        }
        if changed {
            self.request_icons();
            self.kick();
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
        self.rolled = false;
        self.replace(matches, 0);
    }

    /// Matches streamed in, best first: like `set_matches`, but once the
    /// drum was rolled, the selection stays in front as long as it is
    /// among them. Nothing happens if they are the ones shown.
    pub fn stream_matches(&mut self, matches: Vec<Match>) {
        let same = matches.len() == self.ring.len()
            && matches.iter().zip(&self.ring).all(|(m, r)| *m == r.m);
        if same {
            return;
        }
        let kept = (self.kept().map(str::to_string))
            .and_then(|id| matches.iter().position(|m| m.id == id));
        match kept {
            Some(front) => self.replace(matches, front),
            None => self.set_matches(matches),
        }
    }

    /// Forget the roll: the next matches put their best in front.
    pub fn unroll(&mut self) {
        self.rolled = false;
    }

    /// The ID of the selection the user rolled to, if they did.
    pub fn kept(&self) -> Option<&str> {
        let i = self.selected().filter(|_| self.rolled)?;
        Some(&self.ring.get(i)?.m.id)
    }

    /// Put `matches` on the drum with the one at `front` in front; an
    /// unfinished roll carries on. Each slot whose match changed
    /// cross-fades its icon.
    fn replace(&mut self, matches: Vec<Match>, front: usize) {
        let slots = self.attached.as_ref().map_or(1, |a| a.style.matches);
        // What each slot shows now: slot `k` holds the match `k` past the
        // one in front.
        let old_front = self.selected().unwrap_or(0);
        let n_old = self.ring.len();
        let old: Vec<IconKey> = (0..n_old)
            .map(|k| self.ring[(k + old_front) % n_old].key.clone())
            .collect();
        let matches: Vec<Match> = matches.into_iter().take(slots).collect();
        let n = matches.len();
        let front = front.min(n.saturating_sub(1));
        let attached = self.attached.as_ref();
        let ring: Vec<Ringed> = (matches.into_iter().enumerate())
            .map(|(i, m)| {
                let (key, standin) = Self::icon_keys(attached, &m, self.scale, &self.file_icons);
                let prev = old.get((i + n - front) % n).cloned();
                let same = prev.as_ref() == Some(&key);
                let mut fade = Spring::new(if same { 1.0 } else { 0.0 }, FADE_RESPONSE);
                fade.retarget(1.0);
                Ringed {
                    m,
                    key,
                    standin,
                    old: prev.filter(|_| !same),
                    fade,
                }
            })
            .collect();
        self.ring = ring;
        match front {
            0 if !self.rolled => self.roll.snap(0.0),
            _ => {
                let unfinished = self.roll.value - self.roll.target;
                self.roll.target = front as f64;
                self.roll.value = front as f64 + unfinished;
            }
        }
        self.searching
            .retarget(if self.ring.is_empty() { 1.0 } else { 0.0 });
        self.request_icons();
        self.sync_texts();
        self.kick();
    }

    /// How the file search is doing.
    pub fn set_files_status(&mut self, status: FilesStatus) {
        if status != self.files {
            self.files = status;
            self.sync_texts();
            self.kick();
        }
    }

    /// Roll the drum `by` steps (positive: the next match comes to the
    /// front).
    pub fn roll(&mut self, by: i32) {
        if self.ring.len() < 2 || self.closing {
            return;
        }
        let target = self.roll.target + by as f64;
        self.roll.retarget(target);
        self.rolled = true;
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
        let spanned = |text: String, spans: Vec<(Range<usize>, u32)>, color: u32| PillRequest {
            text,
            spans,
            max_width: max,
            scale,
            style: style.line_style(color),
        };
        let line = |text: String, color: u32| spanned(text, Vec::new(), color);
        // The names on the drum, with what the query matched brighter.
        let labels: Vec<PillRequest> = (self.ring.iter())
            .map(|r| label_request(&r.m, style, max, scale))
            .collect();
        let selected = self.selected().and_then(|i| self.ring.get(i)).map(|r| &r.m);
        let q = self.query.text();
        // Zero-width spaces keep the query's own spaces at its ends.
        let query = (!q.is_empty()).then(|| line(format!("\u{200b}{q}\u{200b}"), style.text));
        let (path, inline, after, apart) = match self.query.scope() {
            Scope::Files => {
                let file = selected.and_then(|m| Some((m, m.file.as_ref()?)));
                // Under the query: the selection's path, else the progress.
                let path = match file {
                    Some((_, f)) => Some(path_request(f, style, max, scale)),
                    None => (self.files.progress.clone()).map(|p| PillRequest {
                        text: p,
                        spans: Vec::new(),
                        max_width: max,
                        scale,
                        style: style.path_style(dimmer(style.completion)),
                    }),
                };
                let name = file.map(|(m, _)| m.name.as_str());
                let (inline, apart) = match (completion(q, name), file) {
                    (Completion::None, _) if q.is_empty() => {
                        (Some(line(FILES_PLACEHOLDER.into(), style.completion)), false)
                    }
                    (Completion::Inline(rest), _) if !rest.is_empty() => {
                        (Some(line(rest, style.completion)), false)
                    }
                    // Not the query's continuation: apart, after an arrow,
                    // with what the query matched brighter.
                    (Completion::After(name), Some((m, _))) => {
                        let at = NAME_MARK.len() + 1;
                        let mut spans = vec![(0..NAME_MARK.len(), dimmer(style.completion))];
                        spans.extend(m.matched.iter().map(|r| (r.start + at..r.end + at, style.text)));
                        let text = format!("{NAME_MARK} {name}");
                        (Some(spanned(text, spans, style.completion)), true)
                    }
                    _ => (None, false),
                };
                (path, inline, None, apart)
            }
            Scope::Apps => {
                let name = selected.map(|m| m.name.as_str());
                let (inline, after) = match completion(q, name) {
                    Completion::None if q.is_empty() => (Some(PLACEHOLDER.to_string()), None),
                    Completion::None => (None, None),
                    Completion::Inline(rest) if rest.is_empty() => (None, None),
                    Completion::Inline(rest) => (Some(rest), None),
                    Completion::After(name) => (None, Some(name)),
                };
                let inline = inline.map(|t| line(t, style.completion));
                let after = after.map(|t| line(t, style.completion));
                (None, inline, after, false)
            }
        };
        self.inline_apart = apart;
        let assets = self.assets.clone();
        // The query first, so its width is there when the rest arrives.
        self.query_line.want(&assets, query, &mut self.stale);
        self.inline.want(&assets, inline, &mut self.stale);
        self.after.want(&assets, after, &mut self.stale);
        self.path_line.want(&assets, path, &mut self.stale);
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
        let lines = [
            &mut self.path_line,
            &mut self.query_line,
            &mut self.inline,
            &mut self.after,
        ];
        let ours = (lines.into_iter())
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
            let place = |sub: &mut Sub, icon: Option<(WlBuffer, f64)>, a: f64| match icon {
                Some((buffer, pad)) => sub.present(Some(&buffer), rect.outset(pad * size), a),
                None => sub.clear(),
            };
            match flying {
                true => subs.icon.clear(),
                false => place(&mut subs.icon, r.icon(&cache), alpha * fade),
            }
            match (&r.old, flying) {
                (Some(old), false) => {
                    let icon = old.buffer(&cache).map(|b| (b, old.pad()));
                    place(&mut subs.old, icon, alpha * (1.0 - fade));
                }
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

        // The search icon (or a folder, for files), with nothing found.
        let search = self
            .search_key(self.query.scope())
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
                // Into the dock's own icon at the end.
                let alpha = self.gate(((1.0 - t) / 0.15).clamp(0.0, 1.0));
                let buffer = fly.key.buffer(&cache);
                (self.fly_sub).present(buffer.as_ref(), r.outset(fly.key.pad() * r.w), alpha);
            }
            None => self.fly_sub.clear(),
        }
        drop(cache);

        // The query, the completion and the caret; for files, the path
        // under them, the two lines centered together.
        let text_alpha = self.gate((m - 0.5) / 0.5);
        let line_h = text::line_height(style.font_size) as f64;
        let files = self.query.scope() == Scope::Files;
        let x0 = bar.x + text_x(&style);
        let block_h = match files {
            true => line_h + text::line_height(style.path_size()) as f64,
            false => line_h,
        };
        let ty = (bar.y + (bar.h - block_h) / 2.0).round();
        match files {
            true => self.path_line.present(x0, ty + line_h, text_alpha),
            false => self.path_line.sub.clear(),
        }
        let query_w = match self.query.is_empty() {
            true => 0.0,
            false => self.query_line.width(),
        };
        match self.query.is_empty() {
            true => self.query_line.sub.clear(),
            false => self.query_line.present(x0, ty, text_alpha),
        }
        // A file's name that doesn't continue the query stands apart.
        let gap = match self.inline_apart {
            true => 1.5 * style.padding,
            false => 0.0,
        };
        self.inline.present(x0 + query_w + gap, ty, text_alpha);
        let after_x = bar.right() - 2.0 * style.padding - self.after.width();
        let after_min = x0 + query_w + style.padding;
        self.after.present(after_x.max(after_min), ty, text_alpha);
        let caret_w = (style.font_size / 12.0).round().max(1.0);
        let caret = Rect::new(x0 + query_w, ty, caret_w, line_h);
        let caret_alpha = if self.closing { 0.0 } else { text_alpha };
        self.caret
            .present(self.caret_buffer.as_ref(), caret, caret_alpha);
    }
}

/// A file's path under the query: its directories dim, its name less so,
/// and what the query matched bright. Directories the query didn't match
/// are elided so it fits in `max`, by an estimate.
fn path_request(f: &FileMatch, style: &LauncherStyle, max: i32, scale: i32) -> PillRequest {
    let chars = (max as f64 / (style.path_size() * CHAR_WIDTH)).max(1.0) as usize;
    let (text, matched) = filesearch::fit_path(&f.shown, &f.shown_matched, chars);
    let name_at = text.rfind('/').map_or(0, |i| i + 1);
    // The first span holding a character colors it.
    let mut spans: Vec<(Range<usize>, u32)> =
        matched.into_iter().map(|r| (r, style.text)).collect();
    spans.push((name_at..text.len(), style.completion));
    PillRequest {
        text,
        spans,
        max_width: max,
        scale,
        style: style.path_style(dimmer(style.completion)),
    }
}

/// A match's pill on the drum: its name, with what the query matched
/// brighter, and for a file its parent directory's name, dimmer.
fn label_request(m: &Match, style: &LauncherStyle, max: i32, scale: i32) -> PillRequest {
    let mut spans: Vec<(Range<usize>, u32)> =
        m.matched.iter().map(|r| (r.clone(), style.text)).collect();
    let mut text = m.name.clone();
    if let Some(f) = m.file.as_ref().filter(|f| !f.parent.is_empty()) {
        text.push_str("  ");
        let at = text.len();
        text.push_str(&f.parent);
        // The first span holding a character colors it.
        let matched = f
            .parent_matched
            .iter()
            .map(|r| (r.start + at..r.end + at, style.completion));
        spans.extend(matched);
        spans.push((at..text.len(), dimmer(style.completion)));
    }
    PillRequest {
        text,
        spans,
        max_width: max,
        scale,
        style: style.label_style(),
    }
}

/// `color` at half its alpha.
fn dimmer(color: u32) -> u32 {
    (color & !0xff) | ((color & 0xff) / 2)
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
