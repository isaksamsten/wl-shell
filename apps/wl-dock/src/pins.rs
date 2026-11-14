//! The pinned items, kept in a state file rather than the config, so the
//! dock never rewrites your TOML:
//!
//! ```text
//! $XDG_STATE_HOME/wl-dock/pinned      (default ~/.local/state/wl-dock/pinned)
//! ```
//!
//! One item per line, in dock order. Blank lines and `#` comments are
//! ignored. A line that starts with `/` or `~/` is a path (a folder or a
//! file; not shown yet), anything else an app ID. The file is written
//! atomically (a temp file, then a rename). The dock watches it and
//! reloads it when it changes, so `wl-dock pin` and editing it by hand
//! both work. A missing file means no pins.

use std::io;
use std::path::{Path, PathBuf};

/// One pinned item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pin {
    App(String),
    /// A folder or file, as written (`~/` kept).
    Path(String),
}

impl Pin {
    /// A line of the file, or an argument of `wl-dock pin`.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();
        match s.starts_with('/') || s.starts_with("~/") {
            true => Pin::Path(s.to_string()),
            false => Pin::App(s.to_string()),
        }
    }

    fn text(&self) -> &str {
        match self {
            Pin::App(s) | Pin::Path(s) => s,
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
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pin = Pin::parse(line);
        if !pins.contains(&pin) {
            pins.push(pin);
        }
    }
    pins
}

/// The file's text for `pins`.
pub fn format(pins: &[Pin]) -> String {
    let mut out = String::from("# wl-dock: pinned apps (and paths), one per line, in dock order\n");
    for pin in pins {
        out.push_str(pin.text());
        out.push('\n');
    }
    out
}

/// The app IDs among `pins`, in order.
pub fn apps(pins: &[Pin]) -> Vec<String> {
    (pins.iter())
        .filter_map(|p| match p {
            Pin::App(a) => Some(a.clone()),
            Pin::Path(_) => None,
        })
        .collect()
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
    if pins.contains(&pin) {
        return false;
    }
    pins.push(pin);
    true
}

/// Remove `pin`. Returns false if it wasn't pinned.
pub fn unpin(pins: &mut Vec<Pin>, pin: &Pin) -> bool {
    let before = pins.len();
    pins.retain(|p| p != pin);
    pins.len() != before
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

    /// Pin `item` (an app ID or a path). The message for the user, if any.
    pub fn add(item: &str) -> Result<Option<String>, String> {
        let item = Pin::parse(item);
        if item.text().is_empty() {
            return Err("nothing to pin".into());
        }
        let path = file()?;
        let mut pins = load(&path)?;
        if !pin(&mut pins, item.clone()) {
            return Ok(Some(format!("{} is pinned already", item.text())));
        }
        save(&path, &pins)?;
        Ok(match item {
            Pin::Path(_) => Some("the dock doesn't show folders and files yet".into()),
            Pin::App(_) => None,
        })
    }

    /// Unpin `item`. The message for the user, if any.
    pub fn remove(item: &str) -> Result<Option<String>, String> {
        let item = Pin::parse(item);
        let path = file()?;
        let mut pins = load(&path)?;
        if !unpin(&mut pins, &item) {
            return Ok(Some(format!("{} isn't pinned", item.text())));
        }
        save(&path, &pins)?;
        Ok(None)
    }

    /// The pins, one per line.
    pub fn list() -> Result<String, String> {
        let pins = load(&file()?)?;
        Ok(pins.iter().map(|p| format!("{}\n", p.text())).collect())
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
