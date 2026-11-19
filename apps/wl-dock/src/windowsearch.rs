//! The launcher's window search (the `@` trigger). Pure, unit tested;
//! app.rs feeds it the window list and launcherview.rs shows the result.
//!
//! - The candidates are every window of the toplevel list, in recency
//!   order (`recent`): the ones seen focused by when, then the rest in the
//!   list's order.
//! - `ranked` matches them by title, then by their app's name
//!   (`Search::rank_windows`). Ties keep the recency order. An empty query
//!   keeps all of them, but with the focused window last, so `@`, Enter
//!   goes to the previous window.
//! - `find_app` finds the desktop entry of a window's app ID.
//! - `captures` says which windows hold a capture: only those on the
//!   drum, since there may be dozens of windows.

use wl_shell_core::mru::Mru;
use wl_shell_render::icons::IndexedApp;

use crate::search::Search;

/// A window as the search sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    /// Its ext identifier.
    pub id: String,
    /// Its app's ID as the compositor reports it.
    pub app_id: String,
    /// Its app's name.
    pub app: String,
    pub title: String,
}

impl Window {
    /// Its title, or its app's name if it has none.
    pub fn shown_title(&self) -> &str {
        match self.title.trim() {
            "" => &self.app,
            title => title,
        }
    }

    pub fn untitled(&self) -> bool {
        self.title.trim().is_empty()
    }
}

/// Sort `windows` into the order they were last focused in, the focused
/// one first; the ones never seen focused keep their order, after them.
pub fn recent(windows: &mut [Window], mru: &Mru) {
    mru.sort(windows, |w| &w.id);
}

/// The indices of `windows` (in recency order, see `recent`) that match
/// `query`, best first. With nothing typed, all of them: the window
/// `focused` last, as it is the one `@`, Enter should leave.
pub fn ranked(
    search: &mut Search,
    windows: &[Window],
    query: &str,
    focused: Option<&str>,
) -> Vec<usize> {
    if query.trim().is_empty() {
        let mut all: Vec<usize> = (0..windows.len()).collect();
        // Stable: the rest keep their order.
        all.sort_by_key(|&i| Some(windows[i].id.as_str()) == focused);
        return all;
    }
    let names: Vec<(&str, &str)> = (windows.iter())
        .map(|w| (w.shown_title(), w.app.as_str()))
        .collect();
    search.rank_windows(&names, query)
}

/// The desktop entry for the window app ID `app_id`: the one the dock
/// knows it by (`entry`, the desktop ID the worker found for it), else
/// one named like it or whose `StartupWMClass` is, ignoring case.
pub fn find_app<'a>(
    apps: &'a [IndexedApp],
    entry: Option<&str>,
    app_id: &str,
) -> Option<&'a IndexedApp> {
    let by_entry = entry.and_then(|desktop| {
        let desktop = desktop.trim_end_matches(".desktop");
        apps.iter().find(|a| a.id == desktop)
    });
    by_entry.or_else(|| {
        (apps.iter()).find(|a| {
            a.id.eq_ignore_ascii_case(app_id)
                || a.wm_class
                    .as_deref()
                    .is_some_and(|c| c.eq_ignore_ascii_case(app_id))
        })
    })
}

/// What the captures of a window search should do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Captures {
    /// Windows on the drum without a capture: start one.
    pub start: Vec<String>,
    /// Captures of windows that are no more: drop them.
    pub drop: Vec<String>,
    /// Windows whose capture goes live (if previews are live): those on
    /// the drum. The others pause, and keep their last frame.
    pub live: Vec<String>,
}

/// Which of the `candidates` (the windows that exist) hold a capture,
/// given the `held` ones and the windows on the `ring`.
pub fn captures(candidates: &[&str], held: &[&str], ring: &[&str]) -> Captures {
    let exists = |id: &str| candidates.contains(&id);
    let mut plan = Captures::default();
    for &id in ring.iter().filter(|id| exists(id)) {
        if !plan.live.iter().any(|l| l == id) {
            plan.live.push(id.to_string());
        }
        if !held.contains(&id) && !plan.start.iter().any(|s| s == id) {
            plan.start.push(id.to_string());
        }
    }
    plan.drop = (held.iter())
        .filter(|id| !exists(id))
        .map(|id| id.to_string())
        .collect();
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_render::icons::AppEntry;

    fn window(id: &str, app: &str, title: &str) -> Window {
        Window {
            id: id.into(),
            app_id: app.to_lowercase(),
            app: app.into(),
            title: title.into(),
        }
    }

    fn ids(windows: &[Window], order: &[usize]) -> Vec<String> {
        order.iter().map(|&i| windows[i].id.clone()).collect()
    }

    fn windows() -> Vec<Window> {
        vec![
            window("1", "foot", "PLAN.md - wl-shell"),
            window("2", "Thunderbird", "Inbox — Mozilla Thunderbird"),
            window("3", "Firefox", "rust lifetimes"),
            window("4", "foot", "~"),
        ]
    }

    #[test]
    fn recency_puts_the_focused_first() {
        let mut ws = windows();
        let mut mru = Mru::default();
        mru.focus("3");
        mru.focus("2");
        recent(&mut ws, &mru);
        // Seen focused: 2, then 3; never seen ones follow in order.
        assert_eq!(ids(&ws, &[0, 1, 2, 3]), ["2", "3", "1", "4"]);
        // Without history, the list's order stays.
        let mut ws = windows();
        recent(&mut ws, &Mru::default());
        assert_eq!(ids(&ws, &[0, 1, 2, 3]), ["1", "2", "3", "4"]);
    }

    #[test]
    fn an_empty_query_has_the_focused_window_last() {
        let ws = windows();
        let mut s = Search::default();
        let order = ranked(&mut s, &ws, "", Some("1"));
        assert_eq!(ids(&ws, &order), ["2", "3", "4", "1"]);
        // `@`, Enter: the window in front is the previous one.
        let order = ranked(&mut s, &ws, "  ", Some("1"));
        assert_eq!(ids(&ws, &order)[0], "2");
        // Without focus history, the stream's order is used.
        let order = ranked(&mut s, &ws, "", None);
        assert_eq!(ids(&ws, &order), ["1", "2", "3", "4"]);
        // A focused window that is gone changes nothing.
        let order = ranked(&mut s, &ws, "", Some("9"));
        assert_eq!(ids(&ws, &order), ["1", "2", "3", "4"]);
    }

    #[test]
    fn a_query_ranks_by_title_then_app() {
        let ws = windows();
        let mut s = Search::default();
        let by = |s: &mut Search, q| ids(&ws, &ranked(s, &ws, q, Some("1")));
        assert_eq!(by(&mut s, "inbox"), ["2"]);
        assert_eq!(by(&mut s, "life"), ["3"]);
        // `foot` finds every terminal, the more recent first, even the
        // focused one.
        assert_eq!(by(&mut s, "foot"), ["1", "4"]);
        assert!(by(&mut s, "nothing").is_empty());
    }

    #[test]
    fn untitled_windows_go_by_their_app() {
        let ws = [window("1", "Zathura", " "), window("2", "foot", "Docs")];
        assert!(ws[0].untitled() && !ws[1].untitled());
        assert_eq!(ws[0].shown_title(), "Zathura");
        assert_eq!(ws[1].shown_title(), "Docs");
        let mut s = Search::default();
        assert_eq!(ranked(&mut s, &ws, "zath", None), [0]);
    }

    #[test]
    fn a_retitled_window_is_found_by_its_new_title() {
        let mut ws = windows();
        let mut s = Search::default();
        assert!(ranked(&mut s, &ws, "rustdoc", None).is_empty());
        ws[2].title = "rustdoc - std".into();
        let order = ranked(&mut s, &ws, "rustdoc", None);
        assert_eq!(ids(&ws, &order), ["3"]);
        // Closed: it is gone from the list, and from the results.
        ws.remove(2);
        assert!(ranked(&mut s, &ws, "rustdoc", None).is_empty());
    }

    fn indexed(id: &str, wm_class: Option<&str>) -> IndexedApp {
        IndexedApp {
            id: id.into(),
            name: id.into(),
            generic_name: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            wm_class: wm_class.map(Into::into),
            program: None,
            mime_types: Vec::new(),
            entry: AppEntry::default(),
        }
    }

    #[test]
    fn a_windows_app_is_found_by_its_id() {
        let apps = [
            indexed("org.mozilla.firefox", Some("firefox")),
            indexed("foot", None),
            indexed("code", Some("Code")),
        ];
        let name = |entry, app_id| find_app(&apps, entry, app_id).map(|a| a.id.as_str());
        assert_eq!(name(None, "foot"), Some("foot"));
        assert_eq!(name(None, "Foot"), Some("foot"));
        assert_eq!(name(None, "firefox"), Some("org.mozilla.firefox"));
        assert_eq!(name(None, "code"), Some("code"));
        assert_eq!(name(None, "unknown"), None);
        // The entry the worker found for the app ID comes first.
        assert_eq!(
            name(Some("org.mozilla.firefox.desktop"), "navigator"),
            Some("org.mozilla.firefox")
        );
        assert_eq!(name(Some("gone.desktop"), "foot"), Some("foot"));
    }

    #[test]
    fn only_windows_on_the_drum_hold_captures() {
        let candidates = ["a", "b", "c", "d"];
        // Nothing held: the ones on the drum start, live.
        let plan = captures(&candidates, &[], &["b", "c"]);
        assert_eq!(plan.start, ["b", "c"]);
        assert_eq!(plan.live, ["b", "c"]);
        assert!(plan.drop.is_empty());
        // Rolling on: b leaves the drum and is kept (paused), d comes on.
        let plan = captures(&candidates, &["b", "c"], &["c", "d"]);
        assert_eq!(plan.start, ["d"]);
        assert_eq!(plan.live, ["c", "d"]);
        assert!(plan.drop.is_empty());
        // Rolling back doesn't capture b again.
        let plan = captures(&candidates, &["b", "c", "d"], &["b", "c"]);
        assert!(plan.start.is_empty());
        assert_eq!(plan.live, ["b", "c"]);
        // A window that closed loses its capture, and none starts for it.
        let plan = captures(&["a", "c"], &["b", "c"], &["b", "c"]);
        assert_eq!(plan.drop, ["b"]);
        assert_eq!(plan.live, ["c"]);
        assert!(plan.start.is_empty());
        // Matches that aren't windows (or are listed twice) start nothing.
        let plan = captures(&candidates, &[], &["x", "a", "a"]);
        assert_eq!(plan.start, ["a"]);
        assert_eq!(plan.live, ["a"]);
    }
}
