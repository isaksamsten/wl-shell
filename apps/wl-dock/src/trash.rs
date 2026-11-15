//! The Trash on disk: where it is, and whether anything is in it. The dock
//! watches its `files` directory with inotify and switches the icon
//! between `user-trash` and `user-trash-full`.

use std::path::{Path, PathBuf};

/// The theme icons of an empty and a full Trash.
pub const EMPTY_ICON: &str = "user-trash";
pub const FULL_ICON: &str = "user-trash-full";
/// What `open-command` opens for the Trash.
pub const URI: &str = "trash:///";

/// The home Trash's `files` directory: `$XDG_DATA_HOME/Trash/files`,
/// falling back to `~/.local/share`.
pub fn files_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/share")))?;
    Some(base.join("Trash").join("files"))
}

/// Whether anything is in the Trash whose `files` directory is `dir`. A
/// missing directory is an empty Trash.
pub fn full(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some())
}

/// The icon for a Trash that is `full` or not.
pub fn icon(full: bool) -> &'static str {
    match full {
        true => FULL_ICON,
        false => EMPTY_ICON,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_full() {
        let dir = std::env::temp_dir().join(format!("wl-dock-trash-{}", std::process::id()));
        assert!(!full(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!full(&dir));
        std::fs::write(dir.join("x"), "").unwrap();
        assert!(full(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((icon(false), icon(true)), (EMPTY_ICON, FULL_ICON));
    }
}
