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
//! if the query is the start of it, else dimmed at the bar's end (hidden
//! while it doesn't fit beside the query). Like a text box, a query too
//! long for the bar scrolls out on the left, keeping the caret in view,
//! and the completion after it is cut at the bar's edge. The
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
//! inline after the caret if the query is the start of it, else dimmed at
//! the bar's end with what the query matched brighter (hidden while it
//! doesn't fit beside the query). The second, smaller and
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
//! Actions (Tab on a match): the match is the subject, and its icon
//! flies out of the front on a spring into the context pill, a small
//! pill of the bar's color just above the bar at its right edge (the drum
//! grows from the left, so that is free). The pill holds the subject's
//! icon at the labels' size and its name with its parent directory
//! dimmed; each level entered past it adds the chosen action's icon at
//! its right end (a crumb), flying in the same way, the deepest bright
//! and the earlier ones dimmer. Clicking the subject or a crumb goes back
//! to its level. The bar stays one clean line, as for apps, with the
//! level's placeholder. Entering a level slides the old texts out to the
//! left as the new ones come in from the right; going back slides them
//! the other way. The choices fill the whole drum. Going back flies the
//! crumb (or the subject) back into the front of the drum, which is
//! hidden until it lands, and restores what the drum showed (`leave`).
//!
//! Windows (an app's Windows…): a window's match shows its capture
//! (`set_previews`, `wl_shell_ui::card`) in place of the app's icon,
//! fitted into the icon's square with the icons' shadow, once it has a
//! frame; the app's icon until then, and when it flies to the dock. The
//! captures start (one frame each) when the app becomes the subject, go
//! live while their matches are on the ring (with `previews = "live"`),
//! pause when they leave it or the launcher closes, and go when the
//! search is back.
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
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::icons::{self, NamedIcon};
use wl_shell_render::worker::{Assets, PillRequest};
use wl_shell_render::{files, shadow, text};
use wl_shell_ui::cache::{self, Cache};
use wl_shell_ui::card::{self, Card, Content, Preview};
use wl_shell_ui::shapes::{self, Axis, Shape, ShapeKey};
use wl_shell_ui::{IRect, Rect, Spring, Sub};

use crate::actions::Level;
use crate::app::WlDock;
use crate::config::{Config, DockLayout};
use crate::drum::{self, Drum};
use crate::filesearch;
use crate::launcher::{Completion, Nav, Query, Scope, Selection, completion};
use crate::view::Handover;

type Qh = QueueHandle<Shell<WlDock>>;

/// The icon cross-fade when a slot's match changes, ms.
const FADE_RESPONSE: f64 = 120.0;
/// Shown while nothing was typed.
const PLACEHOLDER: &str = "Search apps";
const FILES_PLACEHOLDER: &str = "Search files";
/// How far the texts slide when a level is entered or left, of the
/// bar's width, at most `SLIDE_MAX` pixels.
const SLIDE: f64 = 0.12;
const SLIDE_MAX: f64 = 120.0;
/// The crumbs before the deepest, of its alpha.
const CRUMB_DIM: f64 = 0.5;
/// The front's icon with an empty query.
pub const SEARCH_ICONS: &[&str] = &["system-search", "edit-find", "search"];
/// Stand-ins for a file's icon, and a folder's.
const FILE_ICONS: [&str; 3] = ["text-x-generic", "application-x-generic", "unknown"];
/// A character's width, of the font size, at most (to fit paths).
const CHAR_WIDTH: f64 = 0.55;
/// The widest the query and its continuation are rendered (they scroll).
const SCROLLED_MAX: i32 = 8192;
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
    /// What the query matched.
    pub matched: u32,
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
            matched: theme.launcher_match,
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

    /// The context pill's icons: as tall as its name's line.
    fn pill_icon(&self) -> f64 {
        text::line_height(self.path_size()) as f64
    }

    /// The space around and between what the pill holds.
    fn pill_pad(&self) -> f64 {
        (self.padding / 2.0).round().max(2.0)
    }

    fn pill_height(&self) -> f64 {
        self.pill_icon() + 2.0 * self.pill_pad()
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
    /// The app whose icon it shows, if not the one `id` names.
    pub app: Option<String>,
    /// Theme icon names it shows instead, best first (an action's).
    pub named: Vec<String>,
    /// The parts of the name the query matched, as byte ranges.
    pub matched: Vec<Range<usize>>,
    /// What a file match has besides.
    pub file: Option<FileMatch>,
    /// The window (its ID) whose capture it shows instead of its icon,
    /// once there is a frame (`set_previews`).
    pub window: Option<String>,
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
    /// A crumb (or the subject) in the context pill was clicked: back to
    /// this depth.
    Back(usize),
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
        sub.crop(None);
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

    /// Present it at (`x`, `y`), showing only what is between `left` and
    /// `right`.
    fn present_clipped(&mut self, x: f64, y: f64, (left, right): (f64, f64), alpha: f64) {
        let Self { sub, shown, .. } = self;
        let Some((request, buffer, w, h)) = shown else {
            sub.clear();
            return;
        };
        // Whole logical pixels, so the crop maps 1:1 onto the destination.
        let (x, y) = (x.round(), y.round());
        let l = left.round().max(x);
        let r = right.round().min(x + *w);
        if r <= l || alpha <= 0.0 {
            sub.clear();
            return;
        }
        let s = request.scale.max(1) as f64;
        match l > x || r < x + *w {
            true => sub.crop(Some([(l - x) * s, 0.0, (r - l) * s, *h * s])),
            false => sub.crop(None),
        }
        sub.present(Some(buffer), Rect::new(l, y, r - l, *h), alpha);
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

/// The bar's texts: the query and what completes it, and for files the
/// path under them. The launcher has two sets: the one shown, and the one
/// sliding out as a level is entered or left.
struct Texts {
    query: Line,
    inline: Line,
    after: Line,
    /// A file search's second line: the selected file's path.
    path: Line,
    /// Laid out as two lines (files).
    two_lines: bool,
}

impl Texts {
    fn new(sub: &dyn Fn() -> Sub) -> Self {
        Self {
            inline: Line::new(sub()),
            query: Line::new(sub()),
            path: Line::new(sub()),
            after: Line::new(sub()),
            two_lines: false,
        }
    }

    fn lines(&mut self) -> [&mut Line; 4] {
        [
            &mut self.query,
            &mut self.inline,
            &mut self.after,
            &mut self.path,
        ]
    }

    /// Stop waiting for texts; with `shown`, drop what is shown too.
    fn forget(&mut self, shown: bool, stale: &mut Vec<WlBuffer>) {
        for line in self.lines() {
            line.pending = None;
            if shown && let Some((_, buffer, ..)) = line.shown.take() {
                stale.push(buffer);
            }
        }
    }

    fn clear(&mut self) {
        for line in self.lines() {
            line.sub.clear();
        }
    }

    /// Lay them out in `bar`, moved `dx` to the right but cut at the
    /// text's edges: the query, the completion inline after it or at the
    /// bar's end, and the path under them, the two lines centered
    /// together. Returns the caret's rect.
    fn place(&mut self, bar: Rect, style: &LauncherStyle, dx: f64, alpha: f64) -> Rect {
        let line_h = text::line_height(style.font_size) as f64;
        let x0 = bar.x + text_x(style);
        let x1 = bar.right() - 2.0 * style.padding;
        let clip = (x0, x1);
        let block_h = match self.two_lines {
            true => line_h + text::line_height(style.path_size()) as f64,
            false => line_h,
        };
        let ty = (bar.y + (bar.h - block_h) / 2.0).round();
        match self.two_lines {
            true => (self.path).present_clipped(x0 + dx, ty + line_h, clip, alpha),
            false => self.path.sub.clear(),
        }
        // A text box's viewport: what doesn't fit scrolls out on the
        // left, keeping the caret (always at the query's end) in view;
        // the completion after it is cut at the right edge.
        let query_w = self.query.width();
        let caret_w = (style.font_size / 12.0).round().max(1.0);
        let scroll = (query_w + caret_w - (x1 - x0)).max(0.0);
        let caret_x = x0 + dx + query_w - scroll;
        (self.query).present_clipped(x0 + dx - scroll, ty, clip, alpha);
        (self.inline).present_clipped(caret_x, ty, clip, alpha);
        // At the bar's end, unless it would run into the query or its completion.
        let after_x = x1 - self.after.width() + dx;
        match after_x >= caret_x + self.inline.width() + 1.5 * style.padding {
            true => self.after.present_clipped(after_x, ty, clip, alpha),
            false => self.after.sub.clear(),
        }
        Rect::new(caret_x, ty, caret_w, line_h)
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
    /// A window's capture and its shadow, in place of `icon`.
    card: Card,
    /// `card` shows something (so it is cleared once).
    card_shown: bool,
    /// The match's name, beside its icon.
    label: Line,
}

/// A card's shadow sides, and the card size they were rendered for.
type CardShadow = ((f64, f64), [Option<Shape>; 4]);

/// Where a window `aspect` wide (of its height) goes in a square icon
/// `rect`: as large as fits, centered.
fn fit(rect: Rect, aspect: f64) -> Rect {
    let (w, h) = match aspect >= 1.0 {
        true => (rect.w, rect.w / aspect),
        false => (rect.h * aspect, rect.h),
    };
    let (cx, cy) = rect.center();
    Rect::new(cx - w / 2.0, cy - h / 2.0, w, h)
}

/// The opened app's icon on its way to the dock.
struct Fly {
    key: IconKey,
    from: Rect,
    to: Rect,
    t: Spring,
}

/// In the context pill: the subject of the actions, or a crumb (the
/// action a level was entered with).
struct Pinned {
    m: Match,
    /// Its icon at the front's size, which it flies with.
    key: IconKey,
    standin: Option<IconKey>,
    /// The same at the pill's size, shown once it got there.
    small: IconKey,
    small_standin: Option<IconKey>,
    /// The depth it was entered from, which it drops back into: 0 for
    /// the subject.
    depth: usize,
    /// 0 in the front of the drum, 1 in the pill.
    t: Spring,
    /// Dropping back into the drum, after which it goes.
    leaving: bool,
}

impl Pinned {
    /// The icon to draw, and its pad of its size: the small one once it
    /// is in the pill (and there), a stand-in only without the real one.
    fn icon(&self, cache: &Cache) -> Option<(WlBuffer, f64)> {
        let shown = |k: &IconKey| k.buffer(cache).map(|b| (b, k.pad()));
        let in_pill = self.t.settled() && self.t.value >= 1.0;
        let small = in_pill.then_some(&self.small);
        let small_standin = self.small_standin.as_ref().filter(|_| in_pill);
        (small.and_then(shown))
            .or_else(|| shown(&self.key))
            .or_else(|| small_standin.and_then(shown))
            .or_else(|| self.standin.as_ref().and_then(shown))
    }

    fn keys(&self) -> impl Iterator<Item = &IconKey> {
        [Some(&self.key), self.standin.as_ref()]
            .into_iter()
            .chain([Some(&self.small), self.small_standin.as_ref()])
            .flatten()
    }
}

/// Where the context pill and what it holds go, for a name `name_w`
/// wide and crumbs each in by its weight (0 to 1: its slot grows as it
/// flies in).
#[derive(Clone, Debug, PartialEq)]
struct PillLayout {
    pill: Rect,
    /// The subject's icon, then each crumb's.
    icons: Vec<Rect>,
    /// Where the name's line starts.
    name_x: f64,
    /// Between the name and the crumbs, and how much it shows.
    divider: Option<(Rect, f64)>,
}

/// The pill just above `bar`, right-aligned to its right edge: the
/// subject's icon and name, then a divider and the crumbs.
fn pill_layout(style: &LauncherStyle, bar: Rect, name_w: f64, crumbs: &[f64]) -> PillLayout {
    let (icon, pad, h) = (style.pill_icon(), style.pill_pad(), style.pill_height());
    // Laid out from 0, then moved into place.
    let mut icons = vec![pad];
    let mut x = pad + icon;
    let name_x = x + pad;
    if name_w > 0.0 {
        x = name_x + name_w;
    }
    let shown = crumbs.iter().fold(0.0f64, |a, &w| a.max(w.clamp(0.0, 1.0)));
    let mut divider = None;
    if shown > 0.0 {
        x += 1.5 * pad * shown;
        divider = Some((x, shown));
        x += (1.0 + 1.5 * pad) * shown;
    }
    for &w in crumbs {
        icons.push(x);
        x += (icon + pad) * w.clamp(0.0, 1.0);
    }
    // The last crumb's gap is the end's.
    let end = match crumbs.last() {
        Some(&w) => pad * (1.0 - w.clamp(0.0, 1.0)),
        None => pad,
    };
    let w = (x + end).max(h);
    let pill = Rect::new(bar.right() - w, bar.y - style.padding - h, w, h);
    let icon_y = pill.y + pad;
    PillLayout {
        icons: (icons.into_iter())
            .map(|ix| Rect::new(pill.x + ix, icon_y, icon, icon))
            .collect(),
        name_x: pill.x + name_x,
        divider: divider.map(|(dx, a)| (Rect::new(pill.x + dx, icon_y, 1.0, icon), a)),
        pill,
    }
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
    /// The context pill, and the size it was rendered for.
    pill_shape: Option<Shape>,
    pill_rendered: (f64, f64),
}

pub struct LauncherView {
    // Field order is drop order: children before their parent surface.
    ring_subs: Vec<RingSubs>,
    /// Captures of the subject's windows (Windows…), while it is an app
    /// with several; their frames show on the ring in their matches'
    /// place of an icon. After the subsurfaces that show them.
    previews: Vec<Preview>,
    /// Capture them live while on the ring (else one frame each).
    live: bool,
    /// The context pill's icons: the subject's, then the crumbs'.
    pinned_subs: Vec<Sub>,
    fly_sub: Sub,
    panel: [Sub; 3],
    bar: [Sub; 3],
    pill: [Sub; 3],
    /// The subject's name in the pill, and the line before the crumbs.
    pill_name: Line,
    divider: Sub,
    search: Sub,
    /// The bar's texts, and those sliding out.
    texts: Texts,
    ghost: Texts,
    caret: Sub,
    /// The ring's subsurfaces go right below it: under the texts.
    ring_below: WlSurface,
    no_input: Region,
    viewport: WpViewport,
    layer: LayerSurface,
    transparent: WlBuffer,
    caret_buffer: Option<WlBuffer>,
    divider_buffer: Option<WlBuffer>,

    g: Rc<Globals>,
    qh: Qh,
    assets: Rc<Assets>,
    cache: Rc<std::cell::RefCell<Cache>>,

    width: f64,
    height: f64,
    scale: i32,
    configured: bool,
    attached: Option<Attached>,

    /// The query of the search or the level entered, and the levels.
    nav: Nav<Option<Level>, Vec<Match>>,
    /// The subject of the actions and the crumbs, while shown (or
    /// dropping back), by depth.
    pinned: Vec<Pinned>,
    /// The pill shows, 0 to 1.
    pill_in: Spring,
    /// The texts' slide: 0 as a level was entered or left, to 1; and
    /// its direction (1: entered, the old texts go left).
    slide: Spring,
    slide_dir: f64,
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
    /// Where the pill's icons were drawn, for clicks: the depth each
    /// goes back to, and its rect; and the pill.
    pill_hits: Vec<(usize, Rect)>,
    pill_at: Option<Rect>,
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
        let pill = [sub(), sub(), sub()];
        let search = sub();
        let texts = Texts::new(&sub);
        let ghost = Texts::new(&sub);
        let ring_below = texts.inline.sub.surface().clone();
        let caret = sub();
        let divider = sub();
        let pill_name = Line::new(sub());
        let fly_sub = sub();
        layer.commit();
        Ok(Self {
            ring_subs: Vec::new(),
            previews: Vec::new(),
            live: false,
            pinned_subs: Vec::new(),
            fly_sub,
            panel,
            bar,
            pill,
            pill_name,
            divider,
            search,
            texts,
            ghost,
            caret,
            ring_below,
            no_input,
            viewport,
            layer,
            transparent,
            caret_buffer: None,
            divider_buffer: None,
            g,
            qh,
            assets,
            cache,
            width: 0.0,
            height: 0.0,
            scale: 1,
            configured: false,
            attached: None,
            nav: Nav::default(),
            pinned: Vec::new(),
            pill_in: Spring::new(0.0, response),
            slide: Spring::new(1.0, response),
            slide_dir: 1.0,
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
            pill_hits: Vec::new(),
            pill_at: None,
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
            pill_shape: None,
            pill_rendered: (1.0, 1.0),
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
        let fi = &self.file_icons;
        for r in &mut self.ring {
            (r.key, r.standin) = Self::icon_keys(attached, &r.m, scale, fi, false);
            r.old = None;
        }
        for p in &mut self.pinned {
            (p.key, p.standin) = Self::icon_keys(attached, &p.m, scale, fi, false);
            (p.small, p.small_standin) = Self::icon_keys(attached, &p.m, scale, fi, true);
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
        // The pill: round ends, the bar's color and shadow.
        let h = a.style.pill_height();
        let radius = (h / 2.0).floor();
        let length = 2.0 * radius + 4.0;
        a.pill_rendered = (length, h);
        let pill = ShapeKey::Panel {
            w: length as i32,
            h: h as i32,
            radius: radius as i32,
            color: a.style.background,
            shadow: a.style.shadow,
            scale,
        };
        a.pill_shape = cache.shape(&self.g, pill, &self.qh);
        drop(cache);
        let (caret, divider) = (a.style.caret, dimmer(a.style.completion));
        for old in [self.caret_buffer.take(), self.divider_buffer.take()] {
            self.stale.extend(old);
        }
        self.caret_buffer = cache::color_buffer(&self.g, caret, &self.qh).ok();
        self.divider_buffer = cache::color_buffer(&self.g, divider, &self.qh).ok();
        for scope in [Scope::Apps, Scope::Files] {
            self.search_key(scope)
                .inspect(|k| self.assets.named_icon(k));
        }
    }

    /// The icon `names` (the first found) at the front's size, or with
    /// `small` the pill's (without a shadow).
    fn named(a: &Attached, scale: i32, small: bool, names: Vec<String>) -> Option<NamedIcon> {
        let mut names = names.into_iter();
        let (size, shadow) = match small {
            true => (a.style.pill_icon(), None),
            false => (a.style.front(), a.style.icon_shadow),
        };
        Some(NamedIcon {
            name: names.next()?,
            fallbacks: names.collect(),
            size: size.ceil() as u32 * scale as u32,
            theme: a.style.icon_theme.clone(),
            shadow,
            scale,
        })
    }

    /// The icon of `m` (at the front's size, or with `small` the pill's),
    /// and what stands in for it until it is there: a file's thumbnail or
    /// type icon (once `file_icons` has it), else a generic one.
    fn icon_keys(
        attached: Option<&Attached>,
        m: &Match,
        scale: i32,
        file_icons: &HashMap<PathBuf, Vec<String>>,
        small: bool,
    ) -> (IconKey, Option<IconKey>) {
        let (size, theme, shadow) = match attached {
            Some(a) if small => (a.style.pill_icon(), a.style.icon_theme.clone(), None),
            Some(a) => (
                a.style.front(),
                a.style.icon_theme.clone(),
                a.style.icon_shadow,
            ),
            None => (60.0, None, None),
        };
        let app = || {
            let id = m.app.as_deref().unwrap_or(&m.id);
            let key = icons::Key::new(id, size.ceil() as u32, theme.clone(), shadow, scale);
            IconKey::App(key.with_icon(m.icon.clone()))
        };
        let named = (attached.filter(|_| !m.named.is_empty()))
            .and_then(|a| Self::named(a, scale, small, m.named.clone()));
        if let Some(named) = named {
            return (IconKey::Named(named), None);
        }
        let (Some(f), Some(a)) = (&m.file, attached) else {
            return (app(), None);
        };
        let generic = match f.dir {
            true => &files::FOLDER_ICONS[..],
            false => &FILE_ICONS[..],
        };
        let generic = generic.iter().map(|s| s.to_string()).collect();
        let standin = Self::named(a, scale, small, generic).map(IconKey::Named);
        let own = (file_icons.get(&f.path).cloned())
            .and_then(|names| Self::named(a, scale, small, names))
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
            Scope::Apps | Scope::Actions | Scope::Web => SEARCH_ICONS,
            Scope::Files => &files::FOLDER_ICONS,
            Scope::Bang => &["web-browser"],
        };
        let names = names.iter().map(|s| s.to_string()).collect();
        Self::named(a, self.scale, false, names)
    }

    /// Have the worker render the ring's icons, and look up those of its
    /// files.
    fn request_icons(&mut self) {
        let cache = self.cache.borrow();
        let font = self
            .attached
            .as_ref()
            .map_or("sans-serif", |a| a.style.font.as_str());
        let pinned = self.pinned.iter().flat_map(Pinned::keys);
        let ringed = (self.ring.iter())
            .flat_map(|r| [Some(&r.key), r.standin.as_ref()].into_iter().flatten());
        for key in ringed.chain(pinned) {
            key.request(&cache, &self.assets, font);
        }
        let ms = (self.ring.iter().map(|r| &r.m)).chain(self.pinned.iter().map(|p| &p.m));
        for m in ms {
            if let Some(f) = &m.file
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
        let (fi, scale) = (&self.file_icons, self.scale);
        let is = |m: &Match| m.file.as_ref().is_some_and(|f| f.path == path);
        for r in self.ring.iter_mut().filter(|r| is(&r.m)) {
            (r.key, r.standin) = Self::icon_keys(attached, &r.m, scale, fi, false);
            changed = true;
        }
        for p in self.pinned.iter_mut().filter(|p| is(&p.m)) {
            (p.key, p.standin) = Self::icon_keys(attached, &p.m, scale, fi, false);
            (p.small, p.small_standin) = Self::icon_keys(attached, &p.m, scale, fi, true);
            changed = true;
        }
        if changed {
            self.request_icons();
            self.kick();
        }
    }

    // -- the query and the matches -------------------------------------------

    pub fn query(&self) -> &Query {
        self.nav.query()
    }

    pub fn query_mut(&mut self) -> &mut Query {
        self.nav.query_mut()
    }

    /// Levels entered: 0 while searching.
    pub fn depth(&self) -> usize {
        self.nav.depth()
    }

    /// The level entered, unless searching.
    pub fn level(&self) -> Option<&Level> {
        self.nav.level()?.as_ref()
    }

    /// The subject of the actions, while entered.
    pub fn subject(&self) -> Option<&Match> {
        (self.pinned.first())
            .filter(|p| p.depth == 0 && !p.leaving)
            .map(|p| &p.m)
    }

    /// Enter `level` from the selection: its query starts empty, and the
    /// drum is emptied for its choices (`set_matches`). The selection
    /// flies out of the front into the pill: from the search it is the
    /// subject, else a crumb.
    pub fn enter(&mut self, level: Level) {
        self.enter_level(Some(level), Query::actions());
    }

    pub fn enter_web(&mut self, keyword: String, text: &str) {
        let mut query = Query::default();
        query.commit_engine(keyword, text);
        self.enter_level(None, query);
    }

    fn enter_level(&mut self, level: Option<Level>, query: Query) {
        let depth = self.nav.depth();
        let selected = self.selected().and_then(|i| self.ring.get(i));
        let from = Selection {
            selected: selected.map(|r| r.m.id.clone()),
            rolled: self.rolled,
            shown: self.ring.iter().map(|r| r.m.clone()).collect(),
        };
        // Any still dropping back into this level (or deeper) are gone.
        self.pinned.retain(|p| !(p.leaving && p.depth >= depth));
        if let Some(r) = selected {
            let a = self.attached.as_ref();
            let response = a.map_or(150.0, |a| a.style.response);
            let mut t = Spring::new(0.0, response);
            t.retarget(1.0);
            let (small, small_standin) =
                Self::icon_keys(a, &r.m, self.scale, &self.file_icons, true);
            self.pinned.push(Pinned {
                m: r.m.clone(),
                key: r.key.clone(),
                standin: r.standin.clone(),
                small,
                small_standin,
                depth,
                t,
                leaving: false,
            });
        }
        self.pill_in.retarget(1.0);
        self.nav.enter_with(from, level, query);
        self.slide_texts(1.0);
        // The choices come in fresh: nothing cross-fades into them.
        self.ring.clear();
        self.roll.snap(0.0);
        self.rolled = false;
        self.request_icons();
        self.sync_texts();
        self.kick();
    }

    /// Go back out of the level: its query, matches and selection are
    /// the ones before again, and what it was entered with (the crumb,
    /// or the subject back in the search) drops into the front of the
    /// drum and stays selected. False while searching.
    pub fn leave(&mut self) -> bool {
        let Some(from) = self.nav.leave() else {
            return false;
        };
        let front = (from.selected.as_deref())
            .and_then(|id| from.shown.iter().position(|m| m.id == id))
            .unwrap_or(0);
        let depth = self.nav.depth();
        for p in self.pinned.iter_mut().filter(|p| p.depth >= depth) {
            p.leaving = true;
            p.t.retarget(0.0);
        }
        if depth == 0 {
            self.pill_in.retarget(0.0);
            // Back in the search: the subject's windows aren't needed.
            self.previews.clear();
        }
        self.slide_texts(-1.0);
        self.ring.clear();
        self.roll.snap(0.0);
        // The selection stays while results stream in.
        self.rolled = from.rolled || (depth == 0 && from.selected.is_some());
        self.replace(from.shown, front);
        true
    }

    /// Slide the texts shown out (`dir` 1: to the left, as a level is
    /// entered; −1: to the right), and the next ones in after them.
    fn slide_texts(&mut self, dir: f64) {
        std::mem::swap(&mut self.texts, &mut self.ghost);
        // The old texts stay as they are; the new ones start blank.
        self.ghost.forget(false, &mut self.stale);
        self.texts.forget(true, &mut self.stale);
        self.slide.snap(0.0);
        self.slide.retarget(1.0);
        self.slide_dir = dir;
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
        let slots = self.attached.as_ref().map_or(1, |a| a.style.matches).max(1);
        // What each slot shows now: slot `k` holds the match `k` past the
        // one in front.
        let old_front = self.selected().unwrap_or(0);
        let n_old = self.ring.len();
        // A window's preview counts: its app's other windows share its icon.
        let old: Vec<(IconKey, Option<String>)> = (0..n_old)
            .map(|k| &self.ring[(k + old_front) % n_old])
            .map(|r| (r.key.clone(), r.m.window.clone()))
            .collect();
        let matches: Vec<Match> = matches.into_iter().take(slots).collect();
        let n = matches.len();
        let front = front.min(n.saturating_sub(1));
        let attached = self.attached.as_ref();
        let ring: Vec<Ringed> = (matches.into_iter().enumerate())
            .map(|(i, m)| {
                let (key, standin) =
                    Self::icon_keys(attached, &m, self.scale, &self.file_icons, false);
                let prev = old.get((i + n - front) % n).cloned();
                let same = prev.as_ref() == Some(&(key.clone(), m.window.clone()));
                let mut fade = Spring::new(if same { 1.0 } else { 0.0 }, FADE_RESPONSE);
                fade.retarget(1.0);
                Ringed {
                    m,
                    key,
                    standin,
                    // A window's card has no icon to fade out: the new
                    // one only fades in.
                    old: (prev.filter(|(_, window)| !same && window.is_none())).map(|(key, _)| key),
                    fade,
                }
            })
            .collect();
        self.ring = ring;
        self.sync_live();
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

    /// Roll the match at ring index `i` to the front, the short way.
    pub fn select(&mut self, i: usize) {
        let n = self.ring.len() as i64;
        let Some(at) = self.selected().filter(|_| (i as i64) < n) else {
            return;
        };
        let mut by = (i as i64 - at as i64).rem_euclid(n);
        if by > n / 2 {
            by -= n;
        }
        if by != 0 {
            self.roll(by as i32);
        }
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
        let scope = self.nav.query().scope();
        // The names on the drum, with what the query matched brighter.
        let labels: Vec<PillRequest> = (self.ring.iter())
            .map(|r| {
                let web = scope == Scope::Web || r.m.id.starts_with(crate::engines::SUGGESTION);
                label_request(&r.m, style, max, scale, web)
            })
            .collect();
        let selected = self.selected().and_then(|i| self.ring.get(i)).map(|r| &r.m);
        let q = self.nav.query().text();
        // Zero-width spaces keep the query's own spaces at its ends.
        let query = (!q.is_empty()).then(|| line(format!("\u{200b}{q}\u{200b}"), style.text));
        // The subject's name in the pill (while it is there at all).
        let pill_name = (self.pinned.first())
            .filter(|p| p.depth == 0)
            .map(|p| name_request(&p.m, style, max / 2, scale));
        let (path, inline, after) = match scope {
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
                let (inline, after) = match (completion(q, name), file) {
                    (Completion::None, _) if q.is_empty() => {
                        (Some(line(FILES_PLACEHOLDER.into(), style.completion)), None)
                    }
                    (Completion::Inline(rest), _) if !rest.is_empty() => {
                        (Some(line(rest, style.completion)), None)
                    }
                    // Not the query's continuation: at the bar's end, with
                    // what the query matched brighter.
                    (Completion::After(name), Some((m, _))) => {
                        let spans = m
                            .matched
                            .iter()
                            .map(|r| (r.clone(), style.matched))
                            .collect();
                        (None, Some(spanned(name, spans, style.completion)))
                    }
                    _ => (None, None),
                };
                (path, inline, after)
            }
            Scope::Apps | Scope::Actions => {
                let name = selected.map(|m| m.name.as_str());
                // With nothing typed: among actions, the selected choice's
                // name (the default's at first); else the placeholder.
                let placeholder = match scope {
                    Scope::Actions => (name.filter(|n| !n.is_empty()))
                        .or_else(|| self.level().map(|l| l.placeholder.as_str()))
                        .unwrap_or(""),
                    _ => PLACEHOLDER,
                };
                let fallback = selected.filter(|m| m.id == crate::engines::FALLBACK);
                let (inline, after) = match (fallback, completion(q, name)) {
                    // "Search Google" continues nothing: always at the end.
                    (Some(m), _) => (None, Some(m.name.clone())),
                    (None, Completion::None) if q.is_empty() => {
                        (Some(placeholder.to_string()), None)
                    }
                    (None, Completion::None) => (None, None),
                    (None, Completion::Inline(rest)) if rest.is_empty() => (None, None),
                    (None, Completion::Inline(rest)) => (Some(rest), None),
                    (None, Completion::After(name)) => (None, Some(name)),
                };
                let inline = inline.map(|t| line(t, style.completion));
                let after = after.map(|t| line(t, style.completion));
                (None, inline, after)
            }
            Scope::Bang => {
                let name = selected.map(|m| m.name.as_str());
                let engine_label = || {
                    selected.map(|m| {
                        let spans = m
                            .matched
                            .iter()
                            .map(|r| (r.clone(), style.matched))
                            .collect();
                        spanned(m.name.clone(), spans, style.completion)
                    })
                };
                if let Some(m) = selected.filter(|m| m.id == crate::engines::FALLBACK) {
                    // An unknown bang, searched whole by the fallback.
                    (None, None, Some(line(m.name.clone(), style.completion)))
                } else {
                    match completion(q, name) {
                        Completion::Inline(rest) if !rest.is_empty() => {
                            (None, Some(line(rest, style.completion)), None)
                        }
                        Completion::After(_) => (None, None, engine_label()),
                        Completion::None if q.is_empty() => (None, None, engine_label()),
                        _ => (None, None, None),
                    }
                }
            }
            Scope::Web => {
                let name = selected
                    .filter(|m| m.id != crate::engines::TYPED)
                    .map(|m| m.name.as_str());
                let (inline, after) = match completion(q, name) {
                    Completion::Inline(rest) if !rest.is_empty() => (Some(rest), None),
                    Completion::After(text) => (None, Some(text)),
                    _ => (None, None),
                };
                (
                    None,
                    inline.map(|t| line(t, style.completion)),
                    after.map(|t| line(t, style.completion)),
                )
            }
        };
        // The query and what continues it scroll in the bar instead of
        // being cut: they are rendered whole (up to a sanity bound).
        let whole = |r: Option<PillRequest>| {
            r.map(|r| PillRequest {
                max_width: SCROLLED_MAX,
                ..r
            })
        };
        let (query, inline) = (whole(query), whole(inline));
        let assets = self.assets.clone();
        // The query first, so its width is there when the rest arrives.
        let texts = &mut self.texts;
        texts.two_lines = scope == Scope::Files;
        texts.query.want(&assets, query, &mut self.stale);
        texts.inline.want(&assets, inline, &mut self.stale);
        texts.after.want(&assets, after, &mut self.stale);
        texts.path.want(&assets, path, &mut self.stale);
        (self.pill_name).want(&assets, pill_name, &mut self.stale);
        self.grow_ring_subs(labels.len());
        let mut labels = labels.into_iter();
        for subs in &mut self.ring_subs {
            subs.label.want(&assets, labels.next(), &mut self.stale);
        }
    }

    /// At least `n` ring slots' subsurfaces.
    fn grow_ring_subs(&mut self, n: usize) {
        while self.ring_subs.len() < n {
            let (g, parent) = (&self.g, self.layer.wl_surface());
            let sub = || Sub::new(g, parent, &self.no_input, &self.qh);
            let card =
                (card::cards(g, parent, &self.no_input, 1, &self.qh).pop()).expect("one card");
            let subs = RingSubs {
                icon: sub(),
                old: sub(),
                card,
                card_shown: false,
                label: Line::pill(sub()),
            };
            // Each goes right below the texts, so the last is on top: the
            // card over the icon, the old icon over both (it fades out).
            subs.icon.place_below(&self.ring_below);
            for s in subs.card.shadow.iter().chain([&subs.card.preview]) {
                s.place_below(&self.ring_below);
            }
            subs.old.place_below(&self.ring_below);
            subs.label.sub.place_below(&self.ring_below);
            self.ring_subs.push(subs);
        }
    }

    /// A text from the worker; true if it was the launcher's.
    pub fn pill_ready(&mut self, tag: u64, request: &PillRequest, pill: &text::Pill) -> bool {
        let _ = request;
        let (g, qh) = (self.g.clone(), self.qh.clone());
        let [query, inline, after, path] = self.texts.lines();
        let lines = [query, inline, after, path, &mut self.pill_name];
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

    // -- window previews -------------------------------------------------------

    /// Capture `windows` (their IDs and handles) for the subject's
    /// Windows…, `live` while on the ring; captures of other windows go.
    /// Empty: none (no previews, or the subject isn't such an app).
    pub fn set_previews(&mut self, windows: Vec<(String, ExtForeignToplevelHandleV1)>, live: bool) {
        self.live = live;
        self.previews
            .retain(|p| windows.iter().any(|(id, _)| *id == p.window));
        for (id, handle) in windows {
            if !self.previews.iter().any(|p| p.window == id) {
                // One frame first: it goes live once it is on the ring.
                let p = Preview::new(id, &handle, &self.g, false, &self.qh);
                self.previews.push(p);
            }
        }
        self.sync_live();
        self.kick();
    }

    /// Capture the windows on the ring live (with `live`), and pause the
    /// rest after the frame they have.
    fn sync_live(&mut self) {
        let ring = &self.ring;
        for p in &mut self.previews {
            let shown = ring.iter().any(|r| r.m.window.as_ref() == Some(&p.window));
            p.set_live(self.live && shown && !self.closing, &self.g, &self.qh);
        }
    }

    pub fn has_capture(&self, id: CaptureId) -> bool {
        self.previews.iter().any(|p| p.id == id)
    }

    /// An event of capture `id`: a new frame shows at the next one.
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
        let shown = (self.ring.iter()).any(|r| r.m.window.as_ref() == Some(&window));
        if shown && (update.shown || update.resized) {
            self.kick();
        }
    }

    /// The cards' shadows for the windows on the ring with a frame: each
    /// rendered for its card at the front's size (and scaled with it).
    fn card_shadows(&self, front: f64) -> HashMap<String, CardShadow> {
        let mut shadows = HashMap::new();
        let Some(shadow) = (self.attached.as_ref()).and_then(|a| a.style.icon_shadow) else {
            return shadows;
        };
        // As the icons' shadow, so cards and icons look alike.
        let settings = shadow::Settings {
            color: shadow.color,
            softness: shadow.softness as f64,
            spread: 0,
            offset: shadow.offset,
        };
        let mut cache = self.cache.borrow_mut();
        let square = Rect::new(0.0, 0.0, front, front);
        for r in &self.ring {
            let Some(p) = (r.m.window.as_ref())
                .and_then(|w| self.previews.iter().find(|p| p.window == *w))
                .filter(|p| p.frame().is_some())
            else {
                continue;
            };
            let card = fit(
                square,
                card::aspect(p.aspect().unwrap_or(card::DEFAULT_ASPECT)),
            );
            let size = (card.w.round().max(1.0), card.h.round().max(1.0));
            let sides = (cache).card_shadow(
                &self.g,
                (size.0 as i32, size.1 as i32),
                settings,
                self.scale,
                &self.qh,
            );
            shadows.insert(p.window.clone(), (size, sides));
        }
        shadows
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
        self.sync_live();
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
                if let Some((to, _)) = self.pill_hits.iter().find(|(_, r)| r.contains(x, y)) {
                    return LauncherInput::Back(*to);
                }
                if let Some((i, _)) = self.hits.iter().find(|(_, r)| r.contains(x, y)) {
                    return LauncherInput::Open(*i);
                }
                let on_pill = self.pill_at.is_some_and(|r| r.contains(x, y));
                if self.bar_at.contains(x, y) || on_pill || self.attached.is_none() {
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
            || !self.pill_in.settled()
            || !self.slide.settled()
            || self.pinned.iter().any(|p| !p.t.settled())
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
        moving |= self.pill_in.advance(dt);
        for p in &mut self.pinned {
            moving |= p.t.advance(dt);
        }
        // Those dropped back are gone, the subject once the pill is too.
        let pill_out = self.pill_in.settled();
        (self.pinned).retain(|p| !(p.leaving && p.t.settled() && (p.depth > 0 || pill_out)));
        moving |= self.slide.advance(dt);
        if self.slide.settled() {
            self.ghost.forget(true, &mut self.stale);
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
        let (pill_shape, pill_rendered) = (a.pill_shape.clone(), a.pill_rendered);
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
        self.grow_pinned_subs(self.pinned.len());
        let card_shadows = self.card_shadows(drum.front);
        let cache = self.cache.borrow();
        let fly_key = self.fly.as_ref().map(|f| f.key.clone());
        self.hits.clear();
        let present = 1.0 - self.searching.value.clamp(0.0, 1.0);
        // What drops back into the front hides the match it lands on.
        let depth = self.nav.depth();
        let dropping = (self.pinned.iter())
            .find(|p| p.leaving && p.depth == depth)
            .map(|p| p.m.id.clone());
        for (i, subs) in self.ring_subs.iter_mut().enumerate() {
            let Some(r) = self.ring.get(i) else {
                subs.icon.clear();
                subs.old.clear();
                subs.label.sub.clear();
                if std::mem::take(&mut subs.card_shown) {
                    subs.card.clear();
                }
                continue;
            };
            let t = drum::position(i, self.roll.value, n);
            let p = drum.place(t, n);
            let size = p.size;
            let center = (cx, cy - p.rise * open);
            let rect = Rect::new(center.0 - size / 2.0, center.1 - size / 2.0, size, size);
            // The front one is flying to the dock, or what was entered
            // with is dropping back into its place.
            let flying =
                (fly_key.as_ref() == Some(&r.key) || dropping.as_ref() == Some(&r.m.id)) && t < 0.5;
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
            // A window shows its capture, once it has a frame, as a card
            // fitted into the icon's square; else (and flying) its icon.
            let preview = (r.m.window.as_ref())
                .and_then(|w| self.previews.iter_mut().find(|p| p.window == *w))
                .filter(|p| p.frame().is_some() && !flying);
            match preview {
                Some(preview) => {
                    let shadow = card_shadows.get(&preview.window);
                    let aspect = card::aspect(preview.aspect().unwrap_or(card::DEFAULT_ASPECT));
                    let card = fit(rect, aspect);
                    let rendered = shadow.map_or((card.w, card.h), |(size, _)| *size);
                    let content = Content::Preview {
                        preview: Some(preview),
                        placeholder: &self.transparent,
                    };
                    let (sides, a) = (shadow.map(|(_, s)| s), alpha * fade);
                    (subs.card).place(Some(content), card, sides, rendered, scale, a, None);
                    subs.card_shown = true;
                    subs.icon.clear();
                }
                None => {
                    if std::mem::take(&mut subs.card_shown) {
                        subs.card.clear();
                    }
                    match flying {
                        true => subs.icon.clear(),
                        false => place(&mut subs.icon, r.icon(&cache), alpha * fade),
                    }
                }
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

        // The context pill, just above the bar at its right end, coming
        // up a little as it fades in.
        let gate = |a: f64| match self.g.alpha_modifier {
            Some(_) => a.clamp(0.0, 1.0),
            None if a >= 0.5 => 1.0,
            None => 0.0,
        };
        let pill_in = self.pill_in.value.clamp(0.0, 1.0);
        let pill_alpha = gate(pill_in * open);
        let has_subject = self.pinned.first().is_some_and(|p| p.depth == 0);
        let weights: Vec<f64> = (self.pinned.iter())
            .filter(|p| p.depth > 0)
            .map(|p| p.t.value)
            .collect();
        let layout = pill_layout(&style, bar, self.pill_name.width(), &weights);
        let rise = ((1.0 - pill_in) * style.pill_pad()).round();
        let shift = |r: Rect| Rect::new(r.x, r.y + rise, r.w, r.h);
        let pill = shift(layout.pill);
        self.pill_hits.clear();
        self.pill_at = None;
        let round = &IRect::round;
        let shown = !self.pinned.is_empty();
        let shape = pill_shape.as_ref().filter(|_| shown);
        let (sp, pr) = (scale, pill_rendered);
        shapes::place_panel(
            &mut self.pill,
            shape,
            pill,
            pr,
            Axis::Horizontal,
            sp,
            round,
            pill_alpha,
        );
        if shown && pill_alpha > 0.3 {
            self.pill_at = Some(pill);
        }
        // The subject's name fades in as its icon gets there.
        let subject_t = (self.pinned.first().filter(|_| has_subject)).map_or(0.0, |p| p.t.value);
        let name_alpha = match shown {
            true => gate(pill_alpha * subject_t),
            false => 0.0,
        };
        let name_y = (pill.y + (pill.h - self.pill_name.height()) / 2.0).round();
        (self.pill_name).present(layout.name_x.round(), name_y, name_alpha);
        match (layout.divider, &self.divider_buffer) {
            (Some((r, a)), Some(buffer)) if shown => {
                (self.divider).present(Some(buffer), shift(r), gate(pill_alpha * a))
            }
            _ => self.divider.clear(),
        }
        // Its icons, flying between the front and their places in it. A
        // crumb dims as the next one comes in; a click on one goes back
        // to where it was chosen.
        let mut crumb = 0;
        for (k, sub) in self.pinned_subs.iter_mut().enumerate() {
            let Some(p) = self.pinned.get(k) else {
                sub.clear();
                continue;
            };
            let slot = match p.depth {
                0 => 0,
                _ => {
                    crumb += 1;
                    crumb
                }
            };
            let Some(&to) = layout.icons.get(slot) else {
                sub.clear();
                continue;
            };
            let to = shift(to);
            let r = front.lerp(to, p.t.value);
            let later = (self.pinned[k + 1..].iter())
                .filter(|q| q.depth > 0)
                .fold(0.0f64, |a, q| a.max(q.t.value.clamp(0.0, 1.0)));
            let dim = match p.depth {
                0 => 1.0,
                _ => 1.0 - (1.0 - CRUMB_DIM) * later,
            };
            match p.icon(&cache) {
                Some((buffer, pad)) => {
                    sub.present(Some(&buffer), r.outset(pad * r.w), gate(open * dim))
                }
                None => sub.clear(),
            }
            if !p.leaving && p.depth < depth && p.t.value > 0.9 && pill_alpha > 0.3 {
                self.pill_hits.push((p.depth, to));
                if p.depth == 0 {
                    let name = Rect::new(layout.name_x, pill.y, self.pill_name.width(), pill.h);
                    self.pill_hits.push((0, name));
                }
            }
        }

        // The search icon (or a folder, for files), with nothing found.
        let search = self
            .search_key(self.nav.query().scope())
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
        // under them. Entering or leaving a level slides the old texts
        // out and the new ones in after them.
        let text_alpha = self.gate((m - 0.5) / 0.5);
        let g = self.slide.value.clamp(0.0, 1.0);
        let dist = (bar.w * SLIDE).min(SLIDE_MAX) * self.slide_dir;
        let alpha = self.gate(text_alpha * g);
        let caret = self
            .texts
            .place(bar, &style, (dist * (1.0 - g)).round(), alpha);
        match self.slide.settled() {
            true => self.ghost.clear(),
            false => {
                let alpha = self.gate(text_alpha * (1.0 - g));
                self.ghost.place(bar, &style, (-dist * g).round(), alpha);
            }
        }
        let caret_alpha = if self.closing { 0.0 } else { alpha };
        self.caret
            .present(self.caret_buffer.as_ref(), caret, caret_alpha);
    }

    /// At least `n` subsurfaces for the pill's icons.
    fn grow_pinned_subs(&mut self, n: usize) {
        while self.pinned_subs.len() < n {
            let sub = Sub::new(&self.g, self.layer.wl_surface(), &self.no_input, &self.qh);
            self.pinned_subs.push(sub);
        }
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
        matched.into_iter().map(|r| (r, style.matched)).collect();
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
fn label_request(m: &Match, style: &LauncherStyle, max: i32, scale: i32, web: bool) -> PillRequest {
    let mut spans: Vec<(Range<usize>, u32)> = m
        .matched
        .iter()
        .map(|r| (r.clone(), if web { style.text } else { style.matched }))
        .collect();
    let mut text = m.name.clone();
    if let Some(f) = m.file.as_ref().filter(|f| !f.parent.is_empty()) {
        text.push_str("  ");
        let at = text.len();
        text.push_str(&f.parent);
        // The first span holding a character colors it.
        let matched = f
            .parent_matched
            .iter()
            .map(|r| (r.start + at..r.end + at, style.matched));
        spans.extend(matched);
        spans.push((at..text.len(), dimmer(style.completion)));
    }
    PillRequest {
        text,
        spans,
        max_width: max,
        scale,
        style: if web && m.id != crate::engines::TYPED {
            text::Style {
                color: style.completion,
                ..style.label_style()
            }
        } else {
            style.label_style()
        },
    }
}

/// The subject's name in the context pill: bare, in the labels' size,
/// with a file's parent directory dimmed after it.
fn name_request(m: &Match, style: &LauncherStyle, max: i32, scale: i32) -> PillRequest {
    let mut text = m.name.clone();
    let mut spans = Vec::new();
    if let Some(f) = m.file.as_ref().filter(|f| !f.parent.is_empty()) {
        text.push_str("  ");
        let at = text.len();
        text.push_str(&f.parent);
        spans.push((at..text.len(), dimmer(style.completion)));
    }
    PillRequest {
        text,
        spans,
        max_width: max.max(1),
        scale,
        style: style.path_style(style.text),
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
    fn a_window_fits_into_the_icons_square() {
        let square = Rect::new(10.0, 20.0, 80.0, 80.0);
        // Wide: as wide as the icon, centered top to bottom.
        assert_eq!(fit(square, 2.0), Rect::new(10.0, 40.0, 80.0, 40.0));
        // Tall: as tall, centered across.
        assert_eq!(fit(square, 0.5), Rect::new(30.0, 20.0, 40.0, 80.0));
        assert_eq!(fit(square, 1.0), square);
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

    #[test]
    fn the_pill_sits_above_the_bar_at_its_right_end() {
        let s = style();
        let bar = bar_rect(&s, 0.0, 1920.0, 1080.0);
        let (icon, pad) = (s.pill_icon(), s.pill_pad());
        let l = pill_layout(&s, bar, 100.0, &[]);
        assert_eq!(l.pill.right(), bar.right());
        assert_eq!(l.pill.bottom(), bar.y - s.padding);
        assert_eq!(l.pill.h, icon + 2.0 * pad);
        // The subject's icon, then its name, with the pad around them.
        assert_eq!(
            l.icons,
            [Rect::new(l.pill.x + pad, l.pill.y + pad, icon, icon)]
        );
        assert_eq!(l.name_x, l.icons[0].right() + pad);
        assert_eq!(l.pill.right(), l.name_x + 100.0 + pad);
        assert_eq!(l.divider, None);
        // A crumb: the divider, then its icon at the right end.
        let one = pill_layout(&s, bar, 100.0, &[1.0]);
        assert_eq!(one.pill.right(), bar.right());
        assert!(one.pill.w > l.pill.w);
        let (divider, shown) = one.divider.unwrap();
        assert_eq!(shown, 1.0);
        assert!(divider.x > one.name_x + 100.0 && divider.right() < one.icons[1].x);
        assert_eq!(one.icons[1].right(), one.pill.right() - pad);
        // Two: the newer one last.
        let two = pill_layout(&s, bar, 100.0, &[1.0, 1.0]);
        assert_eq!(two.icons[2].x, two.icons[1].right() + pad);
        assert_eq!(two.icons[2].right(), two.pill.right() - pad);
    }

    #[test]
    fn the_pill_grows_with_a_crumb_flying_in() {
        let s = style();
        let bar = bar_rect(&s, 0.0, 1920.0, 1080.0);
        let w = |crumbs: &[f64]| pill_layout(&s, bar, 80.0, crumbs).pill.w;
        assert_eq!(w(&[0.0]), w(&[]));
        let steps: Vec<f64> = (0..=10).map(|k| w(&[k as f64 / 10.0])).collect();
        assert!(steps.windows(2).all(|p| p[1] > p[0]), "{steps:?}");
        assert_eq!(*steps.last().unwrap(), w(&[1.0]));
        // Never narrower than round.
        assert!(pill_layout(&s, bar, 0.0, &[]).pill.w >= s.pill_height());
    }
}
