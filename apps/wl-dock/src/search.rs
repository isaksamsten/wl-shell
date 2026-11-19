//! Ranking the launcher's matches. Pure, unit tested.
//!
//! Every installed app (the index, `wl_shell_render::icons::index`) is a
//! candidate. Each is matched by its name, generic name, keywords, desktop
//! ID (without a reverse-DNS prefix) and the program its `Exec=` runs,
//! with the name weighted most: the best of the name's score (plus a bonus
//! when the name starts with the query) and the others' scores, scaled
//! down. Scores are `nucleo-matcher`'s fuzzy subsequence scores, ignoring
//! case and accents, with bonuses at word starts, so acronyms work (`vsc`:
//! Visual Studio Code). Every word of the query has to match.
//!
//! The history (history.rs) adds to the score. Ties go to the shorter
//! name, then alphabetically (ignoring case and accents), then the index
//! order, so the results don't reshuffle while typing.

use std::ops::Range;

use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use wl_shell_render::icons::{IndexedApp, fold};

/// What matching another field than the name is worth, of the name's.
const OTHER_WEIGHT: f64 = 0.6;
/// The bonus for a name that starts with the query.
const PREFIX_BONUS: f64 = 40.0;

/// A matcher, kept for its allocations.
pub struct Search {
    matcher: Matcher,
    buf: Vec<char>,
}

impl Default for Search {
    fn default() -> Self {
        let mut config = Config::DEFAULT;
        config.prefer_prefix = true;
        Self {
            matcher: Matcher::new(config),
            buf: Vec::new(),
        }
    }
}

impl Search {
    /// The indices of the `apps` that match `query`, best first. `boost`
    /// adds to an app's score (its history). An empty query matches
    /// nothing.
    pub fn rank(
        &mut self,
        apps: &[IndexedApp],
        query: &str,
        boost: &dyn Fn(&IndexedApp) -> f64,
    ) -> Vec<usize> {
        // Folded, so accents in the query match without them too.
        let folded = fold(query.trim());
        let pattern = Pattern::new(
            &folded,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        if pattern.atoms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(f64, usize, String, usize)> = Vec::new();
        for (i, app) in apps.iter().enumerate() {
            let Some(score) = self.score(&pattern, &folded, app) else {
                continue;
            };
            let name = fold(&app.name);
            scored.push((score + boost(app), name.chars().count(), name, i));
        }
        scored.sort_by(|a, b| {
            (b.0.total_cmp(&a.0))
                .then(a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        scored.into_iter().map(|(.., i)| i).collect()
    }

    /// How well `app` matches; None if it doesn't.
    fn score(&mut self, pattern: &Pattern, folded: &str, app: &IndexedApp) -> Option<f64> {
        let name = self.field(pattern, &app.name).map(|s| {
            let prefix = fold(&app.name).starts_with(folded);
            s + if prefix { PREFIX_BONUS } else { 0.0 }
        });
        let others = (app.generic_name.iter())
            .chain(&app.keywords)
            .map(String::as_str)
            .chain(std::iter::once(app.short_id()))
            .chain(app.program.as_deref())
            .filter_map(|text| self.field(pattern, text))
            .map(|s| s * OTHER_WEIGHT)
            .reduce(f64::max);
        match (name, others) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    fn field(&mut self, pattern: &Pattern, text: &str) -> Option<f64> {
        let haystack = Utf32Str::new(text, &mut self.buf);
        pattern.score(haystack, &mut self.matcher).map(f64::from)
    }

    /// The parts of `name` that `query` matches, as byte ranges, in
    /// order: each word of the query that matches the name marks its
    /// characters (a match by another field may mark none).
    pub fn highlight(&mut self, name: &str, query: &str) -> Vec<Range<usize>> {
        let pattern = Pattern::new(
            &fold(query.trim()),
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        // By chars, so the indices are easy to map back.
        let chars: Vec<(usize, char)> = name.char_indices().collect();
        self.buf.clear();
        self.buf.extend(chars.iter().map(|(_, c)| *c));
        let haystack = Utf32Str::Unicode(&self.buf);
        let mut indices = Vec::new();
        for atom in &pattern.atoms {
            let mut found = Vec::new();
            if atom
                .indices(haystack, &mut self.matcher, &mut found)
                .is_some()
            {
                indices.extend(found);
            }
        }
        indices.sort_unstable();
        indices.dedup();
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for i in indices {
            let Some(&(start, c)) = chars.get(i as usize) else {
                continue;
            };
            let end = start + c.len_utf8();
            match ranges.last_mut() {
                Some(last) if last.end == start => last.end = end,
                _ => ranges.push(start..end),
            }
        }
        ranges
    }

    /// The indices of `names` that match `query`, best first; equal
    /// scores keep their order. An empty query keeps them all, in order.
    pub fn rank_names(&mut self, names: &[&str], query: &str) -> Vec<usize> {
        let folded = fold(query.trim());
        let pattern = Pattern::new(
            &folded,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        if pattern.atoms.is_empty() {
            return (0..names.len()).collect();
        }
        let mut scored: Vec<(f64, usize)> = (names.iter().enumerate())
            .filter_map(|(i, name)| {
                let prefix = fold(name).starts_with(&folded);
                let score = self.field(&pattern, name)?;
                Some((score + if prefix { PREFIX_BONUS } else { 0.0 }, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }

    /// The indices of `windows` (each a title and its app's name) that
    /// match `query`, best first. The title counts most; the app's name
    /// is worth less, as are the two together for a query of several
    /// words (`inbox thunder`). Equal scores keep the order given (the
    /// windows' recency), never the alphabet. An empty query keeps them
    /// all, in order.
    pub fn rank_windows(&mut self, windows: &[(&str, &str)], query: &str) -> Vec<usize> {
        let folded = fold(query.trim());
        let pattern = Pattern::new(
            &folded,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
        );
        if pattern.atoms.is_empty() {
            return (0..windows.len()).collect();
        }
        let words = pattern.atoms.len() > 1;
        let mut scored: Vec<(f64, usize)> = Vec::new();
        for (i, (title, app)) in windows.iter().enumerate() {
            let by_title = self.field(&pattern, title).map(|s| {
                let prefix = fold(title).starts_with(&folded);
                s + if prefix { PREFIX_BONUS } else { 0.0 }
            });
            let by_app = self.field(&pattern, app).map(|s| s * OTHER_WEIGHT);
            let together = words
                .then(|| self.field(&pattern, &format!("{title} {app}")))
                .flatten()
                .map(|s| s * OTHER_WEIGHT);
            let best = [by_title, by_app, together].into_iter().flatten();
            if let Some(score) = best.reduce(f64::max) {
                scored.push((score, i));
            }
        }
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, i)| i).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_render::icons::AppEntry;

    fn app(id: &str, name: &str) -> IndexedApp {
        IndexedApp {
            id: id.into(),
            name: name.into(),
            generic_name: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            wm_class: None,
            program: Some(id.rsplit('.').next().unwrap().to_lowercase()),
            mime_types: Vec::new(),
            entry: AppEntry::default(),
        }
    }

    fn names(apps: &[IndexedApp], query: &str, boost: &dyn Fn(&IndexedApp) -> f64) -> Vec<String> {
        let ranked = Search::default().rank(apps, query, boost);
        ranked.into_iter().map(|i| apps[i].name.clone()).collect()
    }

    fn plain(apps: &[IndexedApp], query: &str) -> Vec<String> {
        names(apps, query, &|_| 0.0)
    }

    #[test]
    fn empty_query_matches_nothing() {
        let apps = [app("firefox", "Firefox")];
        assert!(plain(&apps, "").is_empty());
        assert!(plain(&apps, "   ").is_empty());
        assert!(plain(&apps, "xyz").is_empty());
    }

    #[test]
    fn prefix_beats_subsequence() {
        let apps = [
            app("a.Fir", "Fast Image Resizer"),
            app("b.Profiler", "Profiler"),
            app("firefox", "Firefox"),
        ];
        assert_eq!(plain(&apps, "fir")[0], "Firefox");
        assert_eq!(plain(&apps, "Fi")[0], "Firefox");
    }

    #[test]
    fn acronyms() {
        let apps = [
            app("a.Movies", "Moviestar Scanner"),
            app("com.visualstudio.code", "Visual Studio Code"),
            app("b.Calc", "Calculator"),
        ];
        assert_eq!(
            plain(&apps, "vsc"),
            ["Visual Studio Code", "Moviestar Scanner"]
        );
    }

    #[test]
    fn accents_and_case() {
        let apps = [app("a.Eclair", "Éclair"), app("b.Eclipse", "eclipse")];
        assert_eq!(plain(&apps, "ECLA"), ["Éclair"]);
        assert_eq!(plain(&apps, "écli"), ["eclipse", "Éclair"]);
    }

    #[test]
    fn other_fields_match_below_the_name() {
        let mut files = app("org.gnome.Nautilus", "Files");
        files.generic_name = Some("File Manager".into());
        files.keywords = vec!["folder".into()];
        let nautilus = app("x.Naut", "Nautical Charts");
        let apps = [files, nautilus];
        assert_eq!(plain(&apps, "folder"), ["Files"]);
        assert_eq!(plain(&apps, "manager"), ["Files"]);
        // By its desktop ID, but the name matching counts more.
        assert_eq!(plain(&apps, "nauti"), ["Nautical Charts", "Files"]);
        // Every word has to match.
        assert_eq!(plain(&apps, "file man"), ["Files"]);
        assert!(plain(&apps, "file zzz").is_empty());
    }

    #[test]
    fn history_orders_equal_matches() {
        let apps = [app("a.One", "Term One"), app("b.Two", "Term Two")];
        assert_eq!(plain(&apps, "term"), ["Term One", "Term Two"]);
        let boost = |a: &IndexedApp| if a.id == "b.Two" { 10.0 } else { 0.0 };
        assert_eq!(names(&apps, "term", &boost), ["Term Two", "Term One"]);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn highlights() {
        let mut s = Search::default();
        assert_eq!(s.highlight("Firefox", "fir"), [0..3]);
        assert_eq!(
            s.highlight("Visual Studio Code", "vsc"),
            [0..1, 7..8, 14..15]
        );
        // Accents, multibyte characters, and every word that matches.
        assert_eq!(s.highlight("Éclair", "ecl"), [0..4]);
        assert_eq!(s.highlight("Term Two", "two term"), [0..4, 5..8]);
        assert!(s.highlight("Files", "manager").is_empty());
        assert!(s.highlight("Files", "").is_empty());
    }

    #[test]
    fn stable_ties() {
        let apps = [
            app("c.Tc", "Tool C"),
            app("a.Ta", "Tool A"),
            app("l.Long", "Tool Longer"),
            app("b.Tb", "tool b"),
        ];
        let ranked = plain(&apps, "tool");
        assert_eq!(ranked, ["Tool A", "tool b", "Tool C", "Tool Longer"]);
        // The same in any input order.
        let mut reversed = apps.to_vec();
        reversed.reverse();
        assert_eq!(plain(&reversed, "tool"), ranked);
    }

    #[test]
    fn names_in_their_order() {
        let names = ["Open file", "Open with…", "Open containing directory"];
        let mut s = Search::default();
        assert_eq!(s.rank_names(&names, ""), [0, 1, 2]);
        assert_eq!(s.rank_names(&names, " "), [0, 1, 2]);
        assert_eq!(s.rank_names(&names, "ow"), [1]);
        assert_eq!(s.rank_names(&names, "ocd"), [2]);
        assert_eq!(s.rank_names(&names, "with")[0], 1);
        // Equal scores keep the order given.
        assert_eq!(s.rank_names(&names, "open"), [0, 1, 2]);
        assert!(s.rank_names(&names, "zz").is_empty());
    }

    /// (title, app) of windows, most recent first.
    const WINDOWS: [(&str, &str); 5] = [
        ("PLAN.md - wl-shell", "foot"),
        ("Inbox — Mozilla Thunderbird", "Thunderbird"),
        ("rust lifetimes - Google Search", "Firefox"),
        ("~", "foot"),
        ("Planning board", "Firefox"),
    ];

    #[test]
    fn windows_by_title() {
        let mut s = Search::default();
        // Nothing typed keeps them all, in their order.
        assert_eq!(s.rank_windows(&WINDOWS, ""), [0, 1, 2, 3, 4]);
        assert_eq!(s.rank_windows(&WINDOWS, "  "), [0, 1, 2, 3, 4]);
        assert_eq!(s.rank_windows(&WINDOWS, "inbox"), [1]);
        assert_eq!(s.rank_windows(&WINDOWS, "lifetimes"), [2]);
        assert!(s.rank_windows(&WINDOWS, "zzz").is_empty());
        // The start of a title beats a match further in.
        assert_eq!(s.rank_windows(&WINDOWS, "plan")[0], 0);
        assert_eq!(s.rank_windows(&WINDOWS, "plan"), [0, 4]);
        // Accents and case do not matter.
        assert_eq!(s.rank_windows(&WINDOWS, "INBÖX"), [1]);
    }

    #[test]
    fn windows_by_app_below_the_title() {
        let mut s = Search::default();
        // `foot` finds both terminals, the most recent first.
        assert_eq!(s.rank_windows(&WINDOWS, "foot"), [0, 3]);
        // A title that matches beats an app that matches.
        let windows = [("Notes", "Firefox"), ("Firefox Relay", "Safari")];
        assert_eq!(s.rank_windows(&windows, "firefox"), [1, 0]);
        // Several words may be spread over the title and the app.
        assert_eq!(s.rank_windows(&WINDOWS, "inbox thunder"), [1]);
        assert_eq!(s.rank_windows(&WINDOWS, "lifetimes firefox"), [2]);
        assert!(s.rank_windows(&WINDOWS, "lifetimes foot").is_empty());
    }

    #[test]
    fn untitled_windows_go_by_their_app() {
        let mut s = Search::default();
        // An untitled window is given its app's name as its title.
        let windows = [("Zathura", "Zathura"), ("Docs", "Zathura")];
        assert_eq!(s.rank_windows(&windows, "zath"), [0, 1]);
        assert_eq!(s.rank_windows(&windows, "docs"), [1]);
    }

    #[test]
    fn windows_tie_by_recency_not_by_name() {
        let mut s = Search::default();
        let windows = [("Tool B", "x"), ("Tool A", "x"), ("Tool C", "x")];
        assert_eq!(s.rank_windows(&windows, "tool"), [0, 1, 2]);
        // Whatever the order given, equal scores stay in it.
        let reversed = [windows[2], windows[1], windows[0]];
        assert_eq!(s.rank_windows(&reversed, "tool"), [0, 1, 2]);
    }
}
