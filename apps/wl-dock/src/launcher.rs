//! The launcher's keys, query and levels. Pure, unit tested;
//! launcherview.rs shows it and app.rs acts on it.
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
//! | Tab                       | a match's actions; an action's choices     |
//! | Shift+Tab                 | back: out of a level, or files to apps     |
//! | Escape                    | clear the query; with an empty one, close  |
//!
//! The trigger, typed into an empty app query, becomes a token in the
//! field (the scope is then files) and the rest is the file query; a
//! space is never one. Backspace (or Ctrl+W) on an empty file query, or
//! Shift+Tab, goes back to the apps with the query empty. A trigger typed
//! in the file query is a plain character (nucleo's exact-match prefix).
//!
//! Tab on a match (a file or an app) enters its actions, a level with a
//! query of its own (`Nav`), and Tab (or Enter) on an action with
//! choices (Open with…) enters those. In a level, Escape, Shift+Tab, or
//! Backspace on an empty query go back one level, restoring its query
//! and selection (`act`). The caret is always at the end of the query.

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
    /// Tab: into the selection's actions or choices.
    Tab,
    /// Shift+Tab: back out of a level, or out of files to the apps.
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
            Keysym::Tab if !mods.ctrl && !mods.alt && !mods.logo => Key::Tab,
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
        !matches!(self, Key::Open { .. } | Key::Escape | Key::Back | Key::Tab)
    }
}

/// What the query searches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Apps,
    Files,
    /// A level's choices (a subject's actions, or an action's choices).
    Actions,
}

/// The query: what was typed, with the caret at its end, and what it
/// searches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
    scope: Scope,
}

impl Query {
    /// An empty query for a level's choices.
    pub fn actions() -> Self {
        Self {
            text: String::new(),
            scope: Scope::Actions,
        }
    }

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
                    .filter(|_| self.scope == Scope::Apps && self.text.is_empty())
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
            Key::Roll(_) | Key::Open { .. } | Key::Tab | Key::Back | Key::Escape => {}
        }
        // Every edit of the text changes its length.
        (self.text.len(), self.scope) != before
    }
}

/// What a key does to the launcher, given how deep it is in levels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Edit the query.
    Edit(Key),
    /// Clear the query.
    Clear,
    /// Close the launcher.
    Close,
    Roll(i32),
    /// Open (or run) the selection.
    Open {
        other: bool,
    },
    /// Into the selection's actions or choices.
    Into,
    /// Back out of the level.
    Out,
}

/// What `key` does with the launcher `depth` levels deep (0: searching)
/// and `query` typed.
pub fn act(key: Key, depth: usize, query: &Query) -> Act {
    match key {
        Key::Roll(by) => Act::Roll(by),
        Key::Open { other } => Act::Open { other },
        Key::Escape | Key::Back if depth > 0 => Act::Out,
        Key::Backspace | Key::DeleteWord if depth > 0 && query.is_empty() => Act::Out,
        Key::Tab => Act::Into,
        Key::Escape if query.is_empty() => Act::Close,
        Key::Escape => Act::Clear,
        key => Act::Edit(key),
    }
}

/// Where the launcher was when it entered a level, to go back to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection<S> {
    /// The selected match's ID.
    pub selected: Option<String>,
    /// The drum had been rolled to it.
    pub rolled: bool,
    /// What was shown.
    pub shown: S,
}

/// The levels the launcher went into (each `T`), with the query of each
/// and where it was (`S`, what it showed), so going back restores them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nav<T, S> {
    query: Query,
    levels: Vec<(Query, Selection<S>, T)>,
}

impl<T, S> Default for Nav<T, S> {
    fn default() -> Self {
        Self {
            query: Query::default(),
            levels: Vec::new(),
        }
    }
}

impl<T, S> Nav<T, S> {
    /// The current level's query.
    pub fn query(&self) -> &Query {
        &self.query
    }

    pub fn query_mut(&mut self) -> &mut Query {
        &mut self.query
    }

    /// Levels entered: 0 while searching.
    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    /// The current level, unless searching.
    pub fn level(&self) -> Option<&T> {
        self.levels.last().map(|(_, _, t)| t)
    }

    /// Every level entered, the first first.
    #[cfg(test)]
    pub fn levels(&self) -> impl Iterator<Item = &T> {
        self.levels.iter().map(|(_, _, t)| t)
    }

    /// Enter `level`, leaving from `from`; its query starts empty.
    pub fn enter(&mut self, from: Selection<S>, level: T) {
        let query = std::mem::replace(&mut self.query, Query::actions());
        self.levels.push((query, from, level));
    }

    /// Go back out of the current level: its query is the one before
    /// again; returns where it was. None while searching.
    pub fn leave(&mut self) -> Option<Selection<S>> {
        let (query, from, _) = self.levels.pop()?;
        self.query = query;
        Some(from)
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
        assert_eq!(k(Keysym::Tab, Some("\t"), false), Some(Key::Tab));
        assert_eq!(k(Keysym::Tab, Some("\t"), true), None);
        assert_eq!(k(Keysym::Shift_L, None, false), None);
        assert_eq!(k(Keysym::ISO_Left_Tab, None, false), Some(Key::Back));
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(Key::from(Keysym::Tab, Some("\t"), &shift), Some(Key::Back));
        assert!(Key::Backspace.repeats() && Key::Roll(1).repeats());
        assert!(!Key::Open { other: false }.repeats() && !Key::Escape.repeats());
        assert!(!Key::Back.repeats() && !Key::Tab.repeats());
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
    fn no_trigger_among_actions() {
        let mut q = Query::actions();
        assert!(q.edit(&Key::Text("'o".into()), Some('\'')));
        assert_eq!((q.scope(), q.text()), (Scope::Actions, "'o"));
        q.edit(&Key::Clear, Some('\''));
        assert!(!q.edit(&Key::Back, Some('\'')) && !q.edit(&Key::Backspace, None));
        assert_eq!(q.scope(), Scope::Actions);
    }

    /// A launcher's levels, as app.rs drives them: `S` is what the drum
    /// showed, by name.
    struct Launcher {
        nav: Nav<&'static str, Vec<&'static str>>,
        shown: Vec<&'static str>,
        selected: usize,
        rolled: bool,
    }

    impl Launcher {
        fn files(query: &str, shown: Vec<&'static str>) -> Self {
            let mut nav = Nav::default();
            nav.query_mut()
                .edit(&Key::Text(format!("'{query}")), Some('\''));
            Self {
                nav,
                shown,
                selected: 0,
                rolled: false,
            }
        }

        fn key(&mut self, key: Key) -> Act {
            let act = act(key, self.nav.depth(), self.nav.query());
            match &act {
                Act::Into => {
                    let from = Selection {
                        selected: Some(self.shown[self.selected].to_string()),
                        rolled: self.rolled,
                        shown: std::mem::take(&mut self.shown),
                    };
                    let (level, choices) = match self.nav.depth() {
                        0 => ("actions", vec!["open", "open-with", "open-directory"]),
                        _ => ("open-with", vec!["firefox", "xournal"]),
                    };
                    self.nav.enter(from, level);
                    (self.shown, self.selected, self.rolled) = (choices, 0, false);
                }
                Act::Out => {
                    let from = self.nav.leave().unwrap();
                    let at = from
                        .shown
                        .iter()
                        .position(|s| Some(*s) == from.selected.as_deref());
                    (self.shown, self.selected, self.rolled) =
                        (from.shown, at.unwrap(), from.rolled);
                }
                Act::Edit(key) => {
                    self.nav.query_mut().edit(key, Some('\''));
                }
                Act::Roll(by) => {
                    let n = self.shown.len() as i32;
                    self.selected = (self.selected as i32 + by).rem_euclid(n) as usize;
                    self.rolled = true;
                }
                _ => {}
            }
            act
        }

        fn front(&self) -> &str {
            self.shown[self.selected]
        }
    }

    #[test]
    fn into_actions_and_back() {
        let t = |s: &str| Key::Text(s.into());
        let mut l = Launcher::files("rep", vec!["report.pdf", "repo", "reply.txt"]);
        l.key(Key::Roll(1));
        assert_eq!(l.front(), "repo");
        // Tab: the actions, with a query of their own.
        assert_eq!(l.key(Key::Tab), Act::Into);
        assert_eq!((l.nav.depth(), l.nav.level()), (1, Some(&"actions")));
        assert_eq!(
            (l.nav.query().scope(), l.nav.query().text()),
            (Scope::Actions, "")
        );
        assert_eq!(l.front(), "open");
        // `ow`, Tab: Open with's apps.
        l.key(t("o"));
        l.key(t("w"));
        assert_eq!(l.nav.query().text(), "ow");
        l.selected = 1;
        assert_eq!(l.key(Key::Tab), Act::Into);
        assert_eq!(
            l.nav.levels().collect::<Vec<_>>(),
            [&"actions", &"open-with"]
        );
        assert_eq!(l.front(), "firefox");
        l.key(t("x"));
        assert_eq!(
            l.key(Key::Open { other: false }),
            Act::Open { other: false }
        );
        // Escape: back to the actions as they were, query and all.
        assert_eq!(l.key(Key::Escape), Act::Out);
        assert_eq!((l.nav.depth(), l.nav.query().text()), (1, "ow"));
        assert_eq!(l.front(), "open-with");
        // Backspace edits a query; on an empty one, it goes back.
        assert_eq!(l.key(Key::Backspace), Act::Edit(Key::Backspace));
        assert_eq!(l.key(Key::DeleteWord), Act::Edit(Key::DeleteWord));
        assert_eq!(l.key(Key::Backspace), Act::Out);
        // Back in the file search, with its query and the subject selected.
        assert_eq!(l.nav.depth(), 0);
        assert_eq!(
            (l.nav.query().scope(), l.nav.query().text()),
            (Scope::Files, "rep")
        );
        assert_eq!((l.front(), l.rolled), ("repo", true));
        // Escape there clears the file query, then closes.
        assert_eq!(l.key(Key::Escape), Act::Clear);
        l.nav.query_mut().edit(&Key::Clear, None);
        assert_eq!(l.key(Key::Escape), Act::Close);
    }

    #[test]
    fn shift_tab_goes_back_one_level() {
        let mut l = Launcher::files("rep", vec!["report.pdf"]);
        l.key(Key::Tab);
        l.key(Key::Text("x".into()));
        assert_eq!(l.key(Key::Back), Act::Out);
        assert_eq!((l.nav.depth(), l.nav.query().text()), (0, "rep"));
        assert_eq!((l.front(), l.rolled), ("report.pdf", false));
        // In files, Shift+Tab is an edit: back to the apps.
        assert_eq!(l.key(Key::Back), Act::Edit(Key::Back));
        assert_eq!(l.nav.query().scope(), Scope::Apps);
        // Tab on an app enters its actions too.
        assert_eq!(l.key(Key::Tab), Act::Into);
        assert_eq!(l.nav.depth(), 1);
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
