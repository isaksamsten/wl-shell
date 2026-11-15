//! Pinned folders: their names and theme icons. Pure but for reading
//! `user-dirs.dirs`; unit tested.
//!
//! A folder's icon is the one its `[folder.<path>]` table names, else the
//! theme's icon for an XDG user directory (`folder-download` for the
//! download directory, from `$XDG_CONFIG_HOME/user-dirs.dirs`, and
//! `user-home` for the home directory), else `folder`.

use std::path::{Path, PathBuf};

/// The icon of a folder that is no user directory.
pub const ICON: &str = "folder";

/// The XDG user directories and their icons.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserDirs {
    home: Option<PathBuf>,
    dirs: Vec<(PathBuf, &'static str)>,
}

/// The icon of each `XDG_<NAME>_DIR`.
const ICONS: [(&str, &str); 8] = [
    ("DESKTOP", "user-desktop"),
    ("DOWNLOAD", "folder-download"),
    ("TEMPLATES", "folder-templates"),
    ("PUBLICSHARE", "folder-publicshare"),
    ("DOCUMENTS", "folder-documents"),
    ("MUSIC", "folder-music"),
    ("PICTURES", "folder-pictures"),
    ("VIDEOS", "folder-videos"),
];

impl UserDirs {
    /// The user's, from `user-dirs.dirs`.
    pub fn load() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".config")));
        let text = config
            .and_then(|c| std::fs::read_to_string(c.join("user-dirs.dirs")).ok())
            .unwrap_or_default();
        match home {
            Some(home) => Self::parse(&text, &home),
            None => Self::default(),
        }
    }

    /// The directories in a `user-dirs.dirs` text, for the home `home`:
    /// `XDG_DOWNLOAD_DIR="$HOME/Downloads"` lines.
    pub fn parse(text: &str, home: &Path) -> Self {
        let mut dirs = Vec::new();
        for line in text.lines() {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            let Some(name) = key
                .strip_prefix("XDG_")
                .and_then(|k| k.strip_suffix("_DIR"))
            else {
                continue;
            };
            let Some(&(_, icon)) = ICONS.iter().find(|(n, _)| *n == name) else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            let path = match value.strip_prefix("$HOME") {
                Some(rest) => home.join(rest.trim_start_matches('/')),
                None if value.starts_with('/') => PathBuf::from(value),
                None => continue,
            };
            // A directory set to the home directory means it's disabled.
            if path != home {
                dirs.push((path, icon));
            }
        }
        Self {
            home: Some(home.to_path_buf()),
            dirs,
        }
    }

    /// The theme icon of the folder at `path`, if it is a user directory.
    pub fn icon(&self, path: &Path) -> Option<&'static str> {
        if self.home.as_deref() == Some(path) {
            return Some("user-home");
        }
        (self.dirs.iter()).find_map(|(p, icon)| (p == path).then_some(*icon))
    }
}

/// The name a pinned folder shows: its last component (`Home` for the
/// home directory).
pub fn name(pin: &str, path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if pin == "~/" || home.as_deref() == Some(path) {
        return "Home".into();
    }
    (path.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| pin.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_dirs() {
        let home = Path::new("/home/u");
        let dirs = UserDirs::parse(
            "# written by xdg-user-dirs-update\n\
             XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n\
             XDG_DOWNLOAD_DIR=\"$HOME/Hämtningar\"\n\
             XDG_MUSIC_DIR=\"/srv/music\"\n\
             XDG_TEMPLATES_DIR=\"$HOME/\"\n\
             XDG_NOPE_DIR=\"$HOME/nope\"\n",
            home,
        );
        assert_eq!(dirs.icon(&home.join("Hämtningar")), Some("folder-download"));
        assert_eq!(dirs.icon(Path::new("/srv/music")), Some("folder-music"));
        assert_eq!(dirs.icon(&home.join("Desktop")), Some("user-desktop"));
        assert_eq!(dirs.icon(home), Some("user-home"));
        // Disabled (the home directory) or unknown: none.
        assert_eq!(dirs.icon(&home.join("nope")), None);
        assert_eq!(dirs.icon(Path::new("/tmp")), None);
    }

    #[test]
    fn names() {
        assert_eq!(name("/srv/x", Path::new("/srv/x")), "x");
        assert_eq!(name("~/", Path::new("/wherever")), "Home");
        assert_eq!(name("/", Path::new("/")), "/");
    }
}
