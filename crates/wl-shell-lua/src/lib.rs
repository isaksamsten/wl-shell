//! Lua plugins for wl-shell programs: one runtime for the dock's widgets
//! now and the launcher's actions and sources later. It knows nothing
//! about docks or launchers: a plugin declares what it provides
//! ([`caps`]), and the host asks for it over one request channel
//! ([`channel`]).
//!
//! - [`Runtime`]: Lua 5.4 states (one per [`InstanceSpec`]) on a small
//!   thread pool, with a timer thread. Each state runs on one thread at a
//!   time, its jobs in order: loading, the host's requests, timers, and
//!   lines from its processes. The host reads [`Event`]s from a sink it
//!   gives; it never waits on Lua.
//! - Lifecycle: a state loads its file and runs `setup(opts)`. Every hook
//!   call has an instruction budget; a state that overruns it (or runs
//!   out of memory, or fails in `setup`) is dropped and restarted with
//!   backoff. A file that doesn't compile waits for [`Runtime::reload`],
//!   which the host calls when the file changes.
//! - Sandbox: the base library without `dofile` and `loadfile`, `load`
//!   for text chunks only, `string`, `table`, `math`, `utf8`,
//!   `coroutine`, and `os` with `clock`, `date`, `difftime`, `getenv` and
//!   `time`. No `io`, `debug` or `package`; `require` loads modules from
//!   the plugin's own folder, then the host's bundled ones
//!   ([`Settings::modules`]). `print` goes to the host's log.
//! - Host functions, in the namespace the host names (`dock`):
//!   `redraw()`, `invalidate()`, `timer(ms, fn)`, `every(ms, fn)`,
//!   `spawn(argv, {on_line, on_exit})`, `run(argv)`, `json.decode`,
//!   `json.encode`, `read(path)`, `clipboard(text)`, `span(n)` and
//!   `now()` (see `host.rs`), `watch(path, fn, opts)` (see `watch.rs`),
//!   `app` (the app a widget backs, by its names, nil for a standalone
//!   one; see [`AppInfo`]), the `dbus` client (see `dbus.rs`), the `ui`
//!   constructors and the `theme`'s color names (see [`ui`]).
//! - Widgets answer `label()` and `icon(tile)`: an icon is a [`ui::Node`]
//!   tree, built from what the hook returned on the plugin's thread. A
//!   standalone widget's item also hears its clicks (`on_click(button)`),
//!   and an app's widget hears `on_app()` when `dock.app` changes.
//! - Launcher actions: the host looks up the ones whose `when` holds for
//!   a [`Subject`] in the [`Registry`] (in Rust), asks for an action's
//!   `choices(s)` and runs its `run(s, choice)` (see `subject.rs`).

mod caps;
pub mod channel;
mod dbus;
mod host;
mod runtime;
mod spec;
mod state;
mod subject;
pub mod ui;
mod watch;

pub use caps::{
    API, ActionDecl, Capabilities, PopupOn, Registry, SPANS, SourceDecl, Span, WidgetCaps,
    span_step,
};
pub use channel::{
    Answer, Axis, InstanceId, Pending, Reply, Request, RequestId, RequestKind, Ticket, Tile, Topic,
};
pub use runtime::{Event, Runtime, Settings};
pub use spec::{AppInfo, InstanceSpec, Role, Source};
pub use subject::{Choice, MAX_CHOICES, Subject, SubjectKind, SubjectWindow, When, glob};
