//! Storage and lifetime rules for window-local notes.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use wl_shell_core::toplevel::Toplevel;
use wl_shell_notes::{Index, Note, Window};

use crate::config::OnClose;
use crate::create;

/// A reversible encoding, including dot names, slashes and non-ASCII IDs.
/// Leave ordinary IDs readable; '%' is itself escaped.
pub fn directory(id: &str) -> String {
    let mut name = String::with_capacity(id.len());
    for b in id.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
            name.push(b as char);
        } else {
            let _ = write!(name, "%{b:02X}");
        }
    }
    name
}

pub fn owner(rel: &Path) -> Option<&str> {
    let mut parts = rel.iter();
    let dir = parts.next()?.to_str()?;
    parts.next()?;
    parts.next().is_none().then_some(dir)
}

/// Only real, immediate subdirectories can be window directories.
pub fn orphans(root: &Path, live: &HashSet<String>) -> std::io::Result<Vec<String>> {
    let mut gone = Vec::new();
    for item in fs::read_dir(root)? {
        let item = item?;
        if item.file_type()?.is_dir()
            && let Ok(name) = item.file_name().into_string()
            && !name.starts_with('.')
            && !live.contains(&name)
        {
            gone.push(name);
        }
    }
    Ok(gone)
}

/// Paths are in creation order, independent of saves (including after restart).
pub fn notes<'a>(index: &'a Index, dir: &str) -> impl Iterator<Item = &'a Path> {
    index
        .iter()
        .filter_map(move |(p, _)| (owner(p) == Some(dir)).then_some(p))
}

pub fn stem() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}-{:020}", create::now_stem(), now.as_nanos())
}

pub fn create(root: &Path, id: &str, text: &str) -> Result<PathBuf, String> {
    let dir = root.join(directory(id));
    let write = || -> std::io::Result<PathBuf> {
        fs::create_dir_all(&dir)?;
        if !fs::symlink_metadata(&dir)?.is_dir() {
            return Err(std::io::Error::other("not a scratch directory"));
        }
        wl_shell_notes::create_new(&dir, &stem(), text)
    };
    write().map_err(|e| format!("{}: {e}", dir.display()))
}

/// Copy the edited text to a fresh time name, then remove the scratch file.
/// Works across filesystems, and a failed write never removes the original.
pub fn keep(path: &Path, dest: &Path, window: Option<(&Window, bool)>) -> Result<PathBuf, String> {
    let read = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut note = Note::parse(read);
    note.edit(|doc| {
        // Strip the old scratch metadata when promoting an existing note.
        for key in ["apps", "match", "tags"] {
            doc.remove(key);
        }
        Ok(())
    })?;
    if let Some((window, exact)) = window {
        note.pin(window, exact)?;
    }
    let write = || -> std::io::Result<PathBuf> {
        fs::create_dir_all(dest)?;
        wl_shell_notes::create_new(dest, &create::now_stem(), note.text())
    };
    let new = write().map_err(|e| format!("{}: {e}", dest.display()))?;
    if let Err(e) = fs::remove_file(path) {
        return match fs::remove_file(&new) {
            Ok(()) => Err(format!("{}: {e}", path.display())),
            Err(rollback) => Err(format!(
                "{}: {e}; cannot undo keep at {}: {rollback}",
                path.display(),
                new.display()
            )),
        };
    }
    Ok(new)
}

pub fn disposition(policy: OnClose, note: &Note) -> OnClose {
    // A title alone is still user content, as is unreadable frontmatter.
    if note.error.is_none() && note.body().trim().is_empty() && note.title().is_none() {
        OnClose::Delete
    } else {
        policy
    }
}

pub fn listing(index: &Index, json: bool, windows: &[Toplevel]) -> String {
    let windows: std::collections::HashMap<_, _> = windows
        .iter()
        .map(|w| (directory(&w.identifier), w))
        .collect();
    let mut out = if json {
        String::from("[")
    } else {
        String::new()
    };
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    for (path, entry) in index.iter() {
        let Some(owner) = owner(path) else {
            continue;
        };
        let window = windows.get(owner);
        let name = crate::list::name(path);
        let fields = [
            path.to_string_lossy().into_owned(),
            entry.note.title_or(&name).to_owned(),
            window.map_or("", |w| &w.app_id).to_owned(),
            window.map_or("", |w| &w.title).to_owned(),
        ];
        if json {
            if out.len() > 1 {
                out.push(',');
            }
            let [path, title, app, window_title] = fields.map(|s| crate::list::string(&s));
            let _ = write!(
                out,
                "{{\"path\":{path},\"title\":{title},\"app\":{app},\"window_title\":{window_title}}}"
            );
        } else {
            let [path, title, app, window_title] = fields.map(|s| clean(&s));
            let _ = writeln!(out, "{path}\t{title}\t{app}\t{window_title}");
        }
    }
    if json {
        out.push_str("]\n");
    }
    out
}

/// Never remove unknown files or a directory with an outstanding Trash job.
pub fn remove_empty(dir: &Path) {
    if let Err(e) = fs::remove_dir(dir)
        && !matches!(
            e.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::DirectoryNotEmpty
        )
    {
        wl_shell_log::warn!("{}: {e}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "wl-note-scratch-{}-{}",
                std::process::id(),
                stem()
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn escaped_identifiers_are_distinct_and_safe() {
        for (id, name) in [
            ("..", "%2E%2E"),
            ("a/b", "a%2Fb"),
            ("a%2Fb", "a%252Fb"),
            ("å a", "%C3%A5%20a"),
            ("abc-1_2", "abc-1_2"),
        ] {
            assert_eq!(directory(id), name);
        }
    }

    #[test]
    fn windows_of_the_same_app_are_separate_after_reload() {
        let tmp = Tmp::new();
        let first = create(
            &tmp.0,
            "a/1",
            &create::text(None, Some("white"), None, Some("first"), None).unwrap(),
        )
        .unwrap();
        let second = create(
            &tmp.0,
            "a/1",
            &create::text(None, Some("pink"), None, Some("second"), None).unwrap(),
        )
        .unwrap();
        create(
            &tmp.0,
            "b/1",
            &create::text(None, Some("white"), None, Some("other"), None).unwrap(),
        )
        .unwrap();
        let expected = [
            first.strip_prefix(&tmp.0).unwrap(),
            second.strip_prefix(&tmp.0).unwrap(),
        ];
        let (mut index, errors) = Index::load(&tmp.0);
        assert!(errors.is_empty());
        let edited = create::text(None, Some("white"), None, Some("edited"), None).unwrap();
        wl_shell_notes::write_atomic(&first, &edited).unwrap();
        index.scan();
        for index in [index, Index::load(&tmp.0).0] {
            assert_eq!(
                notes(&index, &directory("a/1")).collect::<Vec<_>>(),
                expected
            );
            assert_eq!(notes(&index, &directory("b/1")).count(), 1);
            let output = listing(
                &index,
                true,
                &[Toplevel {
                    identifier: "b/1".into(),
                    app_id: "foot".into(),
                    title: "terminal two".into(),
                }],
            );
            assert!(output.contains("\"window_title\":\"terminal two\""));
            assert!(!edited.contains("scratch-"));
            assert!(listing(&index, true, &[]).contains("\"window_title\":\"\""));
        }
        std::os::unix::fs::symlink(&tmp.0, tmp.0.join("link")).unwrap();
        assert_eq!(
            orphans(&tmp.0, &HashSet::from([directory("a/1")])).unwrap(),
            [directory("b/1")]
        );
    }

    #[test]
    fn keep_moves_and_replaces_ignored_matchers() {
        let tmp = Tmp::new();
        let kept = tmp.0.join("kept");
        let w = Window::new("term*[1]", "Session [1]");
        for target in [None, Some((&w, false)), Some((&w, true))] {
            let source = "+++\ntitle = 'Testing' # comment\ncolor = 'white'\napps = ['*']\ntags = ['all']\n+++\n# body\n";
            let path = create(&tmp.0, "a", source).unwrap();
            let new = keep(&path, &kept, target).unwrap();
            assert!(!path.exists());
            let note = Note::parse(fs::read_to_string(new).unwrap());
            assert!(note.text().contains("# comment"));
            assert_eq!(note.body(), "# body\n");
            assert_eq!(note.global(), target.is_none());
            assert!(note.matches(Some(&w)));
            assert_eq!(
                note.matches(Some(&Window::new(&w.app_id, "other"))),
                target != Some((&w, true))
            );
            assert_eq!(
                note.matches(Some(&Window::new("other", &w.title))),
                target.is_none()
            );
        }
        let bad = create(&tmp.0, "a", "+++\nnot TOML\n+++\n").unwrap();
        assert!(keep(&bad, &kept, Some((&w, false))).is_err());
        assert!(bad.exists());
    }

    #[test]
    fn close_dispositions_protect_user_content() {
        for policy in [OnClose::Trash, OnClose::Delete, OnClose::Keep] {
            let empty = create::text(None, Some("white"), None, Some(" \n"), None).unwrap();
            assert_eq!(disposition(policy, &Note::parse(empty)), OnClose::Delete);
            for txt in [
                "some text\n",
                "+++\ntitle = 'remember'\n+++\n",
                "+++\nbroken\n+++\n",
            ] {
                assert_eq!(disposition(policy, &Note::parse(txt)), policy);
            }
        }
    }
}
