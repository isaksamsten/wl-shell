//! Config files: `$XDG_CONFIG_HOME/<program>/config.toml` (default
//! `~/.config/<program>/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::appearance::Scheme;

/// A program's config: a TOML table with defaults for every key.
pub trait ConfigFile: DeserializeOwned + Default + Clone + PartialEq {
    /// The program's directory under `$XDG_CONFIG_HOME`.
    const DIR: &'static str;

    /// Pick the color scheme of its `[appearance]`; `desktop` gives the
    /// desktop's, if needed. See `Appearance::apply`.
    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme);

    /// Called after reading the file at `path`, e.g. to resolve paths in
    /// the config relative to it.
    fn loaded(&mut self, _path: &Path) {}

    /// Parse config text. The scheme is left for the caller to apply.
    fn parse(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// The file `read` reads: `explicit`, or the default config file.
    fn path(explicit: Option<&Path>) -> Option<PathBuf> {
        explicit
            .map(Path::to_path_buf)
            .or_else(|| default_path(Self::DIR))
    }

    /// Read `explicit`, or the default config file if None (missing is
    /// fine). The scheme is left for the caller to apply.
    fn read(explicit: Option<&Path>) -> Result<Self, String> {
        let Some(path) = Self::path(explicit) else {
            return Ok(Self::default());
        };
        let required = explicit.is_some();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if !required && e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut config =
            Self::parse(&text).map_err(|e| format!("{}: {}", path.display(), e.trim_end()))?;
        config.loaded(&path);
        Ok(config)
    }
}

/// `$XDG_CONFIG_HOME/<dir>/config.toml`, falling back to `~/.config`.
pub fn default_path(dir: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".config")))?;
    Some(base.join(dir).join("config.toml"))
}

/// Expand a leading `~/` and make relative paths relative to `dir`.
pub fn resolve(path: &Path, dir: Option<&Path>, home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let (Ok(rest), Some(home)) = (path.strip_prefix("~"), home) {
        return Path::new(home).join(rest);
    }
    match dir {
        Some(dir) if path.is_relative() => dir.join(path),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving() {
        let home = Some(std::ffi::OsStr::new("/home/u"));
        let dir = Some(Path::new("/etc/x"));
        let r = |p: &str| resolve(Path::new(p), dir, home);
        assert_eq!(r("~/a.png"), Path::new("/home/u/a.png"));
        assert_eq!(r("a.png"), Path::new("/etc/x/a.png"));
        assert_eq!(r("/abs.png"), Path::new("/abs.png"));
        assert_eq!(
            resolve(Path::new("~/a"), dir, None),
            Path::new("/etc/x/~/a")
        );
    }
}
