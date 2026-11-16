//! The socket side of a program that clients talk to: a listener on the
//! program's socket (`ipc::socket_path`) in the program's event loop,
//! which reads each client's request and queues it, and notices clients
//! hanging up. The program takes what arrived after each dispatch
//! (`Listener::take`) and answers through the `Client`.
//!
//! `run` (the daemon loop) uses it, and so can a program with its own
//! loop, such as wl-dock.

use std::cell::RefCell;
use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use wl_shell_core::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};
use wl_shell_core::{Core, Error, Program};
use wl_shell_log::warn;

use crate::ipc::{self, Reply, Request};

/// Requests are tiny; anything bigger is not a client.
const MAX_REQUEST: usize = 1 << 20;

/// A connected client whose request arrived: answer it once.
#[derive(Debug)]
pub struct Client {
    id: u64,
    stream: UnixStream,
}

impl Client {
    /// Which client it is, to match `Incoming::Gone`.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Send a reply that isn't the last (a warning). Best effort: replies
    /// are tiny, and a client that went away needs none.
    pub fn send(&self, reply: &Reply) {
        let _ = (&self.stream).write_all(reply.encode().as_bytes());
    }

    /// Send the final reply and close, which also ends the client's watch.
    pub fn answer(self, reply: Reply) {
        self.send(&reply);
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// What arrived on the socket.
#[derive(Debug)]
pub enum Incoming<R> {
    /// A complete request, from `client`.
    Request { client: Client, req: R },
    /// The client with this ID closed its connection after its request.
    Gone(u64),
}

type Inbox<R> = Rc<RefCell<Vec<Incoming<R>>>>;

/// The program's socket, listened on in its event loop. Removes the
/// socket file when dropped.
pub struct Listener<R> {
    inbox: Inbox<R>,
    path: PathBuf,
}

impl<R: Request + 'static> Listener<R> {
    /// Listen on the socket of the program (named by `wl_shell_log`),
    /// replacing a stale one left by a process that died. Fails if
    /// another process listens on it.
    pub fn bind<P: Program>(core: &Core<P>) -> Result<Self, Error> {
        let name = wl_shell_log::name();
        let path = ipc::socket_path(name).ok_or("XDG_RUNTIME_DIR is not set (or not absolute)")?;
        let listener = bind(&path)?;
        listener.set_nonblocking(true)?;
        let inbox: Inbox<R> = Rc::default();
        let accept_inbox = inbox.clone();
        let mut next_client = 0;
        let source = Generic::new(listener, Interest::READ, Mode::Level);
        let inserted = core
            .loop_handle
            .insert_source(source, move |_, listener, shell| {
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
            });
        if let Err(e) = inserted {
            let _ = std::fs::remove_file(&path);
            return Err(format!("failed to watch the socket: {e}").into());
        }
        Ok(Self { inbox, path })
    }
}

impl<R> Listener<R> {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What arrived since the last call, in order.
    pub fn take(&self) -> Vec<Incoming<R>> {
        std::mem::take(&mut *self.inbox.borrow_mut())
    }
}

impl<R> Drop for Listener<R> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read the client's request, then watch for it hanging up.
fn watch_client<P: Program, R: Request + 'static>(
    core: &Core<P>,
    stream: UnixStream,
    id: u64,
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
                        reply_stream = take_request(&inbox, id, reply, &mut buf);
                    }
                    return Ok(PostAction::Continue);
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(_) => break,
            }
            if buf.len() > MAX_REQUEST {
                if let Some(stream) = reply_stream.take() {
                    let client = Client { id, stream };
                    client.answer(Reply::Error("request too large".into()));
                }
                return Ok(PostAction::Remove);
            }
        }
        // End of stream (or an error): the client is gone.
        if reply_stream.is_none() {
            inbox.borrow_mut().push(Incoming::Gone(id));
        }
        Ok(PostAction::Remove)
    });
    result
        .map(|_| ())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// Queue the request in `buf` if complete. Gives `stream` back if not.
fn take_request<R: Request>(
    inbox: &Inbox<R>,
    id: u64,
    stream: UnixStream,
    buf: &mut Vec<u8>,
) -> Option<UnixStream> {
    match R::decode(buf) {
        Ok(Some(req)) => {
            buf.clear();
            let client = Client { id, stream };
            inbox.borrow_mut().push(Incoming::Request { client, req });
            None
        }
        Ok(None) => Some(stream),
        Err(e) => {
            let client = Client { id, stream };
            client.answer(Reply::Error(format!("bad request: {e}")));
            None
        }
    }
}

/// Bind the socket, replacing a stale one left by a process that died.
fn bind(path: &Path) -> Result<UnixListener, Error> {
    match UnixListener::bind(path) {
        Err(e) if e.kind() == ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!("another process is listening on {}", path.display()).into());
            }
            std::fs::remove_file(path)?;
            Ok(UnixListener::bind(path)?)
        }
        other => Ok(other?),
    }
}
