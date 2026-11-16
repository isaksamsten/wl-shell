//! The launcher's keys and query. Pure, unit tested; launcherview.rs
//! shows it and app.rs acts on it.
//!
//! | Key                       | Effect                                     |
//! | ------------------------- | ------------------------------------------ |
//! | typing                    | edits the query                            |
//! | the trigger (`'`)         | typed first: search files instead          |
//! | Backspace, Ctrl+W, Ctrl+U | delete a character, a word, everything     |
//! | Enter                     | open the selection (`launcher-enter`)      |
//! | Ctrl+Enter                | the other of activating and launching      |
//! | Up, Ctrl+N                | the next match: the drum rolls toward you  |
//! | Down, Ctrl+P              | the previous match: the drum rolls back    |
//! | Shift+Tab                 | from files back to the apps                |
//! | Escape                    | clear the query; with an empty one, close  |
//!
//! The trigger, typed into an empty app query, becomes a token in the
//! field (the scope is then files) and the rest is the file query; a
//! space is never one. Backspace (or Ctrl+W) on an empty file query, or
//! Shift+Tab, goes back to the apps with the query empty. A trigger typed
//! in the file query is a plain character (nucleo's exact-match prefix).
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
    /// Shift+Tab: out of files, back to the apps.
    Back,
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
            Keysym::ISO_Left_Tab => Key::Back,
            Keysym::Tab if mods.shift => Key::Back,
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
        !matches!(self, Key::Open { .. } | Key::Escape | Key::Back)
    }
}

/// What the query searches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Apps,
    Files,
}

/// The query: what was typed, with the caret at its end, and what it
/// searches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
    scope: Scope,
}

impl Query {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Apply an editing key; `trigger` typed first in an empty app query
    /// switches to files. Returns whether the text or the scope changed.
    pub fn edit(&mut self, key: &Key, trigger: Option<char>) -> bool {
        let before = (self.text.len(), self.scope);
        let files = self.scope == Scope::Files;
        match key {
            Key::Text(t) => {
                let rest = (trigger.filter(|c| !c.is_whitespace()))
                    .filter(|_| !files && self.text.is_empty())
                    .and_then(|c| t.strip_prefix(c));
                match rest {
                    Some(rest) => {
                        self.scope = Scope::Files;
                        self.text.push_str(rest);
                    }
                    None => self.text.push_str(t),
                }
            }
            Key::Backspace | Key::DeleteWord | Key::Back if files && self.text.is_empty() => {
                self.scope = Scope::Apps;
            }
            Key::Back if files => {
                self.scope = Scope::Apps;
                self.text.clear();
            }
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
            Key::Roll(_) | Key::Open { .. } | Key::Back | Key::Escape => {}
        }
        // Every edit of the text changes its length.
        (self.text.len(), self.scope) != before
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
        assert_eq!(k(Keysym::ISO_Left_Tab, None, false), Some(Key::Back));
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(Key::from(Keysym::Tab, Some("\t"), &shift), Some(Key::Back));
        assert!(Key::Backspace.repeats() && Key::Roll(1).repeats());
        assert!(!Key::Open { other: false }.repeats() && !Key::Escape.repeats());
        assert!(!Key::Back.repeats());
    }

    #[test]
    fn editing() {
        let mut q = Query::default();
        let edit = |q: &mut Query, key: Key| q.edit(&key, Some('\''));
        for t in ["v", "isual", " stu", "dio "] {
            assert!(edit(&mut q, Key::Text(t.into())));
        }
        assert_eq!(q.text(), "visual studio ");
        assert!(edit(&mut q, Key::DeleteWord));
        assert_eq!(q.text(), "visual ");
        assert!(edit(&mut q, Key::Backspace));
        assert_eq!(q.text(), "visual");
        assert!(edit(&mut q, Key::Text("é".into())) && edit(&mut q, Key::Backspace));
        assert_eq!(q.text(), "visual");
        assert!(edit(&mut q, Key::DeleteWord) && q.is_empty());
        assert!(!edit(&mut q, Key::Backspace) && !edit(&mut q, Key::DeleteWord));
        edit(&mut q, Key::Text("a b".into()));
        assert!(edit(&mut q, Key::Clear) && q.is_empty());
        assert!(!edit(&mut q, Key::Roll(1)) && !edit(&mut q, Key::Back));
        assert_eq!(q.scope(), Scope::Apps);
    }

    #[test]
    fn the_files_trigger() {
        let t = |s: &str| Key::Text(s.into());
        let edit = |q: &mut Query, key: Key| q.edit(&key, Some('\''));
        // `'file.pdf` searches files for `file.pdf`, typed or pasted.
        let mut q = Query::default();
        assert!(edit(&mut q, t("'")));
        assert_eq!((q.scope(), q.text()), (Scope::Files, ""));
        edit(&mut q, t("file.pdf"));
        assert_eq!((q.scope(), q.text()), (Scope::Files, "file.pdf"));
        let mut pasted = Query::default();
        edit(&mut pasted, t("'file.pdf"));
        assert_eq!(pasted, q);
        // In the file query, it is a plain character: `''x` is exact.
        let mut exact = Query::default();
        edit(&mut exact, t("'"));
        edit(&mut exact, t("'x"));
        assert_eq!((exact.scope(), exact.text()), (Scope::Files, "'x"));
        // Only first: after anything else, it is part of an app query.
        let mut apps = Query::default();
        edit(&mut apps, t("a'"));
        assert_eq!((apps.scope(), apps.text()), (Scope::Apps, "a'"));
        let mut apps = Query::default();
        edit(&mut apps, t(" "));
        edit(&mut apps, t("'"));
        assert_eq!((apps.scope(), apps.text()), (Scope::Apps, " '"));
        // A space never triggers, and without a trigger there are no files.
        let mut space = Query::default();
        space.edit(&t(" x"), Some(' '));
        assert_eq!((space.scope(), space.text()), (Scope::Apps, " x"));
        let mut none = Query::default();
        none.edit(&t("'x"), None);
        assert_eq!((none.scope(), none.text()), (Scope::Apps, "'x"));
        // Backspace on an empty file query goes back to the apps.
        assert!(edit(&mut q, Key::Clear) && q.scope() == Scope::Files);
        assert!(edit(&mut q, Key::Backspace));
        assert_eq!((q.scope(), q.text()), (Scope::Apps, ""));
        // So does Ctrl+W, and Shift+Tab at any time, emptying the query.
        edit(&mut q, t("'a"));
        assert!(edit(&mut q, Key::DeleteWord) && q.scope() == Scope::Files);
        assert!(edit(&mut q, Key::DeleteWord) && q.scope() == Scope::Apps);
        edit(&mut q, t("'abc"));
        assert!(edit(&mut q, Key::Back));
        assert_eq!((q.scope(), q.text()), (Scope::Apps, ""));
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
