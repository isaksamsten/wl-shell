//! What `wl-note new` and `wl-note focus` ask the running wl-note, in the
//! shared line protocol (see `wl_shell_daemon::ipc`). Pure, unit tested.
//!
//! Both are answered `done` once wl-note has done it, or an error.

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
                for (word, value) in [("color", &n.color), ("title", &n.title), ("text", &n.text)] {
                    if let Some(v) = value {
                        out += &line(word, Some(v));
                    }
                }
                out
            }
            NoteRequest::Focus(note) => line("focus", note.as_deref()),
            NoteRequest::Keep(note) => line("keep", Some(note)),
        }
    }

    fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
        let unknown = || format!("unknown request line '{text}'");
        match (word, p, &mut *self) {
            ("new", None, _) => *self = NoteRequest::New(NewNote::default()),
            ("focus", None, _) => *self = NoteRequest::Focus(None),
            ("focus", p, _) => *self = NoteRequest::Focus(Some(payload(p, text)?)),
            ("keep", p, _) => *self = NoteRequest::Keep(payload(p, text)?),
            ("global", None, NoteRequest::New(n)) => n.global = true,
            ("window", None, NoteRequest::New(n)) => n.window = true,
            ("color", p, NoteRequest::New(n)) => n.color = Some(payload(p, text)?),
            ("title", p, NoteRequest::New(n)) => n.title = Some(payload(p, text)?),
            ("text", p, NoteRequest::New(n)) => n.text = Some(payload(p, text)?),
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
            }),
            NoteRequest::New(NewNote {
                window: true,
                ..NewNote::default()
            }),
            NoteRequest::Keep("window/a.md".into()),
            NoteRequest::Focus(None),
            NoteRequest::Focus(Some("work/release.md".into())),
        ] {
            assert_eq!(NoteRequest::decode(req.encode().as_bytes()), Ok(Some(req)));
        }
        assert_eq!(NoteRequest::Focus(None).encode(), "focus\nend\n");
        // Options only after `new`.
        assert!(NoteRequest::decode(b"global\nend\n").is_err());
        assert!(NoteRequest::decode(b"focus\ntitle x\nend\n").is_err());
        assert!(NoteRequest::decode(b"new\ncolor\nend\n").is_err());
        assert!(NoteRequest::decode(b"hide x\nend\n").is_err());
    }
}
