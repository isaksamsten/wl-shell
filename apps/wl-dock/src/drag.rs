//! Dragging: icons within the dock, and files dropped on it from outside.
//! Pure, unit tested; view.rs shows the drags and app.rs applies drops.
//!
//! Dragging an icon (an app, a standalone widget or a folder):
//!
//! - Only pinned items have an order to change, apps among the apps and
//!   folders among the folders. A pinned item moves the gap among the
//!   other pinned items of its zone; past the last one, the gap stays at
//!   their end.
//! - A widget goes anywhere: among the apps before the separator, among
//!   the folders past its middle (`zone`), so up to just before the Trash.
//!   In the other zone it has no place of its own: the gap is always open
//!   there.
//! - A running app that isn't pinned, dragged among the pinned apps, gets
//!   pinned there. Past them (half an icon after the last), it is back
//!   among the running apps, and dropping it there changes nothing.
//! - On the Trash, a pinned item is unpinned (a widget removed from the
//!   dock). A running app that isn't pinned has nothing to remove, so the
//!   Trash doesn't take it.
//! - Off the dock, the item goes back to its place, and a drop there
//!   changes nothing: dragging off the dock never unpins.
//!
//! Positions are along the dock in the magnified layout as shown, gap
//! included. The gap opens where the pointer is, so the items around it
//! move away from the pointer and the choice holds still.
//!
//! Dropping paths from outside (`Payload`): what each target takes.
//!
//! - Between the folders (from the separator on), only folders: a gap
//!   opens and they get pinned there. A drag holding any file opens no
//!   gap and isn't taken between icons: files are never pinned.
//! - On a folder, files and folders that aren't the folder itself, one of
//!   its parents, or in it already, are moved or copied into it. Folders
//!   go into a folder only from the middle of its icon; nearer its edges
//!   they open the gap.
//! - On an app, they open with it, if it can open files (app.rs decides).
//! - On the Trash, they are moved to the trash.

use std::path::{Path, PathBuf};

use crate::pins::{self, Pin, Zone};

/// The item being dragged, among the pinned items of its zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dragged {
    /// The `i`th pinned app, folder or widget of its zone (from 0).
    Pinned(usize),
    /// A running app that isn't pinned.
    Running,
    /// A widget from the other zone.
    Across,
}

/// The zone a widget dragged with the pointer at `u` goes to, with the
/// separator's center at `separator` (None: there is none, so no
/// folders' zone to go to).
pub fn zone(u: f64, separator: Option<f64>) -> Zone {
    match separator {
        Some(s) if u >= s => Zone::Folders,
        _ => Zone::Apps,
    }
}

/// Where a drop would put the dragged item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// Its own place: the drop changes nothing.
    Home,
    /// Among the pinned items of its kind, before the `k`th of the others
    /// (from 0).
    Gap(usize),
    /// The Trash, which takes it: it is removed from the dock.
    Trash,
}

/// What a drop does to the pins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drop {
    /// Pin the item as the `k`th of its kind (moving it if it is pinned).
    Pin(usize),
    Unpin,
    Nothing,
}

/// Where the pointer is relative to the dock, as shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scene<'a> {
    /// The centers of the pinned items of the dragged one's kind, other
    /// than it, in order.
    pub pinned: &'a [f64],
    /// Where those end (the gap included, if it is after them), or where
    /// they would start if there are none.
    pub pinned_end: f64,
    /// An icon and a gap, at rest.
    pub step: f64,
    /// The pointer is over the dock (else a drop cancels).
    pub on_dock: bool,
    /// The pointer is over the Trash.
    pub on_trash: bool,
}

/// Where a drop of `dragged` with the pointer at `u` would put it.
pub fn target(dragged: Dragged, s: &Scene, u: f64) -> Target {
    if !s.on_dock {
        return Target::Home;
    }
    if s.on_trash {
        return match trash_takes(dragged) {
            true => Target::Trash,
            false => Target::Home,
        };
    }
    let k = s.pinned.iter().filter(|&&c| c < u).count();
    match dragged {
        Dragged::Pinned(i) if i == k => Target::Home,
        Dragged::Pinned(_) | Dragged::Across => Target::Gap(k),
        Dragged::Running if k < s.pinned.len() || u < s.pinned_end + s.step / 2.0 => Target::Gap(k),
        Dragged::Running => Target::Home,
    }
}

/// Whether the Trash takes `dragged`.
pub fn trash_takes(dragged: Dragged) -> bool {
    matches!(dragged, Dragged::Pinned(_) | Dragged::Across)
}

/// What dropping `dragged` on `target` does.
pub fn drop(dragged: Dragged, target: Target) -> Drop {
    match target {
        Target::Home => Drop::Nothing,
        Target::Gap(k) => Drop::Pin(k),
        Target::Trash if trash_takes(dragged) => Drop::Unpin,
        Target::Trash => Drop::Nothing,
    }
}

/// Apply `drop` of the item pinned (or to be pinned) as `pin` to `pins`
/// (a widget's `pin` saying the zone it goes to).
/// Returns whether they changed.
pub fn apply(pins: &mut Vec<Pin>, pin: Pin, drop: Drop) -> bool {
    match drop {
        Drop::Pin(k) => pins::place(pins, pin, k),
        Drop::Unpin => pins::unpin(pins, &pin),
        Drop::Nothing => false,
    }
}

// -- drops from outside -----------------------------------------------------

/// Paths dragged in from outside the dock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Payload {
    pub paths: Vec<PathBuf>,
    /// Every path is a folder: they can be pinned.
    pub folders: bool,
}

impl Payload {
    /// `paths`, looked up on disk.
    pub fn new(paths: Vec<PathBuf>) -> Self {
        let folders = !paths.is_empty() && paths.iter().all(|p| p.is_dir());
        Self { paths, folders }
    }
}

/// Whether the folder at `dest` takes `paths` into it: none is the
/// folder itself or one of its parents, and not all are in it already.
pub fn folder_takes(dest: &Path, paths: &[PathBuf]) -> bool {
    !paths.is_empty()
        && !paths.iter().any(|p| dest.starts_with(p))
        && !paths.iter().all(|p| p.parent() == Some(dest))
}

/// What a drop from outside does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropTarget {
    /// Nothing: the drag isn't taken here.
    Nothing,
    /// Pin the folders as the `k`th folders.
    Pin(usize),
    /// Open the paths with the app of this key.
    Open(String),
    /// Move or copy the paths into the folder of this key.
    Into(String),
    /// Move the paths to the trash.
    Trash,
}

/// What an item under a drag from outside is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitKind {
    App,
    Folder,
    Trash,
    Separator,
}

/// The item under the drag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit<'a> {
    pub key: &'a str,
    pub kind: HitKind,
    /// It takes the drag.
    pub takes: bool,
    /// Where the pointer is across its cell, 0 (start) to 1 (end).
    pub at: f64,
}

/// Where a drag from outside is, as the dock shows it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropScene<'a> {
    /// The pointer is over the dock.
    pub on_dock: bool,
    pub hit: Option<Hit<'a>>,
    /// The folders' centers, in order (the gap included).
    pub folders: &'a [f64],
    /// Where the folders' place starts: the separator, else the end of
    /// the apps.
    pub zone: f64,
    /// The drag holds folders only: it can be pinned.
    pub pinnable: bool,
}

/// Where a folder takes folders, across its cell: nearer its edges, they
/// open the gap instead.
const INTO: std::ops::RangeInclusive<f64> = 0.25..=0.75;

/// What dropping with the pointer at `u` does.
pub fn drop_target(s: &DropScene, u: f64) -> DropTarget {
    if !s.on_dock {
        return DropTarget::Nothing;
    }
    if let Some(hit) = s.hit.filter(|h| h.takes) {
        match hit.kind {
            HitKind::Trash => return DropTarget::Trash,
            HitKind::Folder if !s.pinnable || INTO.contains(&hit.at) => {
                return DropTarget::Into(hit.key.to_string());
            }
            _ => {}
        }
    }
    if s.pinnable && u >= s.zone {
        return DropTarget::Pin(s.folders.iter().filter(|&&c| c < u).count());
    }
    match s.hit {
        Some(hit) if hit.takes && hit.kind == HitKind::App => DropTarget::Open(hit.key.to_string()),
        _ => DropTarget::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three other pinned apps centered at 100, 160 and 220, 60 apart.
    const PINNED: [f64; 3] = [100.0, 160.0, 220.0];

    fn scene() -> Scene<'static> {
        Scene {
            pinned: &PINNED,
            pinned_end: 250.0,
            step: 60.0,
            on_dock: true,
            on_trash: false,
        }
    }

    #[test]
    fn the_gap_follows_the_pointer_among_the_pinned() {
        let s = scene();
        let d = Dragged::Pinned(3);
        assert_eq!(target(d, &s, 50.0), Target::Gap(0));
        assert_eq!(target(d, &s, 130.0), Target::Gap(1));
        assert_eq!(target(d, &s, 190.0), Target::Gap(2));
        // Its own place: nothing moves.
        assert_eq!(target(d, &s, 240.0), Target::Home);
        assert_eq!(target(Dragged::Pinned(0), &s, 50.0), Target::Home);
        // Past the last one, the gap stays at their end.
        assert_eq!(target(Dragged::Pinned(0), &s, 900.0), Target::Gap(3));
    }

    #[test]
    fn running_apps_get_pinned_among_the_pinned() {
        let s = scene();
        let d = Dragged::Running;
        assert_eq!(target(d, &s, 50.0), Target::Gap(0));
        assert_eq!(target(d, &s, 230.0), Target::Gap(3));
        // Up to half an icon after the pinned apps.
        assert_eq!(target(d, &s, 270.0), Target::Gap(3));
        // Further, back among the running apps.
        assert_eq!(target(d, &s, 290.0), Target::Home);
        assert_eq!(drop(d, Target::Gap(3)), Drop::Pin(3));
        // Without pinned apps, the start of the row pins it.
        let none = Scene {
            pinned: &[],
            pinned_end: 40.0,
            ..s
        };
        assert_eq!(target(d, &none, 50.0), Target::Gap(0));
        assert_eq!(target(d, &none, 80.0), Target::Home);
    }

    #[test]
    fn the_trash_takes_pinned_items_only() {
        let s = Scene {
            on_trash: true,
            ..scene()
        };
        assert_eq!(target(Dragged::Pinned(1), &s, 900.0), Target::Trash);
        assert_eq!(drop(Dragged::Pinned(1), Target::Trash), Drop::Unpin);
        assert_eq!(target(Dragged::Running, &s, 900.0), Target::Home);
        assert_eq!(drop(Dragged::Running, Target::Trash), Drop::Nothing);
        assert!(trash_takes(Dragged::Pinned(0)) && !trash_takes(Dragged::Running));
    }

    #[test]
    fn off_the_dock_it_goes_home() {
        let s = Scene {
            on_dock: false,
            on_trash: true,
            ..scene()
        };
        for d in [Dragged::Pinned(1), Dragged::Running] {
            for u in [0.0, 130.0, 900.0] {
                assert_eq!(target(d, &s, u), Target::Home);
            }
            assert_eq!(drop(d, Target::Home), Drop::Nothing);
        }
    }

    fn app(a: &str) -> Pin {
        Pin::App(a.into())
    }

    #[test]
    fn drops() {
        // Moving the third pinned app first.
        let mut p = pins::parse("a\nb\nc\n");
        let k = match target(Dragged::Pinned(2), &scene(), 50.0) {
            Target::Gap(k) => k,
            t => panic!("{t:?}"),
        };
        assert!(apply(
            &mut p,
            app("c"),
            drop(Dragged::Pinned(2), Target::Gap(k))
        ));
        assert_eq!(pins::apps(&p), ["c", "a", "b"]);
        // Pinning a running app between the first two.
        assert!(apply(&mut p, app("d"), Drop::Pin(1)));
        assert_eq!(pins::apps(&p), ["c", "d", "a", "b"]);
        // Removing one.
        assert!(apply(&mut p, app("a"), Drop::Unpin));
        assert_eq!(pins::apps(&p), ["c", "d", "b"]);
    }

    #[test]
    fn widgets_go_across_the_separator() {
        // Past the separator's middle, among the folders.
        assert_eq!(zone(99.0, Some(100.0)), Zone::Apps);
        assert_eq!(zone(100.0, Some(100.0)), Zone::Folders);
        assert_eq!(zone(900.0, None), Zone::Apps);
        // From the other zone, the gap is open everywhere, even past
        // the last one; the Trash takes it.
        let s = scene();
        assert_eq!(target(Dragged::Across, &s, 50.0), Target::Gap(0));
        assert_eq!(target(Dragged::Across, &s, 900.0), Target::Gap(3));
        let trash = Scene {
            on_trash: true,
            ..s
        };
        assert_eq!(target(Dragged::Across, &trash, 900.0), Target::Trash);
        assert_eq!(drop(Dragged::Across, Target::Trash), Drop::Unpin);
        // Dropped among the folders, second there.
        let mut p = pins::parse("a\n@w\n~/x\n/y\n");
        let folders = Pin::Widget("w".into(), Zone::Folders);
        assert!(apply(&mut p, folders, Drop::Pin(1)));
        assert_eq!(
            pins::format(&p),
            pins::format(&pins::parse("a\n---\n~/x\n@w\n/y\n"))
        );
    }

    #[test]
    fn folders_move_among_the_folders() {
        let mut p = pins::parse("a\n~/x\n/y\nb\n");
        assert!(apply(&mut p, Pin::Path("/y".into()), Drop::Pin(0)));
        assert_eq!(pins::paths(&p), ["/y", "~/x"]);
        assert_eq!(pins::apps(&p), ["a", "b"]);
        assert!(apply(&mut p, Pin::Path("~/x".into()), Drop::Unpin));
        assert_eq!(pins::paths(&p), ["/y"]);
    }

    #[test]
    fn cancelling_leaves_the_pins() {
        let before = pins::parse("a\n~/x\nb\n");
        let mut p = before.clone();
        for d in [Dragged::Pinned(0), Dragged::Running] {
            let off = Scene {
                on_dock: false,
                ..scene()
            };
            let t = target(d, &off, 130.0);
            assert!(!apply(&mut p, app("a"), drop(d, t)));
        }
        // A running app on the Trash: refused.
        assert!(!apply(
            &mut p,
            app("z"),
            drop(Dragged::Running, Target::Trash)
        ));
        // A pinned app dropped where it was.
        let t = target(Dragged::Pinned(0), &scene(), 50.0);
        assert!(!apply(&mut p, app("a"), drop(Dragged::Pinned(0), t)));
        assert_eq!(p, before);
    }

    fn paths(ps: &[&str]) -> Vec<PathBuf> {
        ps.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn what_folders_take() {
        let dest = Path::new("/home/u/Downloads");
        assert!(folder_takes(dest, &paths(&["/tmp/a.txt", "/home/u/b"])));
        // Not itself, nor one of its parents.
        assert!(!folder_takes(dest, &paths(&["/home/u/Downloads"])));
        assert!(!folder_takes(dest, &paths(&["/tmp/a", "/home/u"])));
        // Not what is in it already; a mix is fine.
        assert!(!folder_takes(dest, &paths(&["/home/u/Downloads/a"])));
        assert!(folder_takes(
            dest,
            &paths(&["/home/u/Downloads/a", "/tmp/b"])
        ));
        // A sibling whose name starts the same isn't a parent.
        assert!(folder_takes(dest, &paths(&["/home/u/Down"])));
        assert!(!folder_takes(dest, &[]));
    }

    /// Apps centered at 30 and 90, the separator from 120, folders at
    /// 150 and 210, the Trash at 270; 60 apart.
    fn drop_scene<'a>(hit: Option<Hit<'a>>, pinnable: bool) -> DropScene<'a> {
        DropScene {
            on_dock: true,
            hit,
            folders: &[150.0, 210.0],
            zone: 120.0,
            pinnable,
        }
    }

    fn hit(key: &str, kind: HitKind, takes: bool, at: f64) -> Option<Hit<'_>> {
        Some(Hit {
            key,
            kind,
            takes,
            at,
        })
    }

    #[test]
    fn files_go_into_apps_folders_and_the_trash_only() {
        let into =
            |kind, takes, at, u| drop_target(&drop_scene(hit("k", kind, takes, at), false), u);
        assert_eq!(
            into(HitKind::App, true, 0.5, 30.0),
            DropTarget::Open("k".into())
        );
        assert_eq!(into(HitKind::App, false, 0.5, 30.0), DropTarget::Nothing);
        // Anywhere on a folder that takes them.
        for at in [0.05, 0.5, 0.95] {
            assert_eq!(
                into(HitKind::Folder, true, at, 150.0),
                DropTarget::Into("k".into())
            );
        }
        assert_eq!(
            into(HitKind::Folder, false, 0.5, 150.0),
            DropTarget::Nothing
        );
        assert_eq!(into(HitKind::Trash, true, 0.5, 270.0), DropTarget::Trash);
        // Between icons, nothing: files are never pinned.
        assert_eq!(
            into(HitKind::Separator, false, 0.5, 125.0),
            DropTarget::Nothing
        );
        assert_eq!(
            drop_target(&drop_scene(None, false), 180.0),
            DropTarget::Nothing
        );
    }

    #[test]
    fn folders_get_pinned_between_the_folders() {
        let at = |h, u| drop_target(&drop_scene(h, true), u);
        assert_eq!(
            at(hit("s", HitKind::Separator, false, 0.5), 125.0),
            DropTarget::Pin(0)
        );
        // Near a folder's edges, the gap; in its middle, into it.
        assert_eq!(
            at(hit("f", HitKind::Folder, true, 0.1), 125.0),
            DropTarget::Pin(0)
        );
        assert_eq!(
            at(hit("f", HitKind::Folder, true, 0.9), 175.0),
            DropTarget::Pin(1)
        );
        assert_eq!(
            at(hit("f", HitKind::Folder, true, 0.5), 150.0),
            DropTarget::Into("f".into())
        );
        // A folder that doesn't take them: the gap all across it.
        assert_eq!(
            at(hit("f", HitKind::Folder, false, 0.5), 151.0),
            DropTarget::Pin(1)
        );
        assert_eq!(at(None, 240.0), DropTarget::Pin(2));
        // The Trash still trashes them; apps still open them.
        assert_eq!(
            at(hit("t", HitKind::Trash, true, 0.5), 270.0),
            DropTarget::Trash
        );
        assert_eq!(
            at(hit("a", HitKind::App, true, 0.5), 30.0),
            DropTarget::Open("a".into())
        );
        // Off the dock, nothing.
        let off = DropScene {
            on_dock: false,
            ..drop_scene(None, true)
        };
        assert_eq!(drop_target(&off, 150.0), DropTarget::Nothing);
    }

    #[test]
    fn payloads() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("wl-dock-drag-{}", std::process::id()));
        std::fs::write(&file, "").unwrap();
        assert!(Payload::new(vec![dir.clone()]).folders);
        assert!(!Payload::new(vec![dir.clone(), file.clone()]).folders);
        assert!(!Payload::new(Vec::new()).folders);
        std::fs::remove_file(&file).unwrap();
    }
}
