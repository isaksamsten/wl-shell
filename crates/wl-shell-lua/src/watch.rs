//! `dock.watch(path, fn, opts)`: calling `fn(paths)` when files change.
//!
//! ```lua
//! local w = dock.watch('~/.calendars', function(paths) end, { recursive = true })
//! w:cancel()
//! ```
//!
//! - One inotify thread serves every state, the way D-Bus has one
//!   connection thread per bus. Adding and cancelling a watch only
//!   touches its tables (and inotify's), so a plugin never waits on it.
//! - A folder is watched itself, with its subfolders when `recursive`
//!   (not the ones starting with `.`, and not through symlinks; folders
//!   made or moved in later are added, with what is already in them). A
//!   file is watched through its folder, so it is still seen when it is
//!   replaced by a rename (how editors save) or created later; its folder
//!   must exist.
//! - Changes are gathered for `GATHER` from the first one, then handed to
//!   the state as one job with the changed paths (absolute when the
//!   watched path is, sorted, each once). Lost events (inotify's queue
//!   overflowing) come as the watched path itself.
//! - Watches belong to the state that made them: it dropping (a reload,
//!   a restart, an unload) cancels them, and at most `MAX_WATCHES` are
//!   live per state.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use wl_shell_log::warn;

use crate::runtime::{Job, Link, lock};
use crate::state::{WATCHES, host};

/// How long changes are gathered before they are handed on.
pub(crate) const GATHER: Duration = Duration::from_millis(200);
/// Live watches per state.
pub(crate) const MAX_WATCHES: usize = 256;
/// Folders one recursive watch watches at most.
const MAX_DIRS: usize = 4096;

fn flags() -> WatchFlags {
    WatchFlags::CLOSE_WRITE
        | WatchFlags::MODIFY
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO
        | WatchFlags::DELETE_SELF
        | WatchFlags::MOVE_SELF
}

struct Watcher {
    fd: OwnedFd,
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    watches: HashMap<u64, Watch>,
    /// Watched folders by inotify watch descriptor.
    dirs: HashMap<i32, Dir>,
}

struct Dir {
    path: PathBuf,
    /// The watches it serves.
    users: HashSet<u64>,
}

struct Watch {
    link: Link,
    epoch: u64,
    /// What was asked for: a file or a folder.
    path: PathBuf,
    /// Watching a file: its name in the folder watched.
    name: Option<OsString>,
    recursive: bool,
    dirs: HashSet<i32>,
    changed: BTreeSet<PathBuf>,
    /// When `changed` is handed on.
    due: Option<Instant>,
}

impl Watch {
    fn mark(&mut self, path: PathBuf, now: Instant) {
        self.changed.insert(path);
        self.due.get_or_insert(now + GATHER);
    }
}

static WATCHER: OnceLock<Result<&'static Watcher, String>> = OnceLock::new();

/// The watcher, its thread started when first asked for.
fn watcher() -> Result<&'static Watcher, String> {
    WATCHER
        .get_or_init(|| {
            let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
                .map_err(|e| format!("inotify: {e}"))?;
            let w: &'static Watcher = Box::leak(Box::new(Watcher {
                fd,
                inner: Mutex::default(),
            }));
            std::thread::Builder::new()
                .name("lua-watch".into())
                .spawn(move || w.serve())
                .map_err(|e| format!("watch thread: {e}"))?;
            Ok(w)
        })
        .clone()
}

/// Whether `name` is a dot folder, which recursive watches skip.
fn hidden(name: &OsStr) -> bool {
    name.as_bytes().first() == Some(&b'.')
}

/// Start watching `path` for watch `id` of the state behind `link`.
fn add(link: Link, epoch: u64, id: u64, path: PathBuf, recursive: bool) -> Result<(), String> {
    let w = watcher()?;
    let (dir, name) = match path.is_dir() {
        true => (path.clone(), None),
        false => {
            let name = (path.file_name())
                .ok_or_else(|| format!("{}: not a file or folder", path.display()))?;
            let dir = (path.parent())
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            (dir.to_path_buf(), Some(name.to_owned()))
        }
    };
    let recursive = recursive && name.is_none();
    let mut inner = lock(&w.inner);
    inner.watches.insert(
        id,
        Watch {
            link,
            epoch,
            path,
            name,
            recursive,
            dirs: HashSet::new(),
            changed: BTreeSet::new(),
            due: None,
        },
    );
    if let Err(e) = w.add_dir(&mut inner, id, &dir) {
        w.remove(&mut inner, id);
        return Err(format!("{}: {e}", dir.display()));
    }
    if recursive {
        w.add_below(&mut inner, id, &dir, false);
    }
    Ok(())
}

/// Stop watch `id`.
pub(crate) fn cancel(id: u64) {
    if let Some(Ok(w)) = WATCHER.get() {
        w.remove(&mut lock(&w.inner), id);
    }
}

impl Watcher {
    /// Watch the folder `dir` for watch `id`.
    fn add_dir(&self, inner: &mut Inner, id: u64, dir: &Path) -> rustix::io::Result<()> {
        let wd = inotify::add_watch(&self.fd, dir, flags())?;
        let d = inner.dirs.entry(wd).or_insert_with(|| Dir {
            path: dir.to_path_buf(),
            users: HashSet::new(),
        });
        d.users.insert(id);
        if let Some(w) = inner.watches.get_mut(&id) {
            w.dirs.insert(wd);
        }
        Ok(())
    }

    /// Watch the folders under `dir` for watch `id`; with `report`, what
    /// is in them (files and folders), since it may have been made
    /// before they were watched.
    fn add_below(&self, inner: &mut Inner, id: u64, dir: &Path, report: bool) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(dir) = todo.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                if is_dir && !hidden(&entry.file_name()) {
                    let full = (inner.watches.get(&id)).is_none_or(|w| w.dirs.len() >= MAX_DIRS);
                    if full {
                        warn!("watch: more than {MAX_DIRS} folders under {}", dir.display());
                        return found;
                    }
                    if self.add_dir(inner, id, &path).is_ok() {
                        todo.push(path.clone());
                    }
                }
                if report {
                    found.push(path);
                }
            }
        }
        found
    }

    /// Watch `id` no longer needs the folder `wd`.
    fn release(&self, inner: &mut Inner, wd: i32, id: u64) {
        if let Some(w) = inner.watches.get_mut(&id) {
            w.dirs.remove(&wd);
        }
        if let Some(d) = inner.dirs.get_mut(&wd) {
            d.users.remove(&id);
            if d.users.is_empty() {
                inner.dirs.remove(&wd);
                let _ = inotify::remove_watch(&self.fd, wd);
            }
        }
    }

    fn remove(&self, inner: &mut Inner, id: u64) {
        let Some(w) = inner.watches.get(&id) else {
            return;
        };
        for wd in w.dirs.clone() {
            self.release(inner, wd, id);
        }
        inner.watches.remove(&id);
    }

    /// The thread: read events, and hand on what is due.
    fn serve(&self) {
        let mut buf = vec![MaybeUninit::<u8>::uninit(); 16 << 10];
        loop {
            let due = (lock(&self.inner).watches.values())
                .filter_map(|w| w.due)
                .min();
            // Rounded up, so it doesn't wake just before.
            let timeout = due.map(|due| {
                let left = due.saturating_duration_since(Instant::now());
                Timespec::try_from(left + Duration::from_millis(1)).unwrap_or(Timespec {
                    tv_sec: 1,
                    tv_nsec: 0,
                })
            });
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => {
                    warn!("watch: {e}");
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            self.read(&mut buf);
            self.flush();
        }
    }

    /// Take in the events waiting.
    fn read(&self, buf: &mut [MaybeUninit<u8>]) {
        let mut inner = lock(&self.inner);
        let mut reader = inotify::Reader::new(&self.fd, buf);
        loop {
            let (wd, flags, name) = match reader.next() {
                Ok(e) => (
                    e.wd(),
                    e.events(),
                    (e.file_name())
                        .map(|n| OsStr::from_bytes(n.to_bytes()).to_owned())
                        .filter(|n| !n.is_empty()),
                ),
                Err(Errno::INTR) => continue,
                // WOULDBLOCK: all read.
                Err(_) => break,
            };
            self.event(&mut inner, wd, flags, name);
        }
    }

    fn event(&self, inner: &mut Inner, wd: i32, flags: ReadFlags, name: Option<OsString>) {
        let now = Instant::now();
        if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            // Anything may have changed.
            for w in inner.watches.values_mut() {
                let path = w.path.clone();
                w.mark(path, now);
            }
            return;
        }
        let Some(dir) = inner.dirs.get(&wd) else {
            return;
        };
        let users: Vec<u64> = dir.users.iter().copied().collect();
        if flags.contains(ReadFlags::IGNORED) {
            // The folder is gone (or was released).
            inner.dirs.remove(&wd);
            for id in users {
                if let Some(w) = inner.watches.get_mut(&id) {
                    w.dirs.remove(&wd);
                }
            }
            return;
        }
        let path = match &name {
            Some(n) => dir.path.join(n),
            None => dir.path.clone(),
        };
        let subdir = flags.contains(ReadFlags::ISDIR) && name.as_deref().is_some_and(|n| !hidden(n));
        for id in users {
            let Some(w) = inner.watches.get_mut(&id) else {
                continue;
            };
            if let Some(file) = &w.name {
                // Its file, or its folder itself going.
                if name.is_none() || name.as_ref() == Some(file) {
                    let p = w.path.clone();
                    w.mark(p, now);
                }
                continue;
            }
            w.mark(path.clone(), now);
            if !(w.recursive && subdir) {
                continue;
            }
            if flags.intersects(ReadFlags::CREATE | ReadFlags::MOVED_TO) {
                if self.add_dir(inner, id, &path).is_ok() {
                    let found = self.add_below(inner, id, &path, true);
                    if let Some(w) = inner.watches.get_mut(&id) {
                        for p in found {
                            w.mark(p, now);
                        }
                    }
                }
            } else if flags.contains(ReadFlags::MOVED_FROM) {
                // Moved out (or renamed, and back as MOVED_TO): its
                // folders aren't under this one any more.
                let gone: Vec<i32> = (w.dirs.iter())
                    .copied()
                    .filter(|wd| inner.dirs.get(wd).is_some_and(|d| d.path.starts_with(&path)))
                    .collect();
                for wd in gone {
                    self.release(inner, wd, id);
                }
            }
        }
    }

    /// Hand on the changes that are due.
    fn flush(&self) {
        let now = Instant::now();
        let mut due = Vec::new();
        for (id, w) in lock(&self.inner).watches.iter_mut() {
            if w.due.is_some_and(|d| d <= now) {
                w.due = None;
                let paths: Vec<String> = (std::mem::take(&mut w.changed).into_iter())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect();
                due.push((*id, w.link.clone(), w.epoch, paths));
            }
        }
        let gone: Vec<u64> = (due.into_iter())
            .filter(|(id, link, epoch, paths)| {
                !link.push(Job::Watch {
                    epoch: *epoch,
                    id: *id,
                    paths: paths.clone(),
                })
            })
            .map(|(id, ..)| id)
            .collect();
        if !gone.is_empty() {
            let mut inner = lock(&self.inner);
            for id in gone {
                self.remove(&mut inner, id);
            }
        }
    }
}

// -- the Lua side ----------------------------------------------------------

/// Add `ns.watch`.
pub(crate) fn install(lua: &Lua, ns: &Table) -> mlua::Result<()> {
    ns.set("watch", lua.create_function(watch)?)
}

/// `watch(path, fn, opts)`: a handle, or nil and the error.
fn watch(
    lua: &Lua,
    (path, f, opts): (String, Function, Option<Table>),
) -> mlua::Result<(Option<WatchHandle>, Option<String>)> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let recursive = match &opts {
        Some(o) => o.get::<Option<bool>>("recursive")?.unwrap_or(false),
        None => false,
    };
    let host = host(lua)?;
    let mut ids = lock(&host.watches);
    if ids.len() >= MAX_WATCHES {
        return Err(mlua::Error::runtime(format!(
            "watch: at most {MAX_WATCHES} watches"
        )));
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let callbacks: Table = lua.named_registry_value(WATCHES)?;
    callbacks.set(id, f)?;
    let path = crate::host::expand(&path);
    match add(host.link.clone(), host.epoch, id, path, recursive) {
        Ok(()) => {
            ids.push(id);
            Ok((Some(WatchHandle(id)), None))
        }
        Err(e) => {
            callbacks.set(id, Value::Nil)?;
            Ok((None, Some(e)))
        }
    }
}

/// A watch `watch` started.
struct WatchHandle(u64);

impl UserData for WatchHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |lua, this, ()| {
            let callbacks: Table = lua.named_registry_value(WATCHES)?;
            callbacks.set(this.0, Value::Nil)?;
            let host = host(lua)?;
            let mut ids = lock(&host.watches);
            if let Some(i) = ids.iter().position(|id| *id == this.0) {
                ids.remove(i);
                cancel(this.0);
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_folders_are_hidden() {
        assert!(hidden(OsStr::new(".git")));
        assert!(!hidden(OsStr::new("work")));
    }
}

