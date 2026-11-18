//! Configured bang engines, ranking and URL construction.

use serde::{Deserialize, Deserializer, de::MapAccess};
use url::Url;
use wl_shell_log::warn;

use crate::search::Search;
/// Stable ring ID for the query itself, distinct from its suggestions.
pub const TYPED: &str = "typed";

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Web {
    pub fallback: Option<String>,
    pub show: Show,
    #[serde(deserialize_with = "crate::config::files_trigger")]
    pub bang_trigger: Option<char>,
    pub command: String,
    pub suggest: bool,
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
            engines: Vec::new(),
        }
    }
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
}
