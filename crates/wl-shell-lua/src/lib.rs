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
//!   the plugin's own folder. `print` goes to the host's log.
//! - Host functions, in the namespace the host names (`dock`):
//!   `redraw()`, `invalidate()`, `timer(ms, fn)`, `every(ms, fn)`,
//!   `spawn(argv, {on_line, on_exit})`, `run(argv)`, `json.decode`,
//!   `json.encode`, `read(path)` and `clipboard(text)` (see `host.rs`).

mod caps;
pub mod channel;
mod host;
mod runtime;
mod spec;
mod state;

pub use caps::{
    API, ActionDecl, Capabilities, Registry, SPANS, SourceDecl, Span, WidgetCaps, span_step,
};
pub use channel::{
    Answer, InstanceId, Pending, Reply, Request, RequestId, RequestKind, Ticket, Topic,
};
pub use runtime::{Event, Runtime, Settings};
pub use spec::{InstanceSpec, Role, Source};
