//! Watching a directory tree with inotify: a directory and every
//! directory under it, with watches added as directories are created or
//! moved in, and what happened said per file, relative to the tree.
//!
//! Renames within the tree come as one `Moved` (inotify's `MOVED_FROM`
//! and `MOVED_TO` with the same cookie, matched up), so a program keeping
//! something per path can follow them. Moves into the tree come as
//! `Created`, and out of it as `Removed`. A directory created or moved in
//! is reported with what is already in it, since that may have been made
//! before its watch was. Lost events (`IN_Q_OVERFLOW`) come as `Rescan`.
//!
//! Directories starting with `.` (`.git`, editors' temporary
//! directories) aren't watched, and symlinked directories aren't
//! followed. Events about dot files in watched directories are reported.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken, generic::Generic};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

/// What happened in the tree; paths are relative to its root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEvent {
    /// A file was written and closed.
    Written(PathBuf),
    /// A file or directory appeared: created, or moved in.
    Created { path: PathBuf, dir: bool },
    /// A file or directory went: removed, or moved out.
    Removed { path: PathBuf, dir: bool },
    /// Renamed within the tree.
    Moved {
        from: PathBuf,
        to: PathBuf,
        dir: bool,
    },
    /// Events were lost: anything may have changed.
    Rescan,
}

/// Call `f` with what happened in `dir` and under it, a batch at a time
/// (all the events read at one wakeup). `dir` must exist. Remove the
/// returned token from the loop to stop watching.
pub fn watch_tree<D: 'static>(
    handle: &LoopHandle<'static, D>,
    dir: &Path,
    mut f: impl FnMut(&mut D, Vec<TreeEvent>) + 'static,
) -> io::Result<RegistrationToken> {
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    let mut tree = Tree {
        root: dir.to_path_buf(),
        dirs: HashMap::new(),
    };
    // The root must be watched; what is under it, as far as it can be.
    let root = inotify::add_watch(&fd, dir, flags())?;
    tree.dirs.insert(root, PathBuf::new());
    tree.add_below(&fd, Path::new(""), None);
    let source = Generic::new(fd, Interest::READ, Mode::Level);
    handle
        .insert_source(source, move |_, fd, data| {
            let raw = read(&**fd);
            if raw.is_empty() {
                return Ok(PostAction::Continue);
            }
            let events = tree.process(&**fd, raw);
            if !events.is_empty() {
                f(data, events);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| io::Error::other(e.to_string()))
}

fn flags() -> WatchFlags {
    WatchFlags::CLOSE_WRITE
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO
        | WatchFlags::ONLYDIR
        | WatchFlags::DONT_FOLLOW
}

/// An event as read.
struct Raw {
    wd: i32,
    mask: ReadFlags,
    cookie: u32,
    name: Option<Vec<u8>>,
}

/// Every event queued.
fn read(fd: impl AsFd) -> Vec<Raw> {
    let mut buf = [MaybeUninit::uninit(); 8192];
    let mut reader = inotify::Reader::new(fd, &mut buf);
    let mut out = Vec::new();
    loop {
        match reader.next() {
            Ok(e) => out.push(Raw {
                wd: e.wd(),
                mask: e.events(),
                cookie: e.cookie(),
                name: e.file_name().map(|n| n.to_bytes().to_vec()),
            }),
            Err(rustix::io::Errno::INTR) => continue,
            // WOULDBLOCK: all read. Anything else: try again on the next
            // wakeup.
            Err(_) => break,
        }
    }
    out
}

/// Whether a directory named so is left out.
fn skipped(name: &OsStr) -> bool {
    name.as_bytes().starts_with(b".")
}

/// Whether `rel` is in or under a directory left out.
fn hidden(rel: &Path) -> bool {
    rel.components().any(|c| skipped(c.as_os_str()))
}

struct Tree {
    root: PathBuf,
    /// The watched directories, relative to the root, by watch.
    dirs: HashMap<i32, PathBuf>,
}

impl Tree {
    fn watched(&self, rel: &Path) -> bool {
        self.dirs.values().any(|d| d == rel)
    }

    /// Watch `rel` and the directories under it.
    fn add(&mut self, fd: impl AsFd + Copy, rel: &Path, found: Option<&mut Vec<TreeEvent>>) {
        if hidden(rel) {
            return;
        }
        match inotify::add_watch(fd, self.root.join(rel), flags()) {
            Ok(wd) => {
                self.dirs.insert(wd, rel.to_path_buf());
            }
            Err(_) => return, // gone again, or not a directory
        }
        self.add_below(fd, rel, found);
    }

    /// Watch the directories under `rel`, reporting what is in them to
    /// `found` if given.
    fn add_below(
        &mut self,
        fd: impl AsFd + Copy,
        rel: &Path,
        mut found: Option<&mut Vec<TreeEvent>>,
    ) {
        let Ok(entries) = std::fs::read_dir(self.root.join(rel)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = rel.join(&name);
            if let Some(found) = found.as_deref_mut() {
                found.push(TreeEvent::Created {
                    path: path.clone(),
                    dir: kind.is_dir(),
                });
            }
            if kind.is_dir() && !skipped(&name) {
                self.add(fd, &path, found.as_deref_mut());
            }
        }
    }

    /// Stop watching `rel` and everything under it.
    fn remove(&mut self, fd: impl AsFd + Copy, rel: &Path) {
        self.dirs.retain(|wd, d| {
            let under = d.starts_with(rel);
            if under {
                let _ = inotify::remove_watch(fd, *wd);
            }
            !under
        });
    }

    /// `from` (watched or not) is now `to`.
    fn moved(&mut self, fd: impl AsFd + Copy, from: &Path, to: &Path, found: &mut Vec<TreeEvent>) {
        if hidden(to) {
            return self.remove(fd, from);
        }
        if !self.watched(from) {
            return self.add(fd, to, Some(found));
        }
        for d in self.dirs.values_mut() {
            if let Ok(rest) = d.strip_prefix(from) {
                *d = to.join(rest);
            }
        }
    }

    /// Make events of what was read, following the directories.
    fn process(&mut self, fd: impl AsFd + Copy, raw: Vec<Raw>) -> Vec<TreeEvent> {
        let mut out = Vec::new();
        // Moves out not matched yet: the cookie, and where their
        // `Removed` is in `out`.
        let mut froms: HashMap<u32, usize> = HashMap::new();
        let mut rescan = false;
        for e in raw {
            if e.mask.contains(ReadFlags::QUEUE_OVERFLOW) {
                rescan = true;
                continue;
            }
            if e.mask.contains(ReadFlags::IGNORED) {
                self.dirs.remove(&e.wd);
                continue;
            }
            let (Some(dir), Some(name)) = (self.dirs.get(&e.wd), &e.name) else {
                continue;
            };
            let path = dir.join(OsStr::from_bytes(name));
            let is_dir = e.mask.contains(ReadFlags::ISDIR);
            if e.mask.contains(ReadFlags::CLOSE_WRITE) {
                out.push(TreeEvent::Written(path));
            } else if e.mask.contains(ReadFlags::CREATE) {
                out.push(TreeEvent::Created {
                    path: path.clone(),
                    dir: is_dir,
                });
                if is_dir {
                    let mut found = Vec::new();
                    self.add(fd, &path, Some(&mut found));
                    out.extend(found);
                }
            } else if e.mask.contains(ReadFlags::DELETE) {
                out.push(TreeEvent::Removed { path, dir: is_dir });
            } else if e.mask.contains(ReadFlags::MOVED_FROM) {
                froms.insert(e.cookie, out.len());
                out.push(TreeEvent::Removed { path, dir: is_dir });
            } else if e.mask.contains(ReadFlags::MOVED_TO) {
                let from = froms.remove(&e.cookie).and_then(|i| match &out[i] {
                    TreeEvent::Removed { path, .. } => Some((i, path.clone())),
                    _ => None,
                });
                let mut found = Vec::new();
                match from {
                    Some((i, from)) => {
                        if is_dir {
                            self.moved(fd, &from, &path, &mut found);
                        }
                        out[i] = TreeEvent::Moved {
                            from,
                            to: path,
                            dir: is_dir,
                        };
                    }
                    None => {
                        out.push(TreeEvent::Created {
                            path: path.clone(),
                            dir: is_dir,
                        });
                        if is_dir {
                            self.add(fd, &path, Some(&mut found));
                        }
                    }
                }
                out.extend(found);
            }
        }
        // Moved out: stop watching what went.
        for i in froms.into_values() {
            if let TreeEvent::Removed { path, dir: true } = &out[i] {
                let path = path.clone();
                self.remove(fd, &path);
            }
        }
        if rescan {
            // Watch whatever directories were missed (watching one again
            // keeps its watch).
            self.add_below(fd, Path::new(""), None);
            out.push(TreeEvent::Rescan);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    struct Watched {
        dir: PathBuf,
        event_loop: calloop::EventLoop<'static, ()>,
        events: Rc<RefCell<Vec<TreeEvent>>>,
    }

    impl Watched {
        fn new(name: &str, setup: impl FnOnce(&Path)) -> Self {
            let dir =
                std::env::temp_dir().join(format!("wl-shell-tree-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            setup(&dir);
            let event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
            let events = Rc::new(RefCell::new(Vec::new()));
            let e = events.clone();
            watch_tree(&event_loop.handle(), &dir, move |_, batch| {
                e.borrow_mut().extend(batch)
            })
            .unwrap();
            Self {
                dir,
                event_loop,
                events,
            }
        }

        /// What happened since the last call.
        fn take(&mut self) -> Vec<TreeEvent> {
            for _ in 0..3 {
                self.event_loop
                    .dispatch(Some(Duration::from_millis(30)), &mut ())
                    .unwrap();
            }
            std::mem::take(&mut *self.events.borrow_mut())
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.join(rel)
        }
    }

    impl Drop for Watched {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn written(e: &[TreeEvent], rel: &str) -> bool {
        e.contains(&TreeEvent::Written(p(rel)))
    }

    #[test]
    fn nested_files() {
        let mut w = Watched::new("nested", |d| {
            std::fs::create_dir_all(d.join("a/b")).unwrap();
            std::fs::create_dir_all(d.join(".git/x")).unwrap();
        });
        std::fs::write(w.path("top.md"), "1").unwrap();
        std::fs::write(w.path("a/b/deep.md"), "2").unwrap();
        std::fs::write(w.path(".git/x/obj"), "3").unwrap();
        let e = w.take();
        assert!(written(&e, "top.md"), "{e:?}");
        assert!(written(&e, "a/b/deep.md"), "{e:?}");
        // Dot directories aren't watched.
        assert!(!e.iter().any(|e| format!("{e:?}").contains("obj")), "{e:?}");
        std::fs::remove_file(w.path("a/b/deep.md")).unwrap();
        let e = w.take();
        assert_eq!(
            e,
            [TreeEvent::Removed {
                path: p("a/b/deep.md"),
                dir: false
            }]
        );
    }

    #[test]
    fn new_subdirectories() {
        let mut w = Watched::new("subdirs", |_| {});
        std::fs::create_dir_all(w.path("new/inner")).unwrap();
        std::fs::write(w.path("new/inner/early.md"), "x").unwrap();
        let e = w.take();
        assert!(
            e.contains(&TreeEvent::Created {
                path: p("new"),
                dir: true
            }),
            "{e:?}"
        );
        // Made before its watch, maybe: reported either way.
        assert!(
            e.iter().any(|e| matches!(e,
                TreeEvent::Created { path, .. } | TreeEvent::Written(path)
                    if *path == p("new/inner/early.md"))),
            "{e:?}"
        );
        // And watched from now on.
        std::fs::write(w.path("new/inner/late.md"), "y").unwrap();
        assert!(written(&w.take(), "new/inner/late.md"));
    }

    #[test]
    fn renames_within_the_tree_are_one_event() {
        let mut w = Watched::new("renames", |d| {
            std::fs::create_dir_all(d.join("work")).unwrap();
            std::fs::create_dir_all(d.join("dir/sub")).unwrap();
            std::fs::write(d.join("a.md"), "a").unwrap();
        });
        std::fs::rename(w.path("a.md"), w.path("work/b.md")).unwrap();
        assert_eq!(
            w.take(),
            [TreeEvent::Moved {
                from: p("a.md"),
                to: p("work/b.md"),
                dir: false
            }]
        );
        // A directory: what is under it is watched where it went.
        std::fs::rename(w.path("dir"), w.path("work/moved")).unwrap();
        assert_eq!(
            w.take(),
            [TreeEvent::Moved {
                from: p("dir"),
                to: p("work/moved"),
                dir: true
            }]
        );
        std::fs::write(w.path("work/moved/sub/c.md"), "c").unwrap();
        assert!(written(&w.take(), "work/moved/sub/c.md"));
    }

    #[test]
    fn moves_in_and_out_are_creates_and_removes() {
        let outside =
            std::env::temp_dir().join(format!("wl-shell-tree-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(outside.join("folder")).unwrap();
        std::fs::write(outside.join("in.md"), "x").unwrap();
        std::fs::write(outside.join("folder/f.md"), "y").unwrap();
        let mut w = Watched::new("inout", |d| {
            std::fs::create_dir_all(d.join("leaving")).unwrap();
        });
        std::fs::rename(outside.join("in.md"), w.path("in.md")).unwrap();
        std::fs::rename(outside.join("folder"), w.path("folder")).unwrap();
        let e = w.take();
        assert!(
            e.contains(&TreeEvent::Created {
                path: p("in.md"),
                dir: false
            }),
            "{e:?}"
        );
        assert!(
            e.contains(&TreeEvent::Created {
                path: p("folder/f.md"),
                dir: false
            }),
            "{e:?}"
        );
        std::fs::write(w.path("folder/g.md"), "z").unwrap();
        assert!(written(&w.take(), "folder/g.md"));
        std::fs::rename(w.path("in.md"), outside.join("in.md")).unwrap();
        std::fs::rename(w.path("leaving"), outside.join("leaving")).unwrap();
        let e = w.take();
        assert_eq!(
            e,
            [
                TreeEvent::Removed {
                    path: p("in.md"),
                    dir: false
                },
                TreeEvent::Removed {
                    path: p("leaving"),
                    dir: true
                },
            ]
        );
        // Gone from the tree: not reported anymore.
        std::fs::write(outside.join("leaving/x.md"), "x").unwrap();
        assert!(w.take().is_empty());
        let _ = std::fs::remove_dir_all(&outside);
    }
}
