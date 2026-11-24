//! The launcher's keys, query and levels. Pure, unit tested;
//! launcherview.rs shows it and app.rs acts on it.
//!
//! | Key                       | Effect                                     |
//! | ------------------------- | ------------------------------------------ |
//! | typing                    | edits the query                            |
//! | the trigger (`'`)         | typed first: search files instead          |
//! | `@`                       | typed first: search every window instead   |
//! | a source's trigger        | typed first: search a plugin's source      |
//! | Backspace, Ctrl+W, Ctrl+U | delete a character, a word, everything     |
//! | Enter                     | open the selection (`launcher-enter`)      |
//! | Ctrl+Enter                | the other of activating and launching      |
//! | Alt+Enter                 | search the web for the query               |
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
//! Window search's trigger (`@`) works the same way, with the same ways
//! back, and so do the triggers of plugins' launcher sources. Which
//! character enters which scope is a table (`Triggers`): the files
//! trigger, then the windows trigger, then the sources' in order; the
//! first to claim a character keeps it.
//!
//! Tab on a match (a file, an app or a window) enters its actions, a level with a
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
    /// Alt+Enter: search the web for the app query with the fallback
    /// engine, whatever is in front.
    Search,
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
            Keysym::Return | Keysym::KP_Enter if mods.alt && !mods.ctrl => Key::Search,
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
        !matches!(
            self,
            Key::Open { .. } | Key::Search | Key::Escape | Key::Back | Key::Tab
        )
    }
}

/// What the query searches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scope {
    #[default]
    Apps,
    Files,
    Bang,
    Web,
    /// Every open window, by title (the `@` trigger).
    Windows,
    /// A level's choices (a subject's actions, or an action's choices).
    Actions,
    /// A plugin's launcher source, by its key (sources.rs).
    Source(u32),
}

impl Scope {
    /// Backspace on its empty query (and Shift+Tab) goes back to the
    /// apps: the scopes a trigger switched to.
    fn has_way_back(self) -> bool {
        matches!(
            self,
            Scope::Files | Scope::Bang | Scope::Windows | Scope::Source(_)
        )
    }
}

/// Which scope a character typed first into an empty app query enters,
/// in order: the first to claim a character keeps it. Spaces never
/// trigger.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Triggers(Vec<(char, Scope)>);

impl Triggers {
    /// The files trigger, then the windows trigger.
    pub fn new(files: Option<char>, windows: Option<char>) -> Self {
        let mut t = Triggers::default();
        for (c, scope) in [(files, Scope::Files), (windows, Scope::Windows)] {
            if let Some(c) = c {
                t.add(c, scope);
            }
        }
        t
    }

    /// `c` enters `scope`, unless it is a space or taken; whether it
    /// does.
    pub fn add(&mut self, c: char, scope: Scope) -> bool {
        if c.is_whitespace() || self.0.iter().any(|(t, _)| *t == c) {
            return false;
        }
        self.0.push((c, scope));
        true
    }

    /// The scope `text` enters with its first character, and the rest.
    fn find<'a>(&self, text: &'a str) -> Option<(Scope, &'a str)> {
        let c = text.chars().next()?;
        let (_, scope) = self.0.iter().find(|(t, _)| *t == c)?;
        Some((*scope, &text[c.len_utf8()..]))
    }
}

/// The query: what was typed, with the caret at its end, and what it
/// searches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    text: String,
    scope: Scope,
    engine: Option<String>,
}

impl Query {
    /// An empty query for a level's choices.
    pub fn actions() -> Self {
        Self {
            text: String::new(),
            scope: Scope::Actions,
            engine: None,
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

    pub fn engine(&self) -> Option<&str> {
        self.engine.as_deref()
    }

    /// A trigger at the start of the app query is parsed out of band.
    pub fn start_bang(&mut self, text: &str, trigger: Option<char>) -> bool {
        if self.scope != Scope::Apps || !self.is_empty() {
            return false;
        }
        let Some(rest) = trigger.and_then(|c| text.strip_prefix(c)) else {
            return false;
        };
        self.scope = Scope::Bang;
        self.text.push_str(rest);
        true
    }

    pub fn commit_engine(&mut self, keyword: String, text: &str) {
        self.text = text.to_owned();
        self.engine = Some(keyword);
        self.scope = Scope::Web;
    }

    pub fn complete_web(&mut self, text: String) {
        self.text = text;
    }

    /// Apply an editing key; `trigger` typed first in an empty app query
    /// switches to files. Returns whether the text or the scope changed.
    pub fn edit(&mut self, key: &Key, trigger: Option<char>) -> bool {
        self.edit_with(key, &Triggers::new(trigger, None))
    }

    /// Like `edit`, with `windows` as the trigger of window search too.
    /// The file trigger wins should they be the same character.
    #[cfg(test)]
    pub fn edit_in(&mut self, key: &Key, files: Option<char>, windows: Option<char>) -> bool {
        self.edit_with(key, &Triggers::new(files, windows))
    }

    /// Apply an editing key; a trigger of `triggers` typed first in an
    /// empty app query switches to its scope. Returns whether the text or
    /// the scope changed.
    pub fn edit_with(&mut self, key: &Key, triggers: &Triggers) -> bool {
        let before = (self.text.len(), self.scope);
        match key {
            Key::Text(t) => {
                let first = self.scope == Scope::Apps && self.text.is_empty();
                match triggers.find(t).filter(|_| first) {
                    Some((scope, rest)) => {
                        self.scope = scope;
                        self.text.push_str(rest);
                    }
                    None => self.text.push_str(t),
                }
            }
            Key::Back if self.scope.has_way_back() => {
                *self = Self::default();
            }
            Key::Backspace | Key::DeleteWord if self.scope.has_way_back() && self.is_empty() => {
                *self = Self::default();
            }
            Key::Backspace => {
                self.text.pop();
            }
            Key::DeleteWord => {
                let kept = self.text.trim_end().len();
                self.text.truncate(kept);
                let start = self
                    .text
                    .char_indices()
                    .rfind(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                self.text.truncate(start);
            }
            Key::Clear => self.text.clear(),
            Key::Roll(_) | Key::Open { .. } | Key::Search | Key::Tab | Key::Back | Key::Escape => {}
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
    /// Search the web for the app query.
    Search,
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
        Key::Search => Act::Search,
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

    /// The current level, to change it (as its choices arrive).
    pub fn level_mut(&mut self) -> Option<&mut T> {
        self.levels.last_mut().map(|(_, _, t)| t)
    }

    /// Every level entered, the first first.
    #[cfg(test)]
    pub fn levels(&self) -> impl Iterator<Item = &T> {
        self.levels.iter().map(|(_, _, t)| t)
    }

    /// Enter `level`, leaving from `from`; its query starts empty.
    #[cfg(test)]
    pub fn enter(&mut self, from: Selection<S>, level: T) {
        self.enter_with(from, level, Query::actions());
    }

    pub fn enter_with(&mut self, from: Selection<S>, level: T, next: Query) {
        let query = std::mem::replace(&mut self.query, next);
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
        // Alt+Enter searches the web; with Ctrl too, it is an Enter.
        let alt = Modifiers {
            alt: true,
            ..Default::default()
        };
        assert_eq!(Key::from(Keysym::Return, None, &alt), Some(Key::Search));
        assert_eq!(Key::from(Keysym::KP_Enter, None, &alt), Some(Key::Search));
        let both = Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };
        assert_eq!(
            Key::from(Keysym::Return, None, &both),
            Some(Key::Open { other: true })
        );
        assert!(!Key::Search.repeats());
        assert_eq!(act(Key::Search, 0, &Query::default()), Act::Search);
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
    fn the_windows_trigger() {
        let t = |s: &str| Key::Text(s.into());
        let edit = |q: &mut Query, key: Key| q.edit_in(&key, Some('\''), Some('@'));
        // `@` alone switches; the rest is the title query.
        let mut q = Query::default();
        assert!(edit(&mut q, t("@")));
        assert_eq!((q.scope(), q.text()), (Scope::Windows, ""));
        edit(&mut q, t("plan"));
        assert_eq!((q.scope(), q.text()), (Scope::Windows, "plan"));
        // Pasting `@foot` is the same as typing it.
        let mut pasted = Query::default();
        edit(&mut pasted, t("@foot"));
        assert_eq!((pasted.scope(), pasted.text()), (Scope::Windows, "foot"));
        // In the window query, `@` is a plain character; so is the files trigger.
        edit(&mut pasted, t("@'"));
        assert_eq!((pasted.scope(), pasted.text()), (Scope::Windows, "foot@'"));
        // Mid-query, or after anything, it is a character of the app query.
        let mut apps = Query::default();
        edit(&mut apps, t("a@"));
        assert_eq!((apps.scope(), apps.text()), (Scope::Apps, "a@"));
        let mut apps = Query::default();
        edit(&mut apps, t(" "));
        edit(&mut apps, t("@"));
        assert_eq!((apps.scope(), apps.text()), (Scope::Apps, " @"));
        // The files trigger still works beside it.
        let mut files = Query::default();
        edit(&mut files, t("'x"));
        assert_eq!((files.scope(), files.text()), (Scope::Files, "x"));
        // Without a trigger (`trigger = ""`), or with a space, there is no search.
        let mut off = Query::default();
        off.edit_in(&t("@x"), Some('\''), None);
        assert_eq!((off.scope(), off.text()), (Scope::Apps, "@x"));
        let mut space = Query::default();
        space.edit_in(&t(" x"), None, Some(' '));
        assert_eq!((space.scope(), space.text()), (Scope::Apps, " x"));
        // `edit` has no window search.
        let mut plain = Query::default();
        plain.edit(&t("@x"), Some('\''));
        assert_eq!((plain.scope(), plain.text()), (Scope::Apps, "@x"));
        // The same character for both: files win.
        let mut both = Query::default();
        both.edit_in(&t("@x"), Some('@'), Some('@'));
        assert_eq!(both.scope(), Scope::Files);
    }

    #[test]
    fn the_way_back_from_windows() {
        let t = |s: &str| Key::Text(s.into());
        let edit = |q: &mut Query, key: Key| q.edit_in(&key, Some('\''), Some('@'));
        // Backspace and Ctrl+W on an empty query return to the apps.
        let mut q = Query::default();
        edit(&mut q, t("@a"));
        assert!(edit(&mut q, Key::Backspace) && q.scope() == Scope::Windows);
        assert!(edit(&mut q, Key::Backspace));
        assert_eq!((q.scope(), q.text()), (Scope::Apps, ""));
        edit(&mut q, t("@ab"));
        assert!(edit(&mut q, Key::DeleteWord) && q.scope() == Scope::Windows);
        assert!(edit(&mut q, Key::DeleteWord) && q.scope() == Scope::Apps);
        // Shift+Tab at any time, emptying the query.
        edit(&mut q, t("@abc"));
        assert!(edit(&mut q, Key::Back));
        assert_eq!((q.scope(), q.text()), (Scope::Apps, ""));
        // Escape clears the query, then closes (as for files).
        edit(&mut q, t("@abc"));
        assert_eq!(act(Key::Escape, 0, &q), Act::Clear);
        assert_eq!(act(Key::Back, 0, &q), Act::Edit(Key::Back));
        edit(&mut q, Key::Clear);
        assert_eq!(act(Key::Escape, 0, &q), Act::Close);
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

    #[test]
    fn source_triggers() {
        let t = |s: &str| Key::Text(s.into());
        let mut triggers = Triggers::new(Some('\''), Some('@'));
        assert!(triggers.add('>', Scope::Source(1)));
        assert!(triggers.add('=', Scope::Source(2)));
        // Taken (by files, or by an earlier source), or a space: no.
        assert!(!triggers.add('\'', Scope::Source(3)));
        assert!(!triggers.add('>', Scope::Source(3)));
        assert!(!triggers.add(' ', Scope::Source(3)));
        let edit = |q: &mut Query, key: Key| q.edit_with(&key, &triggers);
        // `>web` searches the first source for `web`, typed or pasted.
        let mut q = Query::default();
        assert!(edit(&mut q, t(">")));
        assert_eq!((q.scope(), q.text()), (Scope::Source(1), ""));
        edit(&mut q, t("web"));
        assert_eq!((q.scope(), q.text()), (Scope::Source(1), "web"));
        let mut pasted = Query::default();
        edit(&mut pasted, t("=2*21"));
        assert_eq!((pasted.scope(), pasted.text()), (Scope::Source(2), "2*21"));
        // In its query, the triggers are plain characters.
        edit(&mut pasted, t(">'@"));
        assert_eq!(pasted.text(), "2*21>'@");
        // Mid-query it is part of the app query.
        let mut apps = Query::default();
        edit(&mut apps, t("a>"));
        assert_eq!((apps.scope(), apps.text()), (Scope::Apps, "a>"));
        // The ways back, as for files.
        assert!(edit(&mut q, Key::DeleteWord) && q.scope() == Scope::Source(1));
        assert!(edit(&mut q, Key::Backspace));
        assert_eq!((q.scope(), q.text()), (Scope::Apps, ""));
        edit(&mut q, t(">abc"));
        assert!(edit(&mut q, Key::Back));
        assert_eq!(q.scope(), Scope::Apps);
        edit(&mut q, t(">abc"));
        assert_eq!(act(Key::Escape, 0, &q), Act::Clear);
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
