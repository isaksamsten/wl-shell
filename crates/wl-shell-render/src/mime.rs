//! File types by name, from the shared MIME database
//! (`<data dir>/mime/globs2`), and the theme icons for them (`icons`,
//! `generic-icons`, then the usual fallbacks), for showing files without
//! reading them.
//!
//! Only simple globs are used: literal names (`Makefile`) and `*.<suffix>`
//! patterns; the longest matching suffix wins, then the highest weight.
//! Case-sensitive globs (`:cs`) match as written, the rest ignoring case.
//!
//! A type's parents come from `subclasses` (and `aliases` name the
//! canonical type); every `text/*` type is also a `text/plain`.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// The type of folders.
pub const DIRECTORY: &str = "inode/directory";
/// What a file's start is read for to tell text from the rest, bytes.
const SNIFF: usize = 512;

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
    /// Type to its direct parents.
    parents: HashMap<String, Vec<String>>,
    /// Alias to the canonical type.
    aliases: HashMap<String, String>,
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
            if let Some(text) = read("subclasses") {
                db.add_subclasses(&text);
            }
            if let Some(text) = read("aliases") {
                db.add_aliases(&text);
            }
        }
        db
    }

    /// Add the lines of a `subclasses` file: `<type> <parent>`.
    pub fn add_subclasses(&mut self, text: &str) {
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            if let Some((mime, parent)) = line.trim().split_once(' ') {
                let parents = self.parents.entry(mime.to_string()).or_default();
                let parent = parent.trim().to_string();
                if !parents.contains(&parent) {
                    parents.push(parent);
                }
            }
        }
    }

    /// Add the lines of an `aliases` file: `<alias> <type>`; the first
    /// seen wins.
    pub fn add_aliases(&mut self, text: &str) {
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            if let Some((alias, mime)) = line.trim().split_once(' ') {
                (self.aliases.entry(alias.to_string())).or_insert_with(|| mime.trim().to_string());
            }
        }
    }

    /// The canonical name of `mime`.
    pub fn canonical<'a>(&'a self, mime: &'a str) -> &'a str {
        self.aliases.get(mime).map_or(mime, String::as_str)
    }

    /// `mime` (made canonical) and every type it is a kind of, nearest
    /// first: `text/x-csrc`, `text/plain`, ...
    pub fn lineage(&self, mime: &str) -> Vec<String> {
        let mut out = vec![self.canonical(mime).to_string()];
        let mut i = 0;
        while i < out.len() {
            let mut parents: Vec<String> = self.parents.get(&out[i]).cloned().unwrap_or_default();
            if out[i].starts_with("text/") {
                parents.push("text/plain".into());
            }
            for p in parents {
                let p = self.canonical(&p).to_string();
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            i += 1;
        }
        out
    }

    /// The type of the file at `path` (a folder if `dir`): by its name,
    /// else `text/plain` if it starts with text. None if unknown.
    pub fn type_of_path(&self, path: &Path, dir: bool) -> Option<String> {
        if dir {
            return Some(DIRECTORY.into());
        }
        let name = path.file_name()?.to_string_lossy();
        if let Some(mime) = self.type_of(&name) {
            return Some(mime.to_string());
        }
        let mut start = Vec::with_capacity(SNIFF);
        let file = std::fs::File::open(path).ok()?;
        file.take(SNIFF as u64).read_to_end(&mut start).ok()?;
        looks_like_text(&start).then(|| "text/plain".into())
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

/// Whether a file starting with `start` is text: valid UTF-8 (but for a
/// character cut at the end) without NULs.
fn looks_like_text(start: &[u8]) -> bool {
    if start.contains(&0) {
        return false;
    }
    match std::str::from_utf8(start) {
        Ok(_) => true,
        // Cut in the middle of a character at the end.
        Err(e) => e.error_len().is_none() && start.len() >= SNIFF,
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

    #[test]
    fn lineage() {
        let mut db = db();
        db.add_subclasses(
            "application/x-shellscript application/x-executable\n\
             application/x-shellscript text/plain\n\
             text/x-csrc text/plain\n\
             image/svg+xml application/xml\n\
             application/xml text/plain\n",
        );
        db.add_aliases("text/x-c text/x-csrc\n");
        assert_eq!(db.lineage("text/x-csrc"), ["text/x-csrc", "text/plain"]);
        // Aliases name the canonical type.
        assert_eq!(db.lineage("text/x-c"), ["text/x-csrc", "text/plain"]);
        assert_eq!(
            db.lineage("image/svg+xml"),
            ["image/svg+xml", "application/xml", "text/plain"]
        );
        assert_eq!(
            db.lineage("application/x-shellscript"),
            [
                "application/x-shellscript",
                "application/x-executable",
                "text/plain"
            ]
        );
        // Every text type is plain text.
        assert_eq!(db.lineage("text/markdown"), ["text/markdown", "text/plain"]);
        assert_eq!(db.lineage("image/png"), ["image/png"]);
    }

    #[test]
    fn types_of_paths() {
        let dir = std::env::temp_dir().join(format!("wl-shell-mime-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes"), "hello\n").unwrap();
        std::fs::write(dir.join("blob"), [0u8, 1, 2]).unwrap();
        std::fs::write(dir.join("a.png"), [0u8]).unwrap();
        let db = db();
        assert_eq!(db.type_of_path(&dir, true).as_deref(), Some(DIRECTORY));
        assert_eq!(
            db.type_of_path(&dir.join("a.png"), false).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            db.type_of_path(&dir.join("notes"), false).as_deref(),
            Some("text/plain")
        );
        assert_eq!(db.type_of_path(&dir.join("blob"), false), None);
        assert_eq!(db.type_of_path(&dir.join("missing"), false), None);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(looks_like_text("é".as_bytes()));
        assert!(!looks_like_text(&[0xff, 0xfe]));
    }
}
