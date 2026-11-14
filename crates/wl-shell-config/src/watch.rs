//! Watching a file with inotify, for programs that are always running and
//! pick up changes to their config (or state) files without being asked.
//!
//! The directory is watched rather than the file, so editors that save by
//! writing a new file and renaming it over the old one, files that don't
//! exist yet, and files that are removed all work.

use std::ffi::OsString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken, generic::Generic};
use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};

/// Call `f` whenever the file at `path` may have changed: written,
/// created, renamed over or removed. Its directory must exist. Remove the
/// returned token from the loop to stop watching.
pub fn watch_file<D: 'static>(
    handle: &LoopHandle<'static, D>,
    path: &Path,
    mut f: impl FnMut(&mut D) + 'static,
) -> io::Result<RegistrationToken> {
    let dir = (path.parent())
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name: OsString = (path.file_name())
        .ok_or_else(|| io::Error::other(format!("{}: not a file", path.display())))?
        .to_owned();
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)?;
    let flags = WatchFlags::CLOSE_WRITE
        | WatchFlags::CREATE
        | WatchFlags::DELETE
        | WatchFlags::MOVED_FROM
        | WatchFlags::MOVED_TO;
    inotify::add_watch(&fd, dir, flags)?;
    let source = Generic::new(fd, Interest::READ, Mode::Level);
    handle
        .insert_source(source, move |_, fd, data| {
            let mut buf = [MaybeUninit::uninit(); 4096];
            let mut reader = inotify::Reader::new(&**fd, &mut buf);
            let mut hit = false;
            loop {
                match reader.next() {
                    Ok(event) => {
                        // Events were lost: any of them may have been ours.
                        hit |= event.events().contains(ReadFlags::QUEUE_OVERFLOW)
                            || event
                                .file_name()
                                .is_some_and(|n| n.to_bytes() == name.as_bytes());
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    // WOULDBLOCK: all read. Anything else: try again on
                    // the next wakeup.
                    Err(_) => break,
                }
            }
            if hit {
                f(data);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| io::Error::other(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::Duration;

    #[test]
    fn sees_writes_and_renames_of_its_file_only() {
        let dir = std::env::temp_dir().join(format!("wl-shell-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.toml");
        let mut event_loop: calloop::EventLoop<()> = calloop::EventLoop::try_new().unwrap();
        let hits = Rc::new(Cell::new(0));
        let h = hits.clone();
        watch_file(&event_loop.handle(), &file, move |_| h.set(h.get() + 1)).unwrap();
        let mut dispatch = || {
            event_loop
                .dispatch(Some(Duration::from_millis(50)), &mut ())
                .unwrap()
        };

        std::fs::write(dir.join("other"), "x").unwrap();
        dispatch();
        assert_eq!(hits.get(), 0);

        std::fs::write(&file, "a = 1").unwrap();
        dispatch();
        assert!(hits.get() > 0);

        let before = hits.get();
        std::fs::write(dir.join("tmp"), "a = 2").unwrap();
        std::fs::rename(dir.join("tmp"), &file).unwrap();
        dispatch();
        assert!(hits.get() > before);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
