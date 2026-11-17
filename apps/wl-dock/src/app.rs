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
        keyboard::KeyEvent,
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
use wl_shell_render::files::{self, FileEntry, FolderRequest};
use wl_shell_render::icons::{AppEntry, DesktopAction, IndexedApp};
use wl_shell_render::worker::{Assets, Done};
use wl_shell_ui::Cache;

use crate::actions::{self, Does, FileType, Icon, Subject};
use crate::config::{Config, FolderView, LauncherEnter, Previews, Windows};
use crate::dock::{self, Click, Item, Kind, Order, Placement};
use crate::drag::{self, DropTarget, Payload};
use crate::filesearch::{self, FileSearch, Found};
use crate::folder;
use crate::history::{self, History};
use crate::ipc::DockRequest;
use crate::launch::{self, Command};
use crate::launcher::{self, Act, Scope};
use crate::launcherview::{
    FileMatch, FilesStatus, LauncherInput, LauncherStyle, LauncherView, Match,
};
use crate::menu::{self, Action, MenuKey};
use crate::pins::{self, Pin};
use crate::search::Search;
use crate::strip::Key;
use crate::trash;
use crate::view::{DockView, MenuEvent, Offer, Style};

/// How long the launcher waits for the compositor to say which output it
/// is on before it takes the first bottom dock.
const LAUNCHER_PLACE_TIMEOUT: Duration = Duration::from_millis(500);

/// The mime type of dropped (and dragged out) files.
const URI_LIST: &str = "text/uri-list";

/// Pixels of touchpad scrolling per step of a scroll command.
const SCROLL_STEP: f64 = 50.0;

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
    pressed: Pressed,
    tag: u64,
    view: FolderView,
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
        id: found.rel,
    }
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
    /// A file the launcher opens with an app.
    pending_with: Option<PendingWith>,
    /// A folder the launcher opens a terminal in, and the path its
    /// client is told; run once it gave up the keyboard.
    pending_terminal: Option<(PathBuf, String)>,
    /// An app's action the launcher runs.
    pending_app: Option<PendingApp>,
    files: Option<Files>,
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
            pending_with: None,
            pending_terminal: None,
            pending_app: None,
            files: None,
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
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker and the streams, watch the files, and create the docks.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.start_worker(core)?;
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
                let timer = core.timer(LAUNCHER_PLACE_TIMEOUT, |app: &mut Self, core| {
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
            Some(Scope::Apps) => self.rank_apps(),
            Some(Scope::Files) => self.rank_files(core),
            Some(Scope::Actions) => self.rank_choices(),
            None => {}
        }
        // The query can change without changing the file matches or status
        // (for example, a trailing space). Refresh it independently.
        if let Some(launcher) = &mut self.launcher {
            launcher.sync_texts();
            launcher.kick();
        }
    }

    /// Rank the apps for the launcher's query; the app it meant (picked
    /// for it before) first.
    fn rank_apps(&mut self) {
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
        let query = launcher.query().text();
        let mut ranked = search.rank(apps, query, &boost);
        if let Some(meant) = history.meant(query).filter(|_| learn) {
            history::meant_first(&mut ranked, |i| apps[i].id == meant);
        }
        let slots = (config.launcher.matches as usize).max(1);
        let matches = (ranked.into_iter().take(slots))
            .map(|i| &apps[i])
            .map(|a| Match {
                id: a.id.clone(),
                name: a.name.clone(),
                icon: a.entry.icon.clone(),
                app: None,
                named: Vec::new(),
                matched: search.highlight(&a.name, query),
                file: None,
                window: None,
            })
            .collect();
        launcher.set_matches(matches);
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

    /// A key on the launcher.
    fn launcher_key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let mods = core.input.modifiers;
        let Some(key) = launcher::Key::from(event.keysym, event.utf8.as_deref(), &mods) else {
            return;
        };
        if repeat && !key.repeats() {
            return;
        }
        let trigger = self.config.launcher.files.trigger;
        let Some(l) = &mut self.launcher else { return };
        match launcher::act(key, l.depth(), l.query()) {
            Act::Close => self.cancel_launcher(core),
            Act::Clear => {
                l.query_mut().edit(&launcher::Key::Clear, trigger);
                self.rank_launcher(core);
            }
            Act::Roll(by) => l.roll(by),
            Act::Open { .. } if l.depth() > 0 => self.launcher_run(core, None),
            Act::Open { other } => self.launcher_pick(core, None, other),
            Act::Into => self.launcher_into(core, None),
            Act::Out => {
                let to = l.depth().saturating_sub(1);
                self.launcher_back(to);
            }
            Act::Edit(edit) => {
                let scope = l.query().scope();
                if l.query_mut().edit(&edit, trigger) {
                    // Apps and files never share the drum.
                    if l.query().scope() != scope {
                        l.set_matches(Vec::new());
                    }
                    self.rank_launcher(core);
                }
            }
        }
    }

    /// Go back out of the launcher's levels until `depth` deep, each
    /// restoring what it showed.
    fn launcher_back(&mut self, depth: usize) {
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
        let query = l.query().text().to_string();
        let Some(app) = self.apps.iter().find(|a| a.id == m.id).cloned() else {
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
        let Some(i) = l.selected() else {
            return;
        };
        if l.depth() > 0 {
            let id = l.match_at(i).map(|m| m.id.clone());
            let choice = (l.level().into_iter())
                .flat_map(|level| &level.choices)
                .find(|c| Some(&c.id) == id.as_ref());
            if let Some(Does::Choose(level)) = choice.map(|c| &c.does) {
                let level = level.clone();
                l.enter(level);
                self.rank_choices();
            }
            return;
        }
        let Some(m) = l.match_at(i).cloned() else {
            return;
        };
        let (subject, file_type, pinned, trash) = match &m.file {
            Some(f) => {
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
            None => {
                let Some(app) = self.apps.iter().find(|a| a.id == m.id) else {
                    return;
                };
                let app_id = self.dock_app_id(core, app);
                // As the dock's menu (`menu_groups`).
                let config = self.config.app(&app_id);
                let subject = Subject::App(actions::App {
                    id: app.id.clone(),
                    name: app.name.clone(),
                    desktop: match config.is_none_or(|a| a.desktop_actions) {
                        true => app.entry.actions.clone(),
                        false => Vec::new(),
                    },
                    commands: config.map_or_else(Vec::new, |a| a.actions.clone()),
                    windows: Self::launcher_windows(core, &app_id),
                    launchable: self.launch_command(&app_id, Some(&app.entry)).is_ok(),
                });
                (subject, None, self.pins.contains(&Pin::App(app_id)), false)
            }
        };
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
        };
        let level = actions::actions(&subject, &cx);
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
            l.set_previews(windows, previews == Previews::Live);
            l.enter(level);
        }
        self.rank_choices();
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
        self.file_types.clear();
        self.pending_tab = None;
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
        self.file_types.clear();
        self.pending_tab = None;
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
            || self.pending_with.is_some()
            || self.pending_terminal.is_some()
            || self.pending_app.is_some();
        // The compositor has given the keyboard back before the app is
        // activated (or the file opened), so the app keeps it.
        if pending && let Err(e) = core.conn.roundtrip() {
            warn!("{e}");
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
        let mut cache = self.cache.borrow_mut();
        cache.clear_icons();
        cache.clear_shapes();
        drop(cache);
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
        self.use_streams(core);
        self.watch_trash(core);
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
    /// their menu at once.
    fn prefetch_entries(&self) {
        let Some(assets) = &self.assets else { return };
        let running = self.order.apps().iter().cloned();
        for app_id in pins::apps(&self.pins).into_iter().chain(running) {
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
        let wanted: Vec<(WlOutput, String, i32, Style)> = (core.output_state.outputs())
            .filter_map(|output| {
                let info = core.output_state.info(&output)?;
                let name = info.name?;
                let layout = self.config.dock(&name)?;
                let style = Style::new(&self.config, layout);
                Some((output, name, info.scale_factor.max(1), style))
            })
            .collect();
        self.docks.retain(|dock| {
            (wanted.iter()).any(|(o, name, _, style)| {
                *o == dock.output && *name == dock.name && !dock.style().needs_new_surface(style)
            })
        });
        for (output, name, scale, style) in wanted {
            match self.docks.iter_mut().find(|d| d.output == output) {
                Some(dock) => {
                    dock.set_output_size(Self::output_size(core, &output));
                    dock.set_style(style, reload);
                    dock.set_scale(scale);
                }
                None => match DockView::new(
                    core,
                    output,
                    name,
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
                        self.docks.push(dock);
                    }
                    Err(e) => warn!("failed to create a dock: {e}"),
                },
            }
        }
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

        let pins = pins::apps(&self.pins);
        let focused = (self.config.active_indicator && core.focus_stream.running())
            .then(|| core.mru.first())
            .flatten();
        let streaming = self.output_stream.running();
        // Paths that are files aren't shown: only folders are pinned.
        let folders: Vec<(String, bool)> = (pins::paths(&self.pins).into_iter())
            .filter_map(|p| match std::fs::metadata(pins::expand(&p)) {
                Ok(m) if !m.is_dir() => None,
                Ok(_) => Some((p, true)),
                Err(_) => Some((p, false)),
            })
            .collect();
        let trash = self.config.trash.enabled.then_some(self.trash_full);
        for dock in &mut self.docks {
            let by_output = streaming && dock.style().layout.windows == Windows::Output;
            let mut items = dock::items(&toplevels, &pins, &self.order, focused, |id| {
                self.placement.place(id, &dock.name, by_output)
            });
            dock::add_tail(&mut items, &folders, trash);
            dock.set_items(core, &items);
        }
        // What takes a drag from outside may have changed.
        self.refresh_offer(core);
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
        let Some(item) = self.hovered_item(output) else {
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
        self.close_menus(core);
        match button {
            BTN_LEFT if popup_was => self.release_keyboard(output),
            BTN_LEFT => self.click(core, output, &item, pressed),
            BTN_MIDDLE if item.is_app() => {
                self.close_strips(core);
                self.middle_click(core, &item);
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
            Click::OpenFolder => return self.list_folder(item, pressed),
            Click::Nothing => {}
        }
        self.release_keyboard(output);
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

    /// Close every dock's strip and menu.
    fn close_popups(&mut self, core: &Core<Self>) {
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
        if let Some((key, drop)) = dropped {
            let mut pins = self.pins.clone();
            if drag::apply(&mut pins, Self::pin_of(&key), drop) {
                self.pins = pins;
                self.save_pins();
                self.rebuild(core, None);
            }
        }
        self.release_keyboard(output);
    }

    /// The pin of the item with key `key`: a folder's path, or an app.
    fn pin_of(key: &str) -> Pin {
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

    /// Have the worker list the folder `item` that `pressed` clicked; its
    /// popup opens when the listing arrives.
    fn list_folder(&mut self, item: &Item, pressed: Pressed) {
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
            pressed,
            tag,
            view: settings.view,
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
        let output = pending.pressed.output.clone();
        // Only one grabbing popup at a time: the others go at once.
        for dock in self.docks.iter_mut() {
            dock.drop_strip(core);
            if dock.output != output {
                dock.drop_menu(core);
            }
        }
        let grab = core.input.seat().map(|seat| (seat, pending.pressed.serial));
        let key = pending.pressed.app_id.clone();
        let opened = (self.dock_mut(&output))
            .is_some_and(|d| d.open_folder(core, &key, dir, &entries, pending.view, grab));
        if !opened {
            self.release_keyboard(&output);
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
        self.keep(Self::pin_of(key), pinned);
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

    fn activate(&mut self, core: &Core<Self>, id: &str) {
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
                    self.rank_apps();
                }
            }
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

    fn focus_changed(&mut self, _: &mut Core<Self>) {
        if self.config.active_indicator {
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
        if self.launcher.as_ref().and_then(LauncherView::output) == Some(output) {
            self.cancel_launcher(core);
            self.drop_launcher();
        }
        self.docks.retain(|d| d.output != *output);
    }

    /// The compositor closed a dock, e.g. because its output is going
    /// away. It comes back if the outputs or the config change. Closing
    /// the launcher cancels it.
    fn layer_closed(&mut self, core: &mut Core<Self>, layer: &LayerSurface) {
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
    }

    /// The launcher's surface is on an output: the dock there takes it
    /// over.
    fn surface_enter(&mut self, core: &mut Core<Self>, surface: &WlSurface, output: &WlOutput) {
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
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_configure(popup, &configure);
        }
    }

    /// The compositor dismissed a strip or menu: a click on another
    /// client.
    fn popup_done(&mut self, core: &mut Core<Self>, popup: &Popup) {
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_done(core, popup);
        }
    }

    /// Keys reach the launcher, or the open menu or strip, which have the
    /// keyboard while they are open. Escape cancels a drag (the press gave
    /// the dock the keyboard).
    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        // The launcher has the keyboard while it is open.
        let focus = core.input.keyboard_focus();
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
                LauncherInput::Open(i) if launcher.depth() > 0 => self.launcher_run(core, Some(i)),
                LauncherInput::Open(i) => self.launcher_pick(core, Some(i), false),
                LauncherInput::Back(depth) => self.launcher_back(depth),
                LauncherInput::Close => self.cancel_launcher(core),
                LauncherInput::None => {}
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
                Some(MenuEvent::Pick(app_id, action)) => self.menu_action(core, &app_id, action),
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
            }
            PointerEventKind::Leave { .. } => {
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
