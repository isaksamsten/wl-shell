//! The program: the config, the notes and the context, and the column
//! showing them.
//!
//! - **Notes.** The notes directory is read into an `Index` and watched
//!   with inotify, every directory in it. Any event rescans it (only
//!   changed files are read again), after the loop's dispatch, so a burst
//!   of events is one rescan. New directories get watched as they appear.
//! - **Context.** The window the notes follow: the most recently focused
//!   open window, from the `mru-event-stream`. Focus on anything else
//!   (wl-note's own surfaces, the dock, a launcher: layer surfaces, which
//!   the stream doesn't report) changes nothing, so clicking a note never
//!   takes it away. Title changes count, and a new context only shows
//!   once it held for `settle` (context.rs). When the context's window
//!   closes, the most recent one left takes over. Without a stream, only
//!   global notes show.
//! - **The column** (column.rs) is on one output for now: the one `output`
//!   names, or the first. It peeks, focuses and tucks back by itself, and
//!   asks for the rest (`Act`): toggling a checkbox (written to the file,
//!   which inotify brings back), opening a link (`open-command`) or the
//!   note (`editor-command`), and making a note.
//! - **The socket.** `wl-note new` and `wl-note focus` come over it
//!   (ipc.rs). A new note is attached to the context's app (or global),
//!   written at once, and opened in the editor if it has no text yet, else
//!   focused. A note focused by name shows even if it doesn't belong to
//!   the context, until it is tucked back.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use smithay_client_toolkit::seat::keyboard::KeyEvent;
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::shell::wlr_layer::{LayerSurface, LayerSurfaceConfigure};
use wayland_client::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::Reloader;
use wl_shell_core::reexports::calloop::{RegistrationToken, channel};
use wl_shell_core::spawn::{self, Output};
use wl_shell_core::{Core, Program, Shell};
use wl_shell_daemon::{Incoming, Listener, Reply};
use wl_shell_log::warn;
use wl_shell_notes::{Index, Window};
use wl_shell_render::worker::{Assets, Done};

use crate::column::{Act, Column, Shown, Style, Timer};
use crate::config::Config;
use crate::context::{Proposal, Settle};
use crate::deck::{self, Item, Look};
use crate::ipc::{NewNote, NoteRequest};
use crate::markdown::{self, Content};
use crate::papers::Papers;
use crate::{create, find, list};

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
    /// The directories watched.
    watches: Vec<(PathBuf, RegistrationToken)>,
    /// The worker, started with the program.
    assets: Option<Rc<Assets>>,
    papers: Option<Rc<RefCell<Papers>>>,
    column: Option<Column>,
    /// The outputs (or the config) changed since the column was placed.
    outputs_dirty: bool,
    /// The window the notes follow; None: only global notes.
    context: Settle<Option<Window>>,
    /// When a pending context is due, and the timer that checks.
    settle_timer: Option<(Instant, RegistrationToken)>,
    /// Warned that there is no `mru-event-stream`.
    warned_stream: bool,
    /// The notes shown may have changed.
    dirty: bool,
    /// Sheets may have been replaced: destroy the ones no longer shown.
    trim: bool,
    /// The socket `new` and `focus` come over; None if it couldn't be
    /// bound.
    listener: Option<Listener<NoteRequest>>,
    /// A note focused by name: shown while focused, even if it doesn't
    /// belong to the context.
    forced: Option<String>,
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
            watches: Vec::new(),
            assets: None,
            papers: None,
            column: None,
            outputs_dirty: true,
            context: Settle::new(None),
            settle_timer: None,
            warned_stream: false,
            dirty: true,
            trim: false,
            listener: None,
            forced: None,
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker and the stream, read and watch the notes, and show them.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.start_worker(core)?;
        self.watch_config(core);
        self.open_notes(core);
        self.use_stream(core);
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

    /// Read the notes directory (created if missing) and watch it.
    fn open_notes(&mut self, core: &Core<Self>) {
        for (_, token) in self.watches.drain(..) {
            core.loop_handle.remove(token);
        }
        let dir = self.config.notes_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!("{}: {e}", dir.display());
        }
        self.index = Index::new(dir);
        self.notes_touched = true;
        self.rescan(core);
    }

    /// Read the notes directory again, and watch the directories in it.
    fn rescan(&mut self, core: &Core<Self>) {
        if !std::mem::take(&mut self.notes_touched) {
            return;
        }
        let changes = self.index.scan();
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
        let dirs = self.index.dirs().to_vec();
        self.watches.retain(|(dir, token)| {
            let keep = dirs.contains(dir);
            if !keep {
                core.loop_handle.remove(*token);
            }
            keep
        });
        for dir in dirs {
            if self.watches.iter().any(|(d, _)| *d == dir) {
                continue;
            }
            let watched =
                wl_shell_config::watch_dir(&core.loop_handle, &dir, |shell: &mut Shell<Self>| {
                    shell.program.notes_touched = true;
                });
            match watched {
                Ok(token) => self.watches.push((dir, token)),
                Err(e) => warn!("not watching {}: {e}", dir.display()),
            }
        }
    }

    /// Run the `mru-event-stream`, if there is one.
    fn use_stream(&mut self, core: &mut Core<Self>) {
        let command = self.config.mru_event_stream.as_deref();
        if command.is_none_or(|c| c.trim().is_empty()) && !self.warned_stream {
            warn!("no mru-event-stream: only global notes are shown");
            self.warned_stream = true;
        }
        core.use_focus_stream(command);
    }

    /// After every event loop dispatch: follow config and notes changes,
    /// place the column, and show the notes, once for however many events
    /// arrived.
    pub fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let touched = std::mem::take(&mut self.config_touched);
        if (touched || self.reloader.scheme_changed())
            && (self.reloader).refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.config_changed(core);
        }
        self.rescan(core);
        if std::mem::take(&mut self.outputs_dirty) {
            self.sync_column(core);
        }
        self.serve(core);
        // A note focused by name goes once it is tucked back.
        if let Some(forced) = &self.forced
            && (self.column.as_ref()).is_none_or(|c| c.focused() != Some(forced))
        {
            self.forced = None;
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.show_notes(core);
        }
        if std::mem::take(&mut self.trim) {
            if let Some(papers) = &self.papers {
                let shown = (self.column.as_ref()).map_or(Vec::new(), Column::shown_buffers);
                papers.borrow_mut().trim(&shown);
            }
            if let Some(column) = &mut self.column {
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
            self.open_notes(core);
        }
        if old.mru_event_stream != self.config.mru_event_stream {
            self.warned_stream = false;
            self.use_stream(core);
            self.update_context(core, None);
        }
        self.outputs_dirty = true;
        self.dirty = true;
        self.trim = true;
    }

    // -- the context ---------------------------------------------------------

    /// The window the notes should follow: the most recently focused one
    /// still open (leaving out `closed`, which is closing). None without
    /// a stream, or before any window was seen focused.
    fn candidate(core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) -> Option<Window> {
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
        Some(Window::new(&t.app_id, &t.title))
    }

    /// The focus, the windows or a title changed: follow, once settled.
    fn update_context(&mut self, core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) {
        let candidate = Self::candidate(core, closed);
        let settle = Duration::from_millis(self.config.settle as u64);
        let p = self.context.propose(candidate, Instant::now(), settle);
        self.context_settled(core, p);
    }

    fn context_settled(&mut self, core: &Core<Self>, p: Proposal) {
        match p {
            Proposal::Same => self.cancel_settle(core),
            Proposal::Changed => {
                self.cancel_settle(core);
                self.dirty = true;
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

    // -- the column ----------------------------------------------------------

    /// The output the column goes on: the one `output` names, else the
    /// first. Following the focused window comes with the output stream.
    fn pick_output(&self, core: &Core<Self>) -> Option<(WlOutput, String, i32)> {
        let wanted = self.config.output.trim();
        let named = |o: &WlOutput| {
            let info = core.output_state.info(o)?;
            Some((o.clone(), info.name?, info.scale_factor.max(1)))
        };
        let outputs: Vec<_> = core
            .output_state
            .outputs()
            .filter_map(|o| named(&o))
            .collect();
        if !matches!(wanted, "focused" | "all")
            && let Some(o) = outputs.iter().find(|(_, name, _)| name == wanted)
        {
            return Some(o.clone());
        }
        outputs.into_iter().next()
    }

    /// Create, restyle, move or drop the column to match the outputs and
    /// the config.
    fn sync_column(&mut self, core: &Core<Self>) {
        let (Some(papers), Some(assets)) = (self.papers.clone(), self.assets.clone()) else {
            return; // not started yet
        };
        let style = Style::new(&self.config);
        let Some((output, name, scale)) = self.pick_output(core) else {
            self.column = None;
            return;
        };
        if self.column.as_ref().is_some_and(|c| {
            c.output != output || c.name != name || c.style().needs_new_surface(&style)
        }) {
            self.column = None;
        }
        match &mut self.column {
            Some(column) => {
                column.set_style(style);
                column.set_scale(scale);
            }
            None => match Column::new(core, output, name, scale, style, papers, assets) {
                Ok(column) => {
                    self.column = Some(column);
                    self.dirty = true;
                }
                Err(e) => warn!("failed to create the column: {e}"),
            },
        }
    }

    /// Show the context's notes (and a note focused by name).
    fn show_notes(&mut self, core: &Core<Self>) {
        let Some(column) = &mut self.column else {
            return;
        };
        let window = self.context.value().as_ref();
        let mut paths = self.index.shown(window);
        if let Some(forced) = &self.forced
            && !paths.iter().any(|p| *p == Path::new(forced))
        {
            paths.push(Path::new(forced));
        }
        let notes = (paths.into_iter())
            .filter_map(|path| {
                let entry = self.index.get(path)?;
                let key = path.to_string_lossy().into_owned();
                let color = entry.note.meta.color.as_ref();
                let look = Look {
                    color: self.config.paper_color(color),
                    tilt: deck::tilt(&key, self.config.tilt),
                };
                let content = Content::of_note(&entry.note, &list::name(path));
                Some(Shown {
                    item: Item { key, look },
                    content: Rc::new(content),
                    text: self.config.text_color(color),
                })
            })
            .collect();
        column.set_notes(core, notes);
    }

    /// One of the column's timers fired.
    pub fn column_timer(&mut self, core: &mut Core<Self>, which: Timer) {
        if let Some(column) = &mut self.column {
            column.timer_due(core, which);
        }
    }

    // -- what the column asks for ------------------------------------------

    fn act(&mut self, core: &mut Core<Self>, act: Act) {
        let Some(column) = &mut self.column else {
            return;
        };
        match act {
            Act::None => {}
            Act::Focus(key) => column.focus(core, &key, false),
            Act::Tuck => column.tuck(core),
            Act::Task(key, n) => {
                // A click while peeking took the keyboard for nothing.
                column.release_keyboard();
                self.toggle_task(&key, n as usize);
            }
            Act::Link(key, n) => self.open_link(core, &key, n as usize),
            Act::Edit(key) => {
                column.tuck(core);
                self.edit(core, &key);
            }
            Act::New => {
                column.tuck(core);
                if let Err(e) = self.new_note(core, NewNote::default()) {
                    warn!("{e}");
                }
            }
        }
    }

    /// The absolute path of the note `key`.
    fn path(&self, key: &str) -> PathBuf {
        self.index.dir().join(key)
    }

    /// Check or uncheck the n-th checkbox of `key`, in its file.
    fn toggle_task(&mut self, key: &str, n: usize) {
        let rel = Path::new(key);
        let Some(entry) = self.index.get(rel) else {
            return;
        };
        let content = Content::of_note(&entry.note, &list::name(rel));
        let path = self.path(key);
        // Only over what was read: a change not seen yet wins.
        match std::fs::read_to_string(&path) {
            Ok(now) if now == entry.note.text() => {}
            Ok(_) => {
                self.notes_touched = true;
                return warn!("{}: changed on disk; not toggled", path.display());
            }
            Err(e) => return warn!("{}: {e}", path.display()),
        }
        let new = (content.tasks.get(n))
            .and_then(|range| markdown::toggle(entry.note.text(), range.clone()));
        let Some(new) = new else {
            return warn!("{}: no checkbox to toggle", path.display());
        };
        match wl_shell_notes::write_atomic(&path, &new) {
            Ok(()) => self.notes_touched = true,
            Err(e) => warn!("{}: {e}", path.display()),
        }
    }

    /// Open the n-th link of `key` with `open-command`.
    fn open_link(&self, core: &Core<Self>, key: &str, n: usize) {
        let Some(entry) = self.index.get(Path::new(key)) else {
            return;
        };
        let content = Content::of_note(&entry.note, "");
        let Some(dest) = content.links.get(n) else {
            return;
        };
        if dest.starts_with('#') || dest.is_empty() {
            return; // a heading in the note itself
        }
        let target = match has_scheme(dest) {
            true => dest.clone(),
            // Relative to the note.
            false => {
                let dir = self.path(key);
                let dir = dir.parent().unwrap_or(self.index.dir());
                let dest = dest.split('#').next().unwrap_or(dest);
                dir.join(expand(dest)).to_string_lossy().into_owned()
            }
        };
        let command = spawn::fill(&self.config.open_command, &[("path", &spawn::quote(&target))]);
        let output = Output::Prefixed("open-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("open-command: {e}");
        }
    }

    /// Open the note `key` with `editor-command`.
    fn edit(&self, core: &Core<Self>, key: &str) {
        let path = self.path(key).to_string_lossy().into_owned();
        let command = spawn::fill(&self.config.editor_command, &[("path", &spawn::quote(&path))]);
        let output = Output::Prefixed("editor-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("editor-command: {e}");
        }
    }

    /// Make a note: attached to the context's app unless `global` (or
    /// there is no context), written at once, and focused; without text,
    /// opened in the editor instead.
    fn new_note(&mut self, core: &mut Core<Self>, new: NewNote) -> Result<(), String> {
        let app = match new.global {
            true => None,
            false => self.context.value().as_ref().map(|w| w.app_id.clone()),
        };
        let text = create::text(
            app.as_deref(),
            new.color.as_deref(),
            new.title.as_deref(),
            new.text.as_deref(),
        )?;
        let dir = self.index.dir().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = wl_shell_notes::create_new(&dir, &create::now_stem(), &text)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        let key = (path.strip_prefix(&dir).unwrap_or(&path))
            .to_string_lossy()
            .into_owned();
        // In the index (and the column) right away.
        self.notes_touched = true;
        self.rescan(core);
        let empty = new.text.as_deref().is_none_or(|t| t.trim().is_empty());
        match empty {
            true => {
                self.dirty = true;
                self.edit(core, &key);
            }
            false => self.focus_note(core, &key),
        }
        Ok(())
    }

    /// Focus `key` with the keyboard, showing it even if it doesn't belong
    /// to the context.
    fn focus_note(&mut self, core: &mut Core<Self>, key: &str) {
        let shown = (self.column.as_ref()).is_some_and(|c| c.keys().any(|k| k == key));
        let matches = (self.index.get(Path::new(key)))
            .is_some_and(|e| e.note.matches(self.context.value().as_ref()));
        if !shown && !matches {
            self.forced = Some(key.to_string());
        }
        self.show_notes(core);
        self.dirty = false;
        if let Some(column) = &mut self.column {
            column.focus(core, key, true);
        }
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

    /// `wl-note focus [note]`.
    fn focus_request(&mut self, core: &mut Core<Self>, note: Option<&str>) -> Result<(), String> {
        let Some(column) = &mut self.column else {
            return Err("no output to show notes on".into());
        };
        let key = match note {
            Some(q) => {
                let notes: Vec<(&Path, Option<&str>)> =
                    self.index.iter().map(|(p, e)| (p, e.note.title())).collect();
                let found = find::find(self.index.dir(), &notes, q)?;
                found.to_string_lossy().into_owned()
            }
            // Toggles.
            None if column.focused().is_some() => {
                column.tuck(core);
                return Ok(());
            }
            None => match column.keys().next() {
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
                    && let Some(column) = &mut self.column
                {
                    column.paper_ready();
                }
            }
            Done::Doc { tag, doc, rendered } => {
                if let Some(column) = &mut self.column {
                    column.doc_ready(tag, &doc, &rendered);
                }
            }
            _ => {}
        }
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
    fn toplevel_new(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        // The focused window may only be announced now.
        self.update_context(core, None);
    }

    fn toplevel_update(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.update_context(core, None);
    }

    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
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

    fn output_removed(&mut self, _: &mut Core<Self>, output: &WlOutput) {
        if self.column.as_ref().is_some_and(|c| c.output == *output) {
            self.column = None;
        }
        self.outputs_dirty = true;
    }

    /// The compositor closed the column, e.g. because its output is going
    /// away. It comes back if the outputs or the config change.
    fn layer_closed(&mut self, _: &mut Core<Self>, layer: &LayerSurface) {
        if self.column.as_ref().is_some_and(|c| c.is(layer)) {
            self.column = None;
        }
    }

    fn layer_configure(
        &mut self,
        _: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
        if let Some(column) = self.column.as_mut().filter(|c| c.is(layer)) {
            let (width, height) = configure.new_size;
            column.configure(width, height);
        }
    }

    fn scale_factor_changed(&mut self, _: &mut Core<Self>, surface: &WlSurface, factor: i32) {
        if let Some(column) = self.column.as_mut().filter(|c| c.surface() == surface) {
            column.set_scale(factor);
        }
    }

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
        let Some(column) = (self.column.as_mut()).filter(|c| *c.surface() == event.surface) else {
            return;
        };
        let act = column.pointer(core, event);
        self.act(core, act);
    }

    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, _: bool) {
        let Some(column) = self.column.as_mut() else {
            return;
        };
        if core.input.keyboard_focus() != Some(column.surface()) {
            return;
        }
        let act = column.key(event, &core.input.modifiers);
        self.act(core, act);
    }

    fn keyboard_leave(&mut self, core: &mut Core<Self>, surface: &WlSurface) {
        if let Some(column) = self.column.as_mut().filter(|c| c.surface() == surface) {
            column.keyboard_leave(core);
        }
    }

    fn frame(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if let Some(column) = self.column.as_mut().filter(|c| c.surface() == surface) {
            column.frame_done();
            self.trim = true;
        }
    }
}
