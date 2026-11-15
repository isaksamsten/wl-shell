//! File types by name, from the shared MIME database
//! (`<data dir>/mime/globs2`), and the theme icons for them (`icons`,
//! `generic-icons`, then the usual fallbacks), for showing files without
//! reading them.
//!
//! Only simple globs are used: literal names (`Makefile`) and `*.<suffix>`
//! patterns; the longest matching suffix wins, then the highest weight.
//! Case-sensitive globs (`:cs`) match as written, the rest ignoring case.

use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct MimeDb {
    /// Suffixes (without the dot) to (weight, type); lowercase unless
    /// case-sensitive.
    suffixes: HashMap<String, (u32, String)>,
    exact_suffixes: HashMap<String, (u32, String)>,
    /// Literal file names.
    names: HashMap<String, (u32, String)>,
    /// Type to icon name.
    icons: HashMap<String, String>,
    generic: HashMap<String, String>,
}

/// The data directories, most important first.
fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|d| !d.is_empty()) {
        Some(d) => dirs.push(PathBuf::from(d)),
        None => {
            dirs.extend(std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        }
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(
        system
            .split(':')
            .filter(|d| !d.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// Insert `value` for `key` unless a heavier one is there; the first
/// seen wins ties (more important directories are read first).
fn put(map: &mut HashMap<String, (u32, String)>, key: String, weight: u32, mime: &str) {
    match map.get(&key) {
        Some((w, _)) if *w >= weight => {}
        _ => {
            map.insert(key, (weight, mime.to_string()));
        }
    }
}

impl MimeDb {
    /// The database of every data directory.
    pub fn load() -> Self {
        let mut db = Self::default();
        for dir in data_dirs() {
            let read = |name: &str| std::fs::read_to_string(dir.join("mime").join(name)).ok();
            if let Some(text) = read("globs2") {
                db.add_globs2(&text);
            }
            if let Some(text) = read("icons") {
                add_pairs(&mut db.icons, &text);
            }
            if let Some(text) = read("generic-icons") {
                add_pairs(&mut db.generic, &text);
            }
        }
        db
    }

    /// Add the globs of a `globs2` file: `weight:type:glob[:flags]`.
    pub fn add_globs2(&mut self, text: &str) {
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let mut parts = line.splitn(4, ':');
            let (Some(weight), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let Ok(weight) = weight.trim().parse::<u32>() else {
                continue;
            };
            let cs = parts
                .next()
                .is_some_and(|f| f.split(',').any(|f| f == "cs"));
            let wild = |s: &str| s.contains(['*', '?', '[']);
            if let Some(suffix) = glob.strip_prefix("*.")
                && !suffix.is_empty()
                && !wild(suffix)
            {
                match cs {
                    true => put(&mut self.exact_suffixes, suffix.into(), weight, mime),
                    false => put(&mut self.suffixes, suffix.to_lowercase(), weight, mime),
                }
            } else if !glob.is_empty() && !wild(glob) {
                put(&mut self.names, glob.into(), weight, mime);
            }
        }
    }

    /// The type of a file named `name`, if its name tells.
    pub fn type_of(&self, name: &str) -> Option<&str> {
        if let Some((_, mime)) = self.names.get(name) {
            return Some(mime);
        }
        // The longest suffix first: `.tar.gz` before `.gz`.
        for (i, _) in name.match_indices('.') {
            let suffix = &name[i + 1..];
            if let Some((_, mime)) = self.exact_suffixes.get(suffix) {
                return Some(mime);
            }
            if let Some((_, mime)) = self.suffixes.get(&suffix.to_lowercase()) {
                return Some(mime);
            }
        }
        None
    }

    /// Theme icon names for a file of type `mime` (None: unknown), best
    /// first.
    pub fn icons(&self, mime: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(mime) = mime {
            out.extend(self.icons.get(mime).cloned());
            out.push(mime.replace('/', "-"));
            out.extend(self.generic.get(mime).cloned());
            if let Some((media, _)) = mime.split_once('/') {
                out.push(format!("{media}-x-generic"));
            }
        }
        for fallback in ["application-x-generic", "text-x-generic", "unknown"] {
            out.push(fallback.into());
        }
        let mut seen = Vec::new();
        out.retain(|i| {
            let new = !seen.contains(i);
            seen.push(i.clone());
            new
        });
        out
    }
}

/// `type:value` lines; the first seen wins.
fn add_pairs(map: &mut HashMap<String, String>, text: &str) {
    for line in text.lines() {
        if let Some((mime, icon)) = line.split_once(':')
            && !mime.starts_with('#')
            && !icon.trim().is_empty()
        {
            map.entry(mime.trim().into())
                .or_insert_with(|| icon.trim().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> MimeDb {
        let mut db = MimeDb::default();
        db.add_globs2(
            "# comment\n\
             50:text/x-csrc:*.c\n\
             50:text/x-c++src:*.C:cs\n\
             50:application/gzip:*.gz\n\
             50:application/x-compressed-tar:*.tar.gz\n\
             10:text/x-makefile:Makefile\n\
             50:image/png:*.png\n\
             40:image/x-other:*.png\n\
             50:text/x-readme:README*\n",
        );
        add_pairs(&mut db.generic, "image/png:image-x-generic\n");
        add_pairs(&mut db.icons, "application/gzip:package-x-generic\n");
        db
    }

    #[test]
    fn types_by_name() {
        let db = db();
        assert_eq!(db.type_of("a.c"), Some("text/x-csrc"));
        // Case-sensitive globs match as written, the rest any case.
        assert_eq!(db.type_of("a.C"), Some("text/x-c++src"));
        assert_eq!(db.type_of("A.PNG"), Some("image/png"));
        // The longest suffix wins.
        assert_eq!(db.type_of("x.tar.gz"), Some("application/x-compressed-tar"));
        assert_eq!(db.type_of("x.gz"), Some("application/gzip"));
        assert_eq!(db.type_of("Makefile"), Some("text/x-makefile"));
        // Wildcards beyond `*.` aren't used.
        assert_eq!(db.type_of("README.md"), None);
        assert_eq!(db.type_of("noext"), None);
    }

    #[test]
    fn icons_best_first() {
        let db = db();
        assert_eq!(
            db.icons(Some("image/png")),
            [
                "image-png",
                "image-x-generic",
                "application-x-generic",
                "text-x-generic",
                "unknown"
            ]
        );
        assert_eq!(
            db.icons(Some("application/gzip"))[..2],
            ["package-x-generic", "application-gzip"]
        );
        assert_eq!(db.icons(None)[0], "application-x-generic");
    }
}
