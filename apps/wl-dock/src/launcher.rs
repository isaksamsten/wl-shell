//! The launcher's keys and query. Pure, unit tested; launcherview.rs
//! shows it and app.rs acts on it.
//!
//! | Key                       | Effect                                     |
//! | ------------------------- | ------------------------------------------ |
//! | typing                    | edits the query                            |
//! | Backspace, Ctrl+W, Ctrl+U | delete a character, a word, everything     |
//! | Enter                     | open the selection (`launcher-enter`)      |
//! | Ctrl+Enter                | the other of activating and launching      |
//! | Up, Ctrl+N                | the next match: the drum rolls toward you  |
//! | Down, Ctrl+P              | the previous match: the drum rolls back    |
//! | Escape                    | clear the query; with an empty one, close  |
//!
//! Tab (the selection's actions) comes later. The caret is always at the
//! end of the query.

use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use wl_shell_render::icons::fold;

/// What a key does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// Typed text.
    Text(String),
    Backspace,
    DeleteWord,
    Clear,
    /// Roll to the next match (+1) or the previous one (−1).
    Roll(i32),
    /// Open the selection; `other` (Ctrl) does the other of activating
    /// and launching.
    Open {
        other: bool,
    },
    Escape,
}

impl Key {
    /// The key `sym` (typing `utf8`) with `mods` held; None if it does
    /// nothing.
    pub fn from(sym: Keysym, utf8: Option<&str>, mods: &Modifiers) -> Option<Self> {
        let ctrl = mods.ctrl && !mods.alt;
        Some(match sym {
            Keysym::Escape => Key::Escape,
            Keysym::Return | Keysym::KP_Enter => Key::Open { other: mods.ctrl },
            Keysym::BackSpace if ctrl => Key::DeleteWord,
            Keysym::BackSpace => Key::Backspace,
            Keysym::Up | Keysym::KP_Up => Key::Roll(1),
            Keysym::Down | Keysym::KP_Down => Key::Roll(-1),
            Keysym::n if ctrl => Key::Roll(1),
            Keysym::p if ctrl => Key::Roll(-1),
            Keysym::w if ctrl => Key::DeleteWord,
            Keysym::u if ctrl => Key::Clear,
            _ if mods.ctrl || mods.alt || mods.logo => return None,
            _ => {
                let text: String = utf8?.chars().filter(|c| !c.is_control()).collect();
                if text.is_empty() {
                    return None;
                }
                Key::Text(text)
            }
        })
    }

    /// Whether holding it down repeats it.
    pub fn repeats(&self) -> bool {
        !matches!(self, Key::Open { .. } | Key::Escape)
    }
}

/// The query: what was typed, with the caret at its end.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
}

impl Query {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Apply an editing key. Returns whether the text changed.
    pub fn edit(&mut self, key: &Key) -> bool {
        let before = self.text.len();
        match key {
            Key::Text(t) => self.text.push_str(t),
            Key::Backspace => {
                self.text.pop();
            }
            Key::DeleteWord => {
                let kept = self.text.trim_end().len();
                self.text.truncate(kept);
                let start = self.text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
                self.text.truncate(start);
            }
            Key::Clear => self.text.clear(),
            Key::Roll(_) | Key::Open { .. } | Key::Escape => {}
        }
        // Every edit changes the length.
        self.text.len() != before
    }
}

/// How the selected app's name shows next to the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Completion {
    /// Nothing (no query, or no match).
    None,
    /// The query is the start of the name: the rest follows it inline.
    Inline(String),
    /// Otherwise the name shows at the bar's end.
    After(String),
}

/// How `name` completes `query`. The query is a prefix of the name if
/// each of its characters matches the name's, ignoring case and accents.
pub fn completion(query: &str, name: Option<&str>) -> Completion {
    let Some(name) = name.filter(|_| !query.is_empty()) else {
        return Completion::None;
    };
    let same = |a: char, b: char| {
        let (mut x, mut y) = ([0; 4], [0; 4]);
        fold(a.encode_utf8(&mut x)) == fold(b.encode_utf8(&mut y))
    };
    let mut rest = name.char_indices();
    for q in query.chars() {
        match rest.next() {
            Some((_, n)) if same(q, n) => {}
            _ => return Completion::After(name.to_string()),
        }
    }
    let at = rest.next().map_or(name.len(), |(i, _)| i);
    Completion::Inline(name[at..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool) -> Modifiers {
        Modifiers {
            ctrl,
            ..Default::default()
        }
    }

    #[test]
    fn keys() {
        let k = |sym, utf8: Option<&str>, ctrl| Key::from(sym, utf8, &mods(ctrl));
        assert_eq!(k(Keysym::a, Some("a"), false), Some(Key::Text("a".into())));
        assert_eq!(k(Keysym::A, Some("A"), false), Some(Key::Text("A".into())));
        assert_eq!(
            k(Keysym::space, Some(" "), false),
            Some(Key::Text(" ".into()))
        );
        assert_eq!(
            k(Keysym::Return, None, false),
            Some(Key::Open { other: false })
        );
        assert_eq!(
            k(Keysym::KP_Enter, None, true),
            Some(Key::Open { other: true })
        );
        assert_eq!(k(Keysym::Up, None, false), Some(Key::Roll(1)));
        assert_eq!(k(Keysym::n, Some("\x0e"), true), Some(Key::Roll(1)));
        assert_eq!(k(Keysym::Down, None, false), Some(Key::Roll(-1)));
        assert_eq!(k(Keysym::p, Some("\x10"), true), Some(Key::Roll(-1)));
        assert_eq!(k(Keysym::w, Some("\x17"), true), Some(Key::DeleteWord));
        assert_eq!(
            k(Keysym::BackSpace, Some("\x08"), true),
            Some(Key::DeleteWord)
        );
        assert_eq!(
            k(Keysym::BackSpace, Some("\x08"), false),
            Some(Key::Backspace)
        );
        assert_eq!(k(Keysym::u, Some("\x15"), true), Some(Key::Clear));
        assert_eq!(k(Keysym::Escape, Some("\x1b"), false), Some(Key::Escape));
        // Other shortcuts and control characters type nothing.
        assert_eq!(k(Keysym::a, Some("\x01"), true), None);
        assert_eq!(k(Keysym::Tab, Some("\t"), false), None);
        assert_eq!(k(Keysym::Shift_L, None, false), None);
        assert!(Key::Backspace.repeats() && Key::Roll(1).repeats());
        assert!(!Key::Open { other: false }.repeats() && !Key::Escape.repeats());
    }

    #[test]
    fn editing() {
        let mut q = Query::default();
        for t in ["v", "isual", " stu", "dio "] {
            assert!(q.edit(&Key::Text(t.into())));
        }
        assert_eq!(q.text(), "visual studio ");
        assert!(q.edit(&Key::DeleteWord));
        assert_eq!(q.text(), "visual ");
        assert!(q.edit(&Key::Backspace));
        assert_eq!(q.text(), "visual");
        assert!(q.edit(&Key::Text("é".into())) && q.edit(&Key::Backspace));
        assert_eq!(q.text(), "visual");
        assert!(q.edit(&Key::DeleteWord) && q.is_empty());
        assert!(!q.edit(&Key::Backspace) && !q.edit(&Key::DeleteWord));
        q.edit(&Key::Text("a b".into()));
        assert!(q.edit(&Key::Clear) && q.is_empty());
        assert!(!q.edit(&Key::Roll(1)));
    }

    #[test]
    fn completions() {
        assert_eq!(
            completion("fir", Some("Firefox")),
            Completion::Inline("efox".into())
        );
        assert_eq!(
            completion("FIREFOX", Some("Firefox")),
            Completion::Inline("".into())
        );
        assert_eq!(
            completion("ecl", Some("Éclair")),
            Completion::Inline("air".into())
        );
        assert_eq!(
            completion("ff", Some("Firefox")),
            Completion::After("Firefox".into())
        );
        assert_eq!(
            completion("firefoxes", Some("Firefox")),
            Completion::After("Firefox".into())
        );
        assert_eq!(completion("", Some("Firefox")), Completion::None);
        assert_eq!(completion("x", None), Completion::None);
    }
}
