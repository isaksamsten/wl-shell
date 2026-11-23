//! The pinned items, kept in a state file rather than the config, so the
//! dock never rewrites your TOML:
//!
//! ```text
//! $XDG_STATE_HOME/wl-dock/pinned      (default ~/.local/state/wl-dock/pinned)
//! ```
//!
//! One item per line, in dock order. Blank lines and `#` comments are
//! ignored. A line that starts with `/` or `~/` is a folder's path, one
//! that starts with `@` a standalone widget (`@clock`: the widget of the
//! config's `[widget.clock]`), anything else an app ID. A `---` line
//! stands for the dock's separator: a widget before it sits among the
//! apps, one after it among the folders (just before the Trash). Apps are
//! always before the separator and folders after it, wherever they are in
//! the file. A path that is a file is skipped (files are never pinned).
//! The file is written atomically (a temp file, then a rename), the items
//! before the separator first. The dock watches it and reloads it when it
//! changes, so `wl-dock pin` and editing it by hand both work. A missing
//! file means no pins.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// The line that stands for the separator.
const BREAK: &str = "---";

/// Where in the dock a pin is: among the apps, or after the separator
/// among the folders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Apps,
    Folders,
}

/// One pinned item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pin {
    App(String),
    /// A folder, as written (`~/` kept).
    Path(String),
    /// A standalone widget, by its name (written `@name`), and where it
    /// sits.
    Widget(String, Zone),
}

impl Pin {
    /// A line of the file, or an argument of `wl-dock pin` (a widget
    /// among the apps).
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        if let Some(name) = s.strip_prefix('@') {
            return Pin::Widget(name.trim().to_string(), Zone::Apps);
        }
        match s.starts_with('/') || s.starts_with("~/") {
            true => Pin::Path(s.to_string()).normalized(),
            false => Pin::App(s.to_string()),
        }
    }

    /// As written in the file.
    fn text(&self) -> Cow<'_, str> {
        match self {
            Pin::App(s) | Pin::Path(s) => Cow::Borrowed(s),
            Pin::Widget(name, _) => Cow::Owned(format!("@{name}")),
        }
    }

    /// Where it sits.
    pub fn zone(&self) -> Zone {
        match self {
            Pin::App(_) => Zone::Apps,
            Pin::Path(_) => Zone::Folders,
            Pin::Widget(_, zone) => *zone,
        }
    }

    /// The same item, wherever it sits.
    pub fn same(&self, other: &Pin) -> bool {
        match (self, other) {
            (Pin::Widget(a, _), Pin::Widget(b, _)) => a == b,
            _ => self == other,
        }
    }
}

/// The pins file: `$XDG_STATE_HOME/wl-dock/pinned`, falling back to
/// `~/.local/state`.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(base.join("wl-dock").join("pinned"))
}

/// The pins in a file's text, each once.
pub fn parse(text: &str) -> Vec<Pin> {
    let mut pins: Vec<Pin> = Vec::new();
    let mut zone = Zone::Apps;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == BREAK {
            zone = Zone::Folders;
            continue;
        }
        let pin = match Pin::parse(line) {
            Pin::Widget(name, _) => Pin::Widget(name, zone),
            pin => pin,
        };
        if !pins.iter().any(|p| p.same(&pin)) {
            pins.push(pin);
        }
    }
    pins
}

/// The file's text for `pins`: those among the apps, then the separator
/// and those after it, each in their order.
pub fn format(pins: &[Pin]) -> String {
    let mut out = String::from(
        "# wl-dock: pinned apps and @widgets, then --- and folders and @widgets, in dock order\n",
    );
    let zone = |z: Zone| pins.iter().filter(move |p| p.zone() == z);
    for pin in zone(Zone::Apps) {
        out.push_str(&pin.text());
        out.push('\n');
    }
    if zone(Zone::Folders).next().is_some() {
        out.push_str(BREAK);
        out.push('\n');
    }
    for pin in zone(Zone::Folders) {
        out.push_str(&pin.text());
        out.push('\n');
    }
    out
}

/// The app IDs among `pins`, in order.
pub fn apps(pins: &[Pin]) -> Vec<String> {
    (pins.iter())
        .filter_map(|p| match p {
            Pin::App(a) => Some(a.clone()),
            Pin::Path(_) | Pin::Widget(..) => None,
        })
        .collect()
}

/// The paths among `pins`, in order.
#[cfg(test)]
pub fn paths(pins: &[Pin]) -> Vec<String> {
    (pins.iter())
        .filter_map(|p| match p {
            Pin::Path(p) => Some(p.clone()),
            Pin::App(_) | Pin::Widget(..) => None,
        })
        .collect()
}

/// The widgets' names among `pins`, in order.
pub fn widgets(pins: &[Pin]) -> Vec<String> {
    (pins.iter())
        .filter_map(|p| match p {
            Pin::Widget(name, _) => Some(name.clone()),
            Pin::App(_) | Pin::Path(_) => None,
        })
        .collect()
}

/// A pinned path as a path: `~/` expanded.
pub fn expand(path: &str) -> PathBuf {
    PathBuf::from(wl_shell_render::icons::expand_home(path))
}

/// The pin of the absolute `path`: under `~/` if it is in the home
/// directory, so the pins file reads well.
pub fn for_path(path: &Path) -> Pin {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let text = match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    };
    Pin::Path(text).normalized()
}

impl Pin {
    /// The same pin, a path without trailing slashes (but `/`).
    fn normalized(self) -> Self {
        match self {
            Pin::Path(p) if p.len() > 1 && p.ends_with('/') => {
                let trimmed = match p.trim_end_matches('/') {
                    "" => "/",
                    "~" => "~/",
                    t => t,
                };
                Pin::Path(trimmed.to_string())
            }
            pin => pin,
        }
    }

    /// The same kind of pin: in the same zone.
    fn same_kind(&self, other: &Pin) -> bool {
        self.zone() == other.zone()
    }
}

/// Read the pins at `path`; none if it doesn't exist.
pub fn read(path: &Path) -> io::Result<Vec<Pin>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Write `pins` to `path` atomically, creating its directory.
pub fn write(path: &Path, pins: &[Pin]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, format(pins))
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

/// Add `pin` at the end. Returns false if it was pinned already.
pub fn pin(pins: &mut Vec<Pin>, pin: Pin) -> bool {
    if pins.iter().any(|p| p.same(&pin)) {
        return false;
    }
    pins.push(pin);
    true
}

/// Remove `pin` (a widget wherever it sits). Returns false if it wasn't
/// pinned.
pub fn unpin(pins: &mut Vec<Pin>, pin: &Pin) -> bool {
    let before = pins.len();
    pins.retain(|p| !p.same(pin));
    pins.len() != before
}

/// Pin `pin` as the `k`th pin of its zone (from 0), moving it there if
/// it is pinned already (a widget may move to the other zone). The other
/// zone keeps its places. Returns whether the pins changed.
pub fn place(pins: &mut Vec<Pin>, pin: Pin, k: usize) -> bool {
    let before = pins.clone();
    pins.retain(|p| !p.same(&pin));
    let same: Vec<usize> = (pins.iter().enumerate())
        .filter(|(_, p)| p.same_kind(&pin))
        .map(|(i, _)| i)
        .collect();
    let at = match same.get(k) {
        Some(&i) => i,
        // After the last of its kind, or at the end.
        None => same.last().map_or(pins.len(), |&i| i + 1),
    };
    pins.insert(at, pin);
    *pins != before
}

/// Pin `app_id` as the `k`th pinned app (see `place`).
#[cfg(test)]
pub fn place_app(pins: &mut Vec<Pin>, app_id: &str, k: usize) -> bool {
    place(pins, Pin::App(app_id.to_string()), k)
}

/// What the `pin`, `unpin` and `pins` commands do, on the default file.
pub mod cli {
    use super::*;

    fn file() -> Result<PathBuf, String> {
        path().ok_or_else(|| "neither XDG_STATE_HOME nor HOME is set".into())
    }

    fn load(path: &Path) -> Result<Vec<Pin>, String> {
        read(path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn save(path: &Path, pins: &[Pin]) -> Result<(), String> {
        write(path, pins).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// An argument: an app ID, `@` and a widget's name, or a path
    /// (relative ones made absolute).
    fn item(arg: &str) -> Pin {
        let arg = arg.trim();
        let relative = arg == "."
            || arg == ".."
            || (arg.contains('/')
                && !arg.starts_with('/')
                && !arg.starts_with("~/")
                && !arg.starts_with('@'));
        let absolute = || {
            std::fs::canonicalize(arg)
                .or_else(|_| std::path::absolute(arg))
                .ok()
        };
        match relative.then(absolute).flatten() {
            Some(abs) => for_path(&abs),
            None => Pin::parse(arg),
        }
    }

    /// Pin `item` (an app ID, a widget or a folder). The message for the
    /// user, if any. Files are refused.
    pub fn add(item: &str) -> Result<Option<String>, String> {
        let item = self::item(item);
        if item.text().is_empty() || matches!(&item, Pin::Widget(n, _) if n.is_empty()) {
            return Err("nothing to pin".into());
        }
        if let Pin::Widget(name, _) = &item
            && !crate::config::plugin_name_ok(name)
        {
            return Err(format!(
                "bad widget name {name:?} (letters, digits, - and _)"
            ));
        }
        let mut note = None;
        if let Pin::Path(p) = &item {
            match std::fs::metadata(expand(p)) {
                Ok(m) if !m.is_dir() => {
                    return Err(format!("{p} is a file: only folders can be pinned"));
                }
                Ok(_) => {}
                Err(_) => note = Some(format!("{p} doesn't exist: it shows faded")),
            }
        }
        let path = file()?;
        let mut pins = load(&path)?;
        if !pin(&mut pins, item.clone()) {
            return Ok(Some(format!("{} is pinned already", item.text())));
        }
        save(&path, &pins)?;
        Ok(note)
    }

    /// Unpin `item`. The message for the user, if any.
    pub fn remove(item: &str) -> Result<Option<String>, String> {
        let item = self::item(item);
        let path = file()?;
        let mut pins = load(&path)?;
        if !unpin(&mut pins, &item) {
            return Ok(Some(format!("{} isn't pinned", item.text())));
        }
        save(&path, &pins)?;
        Ok(None)
    }

    /// The pins, one per line, in dock order (as the file has them).
    pub fn list() -> Result<String, String> {
        let pins = load(&file()?)?;
        Ok((format(&pins).lines().skip(1))
            .map(|l| format!("{l}\n"))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing() {
        let text = "# comment\n\nfirefox\n  org.gnome.Nautilus  \n~/Downloads\n/srv/x\nfirefox\n";
        assert_eq!(
            parse(text),
            [
                Pin::App("firefox".into()),
                Pin::App("org.gnome.Nautilus".into()),
                Pin::Path("~/Downloads".into()),
                Pin::Path("/srv/x".into()),
            ]
        );
        assert_eq!(apps(&parse(text)), ["firefox", "org.gnome.Nautilus"]);
        // `~` alone or `~user` aren't paths.
        assert_eq!(Pin::parse("~x"), Pin::App("~x".into()));
    }

    #[test]
    fn round_trip() {
        let pins = vec![Pin::App("a".into()), Pin::Path("~/b".into())];
        assert_eq!(parse(&format(&pins)), pins);
    }

    #[test]
    fn pin_and_unpin() {
        let mut pins = vec![Pin::App("a".into())];
        assert!(pin(&mut pins, Pin::App("b".into())));
        assert!(!pin(&mut pins, Pin::App("a".into())));
        assert_eq!(apps(&pins), ["a", "b"]);
        assert!(unpin(&mut pins, &Pin::App("a".into())));
        assert!(!unpin(&mut pins, &Pin::App("a".into())));
        assert_eq!(apps(&pins), ["b"]);
    }

    #[test]
    fn placing_apps() {
        let mut pins = parse("a\n~/x\nb\nc\n/y\n");
        // Moving one: the others close up, paths stay where they were.
        assert!(place_app(&mut pins, "c", 0));
        assert_eq!(format(&pins), format(&parse("c\na\n~/x\nb\n/y\n")));
        assert!(place_app(&mut pins, "c", 2));
        assert_eq!(format(&pins), format(&parse("a\n~/x\nb\nc\n/y\n")));
        // Past the end: after the last app.
        assert!(place_app(&mut pins, "a", 9));
        assert_eq!(format(&pins), format(&parse("~/x\nb\nc\na\n/y\n")));
        // Where it is already: nothing changes.
        assert!(!place_app(&mut pins, "a", 2));
        // A new one is pinned there.
        assert!(place_app(&mut pins, "d", 1));
        assert_eq!(apps(&pins), ["b", "d", "c", "a"]);
        let mut none = Vec::new();
        assert!(place_app(&mut none, "a", 3));
        assert_eq!(none, [Pin::App("a".into())]);
    }

    #[test]
    fn widgets_sit_anywhere() {
        let mut pins = parse("a\n@clock\n~/x\nb\n @ weather \n---\n@moon\n/y\n");
        let w = |n: &str| Pin::Widget(n.into(), Zone::Apps);
        let t = |n: &str| Pin::Widget(n.into(), Zone::Folders);
        assert_eq!(pins[1], w("clock"));
        assert_eq!(pins[4], w("weather"));
        assert_eq!(pins[5], t("moon"));
        assert_eq!(widgets(&pins), ["clock", "weather", "moon"]);
        assert_eq!(apps(&pins), ["a", "b"]);
        // Paths are after the separator wherever they are written.
        assert_eq!(paths(&pins), ["~/x", "/y"]);
        assert_eq!(pins[2].zone(), Zone::Folders);
        // Written back in dock order, which reads back the same.
        assert_eq!(
            format(&pins).lines().skip(1).collect::<Vec<_>>(),
            ["a", "@clock", "b", "@weather", "---", "~/x", "@moon", "/y"]
        );
        let again = parse(&format(&pins));
        assert_eq!(format(&again), format(&pins));
        assert_eq!(again[5].zone(), Zone::Folders);
        // No separator line without anything after it.
        assert!(!format(&parse("a\n@clock\n")).lines().any(|l| l == BREAK));
        // Placed among the apps and widgets: the paths keep their places.
        assert!(place(&mut pins, w("clock"), 0));
        assert_eq!(apps_zone(&pins), ["@clock", "a", "b", "@weather"]);
        // Moved after the separator: the second there.
        assert!(place(&mut pins, t("weather"), 1));
        assert_eq!(apps_zone(&pins), ["@clock", "a", "b"]);
        assert_eq!(folders_zone(&pins), ["~/x", "@weather", "@moon", "/y"]);
        // And back, the last among the apps.
        assert!(place(&mut pins, w("moon"), 9));
        assert_eq!(apps_zone(&pins), ["@clock", "a", "b", "@moon"]);
        // Once each, wherever: pinning again changes nothing.
        assert!(!pin(&mut pins, w("weather")));
        // Removed from the dock, wherever it sits.
        assert!(unpin(&mut pins, &w("weather")));
        assert!(unpin(&mut pins, &t("clock")));
        assert_eq!(widgets(&pins), ["moon"]);
        // An app ID can't start with `@`; a path can't either.
        assert_eq!(Pin::parse("@x/y"), w("x/y"));
    }

    fn zone_texts(pins: &[Pin], zone: Zone) -> Vec<String> {
        (pins.iter())
            .filter(|p| p.zone() == zone)
            .map(|p| p.text().into_owned())
            .collect()
    }

    fn apps_zone(pins: &[Pin]) -> Vec<String> {
        zone_texts(pins, Zone::Apps)
    }

    fn folders_zone(pins: &[Pin]) -> Vec<String> {
        zone_texts(pins, Zone::Folders)
    }

    #[test]
    fn placing_folders() {
        let mut pins = parse("a\n~/x\nb\n/y\n");
        let path = |p: &str| Pin::Path(p.into());
        // Among the paths only; the apps keep their places.
        assert!(place(&mut pins, path("/y"), 0));
        assert_eq!(format(&pins), format(&parse("a\n/y\n~/x\nb\n")));
        // A new one, after the last path.
        assert!(place(&mut pins, path("/z"), 5));
        assert_eq!(paths(&pins), ["/y", "~/x", "/z"]);
        assert!(place(&mut pins, path("/w"), 1));
        assert_eq!(paths(&pins), ["/y", "/w", "~/x", "/z"]);
        assert_eq!(apps(&pins), ["a", "b"]);
    }

    #[test]
    fn paths_as_written() {
        // Trailing slashes go, but `/` stays.
        assert_eq!(Pin::parse("~/Downloads/"), Pin::Path("~/Downloads".into()));
        assert_eq!(Pin::parse("/"), Pin::Path("/".into()));
        assert_eq!(for_path(Path::new("/srv/x/")), Pin::Path("/srv/x".into()));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
            && home != Path::new("/")
        {
            assert_eq!(for_path(&home.join("Music")), Pin::Path("~/Music".into()));
            assert_eq!(expand("~/Music"), home.join("Music"));
        }
    }

    #[test]
    fn files() {
        let dir = std::env::temp_dir().join(format!("wl-dock-pins-{}", std::process::id()));
        let file = dir.join("state/pinned");
        assert_eq!(read(&file).unwrap(), []);
        let pins = vec![Pin::App("a".into())];
        write(&file, &pins).unwrap();
        assert_eq!(read(&file).unwrap(), pins);
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(dir.join("state")).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
