//! wl-dock as a wl-shell program: the config, the pins, the items, one
//! dock per output, and how they react to the shell's events.
//!
//! Docks follow the outputs: one is created for every output with a name
//! that the config gives a dock (`outputs`, `[output.<name>]`), restyled
//! when the config changes, and destroyed with its output. The config file
//! and the pins file are watched with inotify and the desktop's color
//! scheme followed; a config error keeps the previous config.
//!
//! Each dock gets its own items (dock.rs): the pins, then the running
//! apps, with the windows on its output if it has `windows = "output"`
//! and the `output-event-stream` runs. Clicking activates a window with
//! `activate-command` or launches the app from its desktop entry (looked
//! up by the worker), and a launched app's icon bounces until one of its
//! windows appears.
//!
//! The worker renders icons and labels for all docks into one cache.
//!
//! Clicking an app with several windows opens its strip (view.rs,
//! stripview.rs): a grabbing popup of the dock with a card per window,
//! which takes the keyboard. The dock owns the strip's captures; clicking
//! a card (or Enter) activates its window. Ctrl+click activates the
//! app's most recent window instead: the dock surface takes keyboard
//! focus on clicks, so the modifiers are known by the button's release.
//!
//! Right-clicking an app opens its menu (menu.rs, menuview.rs), another
//! grabbing popup; its rows are built here from the item, its desktop
//! entry (looked up first if need be) and its `[app.<id>]` table, and
//! picking one runs it here: activating or closing windows, launching
//! (a desktop action, or a new window), an app action, or pinning, which
//! writes the pins file. `[app.<id>]` also binds middle click and
//! scrolling on the icon to commands.
//!
//! Dragging: a left press on an app's icon that moves `drag-threshold`
//! before its release picks the icon up (view.rs) instead of clicking
//! it, and closes any strip or menu. Dropping it reorders the pins, pins
//! a running app, or (on the Trash) unpins it (drag.rs); the pins file is
//! written and the items rebuilt at once, so the dock shows the new order
//! as the icon lands. Releasing off the dock, Escape, or the pointer
//! leaving cancels.
//!
//! The Trash: its icon follows whether `$XDG_DATA_HOME/Trash/files` is
//! empty (watched with inotify); clicking it runs `open-command` with
//! `trash:///`, and its menu has Open and Empty Trash
//! (`trash-empty-command`).
//!
//! Folders: the pinned paths that are folders come after the apps.
//! Clicking one lists it (the worker reads it, with thumbnails or type
//! icons) and opens its popup, a menu of its newest entries; picking one
//! opens it, and dragging one out offers it as a file
//! (`wl_data_device`). Its menu has Open and Remove from Dock.
//!
//! Drops from outside (`text/uri-list` of local files): the list is read
//! as soon as a drag enters a dock, and app.rs works out which items take
//! it and what their labels say (`Offer`); the dock shows where it would
//! go (drag.rs) and app.rs accepts or refuses it to match. Dropped, the
//! files open with an app, move or copy into a folder (as the source and
//! compositor agree), go to the trash, or, if they are all folders,
//! get pinned.
//!
//! The launcher (launcher.rs, launcherview.rs): `wl-dock --launcher`
//! asks over the dock's socket. The launcher surface lands on the focused
//! output, and the bottom dock there hands over its panel; with none, the
//! client gets an error. Typing ranks the installed apps (search.rs, with
//! the launch history in history.rs); Enter activates the selection's
//! most recent window or launches it (`launcher-enter`), and the client
//! is answered once the launcher gave up the keyboard. Escape, a click
//! outside, or another `--launcher` closes it.
//!
//! Typing the files trigger first searches files instead (filesearch.rs):
//! the walk starts then, on threads of its own, and lives until the
//! launcher closes. Its news pings the loop; the launcher's next frame
//! takes the best matches (never blocking) and streams them onto the
//! drum. Enter opens the front match with `open-command`, Ctrl+Enter its
//! folder.
//!
//! Tab on a file match asks the worker for its type (once per launcher
//! session), then enters its actions (actions.rs) as a level of the
//! launcher; Tab on an app enters its actions at once (its dock menu's,
//! but its windows). The level's query filters them (`rank_choices`).
//! Running one opens a path with `open-command`, opens the file with an
//! app (Open with…, recorded in the history), launches or runs an app's
//! action, pins or unpins, closes windows, or enters its choices. An app
//! picked for a query is learned (history.rs), and comes first for that
//! query and its prefixes from then on.
//!
//! Plugins' launcher sources: a source's trigger typed first (the
//! trigger table: files', windows', then the sources', `triggers`)
//! enters its scope. A source with `items()` is asked once and its items
//! ranked here with nucleo for every query (`show_source`); one with
//! `search` is asked `SEARCH_DEBOUNCE` after the typing stops, and shows
//! what the plugin emits for the latest query only (`search_source`).
//! Enter runs the item's `run(item)`, else opens a file item or opens an
//! app item as an app match would; Tab gives a file item its file's
//! actions, an app item its app's, and other items the plugins' actions
//! for items (`when = { kind = 'item', source = … }`).
//!
//! Keyboard activation (activate.rs, activateview.rs): `wl-dock
//! --activate n` asks over the socket and is answered at once. The dock
//! maps the activate surface, which takes the keyboard on the focused
//! output; its `wl_surface.enter` picks the dock, whose item n is acted
//! on (its window picked, launched, or its strip or folder popup opened
//! without a grab). The keys and modifiers come through that surface: n
//! again steps the highlight, another number switches items, and letting
//! go of the modifiers held when the keyboard came activates the picked
//! or highlighted window (or opens the entry). What it activates runs
//! once the surface is gone, so the window gets the keyboard.
//!
//! Widgets (widgets.rs): Lua plugins run on wl-shell-lua's thread pool,
//! configured from the `widget` keys, the `[widget.<name>]` tables
//! pinned as `@name`, and the `[plugin.<name>]` tables, and restarted
//! when their files change (watched like the config). Their events come
//! through a channel into the loop; a widget's label replaces its item's
//! name in every dock, its icon tree (painted by the worker, per dock)
//! the item's icon, and its span lengthens the item.
//!
//! Standalone widgets are items of their own, among the pinned apps or
//! among the folders: dragged anywhere from the first app to just before
//! the Trash, removed (on the Trash, or with their menu's Remove from
//! Dock) but never unpinned.
//!
//! A widget's popup opens on hover (an app's always, a standalone
//! widget's unless its plugin says `popup_on = 'click'`), and then the
//! item's label doesn't show unless `popup()` has nothing to show. A
//! click on a standalone widget opens or closes its popup if it opens
//! on click, else calls its `on_click`, else opens or closes its popup.
//! While a popup is open, no label shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use smithay_client_toolkit::{
    seat::{
        keyboard::{KeyEvent, Modifiers},
        pointer::{AxisScroll, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, PointerEvent, PointerEventKind},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerSurface, LayerSurfaceConfigure},
        xdg::popup::{Popup, PopupConfigure},
    },
};
use wayland_client::protocol::{
    wl_data_device_manager::DndAction, wl_output::WlOutput, wl_surface::WlSurface,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::Reloader;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::dnd::DragIcon;
use wl_shell_core::globals::Globals;
use wl_shell_core::reexports::calloop::{LoopHandle, RegistrationToken, channel, ping};
use wl_shell_core::spawn::{self, Output};
use wl_shell_core::stream::{Hooks, LineStream};
use wl_shell_core::toplevel::Toplevel;
use wl_shell_core::{Core, Program, Shell};
use wl_shell_daemon::{Client, Incoming as SocketEvent, Listener, Reply};
use wl_shell_log::warn;
use wl_shell_lua::{PopupOn, RequestId};
use wl_shell_render::files::{self, FileEntry, FolderRequest};
use wl_shell_render::icons::{AppEntry, DesktopAction, IndexedApp};
use wl_shell_render::worker::{Assets, Done};
use wl_shell_ui::Cache;

use crate::actions::{self, Does, FileType, Icon, PluginRun, Subject};
use crate::activate::{self, Press, Release};
use crate::activateview::ActivateView;
use crate::config::{Config, FolderView, LauncherEnter, Previews, Windows};
use crate::dock::{self, Click, Item, Kind, Order, Placement};
use crate::drag::{self, DropTarget, Payload};
use crate::engines::{self, Engine, Place};
use crate::filesearch::{self, FileSearch, Found};
use crate::folder;
use crate::history::{self, History};
use crate::ipc::DockRequest;
use crate::launch::{self, Command};
use crate::launcher::{self, Act, Scope, Triggers};
use crate::launcherview::{
    Detail, FileMatch, FilesStatus, LauncherInput, LauncherStyle, LauncherView, Match, SourceLook,
};
use crate::menu::{self, Action, MenuKey};
use crate::pins::{self, Pin, Zone};
use crate::search::Search;
use crate::sources::SourceKey;
use crate::strip::{self, Key};
use crate::trash;
use crate::view::{DockView, MenuEvent, Offer, Style};
use crate::websuggest;
use crate::widgets::{self, Effect, ItemRun, Widgets};
use crate::windowsearch;

/// How long the launcher (or the activate surface) waits for the
/// compositor to say which output it is on before it takes the first
/// (bottom) dock.
const PLACE_TIMEOUT: Duration = Duration::from_millis(500);

/// The mime type of dropped (and dragged out) files.
const URI_LIST: &str = "text/uri-list";

/// Pixels of touchpad scrolling per step of a scroll command.
const SCROLL_STEP: f64 = 50.0;

/// How long the pointer may be off both the dock and a widget's popup
/// (on its way from one to the other) before the popup closes.
const WIDGET_GRACE: Duration = Duration::from_millis(180);

/// How long typing in a source's search settles before the plugin is
/// asked.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(80);
/// What a source's scope says while nothing matches the query.
const NO_MATCHES: &str = "No matches";
/// What a source's scope says once its plugin is gone.
const SOURCE_GONE: &str = "Not available";

/// An app launched from the dock, until one of its windows appears.
struct Launch {
    app_id: String,
    /// The app's windows when it was launched: a new one ends the launch.
    before: Vec<String>,
    /// Ends it after `launch-timeout`.
    timeout: Option<RegistrationToken>,
}

/// A button that went down on an app's icon: it is clicked if the button
/// comes up on it too.
#[derive(Clone, Debug, PartialEq)]
struct Pressed {
    output: WlOutput,
    app_id: String,
    button: u32,
    /// The press's serial, for the strip's grab.
    serial: u32,
    /// Where it went down on the dock surface.
    at: (f64, f64),
}

/// A drag from outside over a dock.
struct Incoming {
    /// Which drag it is (one may enter again while the last one's data is
    /// still read).
    id: u64,
    output: WlOutput,
    /// The dropped paths, once read; None while reading, or if they
    /// aren't all local files.
    payload: Option<Payload>,
    /// What the dock was last told it takes, and what was accepted.
    accepted: Option<(DropTarget, DndAction)>,
}

/// A folder's popup waiting for its listing.
struct PendingFolder {
    output: WlOutput,
    /// The folder's key.
    key: String,
    /// The click's serial, for the popup's grab; None from the keyboard,
    /// without a grab.
    serial: Option<u32>,
    tag: u64,
    view: FolderView,
    /// From the keyboard: highlight the first entry, then step on this
    /// many more.
    steps: Option<usize>,
}

/// Keyboard activation, while it is on.
struct Keys {
    view: ActivateView,
    mode: activate::Mode,
    /// The dock acting, once the surface's output is known.
    output: Option<WlOutput>,
    /// The item asked for before that, acted on then.
    first: Option<usize>,
    /// The window a press picked, or the strip's highlighted one: what
    /// letting go activates.
    window: Option<String>,
    /// The item acted on, magnified once the dock shows the mode.
    item: Option<String>,
    /// The dock shows the mode (the badges, the magnification, the
    /// strip): after `activate.delay`, so quick switches show nothing.
    shown: bool,
    /// Shows it when it fires.
    timer: Option<RegistrationToken>,
}

/// What keyboard activation does once its surface gave up the keyboard,
/// so the window gets it.
enum KeysRun {
    Activate(String),
    Menu(String, Action),
}

/// An app the launcher opened, run once it gave up the keyboard.
struct PendingOpen {
    /// The app as the dock knows it.
    app_id: String,
    /// Its desktop ID, for the history.
    id: String,
    /// Launch a new instance (else activate a window, if it has one).
    launch: bool,
    /// The query it was picked for, to learn.
    query: String,
}

/// A file the launcher opens with an app (Open with…), once it gave up
/// the keyboard.
struct PendingWith {
    app_id: String,
    /// Its desktop ID, for the history.
    id: String,
    path: PathBuf,
}

/// What the launcher runs for an app (its actions), once it gave up the
/// keyboard, so a new window gets it.
struct PendingApp {
    /// The app as the dock knows it.
    app_id: String,
    run: AppRun,
    /// What the launcher's client is told.
    picked: String,
}

enum AppRun {
    Desktop(DesktopAction),
    Command(String),
    NewWindow,
    Activate(String),
}

/// The launcher's file search, from the trigger until the launcher
/// closes. Dropping it cancels the walk and frees the items.
struct Files {
    search: FileSearch,
    /// Set by the search's threads when there is something new (the
    /// first time since it was cleared, they ping the loop).
    news: Arc<AtomicBool>,
    ping: RegistrationToken,
    handle: LoopHandle<'static, Shell<WlDock>>,
}

impl Drop for Files {
    fn drop(&mut self) {
        self.handle.remove(self.ping);
    }
}

/// A file the search `found` (under `root`), as the launcher shows it.
fn file_match(search: &mut FileSearch, root: &Path, home: Option<&Path>, found: Found) -> Match {
    let matched = search.highlight(&found.rel);
    let parts = filesearch::parts(&found.rel, &matched);
    let path = root.join(&found.rel);
    // The shown path ends with `rel`, so its matches move by the rest.
    let shown = filesearch::tilde(&path, home);
    let shown_matched = match shown.ends_with(found.rel.as_str()) {
        true => {
            let at = shown.len() - found.rel.len();
            (matched.iter()).map(|r| r.start + at..r.end + at).collect()
        }
        false => Vec::new(),
    };
    let file = FileMatch {
        path,
        dir: found.dir,
        parent: parts.parent.to_string(),
        parent_matched: parts.parent_matched,
        shown,
        shown_matched,
    };
    Match {
        name: parts.name.to_string(),
        icon: None,
        app: None,
        named: Vec::new(),
        matched: parts.name_matched,
        file: Some(file),
        window: None,
        detail: None,
        id: found.rel,
    }
}

/// The desktop entry of the window app `app_id`: the one the dock knows
/// it by (`entries`, from the worker), else one named like it.
fn app_of<'a>(
    apps: &'a [IndexedApp],
    entries: &HashMap<String, Option<AppEntry>>,
    app_id: &str,
) -> Option<&'a IndexedApp> {
    let entry = match entries.get(app_id) {
        Some(Some(e)) => Some(e.desktop_id.as_str()),
        _ => None,
    };
    windowsearch::find_app(apps, entry, app_id)
}

/// The name of the window app `app_id`: its desktop entry's, else the ID.
fn app_name(
    apps: &[IndexedApp],
    entries: &HashMap<String, Option<AppEntry>>,
    app_id: &str,
) -> String {
    match (app_of(apps, entries, app_id), entries.get(app_id)) {
        (Some(app), _) => app.name.clone(),
        (None, Some(Some(e))) if !e.name.is_empty() => e.name.clone(),
        _ => app_id.to_string(),
    }
}

/// A window as the launcher shows it: its title (its app's name if it
/// has none), and its app's name after it, dimmed.
fn window_match(
    search: &mut Search,
    apps: &[IndexedApp],
    entries: &HashMap<String, Option<AppEntry>>,
    w: &windowsearch::Window,
    query: &str,
) -> Match {
    let icon = app_of(apps, entries, &w.app_id).and_then(|a| a.entry.icon.clone());
    let title = w.shown_title();
    let detail = (!w.untitled()).then(|| Detail {
        text: w.app.clone(),
        matched: search.highlight(&w.app, query),
    });
    Match {
        id: w.id.clone(),
        name: title.to_string(),
        icon,
        app: Some(w.app_id.clone()),
        named: Vec::new(),
        matched: search.highlight(title, query),
        file: None,
        window: Some(w.id.clone()),
        detail,
    }
}

/// An item of a plugin's source as the launcher shows it: its title,
/// its subtitle dimmed after it (`matched` and `detail` are what the
/// query matched in them), and its icon, else the source's (`fallback`).
/// A file item is a file match (its icon a thumbnail or its type's, its
/// folder after it without a subtitle); an app item shows the app's
/// icon.
pub(crate) fn item_match(
    item: &wl_shell_lua::Item,
    matched: Vec<std::ops::Range<usize>>,
    detail: Vec<std::ops::Range<usize>>,
    fallback: Option<&str>,
    apps: &[IndexedApp],
    home: Option<&Path>,
) -> Match {
    let mut m = Match {
        id: item.id.clone(),
        name: item.title.clone(),
        icon: None,
        app: None,
        named: item.icon.iter().cloned().collect(),
        matched,
        file: None,
        window: None,
        detail: (item.subtitle.clone()).map(|text| Detail {
            text,
            matched: detail,
        }),
    };
    match &item.kind {
        wl_shell_lua::ItemKind::File { path } => {
            let path = PathBuf::from(path);
            if m.detail.is_none()
                && let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
            {
                m.detail = Some(Detail {
                    text: filesearch::tilde(parent, home),
                    matched: Vec::new(),
                });
            }
            m.file = Some(FileMatch {
                dir: path.is_dir(),
                parent: String::new(),
                parent_matched: Vec::new(),
                shown: filesearch::tilde(&path, home),
                shown_matched: Vec::new(),
                path,
            });
        }
        wl_shell_lua::ItemKind::App { app } => {
            m.icon = (apps.iter().find(|a| a.id == *app)).and_then(|a| a.entry.icon.clone());
            m.app = Some(app.clone());
        }
        wl_shell_lua::ItemKind::Item => {
            m.named.extend(fallback.map(str::to_string));
            m.named.push(actions::PLUGIN_ICON.into());
        }
    }
    m
}

/// `n` with its thousands separated: `12,345`.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Tags of folder listings, unique in the process.
fn next_tag() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1 << 40);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

pub struct WlDock {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The config in use.
    config: Config,
    /// The config file was touched since it was last read.
    config_touched: bool,
    /// The pins file; None without a state directory.
    pins_path: Option<PathBuf>,
    pins: Vec<Pin>,
    /// The pins file was touched since it was last read.
    pins_touched: bool,
    /// The worker, started with the dock.
    assets: Option<Rc<Assets>>,
    /// Icons, labels and shapes, shared by the docks.
    cache: Rc<RefCell<Cache>>,
    /// One per output that has a dock.
    docks: Vec<DockView>,
    /// The running apps, in the order their first window appeared.
    order: Order,
    /// Every app ID seen, to notice new apps (which may come with a new
    /// desktop entry).
    seen: Vec<String>,
    /// Which output each window is on, from the output stream.
    placement: Placement,
    output_stream: LineStream<Self>,
    /// Desktop entries from the worker, by app ID; None: it has none.
    entries: HashMap<String, Option<AppEntry>>,
    /// Apps to launch once their desktop entry arrives.
    pending_launches: Vec<String>,
    launching: Vec<Launch>,
    /// The app a button went down on.
    pressed: Option<Pressed>,
    /// A right click whose menu opens once its app's desktop entry
    /// arrives.
    pending_menu: Option<Pressed>,
    /// Scrolling on an icon not yet run as a command: the app, and steps
    /// (down is positive).
    scrolled: Option<(String, f64)>,
    /// Warned that `activate-command` is missing.
    warned_activate: bool,
    /// The items may have changed since they were last built.
    dirty: bool,
    /// The outputs changed since the docks were matched to them.
    outputs_dirty: bool,
    /// Buffers in the cache were replaced: once no dock shows them, they
    /// can go.
    trim: bool,
    /// The cache was cleared: docks get their shapes again.
    reload: bool,
    /// The Trash's `files` directory; None without a data directory.
    trash_dir: Option<PathBuf>,
    /// Something is in the Trash.
    trash_full: bool,
    /// The Trash changed since it was last looked at.
    trash_touched: bool,
    /// Watching the Trash.
    trash_watch: Option<RegistrationToken>,
    /// A drag from outside over a dock.
    incoming: Option<Incoming>,
    /// A folder's popup waiting for its listing.
    pending_folder: Option<PendingFolder>,
    /// The dock a file is being dragged out of (its popup).
    dragging_out: Option<WlOutput>,
    /// The socket `--launcher` talks to; None if it couldn't be bound.
    listener: Option<Listener<DockRequest>>,
    /// The launcher, while open (or closing).
    launcher: Option<LauncherView>,
    /// The `--launcher` waiting for the launcher's outcome.
    launcher_client: Option<Client>,
    pending_open: Option<PendingOpen>,
    /// A file the launcher opened (with `open-command`), and the path
    /// its client is told; run once it gave up the keyboard.
    pending_path: Option<(PathBuf, String)>,
    pending_url: Option<(String, String)>,
    /// A file the launcher opens with an app.
    pending_with: Option<PendingWith>,
    /// A folder the launcher opens a terminal in, and the path its
    /// client is told; run once it gave up the keyboard.
    pending_terminal: Option<(PathBuf, String)>,
    /// A plugin's action the launcher runs, and what the client is told;
    /// run once it gave up the keyboard.
    pending_plugin: Option<(PluginRun, String)>,
    /// An item of a plugin's source the launcher runs (`run(item)`),
    /// and what the client is told.
    pending_item: Option<(ItemRun, String)>,
    /// An app's action the launcher runs.
    pending_app: Option<PendingApp>,
    /// A window the launcher activates, once it gave up the keyboard.
    pending_window: Option<String>,
    files: Option<Files>,
    web_worker: Option<websuggest::Worker>,
    web_session: Option<websuggest::Session>,
    web_generation: u64,
    mime_apps: wl_shell_render::mimeapps::MimeApps,
    /// Files' types for the launcher's actions, for its session.
    file_types: HashMap<PathBuf, FileType>,
    /// Tab on this file waits for its type.
    pending_tab: Option<PathBuf>,
    /// The installed apps, from the worker.
    apps: Arc<Vec<IndexedApp>>,
    search: Search,
    /// Launches, for ranking; None without a state directory.
    history_path: Option<PathBuf>,
    history: History,
    /// Keyboard activation, while it is on.
    keys: Option<Keys>,
    /// What it activates or opens, once its surface is gone.
    pending_keys: Vec<KeysRun>,
    /// The plugins behind widgets.
    widgets: Widgets,
    /// The hovered item whose widget's popup opens (or is open), and on
    /// which dock.
    widget_target: Option<(WlOutput, String)>,
    /// Counts changes of `widget_target`: a timer to open a popup only
    /// opens it if there was none since.
    widget_epoch: u64,
    /// The item whose widget's popup was open when the button went down
    /// on the dock: clicking it closes the popup for good.
    widget_was_open: Option<String>,
    /// The hovered item whose widget had no popup to show (`popup()`
    /// was nil): it shows its label, and isn't asked again until the
    /// pointer moves to another item.
    widget_declined: Option<String>,
    /// The standalone widgets pinned when the plugins were last
    /// configured (they run while they are pinned).
    widget_pins: Vec<String>,
    /// Counts pointer motion on the dock and the popup: a timer to close
    /// the popup only closes it if there was none since.
    widget_grace: u64,
    /// Watching the plugin files.
    plugin_watches: Vec<RegistrationToken>,
}

impl WlDock {
    /// Read the config (`explicit`, or the default file) and the pins.
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        let reloader = Reloader::<Config>::load(explicit).map_err(|e| format!("config: {e}"))?;
        let pins_path = pins::path();
        let pins = match &pins_path {
            Some(path) => pins::read(path).unwrap_or_else(|e| {
                warn!("{}: {e}", path.display());
                Vec::new()
            }),
            None => Vec::new(),
        };
        Ok(Self {
            config: reloader.config().clone(),
            config_touched: false,
            reloader,
            pins_path,
            pins,
            pins_touched: false,
            assets: None,
            cache: Rc::new(RefCell::new(Cache::default())),
            docks: Vec::new(),
            order: Order::default(),
            seen: Vec::new(),
            placement: Placement::default(),
            output_stream: LineStream::new(
                "output-event-stream",
                Hooks {
                    locate: |shell: &mut Shell<Self>| &mut shell.program.output_stream,
                    line: |shell, line| shell.program.output_line(line),
                    changed: |shell| shell.program.dirty = true,
                },
            ),
            entries: HashMap::new(),
            pending_launches: Vec::new(),
            launching: Vec::new(),
            pressed: None,
            pending_menu: None,
            scrolled: None,
            warned_activate: false,
            dirty: true,
            outputs_dirty: true,
            trim: false,
            reload: false,
            trash_dir: trash::files_dir(),
            trash_full: false,
            trash_touched: true,
            trash_watch: None,
            incoming: None,
            pending_folder: None,
            dragging_out: None,
            listener: None,
            launcher: None,
            launcher_client: None,
            pending_open: None,
            pending_path: None,
            pending_url: None,
            pending_with: None,
            pending_terminal: None,
            pending_plugin: None,
            pending_item: None,
            pending_app: None,
            pending_window: None,
            files: None,
            web_worker: None,
            web_session: None,
            web_generation: 0,
            mime_apps: Default::default(),
            file_types: HashMap::new(),
            pending_tab: None,
            apps: Arc::default(),
            search: Search::default(),
            history: (history::path().as_deref())
                .map(|p| {
                    history::read(p).unwrap_or_else(|e| {
                        warn!("{}: {e}", p.display());
                        History::default()
                    })
                })
                .unwrap_or_default(),
            history_path: history::path(),
            keys: None,
            pending_keys: Vec::new(),
            widgets: Widgets::default(),
            widget_target: None,
            widget_epoch: 0,
            widget_was_open: None,
            widget_declined: None,
            widget_pins: Vec::new(),
            widget_grace: 0,
            plugin_watches: Vec::new(),
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker and the streams, watch the files, and create the docks.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.config.launcher.web.check();
        self.start_worker(core)?;
        self.start_widgets(core);
        self.start_web_worker(core);
        self.load_browser_preferences();
        self.watch_config(core);
        self.watch_pins(core);
        self.watch_trash(core);
        self.use_streams(core);
        self.prefetch_entries();
        if let Some(assets) = &self.assets {
            assets.apps();
        }
        match Listener::bind(core) {
            Ok(listener) => self.listener = Some(listener),
            Err(e) => warn!("no launcher: {e}"),
        }
        self.after_dispatch(core);
        Ok(())
    }

    fn start_worker(&mut self, core: &Core<Self>) -> Result<(), String> {
        let (assets, results) =
            Assets::spawn().map_err(|e| format!("failed to start the worker: {e}"))?;
        core.loop_handle
            .insert_source(results, |event, _, shell: &mut Shell<Self>| {
                if let channel::Event::Msg(done) = event {
                    shell.program.worker_done(&shell.core, done);
                }
            })
            .map_err(|e| format!("failed to watch the worker: {e}"))?;
        self.assets = Some(Rc::new(assets));
        Ok(())
    }

    /// Reload the config when its file changes. Without its directory,
    /// there is nothing to watch (and nothing to reload).
    fn watch_config(&mut self, core: &Core<Self>) {
        let Some(path) = self.reloader.path() else {
            return;
        };
        let watched =
            wl_shell_config::watch_file(&core.loop_handle, &path, |shell: &mut Shell<Self>| {
                shell.program.config_touched = true;
            });
        if let Err(e) = watched
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!("not watching {}: {e}", path.display());
        }
    }

    /// Reload the pins when their file changes. Its directory is created,
    /// so pins added later by `wl-dock pin` are seen.
    fn watch_pins(&mut self, core: &Core<Self>) {
        let Some(path) = self.pins_path.clone() else {
            return warn!("no state directory: pins are not kept");
        };
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            return warn!("{}: {e}", dir.display());
        }
        let watched =
            wl_shell_config::watch_file(&core.loop_handle, &path, |shell: &mut Shell<Self>| {
                shell.program.pins_touched = true;
            });
        if let Err(e) = watched {
            warn!("not watching {}: {e}", path.display());
        }
    }

    /// Follow whether the Trash is empty, once the config shows it. Its
    /// directories are created if missing (as any trash implementation
    /// does), so there is something to watch.
    fn watch_trash(&mut self, core: &Core<Self>) {
        if !self.config.trash.enabled || self.trash_watch.is_some() {
            return;
        }
        let Some(dir) = self.trash_dir.clone() else {
            return warn!("no data directory: the Trash isn't followed");
        };
        let info = dir.parent().map(|t| t.join("info"));
        for d in std::iter::once(dir.as_path()).chain(info.as_deref()) {
            if let Err(e) = std::fs::create_dir_all(d) {
                return warn!("{}: {e}", d.display());
            }
        }
        let watched =
            wl_shell_config::watch_dir(&core.loop_handle, &dir, |shell: &mut Shell<Self>| {
                shell.program.trash_touched = true;
            });
        match watched {
            Ok(token) => self.trash_watch = Some(token),
            Err(e) => warn!("not watching {}: {e}", dir.display()),
        }
        self.trash_touched = true;
    }

    // -- widgets -------------------------------------------------------------

    /// Start the plugin runtime; its events come through the loop.
    fn start_widgets(&mut self, core: &Core<Self>) {
        let (send, events) = channel::channel();
        let send = std::sync::Mutex::new(send);
        let started = self.widgets.start(move |e| {
            let _ = send.lock().map(|s| s.send(e));
        });
        if let Err(e) = started {
            return warn!("no widgets: {e}");
        }
        let watched =
            core.loop_handle
                .insert_source(events, |event, _, shell: &mut Shell<Self>| {
                    if let channel::Event::Msg(event) = event {
                        shell.program.plugin_event(&mut shell.core, event);
                    }
                });
        if let Err(e) = watched {
            warn!("no widgets: {e}");
        }
        self.configure_widgets(core);
    }

    /// Run the plugins the config asks for (and the standalone widgets
    /// pinned), and watch their files.
    fn configure_widgets(&mut self, core: &Core<Self>) {
        let dir = widgets::plugin_dir(self.reloader.path().as_deref());
        self.widget_pins = pins::widgets(&self.pins);
        let specs = widgets::specs(&self.config, dir.as_deref(), &self.widget_pins);
        if self.widgets.configure(specs) {
            self.show_widget_names();
            self.show_widget_icons();
        }
        // Their apps' names, as far as they are known.
        let apps: Vec<String> = (self.config.app.iter())
            .filter(|(_, a)| a.widget.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for app in &apps {
            self.share_app_names(app);
        }
        let paths = match self.config.widgets.enabled {
            true => widgets::watched(&self.config, dir.as_deref()),
            false => Vec::new(),
        };
        if paths == self.widgets.watching {
            return;
        }
        for token in self.plugin_watches.drain(..) {
            core.loop_handle.remove(token);
        }
        for path in &paths {
            let p = path.clone();
            let watched = wl_shell_config::watch_file(
                &core.loop_handle,
                path,
                move |shell: &mut Shell<Self>| shell.program.widgets.touched.push(p.clone()),
            );
            match watched {
                Ok(token) => self.plugin_watches.push(token),
                // No plugin folder: nothing to watch until it exists.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => warn!("not watching {}: {e}", path.display()),
            }
        }
        self.widgets.watching = paths;
    }

    /// Tell the widget on `app_id` (if it has one) the names its desktop
    /// entry gives it; ask the worker for the entry if it isn't known.
    fn share_app_names(&self, app_id: &str) {
        if !self.config.app(app_id).is_some_and(|a| a.widget.is_some()) {
            return;
        }
        match self.entries.get(app_id) {
            Some(entry) => {
                let names = widgets::app_info(&self.config, app_id, entry.as_ref());
                self.widgets.set_app(names);
            }
            None => {
                if let Some(assets) = &self.assets {
                    assets.entry(app_id);
                }
            }
        }
    }

    /// Something from the plugin runtime.
    fn plugin_event(&mut self, core: &mut Core<Self>, event: wl_shell_lua::Event) {
        let Effect {
            labels,
            icons,
            spans,
            clipboard,
            popup,
            choices,
            results,
            sources,
        } = self.widgets.event(event);
        if let Some((id, answer)) = choices {
            self.choices_arrived(id, answer);
        }
        if let Some(instance) = sources {
            self.sources_changed(core, &instance);
        }
        if let Some(key) = results
            && self.source_shown() == Some(key)
        {
            self.show_source(key, true);
        }
        if let Some((app, tree)) = popup
            && let Some((output, key)) = self.widget_target.clone()
            && key == app
        {
            if let Some(tree) = tree
                && let Some(dock) = self.dock_mut(&output)
            {
                dock.paint_widget(&app, tree)
            } else {
                // Nothing to show: the label shows instead, and hovering
                // on doesn't ask again.
                self.hide_widget(core);
                self.widget_declined = Some(app);
            }
        }
        if labels {
            self.show_widget_names();
        }
        if icons {
            self.show_widget_icons();
        }
        if spans {
            self.rebuild(core, None);
        }
        if let Some(text) = clipboard {
            let data = ([
                "text/plain;charset=utf-8",
                "text/plain",
                "UTF8_STRING",
                "STRING",
            ])
            .map(|mime| (mime.to_string(), text.clone().into_bytes()));
            if let Err(e) = core.set_selection(data.into()) {
                warn!("a widget's copy: {e}");
            }
        }
    }

    /// Give the docks the widgets' icons.
    fn show_widget_icons(&mut self) {
        for dock in &mut self.docks {
            dock.set_widget_icons(self.widgets.icons());
        }
    }

    /// The tile widgets draw their icons for: the first dock's, at the
    /// largest scale of any.
    fn sync_widget_tile(&mut self) {
        let base = self.docks.first().map(|d| {
            let mut base = d.tile_base();
            base.scale = (self.docks.iter())
                .map(|d| d.tile_base().scale)
                .max()
                .unwrap_or(1);
            base
        });
        self.widgets.set_tile(base);
    }

    /// Give the docks the widgets' labels.
    fn show_widget_names(&mut self) {
        for dock in &mut self.docks {
            dock.set_widget_names(self.widgets.labels());
        }
    }

    /// Look whether anything is in the Trash.
    fn check_trash(&mut self) {
        let full = self.trash_dir.as_deref().is_some_and(trash::full);
        if full != self.trash_full {
            self.trash_full = full;
            self.dirty = true;
        }
    }

    /// Run the streams the config names (restarting changed ones).
    fn use_streams(&mut self, core: &mut Core<Self>) {
        core.use_focus_stream(self.config.integration.mru_event_stream.as_deref());
        let handle = core.loop_handle.clone();
        (self.output_stream).configure(
            self.config.integration.output_event_stream.as_deref(),
            &handle,
        );
    }

    /// After every event loop dispatch: follow config, pins and theme
    /// changes, match the docks to the outputs, and rebuild the items
    /// once for however many events arrived.
    pub fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let touched = std::mem::take(&mut self.config_touched);
        if (touched || self.reloader.scheme_changed())
            && (self.reloader).refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.config_changed(core);
        }
        if std::mem::take(&mut self.pins_touched) {
            self.reload_pins();
        }
        let plugins = std::mem::take(&mut self.widgets.touched);
        if !plugins.is_empty() {
            self.configure_widgets(core);
            for path in &plugins {
                self.widgets.reload(path);
            }
        }
        if std::mem::take(&mut self.trash_touched) {
            self.check_trash();
        }
        if std::mem::take(&mut self.outputs_dirty) {
            self.sync_docks(core);
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.rebuild(core, None);
        }
        if std::mem::take(&mut self.trim) {
            // Draw now, so no dock shows a replaced buffer anymore.
            for dock in &mut self.docks {
                dock.render();
            }
            if let Some(launcher) = &mut self.launcher {
                launcher.render();
            }
            self.cache.borrow_mut().trim();
        }
        // The dock may show another target for a drag from outside (its
        // items changed, or it slid back in): accept to match.
        self.sync_accept(core);
        self.serve(core);
        self.launcher_after(core);
        self.keys_after(core);
    }

    // -- keyboard activation -----------------------------------------------------

    /// `--activate [n]`: start the mode (on item n, if any), or, while it
    /// is on, press n.
    fn activate_request(&mut self, core: &Core<Self>, n: Option<usize>) {
        // The launcher has the keyboard.
        if self.launcher.as_ref().is_some_and(|l| !l.closing()) {
            return;
        }
        if let Some(keys) = &mut self.keys {
            match (keys.output.is_some(), n) {
                (true, Some(n)) => self.keys_press(core, n, false),
                (false, Some(n)) => keys.first = Some(n),
                (_, None) => {}
            }
            return;
        }
        if self.docks.is_empty() {
            return;
        }
        let view = match ActivateView::new(core) {
            Ok(view) => view,
            Err(e) => return warn!("keyboard activation: {e}"),
        };
        self.close_popups(core);
        self.cancel_drags();
        self.keys = Some(Keys {
            view,
            mode: activate::Mode::default(),
            output: None,
            first: n,
            window: None,
            item: None,
            shown: false,
            timer: None,
        });
        // Should the compositor not say where it is, it takes the first
        // dock.
        let timer = core.timer(PLACE_TIMEOUT, |app: &mut Self, core| {
            let unplaced = app.keys.as_ref().is_some_and(|k| k.output.is_none());
            let first = app.docks.first().map(|d| d.output.clone());
            if let (true, Some(output)) = (unplaced, first) {
                app.attach_keys(core, &output);
            }
        });
        if let Err(e) = timer {
            warn!("{e}");
        }
    }

    /// The activate surface is on `output`: its dock (else the first)
    /// numbers its items and acts on the item asked for.
    fn attach_keys(&mut self, core: &Core<Self>, output: &WlOutput) {
        let Some(keys) = &mut self.keys else { return };
        if keys.output.is_some() {
            return;
        }
        let dock = (self.docks.iter())
            .find(|d| d.output == *output)
            .or(self.docks.first());
        let Some(output) = dock.map(|d| d.output.clone()) else {
            return self.end_keys(core);
        };
        keys.output = Some(output.clone());
        if let Some(n) = keys.first.take() {
            self.keys_press(core, n, false);
        }
        let Some(keys) = &mut self.keys else { return };
        keys.mode.start();
        // Shown once the modifier was held a moment: quick switches show
        // nothing.
        let delay = Duration::from_millis(self.config.activate.delay.into());
        if delay.is_zero() {
            self.keys_show(core);
        } else {
            let due = |app: &mut Self, core: &mut Core<Self>| {
                if let Some(keys) = &mut app.keys {
                    keys.timer = None;
                }
                app.keys_show(core);
            };
            match core.timer(delay, due) {
                Ok(token) => keys.timer = Some(token),
                Err(e) => {
                    warn!("{e}");
                    self.keys_show(core);
                }
            }
        }
        self.keys_due(core);
    }

    /// The delay passed (or a tap left something to use): the dock shows
    /// the mode, with the strip or popup of the item acted on.
    fn keys_show(&mut self, core: &Core<Self>) {
        let Some(keys) = &mut self.keys else { return };
        if let Some(token) = keys.timer.take() {
            core.loop_handle.remove(token);
        }
        if std::mem::replace(&mut keys.shown, true) {
            return;
        }
        let (open, item, window) = (keys.mode.open(), keys.item.clone(), keys.window.clone());
        let Some(output) = keys.output.clone() else {
            return;
        };
        let Some(dock) = self.dock_mut(&output) else {
            return;
        };
        dock.begin_keys();
        let Some(key) = item else { return };
        dock.point_at(core, &key);
        match open {
            activate::Open::Strip => {
                let opened = dock.open_strip(core, &key, None);
                if let Some(id) = window.filter(|_| opened) {
                    dock.strip_highlight(&id);
                }
                if !opened && let Some(keys) = &mut self.keys {
                    keys.mode.closed();
                }
            }
            activate::Open::Popup => {
                let item = dock.item(&key).cloned();
                if let Some(item) = item {
                    self.list_folder(&item, &output, None, Some(0));
                }
            }
            activate::Open::Window | activate::Open::Nothing => {}
        }
    }

    /// Press item `n` (from 1) of the mode's dock, `back` with Shift.
    fn keys_press(&mut self, core: &Core<Self>, n: usize, back: bool) {
        let Some(output) = self.keys.as_ref().and_then(|k| k.output.clone()) else {
            return;
        };
        let Some(dock) = self.docks.iter().find(|d| d.output == output) else {
            return;
        };
        let key = (activate::numbered(dock.items()))
            .get(n.wrapping_sub(1))
            .cloned();
        let item = key.as_deref().and_then(|k| dock.item(k)).cloned();
        let subject = (item.as_ref()).map_or(activate::Subject::Nothing, |i| {
            activate::Subject::of(i, dock.can_strip(i))
        });
        let Some(keys) = &mut self.keys else { return };
        let open = keys.mode.open();
        let press = keys.mode.press(n, subject, back);
        let shown = keys.shown;
        let (Some(key), Some(item)) = (key, item) else {
            return;
        };
        // A new item: magnified (once shown), and the next to act on.
        if !matches!(press, Press::Nothing | Press::Step { .. }) {
            self.close_popups(core);
            if let Some(keys) = &mut self.keys {
                keys.item = Some(key.clone());
                keys.window = None;
            }
            if shown && let Some(dock) = self.dock_mut(&output) {
                dock.point_at(core, &key);
            }
        }
        match press {
            Press::Nothing => {}
            Press::Step { back } => match open {
                activate::Open::Strip => self.keys_step(core, &output, &item, back),
                activate::Open::Popup => {
                    // Still being listed: it steps on once it opens.
                    let pending = (self.pending_folder.as_mut()).filter(|p| p.key == key);
                    match pending.and_then(|p| p.steps.as_mut()) {
                        Some(steps) if !back => *steps += 1,
                        Some(steps) => *steps = steps.saturating_sub(1),
                        None => {
                            if let Some(dock) = self.dock_mut(&output) {
                                dock.menu_step(back);
                            }
                        }
                    }
                }
                activate::Open::Window | activate::Open::Nothing => {}
            },
            Press::Select => {
                if let (Some(keys), Click::Activate(id)) = (&mut self.keys, item.click(&core.mru)) {
                    keys.window = Some(id);
                }
            }
            Press::Strip => {
                // On the app's previous window, as a click does.
                let ids: Vec<&str> = item.windows.iter().map(|w| w.id.as_str()).collect();
                let focused = (core.focus_stream.running())
                    .then(|| core.mru.first())
                    .flatten();
                let first = strip::first_highlight(&ids, &core.mru, focused);
                let window = ids.get(first).map(|id| id.to_string());
                if let Some(keys) = &mut self.keys {
                    keys.window = window.clone();
                }
                if shown {
                    let opened = self.dock_mut(&output).is_some_and(|dock| {
                        let opened = dock.open_strip(core, &key, None);
                        if let Some(id) = window.filter(|_| opened) {
                            dock.strip_highlight(&id);
                        }
                        opened
                    });
                    if !opened && let Some(keys) = &mut self.keys {
                        keys.mode.closed();
                    }
                }
            }
            Press::Launch => self.launch(core, &key),
            Press::Popup => {
                if shown {
                    self.list_folder(&item, &output, None, Some(0));
                }
            }
        }
        self.keys_due(core);
    }

    /// Pressing the strip's item again: its next window in MRU order (the
    /// previous one, `back`), from the one highlighted; in card order
    /// without focus history.
    fn keys_step(&mut self, core: &Core<Self>, output: &WlOutput, item: &Item, back: bool) {
        let mut order: Vec<&str> = item.windows.iter().map(|w| w.id.as_str()).collect();
        core.mru.sort(&mut order, |id| id);
        let lit = (self.docks.iter())
            .find(|d| d.output == *output)
            .and_then(DockView::strip_highlighted)
            .map(str::to_string);
        let Some(keys) = &mut self.keys else { return };
        let current = lit.or(keys.window.clone());
        let next = activate::step(&order, current.as_deref(), back).map(str::to_string);
        keys.window = next.clone();
        if let (Some(id), Some(dock)) = (next, self.dock_mut(output)) {
            dock.strip_highlight(&id);
        }
    }

    /// A key on the activate surface.
    fn keys_key(&mut self, core: &Core<Self>, event: &KeyEvent, repeat: bool) {
        let shift = core.input.modifiers.shift;
        let Some(key) = activate::Key::from(event.raw_code, event.keysym, shift) else {
            return;
        };
        if repeat && !key.repeats() {
            return;
        }
        let Some(keys) = &self.keys else { return };
        let open = keys.mode.open();
        let output = keys.output.clone();
        match key {
            activate::Key::Number { n, back } => self.keys_press(core, n, back),
            activate::Key::Escape => self.cancel_keys(core),
            activate::Key::Enter => {
                let release = keys.mode.enter();
                self.keys_release(core, release);
            }
            activate::Key::Arrow(arrow) => {
                let Some(dock) = output.and_then(|o| self.dock_mut(&o)) else {
                    return;
                };
                match (open, arrow) {
                    (activate::Open::Strip, _) => {
                        dock.strip_key(core, Key::Move(arrow));
                    }
                    (activate::Open::Popup, strip::Arrow::Up) => {
                        dock.menu_key(core, MenuKey::Up);
                    }
                    (activate::Open::Popup, strip::Arrow::Down) => {
                        dock.menu_key(core, MenuKey::Down);
                    }
                    _ => {}
                }
            }
        }
    }

    /// The modifiers changed on the activate surface.
    fn keys_modifiers(&mut self, core: &Core<Self>, modifiers: &Modifiers) {
        if let Some(keys) = &mut self.keys {
            keys.mode.modifiers(activate::Mods::from(modifiers));
        }
        self.keys_due(core);
    }

    /// Do what the mode does by itself now, if anything. A tap that left
    /// a strip or popup to use shows it at once.
    fn keys_due(&mut self, core: &Core<Self>) {
        if let Some(release) = self.keys.as_mut().and_then(|k| k.mode.due()) {
            return self.keys_release(core, release);
        }
        if self
            .keys
            .as_ref()
            .is_some_and(|k| k.mode.kept() && !k.shown)
        {
            self.keys_show(core);
        }
    }

    /// The mode ends: activate the picked window or the strip's
    /// highlighted one (the one stepped to, if the strip isn't shown),
    /// or open the popup's highlighted entry, or just end.
    fn keys_release(&mut self, core: &Core<Self>, release: Release) {
        let window = self.keys.as_ref().and_then(|k| k.window.clone());
        let output = self.keys.as_ref().and_then(|k| k.output.clone());
        let dock = output.and_then(|o| self.dock_mut(&o));
        let run = match (release, dock) {
            (Release::Window, _) => window.map(KeysRun::Activate),
            (Release::Activate, Some(dock)) if dock.strip_highlighted().is_some() => {
                dock.strip_key(core, Key::Activate).map(KeysRun::Activate)
            }
            (Release::Activate, _) => window.map(KeysRun::Activate),
            (Release::Open, Some(dock)) => (dock.menu_key(core, MenuKey::Pick))
                .map(|(app_id, action)| KeysRun::Menu(app_id, action)),
            _ => None,
        };
        if run.is_none() {
            self.close_popups(core);
        }
        self.end_keys(core);
        self.pending_keys.extend(run);
    }

    /// Escape, or the keyboard went: close what the mode opened, and end
    /// it.
    fn cancel_keys(&mut self, core: &Core<Self>) {
        if self.keys.is_some() {
            self.close_popups(core);
            self.end_keys(core);
        }
    }

    /// End the mode: the badges go, and so does its surface, which gives
    /// the keyboard back.
    fn end_keys(&mut self, core: &Core<Self>) {
        let Some(keys) = self.keys.take() else { return };
        if let Some(token) = keys.timer {
            core.loop_handle.remove(token);
        }
        if let Some(output) = &keys.output
            && let Some(dock) = self.dock_mut(output)
        {
            dock.end_keys(core);
        }
        // A popup still being listed isn't wanted anymore.
        if self
            .pending_folder
            .as_ref()
            .is_some_and(|p| p.steps.is_some())
        {
            self.pending_folder = None;
        }
    }

    /// After a dispatch: run what the mode activated or opened, now that
    /// its surface is gone and the compositor gave the keyboard back.
    fn keys_after(&mut self, core: &Core<Self>) {
        if self.pending_keys.is_empty() {
            return;
        }
        if let Err(e) = core.conn.roundtrip() {
            warn!("{e}");
        }
        for run in std::mem::take(&mut self.pending_keys) {
            match run {
                KeysRun::Activate(id) => self.activate(core, &id),
                KeysRun::Menu(app_id, action) => self.menu_action(core, &app_id, action),
            }
        }
    }

    /// A row of a menu was picked: do it now, or once keyboard activation
    /// gave the keyboard back.
    fn menu_pick(&mut self, core: &Core<Self>, app_id: String, action: Action) {
        if self.keys.is_some() {
            self.end_keys(core);
            self.pending_keys.push(KeysRun::Menu(app_id, action));
            return;
        }
        self.menu_action(core, &app_id, action);
    }

    // -- the launcher ------------------------------------------------------------

    /// Answer what came over the socket.
    fn serve(&mut self, core: &mut Core<Self>) {
        let Some(listener) = &self.listener else {
            return;
        };
        for event in listener.take() {
            match event {
                SocketEvent::Request { client, req } => match req {
                    DockRequest::Nothing => client.answer(Reply::Error("nothing asked".into())),
                    DockRequest::Launcher => self.toggle_launcher(core, client),
                    DockRequest::Activate(n) => {
                        client.answer(Reply::Done);
                        self.activate_request(core, n.map(usize::from));
                    }
                },
                SocketEvent::Gone(id) => {
                    // Closing the client cancels.
                    if self.launcher_client.as_ref().is_some_and(|c| c.id() == id) {
                        self.launcher_client = None;
                        self.close_launcher(core, None);
                    }
                }
            }
        }
    }

    /// `--launcher`: open the launcher, or close it if it is open.
    fn toggle_launcher(&mut self, core: &mut Core<Self>, client: Client) {
        self.cancel_keys(core);
        if self.launcher.as_ref().is_some_and(|l| !l.closing()) {
            self.cancel_launcher(core);
            return client.answer(Reply::Cancelled);
        }
        // One closing still goes at once.
        self.drop_launcher();
        if !self.docks.iter().any(DockView::takes_launcher) {
            let e = "the launcher needs a dock at the bottom of the screen";
            return client.answer(Reply::Error(e.into()));
        }
        let Some(assets) = self.assets.clone() else {
            return client.answer(Reply::Error("not started yet".into()));
        };
        let response = self.config.appearance.response as f64;
        match LauncherView::new(core, assets, self.cache.clone(), response) {
            Ok(launcher) => {
                self.close_popups(core);
                self.cancel_drags();
                self.launcher = Some(launcher);
                self.launcher_client = Some(client);
                // Should the compositor not say where it is, it takes the
                // first bottom dock.
                let timer = core.timer(PLACE_TIMEOUT, |app: &mut Self, core| {
                    let unplaced = app.launcher.as_ref().is_some_and(|l| l.output().is_none());
                    let first = app.docks.iter().find(|d| d.takes_launcher());
                    if let (true, Some(output)) = (unplaced, first.map(|d| d.output.clone())) {
                        app.attach_launcher(core, &output);
                    }
                });
                if let Err(e) = timer {
                    warn!("{e}");
                }
            }
            Err(e) => client.answer(Reply::Error(format!("cannot show the launcher: {e}"))),
        }
    }

    /// The launcher's surface is on `output`: the dock there takes it
    /// over, if it is at the bottom; else the launcher closes.
    fn attach_launcher(&mut self, core: &Core<Self>, output: &WlOutput) {
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.output().is_some() || launcher.closing() {
            return;
        }
        let Some(dock) =
            (self.docks.iter_mut()).find(|d| d.output == *output && d.takes_launcher())
        else {
            self.drop_launcher();
            if let Some(client) = self.launcher_client.take() {
                let e = "the launcher needs a dock at the bottom of this screen";
                client.answer(Reply::Error(e.into()));
            }
            return;
        };
        let style = LauncherStyle::new(&self.config, &dock.style().layout);
        let scale = (core.output_state.info(output)).map_or(1, |i| i.scale_factor.max(1));
        launcher.attach(output.clone(), scale, style, dock.handover());
        dock.begin_launcher(core);
        self.rank_launcher(core);
    }

    /// Rank the launcher's matches for its query: apps, files, or the
    /// level's choices.
    fn rank_launcher(&mut self, core: &Core<Self>) {
        match self.launcher.as_ref().map(|l| l.query().scope()) {
            Some(Scope::Apps) => self.rank_apps(false),
            Some(Scope::Files) => self.rank_files(core),
            Some(Scope::Windows) => self.rank_windows(core, None, false),
            Some(Scope::Bang | Scope::Web) => self.rank_web(),
            Some(Scope::Actions) => self.rank_choices(),
            Some(Scope::Source(key)) => self.rank_source(core, key),
            None => {}
        }
        // The query can change without changing the file matches or status
        // (for example, a trailing space). Refresh it independently.
        if let Some(launcher) = &mut self.launcher {
            launcher.sync_texts();
            launcher.kick();
        }
    }

    // -- plugins' sources --------------------------------------------------------

    /// The launcher's triggers: files', windows', then the plugins'
    /// sources' (a character taken is skipped).
    fn triggers(&mut self) -> Triggers {
        let config = &self.config.launcher;
        let mut triggers = Triggers::new(config.files.trigger, config.windows.trigger);
        for s in self.widgets.launcher_sources() {
            triggers.add(s.decl.trigger, Scope::Source(s.key));
        }
        triggers
    }

    /// The source the launcher searches (not in a level, not closing).
    fn source_shown(&self) -> Option<SourceKey> {
        let l = (self.launcher.as_ref()).filter(|l| !l.closing() && l.depth() == 0)?;
        match l.query().scope() {
            Scope::Source(key) => Some(key),
            _ => None,
        }
    }

    /// The query of the source `key`'s scope changed (or it was
    /// entered): a search asks the plugin once the typing settles, the
    /// results shown staying until its own come; items are asked for if
    /// they aren't known, and ranked for the query.
    fn rank_source(&mut self, core: &Core<Self>, key: SourceKey) {
        let Some(entry) = self.widgets.source(key) else {
            return self.show_source(key, false);
        };
        if entry.decl.search {
            self.search_source(core, key);
            return self.show_source(key, true);
        }
        self.widgets.ask_items(key);
        if let Some(l) = &mut self.launcher {
            // Typing ranks anew: the best comes to the front again.
            l.unroll();
        }
        self.show_source(key, false);
    }

    /// Ask the source `key` to search for the launcher's query, after
    /// `SEARCH_DEBOUNCE` (at once for an empty one) unless the query
    /// changes before.
    fn search_source(&mut self, core: &Core<Self>, key: SourceKey) {
        let Some(l) = &self.launcher else { return };
        let query = l.query().text().to_string();
        let generation = self.widgets.begin_search(key, &query);
        let delay = match query.trim().is_empty() {
            true => Duration::ZERO,
            false => SEARCH_DEBOUNCE,
        };
        let timer = core.timer(delay, move |app: &mut Self, _| {
            app.widgets.send_search(generation);
        });
        if let Err(e) = timer {
            warn!("{e}");
        }
    }

    /// `instance`'s sources changed (it loaded, failed, or invalidated
    /// its items): the one searched asks again, keeping what it shows
    /// until the answer.
    fn sources_changed(&mut self, core: &Core<Self>, instance: &str) {
        let Some(key) = self.source_shown() else {
            return;
        };
        if self.widgets.source_instance(key) != Some(instance) {
            return;
        }
        match self.widgets.source(key) {
            Some(entry) if entry.decl.search => self.search_source(core, key),
            Some(_) => {
                self.widgets.ask_items(key);
                self.show_source(key, true);
            }
            // Gone: nothing to show.
            None => self.show_source(key, false),
        }
    }

    /// Put the source `key`'s items (ranked for the query) or search
    /// results (in the plugin's order) on the drum; `streamed`: they
    /// arrived, so a rolled-to selection stays.
    fn show_source(&mut self, key: SourceKey, streamed: bool) {
        let entry = self.widgets.source(key);
        let Self {
            launcher: Some(launcher),
            search,
            widgets,
            apps,
            config,
            ..
        } = self
        else {
            return;
        };
        let slots = (config.launcher.matches as usize).max(1);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let query = launcher.query().text().to_string();
        let typed = !query.trim().is_empty();
        let Some(entry) = entry else {
            launcher.set_source(SourceLook {
                status: Some(SOURCE_GONE.into()),
                ..SourceLook::default()
            });
            return launcher.set_matches(Vec::new());
        };
        let icon = entry.decl.icon.as_deref();
        let item = |i: &wl_shell_lua::Item, matched, detail| {
            item_match(i, matched, detail, icon, apps, home.as_deref())
        };
        let (matches, status): (Vec<Match>, Option<&str>) = if entry.decl.search {
            let results = widgets.search_results(key).unwrap_or_default();
            let matches = (results.iter().take(slots))
                .map(|i| item(i, Vec::new(), Vec::new()))
                .collect();
            (matches, None)
        } else {
            match widgets.source_items(key) {
                None => {
                    let loading = widgets.items_loading(key);
                    (Vec::new(), loading.then_some(actions::LOADING))
                }
                Some(items) => {
                    let fields: Vec<(&str, &str)> = (items.iter())
                        .map(|i| (i.title.as_str(), i.subtitle.as_deref().unwrap_or("")))
                        .collect();
                    let order = search.rank_windows(&fields, &query);
                    let matches: Vec<Match> = (order.into_iter().take(slots))
                        .map(|n| &items[n])
                        .map(|i| {
                            let matched = search.highlight(&i.title, &query);
                            let detail = (i.subtitle.as_deref())
                                .map_or_else(Vec::new, |s| search.highlight(s, &query));
                            item(i, matched, detail)
                        })
                        .collect();
                    let none = matches.is_empty() && (typed || items.is_empty());
                    (matches, none.then_some(NO_MATCHES))
                }
            }
        };
        launcher.set_source(SourceLook {
            name: entry.decl.name.clone(),
            icons: entry.decl.icon.iter().cloned().collect(),
            status: status.map(str::to_string),
        });
        match streamed {
            true => launcher.stream_matches(matches),
            false => launcher.set_matches(matches),
        }
    }

    /// Rank the apps for the launcher's query; the app it meant (picked
    /// for it before) first. The web search fallback comes with them
    /// (`add_fallback`). `streamed`: suggestions for it arrived, which
    /// keep a rolled-to selection.
    fn rank_apps(&mut self, streamed: bool) {
        let Self {
            launcher: Some(launcher),
            search,
            apps,
            history,
            config,
            ..
        } = self
        else {
            return;
        };
        let now = history::now();
        let learn = config.launcher.history;
        let boost = |a: &IndexedApp| match learn {
            true => history.boost(&a.id, now),
            false => 0.0,
        };
        let query = launcher.query().text().to_string();
        let mut ranked = search.rank(apps, &query, &boost);
        if let Some(meant) = history.meant(&query).filter(|_| learn) {
            history::meant_first(&mut ranked, |i| apps[i].id == meant);
        }
        let slots = (config.launcher.matches as usize).max(1);
        let mut matches: Vec<Match> = (ranked.into_iter().take(slots))
            .map(|i| &apps[i])
            .map(|a| Match {
                id: a.id.clone(),
                name: a.name.clone(),
                icon: a.entry.icon.clone(),
                app: None,
                named: Vec::new(),
                matched: search.highlight(&a.name, &query),
                file: None,
                window: None,
                detail: None,
            })
            .collect();
        let place = config
            .launcher
            .web
            .fallback_place(Scope::Apps, &query, matches.len());
        self.suggest_fallback(place, &query);
        self.add_fallback(Scope::Apps, &query, place, &mut matches);
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        match streamed {
            true => launcher.stream_matches(matches),
            false => launcher.set_matches(matches),
        }
    }

    /// Filter the level's choices by the launcher's query: in their own
    /// order with an empty one.
    fn rank_choices(&mut self) {
        let Self {
            launcher: Some(launcher),
            search,
            apps,
            ..
        } = self
        else {
            return;
        };
        let Some(level) = launcher.level() else {
            return;
        };
        let query = launcher.query().text().to_string();
        let names: Vec<&str> = level.choices.iter().map(|c| c.name.as_str()).collect();
        let ranked = search.rank_names(&names, &query);
        let matches = (ranked.into_iter())
            .map(|i| &level.choices[i])
            .map(|c| {
                let (icon, app, named) = match &c.icon {
                    Icon::App(id) | Icon::Window { app: id, .. } => {
                        let entry = apps.iter().find(|a| a.id == *id).map(|a| &a.entry);
                        (
                            entry.and_then(|e| e.icon.clone()),
                            Some(id.clone()),
                            Vec::new(),
                        )
                    }
                    Icon::Names(names) => (None, None, names.clone()),
                    // The icon key falls back to the app's own.
                    Icon::Entry { app, icon } => (icon.clone(), Some(app.clone()), Vec::new()),
                };
                let window = match &c.icon {
                    Icon::Window { window, .. } => Some(window.clone()),
                    _ => None,
                };
                Match {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    icon,
                    app,
                    named,
                    matched: search.highlight(&c.name, &query),
                    file: None,
                    window,
                    detail: None,
                }
            })
            .collect();
        launcher.set_matches(matches);
    }

    /// Search files for the launcher's query: start the search if it
    /// isn't running, and match the query. The matches stream in as they
    /// are found (`files_frame`).
    fn rank_files(&mut self, core: &Core<Self>) {
        if self.files.is_none() {
            self.start_files(core);
        }
        let (Some(files), Some(launcher)) = (&mut self.files, &mut self.launcher) else {
            return;
        };
        // Typing ranks anew: the best comes to the front again.
        launcher.unroll();
        files.search.set_query(launcher.query().text());
        files.search.tick();
        self.show_files();
    }

    /// Start walking `launcher.files.root` for the launcher.
    fn start_files(&mut self, core: &Core<Self>) {
        let config = &self.config.launcher.files;
        let settings = filesearch::Settings {
            root: config.root(),
            exclude: config.exclude.clone(),
            hidden: config.hidden,
            gitignore: config.gitignore,
            max: config.max as usize,
        };
        let (ping, source) = match ping::make_ping() {
            Ok(ping) => ping,
            Err(e) => return warn!("cannot search files: {e}"),
        };
        let token = (core.loop_handle).insert_source(source, |_, _, shell: &mut Shell<Self>| {
            // A frame takes the news in.
            if let Some(launcher) = &mut shell.program.launcher {
                launcher.kick();
            }
        });
        let token = match token {
            Ok(token) => token,
            Err(e) => return warn!("cannot search files: {}", e.error),
        };
        let news = Arc::new(AtomicBool::new(false));
        let flag = news.clone();
        // Called for every entry found: only the first news pings.
        let notify = Arc::new(move || {
            // Reading first keeps the flag's cache line shared while set.
            if !flag.load(Ordering::Acquire) && !flag.swap(true, Ordering::AcqRel) {
                ping.ping();
            }
        });
        match FileSearch::start(&settings, notify) {
            Ok(search) => {
                self.files = Some(Files {
                    search,
                    news,
                    ping: token,
                    handle: core.loop_handle.clone(),
                })
            }
            Err(e) => {
                core.loop_handle.remove(token);
                warn!("{e}");
            }
        }
    }

    /// A launcher frame: take in what the file search found since the
    /// last one, without waiting for it.
    fn files_frame(&mut self) {
        let Some(files) = &mut self.files else { return };
        if !files.news.swap(false, Ordering::AcqRel) {
            return;
        }
        if (self.launcher.as_ref()).is_some_and(|l| l.query().scope() == Scope::Files) {
            files.search.tick();
            self.show_files();
        }
    }

    /// Put the file search's best matches on the launcher's drum (once
    /// they are the query's), and say how far it got.
    fn show_files(&mut self) {
        let (Some(files), Some(launcher)) = (&mut self.files, &mut self.launcher) else {
            return;
        };
        if launcher.query().scope() != Scope::Files {
            return;
        }
        let search = &mut files.search;
        let root = search.root().to_path_buf();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let empty = launcher.query().text().trim().is_empty();
        let current = search.current();
        if empty {
            launcher.stream_matches(Vec::new());
        } else if current {
            let slots = (self.config.launcher.matches as usize).max(1);
            let kept = launcher.kept().map(str::to_string);
            let matches = (search.top(slots, kept.as_deref()).into_iter())
                .map(|found| file_match(search, &root, home.as_deref(), found))
                .collect();
            launcher.stream_matches(matches);
        }
        let none = !empty && current && search.settled() && search.matched() == 0;
        let progress = match none {
            true => "No files".to_string(),
            // Still walking: an ellipsis.
            false => {
                let more = if search.walked() { "" } else { "…" };
                format!("{} items{more}", thousands(search.seen()))
            }
        };
        launcher.set_files_status(FilesStatus {
            progress: Some(progress),
        });
    }

    // -- window search -----------------------------------------------------------

    /// Rank the windows for the launcher's window search: the open ones
    /// (not `closed`, which the list still knows), by recency, matched by
    /// title and app name. `streamed`: the windows changed, not the query,
    /// so a rolled-to window stays in front while it exists.
    fn rank_windows(
        &mut self,
        core: &Core<Self>,
        closed: Option<&ExtForeignToplevelHandleV1>,
        streamed: bool,
    ) {
        let Self {
            launcher: Some(launcher),
            search,
            apps,
            entries,
            config,
            ..
        } = self
        else {
            return;
        };
        if launcher.closing() || launcher.query().scope() != Scope::Windows {
            return;
        }
        let open: Vec<_> = (core.toplevels().into_iter())
            .filter(|(h, t)| Some(h) != closed && !t.identifier.is_empty())
            .collect();
        let mut windows: Vec<windowsearch::Window> = (open.iter())
            .map(|(_, t)| windowsearch::Window {
                id: t.identifier.clone(),
                app_id: t.app_id.clone(),
                app: app_name(apps, entries, &t.app_id),
                title: t.title.clone(),
            })
            .collect();
        windowsearch::recent(&mut windows, &core.mru);
        // Only the windows on the drum hold a capture (launcherview.rs).
        let previews = config.windows.previews;
        let candidates = match previews {
            Previews::Off => Vec::new(),
            _ => open.into_iter().map(|(h, t)| (t.identifier, h)).collect(),
        };
        launcher.set_candidates(candidates, previews == Previews::Live);
        let focused = core
            .focus_stream
            .running()
            .then(|| core.mru.first())
            .flatten();
        let query = launcher.query().text().to_string();
        let order = windowsearch::ranked(search, &windows, &query, focused);
        let slots = (config.launcher.matches as usize).max(1);
        let matches: Vec<Match> = (order.into_iter().take(slots))
            .map(|i| window_match(search, apps, entries, &windows[i], &query))
            .collect();
        match streamed {
            true => launcher.stream_matches(matches),
            false => launcher.set_matches(matches),
        }
    }

    /// The window a launcher match is, with its app's rows (Tab).
    fn window_subject(&self, core: &Core<Self>, m: &Match) -> Option<actions::WindowSubject> {
        let id = m.window.clone()?;
        let t = Self::toplevels(core, None)
            .into_iter()
            .find(|t| t.identifier == id)?;
        let app = app_of(&self.apps, &self.entries, &t.app_id)
            .map(|app| self.app_subject(core, app, &t.app_id));
        Some(actions::WindowSubject {
            id,
            app_name: app_name(&self.apps, &self.entries, &t.app_id),
            title: t.title,
            app_id: t.app_id,
            app,
        })
    }

    /// The app `app` (known to the dock as `app_id`) for the launcher's
    /// actions, as the dock's menu has it (`menu_groups`).
    fn app_subject(&self, core: &Core<Self>, app: &IndexedApp, app_id: &str) -> actions::App {
        let config = self.config.app(app_id);
        actions::App {
            id: app.id.clone(),
            name: app.name.clone(),
            desktop: match config.is_none_or(|a| a.desktop_actions) {
                true => app.entry.actions.clone(),
                false => Vec::new(),
            },
            commands: config.map_or_else(Vec::new, |a| a.actions.clone()),
            windows: Self::launcher_windows(core, app_id),
            launchable: self.launch_command(app_id, Some(&app.entry)).is_ok(),
        }
    }

    /// Activate the window `window` of the app `app_id` (as the
    /// compositor reports it) once the launcher gave up the keyboard; its
    /// icon flies to the app's item if this dock has one. Without
    /// `activate-command` there is nothing to do but to say so.
    fn launcher_window(&mut self, core: &Core<Self>, app_id: String, window: String) {
        if self.config.integration.activate_command.is_none() {
            return warn!("set activate-command to activate windows from the launcher");
        }
        let output = self.launcher.as_ref().and_then(|l| l.output().cloned());
        let on_dock = (self.docks.iter())
            .any(|d| Some(&d.output) == output.as_ref() && d.item(&app_id).is_some());
        self.close_launcher(core, on_dock.then_some(app_id));
        self.pending_window = Some(window);
    }

    fn start_web_worker(&mut self, core: &Core<Self>) {
        let (send, source) = channel::channel();
        if let Err(e) =
            core.loop_handle
                .insert_source(source, |event, _, shell: &mut Shell<Self>| {
                    if let channel::Event::Msg(answer) = event {
                        let app = &mut shell.program;
                        if app.web_session.as_mut().is_some_and(|s| s.accept(answer)) {
                            app.web_answer();
                        }
                    }
                })
        {
            return warn!("cannot receive web suggestions: {}", e.error);
        }
        match websuggest::Worker::start(move |answer| {
            let _ = send.send(answer);
        }) {
            Ok(worker) => self.web_worker = Some(worker),
            Err(e) => warn!("cannot start web suggestions: {e}"),
        }
    }

    fn load_browser_preferences(&mut self) {
        let desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let desktops: Vec<_> = desktops
            .split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        self.mime_apps = wl_shell_render::mimeapps::MimeApps::load(&desktops);
    }

    fn browser(&self) -> Option<&IndexedApp> {
        let i = self
            .mime_apps
            .default_app(&["x-scheme-handler/https".into()], &self.apps)?;
        self.apps.get(i)
    }

    fn engine_match(
        &self,
        engine: &Engine,
        id: String,
        name: String,
        matched: Vec<std::ops::Range<usize>>,
    ) -> Match {
        let mut named = Vec::new();
        if let Some(icon) = engine
            .icon
            .as_ref()
            .filter(|s| !s.trim().is_empty())
            .cloned()
        {
            named.push(icon);
        }
        if let Some(icon) = self.browser().and_then(|a| a.entry.icon.clone()) {
            named.push(icon);
        }
        named.push("web-browser".into());
        Match {
            id,
            name,
            icon: None,
            app: None,
            named,
            matched,
            file: None,
            window: None,
            detail: None,
        }
    }

    fn rank_web(&mut self) {
        let Some(l) = &self.launcher else { return };
        let web = &self.config.launcher.web;
        let engine = l
            .query()
            .engine()
            .and_then(|key| web.engine(key))
            .filter(|_| web.suggest);
        if let Some(worker) = &self.web_worker {
            let session = self.web_session.get_or_insert_with(|| {
                self.web_generation += 1;
                websuggest::Session::new(self.web_generation)
            });
            session.query(worker, engine, l.query().text());
        }
        self.show_web(false);
    }

    /// Suggestions stream onto the existing ring; an edit instead puts
    /// the typed query back in front.
    fn show_web(&mut self, streamed: bool) {
        let Some(l) = self.launcher.as_ref().filter(|l| !l.closing()) else {
            return;
        };
        let web = &self.config.launcher.web;
        let slots = (self.config.launcher.matches as usize).max(1);
        let query = l.query();
        let engine = query.engine().and_then(|key| web.engine(key));
        let matches = match query.scope() {
            Scope::Bang => {
                let mut matches: Vec<Match> = web
                    .ranked(&mut self.search, query.text())
                    .into_iter()
                    .take(slots)
                    .map(|e| (e, self.search.highlight(&e.name, query.text())))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|(e, matched)| {
                        self.engine_match(e, e.keyword.clone(), e.name.clone(), matched)
                    })
                    .collect();
                // No engine for the bang: the fallback searches it whole.
                let place = web.fallback_place(Scope::Bang, query.text(), matches.len());
                self.add_fallback(Scope::Bang, query.text(), place, &mut matches);
                matches
            }
            Scope::Web => {
                if let Some(e) = engine {
                    let text = query.text();
                    let mut matches = vec![Match {
                        id: engines::TYPED.into(),
                        name: text.into(),
                        icon: None,
                        app: None,
                        named: crate::launcherview::SEARCH_ICONS
                            .iter()
                            .map(|s| s.to_string())
                            .collect(),
                        matched: Vec::new(),
                        file: None,
                        window: None,
                        detail: None,
                    }];
                    matches.extend(self.suggestions(e, text, slots - 1));
                    matches
                } else {
                    Vec::new()
                }
            }
            _ => return,
        };
        let l = self.launcher.as_mut().unwrap();
        if streamed {
            l.stream_matches(matches);
        } else {
            l.set_matches(matches);
        }
    }

    /// The suggestions in the session's answer for `text` as matches of
    /// `engine`, at most `take`.
    fn suggestions(&self, engine: &Engine, text: &str, take: usize) -> Vec<Match> {
        let Some(session) = &self.web_session else {
            return Vec::new();
        };
        let typed = text.trim();
        (session.values().iter().take(take))
            .map(|s| {
                let matched = (s.starts_with(typed).then_some(0..typed.len()))
                    .into_iter()
                    .collect();
                let id = format!("{}{s}", engines::SUGGESTION);
                self.engine_match(engine, id, s.clone(), matched)
            })
            .collect()
    }

    /// Add the web search fallback to the `matches` of a `scope` query
    /// of `text`, where `place` says it goes (`Web::fallback_place`): in
    /// front when nothing matched, with the engine's suggestions behind
    /// it if asked for; or in the last slot, behind the matches.
    fn add_fallback(&self, scope: Scope, text: &str, place: Place, matches: &mut Vec<Match>) {
        let web = &self.config.launcher.web;
        let Some(engine) = web.fallback_engine().filter(|_| place != Place::Off) else {
            return;
        };
        let slots = (self.config.launcher.matches as usize).max(1);
        let name = format!("Search {}", engine.name);
        let fallback = self.engine_match(engine, engines::FALLBACK.into(), name, Vec::new());
        match place {
            Place::Off => {}
            Place::Front => {
                matches.push(fallback);
                if web.fallback_suggester(scope, place).is_some() {
                    matches.extend(self.suggestions(engine, text, slots - 1));
                }
            }
            // With one slot there is room for nothing else: Alt+Enter.
            Place::Last if slots > 1 => {
                matches.truncate(slots - 1);
                matches.push(fallback);
            }
            Place::Last => {}
        }
    }

    /// Ask the fallback's engine for suggestions for the app query
    /// `text` if `place` and the config want them; else forget any asked.
    fn suggest_fallback(&mut self, place: Place, text: &str) {
        let Self {
            config,
            web_worker: Some(worker),
            web_session,
            web_generation,
            ..
        } = self
        else {
            return;
        };
        let engine = config.launcher.web.fallback_suggester(Scope::Apps, place);
        if engine.is_none() && web_session.is_none() {
            return;
        }
        let session = web_session.get_or_insert_with(|| {
            *web_generation += 1;
            websuggest::Session::new(*web_generation)
        });
        session.query(worker, engine, text);
    }

    /// Suggestions arrived for the query: show them.
    fn web_answer(&mut self) {
        match self.launcher.as_ref().map(|l| l.query().scope()) {
            Some(Scope::Apps) => self.rank_apps(true),
            _ => self.show_web(true),
        }
    }

    /// Search the web for `query` with the engine `keyword`: once the
    /// launcher gave up the keyboard, the browser opens it.
    fn open_search(&mut self, core: &Core<Self>, keyword: &str, query: &str) {
        let web = &self.config.launcher.web;
        let Some(e) = web.engine(keyword) else {
            return;
        };
        let url = engines::url(&e.url, query);
        if engines::http_url(&url).is_none() {
            return;
        }
        let command = spawn::fill(&web.command, &[("url", &spawn::quote(&url))]);
        let output = self.launcher.as_ref().and_then(|l| l.output());
        let browser = self
            .browser()
            .map(|a| self.dock_app_id(core, a))
            .filter(|id| {
                (self.docks.iter())
                    .any(|dock| Some(&dock.output) == output && dock.item(id).is_some())
            });
        self.close_launcher(core, browser);
        self.pending_url = Some((command, url));
    }

    /// Alt+Enter: search the web for the app query with the fallback
    /// engine, whatever is in front.
    fn launcher_search(&mut self, core: &Core<Self>) {
        let Some(l) = &self.launcher else { return };
        let text = l.query().text();
        let windows = l.query().scope() == Scope::Windows;
        if !(windows || l.query().scope() == Scope::Apps) || text.trim().is_empty() {
            return;
        }
        let web = &self.config.launcher.web;
        let Some(keyword) = web.fallback_engine().map(|e| e.keyword.clone()) else {
            return;
        };
        let text = text.to_owned();
        self.open_search(core, &keyword, &text);
    }

    fn edit_launcher(&mut self, core: &Core<Self>, edit: launcher::Key, pasted: bool) {
        // Only the first character of an app query can be a trigger.
        let first = (self.launcher.as_ref())
            .is_some_and(|l| l.query().scope() == Scope::Apps && l.query().is_empty());
        let triggers = match first {
            true => self.triggers(),
            false => Triggers::default(),
        };
        let Some(l) = &mut self.launcher else { return };
        let scope = l.query().scope();
        let config = &self.config.launcher;
        let bang = config
            .web
            .bang_trigger
            .filter(|_| !config.web.engines.is_empty());
        let changed = match &edit {
            launcher::Key::Text(text) if l.query_mut().start_bang(text, bang) => {
                if pasted && let Some((keyword, text)) = config.web.paste_bang(l.query().text()) {
                    let (keyword, text) = (keyword.to_owned(), text.to_owned());
                    l.query_mut().edit(&launcher::Key::Clear, None);
                    l.query_mut()
                        .edit(&launcher::Key::Text(keyword.clone()), None);
                    self.show_web(false);
                    if let Some(l) = &mut self.launcher {
                        l.enter_web(keyword, &text);
                    }
                }
                true
            }
            _ => l.query_mut().edit_with(&edit, &triggers),
        };
        if !changed {
            return;
        }
        if let Some(l) = &mut self.launcher
            && scope != l.query().scope()
        {
            // Nothing cross-fades into a search of its own.
            if matches!(
                l.query().scope(),
                Scope::Files | Scope::Windows | Scope::Source(_)
            ) {
                l.set_matches(Vec::new());
            }
            if scope == Scope::Windows {
                l.clear_candidates();
            }
            // Out of a source: its search is over.
            if matches!(scope, Scope::Source(_)) {
                self.widgets.end_search();
            }
        }
        if self
            .launcher
            .as_ref()
            .is_some_and(|l| !matches!(l.query().scope(), Scope::Web | Scope::Bang | Scope::Apps))
            && let (Some(session), Some(worker)) = (&mut self.web_session, &self.web_worker)
        {
            session.query(worker, None, "");
        }
        self.rank_launcher(core);
    }

    fn clear_web(&mut self) {
        if self.web_session.take().is_some()
            && let Some(worker) = &self.web_worker
        {
            worker.clear();
        }
    }

    fn paste_launcher(&mut self, core: &mut Core<Self>) {
        let Some(l) = &self.launcher else { return };
        let surface = l.surface().clone();
        let offered = core.selection_mime_types();
        let Some(mime) = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
            .into_iter()
            .find(|mime| offered.iter().any(|s| s == mime))
        else {
            return;
        };
        let _ = core.read_selection(mime, move |app, core, bytes| {
            if !app
                .launcher
                .as_ref()
                .is_some_and(|l| l.surface() == &surface && !l.closing())
            {
                return;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                return;
            };
            let text: String = text
                .chars()
                .filter_map(|c| {
                    if c.is_whitespace() {
                        Some(if c.is_control() { ' ' } else { c })
                    } else if c.is_control() {
                        None
                    } else {
                        Some(c)
                    }
                })
                .take(4096)
                .collect();
            if !text.is_empty() {
                app.edit_launcher(core, launcher::Key::Text(text), true);
            }
        });
    }

    /// A key on the launcher.
    fn launcher_key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let mods = core.input.modifiers;
        if mods.ctrl
            && !mods.alt
            && !mods.logo
            && event.keysym == smithay_client_toolkit::seat::keyboard::Keysym::v
        {
            if !repeat {
                self.paste_launcher(core);
            }
            return;
        }
        let Some(key) = launcher::Key::from(event.keysym, event.utf8.as_deref(), &mods) else {
            return;
        };
        if repeat && !key.repeats() {
            return;
        }
        let Some(l) = &mut self.launcher else { return };
        match launcher::act(key, l.depth(), l.query()) {
            Act::Close => self.cancel_launcher(core),
            Act::Clear => self.edit_launcher(core, launcher::Key::Clear, false),
            Act::Roll(by) => l.roll(by),
            Act::Open { .. } if l.level().is_some() => self.launcher_run(core, None),
            Act::Open { other } => self.launcher_pick(core, None, other),
            Act::Search => self.launcher_search(core),
            Act::Into => self.launcher_into(core, None),
            Act::Out => {
                let to = l.depth().saturating_sub(1);
                self.launcher_back(core, to);
            }
            Act::Edit(edit) => self.edit_launcher(core, edit, false),
        }
    }

    /// Go back out of the launcher's levels until `depth` deep, each
    /// restoring what it showed.
    fn launcher_back(&mut self, core: &Core<Self>, depth: usize) {
        let Some(l) = &mut self.launcher else { return };
        let mut left = false;
        while l.depth() > depth && l.leave() {
            left = true;
        }
        if left && l.depth() == 0 && l.query().scope() == Scope::Files {
            // The file search as it was, with what it found since.
            if let Some(files) = &mut self.files {
                files.search.tick();
            }
            self.show_files();
        } else if left && l.query().scope() == Scope::Bang {
            if let (Some(session), Some(worker)) = (&mut self.web_session, &self.web_worker) {
                session.query(worker, None, "");
            }
            self.show_web(true);
        } else if left && l.depth() == 0 && l.query().scope() == Scope::Windows {
            // The windows as they are now.
            self.rank_windows(core, None, true);
        } else if let (true, 0, Scope::Source(key)) = (left, l.depth(), l.query().scope()) {
            // What the source has now.
            self.show_source(key, true);
        }
    }

    /// Open the launcher's match at ring index `index` (None: the
    /// selection): activate it if it runs, else launch it, or the other
    /// way around with `other`. A file opens with `open-command`, or with
    /// `other`, its folder does. It runs once the launcher gave up the
    /// keyboard (`launcher_after`).
    fn launcher_pick(&mut self, core: &Core<Self>, index: Option<usize>, other: bool) {
        let Some(l) = &self.launcher else { return };
        let Some(m) = index.or(l.selected()).and_then(|i| l.match_at(i)) else {
            return;
        };
        let scope = l.query().scope();
        let suggested = scope == Scope::Apps && m.id.starts_with(engines::SUGGESTION);
        if scope == Scope::Web || m.id == engines::FALLBACK || suggested {
            let web = &self.config.launcher.web;
            let target = if scope == Scope::Web {
                let query = if m.id == engines::TYPED {
                    l.query().text()
                } else {
                    &m.name
                };
                (l.query().engine()).map(|key| (key.to_owned(), query.to_owned()))
            } else {
                // The fallback searches the query, an unknown bang whole;
                // a suggestion of it is searched as it is.
                let query = match m.id == engines::FALLBACK {
                    true => web.fallback_query(scope, l.query().text()),
                    false => m.name.clone(),
                };
                (web.fallback_engine()).map(|e| (e.keyword.clone(), query))
            };
            if let Some((keyword, query)) = target {
                self.open_search(core, &keyword, &query);
            }
            return;
        }
        if scope == Scope::Windows {
            let (Some(window), Some(app_id)) = (m.window.clone(), m.app.clone()) else {
                return;
            };
            self.launcher_window(core, app_id, window);
            return;
        }
        if scope == Scope::Bang {
            return;
        }
        // A source's item: its `run`, else what it is (a file opens, an
        // app activates or launches; nothing else does anything).
        let mut app_of_item = None;
        if let Scope::Source(key) = scope {
            let (Some(entry), Some(item)) = (
                self.widgets.source(key),
                self.widgets.source_item(key, &m.id),
            ) else {
                return;
            };
            if entry.decl.run {
                let picked = item.id.clone();
                self.close_launcher(core, None);
                let run = ItemRun {
                    instance: entry.instance,
                    source: entry.decl.id,
                    item,
                };
                self.pending_item = Some((run, picked));
                return;
            }
            match item.kind {
                wl_shell_lua::ItemKind::App { app } => app_of_item = Some(app),
                wl_shell_lua::ItemKind::File { .. } => {}
                wl_shell_lua::ItemKind::Item => return,
            }
        }
        if let Some(f) = &m.file {
            let path = match other {
                true => f.path.parent().unwrap_or(&f.path).to_path_buf(),
                false => f.path.clone(),
            };
            let picked = f.path.to_string_lossy().into_owned();
            self.close_launcher(core, None);
            self.pending_path = Some((path, picked));
            return;
        }
        // Not learned for a source's query: it isn't an app query.
        let query = match app_of_item {
            Some(_) => String::new(),
            None => l.query().text().to_string(),
        };
        let id = app_of_item.as_deref().unwrap_or(&m.id);
        let Some(app) = self.apps.iter().find(|a| a.id == id).cloned() else {
            return;
        };
        let launch = (self.config.launcher.enter == LauncherEnter::Launch) != other;
        let app_id = self.dock_app_id(core, &app);
        // Launching needs its entry, which the index has.
        if !matches!(self.entries.get(&app_id), Some(Some(_))) {
            self.entries.insert(app_id.clone(), Some(app.entry.clone()));
        }
        self.close_launcher(core, Some(app_id.clone()));
        self.pending_open = Some(PendingOpen {
            app_id,
            id: app.id,
            launch,
            query,
        });
    }

    /// Tab on the launcher's match at ring index `index` (None: the
    /// selection): an app's or a file's actions (a file's once its type
    /// is known), or an action's choices.
    fn launcher_into(&mut self, core: &Core<Self>, index: Option<usize>) {
        let Some(l) = &mut self.launcher else { return };
        if let Some(i) = index {
            l.select(i);
        }
        match l.query().scope() {
            Scope::Bang => {
                let id = (l.selected().and_then(|i| l.match_at(i))).map(|m| m.id.clone());
                match id {
                    // An unknown bang is no engine: Enter searches it.
                    Some(id) if id == engines::FALLBACK => {}
                    Some(keyword) => {
                        l.enter_web(keyword, "");
                        self.rank_web();
                    }
                    None => self.rank_web(),
                }
                return;
            }
            Scope::Web => {
                if let Some(text) = l
                    .selected()
                    .and_then(|i| l.match_at(i))
                    .filter(|m| m.id != engines::TYPED)
                    .map(|m| m.name.clone())
                {
                    l.query_mut().complete_web(text);
                    self.rank_web();
                }
                return;
            }
            _ => {}
        }
        // Tab on a suggestion of the web fallback completes the query.
        if l.query().scope() == Scope::Apps
            && let Some(text) = (l.selected().and_then(|i| l.match_at(i)))
                .filter(|m| m.id.starts_with(engines::SUGGESTION))
                .map(|m| m.name.clone())
        {
            l.query_mut().complete_web(text);
            self.rank_launcher(core);
            return;
        }
        let Some(i) = l.selected() else {
            return;
        };
        if l.depth() > 0 {
            let id = l.match_at(i).map(|m| m.id.clone());
            let choice = (l.level().into_iter())
                .flat_map(|level| &level.choices)
                .find(|c| Some(&c.id) == id.as_ref());
            if let Some(Does::Choose(level)) = choice.map(|c| &c.does) {
                let mut level = level.clone();
                // A plugin's choices: asked for now, filled when they come.
                if let Some(ask) = &level.ask {
                    level.asking = self.widgets.ask_choices(ask);
                    if level.asking.is_none() {
                        // Not running: nothing to wait for.
                        actions::fill(&mut level, Err("not running".into()));
                    }
                }
                let asking = level.asking;
                if let Some(l) = &mut self.launcher {
                    l.enter(level);
                }
                self.rank_choices();
                if let Some(id) = asking {
                    self.give_up_choices(core, id);
                }
            }
            return;
        }
        let scope = l.query().scope();
        let Some(m) = l.match_at(i).cloned() else {
            return;
        };
        let (subject, file_type, pinned, trash) = match (&m.file, scope) {
            (Some(f), _) => {
                let Some(file_type) = self.file_types.get(&f.path) else {
                    // Its actions depend on its type: on the worker's answer.
                    if let Some(assets) = &self.assets {
                        assets.file_type(&f.path, f.dir);
                    }
                    self.pending_tab = Some(f.path.clone());
                    return;
                };
                let subject = Subject::File(actions::File {
                    path: f.path.clone(),
                    dir: f.dir,
                });
                let pinned = f.dir && self.pins.contains(&pins::for_path(&f.path));
                let trash = self.trash_takes(std::slice::from_ref(&f.path));
                (subject, Some(file_type), pinned, trash)
            }
            (None, Scope::Windows) => {
                let Some(w) = self.window_subject(core, &m) else {
                    return;
                };
                let pinned = self.pins.contains(&Pin::App(w.app_id.clone()));
                (Subject::Window(w), None, pinned, false)
            }
            // A source's item: an app's actions if it is an installed
            // app, else only the plugins' actions for items.
            (None, Scope::Source(key)) => {
                let (Some(entry), Some(item)) = (
                    self.widgets.source(key),
                    self.widgets.source_item(key, &m.id),
                ) else {
                    return;
                };
                let app = match &item.kind {
                    wl_shell_lua::ItemKind::App { app } => self.apps.iter().find(|a| a.id == *app),
                    _ => None,
                };
                match app {
                    Some(app) => {
                        let app_id = self.dock_app_id(core, app);
                        let subject = Subject::App(self.app_subject(core, app, &app_id));
                        (subject, None, self.pins.contains(&Pin::App(app_id)), false)
                    }
                    None => {
                        let subject = Subject::Item(actions::ItemSubject {
                            source: entry.decl.id,
                            id: item.id,
                            title: item.title,
                            data: item.data,
                        });
                        (subject, None, false, false)
                    }
                }
            }
            (None, _) => {
                let Some(app) = self.apps.iter().find(|a| a.id == m.id) else {
                    return;
                };
                let app_id = self.dock_app_id(core, app);
                let subject = Subject::App(self.app_subject(core, app, &app_id));
                (subject, None, self.pins.contains(&Pin::App(app_id)), false)
            }
        };
        let plugins = self.widgets.launcher_actions();
        let now = history::now();
        let learn = self.config.launcher.history;
        let history = |id: &str| {
            if learn {
                self.history.boost(id, now)
            } else {
                0.0
            }
        };
        let cx = actions::Context {
            apps: &self.apps,
            file_type,
            history: &history,
            trash,
            pinned,
            terminal: !self
                .config
                .integration
                .open_terminal_command
                .trim()
                .is_empty(),
            clipboard: core.dnd.available(),
            close: self.config.integration.close_command.is_some(),
            plugins: &plugins,
        };
        let level = actions::actions(&subject, &cx);
        // An item no plugin acts on has nothing to enter.
        if level.choices.is_empty() && matches!(subject, Subject::Item(_)) {
            return;
        }
        // Windows… shows the windows' captures: start them now (a frame
        // each), so they are there by the time it is entered.
        let previews = self.config.windows.previews;
        let windows: Vec<(String, _)> = match &subject {
            Subject::App(a) if a.windows.len() > 1 && previews != Previews::Off => {
                (core.toplevels().into_iter())
                    .filter(|(_, t)| a.windows.iter().any(|w| w.id == t.identifier))
                    .map(|(handle, t)| (t.identifier, handle))
                    .collect()
            }
            _ => Vec::new(),
        };
        self.pending_tab = None;
        if let Some(l) = &mut self.launcher {
            // A window's captures are those of the search.
            if !matches!(subject, Subject::Window(_)) {
                l.set_previews(windows, previews == Previews::Live);
            }
            l.enter(level);
        }
        self.rank_choices();
    }

    /// A plugin's choices (request `id`) arrived: the level waiting for
    /// them, if it is the launcher's current one, shows them (none, for
    /// an error, which the runtime logged).
    fn choices_arrived(
        &mut self,
        id: RequestId,
        answer: Result<Vec<wl_shell_lua::Choice>, String>,
    ) {
        let Some(level) = (self.launcher.as_mut())
            .and_then(|l| l.level_mut())
            .filter(|level| level.waits_for(id))
        else {
            return;
        };
        actions::fill(level, answer);
        self.rank_choices();
        if let Some(l) = &mut self.launcher {
            l.sync_texts();
            l.kick();
        }
    }

    /// Should the plugin not answer request `id` for its choices in time,
    /// its level shows that there are none, so it stays usable.
    fn give_up_choices(&self, core: &Core<Self>, id: RequestId) {
        // A little past the deadline, so an answer on time comes first.
        let after = actions::CHOICES_TIMEOUT + Duration::from_millis(20);
        let timer = core.timer(after, move |app: &mut Self, _| {
            let waiting =
                (app.launcher.as_ref().and_then(|l| l.level())).filter(|level| level.waits_for(id));
            if let Some(level) = waiting {
                let ms = actions::CHOICES_TIMEOUT.as_millis();
                warn!("{}: the plugin gave no choices in {ms} ms", level.name);
                app.choices_arrived(id, Err("no answer in time".into()));
            }
        });
        if let Err(e) = timer {
            warn!("{e}");
        }
    }

    /// A file's type arrived: a Tab waiting for it goes on, if the file
    /// is still the selection.
    fn file_type_ready(&mut self, core: &Core<Self>, path: PathBuf, file_type: FileType) {
        self.file_types.insert(path.clone(), file_type);
        if self.pending_tab.as_ref() != Some(&path) {
            return;
        }
        self.pending_tab = None;
        let Some(l) = &self.launcher else { return };
        let selected = (l.selected().and_then(|i| l.match_at(i)))
            .and_then(|m| m.file.as_ref())
            .is_some_and(|f| f.path == path);
        if selected && l.depth() == 0 && !l.closing() {
            self.launcher_into(core, None);
        }
    }

    /// Run the launcher's choice at ring index `index` (None: the
    /// selection). One that opens something closes the launcher (and
    /// runs once it gave up the keyboard); one with choices enters them.
    fn launcher_run(&mut self, core: &mut Core<Self>, index: Option<usize>) {
        let Some(l) = &self.launcher else { return };
        let Some(i) = index.or(l.selected()) else {
            return;
        };
        let id = l.match_at(i).map(|m| m.id.clone());
        let choice = (l.level().into_iter())
            .flat_map(|level| &level.choices)
            .find(|c| Some(&c.id) == id.as_ref());
        let Some(does) = choice.map(|c| c.does.clone()) else {
            return;
        };
        // A file's path, or an app's desktop ID.
        let picked = (l.subject())
            .map(|m| match &m.file {
                Some(f) => f.path.to_string_lossy().into_owned(),
                None => m.id.clone(),
            })
            .unwrap_or_default();
        match does {
            Does::Choose(_) => self.launcher_into(core, Some(i)),
            Does::Open(path) => {
                self.close_launcher(core, None);
                self.pending_path = Some((path, picked));
            }
            Does::OpenWith { app, path } => {
                let Some(app) = self.apps.iter().find(|a| a.id == app).cloned() else {
                    return;
                };
                let app_id = self.dock_app_id(core, &app);
                if !matches!(self.entries.get(&app_id), Some(Some(_))) {
                    self.entries.insert(app_id.clone(), Some(app.entry.clone()));
                }
                self.close_launcher(core, Some(app_id.clone()));
                self.pending_with = Some(PendingWith {
                    app_id,
                    id: app.id,
                    path,
                });
            }
            Does::Terminal(dir) => {
                self.close_launcher(core, None);
                self.pending_terminal = Some((dir, picked));
            }
            // Copied while the launcher has the keyboard, which the
            // compositor may want.
            Does::CopyPath(path) => {
                let text = path.to_string_lossy().into_owned();
                let data = ([
                    "text/plain;charset=utf-8",
                    "text/plain",
                    "UTF8_STRING",
                    "STRING",
                ])
                .map(|mime| (mime.to_string(), text.clone().into_bytes()));
                if let Err(e) = core.set_selection(data.into()) {
                    warn!("copying the path: {e}");
                }
                self.finish_launcher(core, picked);
            }
            Does::Pin { path, pinned } => {
                self.finish_launcher(core, picked);
                self.keep(pins::for_path(&path), pinned);
            }
            Does::PinApp { app, pinned } => {
                let Some(app) = self.apps.iter().find(|a| a.id == app).cloned() else {
                    return;
                };
                let app_id = self.dock_app_id(core, &app);
                self.finish_launcher(core, picked);
                self.keep(Pin::App(app_id), pinned);
            }
            Does::Desktop { app, action } => {
                self.launcher_runs(core, &app, AppRun::Desktop(action), picked);
            }
            Does::Command { app, command } => {
                self.launcher_runs(core, &app, AppRun::Command(command), picked);
            }
            Does::NewWindow(app) => self.launcher_runs(core, &app, AppRun::NewWindow, picked),
            Does::Activate { app, window } => {
                self.launcher_runs(core, &app, AppRun::Activate(window), picked);
            }
            Does::ActivateWindow { app_id, window } => {
                self.launcher_window(core, app_id, window);
            }
            Does::Close(ids) => {
                self.finish_launcher(core, picked);
                for id in ids {
                    self.close_window(core, &id);
                }
            }
            Does::Trash(path) => {
                self.finish_launcher(core, picked);
                let paths = std::slice::from_ref(&path);
                let command = launch::paths_command(&self.config.trash.command, paths, None);
                let output = Output::Prefixed("trash-command".into());
                if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
                    warn!("trash-command: {e}");
                }
            }
            Does::Plugin(run) => {
                self.close_launcher(core, None);
                self.pending_plugin = Some((run, picked));
            }
        }
    }

    /// Close the launcher and run `run` for the app `app` (a desktop ID)
    /// once it gave up the keyboard, as the app itself would be: a new
    /// window gets it.
    fn launcher_runs(&mut self, core: &Core<Self>, app: &str, run: AppRun, picked: String) {
        let Some(app) = self.apps.iter().find(|a| a.id == app).cloned() else {
            return;
        };
        let app_id = self.dock_app_id(core, &app);
        // Launching needs its entry, which the index has.
        if !matches!(self.entries.get(&app_id), Some(Some(_))) {
            self.entries.insert(app_id.clone(), Some(app.entry.clone()));
        }
        // A command needn't open anything: no icon flies for it.
        let opened = (!matches!(run, AppRun::Command(_))).then(|| app_id.clone());
        self.close_launcher(core, opened);
        self.pending_app = Some(PendingApp {
            app_id,
            run,
            picked,
        });
    }

    /// Close the launcher after an action that opens nothing, and tell
    /// its client what it was done to.
    fn finish_launcher(&mut self, core: &Core<Self>, picked: String) {
        self.close_launcher(core, None);
        if let Some(client) = self.launcher_client.take() {
            client.answer(Reply::Picked(picked));
        }
    }

    /// The app ID the dock knows `app` by: a pinned or running app whose
    /// desktop entry it is, else a window's app ID that names it, else
    /// its `StartupWMClass` or desktop ID.
    fn dock_app_id(&self, core: &Core<Self>, app: &IndexedApp) -> String {
        let known = pins::apps(&self.pins)
            .into_iter()
            .chain(self.order.apps().iter().cloned());
        for app_id in known {
            if let Some(Some(e)) = self.entries.get(&app_id)
                && e.desktop_id == app.entry.desktop_id
            {
                return app_id;
            }
        }
        let names: Vec<&str> = std::iter::once(app.id.as_str())
            .chain(app.wm_class.as_deref())
            .collect();
        for (_, t) in core.toplevels() {
            if names.iter().any(|n| n.eq_ignore_ascii_case(&t.app_id)) {
                return t.app_id;
            }
        }
        app.wm_class.clone().unwrap_or_else(|| app.id.clone())
    }

    /// Close the launcher, and say so to its client.
    fn cancel_launcher(&mut self, core: &Core<Self>) {
        self.close_launcher(core, None);
        if let Some(client) = self.launcher_client.take() {
            client.answer(Reply::Cancelled);
        }
    }

    /// Run the launcher backwards into its dock, with the icon of
    /// `opened` (an app ID) flying to it if it opened one. A file search
    /// stops now.
    fn close_launcher(&mut self, core: &Core<Self>, opened: Option<String>) {
        let _ = core;
        self.files = None;
        self.clear_web();
        self.file_types.clear();
        self.pending_tab = None;
        self.widgets.end_search();
        let Some(launcher) = &mut self.launcher else {
            return;
        };
        if launcher.closing() {
            return;
        }
        let output = launcher.output().cloned();
        let dock = (self.docks.iter_mut()).find(|d| Some(&d.output) == output.as_ref());
        match dock {
            Some(dock) => {
                let fly_to = opened.as_deref().map(|a| dock.landing_rect(a));
                launcher.close(Some(dock.handover()), fly_to);
                dock.end_launcher(opened);
            }
            None => launcher.close(None, None),
        }
    }

    /// Drop the launcher at once; its dock gets its panel back.
    fn drop_launcher(&mut self) {
        self.files = None;
        self.clear_web();
        self.file_types.clear();
        self.pending_tab = None;
        self.widgets.end_search();
        let Some(launcher) = self.launcher.take() else {
            return;
        };
        if let Some(output) = launcher.output()
            && let Some(dock) = self.docks.iter_mut().find(|d| d.output == *output)
        {
            dock.launcher_gone();
        }
        // The dock shows its panel before the launcher's surface goes.
        drop(launcher);
    }

    /// After a dispatch: run what the launcher opened, now that it gave up
    /// the keyboard, and drop it once it is out of sight.
    fn launcher_after(&mut self, core: &mut Core<Self>) {
        let pending = self.pending_open.is_some()
            || self.pending_path.is_some()
            || self.pending_url.is_some()
            || self.pending_with.is_some()
            || self.pending_terminal.is_some()
            || self.pending_app.is_some()
            || self.pending_window.is_some()
            || self.pending_plugin.is_some()
            || self.pending_item.is_some();
        if pending {
            // Remove the launcher before starting an opener or activating
            // an existing window. Keep the focus handoff, not the animation.
            self.drop_launcher();
            if let Err(e) = core.conn.roundtrip() {
                warn!("{e}");
            }
        }
        if let Some(open) = self.pending_open.take() {
            if self.config.launcher.history {
                self.history.learn(&open.query, &open.id, history::now());
            }
            let (_, recent) = Self::app_windows(core, &open.app_id);
            match recent {
                Some(id) if !open.launch => {
                    self.activate(core, &id);
                    self.record(&open.id);
                }
                _ => self.launch(core, &open.app_id),
            }
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(open.id));
            }
        }
        if let Some(with) = self.pending_with.take() {
            self.open_with(core, &with);
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(with.path.to_string_lossy().into_owned()));
            }
        }
        if let Some(PendingApp {
            app_id,
            run,
            picked,
        }) = self.pending_app.take()
        {
            match run {
                AppRun::Desktop(action) => self.launch_action(core, &app_id, &action),
                AppRun::Command(command) => self.app_command(core, &app_id, &command, "action"),
                AppRun::NewWindow => self.launch(core, &app_id),
                AppRun::Activate(window) => self.activate(core, &window),
            }
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(picked));
            }
        }
        if let Some(window) = self.pending_window.take() {
            self.activate(core, &window);
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(window));
            }
        }
        if let Some((dir, picked)) = self.pending_terminal.take() {
            let command = launch::dir_command(&self.config.integration.open_terminal_command, &dir);
            let output = Output::Prefixed("open-terminal-command".into());
            if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
                warn!("open-terminal-command: {e}");
            }
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(picked));
            }
        }
        if let Some((run, picked)) = self.pending_plugin.take() {
            self.widgets.run_action(run);
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(picked));
            }
        }
        if let Some((run, picked)) = self.pending_item.take() {
            self.widgets.open_item(run);
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(picked));
            }
        }
        if let Some((command, url)) = self.pending_url.take() {
            if let Err(e) = spawn::shell(
                &core.loop_handle,
                &command,
                Output::Prefixed("web-command".into()),
            ) {
                warn!("web-command: {e}");
            }
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(url));
            }
        }
        if let Some((path, picked)) = self.pending_path.take() {
            self.open_path(core, &path);
            if let Some(client) = self.launcher_client.take() {
                client.answer(Reply::Picked(picked));
            }
        }
        if self.launcher.as_ref().is_some_and(LauncherView::gone) {
            self.drop_launcher();
        }
    }

    /// Open a file with an app (Open with… in the launcher), and record
    /// the app in the history.
    fn open_with(&mut self, core: &Core<Self>, with: &PendingWith) {
        let PendingWith { app_id, id, path } = with;
        let commands = match self.open_commands(app_id, std::slice::from_ref(path)) {
            Ok(commands) => commands,
            Err(e) => return warn!("cannot open {} with {app_id}: {e}", path.display()),
        };
        for command in &commands {
            let result = match command {
                Command::Argv(argv) => spawn::argv(&core.loop_handle, argv, Output::Inherit),
                Command::Shell(line) => spawn::shell(&core.loop_handle, line, Output::Inherit),
            };
            if let Err(e) = result {
                return warn!("cannot open {} with {app_id}: {e}", path.display());
            }
        }
        self.record(id);
        let (windows, _) = Self::app_windows(core, app_id);
        if windows.is_empty() {
            self.start_bounce(core, app_id);
        }
    }

    /// `id` (a desktop ID, or an app ID) was opened: remember it for
    /// ranking.
    fn record(&mut self, id: &str) {
        if !self.config.launcher.history {
            return;
        }
        self.history.record(id, history::now());
        if let Some(path) = &self.history_path
            && let Err(e) = history::write(path, &self.history)
        {
            warn!("{}: {e}", path.display());
        }
    }

    /// `app_id` was launched: record it under its desktop ID if it has
    /// one.
    fn record_launch(&mut self, app_id: &str) {
        let id = match self.entries.get(app_id) {
            Some(Some(e)) => e.desktop_id.trim_end_matches(".desktop").to_string(),
            _ => app_id.to_string(),
        };
        self.record(&id);
    }

    /// The config (or the scheme in use) changed: forget what was rendered
    /// with the old one and restyle the docks.
    fn config_changed(&mut self, core: &mut Core<Self>) {
        self.config = self.reloader.config().clone();
        self.config.launcher.web.check();
        self.load_browser_preferences();
        self.clear_web();
        if self
            .launcher
            .as_ref()
            .is_some_and(|l| !l.closing() && matches!(l.query().scope(), Scope::Bang | Scope::Web))
        {
            self.rank_web();
        }
        let mut cache = self.cache.borrow_mut();
        cache.clear_icons();
        cache.clear_shapes();
        drop(cache);
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
        self.use_streams(core);
        self.watch_trash(core);
        self.configure_widgets(core);
        self.outputs_dirty = true;
        self.reload = true;
        self.trim = true;
    }

    fn reload_pins(&mut self) {
        let Some(path) = &self.pins_path else { return };
        match pins::read(path) {
            Ok(pins) if pins != self.pins => {
                self.pins = pins;
                self.dirty = true;
                self.prefetch_entries();
            }
            Ok(_) => {}
            Err(e) => warn!("{}: {e}", path.display()),
        }
    }

    /// Have the worker look up the desktop entries of the pinned and
    /// running apps, so a click launches them and a right click shows
    /// their menu at once (and of the apps widgets back, for their
    /// names).
    fn prefetch_entries(&self) {
        let Some(assets) = &self.assets else { return };
        let running = self.order.apps().iter().cloned();
        let widgets = (self.config.app.iter())
            .filter(|(_, a)| a.widget.is_some())
            .map(|(id, _)| id.clone());
        for app_id in pins::apps(&self.pins)
            .into_iter()
            .chain(running)
            .chain(widgets)
        {
            if !self.entries.contains_key(&app_id) {
                assets.entry(&app_id);
            }
        }
    }

    /// The dock on `output`.
    pub fn dock_mut(&mut self, output: &WlOutput) -> Option<&mut DockView> {
        self.docks.iter_mut().find(|d| d.output == *output)
    }

    fn dock_on(&mut self, surface: &WlSurface) -> Option<&mut DockView> {
        self.docks.iter_mut().find(|d| d.surface() == surface)
    }

    /// Create, restyle and destroy docks to match the outputs and the
    /// config.
    fn sync_docks(&mut self, core: &Core<Self>) {
        let Some(assets) = self.assets.clone() else {
            return; // not started yet
        };
        let reload = std::mem::take(&mut self.reload);
        let outputs = core.outputs();
        let idents: Vec<wl_shell_config::output::Ident> = outputs
            .iter()
            .map(|o| crate::config::ident(&o.ident))
            .collect();
        // While `output` names one that isn't connected, the first output
        // stands in.
        let fallback = self.config.output.unmatched(&idents).then_some(0);
        let wanted: Vec<(WlOutput, String, Option<String>, i32, Style)> = outputs
            .into_iter()
            .enumerate()
            .filter_map(|(i, output)| {
                let layout = self.config.dock(&idents[i], fallback == Some(i))?;
                let style = Style::new(&self.config, layout);
                Some((
                    output.wl,
                    output.key,
                    output.ident.name,
                    output.scale,
                    style,
                ))
            })
            .collect();
        self.docks.retain(|dock| {
            (wanted.iter()).any(|(o, name, _, _, style)| {
                *o == dock.output && *name == dock.name && !dock.style().needs_new_surface(style)
            })
        });
        for (output, name, connector, scale, style) in wanted {
            match self.docks.iter_mut().find(|d| d.output == output) {
                Some(dock) => {
                    dock.connector = connector;
                    dock.set_output_size(Self::output_size(core, &output));
                    dock.set_style(style, reload);
                    dock.set_scale(scale);
                }
                None => match DockView::new(
                    core,
                    output,
                    name,
                    connector,
                    scale,
                    style,
                    self.cache.clone(),
                    assets.clone(),
                ) {
                    Ok(mut dock) => {
                        dock.set_output_size(Self::output_size(core, &dock.output.clone()));
                        let apps: Vec<String> =
                            self.launching.iter().map(|l| l.app_id.clone()).collect();
                        dock.set_launching(&apps);
                        dock.set_widget_names(self.widgets.labels());
                        dock.set_widget_icons(self.widgets.icons());
                        self.docks.push(dock);
                    }
                    Err(e) => warn!("failed to create a dock: {e}"),
                },
            }
        }
        self.sync_widget_tile();
    }

    /// `output`'s logical size, if known.
    fn output_size(core: &Core<Self>, output: &WlOutput) -> Option<(i32, i32)> {
        core.output_state.info(output)?.logical_size
    }

    /// The open windows, leaving out `closed` (the list still knows a
    /// window while its close is handled).
    fn toplevels(core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) -> Vec<Toplevel> {
        (core.toplevels().into_iter())
            .filter(|(h, _)| Some(h) != closed)
            .map(|(_, t)| t)
            .collect()
    }

    /// Rebuild every dock's items from the window list and the pins.
    fn rebuild(&mut self, core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) {
        let toplevels = Self::toplevels(core, closed);
        self.order.update(&toplevels);
        // An app not seen before may come with a new desktop entry.
        let mut new_app = false;
        for t in &toplevels {
            if !self.seen.contains(&t.app_id) {
                self.seen.push(t.app_id.clone());
                new_app = true;
            }
        }
        if new_app && let Some(assets) = &self.assets {
            assets.refresh();
        }
        if new_app {
            self.prefetch_entries();
        }
        self.end_launches(core, &toplevels);

        // The standalone widgets pinned run while they are.
        if pins::widgets(&self.pins) != self.widget_pins {
            self.configure_widgets(core);
        }
        // The pinned apps, with the standalone widgets among them; then
        // the folders and the widgets among those. Paths that are files
        // aren't shown: only folders are pinned.
        let shown = |name: &str| widgets::standalone(&self.config, name);
        let mut pins: Vec<String> = Vec::new();
        let mut tail: Vec<dock::Tail> = Vec::new();
        for pin in &self.pins {
            match pin {
                Pin::App(app) => pins.push(app.clone()),
                Pin::Widget(name, Zone::Apps) if shown(name) => pins.push(dock::widget_key(name)),
                Pin::Widget(name, Zone::Folders) if shown(name) => {
                    tail.push(dock::Tail::Widget(name.clone()))
                }
                Pin::Widget(..) => {}
                Pin::Path(p) => match std::fs::metadata(pins::expand(p)) {
                    Ok(m) if !m.is_dir() => {}
                    Ok(_) => tail.push(dock::Tail::Folder(p.clone(), true)),
                    Err(_) => tail.push(dock::Tail::Folder(p.clone(), false)),
                },
            }
        }
        let focused = (self.config.active_indicator && core.focus_stream.running())
            .then(|| core.mru.first())
            .flatten();
        let streaming = self.output_stream.running();
        let trash = self.config.trash.enabled.then_some(self.trash_full);
        for dock in &mut self.docks {
            // The stream names connectors: an output without one can't be
            // told apart in it, so its dock shows every window.
            let by_output = streaming
                && dock.connector.is_some()
                && dock.style().layout.windows == Windows::Output;
            let connector = dock.connector.as_deref().unwrap_or("");
            let mut items = dock::items(&toplevels, &pins, &self.order, focused, |id| {
                self.placement.place(id, connector, by_output)
            });
            dock::add_tail(&mut items, &tail, trash);
            // Widgets' tiles may be longer along the dock.
            let axis = dock.tile_base().axis;
            for item in items.iter_mut().filter(|i| i.is_app() || i.is_widget()) {
                item.span = dock::Span::of(self.widgets.span(&item.app_id, axis));
            }
            dock.set_items(core, &items);
        }
        // What takes a drag from outside may have changed.
        self.refresh_offer(core);
        // A window search follows the windows.
        self.rank_windows(core, closed, true);
    }

    /// The items of the dock on `output`'s hovered app, if any.
    fn hovered_item(&self, output: &WlOutput) -> Option<Item> {
        let dock = self.docks.iter().find(|d| d.output == *output)?;
        dock.hovered_item().cloned()
    }

    // -- the output stream ---------------------------------------------------

    fn output_line(&mut self, line: &str) {
        if let Some((output, id)) = dock::parse_output_line(line)
            && self.placement.report(output, id)
        {
            self.dirty = true;
        }
    }

    /// A new window: unless the stream has placed it already, keep it off
    /// the docks that show their output's windows for `output-grace`.
    fn hold_new_window(&mut self, core: &Core<Self>, id: &str) {
        let by_output = (self.docks.iter()).any(|d| d.style().layout.windows == Windows::Output);
        if !by_output || !self.output_stream.running() || self.placement.placed(id) {
            return;
        }
        self.placement.hold(id);
        let grace = Duration::from_millis(self.config.windows.output_grace.into());
        let held = id.to_string();
        let timer = core.timer(grace, move |app: &mut Self, _| {
            if app.placement.release(&held) {
                app.dirty = true;
            }
        });
        if let Err(e) = timer {
            warn!("{e}");
            self.placement.release(id);
        }
    }

    // -- clicks ----------------------------------------------------------------

    fn button(
        &mut self,
        core: &Core<Self>,
        output: &WlOutput,
        button: u32,
        serial: u32,
        at: (f64, f64),
        down: bool,
    ) {
        let item = self
            .docks
            .iter()
            .find(|dock| dock.output == *output)
            .and_then(|dock| dock.click_item(at.0, at.1))
            .cloned();
        let Some(item) = item else {
            self.pressed = None;
            // A click on the dock between icons (or on a hidden dock's
            // trigger band) is a click outside the strip or menu, and
            // gives back the keyboard focus it brought.
            if !down {
                self.close_popups(core);
                self.release_keyboard(output);
            }
            return;
        };
        if down {
            self.pressed = Some(Pressed {
                output: output.clone(),
                app_id: item.app_id,
                button,
                serial,
                at,
            });
            return;
        }
        // Clicks act on release, on the icon the button went down on.
        let Some(pressed) = self.pressed.take() else {
            return;
        };
        if pressed.output != *output || pressed.app_id != item.app_id || pressed.button != button {
            return;
        }
        // Any click closes the open menu (or folder popup); right-clicking
        // its icon only that, and clicking a folder whose popup is open
        // only closes it.
        let dock = self.docks.iter().find(|d| d.output == *output);
        let menu_was = dock.and_then(|d| d.menu_app()) == Some(item.app_id.as_str());
        let popup_was = dock.and_then(|d| d.popup_folder()) == Some(item.app_id.as_str());
        if let Some(dock) = self.dock_mut(output) {
            dock.transfer_zoom(&item.app_id);
        }
        self.close_menus(core);
        match button {
            BTN_LEFT if popup_was => self.release_keyboard(output),
            BTN_LEFT => self.click(core, output, &item, pressed),
            BTN_MIDDLE if item.is_app() => {
                self.close_strips(core);
                self.middle_click(core, &item);
                self.release_keyboard(output);
            }
            BTN_MIDDLE if item.is_widget() && self.widgets.takes_middle_clicks(&item.app_id) => {
                self.close_strips(core);
                let kind = wl_shell_lua::RequestKind::Click("middle".into());
                self.widgets.notify(&item.app_id, kind);
                self.release_keyboard(output);
            }
            BTN_RIGHT if !menu_was => self.open_menu(core, pressed),
            _ => self.release_keyboard(output),
        }
    }

    /// A middle click: the app's `middle-click` command, else a new
    /// instance (or window), as the app does it.
    fn middle_click(&mut self, core: &Core<Self>, item: &Item) {
        let command = (self.config.app(&item.app_id)).and_then(|a| a.middle_click.clone());
        match command {
            Some(command) => self.app_command(core, &item.app_id, &command, "middle-click"),
            None => self.launch(core, &item.app_id),
        }
    }

    /// Scrolling on an icon: the app's `scroll-up` and `scroll-down`
    /// commands, one per wheel notch (or `SCROLL_STEP` pixels of touchpad
    /// scrolling).
    fn scroll(&mut self, core: &Core<Self>, output: &WlOutput, vertical: &AxisScroll) {
        let Some(item) = self.hovered_item(output) else {
            self.scrolled = None;
            return;
        };
        let app = self.config.app(&item.app_id);
        let up = app.and_then(|a| a.scroll_up.clone());
        let down = app.and_then(|a| a.scroll_down.clone());
        if up.is_none() && down.is_none() {
            return;
        }
        let steps = match (vertical.value120, vertical.discrete) {
            (0, 0) => vertical.absolute / SCROLL_STEP,
            (0, d) => d as f64,
            (v, _) => v as f64 / 120.0,
        };
        let mut total = match self.scrolled.take() {
            Some((app_id, total)) if app_id == item.app_id => total,
            _ => 0.0,
        };
        // Turning around starts over.
        if total * steps < 0.0 {
            total = 0.0;
        }
        total += steps;
        while total.abs() >= 1.0 - 1e-6 {
            let (command, key) = match total > 0.0 {
                true => (&down, "scroll-down"),
                false => (&up, "scroll-up"),
            };
            if let Some(command) = command {
                self.app_command(core, &item.app_id, command, key);
            }
            total -= total.signum();
        }
        self.scrolled = Some((item.app_id, total));
    }

    /// A left click on `item` on the dock on `output`, `pressed` there:
    /// close its strip if open, else open it if it gets one, else
    /// activate or launch (or open a folder's popup). Ctrl activates the
    /// most recent window.
    fn click(&mut self, core: &Core<Self>, output: &WlOutput, item: &Item, pressed: Pressed) {
        let serial = pressed.serial;
        // Modifiers are only current while the dock has keyboard focus,
        // which the click gave it.
        let ctrl = core.input.keyboard_focus().is_some() && core.input.modifiers.ctrl;
        let Some(dock) = self.docks.iter().find(|d| d.output == *output) else {
            return;
        };
        let toggle = dock.strip_app() == Some(item.app_id.as_str());
        let strip = !ctrl && !toggle && dock.can_strip(item);
        // Any click closes the strip that is open; clicking its icon only
        // that.
        if toggle {
            self.close_strips(core);
            return;
        }
        if strip {
            // Only one grabbing popup at a time: the others go at once.
            // The clicked dock replaces its own, keeping its keyboard.
            for dock in self.docks.iter_mut().filter(|d| d.output != *output) {
                dock.drop_strip(core);
                dock.drop_menu(core);
            }
            let grab = core.input.seat().map(|seat| (seat, serial));
            if let Some(dock) = self.dock_mut(output)
                && dock.open_strip(core, &item.app_id, grab)
            {
                return;
            }
        }
        self.close_popups(core);
        match item.click(&core.mru) {
            Click::Activate(id) => self.activate(core, &id),
            Click::Launch => self.launch(core, &item.app_id),
            Click::OpenTrash => self.open_trash(core),
            // The popup opens once the folder is listed.
            Click::OpenFolder => {
                return self.list_folder(item, output, Some(pressed.serial), None);
            }
            Click::Widget => self.click_widget(output, item),
            Click::Nothing => {}
        }
        self.release_keyboard(output);
    }

    /// A left click on the standalone widget `item` on `output`: its
    /// popup opens, or stays closed if it was open (the press closed
    /// it), unless the plugin takes the click (`on_click("left")`, when
    /// its popup doesn't open on click). Either way, hovering it doesn't
    /// open the popup again until the pointer moves to another item.
    fn click_widget(&mut self, output: &WlOutput, item: &Item) {
        let key = &item.app_id;
        let was_open = self.widget_was_open.take().as_ref() == Some(key);
        self.widget_epoch = self.widget_epoch.wrapping_add(1);
        self.widget_target = Some((output.clone(), key.clone()));
        self.widget_declined = None;
        if self.widgets.takes_clicks(key) {
            let kind = wl_shell_lua::RequestKind::Click("left".into());
            self.widgets.notify(key, kind);
        } else if !was_open && self.widgets.supports_popup(key) {
            self.widgets.select_popup(Some(key));
        }
        self.sync_label_hold();
    }

    /// Close every dock's strip.
    fn close_strips(&mut self, core: &Core<Self>) {
        for dock in &mut self.docks {
            dock.close_strip(core);
        }
    }

    /// Close every dock's menu, and forget one waiting to open.
    fn close_menus(&mut self, core: &Core<Self>) {
        self.pending_menu = None;
        self.pending_folder = None;
        for dock in &mut self.docks {
            dock.close_menu(core);
        }
    }

    /// Close the widget's popup (fading out), and stop waiting to open one.
    fn hide_widget(&mut self, core: &Core<Self>) {
        self.widget_epoch = self.widget_epoch.wrapping_add(1);
        self.widget_target = None;
        self.widgets.select_popup(None);
        for d in &mut self.docks {
            d.close_widget(core);
        }
        self.sync_label_hold();
    }

    /// Hold back the label of the item whose popup opens on hover (and
    /// is about to, or is open), so the two never show together.
    fn sync_label_hold(&mut self) {
        let held = (self.widget_target.as_ref())
            .filter(|(_, key)| self.widgets.popup_on(key) == Some(PopupOn::Hover))
            .cloned();
        for dock in &mut self.docks {
            let key = (held.as_ref())
                .filter(|(output, _)| *output == dock.output)
                .map(|(_, key)| key.as_str());
            dock.hold_label(key);
        }
    }

    /// The pointer left the dock or the widget's popup: close the popup
    /// unless it comes back to either soon (from the icon to the popup).
    fn leave_widget(&mut self, core: &Core<Self>) {
        self.widget_declined = None;
        if self.widget_target.is_none() {
            return;
        }
        let grace = self.widget_grace;
        let _ = core.timer(WIDGET_GRACE, move |s: &mut Self, core| {
            if s.widget_grace == grace {
                s.hide_widget(core);
            }
        });
    }

    /// The pointer moved on the dock on `output`: open the hovered item's
    /// widget's popup after its delay (at once if another one was open,
    /// as the strip hands over), or close the one open. A popup opened by
    /// a click stays open over items without a hover popup.
    fn hover_widget(&mut self, core: &Core<Self>, output: &WlOutput) {
        self.widget_grace = self.widget_grace.wrapping_add(1);
        let hovered = self.hovered_item(output).map(|i| i.app_id);
        if self.widget_declined != hovered {
            self.widget_declined = None;
        }
        let target = hovered
            .filter(|key| self.widgets.popup_on(key) == Some(PopupOn::Hover))
            .filter(|key| self.widget_declined.as_ref() != Some(key))
            .map(|key| (output.clone(), key));
        if self.widget_target == target {
            return;
        }
        let clicked = (self.widget_target.as_ref())
            .is_some_and(|(_, key)| self.widgets.popup_on(key) == Some(PopupOn::Click));
        if clicked && target.is_none() {
            return;
        }
        let handover = self.docks.iter().any(DockView::widget_open);
        self.hide_widget(core);
        let Some((output, app)) = target else { return };
        let own = match dock::widget_name(&app) {
            Some(name) => self.config.widget.get(name).and_then(|w| w.popup_delay),
            None => self.config.app(&app).and_then(|a| a.popup_delay),
        };
        let delay = match handover {
            true => 0,
            false => own.unwrap_or(self.config.widgets.popup_delay),
        };
        self.widget_target = Some((output.clone(), app.clone()));
        self.sync_label_hold();
        let epoch = self.widget_epoch;
        let _ = core.timer(
            Duration::from_millis(delay.into()),
            move |s: &mut Self, _| {
                if s.widget_epoch == epoch
                    && s.pressed.is_none()
                    && s.dock_mut(&output)
                        .is_some_and(|d| !d.menu_open() && !d.strip_open() && !d.dragging())
                {
                    s.widgets.select_popup(Some(&app));
                }
            },
        );
    }

    /// Close every dock's strip and menu, and the widget's popup.
    fn close_popups(&mut self, core: &Core<Self>) {
        self.hide_widget(core);
        self.close_strips(core);
        self.close_menus(core);
    }

    // -- dragging --------------------------------------------------------------

    /// The app whose icon a left press on the dock on `output` went down
    /// on, if the pointer, now at (`x`, `y`), moved far enough to drag it.
    fn drag_due(&self, output: &WlOutput, x: f64, y: f64) -> Option<String> {
        let p = self.pressed.as_ref()?;
        let (dx, dy) = (x - p.at.0, y - p.at.1);
        let far = dx.hypot(dy) > self.config.drag_threshold as f64;
        (p.output == *output && p.button == BTN_LEFT && far).then(|| p.app_id.clone())
    }

    /// The dragged icon was dropped on the dock on `output`: change the
    /// pins as the drop says, and show them at once.
    fn drop_icon(&mut self, core: &Core<Self>, output: &WlOutput) {
        let dropped = self.dock_mut(output).and_then(DockView::end_drag);
        if let Some((key, drop, zone)) = dropped {
            let mut pins = self.pins.clone();
            if drag::apply(&mut pins, Self::pin_of(&key, zone), drop) {
                self.pins = pins;
                self.save_pins();
                self.rebuild(core, None);
            }
        }
        self.release_keyboard(output);
    }

    /// The pin of the item with key `key`: a folder's path, a standalone
    /// widget (in `zone`), or an app.
    fn pin_of(key: &str, zone: Zone) -> Pin {
        if let Some(name) = dock::widget_name(key) {
            return Pin::Widget(name.to_string(), zone);
        }
        match dock::folder_pin(key) {
            Some(path) => Pin::Path(path.to_string()),
            None => Pin::App(key.to_string()),
        }
    }

    /// Cancel every drag (the icons fly back), and give back the keyboard
    /// the press brought.
    fn cancel_drags(&mut self) {
        for dock in self.docks.iter_mut().filter(|d| d.dragging()) {
            dock.cancel_drag();
            dock.release_keyboard();
        }
    }

    // -- the Trash -------------------------------------------------------------

    fn open_trash(&mut self, core: &Core<Self>) {
        let command = launch::path_command(&self.config.files.open_command, trash::URI);
        let output = Output::Prefixed("open-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("open-command: {e}");
        }
    }

    fn empty_trash(&mut self, core: &Core<Self>) {
        let command = &self.config.trash.empty_command;
        let output = Output::Prefixed("trash-empty-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, command, output) {
            warn!("trash-empty-command: {e}");
        }
    }

    // -- folders ---------------------------------------------------------------

    /// Run `open-command` for `path`.
    fn open_path(&mut self, core: &Core<Self>, path: &Path) {
        let command =
            launch::path_command(&self.config.files.open_command, &path.to_string_lossy());
        let output = Output::Prefixed("open-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("open-command: {e}");
        }
    }

    /// Have the worker list the folder `item` on the dock on `output`,
    /// clicked by the press with `serial` (None: from the keyboard, with
    /// the first entry highlighted, stepped on `steps` more); its popup
    /// opens when the listing arrives.
    fn list_folder(
        &mut self,
        item: &Item,
        output: &WlOutput,
        serial: Option<u32>,
        steps: Option<usize>,
    ) {
        let (Some(pin), Some(assets)) = (item.folder(), &self.assets) else {
            return;
        };
        let settings = self.config.folder(pin).cloned().unwrap_or_default();
        let tag = next_tag();
        assets.folder(
            tag,
            FolderRequest {
                dir: pins::expand(pin),
                sort: settings.sort.into(),
                count: settings.count as usize,
            },
        );
        self.pending_folder = Some(PendingFolder {
            output: output.clone(),
            key: item.app_id.clone(),
            serial,
            tag,
            view: settings.view,
            steps,
        });
    }

    /// A folder's listing arrived: open its popup, if it is still wanted.
    fn folder_listed(
        &mut self,
        core: &Core<Self>,
        tag: u64,
        dir: &Path,
        entries: Result<Vec<FileEntry>, String>,
    ) {
        let Some(pending) = self.pending_folder.take_if(|p| p.tag == tag) else {
            return;
        };
        let entries = entries.unwrap_or_else(|e| {
            warn!("{}: {e}", dir.display());
            Vec::new()
        });
        let output = pending.output.clone();
        // Only one grabbing popup at a time: the others go at once.
        for dock in self.docks.iter_mut() {
            dock.drop_strip(core);
            if dock.output != output {
                dock.drop_menu(core);
            }
        }
        let grab = core.input.seat().zip(pending.serial);
        let key = pending.key.clone();
        let opened = (self.dock_mut(&output))
            .is_some_and(|d| d.open_folder(core, &key, dir, &entries, pending.view, grab));
        match (opened, pending.steps) {
            // From the keyboard: the first entry lit, and on.
            (true, Some(steps)) => {
                if let Some(dock) = self.dock_mut(&output) {
                    for _ in 0..=steps {
                        dock.menu_step(false);
                    }
                }
            }
            (true, None) => {}
            (false, Some(_)) => {
                if let Some(keys) = &mut self.keys {
                    keys.mode.closed();
                }
                self.keys_due(core);
            }
            (false, None) => self.release_keyboard(&output),
        }
    }

    /// A file's row was dragged out of a folder's popup on `output`:
    /// offer the file to whoever it is dropped on.
    fn drag_out(&mut self, core: &mut Core<Self>, output: &WlOutput, event: MenuEvent) {
        let MenuEvent::DragOut {
            path,
            serial,
            surface,
            icon,
            scale,
        } = event
        else {
            return;
        };
        let uris = files::uri_list(std::slice::from_ref(&path));
        let data = vec![
            (URI_LIST.to_string(), uris.into_bytes()),
            (
                "text/plain;charset=utf-8".to_string(),
                path.to_string_lossy().into_owned().into_bytes(),
            ),
        ];
        let icon = icon.as_ref().map(|(buffer, size)| DragIcon {
            buffer,
            scale,
            hotspot: ((size / 2.0) as i32, (size / 2.0) as i32),
        });
        let actions = DndAction::Copy | DndAction::Move;
        match core.start_drag(&surface, serial, data, actions, icon) {
            Ok(()) => {
                self.dragging_out = Some(output.clone());
                if let Some(dock) = self.dock_mut(output) {
                    dock.hold_menu(core);
                }
            }
            Err(e) => warn!("cannot drag {}: {e}", path.display()),
        }
    }

    // -- the menu --------------------------------------------------------------

    /// Open the menu of the item `pressed` was on, once its desktop entry
    /// is known.
    fn open_menu(&mut self, core: &Core<Self>, pressed: Pressed) {
        let app_id = pressed.app_id.clone();
        let Some(item) = (self.docks.iter())
            .find(|d| d.output == pressed.output)
            .and_then(|d| d.item(&app_id).cloned())
        else {
            self.pending_menu = None;
            return;
        };
        if item.is_app() && !app_id.is_empty() && !self.entries.contains_key(&app_id) {
            if let Some(assets) = &self.assets {
                assets.entry(&app_id);
            }
            self.pending_menu = Some(pressed);
            return;
        }
        self.pending_menu = None;
        let groups = match item.kind {
            Kind::App => self.menu_groups(core, &item),
            Kind::Widget { .. } => menu::widget(),
            Kind::Trash { full } => menu::trash(full),
            Kind::Folder { exists } => {
                let pin = item.folder().unwrap_or_default();
                menu::folder(&pins::expand(pin), exists)
            }
            Kind::Separator => Vec::new(),
        };
        // Only one grabbing popup at a time: the others go at once.
        for dock in self.docks.iter_mut() {
            if dock.output != pressed.output {
                dock.drop_strip(core);
                dock.drop_menu(core);
            }
        }
        let grab = core.input.seat().map(|seat| (seat, pressed.serial));
        let opened = (self.dock_mut(&pressed.output))
            .is_some_and(|dock| dock.open_menu(core, &app_id, groups, grab));
        if !opened {
            self.release_keyboard(&pressed.output);
        }
    }

    /// The rows of `item`'s menu.
    fn menu_groups(&self, core: &Core<Self>, item: &Item) -> Vec<menu::Group> {
        let app = self.config.app(&item.app_id);
        let entry = self.entries.get(&item.app_id).and_then(Option::as_ref);
        let desktop: &[DesktopAction] = match (entry, app.is_none_or(|a| a.desktop_actions)) {
            (Some(e), true) => &e.actions,
            _ => &[],
        };
        let focused = (core.focus_stream.running())
            .then(|| core.mru.first())
            .flatten();
        menu::build(&menu::Source {
            item,
            focused,
            desktop,
            actions: app.map_or(&[], |a| &a.actions),
            can_launch: self.launch_command(&item.app_id, entry).is_ok(),
            can_close: self.config.integration.close_command.is_some(),
        })
    }

    /// Do what the picked row of `app_id`'s menu says.
    fn menu_action(&mut self, core: &Core<Self>, app_id: &str, action: Action) {
        match action {
            Action::Activate(id) => self.activate(core, &id),
            Action::NewWindow => self.launch(core, app_id),
            Action::Desktop(action) => self.launch_action(core, app_id, &action),
            Action::Command(command) => self.app_command(core, app_id, &command, "action"),
            Action::Pin => self.set_pinned(app_id, true),
            Action::Unpin => self.set_pinned(app_id, false),
            Action::Close(ids) => {
                for id in ids {
                    self.close_window(core, &id);
                }
            }
            Action::OpenTrash => self.open_trash(core),
            Action::EmptyTrash => self.empty_trash(core),
            Action::Open(path) => self.open_path(core, &path),
        }
    }

    /// Keep the item `key` (an app or a folder) in the dock, or not, and
    /// write the pins file.
    fn set_pinned(&mut self, key: &str, pinned: bool) {
        self.keep(Self::pin_of(key, Zone::Apps), pinned);
    }

    /// Keep `pin` in the dock, or not, and write the pins file.
    fn keep(&mut self, pin: Pin, pinned: bool) {
        let changed = match pinned {
            true => pins::pin(&mut self.pins, pin),
            false => pins::unpin(&mut self.pins, &pin),
        };
        if changed {
            self.dirty = true;
            self.save_pins();
        }
    }

    /// Write the pins file.
    fn save_pins(&self) {
        match &self.pins_path {
            // The watch reads it back, and finds nothing new.
            Some(path) => {
                if let Err(e) = pins::write(path, &self.pins) {
                    warn!("{}: {e}", path.display());
                }
            }
            None => warn!("no state directory: pins are not kept"),
        }
    }

    /// `app_id`'s windows for the launcher's actions, the most recently
    /// focused first.
    fn launcher_windows(core: &Core<Self>, app_id: &str) -> Vec<actions::Window> {
        let mut windows: Vec<actions::Window> = (Self::toplevels(core, None).into_iter())
            .filter(|t| t.app_id == app_id)
            .map(|t| actions::Window {
                id: t.identifier,
                title: t.title,
            })
            .collect();
        core.mru.sort(&mut windows, |w| &w.id);
        windows
    }

    /// The IDs of `app_id`'s windows, and the most recent of them.
    fn app_windows(core: &Core<Self>, app_id: &str) -> (Vec<String>, Option<String>) {
        let ids: Vec<String> = (Self::toplevels(core, None).into_iter())
            .filter(|t| t.app_id == app_id)
            .map(|t| t.identifier)
            .collect();
        let recent = (core.mru)
            .most_recent(ids.iter().map(String::as_str))
            .map(str::to_string)
            .or_else(|| ids.first().cloned());
        (ids, recent)
    }

    /// Run an app command of `app_id` (`key` names where it came from).
    fn app_command(&mut self, core: &Core<Self>, app_id: &str, template: &str, key: &str) {
        let (ids, recent) = Self::app_windows(core, app_id);
        let command = launch::app_command(template, app_id, recent.as_deref(), &ids);
        let output = Output::Prefixed(key.into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("{key}: {e}");
        }
    }

    fn close_window(&mut self, core: &Core<Self>, id: &str) {
        let Some(template) = self.config.integration.close_command.as_deref() else {
            return;
        };
        let command = launch::window_command(template, id);
        let output = Output::Prefixed("close-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("close-command: {e}");
        }
    }

    /// The dock on `output` handled a click without opening a popup: it
    /// gives the keyboard focus back.
    fn release_keyboard(&mut self, output: &WlOutput) {
        if let Some(dock) = self.dock_mut(output) {
            dock.release_keyboard();
        }
    }

    /// Activate the window `id`; during keyboard activation, the mode
    /// ends first and it is activated once the mode's surface is gone.
    fn activate(&mut self, core: &Core<Self>, id: &str) {
        if self.keys.is_some() {
            self.end_keys(core);
            self.pending_keys.push(KeysRun::Activate(id.to_string()));
            return;
        }
        let Some(template) = self.config.integration.activate_command.as_deref() else {
            if !std::mem::replace(&mut self.warned_activate, true) {
                warn!("set activate-command to focus windows from the dock");
            }
            return;
        };
        let command = launch::window_command(template, id);
        let output = Output::Prefixed("activate-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("activate-command: {e}");
        }
    }

    /// How to launch `app_id` (whose desktop entry is `entry`): its own
    /// `launch-command`, else the global one, else the entry's command.
    fn launch_command(&self, app_id: &str, entry: Option<&AppEntry>) -> Result<Command, String> {
        let own = self
            .config
            .app(app_id)
            .and_then(|a| a.launch_command.as_deref());
        let template = own.or(self.config.integration.launch_command.as_deref());
        launch::launch(app_id, entry, template, &self.config.integration.terminal)
    }

    /// Launch `app_id`, once its desktop entry is known.
    fn launch(&mut self, core: &Core<Self>, app_id: &str) {
        let Some(entry) = self.entries.get(app_id) else {
            if !self.pending_launches.iter().any(|a| a == app_id) {
                self.pending_launches.push(app_id.to_string());
            }
            if let Some(assets) = &self.assets {
                assets.entry(app_id);
            }
            return;
        };
        let command = self.launch_command(app_id, entry.as_ref());
        self.run_launch(core, app_id, command);
    }

    /// Launch `app_id`'s desktop action `action`: like the app, with the
    /// action's command. `launch-command` is only used if it runs
    /// `{exec}`; anything else would launch the app instead.
    fn launch_action(&mut self, core: &Core<Self>, app_id: &str, action: &DesktopAction) {
        let Some(Some(entry)) = self.entries.get(app_id) else {
            return warn!("cannot launch {app_id}: its desktop entry is gone");
        };
        let entry = entry.with_action(action);
        let template =
            (self.config.integration.launch_command.as_deref()).filter(|t| spawn::uses(t, "exec"));
        let command = launch::launch(
            app_id,
            Some(&entry),
            template,
            &self.config.integration.terminal,
        );
        self.run_launch(core, app_id, command);
    }

    /// Run a launch `command` of `app_id`, and bounce its icon.
    fn run_launch(&mut self, core: &Core<Self>, app_id: &str, command: Result<Command, String>) {
        let result = match &command {
            Ok(Command::Argv(argv)) => spawn::argv(&core.loop_handle, argv, Output::Inherit),
            Ok(Command::Shell(line)) => spawn::shell(&core.loop_handle, line, Output::Inherit),
            Err(e) => return warn!("cannot launch {app_id}: {e}"),
        };
        if let Err(e) = result {
            return warn!("cannot launch {app_id}: {e}");
        }
        self.record_launch(app_id);
        self.start_bounce(core, app_id);
    }

    /// Bounce `app_id`'s icon until a new window of it appears, or for
    /// `launch-timeout`.
    fn start_bounce(&mut self, core: &Core<Self>, app_id: &str) {
        let timeout = Duration::from_millis(self.config.integration.launch_timeout.into());
        if timeout.is_zero() {
            return;
        }
        if let Some(old) = self.launching.iter().position(|l| l.app_id == app_id) {
            let old = self.launching.remove(old);
            if let Some(token) = old.timeout {
                core.loop_handle.remove(token);
            }
        }
        let before = (Self::toplevels(core, None).into_iter())
            .filter(|t| t.app_id == app_id)
            .map(|t| t.identifier)
            .collect();
        let id = app_id.to_string();
        let token = core.timer(timeout, move |app: &mut Self, _| {
            app.launching.retain(|l| l.app_id != id);
            app.launching_changed();
        });
        self.launching.push(Launch {
            app_id: app_id.to_string(),
            before,
            timeout: token.ok(),
        });
        self.launching_changed();
    }

    /// End the launches whose app got a new window.
    fn end_launches(&mut self, core: &Core<Self>, toplevels: &[Toplevel]) {
        let before = self.launching.len();
        self.launching.retain_mut(|l| {
            let done = (toplevels.iter())
                .any(|t| t.app_id == l.app_id && !l.before.contains(&t.identifier));
            if done && let Some(token) = l.timeout.take() {
                core.loop_handle.remove(token);
            }
            !done
        });
        if self.launching.len() != before {
            self.launching_changed();
        }
    }

    fn launching_changed(&mut self) {
        let apps: Vec<String> = self.launching.iter().map(|l| l.app_id.clone()).collect();
        for dock in &mut self.docks {
            dock.set_launching(&apps);
        }
    }

    // -- drops from outside ----------------------------------------------------

    /// The display name of `app_id`: its `[app.<id>]` name, else its
    /// desktop entry's, else the ID.
    fn app_name(&self, app_id: &str) -> String {
        let own = self.config.app(app_id).and_then(|a| a.name.clone());
        let entry = self.entries.get(app_id).and_then(Option::as_ref);
        (own.or_else(|| entry.map(|e| e.name.clone())))
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| app_id.to_string())
    }

    /// How `app_id` opens `paths`; Err if it can't (or its desktop entry
    /// isn't known yet).
    fn open_commands(&self, app_id: &str, paths: &[PathBuf]) -> Result<Vec<Command>, String> {
        let entry: Option<&AppEntry> = match self.entries.get(app_id) {
            Some(entry) => entry.as_ref(),
            None => return Err("its desktop entry isn't known yet".into()),
        };
        let app = self.config.app(app_id);
        let own = app.and_then(|a| a.launch_command.as_deref());
        let template = own.or(self.config.integration.launch_command.as_deref());
        let drop = app.and_then(|a| a.drop.as_deref());
        launch::open_with(
            app_id,
            entry,
            drop,
            template,
            &self.config.integration.terminal,
            paths,
        )
    }

    /// Whether the Trash takes `paths`: none is in it already.
    fn trash_takes(&self, paths: &[PathBuf]) -> bool {
        let root = self.trash_dir.as_deref().and_then(Path::parent);
        root.is_some_and(|root| !paths.iter().any(|p| p.starts_with(root)))
    }

    /// Whether `paths` go into the folder `dest` by moving: as the
    /// compositor picked, else if they are on its file system.
    fn moves(dest: &Path, paths: &[PathBuf], action: DndAction) -> bool {
        if action.contains(DndAction::Move) {
            return true;
        }
        if action.contains(DndAction::Copy) {
            return false;
        }
        let dev = |p: &Path| std::fs::metadata(p).ok().map(|m| m.dev());
        let dest = dev(dest);
        dest.is_some() && paths.iter().all(|p| dev(p) == dest)
    }

    /// What the items of `dock` do with `payload`, and their labels.
    fn offer_for(&self, dock: &DockView, payload: &Payload, action: DndAction) -> Offer {
        let paths = &payload.paths;
        let mut takers = Vec::new();
        for item in dock.items() {
            let label = match item.kind {
                Kind::App if !item.app_id.is_empty() => (self.open_commands(&item.app_id, paths))
                    .ok()
                    .map(|_| format!("Open with {}", self.app_name(&item.app_id))),
                Kind::Folder { exists: true } => item.folder().and_then(|pin| {
                    let dest = pins::expand(pin);
                    let verb = match Self::moves(&dest, paths, action) {
                        true => "Move to",
                        false => "Copy to",
                    };
                    (drag::folder_takes(&dest, paths))
                        .then(|| format!("{verb} {}", folder::name(pin, &dest)))
                }),
                Kind::Trash { .. } => self.trash_takes(paths).then(|| "Move to Trash".into()),
                _ => None,
            };
            if let Some(label) = label {
                takers.push((item.app_id.clone(), label));
            }
        }
        Offer {
            takers,
            pin: payload.folders.then(|| "Keep in Dock".into()),
        }
    }

    /// Tell the dock under a drag from outside what it takes, and accept
    /// to match.
    fn refresh_offer(&mut self, core: &Core<Self>) {
        let Some(incoming) = &self.incoming else {
            return;
        };
        let output = incoming.output.clone();
        let offer = (incoming.payload.as_ref())
            .zip(self.docks.iter().find(|d| d.output == output))
            .map(|(payload, dock)| self.offer_for(dock, payload, core.dnd.action()));
        if let Some(dock) = self.dock_mut(&output) {
            dock.set_offer(offer);
        }
        self.sync_accept(core);
    }

    /// Accept the drag from outside as its dock's target takes it, or
    /// refuse it.
    fn sync_accept(&mut self, core: &Core<Self>) {
        let Some(incoming) = &self.incoming else {
            return;
        };
        let dock = self.docks.iter().find(|d| d.output == incoming.output);
        let target =
            (dock.and_then(DockView::incoming_target).cloned()).unwrap_or(DropTarget::Nothing);
        let (copy, both) = (DndAction::Copy, DndAction::Copy | DndAction::Move);
        let (mime, actions, preferred) = match &target {
            DropTarget::Nothing => (None, DndAction::empty(), DndAction::empty()),
            DropTarget::Pin(_) => (Some(URI_LIST), copy, copy),
            // The dock does the work: the source keeps its files.
            DropTarget::Open(_) | DropTarget::Trash => (Some(URI_LIST), both, copy),
            DropTarget::Into(key) => {
                let dest = dock::folder_pin(key).map(pins::expand).unwrap_or_default();
                let paths = incoming.payload.as_ref().map_or(&[][..], |p| &p.paths[..]);
                let preferred = match Self::moves(&dest, paths, DndAction::empty()) {
                    true => DndAction::Move,
                    false => copy,
                };
                (Some(URI_LIST), both, preferred)
            }
        };
        let wanted = Some((target, preferred));
        if incoming.accepted == wanted {
            return;
        }
        core.dnd.accept(mime, actions, preferred);
        if let Some(incoming) = &mut self.incoming {
            incoming.accepted = wanted;
        }
    }

    /// The uri-list of the drag `id` arrived.
    fn dnd_data(&mut self, core: &Core<Self>, id: u64, data: Vec<u8>) {
        let Some(incoming) = (self.incoming.as_mut()).filter(|i| i.id == id) else {
            return;
        };
        let text = String::from_utf8_lossy(&data);
        incoming.payload = files::parse_uri_list(&text).map(Payload::new);
        self.refresh_offer(core);
    }

    /// Do what dropping `payload` on `target` of the dock on `output` does.
    fn drop_paths(
        &mut self,
        core: &Core<Self>,
        output: &WlOutput,
        target: DropTarget,
        payload: Payload,
        action: DndAction,
    ) {
        let paths = &payload.paths;
        let run = |command: &str, key: &str| {
            let out = Output::Prefixed(key.into());
            if let Err(e) = spawn::shell(&core.loop_handle, command, out) {
                warn!("{key}: {e}");
            }
        };
        match target {
            DropTarget::Nothing => {}
            DropTarget::Pin(k) => {
                let mut pins = self.pins.clone();
                let mut changed = false;
                for (i, path) in paths.iter().enumerate() {
                    changed |= pins::place(&mut pins, pins::for_path(path), k + i);
                }
                if changed {
                    self.pins = pins;
                    self.save_pins();
                    self.rebuild(core, None);
                }
            }
            DropTarget::Open(app_id) => {
                let commands = match self.open_commands(&app_id, paths) {
                    Ok(commands) => commands,
                    Err(e) => return warn!("cannot open the files with {app_id}: {e}"),
                };
                for command in &commands {
                    let result = match command {
                        Command::Argv(argv) => {
                            spawn::argv(&core.loop_handle, argv, Output::Inherit)
                        }
                        Command::Shell(line) => {
                            spawn::shell(&core.loop_handle, line, Output::Inherit)
                        }
                    };
                    if let Err(e) = result {
                        return warn!("cannot open the files with {app_id}: {e}");
                    }
                }
                // A running app opens them in a window it may have.
                let running = (self.docks.iter())
                    .find(|d| d.output == *output)
                    .and_then(|d| d.item(&app_id))
                    .is_some_and(|i| !i.windows.is_empty() || !i.elsewhere.is_empty());
                if !running {
                    self.start_bounce(core, &app_id);
                }
            }
            DropTarget::Into(key) => {
                let Some(dest) = dock::folder_pin(&key).map(pins::expand) else {
                    return;
                };
                let (template, name) = match Self::moves(&dest, paths, action) {
                    true => (&self.config.files.move_command, "move-command"),
                    false => (&self.config.files.copy_command, "copy-command"),
                };
                run(&launch::paths_command(template, paths, Some(&dest)), name);
            }
            DropTarget::Trash => {
                let command = launch::paths_command(&self.config.trash.command, paths, None);
                run(&command, "trash-command");
            }
        }
    }

    // -- the worker ------------------------------------------------------------

    /// A worker result: into the cache, then to the docks.
    fn worker_done(&mut self, core: &Core<Self>, done: Done) {
        let Some(assets) = &self.assets else { return };
        assets.received(&done);
        match done {
            Done::Icon { key, image } => {
                (self.cache.borrow_mut()).insert_icon(&core.g, key.clone(), &image, &core.qh);
                self.trim = true;
                for dock in &mut self.docks {
                    dock.icon_ready(&key);
                }
                if let Some(launcher) = &mut self.launcher {
                    launcher.icon_ready();
                }
            }
            Done::Named { key, image } => {
                let mut cache = self.cache.borrow_mut();
                cache.insert_named_icon(&core.g, key, image.as_deref(), &core.qh);
                drop(cache);
                self.trim = true;
                for dock in &mut self.docks {
                    dock.named_icon_ready();
                }
                if let Some(launcher) = &mut self.launcher {
                    launcher.icon_ready();
                }
            }
            Done::Label { key, pill } => {
                (self.cache.borrow_mut()).insert_label(&core.g, key.clone(), &pill, &core.qh);
                self.trim = true;
                for dock in &mut self.docks {
                    dock.label_ready(&key);
                }
            }
            // Re-rendered icons replace the old ones as they arrive.
            Done::IconsReloaded => {
                self.entries.clear();
                self.prefetch_entries();
                for dock in &self.docks {
                    dock.request_assets(true);
                }
                assets.apps();
            }
            Done::Entry { app_id, entry } => {
                self.entries.insert(app_id.clone(), entry);
                self.share_app_names(&app_id);
                // The app may open the files of a drag over the dock.
                self.refresh_offer(core);
                if let Some(i) = self.pending_launches.iter().position(|a| *a == app_id) {
                    self.pending_launches.remove(i);
                    self.launch(core, &app_id);
                }
                if let Some(pending) = self.pending_menu.take() {
                    match pending.app_id == app_id {
                        true => self.open_menu(core, pending),
                        false => self.pending_menu = Some(pending),
                    }
                }
            }
            Done::Pill { tag, request, pill } => {
                let launcher = self.launcher.as_mut();
                if launcher.is_some_and(|l| l.pill_ready(tag, &request, &pill)) {
                    return;
                }
                for dock in &mut self.docks {
                    if dock.pill_ready(tag, &request, &pill) {
                        break;
                    }
                }
            }
            Done::Bubble { tag, bubble, image } => {
                for dock in &mut self.docks {
                    if dock.bubble_ready(tag, &bubble, &image) {
                        break;
                    }
                }
            }
            Done::Folder {
                tag,
                request,
                entries,
            } => self.folder_listed(core, tag, &request.dir, entries),
            Done::FileType { path, types, assoc } => {
                self.file_type_ready(core, path, FileType { types, assoc });
            }
            Done::FileIcons { path, icons } => {
                if let Some(launcher) = &mut self.launcher {
                    launcher.file_icons_ready(path, icons);
                }
            }
            Done::Apps(apps) => {
                self.apps = apps;
                if (self.launcher.as_ref()).is_some_and(|l| l.query().scope() == Scope::Apps) {
                    self.rank_apps(false);
                } else {
                    self.show_web(true);
                }
            }
            Done::Ui {
                tag,
                request,
                rendered,
            } => {
                // A widget's popup, or one of its live layers.
                for d in &mut self.docks {
                    if !d.widget_ready(tag, &request, &rendered) {
                        continue;
                    }
                    // Its live layers are painted again when they are due.
                    if let Some((id, due)) = d.widget_tick_due() {
                        let output = d.output.clone();
                        let _ = core.timer(due, move |s: &mut Self, _| {
                            if let Some(d) = s.dock_mut(&output) {
                                d.widget_tick(id);
                            }
                        });
                    }
                    break;
                }
            }
            // A widget's cover art (or any remote image) is in: what
            // shows it is painted again.
            Done::ImageFetched { src } => {
                if self.widgets.image_arrived(&src) {
                    self.show_widget_icons();
                }
                for d in &mut self.docks {
                    d.image_arrived(&src);
                }
            }
            Done::Paper { .. } | Done::Doc { .. } => {}
        }
    }
}

impl Program for WlDock {
    fn toplevel_new(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(t) = core.toplevel(handle) {
            self.hold_new_window(core, &t.identifier);
        }
        self.dirty = true;
    }

    fn toplevel_update(&mut self, _: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.dirty = true;
    }

    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(t) = core.toplevel(handle) {
            self.placement.forget(&t.identifier);
        }
        self.rebuild(core, Some(handle));
    }

    fn keyboard_leave(&mut self, core: &mut Core<Self>, surface: &WlSurface) {
        // Without the keyboard, the mode can't see the release.
        if self
            .keys
            .as_ref()
            .is_some_and(|k| k.view.surface() == surface)
        {
            self.cancel_keys(core);
        }
    }

    fn modifiers(&mut self, core: &mut Core<Self>, modifiers: &Modifiers) {
        let focus = core.input.keyboard_focus();
        if self
            .keys
            .as_ref()
            .is_some_and(|k| Some(k.view.surface()) == focus)
        {
            self.keys_modifiers(core, modifiers);
        }
    }

    fn focus_changed(&mut self, _: &mut Core<Self>) {
        let searching =
            (self.launcher.as_ref()).is_some_and(|l| l.query().scope() == Scope::Windows);
        if self.config.active_indicator || searching {
            self.dirty = true;
        }
    }

    const DATA_DEVICE: bool = true;

    /// A drag entered: if it is on a dock and offers files, read them,
    /// so the dock can show what a drop would do.
    fn dnd_enter(
        &mut self,
        core: &mut Core<Self>,
        surface: &WlSurface,
        (x, y): (f64, f64),
        mime_types: &[String],
    ) {
        self.incoming = None;
        let Some(output) = (self.docks.iter())
            .find(|d| d.surface() == surface)
            .map(|d| d.output.clone())
        else {
            return core
                .dnd
                .accept(None, DndAction::empty(), DndAction::empty());
        };
        let id = next_tag();
        self.incoming = Some(Incoming {
            id,
            output: output.clone(),
            payload: None,
            accepted: None,
        });
        if let Some(dock) = self.dock_mut(&output) {
            dock.dnd_enter(core, x, y);
        }
        if mime_types.iter().any(|m| m == URI_LIST) {
            let received = core.dnd_receive(URI_LIST, move |app: &mut Self, core, data| {
                app.dnd_data(core, id, data);
            });
            if let Err(e) = received {
                warn!("cannot read the drag: {e}");
            }
        }
        self.sync_accept(core);
    }

    fn dnd_motion(&mut self, core: &mut Core<Self>, (x, y): (f64, f64)) {
        let Some(output) = self.incoming.as_ref().map(|i| i.output.clone()) else {
            return;
        };
        if let Some(dock) = self.dock_mut(&output) {
            dock.dnd_motion(x, y);
        }
        self.sync_accept(core);
    }

    fn dnd_leave(&mut self, core: &mut Core<Self>) {
        if let Some(incoming) = self.incoming.take()
            && let Some(dock) = self.dock_mut(&incoming.output)
        {
            dock.dnd_leave(core);
        }
    }

    fn dnd_drop(&mut self, core: &mut Core<Self>) {
        if let Some(incoming) = self.incoming.take() {
            let target = (self.docks.iter())
                .find(|d| d.output == incoming.output)
                .and_then(DockView::incoming_target)
                .cloned();
            if let (Some(target), Some(payload)) = (target, incoming.payload) {
                let action = core.dnd.action();
                self.drop_paths(core, &incoming.output, target, payload, action);
            }
            if let Some(dock) = self.dock_mut(&incoming.output) {
                dock.dnd_leave(core);
            }
        }
        core.dnd.finish();
    }

    /// Moving or copying into a folder: its label says which.
    fn dnd_action(&mut self, core: &mut Core<Self>, _: DndAction) {
        self.refresh_offer(core);
    }

    /// A file dragged out of a folder's popup was dropped (or not): the
    /// popup can go.
    fn drag_source_done(&mut self, _: &mut Core<Self>, _: Option<DndAction>) {
        if let Some(output) = self.dragging_out.take()
            && let Some(dock) = self.dock_mut(&output)
        {
            dock.release_menu();
        }
    }

    fn output_new(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_update(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_removed(&mut self, core: &mut Core<Self>, output: &WlOutput) {
        if self.keys.as_ref().and_then(|k| k.output.as_ref()) == Some(output) {
            self.cancel_keys(core);
        }
        if self.launcher.as_ref().and_then(LauncherView::output) == Some(output) {
            self.cancel_launcher(core);
            self.drop_launcher();
        }
        self.docks.retain(|d| d.output != *output);
    }

    /// The compositor closed a dock, e.g. because its output is going
    /// away. It comes back if the outputs or the config change. Closing
    /// the launcher cancels it, and closing the activate surface ends
    /// keyboard activation.
    fn layer_closed(&mut self, core: &mut Core<Self>, layer: &LayerSurface) {
        if self.keys.as_ref().is_some_and(|k| k.view.is(layer)) {
            self.cancel_keys(core);
        }
        if self.launcher.as_ref().is_some_and(|l| l.is(layer)) {
            self.cancel_launcher(core);
            self.drop_launcher();
        }
        self.docks.retain(|d| d.surface() != layer.wl_surface());
    }

    fn layer_configure(
        &mut self,
        _: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
        let (width, height) = configure.new_size;
        if let Some(keys) = (self.keys.as_mut()).filter(|k| k.view.is(layer)) {
            keys.view.configure();
        }
        if let Some(launcher) = (self.launcher.as_mut()).filter(|l| l.is(layer)) {
            launcher.configure(width, height);
        }
        if let Some(dock) = self.dock_on(layer.wl_surface()) {
            dock.configure(width, height);
        }
    }

    fn scale_factor_changed(&mut self, _: &mut Core<Self>, surface: &WlSurface, factor: i32) {
        if let Some(launcher) = (self.launcher.as_mut()).filter(|l| l.surface() == surface) {
            launcher.set_scale(factor);
        }
        if let Some(dock) = self.dock_on(surface) {
            dock.set_scale(factor);
        }
        self.sync_widget_tile();
    }

    /// The launcher's (or the activate) surface is on an output: the dock
    /// there takes it over (or acts).
    fn surface_enter(&mut self, core: &mut Core<Self>, surface: &WlSurface, output: &WlOutput) {
        if self
            .keys
            .as_ref()
            .is_some_and(|k| k.view.surface() == surface)
        {
            self.attach_keys(core, output);
        }
        if self
            .launcher
            .as_ref()
            .is_some_and(|l| l.surface() == surface)
        {
            self.attach_launcher(core, output);
        }
    }

    fn frame(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if (self.launcher.as_ref()).is_some_and(|l| l.surface() == surface) {
            self.files_frame();
        }
        if let Some(launcher) = (self.launcher.as_mut()).filter(|l| l.surface() == surface) {
            launcher.frame_done();
        }
        if let Some(dock) = self.dock_on(surface) {
            dock.frame_done();
        }
    }

    fn popup_configure(&mut self, _: &mut Core<Self>, popup: &Popup, configure: PopupConfigure) {
        if let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_widget_popup(popup)) {
            return dock.widget_configure(&configure);
        }
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_configure(popup, &configure);
        }
    }

    /// The compositor dismissed a strip, menu or widget's popup: a click
    /// on another client.
    fn popup_done(&mut self, core: &mut Core<Self>, popup: &Popup) {
        if self.docks.iter().any(|d| d.has_widget_popup(popup)) {
            return self.hide_widget(core);
        }
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_done(core, popup);
        }
    }

    /// Keys reach the launcher, or the open menu or strip, which have the
    /// keyboard while they are open. Escape cancels a drag (the press gave
    /// the dock the keyboard).
    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let focus = core.input.keyboard_focus();
        // Keyboard activation has the keyboard while it is on.
        if self
            .keys
            .as_ref()
            .is_some_and(|k| Some(k.view.surface()) == focus)
        {
            return self.keys_key(core, event, repeat);
        }
        // The launcher has the keyboard while it is open.
        if (self.launcher.as_ref()).is_some_and(|l| !l.closing() && Some(l.surface()) == focus) {
            return self.launcher_key(core, event, repeat);
        }
        let Some(key) = Key::from_sym(event.keysym) else {
            return;
        };
        if repeat && !key.repeats() {
            return;
        }
        if self.docks.iter().any(DockView::dragging) {
            if key == Key::Close {
                self.cancel_drags();
            }
            return;
        }
        let Some(focus) = core.input.keyboard_focus().cloned() else {
            return;
        };
        if let Some(dock) = (self.docks.iter_mut()).find(|d| d.menu_has_keys(&focus)) {
            if let Some(key) = MenuKey::from_key(key)
                && let Some((app_id, action)) = dock.menu_key(core, key)
            {
                self.menu_action(core, &app_id, action);
            }
            return;
        }
        let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_strip_surface(&focus)) else {
            return;
        };
        if let Some(id) = dock.strip_key(core, key) {
            self.activate(core, &id);
        }
    }

    fn capture_update(
        &mut self,
        _: &mut Core<Self>,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        // Events of captures a dock (or the launcher) dropped find
        // nothing.
        if let Some(l) = (self.launcher.as_mut()).filter(|l| l.has_capture(id)) {
            l.capture_update(id, f);
        } else if let Some(dock) = self.docks.iter_mut().find(|d| d.has_capture(id)) {
            dock.capture_update(id, f);
        }
    }

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
        if let Some(launcher) = (self.launcher.as_mut()).filter(|l| *l.surface() == event.surface) {
            match launcher.pointer(event) {
                LauncherInput::Open(i) if launcher.level().is_some() => {
                    self.launcher_run(core, Some(i))
                }
                LauncherInput::Open(i)
                    if launcher.query().scope() == Scope::Bang
                        && !launcher
                            .match_at(i)
                            .is_some_and(|m| m.id == engines::FALLBACK) =>
                {
                    self.launcher_into(core, Some(i));
                }
                LauncherInput::Open(i) => self.launcher_pick(core, Some(i), false),
                LauncherInput::Back(depth) => self.launcher_back(core, depth),
                LauncherInput::Close => self.cancel_launcher(core),
                LauncherInput::None => {}
            }
            return;
        }
        if let Some(d) = (self.docks.iter_mut()).find(|d| d.has_widget_surface(&event.surface)) {
            if let Some((app, key, event)) = d.widget_pointer(event) {
                let kind = wl_shell_lua::RequestKind::Interaction { key, event };
                self.widgets.notify(&app, kind);
            }
            // On the popup, it stays; off it, it closes unless the
            // pointer comes back (to it or the dock) soon.
            self.widget_grace = self.widget_grace.wrapping_add(1);
            if matches!(event.kind, PointerEventKind::Leave { .. }) {
                self.leave_widget(core);
            }
            return;
        }
        if let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_strip_surface(&event.surface)) {
            if let Some(id) = dock.strip_pointer(core, event) {
                self.activate(core, &id);
            }
            return;
        }
        if let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_menu_surface(&event.surface)) {
            let output = dock.output.clone();
            match dock.menu_pointer(core, event) {
                Some(MenuEvent::Pick(app_id, action)) => self.menu_pick(core, app_id, action),
                Some(drag @ MenuEvent::DragOut { .. }) => self.drag_out(core, &output, drag),
                None => {}
            }
            return;
        }
        let Some(output) = (self.docks.iter())
            .find(|d| *d.surface() == event.surface)
            .map(|d| d.output.clone())
        else {
            return;
        };
        let dragging = (self.docks.iter()).any(|d| d.output == output && d.dragging());
        let (x, y) = event.position;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                // A press on an icon that moves far enough picks it up.
                if !dragging && let Some(app_id) = self.drag_due(&output, x, y) {
                    let draggable = (self.docks.iter())
                        .find(|d| d.output == output)
                        .and_then(|d| d.item(&app_id))
                        .is_some_and(Item::draggable);
                    if draggable {
                        self.pressed = None;
                        self.close_popups(core);
                        if let Some(dock) = self.dock_mut(&output)
                            && dock.start_drag(core, &app_id, x, y)
                        {
                            return;
                        }
                    }
                }
                if let Some(dock) = self.dock_mut(&output) {
                    dock.pointer_motion(core, x, y);
                }
                if !dragging {
                    self.hover_widget(core, &output);
                }
            }
            PointerEventKind::Leave { .. } => {
                self.leave_widget(core);
                if let Some(dock) = self.dock_mut(&output) {
                    // The grab broke: the icon goes back.
                    if dragging {
                        dock.cancel_drag();
                        dock.release_keyboard();
                    }
                    dock.pointer_leave(core);
                }
                self.pressed = None;
            }
            PointerEventKind::Press { button, serial, .. } => {
                // A click on a standalone widget whose popup this closes
                // leaves it closed.
                self.widget_was_open = (self.docks.iter())
                    .find(|d| d.output == output)
                    .and_then(|d| d.widget_item().map(str::to_string));
                self.hide_widget(core);
                self.button(core, &output, button, serial, (x, y), true);
            }
            PointerEventKind::Release { .. } if dragging => self.drop_icon(core, &output),
            PointerEventKind::Release { button, serial, .. } => {
                self.button(core, &output, button, serial, (x, y), false);
            }
            PointerEventKind::Axis { .. } if dragging => {}
            PointerEventKind::Axis { vertical, .. } => {
                self.scroll(core, &output, &vertical);
            }
        }
    }
}
