//! Autohide: when a dock slides off its edge and comes back, and how far
//! it slides. Pure, unit tested.
//!
//! A dock with `autohide` is shown while the pointer is on it or a strip
//! or menu of it is open (`held`). Hidden, its input region is a thin
//! band along its edge (`autohide-trigger`): the pointer resting there
//! for `reveal-delay` brings it back. Shown, it hides `hide-delay` after
//! the pointer leaves, once nothing holds it. Without autohide it is
//! always shown. The view keeps one timer for the change `next` asks for
//! and applies it when it fires.

use std::time::Duration;

use wl_shell_render::shadow;

use crate::layout::Metrics;

/// A dock's autohide settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub enabled: bool,
    pub reveal_delay: Duration,
    pub hide_delay: Duration,
    /// The trigger band's thickness while hidden, logical pixels.
    pub trigger: f64,
}

/// Where a dock that is `shown` (or hidden) is headed, with the pointer
/// on it or not and a popup holding it open or not: Some(shown, after),
/// or None to stay as it is.
pub fn next(s: &Settings, shown: bool, pointer_in: bool, held: bool) -> Option<(bool, Duration)> {
    let want = !s.enabled || pointer_in || held;
    if want == shown {
        return None;
    }
    let delay = match (s.enabled, want) {
        // Turned off: back at once.
        (false, _) => Duration::ZERO,
        (true, true) => s.reveal_delay,
        (true, false) => s.hide_delay,
    };
    Some((want, delay))
}

/// How far the panel's shadow reaches past it.
pub fn shadow_reach(s: Option<&shadow::Settings>) -> f64 {
    s.map_or(0.0, |s| {
        let offset = s.offset.0.abs().max(s.offset.1.abs()) as f64;
        1.5 * s.softness.max(0.0) + s.spread.max(0) as f64 + offset
    })
}

/// How far the resting dock reaches from its edge, with the panel's
/// shadow reaching `shadow` past the panel: sliding it this far takes it
/// off the output.
pub fn reach(m: &Metrics, shadow: f64) -> f64 {
    (m.panel_v() + m.panel() + shadow).max(m.icon_v() + m.icon + m.icon_shadow)
}

/// How far toward its edge a dock `shown` (0: hidden, 1: shown) is slid,
/// for a dock that reaches `reach` from it.
pub fn slide(shown: f64, reach: f64) -> f64 {
    (1.0 - shown.clamp(0.0, 1.0)) * reach
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn on() -> Settings {
        Settings {
            enabled: true,
            reveal_delay: 150 * MS,
            hide_delay: 500 * MS,
            trigger: 2.0,
        }
    }

    #[test]
    fn without_autohide_the_dock_stays() {
        let s = Settings {
            enabled: false,
            ..on()
        };
        for (pointer_in, held) in [(false, false), (true, false), (false, true)] {
            assert_eq!(next(&s, true, pointer_in, held), None);
            // Turned off while hidden: back at once.
            assert_eq!(
                next(&s, false, pointer_in, held),
                Some((true, Duration::ZERO))
            );
        }
    }

    #[test]
    fn the_pointer_on_the_edge_reveals_after_the_delay() {
        assert_eq!(next(&on(), false, true, false), Some((true, 150 * MS)));
        // Leaving first: it stays hidden.
        assert_eq!(next(&on(), false, false, false), None);
    }

    #[test]
    fn leaving_hides_after_the_delay() {
        assert_eq!(next(&on(), true, false, false), Some((false, 500 * MS)));
        assert_eq!(next(&on(), true, true, false), None);
    }

    #[test]
    fn popups_hold_the_dock() {
        assert_eq!(next(&on(), true, false, true), None);
        assert_eq!(next(&on(), false, false, true), Some((true, 150 * MS)));
    }

    #[test]
    fn slides_the_whole_reach() {
        assert_eq!(slide(1.0, 80.0), 0.0);
        assert_eq!(slide(0.0, 80.0), 80.0);
        assert_eq!(slide(0.25, 80.0), 60.0);
        // Overshooting springs don't push it off its rest or past hidden.
        assert_eq!(slide(1.1, 80.0), 0.0);
        assert_eq!(slide(-0.1, 80.0), 80.0);
    }

    #[test]
    fn reach_covers_the_panel_and_its_shadow() {
        let m = Metrics {
            icon: 48.0,
            padding: 6.0,
            margin: 6.0,
            magnification: 2.0,
            label: 30.0,
            icon_shadow: 11.0,
        };
        // The icon's shadow reaches past the panel without a panel shadow.
        assert_eq!(reach(&m, 0.0), 12.0 + 48.0 + 11.0);
        let s = shadow::Settings {
            color: 0x00000066,
            softness: 24.0,
            spread: 0,
            offset: (0, 4),
        };
        assert_eq!(shadow_reach(Some(&s)), 40.0);
        assert_eq!(reach(&m, shadow_reach(Some(&s))), 6.0 + 60.0 + 40.0);
        assert_eq!(shadow_reach(None), 0.0);
    }
}
