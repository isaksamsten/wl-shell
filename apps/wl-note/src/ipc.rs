//! What `wl-note new`, `focus`, `keep` and `screenshot` ask the running
//! wl-note, in the shared line protocol (see `wl_shell_daemon::ipc`).
//! Pure, unit tested.
//!
//! Each is answered `done` once wl-note has done it (a screenshot: once
//! it has started), or an error.

use wl_shell_daemon::ipc::{Request, line, payload};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum NoteRequest {
    /// Nothing asked (an empty request).
    #[default]
    Nothing,
    /// Make a note.
    New(NewNote),
    /// Focus the note named so (see find.rs); None: the first shown, or
    /// tuck the focused one back.
    Focus(Option<String>),
    Keep(String),
    /// Take a screenshot onto a note.
    Screenshot(Screenshot),
}

/// Where a screenshot goes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Screenshot {
    /// Onto the note named so (see find.rs); None: the open note, or a
    /// new one if none is open.
    pub note: Option<String>,
    /// Onto a new note.
    pub new: bool,
}

/// A note to make.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewNote {
    /// Shown everywhere, not attached to the focused app.
    pub global: bool,
    pub window: bool,
    pub color: Option<String>,
    pub title: Option<String>,
    /// Its body.
    pub text: Option<String>,
    /// When it is due, as a TOML date or datetime.
    pub due: Option<String>,
}

impl Request for NoteRequest {
    fn encode_lines(&self) -> String {
        match self {
            NoteRequest::Nothing => String::new(),
            NoteRequest::New(n) => {
                let mut out = line("new", None);
                if n.global {
                    out += &line("global", None);
                }
                if n.window {
                    out += &line("window", None);
                }
                let options = [
                    ("color", &n.color),
                    ("title", &n.title),
                    ("text", &n.text),
                    ("due", &n.due),
                ];
                for (word, value) in options {
                    if let Some(v) = value {
                        out += &line(word, Some(v));
                    }
                }
                out
            }
            NoteRequest::Focus(note) => line("focus", note.as_deref()),
            NoteRequest::Keep(note) => line("keep", Some(note)),
            NoteRequest::Screenshot(s) => {
                let mut out = line("screenshot", s.note.as_deref());
                if s.new {
                    out += &line("new", None);
                }
                out
            }
        }
    }

    fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
        let unknown = || format!("unknown request line '{text}'");
        match (word, p, &mut *self) {
            // Options of a screenshot, before `new` starts a note.
            ("new", None, NoteRequest::Screenshot(s)) => s.new = true,
            ("new", None, _) => *self = NoteRequest::New(NewNote::default()),
            ("screenshot", None, _) => *self = NoteRequest::Screenshot(Screenshot::default()),
            ("screenshot", p, _) => {
                *self = NoteRequest::Screenshot(Screenshot {
                    note: Some(payload(p, text)?),
                    new: false,
                })
            }
            ("focus", None, _) => *self = NoteRequest::Focus(None),
            ("focus", p, _) => *self = NoteRequest::Focus(Some(payload(p, text)?)),
            ("keep", p, _) => *self = NoteRequest::Keep(payload(p, text)?),
            ("global", None, NoteRequest::New(n)) => n.global = true,
            ("window", None, NoteRequest::New(n)) => n.window = true,
            ("color", p, NoteRequest::New(n)) => n.color = Some(payload(p, text)?),
            ("title", p, NoteRequest::New(n)) => n.title = Some(payload(p, text)?),
            ("text", p, NoteRequest::New(n)) => n.text = Some(payload(p, text)?),
            ("due", p, NoteRequest::New(n)) => n.due = Some(payload(p, text)?),
            _ => return Err(unknown()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for req in [
            NoteRequest::Nothing,
            NoteRequest::New(NewNote::default()),
            NoteRequest::New(NewNote {
                global: true,
                window: false,
                color: Some("pink".into()),
                title: Some("A title".into()),
                text: Some("- [ ] one\n- [ ] two\n".into()),
                due: Some("2026-11-20T09:00".into()),
            }),
            NoteRequest::New(NewNote {
                window: true,
                ..NewNote::default()
            }),
            NoteRequest::Keep("window/a.md".into()),
            NoteRequest::Focus(None),
            NoteRequest::Focus(Some("work/release.md".into())),
            NoteRequest::Screenshot(Screenshot::default()),
            NoteRequest::Screenshot(Screenshot {
                note: None,
                new: true,
            }),
            NoteRequest::Screenshot(Screenshot {
                note: Some("work/release.md".into()),
                new: false,
            }),
        ] {
            assert_eq!(NoteRequest::decode(req.encode().as_bytes()), Ok(Some(req)));
        }
        assert_eq!(NoteRequest::Focus(None).encode(), "focus\nend\n");
        // Options only after `new`.
        assert!(NoteRequest::decode(b"global\nend\n").is_err());
        assert!(NoteRequest::decode(b"focus\ntitle x\nend\n").is_err());
        assert!(NoteRequest::decode(b"new\ncolor\nend\n").is_err());
        assert!(NoteRequest::decode(b"hide x\nend\n").is_err());
        assert!(NoteRequest::decode(b"screenshot\ncolor pink\nend\n").is_err());
    }
}
