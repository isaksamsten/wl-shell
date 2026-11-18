//! The program: the config, the notes, their arrangement and the context,
//! and the columns showing them.
//!
//! - **Notes.** The notes directory is read into an `Index` and watched
//!   with inotify, the whole tree (`watch_tree`). Any event rescans it
//!   (only changed files are read again), after the loop's dispatch, so a
//!   burst of events is one rescan.
//! - **The arrangement** (the order and the hidden notes) is kept in the
//!   state files (`wl_shell_notes::state`), watched too, so editing them
//!   by hand or with `wl-note hide` shows at once. Renames inside the
//!   notes directory (a file manager, `git mv`) move their entries, so a
//!   renamed note keeps its place; an open note follows its file. Nothing
//!   here rewrites a note to arrange it.
//! - **Context.** The window the notes follow: the most recently focused
//!   open window, from the `mru-event-stream`. Focus on anything else
//!   (wl-note's own surfaces, the dock, a launcher: layer surfaces, which
//!   the stream doesn't report) changes nothing, so clicking a note never
//!   takes it away. Title changes count, and a new context only shows
//!   once it held for `settle` (context.rs). When the context's window
//!   closes, the most recent one left takes over. Without a stream, only
//!   global notes show.
//! - **Outputs.** `output` says where the columns are (column.rs): on the
//!   output it names (else the first), on every output (`all`), or on the
//!   context's (`focused`, from the `output-event-stream`; without it, the
//!   first). Following the focus, the column slides away on the old
//!   output and in on the new one, but not while a note is open in it:
//!   the user put it there. One note is open at a time, on any output.
//! - **The columns** follow the pointer by themselves; they ask for the
//!   rest (`Act`): opening and closing notes, toggling a checkbox,
//!   retitling, pinning and unpinning (written to the file, which inotify
//!   brings back), hiding and reordering (written to the state), opening a
//!   link or a file (`files.open-command`) or the note (`editor.command`), and
//!   making a note. An open note stays in its column whatever the context
//!   does, where it was, until it is closed.
//! - **The socket.** `wl-note new` and `wl-note focus` come over it
//!   (ipc.rs). A new note is attached to the context's app (or global),
//!   written at once, put at the end of the order, and opened in the
//!   editor if it has no text yet, else opened. A note opened by name is
//!   shown again if hidden, and shows even if it doesn't belong to the
//!   context, until it is closed.
//! - **Fonts on the main thread**, for the open note's editable title,
//!   are loaded on a thread of their own at the start.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::shell::wlr_layer::{LayerSurface, LayerSurfaceConfigure};
use wayland_client::protocol::{
    wl_data_device_manager::DndAction, wl_output::WlOutput, wl_surface::WlSurface,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::{Reloader, TreeEvent};
use wl_shell_core::reexports::calloop::{RegistrationToken, channel};
use wl_shell_core::spawn::{self, Output};
use wl_shell_core::stream::{Hooks, LineStream};
use wl_shell_core::{Core, Program, Shell};
use wl_shell_daemon::{Incoming, Listener, Reply};
use wl_shell_log::warn;
use wl_shell_notes::{Index, Note, State, Window, state};
use wl_shell_render::text::Text;
use wl_shell_render::worker::{Assets, Done};

use crate::column::{Act, Column, FileLink, FileTarget, Fonts, Pin, Shown, Style, Timer};
use crate::config::Config;
use crate::context::{Proposal, Settle};
use crate::deck::{self, Group, Item, Look};
use crate::draft::{Draft, SaveBody, read_note, save_body};
use crate::ipc::{NewNote, NoteRequest};
use crate::markdown::{self, Content};
use crate::papers::Papers;
use crate::{bar, create, files, find, list, reorder};

/// Windows whose output is remembered at most: the stream may report
/// windows the toplevel list never announces, so not all are forgotten.
const MAX_PLACED: usize = 4096;
const CLIPBOARD_TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// The window the notes follow.
#[derive(Clone, Debug, PartialEq)]
struct Ctx {
    window: Window,
    /// Its ext identifier, for the output it is on.
    id: String,
}

/// An output a column can go on: it, its name and its scale.
type Target = (WlOutput, String, i32);

struct FileOffer {
    surface: WlSurface,
    at: (f64, f64),
}

struct FileDrop {
    surface: WlSurface,
    dir: PathBuf,
    target: FileTarget,
    app: Option<String>,
}

pub struct WlNote {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The config in use.
    config: Config,
    /// The config file was touched since it was last read.
    config_touched: bool,
    index: Index,
    /// Something in the notes directory changed since the last scan.
    notes_touched: bool,
    /// The notes directory's watch.
    tree: Option<RegistrationToken>,
    /// Renames in the notes directory not followed yet, as (from, to).
    moves: Vec<(PathBuf, PathBuf)>,
    /// Notes with a Move to Trash command in flight (absolute paths).
    trash_pending: HashSet<PathBuf>,
    /// The order and the hidden notes.
    state: State,
    /// Where they are kept; None: nowhere (no `$HOME`), so only for now.
    state_dir: Option<PathBuf>,
    /// The state files were touched since they were last read.
    state_touched: bool,
    /// The worker, started with the program.
    assets: Option<Rc<Assets>>,
    papers: Option<Rc<RefCell<Papers>>>,
    /// The columns: those shown, and those retiring.
    columns: Vec<Column>,
    file_offer: Option<FileOffer>,
    file_drop_serial: u64,
    save_timer: Option<RegistrationToken>,
    /// Drafts whose surfaces were forcibly removed, keyed by absolute path.
    detached_drafts: HashMap<PathBuf, Draft>,
    /// The outputs, the config or the context's output changed since the
    /// columns were placed.
    outputs_dirty: bool,
    /// The window the notes follow; None: only global notes.
    context: Settle<Option<Ctx>>,
    /// When a pending context is due, and the timer that checks.
    settle_timer: Option<(Instant, RegistrationToken)>,
    /// Warned that there is no `mru-event-stream`.
    warned_stream: bool,
    /// The `output-event-stream`, for `output = "focused"`.
    output_stream: LineStream<Self>,
    /// Which output each window is on, by ID, as the stream says.
    window_outputs: HashMap<String, String>,
    /// The notes shown may have changed.
    dirty: bool,
    /// Sheets may have been replaced: destroy the ones no longer shown.
    trim: bool,
    /// The socket `new` and `focus` come over; None if it couldn't be
    /// bound.
    listener: Option<Listener<NoteRequest>>,
    /// The columns' open notes when the notes were last shown: once one
    /// closes, the notes are shown again (it may not belong to the
    /// context), and the columns may move.
    last_open: Vec<Option<String>>,
    /// Fonts for the main thread, once loaded.
    fonts: Fonts,
}

impl WlNote {
    /// Read the config (`explicit`, or the default file).
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        let reloader = Reloader::<Config>::load(explicit).map_err(|e| format!("config: {e}"))?;
        let config = reloader.config().clone();
        Ok(Self {
            index: Index::new(config.notes_dir()),
            config,
            reloader,
            config_touched: false,
            notes_touched: false,
            tree: None,
            moves: Vec::new(),
            trash_pending: HashSet::new(),
            state: State::default(),
            state_dir: None,
            state_touched: false,
            assets: None,
            papers: None,
            columns: Vec::new(),
            file_offer: None,
            file_drop_serial: 0,
            save_timer: None,
            detached_drafts: HashMap::new(),
            outputs_dirty: true,
            context: Settle::new(None),
            settle_timer: None,
            warned_stream: false,
            output_stream: LineStream::new(
                "output-event-stream",
                Hooks {
                    locate: |shell: &mut Shell<Self>| &mut shell.program.output_stream,
                    line: |shell, line| shell.program.output_line(line),
                    changed: |shell| shell.program.outputs_dirty = true,
                },
            ),
            window_outputs: HashMap::new(),
            dirty: true,
            trim: false,
            listener: None,
            last_open: Vec::new(),
            fonts: Rc::new(RefCell::new(None)),
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker and the streams, read and watch the notes and their state,
    /// and show them.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.start_worker(core)?;
        self.load_fonts(core);
        self.watch_config(core);
        self.open_state(core);
        self.open_notes(core);
        self.use_stream(core);
        self.use_output_stream(core);
        // The first context shows at once.
        self.context.set(Self::candidate(core, None));
        match Listener::bind(core) {
            Ok(listener) => self.listener = Some(listener),
            Err(e) => warn!("no `new` or `focus` from the command line: {e}"),
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
        let assets = Rc::new(assets);
        self.papers = Some(Rc::new(RefCell::new(Papers::new(assets.clone()))));
        self.assets = Some(assets);
        Ok(())
    }

    /// Load the fonts for the main thread on a thread of their own.
    fn load_fonts(&mut self, core: &Core<Self>) {
        let (send, fonts) = channel::channel::<Text>();
        let spawned = std::thread::Builder::new()
            .name("wl-note-fonts".into())
            .spawn(move || {
                let _ = send.send(Text::new());
            });
        if let Err(e) = spawned {
            return warn!("no fonts for titles: {e}");
        }
        let inserted =
            core.loop_handle
                .insert_source(fonts, |event, _, shell: &mut Shell<Self>| {
                    if let channel::Event::Msg(text) = event {
                        *shell.program.fonts.borrow_mut() = Some(text);
                        for column in &mut shell.program.columns {
                            column.fonts_ready();
                        }
                    }
                });
        if let Err(e) = inserted {
            warn!("no fonts for titles: {e}");
        }
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

    /// Find the state directory (created if missing), and watch its
    /// files.
    fn open_state(&mut self, core: &Core<Self>) {
        let Some(dir) = state::dir() else {
            return warn!(
                "neither XDG_STATE_HOME nor HOME is set: the order and hidden notes aren't kept"
            );
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("{}: {e}", dir.display());
        }
        for name in [state::ORDER, state::HIDDEN] {
            let path = dir.join(name);
            let watched =
                wl_shell_config::watch_file(&core.loop_handle, &path, |shell: &mut Shell<Self>| {
                    shell.program.state_touched = true;
                });
            if let Err(e) = watched {
                warn!("not watching {}: {e}", path.display());
            }
        }
        self.state_dir = Some(dir);
        self.state_touched = true;
        self.reload_state();
    }

    /// Read the state files again, if they were touched. Notes hidden
    /// from elsewhere (`wl-note hide`) crumple too.
    fn reload_state(&mut self) {
        if !std::mem::take(&mut self.state_touched) {
            return;
        }
        let Some(dir) = &self.state_dir else {
            return;
        };
        let (state, errors) = State::load(dir);
        for e in errors {
            warn!("{e}");
        }
        if state != self.state {
            for p in state.hidden.paths() {
                if self.state.is_hidden(p) {
                    continue;
                }
                let key = p.to_string_lossy();
                for column in &mut self.columns {
                    column.crumple(&key);
                }
            }
            self.state = state;
            self.dirty = true;
        }
    }

    /// Write the order and/or the hidden file.
    fn save_state(&mut self, order: bool, hidden: bool) {
        let Some(dir) = &self.state_dir else {
            return;
        };
        if order && let Err(e) = self.state.save_order(dir) {
            warn!("{}: {e}", dir.join(state::ORDER).display());
        }
        if hidden && let Err(e) = self.state.save_hidden(dir) {
            warn!("{}: {e}", dir.join(state::HIDDEN).display());
        }
    }

    /// Read the notes directory (created if missing) and watch it.
    fn open_notes(&mut self, core: &mut Core<Self>) {
        if let Some(token) = self.tree.take() {
            core.loop_handle.remove(token);
        }
        self.moves.clear();
        let dir = self.config.notes_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("{}: {e}", dir.display());
        }
        let watched = wl_shell_config::watch_tree(
            &core.loop_handle,
            &dir,
            |shell: &mut Shell<Self>, events| shell.program.tree_events(events),
        );
        match watched {
            Ok(token) => self.tree = Some(token),
            Err(e) => warn!("not watching {}: {e}", dir.display()),
        }
        self.index = Index::new(dir);
        self.notes_touched = true;
        self.rescan(core);
    }

    /// Something changed in the notes directory: rescan, and follow the
    /// renames of notes and directories (not those to other names, such
    /// as an editor's backup copy).
    fn tree_events(&mut self, events: Vec<TreeEvent>) {
        for e in events {
            if let TreeEvent::Moved { from, to, dir } = e
                && (dir || is_note(&to))
            {
                self.moves.push((from, to));
            }
        }
        self.notes_touched = true;
    }

    /// Read the notes directory again, and follow the renames in it.
    fn rescan(&mut self, core: &mut Core<Self>) {
        if !std::mem::take(&mut self.notes_touched) {
            return;
        }
        let changes = self.index.scan();
        for rel in &changes.removed {
            if self.trash_pending.contains(&self.index.dir().join(rel)) {
                let key = rel.to_string_lossy();
                for column in &mut self.columns {
                    column.crumple(&key);
                }
            }
        }
        for e in &changes.errors {
            warn!("{e}");
        }
        for rel in changes.added.iter().chain(&changes.changed) {
            if let Some(e) = self.index.get(rel)
                && let Some(error) = &e.note.error
            {
                warn!("{}: {error}", rel.display());
            }
        }
        if !changes.is_empty() {
            self.dirty = true;
        }
        let moves = std::mem::take(&mut self.moves);
        let (mut order, mut hidden) = (false, false);
        for (from, to) in &moves {
            let (o, h) = self.state.rename(from, to);
            (order, hidden) = (order || o, hidden || h);
            // An open note follows its file.
            for i in 0..self.columns.len() {
                let Some(open) = self.columns[i].open_key().map(PathBuf::from) else {
                    continue;
                };
                let Ok(rest) = open.strip_prefix(from) else {
                    continue;
                };
                let new = match rest.as_os_str().is_empty() {
                    true => to.clone(),
                    false => to.join(rest),
                };
                if self.index.get(&open).is_none() && self.index.get(&new).is_some() {
                    let key = new.to_string_lossy().into_owned();
                    let acts = self.columns[i].open(core, &key);
                    if let Some(draft) = &mut self.columns[i].draft {
                        draft.key = key;
                    }
                    let surface = self.columns[i].surface().clone();
                    self.acts(core, &surface, acts);
                    self.dirty = true;
                }
            }
        }
        if !changes.is_empty() {
            self.observe_drafts();
        }
        if order || hidden {
            self.save_state(order, hidden);
            self.dirty = true;
        }
    }

    /// Run the `mru-event-stream`, if there is one.
    fn use_stream(&mut self, core: &mut Core<Self>) {
        let command = self.config.integration.mru_event_stream.as_deref();
        if command.is_none_or(|c| c.trim().is_empty()) && !self.warned_stream {
            warn!("no integration.mru-event-stream: only global notes are shown");
            self.warned_stream = true;
        }
        core.use_focus_stream(command);
    }

    /// Run the `output-event-stream`, if there is one and the column
    /// follows the focus.
    fn use_output_stream(&mut self, core: &Core<Self>) {
        let command = self
            .config
            .integration
            .output_event_stream
            .as_deref()
            .filter(|_| self.config.output.trim() == "focused");
        let handle = core.loop_handle.clone();
        self.output_stream.configure(command, &handle);
    }

    /// A line of the output stream: `<output> <id>`.
    fn output_line(&mut self, line: &str) {
        let Some((output, id)) = parse_output_line(line) else {
            return;
        };
        if self.window_outputs.get(id).is_some_and(|o| o == output) {
            return;
        }
        if self.window_outputs.len() >= MAX_PLACED && !self.window_outputs.contains_key(id) {
            self.window_outputs.clear();
        }
        self.window_outputs
            .insert(id.to_string(), output.to_string());
        if self.context.value().as_ref().is_some_and(|c| c.id == id) {
            self.outputs_dirty = true;
        }
    }

    /// After every event loop dispatch: follow config and notes changes,
    /// place the columns, and show the notes, once for however many
    /// events arrived.
    pub fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let touched = std::mem::take(&mut self.config_touched);
        if (touched || self.reloader.scheme_changed())
            && (self.reloader).refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.config_changed(core);
        }
        // The state first, so renames follow what is on disk.
        self.reload_state();
        self.rescan(core);
        self.serve(core);
        // A note kept open goes once it is closed, and the column may
        // follow the focus again.
        if self.open_keys() != self.last_open {
            self.dirty = true;
            self.outputs_dirty = true;
        }
        // A seat can disappear without delivering a source cancellation.
        if !core.dnd.dragging() {
            self.release_file_drag(core);
        }
        if std::mem::take(&mut self.outputs_dirty) {
            self.sync_columns(core);
        }
        if std::mem::take(&mut self.dirty) {
            self.show_notes();
        }
        // Columns gone from their outputs, once their notes slid away.
        let before = self.columns.len();
        self.columns.retain(|c| !c.retired());
        if self.columns.len() != before {
            self.trim = true;
        }
        if std::mem::take(&mut self.trim) {
            if let Some(papers) = &self.papers {
                let shown: Vec<_> = self
                    .columns
                    .iter()
                    .flat_map(Column::shown_buffers)
                    .collect();
                papers.borrow_mut().trim(&shown);
            }
            for column in &mut self.columns {
                column.trim();
            }
        }
    }

    fn config_changed(&mut self, core: &mut Core<Self>) {
        let old = std::mem::replace(&mut self.config, self.reloader.config().clone());
        if let Some(papers) = &self.papers {
            papers.borrow_mut().clear();
        }
        if old.notes_dir() != self.config.notes_dir() {
            self.remove_columns(core, |_| true);
            self.open_notes(core);
        }
        if old.integration.mru_event_stream != self.config.integration.mru_event_stream {
            self.warned_stream = false;
            self.use_stream(core);
            self.update_context(core, None);
        }
        self.use_output_stream(core);
        self.outputs_dirty = true;
        self.dirty = true;
        self.trim = true;
    }

    // -- the context ---------------------------------------------------------

    /// The window the notes should follow: the most recently focused one
    /// still open (leaving out `closed`, which is closing). None without
    /// a stream, or before any window was seen focused.
    fn candidate(core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) -> Option<Ctx> {
        if !core.focus_stream.configured() {
            return None;
        }
        let open: Vec<_> = (core.toplevels().into_iter())
            .filter(|(h, _)| Some(h) != closed)
            .map(|(_, t)| t)
            .collect();
        let id = core
            .mru
            .most_recent(open.iter().map(|t| t.identifier.as_str()))?;
        let t = open.iter().find(|t| t.identifier == id)?;
        Some(Ctx {
            window: Window::new(&t.app_id, &t.title),
            id: t.identifier.clone(),
        })
    }

    /// The context's window.
    fn window(&self) -> Option<&Window> {
        self.context.value().as_ref().map(|c| &c.window)
    }

    /// The focus, the windows or a title changed: follow, once settled.
    fn update_context(&mut self, core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) {
        let candidate = Self::candidate(core, closed);
        let settle = Duration::from_millis(self.config.integration.settle as u64);
        let p = self.context.propose(candidate, Instant::now(), settle);
        self.context_settled(core, p);
    }

    fn context_settled(&mut self, core: &Core<Self>, p: Proposal) {
        match p {
            Proposal::Same => self.cancel_settle(core),
            Proposal::Changed => {
                self.cancel_settle(core);
                self.dirty = true;
                self.outputs_dirty = true;
            }
            Proposal::Wait(at) => {
                if self.settle_timer.as_ref().is_some_and(|(t, _)| *t == at) {
                    return;
                }
                self.cancel_settle(core);
                let after = at.saturating_duration_since(Instant::now());
                let timer = core.timer(after, |app: &mut Self, core| {
                    app.settle_timer = None;
                    let p = app.context.due(Instant::now());
                    app.context_settled(core, p);
                });
                match timer {
                    Ok(token) => self.settle_timer = Some((at, token)),
                    Err(e) => warn!("{e}"),
                }
            }
        }
    }

    fn cancel_settle(&mut self, core: &Core<Self>) {
        if let Some((_, token)) = self.settle_timer.take() {
            core.loop_handle.remove(token);
        }
    }

    // -- the columns ---------------------------------------------------------

    /// The output the context's window is on, if the stream said.
    fn context_output(&self) -> Option<&str> {
        if !self.output_stream.running() {
            return None;
        }
        let id = &self.context.value().as_ref()?.id;
        self.window_outputs.get(id).map(String::as_str)
    }

    /// The outputs the columns go on (see the module's comment).
    fn targets(&self, core: &Core<Self>) -> Vec<Target> {
        let named = |o: &WlOutput| {
            let info = core.output_state.info(o)?;
            Some((o.clone(), info.name?, info.scale_factor.max(1)))
        };
        let outputs: Vec<Target> = (core.output_state.outputs())
            .filter_map(|o| named(&o))
            .collect();
        let on = |output: &WlOutput| outputs.iter().find(|t| t.0 == *output);
        let pick = match self.config.output.trim() {
            "all" => return outputs,
            "focused" => {
                let current = self.columns.iter().find(|c| !c.retiring);
                let held =
                    current.filter(|c| c.open_key().is_some() || c.dragged_file_note().is_some());
                (held.and_then(|c| on(&c.output)))
                    .or_else(|| {
                        let name = self.context_output()?;
                        outputs.iter().find(|t| t.1 == name)
                    })
                    .or_else(|| current.and_then(|c| on(&c.output)))
                    .or(outputs.first())
            }
            name => outputs.iter().find(|t| t.1 == name).or(outputs.first()),
        };
        pick.cloned().into_iter().collect()
    }

    /// Create, restyle, move or retire the columns to match the outputs,
    /// the config and the context.
    fn sync_columns(&mut self, core: &mut Core<Self>) {
        let (Some(papers), Some(assets)) = (self.papers.clone(), self.assets.clone()) else {
            return; // not started yet
        };
        let style = Style::new(&self.config);
        let targets = self.targets(core);
        // A new edge or layer takes new surfaces.
        self.remove_columns(core, |c| c.style().needs_new_surface(&style));
        let mut acts = Vec::new();
        for column in &mut self.columns {
            match targets
                .iter()
                .find(|t| t.0 == column.output && t.1 == column.name)
            {
                Some((_, _, scale)) => {
                    if column.retiring {
                        column.retiring = false;
                        self.dirty = true;
                    }
                    column.set_style(style.clone());
                    column.set_scale(*scale);
                }
                None if !column.retiring => {
                    // Its notes slide away; it goes once they are gone.
                    if column.draft.as_ref().is_some_and(Draft::dirty) {
                        continue;
                    }
                    if column.dragged_file_note().is_some() {
                        continue;
                    }
                    acts.push((column.surface().clone(), column.close()));
                    column.retiring = true;
                    column.set_notes(Vec::new());
                }
                None => {}
            }
        }
        for (output, name, scale) in targets {
            if self
                .columns
                .iter()
                .any(|c| c.output == output && c.name == name)
            {
                continue;
            }
            let column = Column::new(
                core,
                output,
                name,
                scale,
                style.clone(),
                papers.clone(),
                assets.clone(),
                self.fonts.clone(),
            );
            match column {
                Ok(column) => {
                    self.columns.push(column);
                    self.dirty = true;
                }
                Err(e) => warn!("failed to create a column: {e}"),
            }
        }
        for (surface, acts) in acts {
            self.acts(core, &surface, acts);
        }
    }

    /// The column whose surface is `surface`.
    fn column_at(&self, surface: &WlSurface) -> Option<usize> {
        self.columns.iter().position(|c| c.surface() == surface)
    }

    /// The column the CLI's notes open in: the one on the context's
    /// output, else the first shown.
    fn main_column(&self) -> Option<usize> {
        let shown = |c: &Column| !c.retiring;
        let output = self.context_output();
        (self.columns.iter())
            .position(|c| shown(c) && output.is_some_and(|o| c.name == o))
            .or_else(|| self.columns.iter().position(shown))
    }

    /// The columns' open notes.
    fn open_keys(&self) -> Vec<Option<String>> {
        (self.columns.iter())
            .filter(|c| !c.retiring)
            .map(|c| c.open_key().map(String::from))
            .collect()
    }

    /// What the column shows of the note `path`.
    fn shown(
        &self,
        path: &Path,
        window: Option<&Window>,
        fallback: Option<&Note>,
    ) -> Option<Shown> {
        let note = self.index.get(path).map(|e| &e.note).or(fallback)?;
        let key = path.to_string_lossy().into_owned();
        let color = note.meta.color.as_ref();
        let look = Look {
            color: self.config.paper_color(color),
            tilt: deck::tilt(&key, self.config.sheet.tilt),
        };
        let content = Content::of_note(note, &list::name(path));
        let files = (bar::files(&content.links).into_iter())
            .map(|path| FileLink {
                exists: path.exists(),
                path,
            })
            .collect();
        let pin = pin_state(note, window);
        let group = match note.global() {
            true => Group::Global,
            false => Group::Window,
        };
        // Dragged into the other group, it is pinned to the window, or
        // unpinned and global.
        let cross = matches!(
            (group, &pin),
            (Group::Global, Pin::Unpinned(_)) | (Group::Window, Pin::Pinned { global_after: true })
        );
        Some(Shown {
            item: Item {
                key,
                look,
                group,
                cross,
            },
            content: Rc::new(content),
            text: self.config.text_color(color),
            files,
            pin,
            color: color.copied().unwrap_or(self.config.sheet.default_color),
        })
    }

    /// Show the context's notes in every column, and each one's open
    /// note where it was.
    fn show_notes(&mut self) {
        let window = self.window().cloned();
        let window = window.as_ref();
        let paths: Vec<PathBuf> = (self.state.shown(&self.index, window).into_iter())
            .map(Path::to_path_buf)
            .collect();
        let mut lists: Vec<Option<Vec<Shown>>> = Vec::new();
        for column in &self.columns {
            if column.retiring {
                lists.push(None);
                continue;
            }
            let mut mine = paths.clone();
            for key in [column.open_key(), column.dragged_file_note()]
                .into_iter()
                .flatten()
            {
                let path = PathBuf::from(key);
                if !mine.contains(&path)
                    && ((self.index.get(&path).is_some() && !self.state.is_hidden(&path))
                        || column
                            .draft
                            .as_ref()
                            .is_some_and(|d| d.key == key && d.dirty()))
                {
                    let at = column.keys().position(|k| k == key).unwrap_or(mine.len());
                    mine.insert(at.min(mine.len()), path);
                }
            }
            let notes = mine
                .iter()
                .filter_map(|p| {
                    let fallback = column
                        .draft
                        .as_ref()
                        .filter(|d| Path::new(&d.key) == p)
                        .map(Draft::base);
                    self.shown(p, window, fallback)
                })
                .collect();
            lists.push(Some(notes));
        }
        for (column, notes) in self.columns.iter_mut().zip(lists) {
            if let Some(notes) = notes {
                column.set_notes(notes);
            }
        }
        self.last_open = self.open_keys();
    }

    /// One of a column's timers fired.
    pub fn column_timer(&mut self, _: &mut Core<Self>, surface: &WlSurface, which: Timer) {
        if let Some(i) = self.column_at(surface) {
            self.columns[i].timer_due(which);
        }
    }

    // -- what the columns ask for ------------------------------------------

    fn acts(&mut self, core: &mut Core<Self>, from: &WlSurface, acts: Vec<Act>) {
        for act in acts {
            self.act(core, from, act);
        }
    }

    fn act(&mut self, core: &mut Core<Self>, from: &WlSurface, act: Act) {
        let Some(i) = self.column_at(from) else {
            return;
        };
        match act {
            Act::Open(key) => self.open_in(core, i, &key),
            Act::Close => {
                if !self.save_draft(i, false) {
                    return;
                }
                let more = self.columns[i].close();
                self.acts(core, from, more);
            }
            Act::Task(key, n) => self.toggle_task(&key, n as usize),
            Act::Link(key, n) => self.open_link(core, &key, n as usize),
            Act::OpenLink(key, dest) => self.open_destination(core, &key, &dest),
            Act::QueueSave => self.queue_save(core),
            Act::Save => {
                self.save_draft(i, false);
            }
            Act::KeepMine => {
                self.save_draft(i, true);
            }
            Act::Reload => self.reload_draft(i),
            Act::Copy(text) => {
                if let Err(e) = core.set_selection(
                    CLIPBOARD_TEXT
                        .into_iter()
                        .map(|m| (m.into(), text.as_bytes().to_vec()))
                        .collect(),
                ) {
                    warn!("copying text: {e}");
                }
            }
            Act::Paste(serial) => {
                let mimes = core.selection_mime_types();
                if let Some(mime) = CLIPBOARD_TEXT
                    .into_iter()
                    .find_map(|m| mimes.iter().find(|s| s.eq_ignore_ascii_case(m)))
                {
                    let surface = from.clone();
                    if let Err(e) = core.read_selection(mime, move |app, core, data| {
                        if core.input.keyboard_focus() == Some(&surface)
                            && let Some(i) = app.column_at(&surface)
                            && let Ok(text) = std::str::from_utf8(&data)
                            && app.columns[i].paste(serial, text)
                        {
                            app.queue_save(core);
                        }
                    }) {
                        warn!("pasting text: {e}");
                    }
                }
            }
            Act::File(path) => self.open_path(core, &path.to_string_lossy()),
            Act::Edit(key) => {
                self.save_draft(i, false);
                self.edit(core, &key);
            }
            Act::New => {
                if let Err(e) = self.new_note(core, NewNote::default()) {
                    warn!("{e}");
                }
            }
            Act::Title(key, title) => self.edit_note(&key, |note| match title.trim() {
                "" => note.set("title", None),
                t => note.set_value("title", t),
            }),
            Act::Pin(key, exact) => {
                let Some(window) = self.window().cloned() else {
                    return;
                };
                self.edit_note(&key, |note| note.pin(&window, exact));
            }
            Act::Unpin(key) => {
                let Some(window) = self.window().cloned() else {
                    return;
                };
                self.edit_note(&key, |note| note.unpin(&window));
            }
            Act::Hide(key) => self.hide_note(core, &key),
            Act::Color(key, color) => {
                self.edit_note(&key, |note| note.set_value("color", bar::color_name(color)))
            }
            Act::Trash(key) => self.trash_note(core, &key),
            Act::RemoveFile(key, path) => {
                if !path.exists() {
                    self.edit_note(&key, |note| {
                        if let Some(text) = markdown::remove_file(note, &path) {
                            *note = Note::parse(text);
                        }
                        Ok(())
                    });
                }
            }
            Act::Reorder(key, shown, gap) => self.reorder(&key, &shown, gap),
        }
    }

    /// Open `key` in the column `i`, closing whatever is open in the
    /// others: one note is open at a time.
    fn open_in(&mut self, core: &mut Core<Self>, i: usize, key: &str) {
        for j in 0..self.columns.len() {
            if (j != i || self.columns[j].open_key() != Some(key)) && !self.save_draft(j, false) {
                return;
            }
        }
        let path = self.path(key);
        let draft = self.detached_drafts.remove(&path).or_else(|| {
            // Another column may just have saved this note; the index
            // will catch up on the next dispatch.
            read_note(&path)
                .ok()
                .flatten()
                .map(|source| Draft::new(key.to_owned(), Note::parse(source)))
        });
        for j in 0..self.columns.len() {
            if j != i && self.columns[j].open_key().is_some() {
                let more = self.columns[j].close();
                let surface = self.columns[j].surface().clone();
                self.acts(core, &surface, more);
            }
        }
        let Some(column) = self.columns.get_mut(i) else {
            return;
        };
        let more = column.open(core, key);
        if column.draft.as_ref().is_none_or(|d| d.key != key) {
            column.draft = draft;
            column.draft_changed();
        }
        let surface = column.surface().clone();
        self.acts(core, &surface, more);
    }

    /// Hide `key`: it crumples into the edge (closing first, if open),
    /// and goes in the hidden file.
    fn hide_note(&mut self, core: &mut Core<Self>, key: &str) {
        for i in 0..self.columns.len() {
            if self.columns[i].open_key() == Some(key) {
                if !self.save_draft(i, false) {
                    return;
                }
                let more = self.columns[i].close();
                let surface = self.columns[i].surface().clone();
                self.acts(core, &surface, more);
            }
            self.columns[i].crumple(key);
        }
        if self.state.hidden.push(Path::new(key)) {
            self.save_state(false, true);
        }
        self.dirty = true;
    }

    /// Run only the configured trash command. The filesystem scan, not
    /// the click or the command's launch, removes the note from the deck.
    fn trash_note(&mut self, core: &Core<Self>, key: &str) {
        if let Some(i) = self.columns.iter().position(|c| c.open_key() == Some(key))
            && !self.save_draft(i, false)
        {
            return;
        }
        if self.index.get(Path::new(key)).is_none() {
            return;
        }
        let path = self.path(key);
        if self.trash_pending.contains(&path) {
            return;
        }
        let template = &self.config.files.trash_command;
        if !spawn::uses(template, "paths") {
            return warn!("files.trash-command: expected {{paths}}");
        }
        // Pass the path as a standalone shell argument, including when
        // its filename is not UTF-8.
        let command = spawn::fill(template, &[("paths", "\"$1\"")]);
        let (send, results) = channel::channel();
        let tracked = path.clone();
        let registered = core
            .loop_handle
            .insert_source(results, move |event, _, shell| {
                if let channel::Event::Msg(result) = event {
                    if let Err(e) = result {
                        warn!("files.trash-command: {}: {e}", tracked.display());
                    }
                    let app = &mut shell.program;
                    app.notes_touched = true;
                    app.rescan(&mut shell.core);
                    app.trash_pending.remove(&tracked);
                }
            });
        let token = match registered {
            Ok(token) => token,
            Err(e) => return warn!("files.trash-command: {e}"),
        };
        let target = path.clone();
        let started = std::thread::Builder::new()
            .name("wl-note-trash".into())
            .spawn(move || {
                let result = std::process::Command::new("sh")
                    .args(["-c", &command, "wl-note-trash"])
                    .arg(&target)
                    .stdin(std::process::Stdio::null())
                    .status()
                    .and_then(|s| {
                        if s.success() {
                            Ok(())
                        } else {
                            Err(std::io::Error::other(s.to_string()))
                        }
                    });
                let _ = send.send(result);
            });
        match started {
            Ok(_) => {
                self.trash_pending.insert(path);
            }
            Err(e) => {
                core.loop_handle.remove(token);
                warn!("files.trash-command: {e}");
            }
        }
    }

    /// `key` was dropped at slot `gap` of a group showing `shown`: move it
    /// there in the order, keeping the notes not shown, and the other
    /// group's, in their places.
    fn reorder(&mut self, key: &str, shown: &[String], gap: usize) {
        let full = self.state.full_order(&self.index);
        let shown: Vec<&Path> = shown.iter().map(Path::new).collect();
        let new = reorder::land(&full, &shown, Path::new(key), gap);
        if new != full {
            self.state.order.set(new);
            self.save_state(true, false);
        }
        // Back to its slot, or to its new one.
        self.dirty = true;
    }

    /// The absolute path of the note `key`.
    fn path(&self, key: &str) -> PathBuf {
        self.index.dir().join(key)
    }

    fn observe_drafts(&mut self) {
        for c in &mut self.columns {
            if let Some(d) = &mut c.draft {
                let path = self.index.dir().join(&d.key);
                match read_note(&path) {
                    Ok(disk) => d.observe(disk.as_deref()),
                    Err(e) => d.error = Some(format!("Cannot read note: {e}")),
                }
                c.draft_changed();
            }
        }
    }

    fn queue_save(&mut self, core: &Core<Self>) {
        if let Some(token) = self.save_timer.take() {
            core.loop_handle.remove(token);
        }
        let delay = Duration::from_millis(self.config.editor.save_delay.into());
        if delay.is_zero() {
            self.save_open_drafts();
        } else {
            match core.timer(delay, |app: &mut Self, _| {
                app.save_timer = None;
                app.save_open_drafts();
            }) {
                Ok(token) => self.save_timer = Some(token),
                Err(e) => {
                    warn!("save timer: {e}");
                    self.save_open_drafts();
                }
            }
        }
    }

    fn save_open_drafts(&mut self) {
        for i in 0..self.columns.len() {
            self.save_draft(i, false);
        }
    }

    fn save_draft(&mut self, i: usize, force: bool) -> bool {
        let Some(d) = &mut self.columns[i].draft else {
            return true;
        };
        let path = self.index.dir().join(&d.key);
        let result = save_body(d, &path, force);
        self.columns[i].draft_changed();
        match result {
            Ok(SaveBody::Unchanged) => true,
            Ok(SaveBody::Written) => {
                self.notes_touched = true;
                true
            }
            Ok(SaveBody::Conflict) => false,
            Err(e) => {
                let message = format!("Cannot save: {e}");
                self.columns[i].draft.as_mut().unwrap().error = Some(message.clone());
                warn!("{}: {message}", path.display());
                false
            }
        }
    }

    fn reload_draft(&mut self, i: usize) {
        let Some(d) = &mut self.columns[i].draft else {
            return;
        };
        let path = self.index.dir().join(&d.key);
        match read_note(&path) {
            Ok(Some(disk)) => d.reload(Note::parse(disk)),
            Ok(None) => {
                self.columns[i].close();
            }
            Err(e) => {
                warn!("{}: cannot reload: {e}", path.display());
                return;
            }
        }
        self.notes_touched = true;
        self.dirty = true;
        self.columns[i].draft_changed();
    }

    fn remove_columns(&mut self, core: &mut Core<Self>, remove: impl Fn(&Column) -> bool) {
        for i in (0..self.columns.len()).rev() {
            if !remove(&self.columns[i]) {
                continue;
            }
            let saved = self.save_draft(i, false);
            if !saved && let Some(draft) = self.columns[i].draft.take() {
                let path = self.path(&draft.key);
                warn!(
                    "{}: unsaved draft retained; reopen the note to resolve it",
                    path.display()
                );
                self.detached_drafts.insert(path, draft);
            }
            let surface = self.columns[i].surface().clone();
            let acts = self.columns[i].close();
            self.acts(core, &surface, acts);
            self.columns.remove(i);
        }
    }

    /// Apply a sheet control to the latest file, after flushing its body draft.
    fn edit_note(&mut self, key: &str, f: impl FnOnce(&mut Note) -> Result<(), String>) {
        if let Some(i) = self.columns.iter().position(|c| c.open_key() == Some(key))
            && !self.save_draft(i, false)
        {
            return;
        }
        let path = self.path(key);
        let current = match std::fs::read_to_string(&path) {
            Ok(now) => now,
            Err(e) => return warn!("{}: {e}", path.display()),
        };
        let mut note = Note::parse(current.clone());
        if let Err(e) = f(&mut note) {
            return warn!("{}: {e}", path.display());
        }
        if note.text() == current {
            return;
        }
        match wl_shell_notes::write_atomic(&path, note.text()) {
            Ok(()) => {
                self.notes_touched = true;
                self.observe_drafts();
            }
            Err(e) => warn!("{}: {e}", path.display()),
        }
    }

    /// Check or uncheck the n-th checkbox of `key`, in its file.
    fn toggle_task(&mut self, key: &str, n: usize) {
        let rel = Path::new(key);
        let Some(entry) = self.index.get(rel) else {
            return;
        };
        let content = Content::of_note(&entry.note, &list::name(rel));
        let Some(range) = content.tasks.get(n).cloned() else {
            return;
        };
        let expected = entry.note.text().to_owned();
        self.edit_note(key, |note| {
            if note.text() != expected {
                return Err("changed on disk; checkbox not toggled".into());
            }
            let new = markdown::toggle(note.text(), range).ok_or("no checkbox to toggle")?;
            *note = Note::parse(new);
            Ok(())
        });
    }

    /// Open `target` (a path or URL) with `files.open-command`.
    fn open_path(&self, core: &Core<Self>, target: &str) {
        let command = spawn::fill(
            &self.config.files.open_command,
            &[("path", &spawn::quote(target))],
        );
        let output = Output::Prefixed("files.open-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("files.open-command: {e}");
        }
    }

    /// Open the n-th link of `key` with `files.open-command`.
    fn open_link(&self, core: &Core<Self>, key: &str, n: usize) {
        let Some(entry) = self.index.get(Path::new(key)) else {
            return;
        };
        let content = Content::of_note(&entry.note, "");
        let Some(dest) = content.links.get(n) else {
            return;
        };
        self.open_destination(core, key, dest);
    }

    fn open_destination(&self, core: &Core<Self>, key: &str, dest: &str) {
        if dest.starts_with('#') || dest.is_empty() {
            return; // a heading in the note itself
        }
        let target = match has_scheme(dest) {
            true => dest.to_owned(),
            // Relative to the note.
            false => {
                let dir = self.path(key);
                let dir = dir.parent().unwrap_or(self.index.dir());
                let dest = dest.split('#').next().unwrap_or(dest);
                dir.join(expand(dest)).to_string_lossy().into_owned()
            }
        };
        self.open_path(core, &target);
    }

    /// Open the note `key` with `editor.command`.
    fn edit(&self, core: &Core<Self>, key: &str) {
        let path = self.path(key).to_string_lossy().into_owned();
        let command = spawn::fill(
            &self.config.editor.command,
            &[("path", &spawn::quote(&path))],
        );
        let output = Output::Prefixed("editor.command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("editor.command: {e}");
        }
    }

    /// Make a note: attached to the context's app unless `global` (or
    /// there is no context), written at once, put at the end of the
    /// order, and opened in the built-in editor.
    fn new_note(&mut self, core: &mut Core<Self>, new: NewNote) -> Result<(), String> {
        let app = match new.global {
            true => None,
            false => self.window().map(|w| w.app_id.clone()),
        };
        let text = create::text(
            app.as_deref(),
            new.color.as_deref(),
            new.title.as_deref(),
            new.text.as_deref(),
        )?;
        let key = self.write_new_note(core, &text)?;
        self.focus_note(core, &key);
        Ok(())
    }

    fn write_new_note(&mut self, core: &mut Core<Self>, text: &str) -> Result<String, String> {
        let dir = self.index.dir().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = wl_shell_notes::create_new(&dir, &create::now_stem(), text)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let rel = path.strip_prefix(&dir).unwrap_or(&path).to_path_buf();
        let key = rel.to_string_lossy().into_owned();
        if self.state.order.push(&rel) {
            self.save_state(true, false);
        }
        // In the index (and the column) right away.
        self.notes_touched = true;
        self.rescan(core);
        Ok(key)
    }

    /// Open `key` with the keyboard, showing it even if it doesn't belong
    /// to the context.
    fn focus_note(&mut self, core: &mut Core<Self>, key: &str) {
        let Some(i) = self.main_column() else {
            return;
        };
        self.open_in(core, i, key);
        // Among the notes before its card is made, so it is kept.
        self.show_notes();
        self.dirty = false;
    }

    // -- file drops --------------------------------------------------------

    fn release_file_drag(&mut self, core: &Core<Self>) {
        for column in &mut self.columns {
            if column.dragged_file_note().is_some() {
                column.file_drag_done(core);
                self.dirty = true;
                self.outputs_dirty = true;
            }
        }
    }

    fn track_file_offer(&mut self, core: &Core<Self>, at: (f64, f64)) {
        let Some(offer) = &mut self.file_offer else {
            return;
        };
        offer.at = at;
        if let Some(c) = self
            .columns
            .iter_mut()
            .find(|c| c.surface() == &offer.surface && !c.retiring)
        {
            c.file_hover(core, at.0, at.1);
        }
    }

    fn leave_file_offer(&mut self, core: &Core<Self>) {
        let Some(offer) = self.file_offer.take() else {
            return;
        };
        if let Some(i) = self.column_at(&offer.surface)
            && self.columns[i].dragged_file_note().is_none()
        {
            self.columns[i].pointer_left(core);
        }
    }

    fn receive_files(&mut self, core: &mut Core<Self>, drop: FileDrop, data: Vec<u8>) {
        let Some(paths) = std::str::from_utf8(&data)
            .ok()
            .and_then(wl_shell_render::files::parse_uri_list)
        else {
            return warn!("drop did not contain a local file list");
        };
        let result = self.apply_drop(core, drop, paths);
        if let Err(e) = result {
            warn!("attaching files: {e}");
        }
    }

    fn drop_note(&self, key: &str) -> Result<Note, String> {
        let path = self.path(key);
        if let Some(d) = self
            .columns
            .iter()
            .filter_map(|c| c.draft.as_ref())
            .find(|d| d.key == key)
            .or_else(|| self.detached_drafts.get(&path))
        {
            return Ok(Note::parse(d.source()));
        }
        read_note(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .map(Note::parse)
            .ok_or_else(|| format!("{} is no longer there", path.display()))
    }

    fn apply_drop(
        &mut self,
        core: &mut Core<Self>,
        drop: FileDrop,
        mut paths: Vec<PathBuf>,
    ) -> Result<(), String> {
        if self.index.dir() != drop.dir {
            return Err("notes directory changed during drop".into());
        }
        match drop.target {
            FileTarget::New => {
                let body = files::append("", &paths, "\n");
                let text = create::text(drop.app.as_deref(), None, None, Some(&body))?;
                let key = self.write_new_note(core, &text)?;
                if let Some(i) = self
                    .column_at(&drop.surface)
                    .filter(|&i| !self.columns[i].retiring)
                    .or_else(|| self.main_column())
                {
                    self.open_in(core, i, &key);
                    self.show_notes();
                }
            }
            FileTarget::Note { key, editor } => {
                let path = self.path(&key);
                let note = self.drop_note(&key)?;
                files::keep_unlinked(&mut paths, note.body());
                if paths.is_empty() {
                    return Ok(());
                }
                if let Some(i) = self
                    .columns
                    .iter()
                    .position(|c| c.draft.as_ref().is_some_and(|d| d.key == key))
                {
                    let editor = (self.columns[i].surface() == &drop.surface)
                        .then_some(editor)
                        .flatten();
                    self.columns[i].attach_files(&key, editor, &paths);
                    self.save_draft(i, false);
                } else if self.detached_drafts.contains_key(&path) {
                    return Err(
                        "reopen the note to resolve its unsaved draft before attaching files"
                            .into(),
                    );
                } else {
                    let newline = if note.text().contains("\r\n") {
                        "\r\n"
                    } else {
                        "\n"
                    };
                    let added = files::append(note.body(), &paths, newline);
                    let mut text = note.text().to_owned();
                    if note.body().is_empty() && !text.is_empty() && !text.ends_with('\n') {
                        text.push_str(newline);
                    }
                    text.push_str(&added);
                    wl_shell_notes::write_atomic(&path, &text)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    self.notes_touched = true;
                }
            }
        }
        Ok(())
    }

    // -- the socket --------------------------------------------------------

    /// Answer what came over the socket.
    fn serve(&mut self, core: &mut Core<Self>) {
        let Some(listener) = &self.listener else {
            return;
        };
        for incoming in listener.take() {
            let Incoming::Request { client, req } = incoming else {
                continue;
            };
            let result = match req {
                NoteRequest::Nothing => Err("nothing asked".into()),
                NoteRequest::New(new) => self.new_note(core, new),
                NoteRequest::Focus(note) => self.focus_request(core, note.as_deref()),
            };
            client.answer(match result {
                Ok(()) => Reply::Done,
                Err(e) => Reply::Error(e),
            });
        }
    }

    /// `wl-note focus [note]` (and `show --focus`): a hidden note is shown
    /// again.
    fn focus_request(&mut self, core: &mut Core<Self>, note: Option<&str>) -> Result<(), String> {
        let Some(main) = self.main_column() else {
            return Err("no output to show notes on".into());
        };
        let key = match note {
            Some(q) => {
                // `show --focus` changed the state just now, maybe before
                // its inotify event arrives.
                self.state_touched = true;
                self.reload_state();
                self.notes_touched = true;
                self.rescan(core);
                let notes: Vec<(&Path, Option<&str>)> = self
                    .index
                    .iter()
                    .map(|(p, e)| (p, e.note.title()))
                    .collect();
                let found = find::find(self.index.dir(), &notes, q)?.to_path_buf();
                if self.state.hidden.remove(&found) {
                    self.save_state(false, true);
                }
                found.to_string_lossy().into_owned()
            }
            // Toggles.
            None if self.columns.iter().any(|c| c.open_key().is_some()) => {
                for i in 0..self.columns.len() {
                    if self.columns[i].open_key().is_some() {
                        if !self.save_draft(i, false) {
                            return Err(
                                "resolve the open note's unsaved changes before closing it".into(),
                            );
                        }
                        let acts = self.columns[i].close();
                        let surface = self.columns[i].surface().clone();
                        self.acts(core, &surface, acts);
                    }
                }
                return Ok(());
            }
            None => match self.columns[main].keys().next() {
                Some(k) => k.to_string(),
                None => return Err("no notes shown".into()),
            },
        };
        self.focus_note(core, &key);
        Ok(())
    }

    fn worker_done(&mut self, core: &Core<Self>, done: Done) {
        if let Some(assets) = &self.assets {
            assets.received(&done);
        }
        match done {
            Done::Paper { paper, rendered } => {
                if let Some(papers) = &self.papers
                    && (papers.borrow_mut()).insert(&core.g, &core.qh, paper, &rendered)
                {
                    for column in &mut self.columns {
                        column.paper_ready();
                    }
                }
            }
            Done::FileIcons { path, icons } => {
                for column in &mut self.columns {
                    column.file_icons_ready(path.clone(), icons.clone());
                }
            }
            Done::Named { key, image } => {
                for column in &mut self.columns {
                    column.icon_ready(key.clone(), image.clone());
                }
            }
            Done::Pill { request, pill, .. } => {
                for column in &mut self.columns {
                    column.line_ready(&request, &pill);
                }
            }
            _ => {}
        }
    }
}

/// Whether `rel` (in the notes directory) is where a note can be: a
/// `.md` file outside dot directories, not a dot file.
fn is_note(rel: &Path) -> bool {
    rel.extension().is_some_and(|e| e == "md")
        && !rel
            .components()
            .any(|c| c.as_os_str().as_encoded_bytes().starts_with(b"."))
}

/// A line of the output stream: `<output-name> <id>`. Everything after
/// the first space is the ID.
fn parse_output_line(line: &str) -> Option<(&str, &str)> {
    let (output, id) = line.trim_start().split_once(' ')?;
    let id = id.trim_end_matches(['\r', '\n']);
    (!output.is_empty() && !id.is_empty()).then_some((output, id))
}

/// What the pin icon of `note` does with `window` as the context.
fn pin_state(note: &Note, window: Option<&Window>) -> Pin {
    match window {
        _ if note.error.is_some() => Pin::Locked,
        None => Pin::None,
        Some(w) if w.app_id.is_empty() => Pin::None,
        Some(w) if note.pinned_to(w) => Pin::Pinned {
            global_after: note.unpin_leaves_global(w),
        },
        Some(w) => Pin::Unpinned(w.app_id.clone()),
    }
}

/// Whether a link's destination is a URL (`https:`, `mailto:`, `file:`)
/// rather than a path.
fn has_scheme(dest: &str) -> bool {
    dest.split_once(':').is_some_and(|(scheme, _)| {
        scheme.len() > 1
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && (scheme.chars()).all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    })
}

/// A path with `~/` expanded.
fn expand(path: &str) -> String {
    wl_shell_render::icons::expand_home(path)
}

impl Program for WlNote {
    const DATA_DEVICE: bool = true;

    fn dnd_enter(
        &mut self,
        core: &mut Core<Self>,
        surface: &WlSurface,
        at: (f64, f64),
        mimes: &[String],
    ) {
        self.leave_file_offer(core);
        self.file_drop_serial = self.file_drop_serial.wrapping_add(1);
        let accepted = !core.dnd.dragging()
            && mimes.iter().any(|m| m == files::URI_LIST)
            && self
                .column_at(surface)
                .is_some_and(|i| !self.columns[i].retiring);
        let (mime, action) = if accepted {
            self.file_offer = Some(FileOffer {
                surface: surface.clone(),
                at,
            });
            self.track_file_offer(core, at);
            (Some(files::URI_LIST), DndAction::Copy)
        } else {
            (None, DndAction::empty())
        };
        core.dnd.accept(mime, action, action);
    }

    fn dnd_motion(&mut self, core: &mut Core<Self>, at: (f64, f64)) {
        self.track_file_offer(core, at);
    }

    fn dnd_leave(&mut self, core: &mut Core<Self>) {
        self.leave_file_offer(core);
    }

    fn dnd_drop(&mut self, core: &mut Core<Self>) {
        let drop = self.file_offer.as_ref().and_then(|o| {
            let target = self.columns[self.column_at(&o.surface)?].file_target(o.at.0, o.at.1)?;
            Some(FileDrop {
                surface: o.surface.clone(),
                dir: self.index.dir().to_path_buf(),
                target,
                app: self.window().map(|w| w.app_id.clone()),
            })
        });
        self.leave_file_offer(core);
        if let Some(drop) = drop
            && core.dnd.action() == DndAction::Copy
        {
            let serial = self.file_drop_serial;
            match core.dnd_receive(files::URI_LIST, move |app, core, data| {
                if app.file_drop_serial == serial {
                    core.dnd.finish();
                }
                app.receive_files(core, drop, data);
            }) {
                Ok(()) => return,
                Err(e) => warn!("reading dropped files: {e}"),
            }
        }
        core.dnd.finish();
    }

    fn drag_source_done(&mut self, core: &mut Core<Self>, _: Option<DndAction>) {
        self.release_file_drag(core);
    }

    fn toplevel_new(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        // The focused window may only be announced now.
        self.update_context(core, None);
    }

    fn toplevel_update(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.update_context(core, None);
    }

    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(t) = core.toplevel(handle) {
            self.window_outputs.remove(&t.identifier);
        }
        self.update_context(core, Some(handle));
    }

    fn focus_changed(&mut self, core: &mut Core<Self>) {
        self.update_context(core, None);
    }

    fn output_new(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_update(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_removed(&mut self, core: &mut Core<Self>, output: &WlOutput) {
        self.remove_columns(core, |c| c.output == *output);
        self.outputs_dirty = true;
    }

    /// The compositor closed a column, e.g. because its output is going
    /// away. It comes back if the outputs or the config change.
    fn layer_closed(&mut self, core: &mut Core<Self>, layer: &LayerSurface) {
        self.remove_columns(core, |c| c.is(layer));
    }

    fn layer_configure(
        &mut self,
        _: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
        if let Some(column) = self.columns.iter_mut().find(|c| c.is(layer)) {
            let (width, height) = configure.new_size;
            column.configure(width, height);
        }
    }

    fn scale_factor_changed(&mut self, _: &mut Core<Self>, surface: &WlSurface, factor: i32) {
        if let Some(column) = self.columns.iter_mut().find(|c| c.surface() == surface) {
            column.set_scale(factor);
        }
    }

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
        let Some(i) = self.column_at(&event.surface) else {
            return;
        };
        let mods = core.input.modifiers;
        let acts = self.columns[i].pointer(core, event, &mods);
        self.acts(core, &event.surface, acts);
    }

    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, _: bool) {
        let Some(surface) = core.input.keyboard_focus().cloned() else {
            return;
        };
        let Some(i) = self.column_at(&surface) else {
            return;
        };
        let acts = self.columns[i].key(event, &core.input.modifiers);
        self.acts(core, &surface, acts);
    }

    fn keyboard_enter(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if let Some(column) = self.columns.iter_mut().find(|c| c.surface() == surface) {
            column.keyboard_enter();
        }
    }

    fn keyboard_leave(&mut self, core: &mut Core<Self>, surface: &WlSurface) {
        if let Some(i) = self.column_at(surface) {
            let acts = self.columns[i].keyboard_leave();
            self.acts(core, surface, acts);
        }
    }

    fn frame(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if let Some(column) = self.columns.iter_mut().find(|c| c.surface() == surface) {
            column.frame_done();
            self.trim = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_lines() {
        assert_eq!(parse_output_line("DP-1 42"), Some(("DP-1", "42")));
        assert_eq!(
            parse_output_line("  HDMI-A-1 a b\n"),
            Some(("HDMI-A-1", "a b"))
        );
        assert_eq!(parse_output_line("DP-1"), None);
        assert_eq!(parse_output_line(""), None);
    }

    #[test]
    fn notes_to_follow() {
        assert!(is_note(Path::new("a.md")));
        assert!(is_note(Path::new("work/a.md")));
        // An editor's backup, or temporary files.
        assert!(!is_note(Path::new("a.md~")));
        assert!(!is_note(Path::new(".a.md.swp")));
        assert!(!is_note(Path::new(".git/a.md")));
    }
}
