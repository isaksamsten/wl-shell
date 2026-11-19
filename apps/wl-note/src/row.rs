//! The `+` row's available actions and geometry, independent of Wayland.

use std::time::Instant;

use serde::Deserialize;
use wl_shell_ui::Rect;

use crate::config::Edge;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Button {
    Undo,
    Window,
    Global,
    Paste,
    Hidden,
}

impl Button {
    pub const ALL: [Self; 5] = [
        Self::Undo,
        Self::Window,
        Self::Global,
        Self::Paste,
        Self::Hidden,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Window => "Window",
            Self::Global => "Global",
            Self::Paste => "Paste",
            Self::Hidden => "Hidden",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Undo => "↶",
            Self::Window => "▭",
            Self::Global => "◯",
            Self::Paste => "⎘",
            Self::Hidden => "≡",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Available {
    pub undo: bool,
    pub window: bool,
    pub paste: bool,
    /// The notes hidden from this context, and those hidden anywhere.
    pub hidden_here: usize,
    pub hidden_all: usize,
}

/// What a button shows: its glyph, with a count where it has one (`9+`
/// past nine, so it stays within the button).
pub fn glyph(button: Button, state: Available) -> String {
    if button != Button::Hidden || state.hidden_here == 0 {
        return button.glyph().into();
    }
    match state.hidden_here {
        n @ 1..=9 => format!("{}{n}", button.glyph()),
        _ => format!("{}9+", button.glyph()),
    }
}

/// What a button says it does on hover.
pub fn label(button: Button, state: Available) -> String {
    match (button, state.hidden_here, state.hidden_all) {
        (Button::Hidden, 0, all) => format!("Hidden notes ({all} elsewhere)"),
        (Button::Hidden, here, _) => format!("Hidden notes ({here} here)"),
        _ => button.label().into(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Rest,
    Drag,
    Closed,
}

pub fn buttons(order: &[Button], state: Available, mode: Mode) -> Vec<Button> {
    let mut result = Vec::new();
    for &button in order {
        let show = match button {
            Button::Undo => state.undo && mode != Mode::Drag,
            Button::Window => state.window && mode != Mode::Closed,
            Button::Global => mode != Mode::Closed,
            Button::Paste => state.paste && mode == Mode::Rest,
            Button::Hidden => state.hidden_all > 0 && mode == Mode::Rest,
        };
        if show && !result.contains(&button) {
            result.push(button);
        }
    }
    result
}

pub fn places(
    buttons: &[Button],
    plus: Rect,
    edge: Edge,
    width: f64,
    sheet: f64,
) -> impl Iterator<Item = (Button, Rect)> + '_ {
    let step = plus.w + 5.0;
    let sign = if edge == Edge::Left { 1.0 } else { -1.0 };
    let reach = sheet.min(width);
    let (left, right) = if edge == Edge::Left {
        (0.0, reach)
    } else {
        (width - reach, width)
    };
    buttons
        .iter()
        .copied()
        .enumerate()
        .map(move |(i, button)| {
            (
                button,
                Rect::new(
                    plus.x + sign * step * (i + 1) as f64,
                    plus.y,
                    plus.w,
                    plus.h,
                ),
            )
        })
        .take_while(move |(_, r)| r.x >= left && r.x + r.w <= right)
}

/// The last removal. Every way of giving up the slot returns the old action
/// to its owner, which commits a trash but has nothing to do for a hide.
pub struct Undo<T> {
    action: Option<(Instant, T)>,
}

impl<T> Undo<T> {
    pub fn new() -> Self {
        Self { action: None }
    }

    pub fn action(&self) -> Option<&T> {
        self.action.as_ref().map(|(_, action)| action)
    }

    pub fn action_mut(&mut self) -> Option<&mut T> {
        self.action.as_mut().map(|(_, action)| action)
    }

    pub fn replace(&mut self, until: Instant, action: T) -> Option<T> {
        self.action
            .replace((until, action))
            .map(|(_, action)| action)
    }

    pub fn take(&mut self) -> Option<T> {
        self.action.take().map(|(_, action)| action)
    }

    pub fn expire(&mut self, now: Instant) -> Option<T> {
        if self.action.as_ref().is_some_and(|(until, _)| now >= *until) {
            self.take()
        } else {
            None
        }
    }
}

pub fn paste_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.contains(|c: char| c.is_whitespace() || matches!(c, '<' | '>'))
        && url::Url::parse(trimmed)
            .is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "ftp" | "mailto"))
    {
        return Some(format!("<{trimmed}>"));
    }
    Some(text.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use Button::*;
    use std::time::Duration;

    #[test]
    fn available_actions_in_configured_order() {
        let order = [Paste, Window, Undo, Global, Window];
        for undo in [false, true] {
            for window in [false, true] {
                for paste in [false, true] {
                    let state = Available {
                        undo,
                        window,
                        paste,
                        ..Available::default()
                    };
                    assert_eq!(
                        buttons(&order, state, Mode::Closed),
                        undo.then_some(Undo).into_iter().collect::<Vec<_>>()
                    );
                    assert_eq!(
                        buttons(&order, state, Mode::Drag),
                        window
                            .then_some(Window)
                            .into_iter()
                            .chain([Global])
                            .collect::<Vec<_>>()
                    );
                    assert_eq!(
                        buttons(&order, state, Mode::Rest),
                        paste
                            .then_some(Paste)
                            .into_iter()
                            .chain(window.then_some(Window))
                            .chain(undo.then_some(Undo))
                            .chain([Global])
                            .collect::<Vec<_>>()
                    );
                }
            }
        }
        assert!(buttons(&[], Available::default(), Mode::Rest).is_empty());
    }

    #[test]
    fn hidden_button_is_for_rest_and_when_something_is_hidden() {
        let order = [Hidden, Paste];
        let none = Available::default();
        let some = Available {
            hidden_all: 2,
            ..none
        };
        for mode in [Mode::Closed, Mode::Drag, Mode::Rest] {
            assert!(!buttons(&order, none, mode).contains(&Hidden));
        }
        assert!(!buttons(&order, some, Mode::Closed).contains(&Hidden));
        assert!(!buttons(&order, some, Mode::Drag).contains(&Hidden));
        assert_eq!(buttons(&order, some, Mode::Rest), [Hidden]);
        // Hidden elsewhere only: still there, with no count.
        assert_eq!(glyph(Hidden, some), "≡");
        assert_eq!(label(Hidden, some), "Hidden notes (2 elsewhere)");
    }

    #[test]
    fn the_hidden_count_stays_in_the_button() {
        let with = |n| Available {
            hidden_here: n,
            hidden_all: n,
            ..Available::default()
        };
        assert_eq!(glyph(Hidden, with(3)), "≡3");
        assert_eq!(glyph(Hidden, with(9)), "≡9");
        assert_eq!(glyph(Hidden, with(10)), "≡9+");
        assert_eq!(glyph(Hidden, with(300)), "≡9+");
        assert_eq!(label(Hidden, with(12)), "Hidden notes (12 here)");
        // Other buttons never carry it.
        assert_eq!(glyph(Paste, with(3)), "⎘");
        assert_eq!(label(Paste, with(3)), "Paste");
    }

    #[test]
    fn fit_and_mirror() {
        let left = Rect::new(3.0, 8.0, 22.0, 22.0);
        let right = Rect::new(75.0, 8.0, 22.0, 22.0);
        for (width, sheet, count) in [
            (100.0, 51.0, 0),
            (100.0, 52.0, 1),
            (100.0, 79.0, 2),
            (100.0, 150.0, 2),
        ] {
            let a: Vec<_> = places(&Button::ALL, left, Edge::Left, width, sheet).collect();
            let b: Vec<_> = places(&Button::ALL, right, Edge::Right, width, sheet).collect();
            assert_eq!(a.len(), count);
            assert_eq!(b.len(), count);
            for ((button, l), (other, r)) in a.into_iter().zip(b) {
                assert_eq!(button, other);
                assert_eq!(l.x + r.x + l.w, width);
                assert_eq!(l.y, r.y);
            }
        }
    }

    #[test]
    fn removal_slot_expires_replaces_flushes_and_can_be_undone() {
        let now = Instant::now();
        let end = now + Duration::from_secs(5);
        let mut slot = super::Undo::new();
        assert_eq!(slot.replace(end, "first trash"), None);
        assert_eq!(slot.expire(now), None);
        assert_eq!(slot.expire(end), Some("first trash"));
        assert_eq!(slot.expire(end), None);
        assert_eq!(slot.replace(end, "second trash"), None);
        assert_eq!(slot.replace(end, "hide"), Some("second trash"));
        assert_eq!(slot.replace(end, "exit trash"), Some("hide"));
        assert_eq!(slot.take(), Some("exit trash"));
        slot.replace(end, "undone trash");
        assert_eq!(slot.take(), Some("undone trash"));
        assert_eq!(slot.expire(end), None);
    }

    #[test]
    fn paste_url_or_verbatim_text() {
        assert_eq!(
            paste_text(" https://example.com/path\n").as_deref(),
            Some("<https://example.com/path>")
        );
        assert_eq!(
            paste_text(" https://example.com with words\n").as_deref(),
            Some(" https://example.com with words\n")
        );
        assert_eq!(paste_text(" \n"), None);
    }
}
