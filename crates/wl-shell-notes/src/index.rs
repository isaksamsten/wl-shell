//! Every note in a directory: its `.md` files, in subdirectories too.
//! Files and directories starting with `.` are left out, and symlinked
//! directories aren't followed.
//!
//! `scan` reads the tree again, re-reading only files whose size, mtime or
//! inode changed, and says what changed. A program watching the directory
//! (inotify) rescans on each batch of events; `dirs` lists the directories
//! to watch.

use std::collections::BTreeMap;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::matcher::Window;
use crate::note::Note;

/// What a file looked like when it was read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
    ino: u64,
}

impl Stamp {
    fn of(m: &std::fs::Metadata) -> Self {
        Self {
            modified: m.modified().ok(),
            len: m.len(),
            ino: m.ino(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub note: Note,
    /// When the file was created (else last modified), or first seen by
    /// this index if later: newer notes come first. A file replaced under
    /// the same name (a save by rename) keeps its time, so editing a note
    /// never moves it.
    pub created: SystemTime,
    stamp: Stamp,
}

/// What a scan found changed, as paths relative to the directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub added: Vec<PathBuf>,
    pub changed: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    /// Files and directories that couldn't be read.
    pub errors: Vec<String>,
}

impl Changes {
    /// Nothing about the notes changed (errors aside).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct Index {
    dir: PathBuf,
    /// By path relative to `dir`.
    notes: BTreeMap<PathBuf, Entry>,
    /// The directories found by the last scan, `dir` first.
    dirs: Vec<PathBuf>,
}

impl Index {
    /// An empty index of `dir`; `scan` fills it.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            notes: BTreeMap::new(),
            dirs: Vec::new(),
        }
    }

    /// The index of `dir`, scanned, and the errors found.
    pub fn load(dir: impl Into<PathBuf>) -> (Self, Vec<String>) {
        let mut index = Self::new(dir);
        let changes = index.scan();
        (index, changes.errors)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The directories the notes are in (absolute, the notes directory
    /// first), as of the last scan.
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// The note at `rel` (relative to the directory).
    pub fn get(&self, rel: &Path) -> Option<&Entry> {
        self.notes.get(rel)
    }

    /// The index's own path of the note at `rel`, if there is one.
    pub fn get_key(&self, rel: &Path) -> Option<&Path> {
        self.notes.get_key_value(rel).map(|(k, _)| k.as_path())
    }

    /// Every note, by path.
    pub fn iter(&self) -> impl Iterator<Item = (&Path, &Entry)> {
        self.notes.iter().map(|(p, e)| (p.as_path(), e))
    }

    /// Every note, newest first (by creation time), then by path.
    pub fn ordered(&self) -> Vec<&Path> {
        let mut all: Vec<(&Path, &Entry)> = self.iter().collect();
        all.sort_by(|(pa, a), (pb, b)| b.created.cmp(&a.created).then(pa.cmp(pb)));
        all.into_iter().map(|(p, _)| p).collect()
    }

    /// The notes shown for `window` (None: no window, so only the global
    /// notes), in `ordered` order.
    pub fn shown(&self, window: Option<&Window>) -> Vec<&Path> {
        (self.ordered().into_iter())
            .filter(|p| self.notes[*p].note.matches(window))
            .collect()
    }

    /// Read the directory again: new files are added, changed ones read
    /// again, and missing ones removed. A file that can't be read keeps
    /// what was read of it before.
    pub fn scan(&mut self) -> Changes {
        let mut changes = Changes::default();
        let mut found: Vec<(PathBuf, std::fs::Metadata)> = Vec::new();
        let mut dirs = Vec::new();
        if let Err(e) = walk(
            &self.dir,
            Path::new(""),
            &mut found,
            &mut dirs,
            &mut changes,
        ) {
            changes.errors.push(format!("{}: {e}", self.dir.display()));
        }
        let mut seen = std::collections::HashSet::new();
        for (rel, meta) in found {
            seen.insert(rel.clone());
            let stamp = Stamp::of(&meta);
            let old = self.notes.get(&rel);
            if old.is_some_and(|e| e.stamp == stamp) {
                continue;
            }
            let path = self.dir.join(&rel);
            let text = match std::fs::read(&path) {
                Ok(bytes) => String::from_utf8(bytes)
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()),
                Err(e) => {
                    changes.errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            let note = Note::parse(text);
            let created = match old {
                Some(e) => e.created,
                None => (meta.created().ok())
                    .or(stamp.modified)
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            };
            match old {
                Some(e) if e.note == note => {}
                Some(_) => changes.changed.push(rel.clone()),
                None => changes.added.push(rel.clone()),
            }
            self.notes.insert(
                rel,
                Entry {
                    note,
                    created,
                    stamp,
                },
            );
        }
        let gone: Vec<PathBuf> = (self.notes.keys())
            .filter(|p| !seen.contains(*p))
            .cloned()
            .collect();
        for rel in gone {
            self.notes.remove(&rel);
            changes.removed.push(rel);
        }
        self.dirs = dirs;
        changes
    }
}

/// Collect the notes under `root.join(rel)` into `found`, and the
/// directories into `dirs`. Fails only if that directory can't be read;
/// errors below it go to `changes`.
fn walk(
    root: &Path,
    rel: &Path,
    found: &mut Vec<(PathBuf, std::fs::Metadata)>,
    dirs: &mut Vec<PathBuf>,
    changes: &mut Changes,
) -> io::Result<()> {
    let dir = root.join(rel);
    let entries = std::fs::read_dir(&dir)?;
    dirs.push(dir.clone());
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        if name.as_encoded_bytes().starts_with(b".") {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let rel = rel.join(&name);
        if kind.is_dir() {
            if let Err(e) = walk(root, &rel, found, dirs, changes) {
                (changes.errors).push(format!("{}: {e}", root.join(&rel).display()));
            }
            continue;
        }
        if Path::new(&name).extension().is_none_or(|e| e != "md") {
            continue;
        }
        // Symlinks to files count; to directories, they don't.
        match std::fs::metadata(entry.path()) {
            Ok(m) if m.is_file() => found.push((rel, m)),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("wl-shell-notes-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.0.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            // Creation times far enough apart to order by.
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn paths(v: &[&Path]) -> Vec<String> {
        v.iter().map(|p| p.display().to_string()).collect()
    }

    #[test]
    fn reads_the_tree() {
        let t = Tmp::new("tree");
        t.write("a.md", "# A");
        t.write("work/b.md", "+++\napps = [\"firefox\"]\n+++\n");
        t.write(
            "work/deep/c.md",
            "+++\n[[match]]\ntitle = \"*wl-shell*\"\n+++\n",
        );
        t.write("notes.txt", "not a note");
        t.write(".hidden.md", "hidden");
        t.write(".git/d.md", "in a dot directory");
        let (index, errors) = Index::load(&t.0);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(index.len(), 3);
        assert_eq!(
            paths(&index.ordered()),
            ["work/deep/c.md", "work/b.md", "a.md"]
        );
        assert_eq!(index.dirs().len(), 3);
        assert_eq!(index.dirs()[0], t.0);
        let ff = Window::new("firefox", "Mozilla Firefox");
        assert_eq!(paths(&index.shown(Some(&ff))), ["work/b.md", "a.md"]);
        let foot = Window::new("foot", "~/projects/wl-shell");
        assert_eq!(paths(&index.shown(Some(&foot))), ["work/deep/c.md", "a.md"]);
        assert_eq!(paths(&index.shown(None)), ["a.md"]);
        let a = index.get(Path::new("a.md")).unwrap();
        assert_eq!(a.note.title(), Some("A"));
    }

    #[test]
    fn rescans_report_changes() {
        let t = Tmp::new("changes");
        t.write("a.md", "a");
        t.write("b.md", "b");
        let (mut index, _) = Index::load(&t.0);
        assert!(index.scan().is_empty());
        t.write("a.md", "+++\ncolor = \"pink\"\n+++\na");
        t.write("sub/c.md", "c");
        std::fs::remove_file(t.0.join("b.md")).unwrap();
        let changes = index.scan();
        assert_eq!(changes.changed, [PathBuf::from("a.md")]);
        assert_eq!(changes.added, [PathBuf::from("sub/c.md")]);
        assert_eq!(changes.removed, [PathBuf::from("b.md")]);
        assert_eq!(index.dirs().len(), 2);
        // A save by rename with the same text is no change.
        std::fs::write(t.0.join("tmp"), "c").unwrap();
        std::fs::rename(t.0.join("tmp"), t.0.join("sub/c.md")).unwrap();
        assert!(index.scan().is_empty());
        // Nor does a new file under an old name move it.
        let before = index.ordered().iter().map(|p| p.to_path_buf()).collect::<Vec<_>>();
        std::fs::write(t.0.join("tmp"), "c2").unwrap();
        std::fs::rename(t.0.join("tmp"), t.0.join("a.md")).unwrap();
        assert_eq!(index.scan().changed, [PathBuf::from("a.md")]);
        let after = index.ordered().iter().map(|p| p.to_path_buf()).collect::<Vec<_>>();
        assert_eq!(before, after);
        // The directory going away removes everything.
        std::fs::remove_dir_all(&t.0).unwrap();
        let changes = index.scan();
        assert_eq!(changes.removed.len(), 2);
        assert_eq!(changes.errors.len(), 1);
        assert!(index.is_empty());
    }
}
