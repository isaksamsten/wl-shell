//! `--daemon`: keep the Wayland connection, window list and config ready,
//! and show the program for clients on a Unix socket (see [`ipc`]).
//!
//! One show at a time. What a request while a show is on does is up to the
//! program (`Daemon::busy`). A client that disconnects cancels its show.

pub mod ipc;

use std::cell::RefCell;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;

use wl_shell_core::reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic};
use wl_shell_core::{Core, Error, Program, Shell};
use wl_shell_log::warn;

pub use ipc::{Reply, Request};

/// Requests are tiny; anything bigger is not a client.
const MAX_REQUEST: usize = 1 << 20;

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

/// Client events, queued by the socket sources for the daemon loop.
enum Incoming<R> {
    /// A complete request; `stream` is for the replies.
    Request {
        client: u64,
        stream: UnixStream,
        req: R,
    },
    /// The client closed its connection.
    Gone(u64),
}

type Inbox<R> = Rc<RefCell<Vec<Incoming<R>>>>;

/// Hand `req` to the running daemon and relay its answer: the picked ID on
/// stdout and exit 0; exit 1 when cancelled or on errors.
pub fn client(req: &impl Request) -> ExitCode {
    let name = wl_shell_log::name();
    let fail = |m: &str| {
        warn!("{m}");
        ExitCode::FAILURE
    };
    let Some(path) = ipc::socket_path(name) else {
        return fail("XDG_RUNTIME_DIR is not set (or not absolute)");
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return fail(&format!(
            "no daemon is running (start one with `{name} --daemon`)"
        ));
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
            Ok(Reply::Error(m)) => return fail(&m),
            Err(e) => return fail(&format!("daemon: {e}")),
        }
    }
}

/// Run the daemon for `program` until the connection fails.
pub fn run<P: Daemon>(program: P) -> Result<(), Error> {
    let name = wl_shell_log::name();
    let path = ipc::socket_path(name).ok_or("XDG_RUNTIME_DIR is not set (or not absolute)")?;
    let (mut shell, mut event_loop): (Shell<P>, EventLoop<Shell<P>>) = Shell::connect(program)?;
    shell.program.started(&mut shell.core);

    let inbox: Inbox<P::Request> = Rc::default();
    let listener = bind(&path)?;
    let _socket = SocketFile(path.clone());
    listener.set_nonblocking(true)?;
    let mut next_client = 0;
    let accept_inbox = inbox.clone();
    event_loop.handle().insert_source(
        Generic::new(listener, Interest::READ, Mode::Level),
        move |_, listener, shell| {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        next_client += 1;
                        let inbox = accept_inbox.clone();
                        if let Err(e) = watch_client(&shell.core, stream, next_client, inbox) {
                            warn!("client: {e}");
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(e) => {
                        warn!("accept: {e}");
                        break;
                    }
                }
            }
            Ok(PostAction::Continue)
        },
    )?;
    warn!("daemon listening on {}", path.display());

    // The client of the current show.
    let mut active: Option<(u64, UnixStream)> = None;
    loop {
        event_loop
            .dispatch(None, &mut shell)
            .map_err(|e| format!("wayland connection error: {e}"))?;
        let Shell { core, program } = &mut shell;
        program.after_dispatch(core);

        let incoming = std::mem::take(&mut *inbox.borrow_mut());
        for incoming in incoming {
            match incoming {
                Incoming::Request { stream, .. } if active.is_some() || program.showing() => {
                    answer(stream, program.busy(core));
                }
                Incoming::Request {
                    client,
                    stream,
                    req,
                } => {
                    let mut warn = |m: String| send(&stream, &Reply::Warn(m));
                    program.refresh(core, &mut warn);
                    match program.show(core, req, &mut warn) {
                        Ok(()) => active = Some((client, stream)),
                        Err(e) => {
                            program.end_show(core);
                            answer(stream, Reply::Error(e));
                        }
                    }
                }
                Incoming::Gone(client) => {
                    if active.as_ref().is_some_and(|(c, _)| *c == client) {
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
            if let Some((_, stream)) = active.take() {
                answer(
                    stream,
                    match outcome {
                        Outcome::Picked(id) => Reply::Picked(id),
                        Outcome::Cancelled => Reply::Cancelled,
                    },
                );
            }
            program.after_show(core);
        }

        if active.is_none() {
            program.idle(core);
        }
    }
}

/// Read the client's request, then watch for it hanging up.
fn watch_client<P: Program, R: Request + 'static>(
    core: &Core<P>,
    stream: UnixStream,
    client: u64,
    inbox: Inbox<R>,
) -> std::io::Result<()> {
    stream.set_nonblocking(true)?;
    // Some(..) until the request is complete.
    let mut reply_stream = Some(stream.try_clone()?);
    let mut buf = Vec::new();
    let source = Generic::new(stream, Interest::READ, Mode::Level);
    let result = core.loop_handle.insert_source(source, move |_, stream, _| {
        let mut chunk = [0; 4096];
        loop {
            match (&**stream).read(&mut chunk) {
                Ok(0) => break,
                Ok(n) if reply_stream.is_some() => buf.extend_from_slice(&chunk[..n]),
                Ok(_) => {} // nothing is expected after the request
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if let Some(reply) = reply_stream.take() {
                        reply_stream = take_request(&inbox, client, reply, &mut buf);
                    }
                    return Ok(PostAction::Continue);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
            if buf.len() > MAX_REQUEST {
                if let Some(reply) = reply_stream.take() {
                    answer(reply, Reply::Error("request too large".into()));
                }
                return Ok(PostAction::Remove);
            }
        }
        // End of stream (or an error): the client is gone.
        if reply_stream.is_none() {
            inbox.borrow_mut().push(Incoming::Gone(client));
        }
        Ok(PostAction::Remove)
    });
    result
        .map(|_| ())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Queue the request in `buf` if complete. Gives `reply` back if not.
fn take_request<R: Request>(
    inbox: &Inbox<R>,
    client: u64,
    reply: UnixStream,
    buf: &mut Vec<u8>,
) -> Option<UnixStream> {
    match R::decode(buf) {
        Ok(Some(req)) => {
            buf.clear();
            inbox.borrow_mut().push(Incoming::Request {
                client,
                stream: reply,
                req,
            });
            None
        }
        Ok(None) => Some(reply),
        Err(e) => {
            answer(reply, Reply::Error(format!("bad request: {e}")));
            None
        }
    }
}

/// Best effort: replies are tiny, and a client that went away needs none.
fn send(stream: &UnixStream, reply: &Reply) {
    let _ = (&*stream).write_all(reply.encode().as_bytes());
}

/// Send the final reply and close, which also ends the client's watch.
fn answer(stream: UnixStream, reply: Reply) {
    send(&stream, &reply);
    let _ = stream.shutdown(Shutdown::Both);
}

/// Bind the socket, replacing a stale one left by a daemon that died.
fn bind(path: &Path) -> Result<UnixListener, Error> {
    match UnixListener::bind(path) {
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!("a daemon is already listening on {}", path.display()).into());
            }
            std::fs::remove_file(path)?;
            Ok(UnixListener::bind(path)?)
        }
        other => Ok(other?),
    }
}

/// Removes the socket on exit.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
