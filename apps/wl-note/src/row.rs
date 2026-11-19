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
    Templates,
    Hidden,
    Due,
}

impl Button {
    pub const ALL: [Self; 7] = [
        Self::Undo,
        Self::Window,
        Self::Global,
        Self::Paste,
        Self::Templates,
        Self::Hidden,
        Self::Due,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Window => "Window",
            Self::Global => "Global",
            Self::Paste => "Paste",
            Self::Templates => "Templates",
            Self::Hidden => "Hidden",
            Self::Due => "Due",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Undo => "builtin:note-undo",
            Self::Window => "builtin:note-window",
            Self::Global => "builtin:note-global",
            Self::Paste => "builtin:note-paste",
            Self::Templates => "builtin:note-templates",
            Self::Hidden => "builtin:note-hidden",
            Self::Due => "builtin:clock",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Available {
    pub undo: bool,
    pub window: bool,
    pub paste: bool,
    /// How many templates there are.
    pub templates: usize,
    /// The notes hidden from this context, and those hidden anywhere.
    pub hidden_here: usize,
    pub hidden_all: usize,
    /// The notes with a `due`, and those of them overdue.
    pub due_all: usize,
    pub due_overdue: usize,
}

/// The count a button carries: `9+` past nine, so it stays within the
/// button. None: it has none.
pub fn count(button: Button, state: Available) -> Option<String> {
    let n = match button {
        Button::Hidden => state.hidden_here,
        Button::Due => state.due_overdue,
        _ => 0,
    };
    match n {
        0 => None,
        1..=9 => Some(n.to_string()),
        _ => Some("9+".into()),
    }
}

/// What a button says it does on hover.
pub fn label(button: Button, state: Available) -> String {
    match (button, state.hidden_here, state.hidden_all) {
        (Button::Hidden, 0, all) => return format!("Hidden notes ({all} elsewhere)"),
        (Button::Hidden, here, _) => return format!("Hidden notes ({here} here)"),
        _ => {}
    }
    match (button, state.due_overdue, state.due_all) {
        (Button::Due, 0, all) => format!("Due notes ({all})"),
        (Button::Due, over, _) => format!("Due notes ({over} overdue)"),
        (Button::Templates, ..) => "New from a template".into(),
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
            Button::Templates => state.templates > 0 && mode == Mode::Rest,
            Button::Hidden => state.hidden_all > 0 && mode == Mode::Rest,
            Button::Due => state.due_all > 0 && mode == Mode::Rest,
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
        assert_eq!(count(Hidden, some), None);
        assert_eq!(label(Hidden, some), "Hidden notes (2 elsewhere)");
    }

    #[test]
    fn the_hidden_count_stays_in_the_button() {
        let with = |n| Available {
            hidden_here: n,
            hidden_all: n,
            ..Available::default()
        };
        assert_eq!(count(Hidden, with(3)).as_deref(), Some("3"));
        assert_eq!(count(Hidden, with(9)).as_deref(), Some("9"));
        assert_eq!(count(Hidden, with(10)).as_deref(), Some("9+"));
        assert_eq!(count(Hidden, with(300)).as_deref(), Some("9+"));
        assert_eq!(label(Hidden, with(12)), "Hidden notes (12 here)");
        // Other buttons never carry it.
        assert_eq!(count(Paste, with(3)), None);
        assert_eq!(label(Paste, with(3)), "Paste");
    }

    #[test]
    fn templates_and_due_show_at_rest_when_there_is_something() {
        let order = [Templates, Due];
        let none = Available::default();
        for mode in [Mode::Closed, Mode::Drag, Mode::Rest] {
            assert!(buttons(&order, none, mode).is_empty());
        }
        let some = Available {
            templates: 2,
            due_all: 1,
            ..none
        };
        for mode in [Mode::Closed, Mode::Drag] {
            assert!(buttons(&order, some, mode).is_empty());
        }
        assert_eq!(buttons(&order, some, Mode::Rest), [Templates, Due]);
        assert_eq!(
            buttons(&[Due, Templates], some, Mode::Rest),
            [Due, Templates]
        );
        // Each on its own.
        let templates = Available { due_all: 0, ..some };
        assert_eq!(buttons(&order, templates, Mode::Rest), [Templates]);
        let due = Available {
            templates: 0,
            ..some
        };
        assert_eq!(buttons(&order, due, Mode::Rest), [Due]);
    }

    #[test]
    fn the_due_count_is_of_the_overdue_and_stays_in_the_button() {
        let with = |all, overdue| Available {
            due_all: all,
            due_overdue: overdue,
            ..Available::default()
        };
        // Nothing overdue: no count.
        assert_eq!(count(Due, with(4, 0)), None);
        assert_eq!(label(Due, with(4, 0)), "Due notes (4)");
        assert_eq!(count(Due, with(4, 2)).as_deref(), Some("2"));
        assert_eq!(count(Due, with(40, 9)).as_deref(), Some("9"));
        assert_eq!(count(Due, with(40, 10)).as_deref(), Some("9+"));
        assert_eq!(label(Due, with(4, 2)), "Due notes (2 overdue)");
        // The hidden count isn't the due one, and the templates have none.
        let state = Available {
            hidden_here: 3,
            templates: 5,
            ..with(4, 2)
        };
        assert_eq!(count(Hidden, state).as_deref(), Some("3"));
        assert_eq!(count(Templates, state), None);
        assert_eq!(label(Templates, state), "New from a template");
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
