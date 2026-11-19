//! Configured bang engines, ranking and URL construction.

use serde::{Deserialize, Deserializer, de::MapAccess};
use url::Url;
use wl_shell_log::warn;

use crate::launcher::Scope;
use crate::search::Search;
/// Stable ring ID for the query itself, distinct from its suggestions.
pub const TYPED: &str = "typed";
/// Ring ID of the web search fallback.
pub const FALLBACK: &str = "web-fallback";
/// What the ID of a ring match for a suggestion starts with.
pub const SUGGESTION: &str = "suggestion:";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Web {
    pub fallback: Option<String>,
    pub show: Show,
    #[serde(deserialize_with = "crate::config::files_trigger")]
    pub bang_trigger: Option<char>,
    pub command: String,
    pub suggest: bool,
    pub fallback_suggest: bool,
    #[serde(rename = "engine", deserialize_with = "read_engines")]
    pub engines: Vec<Engine>,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            fallback: None,
            show: Show::Empty,
            bang_trigger: Some('!'),
            command: "xdg-open {url}".into(),
            suggest: true,
            fallback_suggest: false,
            engines: Vec::new(),
        }
    }
}

/// Where the web search fallback sits on the drum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// Not shown.
    Off,
    /// In front: nothing else matched.
    Front,
    /// In the last slot, behind the matches (`show = "always"`).
    Last,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Show {
    #[default]
    Empty,
    Always,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Engine {
    #[serde(skip)]
    pub keyword: String,
    pub name: String,
    pub url: String,
    pub suggest: String,
    pub icon: Option<String>,
}

fn read_engines<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<Engine>, D::Error> {
    struct Engines;
    impl<'de> serde::de::Visitor<'de> for Engines {
        type Value = Vec<Engine>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a table of web search engines")
        }
        fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Self::Value, M::Error> {
            let mut engines = Vec::new();
            while let Some((keyword, mut engine)) = m.next_entry::<String, Engine>()? {
                if keyword.is_empty()
                    || keyword.chars().any(char::is_whitespace)
                    || engine.name.trim().is_empty()
                    || !valid_template(&engine.url)
                {
                    warn!(
                        "ignoring launcher.web.engine.{keyword}: needs a keyword, name and an HTTP(S) url containing {{query}}"
                    );
                    continue;
                }
                if !engine.suggest.is_empty() && !valid_template(&engine.suggest) {
                    warn!("ignoring suggest URL for launcher.web.engine.{keyword}");
                    engine.suggest.clear();
                }
                engine.keyword = keyword;
                engines.push(engine);
            }
            Ok(engines)
        }
    }
    d.deserialize_map(Engines)
}

fn valid_template(template: &str) -> bool {
    template.contains("{query}") && http_url(&template.replace("{query}", "test")).is_some()
}

pub fn http_url(text: &str) -> Option<Url> {
    let url = Url::parse(text).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.has_host()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}

impl Web {
    /// Turn off a fallback that names no engine, with a warning. Run
    /// once the config is read.
    pub fn check(&mut self) {
        if let Some(keyword) = &self.fallback
            && self.engine(keyword).is_none()
        {
            warn!("launcher.web.fallback: there is no engine {keyword:?}; the web fallback is off");
            self.fallback = None;
        }
    }

    /// The engine the fallback searches with.
    pub fn fallback_engine(&self) -> Option<&Engine> {
        self.engine(self.fallback.as_deref()?)
    }

    /// Where the fallback goes for a `scope` query of `text` that has
    /// `matches` matches (apps, or the engines of a bang). Only app
    /// queries fall back, and an unknown bang: a file search or a
    /// level's choices never do.
    pub fn fallback_place(&self, scope: Scope, text: &str, matches: usize) -> Place {
        if self.fallback_engine().is_none() || text.trim().is_empty() {
            return Place::Off;
        }
        match scope {
            Scope::Apps | Scope::Bang if matches == 0 => Place::Front,
            Scope::Apps if self.show == Show::Always => Place::Last,
            _ => Place::Off,
        }
    }

    /// What the fallback searches for: an unknown bang is searched
    /// whole, trigger included.
    pub fn fallback_query(&self, scope: Scope, text: &str) -> String {
        match (scope, self.bang_trigger) {
            (Scope::Bang, Some(trigger)) => format!("{trigger}{text}"),
            _ => text.to_owned(),
        }
    }

    /// The engine to ask for suggestions of an app query, if its
    /// fallback is in front: only with `fallback-suggest` (and not
    /// `suggest = false`), since it sends app queries over the network.
    pub fn fallback_suggester(&self, scope: Scope, place: Place) -> Option<&Engine> {
        (scope == Scope::Apps && place == Place::Front && self.suggest && self.fallback_suggest)
            .then(|| self.fallback_engine())
            .flatten()
    }

    pub fn engine(&self, keyword: &str) -> Option<&Engine> {
        self.engines.iter().find(|e| e.keyword == keyword)
    }

    pub fn exact(&self, text: &str) -> Option<&Engine> {
        self.engines
            .iter()
            .find(|e| e.keyword.eq_ignore_ascii_case(text))
    }

    /// A pasted bang and query can skip the picker when the keyword is exact.
    pub fn paste_bang<'a>(&'a self, text: &'a str) -> Option<(&'a str, &'a str)> {
        let (word, rest) = split_bang(text);
        Some((&self.exact(word)?.keyword, rest?))
    }

    pub fn ranked(&self, search: &mut Search, text: &str) -> Vec<&Engine> {
        if text.trim().is_empty() {
            return self.engines.iter().collect();
        }
        let text = text.trim();
        let names: Vec<_> = self.engines.iter().map(|e| e.name.as_str()).collect();
        let mut ranked: Vec<_> = self.exact(text).into_iter().collect();
        for i in search.rank_names(&names, text) {
            if !ranked.contains(&&self.engines[i]) {
                ranked.push(&self.engines[i]);
            }
        }
        let prefix = text.to_ascii_lowercase();
        for e in &self.engines {
            if e.keyword.to_ascii_lowercase().starts_with(&prefix) && !ranked.contains(&e) {
                ranked.push(e);
            }
        }
        ranked
    }
}

fn split_bang(text: &str) -> (&str, Option<&str>) {
    text.split_once(char::is_whitespace)
        .map_or((text, None), |(word, rest)| (word, Some(rest.trim_start())))
}

pub fn url(template: &str, query: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::new();
    for b in query.trim().bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[(b >> 4) as usize]));
            encoded.push(char::from(HEX[(b & 15) as usize]));
        }
    }
    template.replace("{query}", &encoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::launcher::{Act, Key, Nav, Query, Scope, Selection, act};
    use wl_shell_config::ConfigFile;

    #[test]
    fn engine_order_and_validation() {
        let c = Config::parse(
            r#"
            [launcher.web.engine.z]
            name = 'Last alphabetically'
            url = 'https://example.com/?q={query}'
            [launcher.web.engine.bad]
            name = 'Missing URL'
            [launcher.web.engine.b]
            name = 'Second'
            url = 'https://example.com/{query}'
            [launcher.web.engine.nope]
            name = 'Invalid URL'
            url = 'https://example.com/'
        "#,
        )
        .unwrap();
        let keys: Vec<_> = c
            .launcher
            .web
            .engines
            .iter()
            .map(|e| e.keyword.as_str())
            .collect();
        assert_eq!(keys, ["z", "b"]);
    }

    #[test]
    fn encoding() {
        assert_eq!(
            url("?q={query}", "  a &#+/ å-._~  "),
            "?q=a%20%26%23%2B%2F%20%C3%A5-._~"
        );
    }

    #[test]
    fn pick_engine_paste_and_back() {
        let mut search = Search::default();
        let web = Web {
            engines: [("gh", "GitHub"), ("g", "Google"), ("a", "Amazon")]
                .into_iter()
                .map(|(keyword, name)| Engine {
                    keyword: keyword.into(),
                    name: name.into(),
                    ..Engine::default()
                })
                .collect(),
            ..Web::default()
        };
        let mut query = Query::default();
        assert!(query.start_bang("!", web.bang_trigger));
        assert_eq!(query.scope(), Scope::Bang);
        query.edit(&Key::Text("g".into()), None);
        let ranked = web.ranked(&mut search, query.text());
        assert_eq!(ranked[0].keyword, "g");
        assert!(ranked.iter().any(|e| e.keyword == "gh"));
        let mut nav = Nav::default();
        *nav.query_mut() = query;
        let from = Selection {
            selected: Some("g".into()),
            rolled: true,
            shown: vec!["g", "gh"],
        };
        let mut web_query = Query::default();
        web_query.commit_engine("g".into(), "");
        nav.enter_with(from.clone(), (), web_query);
        assert_eq!((nav.query().scope(), nav.query().text()), (Scope::Web, ""));
        assert_eq!(act(Key::Backspace, nav.depth(), nav.query()), Act::Out);
        assert_eq!(nav.leave(), Some(from));
        assert_eq!(
            (nav.query().scope(), nav.query().text()),
            (Scope::Bang, "g")
        );
        assert_eq!(act(Key::Back, 0, nav.query()), Act::Edit(Key::Back));

        let mut query = Query::default();
        assert!(query.start_bang("!gh rust", web.bang_trigger));
        assert_eq!(web.paste_bang(query.text()), Some(("gh", "rust")));

        let mut query = Query::default();
        assert!(query.start_bang("!xyz foo", web.bang_trigger));
        assert_eq!(web.paste_bang(query.text()), None);
        assert_eq!(query.scope(), Scope::Bang);
        assert!(web.ranked(&mut search, query.text()).is_empty());
        assert_eq!(act(Key::Escape, 0, &query), Act::Clear);
        query.edit(&Key::Clear, None);
        assert_eq!(act(Key::Escape, 0, &query), Act::Close);
        assert!(!Query::default().start_bang("!a", None));
    }

    fn web(fallback: Option<&str>, show: Show) -> Web {
        Web {
            fallback: fallback.map(str::to_owned),
            show,
            engines: [("g", "Google"), ("d", "DuckDuckGo")]
                .into_iter()
                .map(|(keyword, name)| Engine {
                    keyword: keyword.into(),
                    name: name.into(),
                    url: "https://example.org/?q={query}".into(),
                    suggest: "https://example.org/s?q={query}".into(),
                    ..Engine::default()
                })
                .collect(),
            ..Web::default()
        }
    }

    #[test]
    fn the_fallback_is_in_front_when_nothing_matches() {
        let web = web(Some("g"), Show::Empty);
        assert_eq!(web.fallback_engine().unwrap().name, "Google");
        assert_eq!(web.fallback_place(Scope::Apps, "how tall", 0), Place::Front);
        // A match is enough with the default `show`, however weak.
        assert_eq!(web.fallback_place(Scope::Apps, "weather", 1), Place::Off);
        // Nothing typed, nothing to search for.
        assert_eq!(web.fallback_place(Scope::Apps, "", 0), Place::Off);
        assert_eq!(web.fallback_place(Scope::Apps, "  ", 0), Place::Off);
    }

    #[test]
    fn always_puts_it_last_behind_the_matches() {
        let web = web(Some("d"), Show::Always);
        assert_eq!(web.fallback_place(Scope::Apps, "weather", 3), Place::Last);
        assert_eq!(web.fallback_place(Scope::Apps, "weather", 1), Place::Last);
        // Without matches, it is the front.
        assert_eq!(web.fallback_place(Scope::Apps, "weather", 0), Place::Front);
        assert_eq!(web.fallback_place(Scope::Apps, "", 3), Place::Off);
    }

    #[test]
    fn no_fallback_without_an_engine() {
        let none = web(None, Show::Always);
        assert_eq!(none.fallback_place(Scope::Apps, "x", 0), Place::Off);
        assert_eq!(none.fallback_place(Scope::Bang, "x", 0), Place::Off);
        // A keyword that names no engine turns it off.
        let mut unknown = web(Some("nope"), Show::Empty);
        assert_eq!(unknown.fallback_place(Scope::Apps, "x", 0), Place::Off);
        unknown.check();
        assert_eq!(unknown.fallback, None);
        let mut known = web(Some("g"), Show::Empty);
        known.check();
        assert_eq!(known.fallback.as_deref(), Some("g"));
    }

    #[test]
    fn only_app_queries_and_unknown_bangs_fall_back() {
        let web = web(Some("g"), Show::Always);
        for scope in [Scope::Files, Scope::Actions, Scope::Web] {
            assert_eq!(web.fallback_place(scope, "x", 0), Place::Off);
            assert_eq!(web.fallback_place(scope, "x", 4), Place::Off);
        }
        // An unknown bang has no engines to show; the fallback searches it whole.
        assert_eq!(web.fallback_place(Scope::Bang, "xyz foo", 0), Place::Front);
        assert_eq!(web.fallback_query(Scope::Bang, "xyz foo"), "!xyz foo");
        assert_eq!(web.fallback_query(Scope::Apps, "how tall"), "how tall");
        // Known engines on the drum: the picker, even with `show = "always"`.
        assert_eq!(web.fallback_place(Scope::Bang, "g", 2), Place::Off);
        assert_eq!(web.fallback_place(Scope::Bang, "", 0), Place::Off);
    }

    #[test]
    fn suggestions_for_app_queries_only_when_asked() {
        let mut web = web(Some("g"), Show::Empty);
        fn front(web: &Web) -> Option<&Engine> {
            web.fallback_suggester(Scope::Apps, Place::Front)
        }
        assert!(front(&web).is_none());
        web.fallback_suggest = true;
        assert_eq!(front(&web).unwrap().keyword, "g");
        // Not behind a weak match, an unknown bang, or with suggest = false.
        assert!(web.fallback_suggester(Scope::Apps, Place::Last).is_none());
        assert!(web.fallback_suggester(Scope::Apps, Place::Off).is_none());
        assert!(web.fallback_suggester(Scope::Bang, Place::Front).is_none());
        web.suggest = false;
        assert!(front(&web).is_none());
    }

    #[test]
    fn the_fallback_options_are_read() {
        let c = Config::parse(
            r#"
            [launcher.web]
            fallback = 'g'
            show = 'always'
            fallback-suggest = true
            [launcher.web.engine.g]
            name = 'Google'
            url = 'https://example.com/?q={query}'
        "#,
        )
        .unwrap();
        let web = c.launcher.web;
        assert_eq!(web.fallback.as_deref(), Some("g"));
        assert_eq!((web.show, web.fallback_suggest), (Show::Always, true));
        let off = Config::parse("").unwrap().launcher.web;
        assert_eq!(
            (off.fallback, off.show, off.fallback_suggest),
            (None, Show::Empty, false)
        );
    }
}
