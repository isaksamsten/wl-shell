//! Which windows a note belongs to. Pure, unit tested.
//!
//! A matcher has an `app` glob, a `title` glob, or both, and matches a
//! window when all it has match. `app` matches the app ID as is,
//! case-sensitively (as the dock groups windows); `title` ignores case.
//! `*` crosses `/`, so `*wl-shell*` matches `~/projects/wl-shell`. A glob
//! that doesn't parse makes its matcher never match.

use globset::{GlobBuilder, GlobMatcher};

/// The window notes are matched against.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub app_id: String,
    pub title: String,
}

impl Window {
    pub fn new(app_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            title: title.into(),
        }
    }
}

/// A glob matching exactly `s` (an app ID, say), its special characters
/// escaped (a backslash too: it escapes in a glob).
pub fn escape(s: &str) -> String {
    globset::escape(s).replace('\\', "\\\\")
}

/// A glob as written, and compiled; None if it doesn't parse.
#[derive(Clone, Debug)]
pub struct Pattern {
    text: String,
    glob: Option<GlobMatcher>,
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.glob.is_some() == other.glob.is_some()
    }
}

impl Pattern {
    /// Compile `text`; the error says why it doesn't parse.
    fn new(text: &str, ignore_case: bool) -> (Self, Option<String>) {
        let built = GlobBuilder::new(text)
            .case_insensitive(ignore_case)
            .literal_separator(false)
            .backslash_escape(true)
            .build();
        let (glob, error) = match built {
            Ok(g) => (Some(g.compile_matcher()), None),
            Err(e) => (None, Some(format!("'{text}': {}", e.kind()))),
        };
        let pattern = Self {
            text: text.to_string(),
            glob,
        };
        (pattern, error)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether it parsed.
    pub fn valid(&self) -> bool {
        self.glob.is_some()
    }

    fn is_match(&self, s: &str) -> bool {
        self.glob.as_ref().is_some_and(|g| g.is_match(s))
    }
}

/// One way a note matches windows.
#[derive(Clone, Debug, PartialEq)]
pub struct Matcher {
    pub app: Option<Pattern>,
    pub title: Option<Pattern>,
}

impl Matcher {
    /// A matcher for `app` and `title` globs (at least one should be
    /// given), and the errors of those that don't parse.
    pub fn new(app: Option<&str>, title: Option<&str>) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut pattern = |text: Option<&str>, ignore_case| {
            text.map(|t| {
                let (p, e) = Pattern::new(t, ignore_case);
                errors.extend(e);
                p
            })
        };
        let app = pattern(app, false);
        let title = pattern(title, true);
        (Self { app, title }, errors)
    }

    /// Whether `window` matches: everything given matches, and something
    /// is given.
    pub fn matches(&self, window: &Window) -> bool {
        let ok = |p: &Option<Pattern>, s: &str| p.as_ref().is_none_or(|p| p.is_match(s));
        (self.app.is_some() || self.title.is_some())
            && ok(&self.app, &window.app_id)
            && ok(&self.title, &window.title)
    }

    /// Whether every glob in it parsed.
    pub fn valid(&self) -> bool {
        [&self.app, &self.title]
            .into_iter()
            .flatten()
            .all(Pattern::valid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(app: Option<&str>, title: Option<&str>) -> Matcher {
        let (m, errors) = Matcher::new(app, title);
        assert!(errors.is_empty(), "{errors:?}");
        m
    }

    #[test]
    fn app_globs() {
        let ff = Window::new("firefox", "Mozilla Firefox");
        let nightly = Window::new("org.mozilla.firefox", "x");
        assert!(m(Some("firefox"), None).matches(&ff));
        assert!(!m(Some("firefox"), None).matches(&nightly));
        assert!(m(Some("org.mozilla.*"), None).matches(&nightly));
        assert!(m(Some("fire?ox"), None).matches(&ff));
        assert!(m(Some("[ef]irefox"), None).matches(&ff));
        assert!(!m(Some("fire"), None).matches(&ff));
    }

    #[test]
    fn app_is_case_sensitive_and_title_is_not() {
        let w = Window::new("Firefox", "Jira Board");
        assert!(!m(Some("firefox"), None).matches(&w));
        assert!(m(Some("Firefox"), None).matches(&w));
        assert!(m(None, Some("*jira*")).matches(&w));
        assert!(m(None, Some("JIRA BOARD")).matches(&w));
    }

    #[test]
    fn title_globs_cross_slashes() {
        let w = Window::new("foot", "isak@host: ~/projects/wl-shell/apps");
        assert!(m(None, Some("*wl-shell*")).matches(&w));
        assert!(m(None, Some("*/projects/*")).matches(&w));
        assert!(!m(None, Some("wl-shell")).matches(&w));
    }

    #[test]
    fn both_must_match() {
        let both = m(Some("foot"), Some("*wl-shell*"));
        assert!(both.matches(&Window::new("foot", "~/projects/wl-shell")));
        assert!(!both.matches(&Window::new("foot", "~/projects/other")));
        assert!(!both.matches(&Window::new("kitty", "~/projects/wl-shell")));
    }

    #[test]
    fn empty_matches_nothing() {
        assert!(!m(None, None).matches(&Window::new("a", "b")));
        assert!(!m(None, None).matches(&Window::default()));
    }

    #[test]
    fn an_escaped_glob_matches_only_itself() {
        for s in ["org.x[1]*", "a\\b", "C:\\*", "{a,b}?", "\\[x]"] {
            let w = Window::new(s, s);
            assert!(m(Some(&escape(s)), Some(&escape(s))).matches(&w), "{s}");
            assert!(
                !m(Some(&escape(s)), None).matches(&Window::new("other", "")),
                "{s}"
            );
        }
        assert!(!m(Some(&escape("a\\b")), None).matches(&Window::new("ab", "")));
    }

    #[test]
    fn broken_globs_never_match() {
        let (broken, errors) = Matcher::new(Some("fire[fox"), None);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("fire[fox"), "{errors:?}");
        assert!(!broken.valid());
        assert!(!broken.matches(&Window::new("fire[fox", "")));
        assert!(!broken.matches(&Window::new("firefox", "")));
        // A broken title glob breaks the matcher even if the app matches.
        let (broken, errors) = Matcher::new(Some("foot"), Some("{a"));
        assert_eq!(errors.len(), 1);
        assert!(!broken.matches(&Window::new("foot", "{a")));
    }
}
