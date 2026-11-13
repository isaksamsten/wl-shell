//! Keys and modifiers: what they do in the switcher. Pure, unit tested;
//! the shell handles the seat and passes the events on (see app.rs).
//!
//! Once open, the switcher has the keyboard and handles every key itself
//! while the modifier is held:
//!
//! - Tab / Shift+Tab (ISO_Left_Tab): next / previous app.
//! - The grave key / with Shift: next / previous window. It is matched by
//!   evdev keycode (`KEY_GRAVE`), not keysym, so it works on any layout and
//!   with Shift held.
//! - Left / Right: previous / next app; Up / Down: previous / next window.
//! - Escape cancels, Return picks.
//!
//! Releasing the modifier picks (see `Hold`). The pointer selects what it
//! moves over (an icon, or a card in the strip); a click picks it, a click
//! outside the bar or strip cancels.

use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

use crate::config::Modifier;
use crate::switcher::Step;

/// linux/input-event-codes.h
const KEY_GRAVE: u32 = 41;

/// The modifiers that matter for release-to-pick.
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

    /// Those of `self` that `which` cares about.
    fn only(self, which: Modifier) -> Self {
        let none = Self::default();
        match which {
            Modifier::Auto => self,
            Modifier::Alt => Self {
                alt: self.alt,
                ..none
            },
            Modifier::Ctrl => Self {
                ctrl: self.ctrl,
                ..none
            },
            Modifier::Super => Self {
                logo: self.logo,
                ..none
            },
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

/// Release-to-pick. On keyboard enter, the next `modifiers` event says
/// which (non-Shift) modifiers are held; once all of them are released, it
/// is time to pick. If none are held when focus arrives (a very fast tap),
/// that is right away.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hold {
    /// No focus, or picked already.
    #[default]
    Idle,
    /// Focus arrived; waiting for the modifier state.
    Entered,
    Holding(Mods),
}

impl Hold {
    pub fn enter(&mut self) {
        *self = Hold::Entered;
    }

    pub fn leave(&mut self) {
        *self = Hold::Idle;
    }

    /// The modifiers are now `mods`. True: released, pick.
    pub fn modifiers(&mut self, mods: Mods, which: Modifier) -> bool {
        match *self {
            Hold::Idle => false,
            Hold::Entered => {
                let held = mods.only(which);
                *self = if held.any() {
                    Hold::Holding(held)
                } else {
                    Hold::Idle
                };
                !held.any()
            }
            Hold::Holding(held) => {
                let released = !mods.holds_any_of(held);
                if released {
                    *self = Hold::Idle;
                }
                released
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Cancel,
    Pick,
    Step(Step),
}

impl Action {
    /// `raw`: the evdev keycode.
    pub fn from_key(sym: Keysym, raw: u32, shift: bool) -> Option<Self> {
        // Shift reverses Tab and the grave key.
        let next = !shift;
        let app = |forward| {
            Action::Step(if forward {
                Step::NextApp
            } else {
                Step::PrevApp
            })
        };
        let window = |forward| {
            Action::Step(if forward {
                Step::NextWindow
            } else {
                Step::PrevWindow
            })
        };
        if raw == KEY_GRAVE {
            return Some(window(next));
        }
        Some(match sym {
            Keysym::Escape => Action::Cancel,
            Keysym::Return | Keysym::KP_Enter => Action::Pick,
            Keysym::Tab => app(next),
            Keysym::ISO_Left_Tab => app(false),
            Keysym::Right => app(true),
            Keysym::Left => app(false),
            Keysym::Down => window(true),
            Keysym::Up => window(false),
            _ => return None,
        })
    }

    /// Stepping repeats while the key is held; the rest doesn't.
    pub fn repeats(self) -> bool {
        matches!(self, Action::Step(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALT: Mods = Mods {
        alt: true,
        ctrl: false,
        logo: false,
    };
    const NONE: Mods = Mods {
        alt: false,
        ctrl: false,
        logo: false,
    };

    #[test]
    fn release_picks() {
        let mut h = Hold::default();
        // Before focus, modifier changes mean nothing.
        assert!(!h.modifiers(NONE, Modifier::Auto));
        h.enter();
        assert!(!h.modifiers(ALT, Modifier::Auto));
        assert_eq!(h, Hold::Holding(ALT));
        // Shift (not tracked) and other changes while held: no pick.
        assert!(!h.modifiers(ALT, Modifier::Auto));
        assert!(h.modifiers(NONE, Modifier::Auto));
        // Once only.
        assert!(!h.modifiers(NONE, Modifier::Auto));
    }

    #[test]
    fn quick_tap_picks_on_focus() {
        let mut h = Hold::default();
        h.enter();
        assert!(h.modifiers(NONE, Modifier::Auto));
    }

    #[test]
    fn all_held_modifiers_must_be_released() {
        let mut h = Hold::default();
        h.enter();
        let both = Mods { ctrl: true, ..ALT };
        assert!(!h.modifiers(both, Modifier::Auto));
        assert!(!h.modifiers(ALT, Modifier::Auto));
        assert!(h.modifiers(NONE, Modifier::Auto));
    }

    #[test]
    fn configured_modifier_only() {
        let mut h = Hold::default();
        h.enter();
        let both = Mods { logo: true, ..ALT };
        assert!(!h.modifiers(both, Modifier::Super));
        // Alt is not the configured one: still holding Super.
        assert!(!h.modifiers(Mods { alt: false, ..both }, Modifier::Super));
        assert!(h.modifiers(ALT, Modifier::Super));
        // Super configured but not held when focus arrives: pick.
        h.enter();
        assert!(h.modifiers(ALT, Modifier::Super));
    }

    #[test]
    fn keys() {
        let k = |sym, shift| Action::from_key(sym, 0, shift);
        assert_eq!(k(Keysym::Escape, false), Some(Action::Cancel));
        assert_eq!(k(Keysym::Return, false), Some(Action::Pick));
        assert_eq!(k(Keysym::Tab, false), Some(Action::Step(Step::NextApp)));
        assert_eq!(k(Keysym::Tab, true), Some(Action::Step(Step::PrevApp)));
        assert_eq!(
            k(Keysym::ISO_Left_Tab, true),
            Some(Action::Step(Step::PrevApp))
        );
        assert_eq!(k(Keysym::Left, false), Some(Action::Step(Step::PrevApp)));
        assert_eq!(k(Keysym::Down, false), Some(Action::Step(Step::NextWindow)));
        assert_eq!(k(Keysym::a, false), None);
        // The grave key by keycode, whatever the layout maps it to.
        let grave = |sym, shift| Action::from_key(sym, KEY_GRAVE, shift);
        assert_eq!(
            grave(Keysym::grave, false),
            Some(Action::Step(Step::NextWindow))
        );
        assert_eq!(
            grave(Keysym::asciitilde, true),
            Some(Action::Step(Step::PrevWindow))
        );
        assert_eq!(
            grave(Keysym::section, false),
            Some(Action::Step(Step::NextWindow))
        );
        assert!(Action::Step(Step::NextApp).repeats() && !Action::Pick.repeats());
    }
}
