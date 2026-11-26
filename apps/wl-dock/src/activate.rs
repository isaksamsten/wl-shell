//! Keyboard activation: the rules. Pure, unit tested; app.rs runs them
//! with the activate surface's keyboard, and view.rs shows the numbers.
//!
//! Holding Super, the compositor binds the number keys to `wl-dock
//! --activate <n>`. The first ten dock items are numbered 1–9 and 0, in
//! dock order (the apps, then the folders); standalone widgets, the
//! Trash and the separator have none. A press of n acts on item n:
//!
//! | item n               | first press                | pressing n again           |
//! | -------------------- | -------------------------- | -------------------------- |
//! | a window to activate | magnify it; nothing yet    | nothing (it is picked)     |
//! | windows for a strip  | its strip, without a grab  | the next window, MRU order |
//! | not running          | launch it                  | nothing (it is launching)  |
//! | a folder             | its popup, first entry lit | the next entry             |
//!
//! Shift+n steps back. Another number closes what is open and acts on its
//! item. The mode holds the keyboard (the activate surface) for as long
//! as the modifiers held when it got the keyboard stay down; letting go
//! activates the highlighted window, or opens the highlighted entry. If
//! none was held by then (a quick tap), an open strip or popup stays,
//! with the keyboard, and nothing activates by itself; holding a modifier
//! again re-arms the release.

use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

use crate::dock::{Item, Kind};
use crate::strip::Arrow;

/// Items numbered at most: 1–9, then 0.
pub const MAX: usize = 10;

/// linux/input-event-codes.h: `KEY_ESC`.
const KEY_ESC: u32 = 1;

/// linux/input-event-codes.h: `KEY_1` … `KEY_9`, `KEY_0`.
const KEY_1: u32 = 2;
const KEY_0: u32 = 11;

/// The keys of the numbered items, in order: item n is `[n - 1]`. Apps
/// and folders count; widgets, the separator and the Trash don't.
pub fn numbered<'a>(items: impl IntoIterator<Item = &'a Item>) -> Vec<String> {
    (items.into_iter())
        .filter(|i| matches!(i.kind, Kind::App | Kind::Folder { .. }))
        .take(MAX)
        .map(|i| i.app_id.clone())
        .collect()
}

/// The digit item `n` (from 1) shows: `1`–`9`, `0` for the tenth.
pub fn label(n: usize) -> String {
    (n % 10).to_string()
}

/// A key while the mode has the keyboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A number key: item n (from 1), `back` with Shift.
    Number {
        n: usize,
        back: bool,
    },
    /// Activate (or open) the highlighted one now.
    Enter,
    /// End the mode without activating.
    Escape,
    Arrow(Arrow),
}

impl Key {
    /// The key with evdev code `code` and keysym `sym`, with Shift held
    /// or not. Numbers go by code, so Shift and the layout don't matter.
    pub fn from(code: u32, sym: Keysym, shift: bool) -> Option<Self> {
        if (KEY_1..=KEY_0).contains(&code) {
            let n = (code - KEY_1 + 1) as usize;
            return Some(Key::Number { n, back: shift });
        }
        if code == KEY_ESC {
            return Some(Key::Escape);
        }
        Some(match sym {
            Keysym::Escape => Key::Escape,
            Keysym::Return | Keysym::KP_Enter | Keysym::space => Key::Enter,
            Keysym::Left => Key::Arrow(Arrow::Left),
            Keysym::Right => Key::Arrow(Arrow::Right),
            Keysym::Up => Key::Arrow(Arrow::Up),
            Keysym::Down => Key::Arrow(Arrow::Down),
            _ => return None,
        })
    }

    /// Whether holding it repeats.
    pub fn repeats(self) -> bool {
        matches!(self, Key::Arrow(_))
    }
}

/// What an item is to a press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    /// An app whose window a click activates (one window here, too few
    /// for a strip, or windows only on other outputs).
    Window,
    /// An app with a strip.
    Windows,
    /// An app without windows.
    NotRunning,
    /// A folder that exists.
    Folder,
    /// Nothing to do (no such item, or a missing folder).
    Nothing,
}

impl Subject {
    /// What `item` is, if it gets a strip (`strip`) or not.
    pub fn of(item: &Item, strip: bool) -> Self {
        match item.kind {
            Kind::App if !item.windows.is_empty() && strip => Subject::Windows,
            Kind::App if !item.windows.is_empty() || !item.elsewhere.is_empty() => Subject::Window,
            Kind::App => Subject::NotRunning,
            Kind::Folder { exists: true } => Subject::Folder,
            Kind::Folder { exists: false }
            | Kind::Separator
            | Kind::Trash { .. }
            | Kind::Widget { .. } => Subject::Nothing,
        }
    }
}

/// What a press does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// Pick the item's most recent window, activated once the modifiers
    /// are let go.
    Select,
    /// Open its strip, on the app's previous window.
    Strip,
    /// Open its popup, with the first entry highlighted.
    Popup,
    /// Launch it.
    Launch,
    /// Highlight the next card or entry (the previous one, `back`).
    Step {
        back: bool,
    },
    Nothing,
}

/// What is open in the mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Open {
    #[default]
    Nothing,
    Strip,
    /// An app's window, picked by `Press::Select`.
    Window,
    Popup,
}

/// What the mode does now, by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// Activate the highlighted card's window; the mode ends.
    Activate,
    /// Activate the picked app's window (`Press::Select`); the mode ends.
    Window,
    /// Open the highlighted entry; the mode ends.
    Open,
    /// The mode ends.
    End,
}

/// The modifiers that hold the mode: Shift only steps back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub alt: bool,
    pub ctrl: bool,
    pub logo: bool,
}

impl Mods {
    pub fn from(m: &Modifiers) -> Self {
        Self {
            alt: m.alt,
            ctrl: m.ctrl,
            logo: m.logo,
        }
    }

    fn any(self) -> bool {
        self.alt || self.ctrl || self.logo
    }

    /// Whether any of `held` is still down in `self`.
    fn holds_any_of(self, held: Self) -> bool {
        (held.alt && self.alt) || (held.ctrl && self.ctrl) || (held.logo && self.logo)
    }
}

/// The modifiers' side of the mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Hold {
    /// No modifiers seen yet (the keyboard isn't there yet).
    #[default]
    Waiting,
    /// These were held when the keyboard came, and still are.
    Holding(Mods),
    /// None was held when the keyboard came.
    Tapped,
    /// They were let go.
    Released,
    /// A tap left something open, which stays until a key closes it.
    Kept,
}

/// The mode's state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mode {
    hold: Hold,
    /// The first press (or, without a number, the badges) is done.
    started: bool,
    /// The item last pressed (its number).
    item: Option<usize>,
    open: Open,
}

impl Mode {
    /// What is open.
    pub fn open(&self) -> Open {
        self.open
    }

    /// A tap left something open, to be used with the keys: it shows
    /// without waiting for the delay.
    pub fn kept(&self) -> bool {
        self.hold == Hold::Kept
    }

    /// The press of item `n` (what it is: `subject`), back with Shift.
    pub fn press(&mut self, n: usize, subject: Subject, back: bool) -> Press {
        let again = self.item == Some(n);
        let press = match subject {
            Subject::Nothing => return Press::Nothing,
            Subject::Window if again && self.open == Open::Window => Press::Nothing,
            Subject::Window => Press::Select,
            Subject::Windows if again && self.open == Open::Strip => Press::Step { back },
            Subject::Windows => Press::Strip,
            Subject::NotRunning if again => Press::Nothing,
            Subject::NotRunning => Press::Launch,
            Subject::Folder if again && self.open == Open::Popup => Press::Step { back },
            Subject::Folder => Press::Popup,
        };
        self.item = Some(n);
        self.open = match press {
            Press::Strip => Open::Strip,
            Press::Popup => Open::Popup,
            Press::Select => Open::Window,
            Press::Step { .. } => self.open,
            // Pressed again: still picked.
            Press::Nothing if again => self.open,
            Press::Launch | Press::Nothing => Open::Nothing,
        };
        press
    }

    /// What was open closed (Escape on it, or its app went).
    pub fn closed(&mut self) {
        self.open = Open::Nothing;
    }

    /// The first press is done (or there was none to do).
    pub fn start(&mut self) {
        self.started = true;
    }

    /// The modifiers are now `mods`.
    pub fn modifiers(&mut self, mods: Mods) {
        self.hold = match self.hold {
            Hold::Waiting if mods.any() => Hold::Holding(mods),
            Hold::Waiting => Hold::Tapped,
            Hold::Holding(held) if !mods.holds_any_of(held) => Hold::Released,
            // Holding one again after a tap: letting go acts again.
            Hold::Kept if mods.any() => Hold::Holding(mods),
            hold => hold,
        };
    }

    /// What Enter does: as letting go would.
    pub fn enter(&self) -> Release {
        match self.open {
            Open::Strip => Release::Activate,
            Open::Window => Release::Window,
            Open::Popup => Release::Open,
            Open::Nothing => Release::End,
        }
    }

    /// What the mode does by itself now, if anything: once the first
    /// press is done, letting go acts, and a tap leaves an open strip or
    /// popup, but no mode without one; a tap on a window activates it.
    pub fn due(&mut self) -> Option<Release> {
        if !self.started {
            return None;
        }
        match self.hold {
            Hold::Waiting | Hold::Holding(_) => None,
            Hold::Released => Some(self.enter()),
            Hold::Tapped | Hold::Kept => match self.open {
                Open::Nothing => Some(Release::End),
                Open::Window => Some(Release::Window),
                _ => {
                    self.hold = Hold::Kept;
                    None
                }
            },
        }
    }
}

/// The window after `current` in `order` (the next more recent, `back`),
/// wrapping; the first without a current one.
pub fn step<'a>(order: &[&'a str], current: Option<&str>, back: bool) -> Option<&'a str> {
    let n = order.len();
    let at = current.and_then(|c| order.iter().position(|id| *id == c));
    let i = match (at, back) {
        (None, _) => 0,
        (Some(i), false) => (i + 1) % n.max(1),
        (Some(i), true) => (i + n.max(1) - 1) % n.max(1),
    };
    order.get(i).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::{self, Span, Window};
    use crate::strip;
    use wl_shell_core::mru::Mru;

    fn app(id: &str, windows: usize) -> Item {
        Item {
            app_id: id.into(),
            kind: Kind::App,
            pinned: true,
            windows: (0..windows)
                .map(|i| Window {
                    id: format!("{id}{i}"),
                    title: String::new(),
                })
                .collect(),
            elsewhere: Vec::new(),
            active: false,
            span: Span::Normal,
        }
    }

    fn other(kind: Kind, key: &str) -> Item {
        Item {
            app_id: key.into(),
            kind,
            ..app("", 0)
        }
    }

    const SUPER: Mods = Mods {
        logo: true,
        alt: false,
        ctrl: false,
    };
    const NONE: Mods = Mods {
        logo: false,
        alt: false,
        ctrl: false,
    };

    #[test]
    fn numbering() {
        let mut items: Vec<Item> = (0..3).map(|i| app(&format!("a{i}"), 1)).collect();
        items.push(other(Kind::Separator, dock::SEPARATOR));
        items.push(other(
            Kind::Folder { exists: true },
            &dock::folder_key("~/x"),
        ));
        items.push(other(Kind::Trash { full: false }, dock::TRASH));
        let n = numbered(&items);
        assert_eq!(n, ["a0", "a1", "a2", &dock::folder_key("~/x")]);
        // Ten at most; the tenth shows 0.
        let many: Vec<Item> = (0..12).map(|i| app(&format!("a{i}"), 0)).collect();
        let n = numbered(&many);
        assert_eq!((n.len(), n[9].as_str()), (10, "a9"));
        assert_eq!(
            (label(1), label(9), label(10)),
            ("1".into(), "9".into(), "0".into())
        );
    }

    #[test]
    fn keys() {
        let k = |code, sym, shift| Key::from(code, sym, shift);
        assert_eq!(
            k(2, Keysym::_1, false),
            Some(Key::Number { n: 1, back: false })
        );
        // Shift+2 is `@` on many layouts: the code counts.
        assert_eq!(
            k(3, Keysym::at, true),
            Some(Key::Number { n: 2, back: true })
        );
        assert_eq!(
            k(11, Keysym::_0, false),
            Some(Key::Number { n: 10, back: false })
        );
        assert_eq!(k(1, Keysym::Escape, false), Some(Key::Escape));
        // With Super (or anything) held, by its code.
        assert_eq!(k(1, Keysym::NoSymbol, true), Some(Key::Escape));
        assert_eq!(k(28, Keysym::Return, false), Some(Key::Enter));
        assert_eq!(k(103, Keysym::Up, false), Some(Key::Arrow(Arrow::Up)));
        assert_eq!(k(30, Keysym::a, false), None);
        assert!(Key::Arrow(Arrow::Left).repeats() && !Key::Enter.repeats());
    }

    #[test]
    fn subjects() {
        assert_eq!(Subject::of(&app("a", 1), false), Subject::Window);
        assert_eq!(Subject::of(&app("a", 3), true), Subject::Windows);
        // Strips off, or too few windows: the most recent one.
        assert_eq!(Subject::of(&app("a", 3), false), Subject::Window);
        assert_eq!(Subject::of(&app("a", 0), true), Subject::NotRunning);
        let mut away = app("a", 0);
        away.elsewhere = app("b", 1).windows;
        assert_eq!(Subject::of(&away, true), Subject::Window);
        let folder = |exists| other(Kind::Folder { exists }, "f");
        assert_eq!(Subject::of(&folder(true), true), Subject::Folder);
        assert_eq!(Subject::of(&folder(false), true), Subject::Nothing);
        assert_eq!(
            Subject::of(&other(Kind::Trash { full: true }, dock::TRASH), true),
            Subject::Nothing
        );
    }

    #[test]
    fn presses() {
        let mut m = Mode::default();
        assert_eq!(m.press(1, Subject::Window, false), Press::Select);
        assert_eq!(m.open(), Open::Window);
        // Again: still picked.
        assert_eq!(m.press(1, Subject::Window, false), Press::Nothing);
        assert_eq!(m.open(), Open::Window);
        // Another item instead.
        assert_eq!(m.press(2, Subject::Windows, false), Press::Strip);
        assert_eq!(m.press(1, Subject::Window, false), Press::Select);

        let mut m = Mode::default();
        assert_eq!(m.press(2, Subject::Windows, false), Press::Strip);
        assert_eq!(
            m.press(2, Subject::Windows, false),
            Press::Step { back: false }
        );
        assert_eq!(
            m.press(2, Subject::Windows, true),
            Press::Step { back: true }
        );
        assert_eq!(m.open(), Open::Strip);
        // Another number: its item instead.
        assert_eq!(m.press(3, Subject::Folder, false), Press::Popup);
        assert_eq!(m.open(), Open::Popup);
        assert_eq!(
            m.press(3, Subject::Folder, false),
            Press::Step { back: false }
        );
        assert_eq!(m.press(4, Subject::NotRunning, false), Press::Launch);
        assert_eq!(m.open(), Open::Nothing);
        // It is launching.
        assert_eq!(m.press(4, Subject::NotRunning, false), Press::Nothing);
        // No such item: nothing changes.
        assert_eq!(m.press(2, Subject::Windows, false), Press::Strip);
        assert_eq!(m.press(9, Subject::Nothing, false), Press::Nothing);
        assert_eq!(m.open(), Open::Strip);
        // The strip closed by itself: a press opens it again.
        m.closed();
        assert_eq!(m.press(2, Subject::Windows, false), Press::Strip);
    }

    #[test]
    fn letting_go_activates() {
        let mut m = Mode::default();
        m.press(2, Subject::Windows, false);
        m.start();
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        // Another event with Super still held (Shift isn't tracked).
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Activate));

        let mut m = Mode::default();
        m.press(3, Subject::Folder, false);
        m.start();
        m.modifiers(SUPER);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Open));

        // A launch: the mode ends on letting go.
        let mut m = Mode::default();
        m.press(4, Subject::NotRunning, false);
        m.start();
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::End));
    }

    #[test]
    fn a_single_window_waits_for_letting_go() {
        let mut m = Mode::default();
        m.press(1, Subject::Window, false);
        m.start();
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Window));
        assert_eq!(m.enter(), Release::Window);

        // A quick tap: nothing to keep open, so it activates.
        let mut m = Mode::default();
        m.press(1, Subject::Window, false);
        m.start();
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Window));
    }

    #[test]
    fn every_held_modifier_counts() {
        let mut m = Mode::default();
        m.press(2, Subject::Windows, false);
        m.start();
        let both = Mods { alt: true, ..SUPER };
        m.modifiers(both);
        m.modifiers(Mods {
            logo: false,
            ..both
        });
        assert_eq!(m.due(), None);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Activate));
    }

    #[test]
    fn a_release_before_the_first_press_waits_for_it() {
        // The keyboard came (and Super was let go) before the dock knew
        // where it is.
        let mut m = Mode::default();
        m.modifiers(SUPER);
        m.modifiers(NONE);
        assert_eq!(m.due(), None);
        m.press(2, Subject::Windows, false);
        m.start();
        assert_eq!(m.due(), Some(Release::Activate));
    }

    #[test]
    fn a_quick_tap_leaves_a_usable_strip() {
        let mut m = Mode::default();
        m.press(2, Subject::Windows, false);
        m.start();
        // Super was up by the time the keyboard came.
        m.modifiers(NONE);
        assert!(!m.kept());
        assert_eq!(m.due(), None);
        assert!(m.kept());
        assert_eq!(m.due(), None);
        // The keys still work on it.
        assert_eq!(
            m.press(2, Subject::Windows, false),
            Press::Step { back: false }
        );
        assert_eq!(m.enter(), Release::Activate);
        // Holding Super again re-arms letting go.
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Activate));

        // Nothing left open after a tap: the mode ends.
        let mut m = Mode::default();
        m.press(4, Subject::NotRunning, false);
        m.start();
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::End));
        // Kept, then a press that opens nothing: it ends too.
        let mut m = Mode::default();
        m.press(2, Subject::Windows, false);
        m.start();
        m.modifiers(NONE);
        assert_eq!(m.due(), None);
        m.press(4, Subject::NotRunning, false);
        assert_eq!(m.due(), Some(Release::End));
    }

    #[test]
    fn badges_only() {
        // `--activate` without a number: the badges, until let go.
        let mut m = Mode::default();
        m.start();
        m.modifiers(SUPER);
        assert_eq!(m.due(), None);
        assert_eq!(m.press(1, Subject::Windows, false), Press::Strip);
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Activate));
    }

    #[test]
    fn steps_wrap_both_ways() {
        let order = ["a", "b", "c"];
        assert_eq!(step(&order, None, false), Some("a"));
        assert_eq!(step(&order, Some("a"), false), Some("b"));
        assert_eq!(step(&order, Some("c"), false), Some("a"));
        assert_eq!(step(&order, Some("a"), true), Some("c"));
        assert_eq!(step(&order, Some("gone"), false), Some("a"));
        assert_eq!(step(&[], Some("a"), false), None);
    }

    /// The exit criterion: holding Super and tapping 2, 2, 2 lands on the
    /// second item's fourth most recent window, the focused one first.
    #[test]
    fn two_two_two_lands_on_the_fourth_most_recent() {
        let items = [app("a", 1), app("b", 5)];
        let key = &numbered(&items)[1];
        let item = items.iter().find(|i| i.app_id == *key).unwrap();
        // Focused last: b3, then b1, b4, b0; b2 never.
        let mut mru = Mru::default();
        for id in ["b2x", "b0", "b4", "b1", "b3"] {
            mru.focus(id);
        }
        let ids: Vec<&str> = item.windows.iter().map(|w| w.id.as_str()).collect();
        let mut order = ids.clone();
        mru.sort(&mut order, |id| id);
        assert_eq!(order, ["b3", "b1", "b4", "b0", "b2"]);

        let mut m = Mode::default();
        let subject = Subject::of(item, true);
        assert_eq!(m.press(2, subject, false), Press::Strip);
        m.start();
        m.modifiers(SUPER);
        let first = strip::first_highlight(&ids, &mru, Some("b3"));
        let mut lit = Some(ids[first]);
        assert_eq!(lit, Some("b1"));
        for _ in 0..2 {
            assert_eq!(m.press(2, subject, false), Press::Step { back: false });
            lit = step(&order, lit, false);
        }
        m.modifiers(NONE);
        assert_eq!(m.due(), Some(Release::Activate));
        assert_eq!(lit, Some("b0"));
        // Shift+2 goes back.
        assert_eq!(step(&order, lit, true), Some("b4"));
    }
}
