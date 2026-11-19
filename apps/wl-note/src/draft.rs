//! The part of an open note the body editor owns, and its disk baseline.

use std::{io, path::Path};
use wl_shell_notes::Note;

use crate::{editor::Editor, markdown};

pub struct Draft {
    pub key: String,
    pub editor: Editor,
    body_start: usize,
    base: Note,
    clean: String,
    /// The exact disk version presented to the user (None if removed).
    pub conflict: Option<Option<String>>,
    pub error: Option<String>,
}

impl Draft {
    pub fn new(key: String, note: Note) -> Self {
        let body_start = markdown::editable(&note);
        let editor = Editor::new(&note.text()[body_start..]);
        let clean = editor.text().into();
        Self {
            key,
            editor,
            base: note,
            body_start,
            clean,
            conflict: None,
            error: None,
        }
    }

    pub fn base(&self) -> &Note {
        &self.base
    }

    pub fn dirty(&self) -> bool {
        self.editor.text() != self.clean
    }

    pub fn source(&self) -> String {
        let prefix = &self.base.text()[..self.body_start];
        let newline = self.newline();
        let mut source = prefix.to_string();
        if !prefix.is_empty() && !prefix.ends_with('\n') && !self.editor.text().is_empty() {
            source.push_str(newline);
        }
        source.push_str(&self.editor.text().replace('\n', newline));
        source
    }

    fn newline(&self) -> &'static str {
        if self.base.text().contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    pub fn saved(&mut self, source: String) {
        // A heading typed in the editor must not become part of the prefix
        // between saves, nor should blank lines migrate out of the body.
        let bytes = self.editor.text().replace('\n', self.newline()).len();
        self.body_start = source.len() - bytes;
        self.base = Note::parse(source);
        self.clean = self.editor.text().into();
        self.conflict = None;
        self.error = None;
    }

    pub fn reload(&mut self, note: Note) {
        self.body_start = markdown::editable(&note);
        let text = &note.text()[self.body_start..];
        if Editor::new(text).text() != self.editor.text() {
            self.editor.reload(text);
        }
        self.clean = self.editor.text().into();
        self.base = note;
        self.conflict = None;
        self.error = None;
    }

    /// Keep a dirty editor intact. Clean external updates keep its caret.
    pub fn observe(&mut self, disk: Option<&str>) {
        if disk == Some(self.base.text()) {
            self.conflict = None;
        } else if self.dirty() {
            self.conflict = Some(disk.map(str::to_owned));
        } else if let Some(text) = disk {
            self.reload(Note::parse(text));
        }
    }
}

pub enum SaveBody {
    Unchanged,
    Written,
    Conflict,
}

pub fn read_note(path: &Path) -> io::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

pub fn save_body(d: &mut Draft, path: &Path, force: bool) -> io::Result<SaveBody> {
    if !d.dirty() && d.conflict.is_none() {
        return Ok(SaveBody::Unchanged);
    }
    let disk = read_note(path)?;
    let accepted = force && d.conflict.as_ref() == Some(&disk);
    if !accepted && disk.as_deref() != Some(d.base().text()) {
        d.observe(disk.as_deref());
        return Ok(if d.conflict.is_some() {
            SaveBody::Conflict
        } else {
            SaveBody::Unchanged
        });
    }
    let source = d.source();
    wl_shell_notes::write_atomic(path, &source)?;
    d.saved(source);
    Ok(SaveBody::Written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_keeps_draft_and_reload_preserves_position() {
        let base = "+++\n# keep\n+++\n# Title\n\nfirst\nsecond";
        let mut d = Draft::new("note.md".into(), Note::parse(base));
        d.editor.doc_end(false);
        d.editor.insert("!");
        let theirs = base.replace("first", "other");
        d.observe(Some(&theirs));
        assert_eq!(d.conflict, Some(Some(theirs.clone())));
        assert_eq!(d.source(), format!("{base}!"));
        d.reload(Note::parse(theirs));
        assert_eq!(d.editor.line_col(d.editor.caret()), (3, 6));
        assert!(!d.dirty());
    }

    #[test]
    fn own_write_preserves_undo_and_frontmatter() {
        let mut d = Draft::new(
            "note.md".into(),
            Note::parse("+++\r\n# c\r\n+++\r\n# Title"),
        );
        d.editor.doc_end(false);
        d.editor.insert("\na\nb");
        let source = d.source();
        assert_eq!(source, "+++\r\n# c\r\n+++\r\n# Title\r\na\r\nb");
        d.saved(source.clone());
        d.observe(Some(&source));
        assert!(d.editor.undo());
        assert!(d.dirty());
    }

    #[test]
    fn repeated_saves_do_not_duplicate_headings() {
        let mut d = Draft::new("note.md".into(), Note::parse(""));
        d.editor.insert("# Heading\n\nbody");
        d.saved(d.source());
        d.editor.insert("!");
        assert_eq!(d.source(), "# Heading\n\nbody!");
    }

    #[test]
    fn save_checks_disk_again_before_overwriting() {
        let dir = std::env::temp_dir().join(format!("wl-note-conflict-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("note.md");
        let mut d = Draft::new("note.md".into(), Note::parse("original"));
        d.editor.doc_end(false);
        d.editor.insert("!");
        std::fs::write(&path, "theirs").unwrap();
        assert!(matches!(
            save_body(&mut d, &path, false).unwrap(),
            SaveBody::Conflict
        ));
        std::fs::write(&path, "newer").unwrap();
        assert!(matches!(
            save_body(&mut d, &path, true).unwrap(),
            SaveBody::Conflict
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "newer");
        assert!(matches!(
            save_body(&mut d, &path, true).unwrap(),
            SaveBody::Written
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original!");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
