//! How the notes are arranged, kept outside them so arranging never
//! rewrites a note: in `$XDG_STATE_HOME/wl-note/` (default
//! `~/.local/state/wl-note/`), as wl-dock keeps its pins.
//!
//! ```text
//! order     notes in column order, one per line
//! hidden    hidden notes, one per line
//! ```
//!
//! Each line is a path relative to the notes directory
//! (`work/release.md`). Blank lines and `#` comments are ignored. Paths
//! that name no note are kept and ignored, since a note may be away for a
//! moment (a sync, an editor saving by rename); `prune` drops them. A
//! missing file means no order, or nothing hidden. Files are written
//! atomically.
//!
//! The column shows notes in the order file's order; notes not in it come
//! after, newest first (`arrange`). One order for all notes: each context
//! shows its subset in it.

use std::io;
use std::path::{Component, Path, PathBuf};

use crate::index::Index;
use crate::matcher::Window;
use crate::write::write_atomic;

/// The order file's name in the state directory.
pub const ORDER: &str = "order";
/// The hidden file's.
pub const HIDDEN: &str = "hidden";

/// The state directory: `$XDG_STATE_HOME/wl-note`, falling back to
/// `~/.local/state/wl-note`. None without either variable.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".local/state")))?;
    Some(base.join("wl-note"))
}

/// A list of notes, each once, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PathList {
    paths: Vec<PathBuf>,
}

/// A path as the lists keep it: relative, without `.` parts or a
/// trailing slash. None for what isn't a path in the notes directory.
fn normal(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

impl PathList {
    /// The list in a file's text.
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            list.push(Path::new(line));
        }
        list
    }

    /// The file's text: `header` as a comment, then a path per line.
    pub fn format(&self, header: &str) -> String {
        let mut out = String::new();
        for line in header.lines() {
            out.push_str("# ");
            out.push_str(line);
            out.push('\n');
        }
        for p in &self.paths {
            out.push_str(&p.to_string_lossy());
            out.push('\n');
        }
        out
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        normal(path).is_some_and(|p| self.paths.contains(&p))
    }

    /// Where `path` is in the list.
    pub fn position(&self, path: &Path) -> Option<usize> {
        let p = normal(path)?;
        self.paths.iter().position(|q| *q == p)
    }

    /// Add `path` at the end. Returns false if it was there already.
    pub fn push(&mut self, path: &Path) -> bool {
        match normal(path) {
            Some(p) if !self.paths.contains(&p) => {
                self.paths.push(p);
                true
            }
            _ => false,
        }
    }

    /// Remove `path`. Returns false if it wasn't there.
    pub fn remove(&mut self, path: &Path) -> bool {
        let Some(p) = normal(path) else {
            return false;
        };
        let before = self.paths.len();
        self.paths.retain(|q| *q != p);
        self.paths.len() != before
    }

    /// Replace the list with `paths` (each kept once).
    pub fn set(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.paths.clear();
        for p in paths {
            self.push(&p);
        }
    }

    /// `from` (a note, or a directory of them) was renamed to `to`: its
    /// entries, and those under it, follow. Returns whether any did.
    pub fn rename(&mut self, from: &Path, to: &Path) -> bool {
        let (Some(from), Some(to)) = (normal(from), normal(to)) else {
            return false;
        };
        let mut changed = false;
        for i in 0..self.paths.len() {
            let Ok(rest) = self.paths[i].strip_prefix(&from) else {
                continue;
            };
            let new = match rest.as_os_str().is_empty() {
                true => to.clone(),
                false => to.join(rest),
            };
            changed = true;
            // A note already there keeps its place; this one's entry goes.
            match self.paths.contains(&new) {
                true => self.paths[i] = PathBuf::new(),
                false => self.paths[i] = new,
            }
        }
        self.paths.retain(|p| !p.as_os_str().is_empty());
        changed
    }

    /// Keep only the entries `keep` says to. Returns how many went.
    pub fn retain(&mut self, mut keep: impl FnMut(&Path) -> bool) -> usize {
        let before = self.paths.len();
        self.paths.retain(|p| keep(p));
        before - self.paths.len()
    }
}

/// Read a list; empty if the file doesn't exist.
pub fn read(path: &Path) -> io::Result<PathList> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(PathList::parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PathList::default()),
        Err(e) => Err(e),
    }
}

/// Write a list atomically, creating its directory.
pub fn write(path: &Path, list: &PathList, header: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(path, &list.format(header))
}

const ORDER_HEADER: &str = "wl-note: notes in column order, one per line, relative to notes-dir.\n\
                            Notes not listed come after, newest first.";
const HIDDEN_HEADER: &str = "wl-note: hidden notes, one per line, relative to notes-dir.";

/// The order and the hidden notes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub order: PathList,
    pub hidden: PathList,
}

impl State {
    /// Read the state in `dir`, and the errors found (a file that can't be
    /// read counts as empty).
    pub fn load(dir: &Path) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut list = |name: &str| {
            let path = dir.join(name);
            read(&path).unwrap_or_else(|e| {
                errors.push(format!("{}: {e}", path.display()));
                PathList::default()
            })
        };
        let state = State {
            order: list(ORDER),
            hidden: list(HIDDEN),
        };
        (state, errors)
    }

    /// Write the order file in `dir`.
    pub fn save_order(&self, dir: &Path) -> io::Result<()> {
        write(&dir.join(ORDER), &self.order, ORDER_HEADER)
    }

    /// Write the hidden file in `dir`.
    pub fn save_hidden(&self, dir: &Path) -> io::Result<()> {
        write(&dir.join(HIDDEN), &self.hidden, HIDDEN_HEADER)
    }

    pub fn is_hidden(&self, path: &Path) -> bool {
        self.hidden.contains(path)
    }

    /// Every note of `index` in column order: the order file's first,
    /// then the others newest first.
    pub fn arrange<'a>(&self, index: &'a Index) -> Vec<&'a Path> {
        let ordered = (self.order.paths.iter()).filter_map(|p| index.get_key(p));
        let rest = (index.ordered().into_iter()).filter(|p| !self.order.contains(p));
        ordered.chain(rest).collect()
    }

    /// The notes shown for `window` (None: only the global ones), in
    /// column order: those that match it and aren't hidden.
    pub fn shown<'a>(&self, index: &'a Index, window: Option<&Window>) -> Vec<&'a Path> {
        self.shown_where(index, |note| note.matches(window))
    }

    /// The notes that aren't hidden and `wanted` says to show, in column
    /// order.
    pub fn shown_where<'a>(
        &self,
        index: &'a Index,
        wanted: impl Fn(&crate::note::Note) -> bool,
    ) -> Vec<&'a Path> {
        (self.arrange(index).into_iter())
            .filter(|p| !self.is_hidden(p))
            .filter(|p| index.get(p).is_some_and(|e| wanted(&e.note)))
            .collect()
    }

    /// The hidden notes, in column order.
    pub fn hidden_notes<'a>(&self, index: &'a Index) -> Vec<&'a Path> {
        (self.arrange(index).into_iter())
            .filter(|p| self.is_hidden(p))
            .collect()
    }

    /// The order file's entries, followed by the notes not in it (as
    /// `arrange` puts them): the whole column order, written out, stale
    /// entries kept where they are.
    pub fn full_order(&self, index: &Index) -> Vec<PathBuf> {
        let mut all = self.order.paths.clone();
        all.extend(
            (index.ordered().into_iter())
                .filter(|p| !self.order.contains(p))
                .map(Path::to_path_buf),
        );
        all
    }

    /// A note (or directory) was renamed: its entries follow. Returns
    /// whether the order and the hidden list changed.
    pub fn rename(&mut self, from: &Path, to: &Path) -> (bool, bool) {
        (self.order.rename(from, to), self.hidden.rename(from, to))
    }

    /// Drop the entries that name no note of `index`. Returns how many
    /// went from the order and from the hidden list.
    pub fn prune(&mut self, index: &Index) -> (usize, usize) {
        let mut keep = |p: &Path| index.get(p).is_some();
        (self.order.retain(&mut keep), self.hidden.retain(&mut keep))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn list(paths: &[&str]) -> PathList {
        let mut l = PathList::default();
        for p in paths {
            l.push(Path::new(p));
        }
        l
    }

    fn strs(paths: &[&Path]) -> Vec<String> {
        paths.iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn parsing() {
        let l =
            PathList::parse("# comment\n\n  work/a.md  \n./b.md\nwork/a.md\n../out.md\n/abs.md\n");
        assert_eq!(l, list(&["work/a.md", "b.md"]));
        assert!(l.contains(Path::new("./work/a.md")));
        let text = l.format("one\ntwo");
        assert_eq!(text, "# one\n# two\nwork/a.md\nb.md\n");
        assert_eq!(PathList::parse(&text), l);
    }

    #[test]
    fn renames_follow_files_and_directories() {
        let mut l = list(&["a.md", "work/b.md", "work/deep/c.md", "workshop.md"]);
        assert!(l.rename(Path::new("a.md"), Path::new("z.md")));
        assert!(l.rename(Path::new("work"), Path::new("done/work")));
        assert_eq!(
            l,
            list(&[
                "z.md",
                "done/work/b.md",
                "done/work/deep/c.md",
                "workshop.md"
            ])
        );
        assert!(!l.rename(Path::new("nope.md"), Path::new("x.md")));
        // Onto a name listed already: that one keeps its place.
        assert!(l.rename(Path::new("z.md"), Path::new("workshop.md")));
        assert_eq!(
            l,
            list(&["done/work/b.md", "done/work/deep/c.md", "workshop.md"])
        );
    }

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("wl-shell-state-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn arranging() {
        let t = Tmp::new("arrange");
        for n in ["a.md", "b.md", "c.md", "d.md"] {
            t.write(n, "x");
        }
        t.write("ff.md", "+++\napps = [\"firefox\"]\n+++\n");
        let (index, _) = Index::load(&t.0);
        let mut s = State {
            order: list(&["c.md", "gone.md", "a.md"]),
            hidden: list(&["d.md", "gone.md"]),
        };
        // The ordered ones first; the rest newest first; stale ones
        // ignored.
        assert_eq!(
            strs(&s.arrange(&index)),
            ["c.md", "a.md", "ff.md", "d.md", "b.md"]
        );
        assert_eq!(strs(&s.shown(&index, None)), ["c.md", "a.md", "b.md"]);
        let ff = Window::new("firefox", "");
        assert_eq!(
            strs(&s.shown(&index, Some(&ff))),
            ["c.md", "a.md", "ff.md", "b.md"]
        );
        assert_eq!(strs(&s.hidden_notes(&index)), ["d.md"]);
        assert_eq!(
            s.full_order(&index),
            ["c.md", "gone.md", "a.md", "ff.md", "d.md", "b.md"].map(PathBuf::from)
        );
        // Pruning drops the stale lines only.
        assert_eq!(s.prune(&index), (1, 1));
        assert_eq!(s.order, list(&["c.md", "a.md"]));
        assert_eq!(s.hidden, list(&["d.md"]));
    }

    #[test]
    fn files() {
        let t = Tmp::new("files");
        let dir = t.0.join("state");
        let (s, errors) = State::load(&dir);
        assert!(errors.is_empty());
        assert_eq!(s, State::default());
        let s = State {
            order: list(&["b.md", "a.md"]),
            hidden: list(&["c.md"]),
        };
        s.save_order(&dir).unwrap();
        s.save_hidden(&dir).unwrap();
        assert_eq!(State::load(&dir).0, s);
        let text = std::fs::read_to_string(dir.join(ORDER)).unwrap();
        assert!(
            text.starts_with("# wl-note:") && text.ends_with("b.md\na.md\n"),
            "{text}"
        );
        // No temporary files left.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    }
}
