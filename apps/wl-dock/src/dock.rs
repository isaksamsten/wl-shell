//! What a dock shows, and what clicking an item does. Pure, unit tested;
//! the Wayland side feeds it the window list, the pins, and where the
//! output stream placed each window.
//!
//! - Pinned apps come first, in their pinned order, on every dock, whether
//!   or not they're running.
//! - Then the running apps that aren't pinned, in the order their first
//!   window appeared (`Order`). An app keeps its place while it has
//!   windows, so icons don't move around when windows close.
//! - Every window with the same `app_id` (as is) belongs to one item.
//! - With `windows = "output"`, a dock shows the windows the output stream
//!   placed on its output (`Placement`). A new window it hasn't reported
//!   yet is held back for the grace period, then shows on every dock until
//!   it's reported. Without a running stream, every dock shows every
//!   window. Unpinned apps only show where they have windows.
//! - Then the separator, the pinned folders (in their pinned order, on
//!   every dock, faded if they don't exist), and the Trash. The separator
//!   shows when anything follows it.

use std::collections::{HashMap, HashSet};

use wl_shell_core::mru::Mru;
use wl_shell_core::toplevel::Toplevel;

/// One window of an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// The ext foreign toplevel identifier.
    pub id: String,
    pub title: String,
}

/// What an item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    App,
    /// The line between the apps and the folders and Trash.
    Separator,
    /// A pinned folder; `exists` if it is there (as a directory).
    Folder {
        exists: bool,
    },
    /// The Trash; `full` if anything is in it.
    Trash {
        full: bool,
    },
}

/// The separator's key (`Item::app_id`): no app ID can have a NUL.
pub const SEPARATOR: &str = "\0separator";
/// The Trash's key.
pub const TRASH: &str = "\0trash";
/// What folders' keys start with; their pin follows.
const FOLDER: &str = "\0folder\0";

/// The key of the folder pinned as `pin`.
pub fn folder_key(pin: &str) -> String {
    format!("{FOLDER}{pin}")
}

/// The pin of the folder whose key is `key`, if it is one.
pub fn folder_pin(key: &str) -> Option<&str> {
    key.strip_prefix(FOLDER)
}

/// One icon in the dock: every window with the same app ID shares it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The app ID; for the other items, their keys.
    pub app_id: String,
    pub kind: Kind,
    pub pinned: bool,
    /// The windows shown on this dock, in announcement order. Empty for a
    /// pinned app without windows here.
    pub windows: Vec<Window>,
    /// The app's windows on other outputs.
    pub elsewhere: Vec<Window>,
    /// The focused window is one of `windows`.
    pub active: bool,
}

/// What clicking an item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Click {
    /// Activate the window with this ID.
    Activate(String),
    /// Launch the app.
    Launch,
    OpenTrash,
    /// Open the folder's popup.
    OpenFolder,
    Nothing,
}

impl Item {
    /// An item that isn't an app or a folder.
    fn other(kind: Kind) -> Self {
        let key = match kind {
            Kind::Separator => SEPARATOR,
            Kind::Trash { .. } | Kind::App | Kind::Folder { .. } => TRASH,
        };
        Self::keyed(key.to_string(), kind)
    }

    fn keyed(key: String, kind: Kind) -> Self {
        Self {
            app_id: key,
            kind,
            pinned: false,
            windows: Vec::new(),
            elsewhere: Vec::new(),
            active: false,
        }
    }

    pub fn is_app(&self) -> bool {
        self.kind == Kind::App
    }

    pub fn is_trash(&self) -> bool {
        matches!(self.kind, Kind::Trash { .. })
    }

    pub fn is_folder(&self) -> bool {
        matches!(self.kind, Kind::Folder { .. })
    }

    /// The folder's pin, if it is a folder.
    pub fn folder(&self) -> Option<&str> {
        folder_pin(&self.app_id).filter(|_| self.is_folder())
    }

    /// Whether it can be dragged to another place (or the Trash): apps
    /// that can be pinned, and folders.
    pub fn draggable(&self) -> bool {
        (self.is_app() && !self.app_id.is_empty()) || self.is_folder()
    }

    /// What a click does: activate the most recently focused of the
    /// windows here (else the first), else of the windows elsewhere, else
    /// launch the app.
    pub fn click(&self, mru: &Mru) -> Click {
        match self.kind {
            Kind::App => {}
            Kind::Separator => return Click::Nothing,
            Kind::Trash { .. } => return Click::OpenTrash,
            Kind::Folder { exists } => {
                return if exists {
                    Click::OpenFolder
                } else {
                    Click::Nothing
                };
            }
        }
        for windows in [&self.windows, &self.elsewhere] {
            if let Some(first) = windows.first() {
                let id = mru
                    .most_recent(windows.iter().map(|w| w.id.as_str()))
                    .unwrap_or(&first.id);
                return Click::Activate(id.to_string());
            }
        }
        Click::Launch
    }

    #[cfg(test)]
    /// Whether the app has a window anywhere.
    pub fn running(&self) -> bool {
        !self.windows.is_empty() || !self.elsewhere.is_empty()
    }
}

/// The running apps in the order their first window appeared. Kept across
/// window list changes, so an app keeps its place while it has windows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Order(Vec<String>);

impl Order {
    /// Drop the apps without windows and append new ones in the order of
    /// their first window.
    pub fn update(&mut self, toplevels: &[Toplevel]) {
        self.0.retain(|a| toplevels.iter().any(|t| t.app_id == *a));
        for t in toplevels {
            if !self.0.contains(&t.app_id) {
                self.0.push(t.app_id.clone());
            }
        }
    }

    pub fn apps(&self) -> &[String] {
        &self.0
    }
}

/// Where a window is, for one dock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Here,
    /// On another output.
    Elsewhere,
    /// New and not placed yet: on no dock for now.
    Held,
}

/// The windows' outputs, as the output stream reported them.
#[derive(Clone, Debug, Default)]
pub struct Placement {
    /// Window ID to output name.
    outputs: HashMap<String, String>,
    /// New windows not reported yet, within their grace period.
    held: HashSet<String>,
}

/// Windows remembered at most: the stream may report IDs the toplevel list
/// never announces, so not every one gets forgotten on close.
const MAX_PLACED: usize = 4096;

/// A line of the output stream: `<output-name> <id>`. Everything after the
/// first space is the ID.
pub fn parse_output_line(line: &str) -> Option<(&str, &str)> {
    let (output, id) = line.trim_start().split_once(' ')?;
    let id = id.trim_end_matches(['\r', '\n']);
    (!output.is_empty() && !id.is_empty()).then_some((output, id))
}

impl Placement {
    /// The stream placed window `id` on `output`. Returns whether that
    /// changed anything.
    pub fn report(&mut self, output: &str, id: &str) -> bool {
        let released = self.held.remove(id);
        if self.outputs.get(id).is_some_and(|o| o == output) {
            return released;
        }
        if self.outputs.len() >= MAX_PLACED && !self.outputs.contains_key(id) {
            self.outputs.clear();
        }
        self.outputs.insert(id.to_string(), output.to_string());
        true
    }

    pub fn placed(&self, id: &str) -> bool {
        self.outputs.contains_key(id)
    }

    /// Hold the new window `id` back until it's reported or released.
    pub fn hold(&mut self, id: &str) {
        if !self.placed(id) {
            self.held.insert(id.to_string());
        }
    }

    /// The grace period of `id` is over. Returns whether it was held.
    pub fn release(&mut self, id: &str) -> bool {
        self.held.remove(id)
    }

    /// Window `id` closed.
    pub fn forget(&mut self, id: &str) {
        self.outputs.remove(id);
        self.held.remove(id);
    }

    /// Where window `id` is for the dock on `output`. With `by_output`
    /// false (`windows = "all"`, or no running stream), everywhere.
    pub fn place(&self, id: &str, output: &str, by_output: bool) -> Place {
        if !by_output {
            return Place::Here;
        }
        match self.outputs.get(id) {
            Some(o) if o == output => Place::Here,
            Some(_) => Place::Elsewhere,
            None if self.held.contains(id) => Place::Held,
            None => Place::Here,
        }
    }
}

/// A dock's items. `pins` are app IDs in pinned order, `order` the running
/// apps' order, `focused` the focused window's ID (None: don't mark the
/// active app), and `place` says where each window is for this dock.
pub fn items(
    toplevels: &[Toplevel],
    pins: &[String],
    order: &Order,
    focused: Option<&str>,
    place: impl Fn(&str) -> Place,
) -> Vec<Item> {
    let item = |app_id: &str, pinned: bool| {
        let mut item = Item {
            app_id: app_id.to_string(),
            kind: Kind::App,
            pinned,
            windows: Vec::new(),
            elsewhere: Vec::new(),
            active: false,
        };
        for t in toplevels.iter().filter(|t| t.app_id == app_id) {
            let window = Window {
                id: t.identifier.clone(),
                title: t.title.clone(),
            };
            match place(&t.identifier) {
                Place::Here => {
                    item.active |= focused == Some(t.identifier.as_str());
                    item.windows.push(window);
                }
                Place::Elsewhere => item.elsewhere.push(window),
                Place::Held => {}
            }
        }
        item
    };
    let mut out: Vec<Item> = Vec::new();
    for pin in pins {
        if !out.iter().any(|i| i.app_id == *pin) {
            out.push(item(pin, true));
        }
    }
    for app_id in order.apps() {
        if out.iter().any(|i| i.app_id == *app_id) {
            continue;
        }
        let item = item(app_id, false);
        if !item.windows.is_empty() {
            out.push(item);
        }
    }
    out
}

/// After `items`: the separator if anything follows it, the pinned
/// `folders` (each a pin and whether it exists), and the Trash if
/// `trash` says whether it is full.
pub fn add_tail(items: &mut Vec<Item>, folders: &[(String, bool)], trash: Option<bool>) {
    if folders.is_empty() && trash.is_none() {
        return;
    }
    items.push(Item::other(Kind::Separator));
    for (pin, exists) in folders {
        let key = folder_key(pin);
        if !items.iter().any(|i| i.app_id == key) {
            let mut item = Item::keyed(key, Kind::Folder { exists: *exists });
            item.pinned = true;
            items.push(item);
        }
    }
    if let Some(full) = trash {
        items.push(Item::other(Kind::Trash { full }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toplevel(id: &str, app_id: &str) -> Toplevel {
        Toplevel {
            identifier: id.into(),
            app_id: app_id.into(),
            title: format!("{app_id} {id}"),
        }
    }

    fn summary(items: &[Item]) -> Vec<(&str, Vec<&str>)> {
        (items.iter())
            .map(|i| {
                let ids = i.windows.iter().map(|w| w.id.as_str()).collect();
                (i.app_id.as_str(), ids)
            })
            .collect()
    }

    fn all(toplevels: &[Toplevel], pins: &[&str]) -> Vec<Item> {
        let mut order = Order::default();
        order.update(toplevels);
        let pins: Vec<String> = pins.iter().map(|p| p.to_string()).collect();
        items(toplevels, &pins, &order, None, |_| Place::Here)
    }

    #[test]
    fn empty() {
        assert!(all(&[], &[]).is_empty());
    }

    #[test]
    fn grouped_in_order_of_first_window() {
        let tls = [
            toplevel("1", "term"),
            toplevel("2", "browser"),
            toplevel("3", "term"),
            toplevel("4", "chat"),
        ];
        assert_eq!(
            summary(&all(&tls, &[])),
            [
                ("term", vec!["1", "3"]),
                ("browser", vec!["2"]),
                ("chat", vec!["4"]),
            ]
        );
    }

    #[test]
    fn empty_app_id_is_one_app() {
        let got = all(&[toplevel("1", ""), toplevel("2", "")], &[]);
        assert_eq!(summary(&got), [("", vec!["1", "2"])]);
    }

    #[test]
    fn apps_keep_their_place_while_they_have_windows() {
        let mut order = Order::default();
        order.update(&[toplevel("1", "a"), toplevel("2", "b"), toplevel("3", "a")]);
        assert_eq!(order.apps(), ["a", "b"]);
        // a's first window closes: a now has a later window than b's,
        // but stays first.
        order.update(&[toplevel("2", "b"), toplevel("3", "a")]);
        assert_eq!(order.apps(), ["a", "b"]);
        // Once gone, an app comes back at the end.
        order.update(&[toplevel("2", "b")]);
        order.update(&[toplevel("2", "b"), toplevel("4", "a")]);
        assert_eq!(order.apps(), ["b", "a"]);
    }

    #[test]
    fn pinned_first_running_or_not() {
        let tls = [toplevel("1", "term"), toplevel("2", "browser")];
        let got = all(&tls, &["files", "browser", "files"]);
        assert_eq!(
            summary(&got),
            [
                ("files", vec![]),
                ("browser", vec!["2"]),
                ("term", vec!["1"]),
            ]
        );
        assert_eq!(
            got.iter().map(|i| i.pinned).collect::<Vec<_>>(),
            [true, true, false]
        );
        assert!(!got[0].running() && got[1].running());
    }

    #[test]
    fn active_app() {
        let tls = [toplevel("1", "term"), toplevel("2", "browser")];
        let mut order = Order::default();
        order.update(&tls);
        let got = items(&tls, &[], &order, Some("2"), |_| Place::Here);
        assert_eq!(
            got.iter().map(|i| i.active).collect::<Vec<_>>(),
            [false, true]
        );
        let got = items(&tls, &[], &order, None, |_| Place::Here);
        assert!(got.iter().all(|i| !i.active));
    }

    #[test]
    fn clicks() {
        let tls = [
            toplevel("1", "term"),
            toplevel("2", "term"),
            toplevel("3", "browser"),
        ];
        let got = all(&tls, &["files"]);
        let mut mru = Mru::default();
        // Not running: launch. No focus history: the first window.
        assert_eq!(got[0].click(&mru), Click::Launch);
        assert_eq!(got[1].click(&mru), Click::Activate("1".into()));
        mru.focus("2");
        mru.focus("3");
        assert_eq!(got[1].click(&mru), Click::Activate("2".into()));
    }

    #[test]
    fn output_lines() {
        assert_eq!(parse_output_line("DP-1 42"), Some(("DP-1", "42")));
        assert_eq!(parse_output_line("DP-1 a b \r"), Some(("DP-1", "a b ")));
        assert_eq!(parse_output_line("  eDP-1 7"), Some(("eDP-1", "7")));
        assert_eq!(parse_output_line("DP-1"), None);
        assert_eq!(parse_output_line("DP-1 "), None);
        assert_eq!(parse_output_line(""), None);
    }

    #[test]
    fn placement() {
        let mut p = Placement::default();
        // Not by output: everywhere, held or not.
        p.hold("1");
        assert_eq!(p.place("1", "A", false), Place::Here);
        // Held until reported or released.
        assert_eq!(p.place("1", "A", true), Place::Held);
        assert!(p.report("B", "1"));
        assert_eq!(p.place("1", "A", true), Place::Elsewhere);
        assert_eq!(p.place("1", "B", true), Place::Here);
        assert!(!p.report("B", "1"));
        assert!(p.report("A", "1"));
        assert_eq!(p.place("1", "A", true), Place::Here);
        // Released unreported: everywhere.
        p.hold("2");
        assert!(p.release("2"));
        assert!(!p.release("2"));
        assert_eq!(p.place("2", "A", true), Place::Here);
        assert_eq!(p.place("2", "B", true), Place::Here);
        // Placed windows aren't held.
        p.hold("1");
        assert_eq!(p.place("1", "A", true), Place::Here);
        p.forget("1");
        assert_eq!(p.place("1", "B", true), Place::Here);
    }

    /// Windows 1 (term) and 3 (browser) on A, 2 (term) on B, 4 (chat) on
    /// B, 5 (chat) held.
    fn two_outputs() -> (Vec<Toplevel>, Order, Placement) {
        let tls = vec![
            toplevel("1", "term"),
            toplevel("2", "term"),
            toplevel("3", "browser"),
            toplevel("4", "chat"),
            toplevel("5", "chat"),
        ];
        let mut order = Order::default();
        order.update(&tls);
        let mut p = Placement::default();
        p.report("A", "1");
        p.report("B", "2");
        p.report("A", "3");
        p.report("B", "4");
        p.hold("5");
        (tls, order, p)
    }

    #[test]
    fn docks_show_their_outputs_windows() {
        let (tls, order, p) = two_outputs();
        let on = |output: &str| items(&tls, &[], &order, None, |id| p.place(id, output, true));
        let a = on("A");
        assert_eq!(summary(&a), [("term", vec!["1"]), ("browser", vec!["3"])]);
        assert_eq!(a[0].elsewhere.len(), 1);
        // The held window is on no dock yet.
        assert_eq!(
            summary(&on("B")),
            [("term", vec!["2"]), ("chat", vec!["4"])]
        );
    }

    #[test]
    fn pinned_apps_show_on_every_dock() {
        let (tls, order, p) = two_outputs();
        let pins = ["browser".to_string(), "files".to_string()];
        let b = items(&tls, &pins, &order, Some("3"), |id| p.place(id, "B", true));
        assert_eq!(
            summary(&b),
            [
                ("browser", vec![]),
                ("files", vec![]),
                ("term", vec!["2"]),
                ("chat", vec!["4"]),
            ]
        );
        // The browser runs on A: hollow here, and clicking it activates
        // its window there. The focus is there too, so it isn't active.
        let browser = &b[0];
        assert!(browser.running() && browser.windows.is_empty() && !browser.active);
        assert_eq!(browser.click(&Mru::default()), Click::Activate("3".into()));
        assert!(!b[1].running());
    }

    #[test]
    fn without_a_stream_every_dock_shows_every_window() {
        let (tls, order, p) = two_outputs();
        let got = items(&tls, &[], &order, None, |id| p.place(id, "A", false));
        assert_eq!(
            summary(&got),
            [
                ("term", vec!["1", "2"]),
                ("browser", vec!["3"]),
                ("chat", vec!["4", "5"]),
            ]
        );
    }

    #[test]
    fn the_trash_comes_last() {
        let mut got = all(&[toplevel("1", "term")], &["files"]);
        add_tail(&mut got, &[], Some(true));
        let kinds: Vec<Kind> = got.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::App,
                Kind::App,
                Kind::Separator,
                Kind::Trash { full: true }
            ]
        );
        // Neither is an app: no windows, no pinning, no dragging.
        assert!(!got[2].draggable() && !got[3].draggable());
        assert!(got[3].is_trash() && !got[3].running());
        let mru = Mru::default();
        assert_eq!(got[2].click(&mru), Click::Nothing);
        assert_eq!(got[3].click(&mru), Click::OpenTrash);
        // Their keys are no app's.
        assert!(
            got.iter()
                .filter(|i| i.is_app())
                .all(|i| !i.app_id.contains('\0'))
        );
    }

    #[test]
    fn folders_after_the_separator() {
        let mut got = all(&[toplevel("1", "term")], &[]);
        let folders = [
            ("~/Downloads".to_string(), true),
            ("/gone".to_string(), false),
            ("~/Downloads".to_string(), true),
        ];
        add_tail(&mut got, &folders, None);
        let kinds: Vec<Kind> = got.iter().map(|i| i.kind).collect();
        assert_eq!(
            kinds,
            [
                Kind::App,
                Kind::Separator,
                Kind::Folder { exists: true },
                Kind::Folder { exists: false },
            ]
        );
        assert_eq!(got[2].folder(), Some("~/Downloads"));
        assert_eq!(folder_pin(&got[3].app_id), Some("/gone"));
        assert!(got[2].draggable() && got[2].pinned && !got[0].is_folder());
        let mru = Mru::default();
        assert_eq!(got[2].click(&mru), Click::OpenFolder);
        // A missing folder does nothing.
        assert_eq!(got[3].click(&mru), Click::Nothing);
        // Nothing after the apps: no separator.
        let mut alone = all(&[toplevel("1", "term")], &[]);
        add_tail(&mut alone, &[], None);
        assert_eq!(alone.len(), 1);
    }
}
