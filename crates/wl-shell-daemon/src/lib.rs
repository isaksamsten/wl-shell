//! `--daemon`: keep the Wayland connection, window list and config ready,
//! and show the program for clients on a Unix socket (see [`ipc`]).
//!
//! One show at a time. What a request while a show is on does is up to the
//! program (`Daemon::busy`). A client that disconnects cancels its show.

pub mod ipc;
pub mod listen;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use wl_shell_core::reexports::calloop::EventLoop;
use wl_shell_core::{Core, Error, Program, Shell};
use wl_shell_log::warn;

pub use ipc::{Reply, Request};
pub use listen::{Client, Incoming, Listener};

/// How a show ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The identifier of the picked window.
    Picked(String),
    Cancelled,
}

/// A program that shows something for each client request.
pub trait Daemon: Program {
    type Request: Request;

    /// The daemon is up: load the config, start what it names, warm caches.
    fn started(&mut self, core: &mut Core<Self>);

    /// Before a show: reload the config if it changed. Warnings go to the
    /// client.
    fn refresh(&mut self, core: &mut Core<Self>, warn: &mut dyn FnMut(String));

    /// Start a show for `req`. Warnings go to the client; an error ends the
    /// request.
    fn show(
        &mut self,
        core: &mut Core<Self>,
        req: Self::Request,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), String>;

    /// Whether a show is on (it may outlive its client while it closes).
    fn showing(&self) -> bool;

    /// A request came while a show is on: the reply for it.
    fn busy(&mut self, core: &mut Core<Self>) -> Reply;

    /// The client of the current show disconnected.
    fn client_gone(&mut self, core: &mut Core<Self>);

    /// The show's outcome, once there is one.
    fn take_outcome(&mut self) -> Option<Outcome>;

    /// End the show: whatever has keyboard focus or input must go. The
    /// daemon roundtrips before answering the client, so the compositor
    /// has applied it when the client acts on the outcome.
    fn end_show(&mut self, core: &mut Core<Self>);

    /// After the client was answered.
    fn after_show(&mut self, core: &mut Core<Self>) {
        let _ = core;
    }

    /// After every event loop dispatch.
    fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let _ = core;
    }

    /// After every dispatch while no client is waiting, e.g. to follow a
    /// theme switch right away rather than on the next show.
    fn idle(&mut self, core: &mut Core<Self>) {
        let _ = core;
    }
}

/// Hand `req` to the running daemon and relay its answer: the picked ID on
/// stdout and exit 0; exit 0 when done (nothing to wait for); exit 1 when
/// cancelled or on errors.
pub fn client(req: &impl Request) -> ExitCode {
    let name = wl_shell_log::name();
    client_to(
        req,
        &format!("no daemon is running (start one with `{name} --daemon`)"),
    )
}

/// `client`, saying `missing` if nothing listens on the socket.
pub fn client_to(req: &impl Request, missing: &str) -> ExitCode {
    let name = wl_shell_log::name();
    let fail = |m: &str| {
        warn!("{m}");
        ExitCode::FAILURE
    };
    let Some(path) = ipc::socket_path(name) else {
        return fail("XDG_RUNTIME_DIR is not set (or not absolute)");
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return fail(missing);
    };
    if let Err(e) = stream.write_all(req.encode().as_bytes()) {
        return fail(&format!("daemon: {e}"));
    }
    // Keep the connection open while waiting: closing it cancels.
    let mut lines = BufReader::new(stream).lines();
    loop {
        let reply = match lines.next() {
            Some(Ok(line)) => Reply::decode(&line),
            Some(Err(e)) => Err(e.to_string()),
            None => Err("connection closed".into()),
        };
        match reply {
            Ok(Reply::Warn(m)) => warn!("{m}"),
            Ok(Reply::Picked(id)) => {
                println!("{id}");
                return ExitCode::SUCCESS;
            }
            Ok(Reply::Cancelled) => return ExitCode::FAILURE,
            Ok(Reply::Done) => return ExitCode::SUCCESS,
            Ok(Reply::Error(m)) => return fail(&m),
            Err(e) => return fail(&format!("daemon: {e}")),
        }
    }
}

/// Run the daemon for `program` until the connection fails.
pub fn run<P: Daemon>(program: P) -> Result<(), Error> {
    let (mut shell, mut event_loop): (Shell<P>, EventLoop<Shell<P>>) = Shell::connect(program)?;
    shell.program.started(&mut shell.core);
    let listener: Listener<P::Request> = Listener::bind(&shell.core)?;
    warn!("daemon listening on {}", listener.path().display());

    // The client of the current show.
    let mut active: Option<Client> = None;
    loop {
        event_loop
            .dispatch(None, &mut shell)
            .map_err(|e| format!("wayland connection error: {e}"))?;
        let Shell { core, program } = &mut shell;
        program.after_dispatch(core);

        for incoming in listener.take() {
            match incoming {
                Incoming::Request { client, .. } if active.is_some() || program.showing() => {
                    client.answer(program.busy(core));
                }
                Incoming::Request { client, req } => {
                    let mut warn = |m: String| client.send(&Reply::Warn(m));
                    program.refresh(core, &mut warn);
                    match program.show(core, req, &mut warn) {
                        Ok(()) => active = Some(client),
                        Err(e) => {
                            program.end_show(core);
                            client.answer(Reply::Error(e));
                        }
                    }
                }
                Incoming::Gone(id) => {
                    if active.as_ref().is_some_and(|c| c.id() == id) {
                        program.client_gone(core);
                    }
                }
            }
        }

        if let Some(outcome) = program.take_outcome() {
            program.end_show(core);
            // Make sure the show is gone, or closing without keyboard focus
            // and input, before the client acts on the result.
            core.conn.roundtrip()?;
            if let Some(client) = active.take() {
                client.answer(match outcome {
                    Outcome::Picked(id) => Reply::Picked(id),
                    Outcome::Cancelled => Reply::Cancelled,
                });
            }
            program.after_show(core);
        }

        if active.is_none() {
            program.idle(core);
        }
    }
}
