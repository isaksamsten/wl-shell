//! The launcher's plugin sources, as the dock keeps them: which source a
//! scope is, the items each answered, and the search running. Pure, unit
//! tested; widgets.rs asks the plugins and app.rs shows the results.
//!
//! A source is known by a key (`Scope::Source(key)`) that stays the same
//! for its instance and ID while the dock runs, whatever loads or goes:
//! a scope whose source went is simply empty.
//!
//! Sources with `items()` are asked once, when their scope is entered,
//! and the answer is kept (and ranked by the dock for every query) until
//! the plugin says it changed (`invalidate()`), reloads or fails.
//!
//! Sources with `search(query, emit)` are asked per query. Each query
//! is a new generation (`begin`), sent once the typing settles (`due`);
//! what the plugin emits counts only if it is for the latest generation
//! (`emitted`), so results for an older query never show. The results
//! shown stay until the new ones come.

use std::collections::HashMap;
use std::time::Instant;

use wl_shell_lua::{Item, RequestId, SourceDecl};

/// A source's key, for as long as the dock runs.
pub type SourceKey = u32;

/// A source a plugin declares, with its key and instance.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceEntry {
    pub key: SourceKey,
    pub instance: String,
    pub decl: SourceDecl,
}

/// The search the launcher is in.
#[derive(Clone, Debug, PartialEq)]
struct Search {
    key: SourceKey,
    /// The latest query's generation, and the query.
    generation: u64,
    query: String,
    /// What the plugin emitted last for its generation then.
    results: Vec<Item>,
}

#[derive(Debug, Default)]
pub struct Sources {
    /// (instance, source ID), by key.
    keys: Vec<(String, String)>,
    /// The `items()` answers, by key.
    items: HashMap<SourceKey, Vec<Item>>,
    /// The `items()` requests open, by key, with their deadlines.
    asking: HashMap<SourceKey, (RequestId, Instant)>,
    generation: u64,
    search: Option<Search>,
}

impl Sources {
    /// The key of `instance`'s source `id`.
    pub fn key(&mut self, instance: &str, id: &str) -> SourceKey {
        let found = (self.keys.iter()).position(|(i, s)| i == instance && s == id);
        let at = found.unwrap_or_else(|| {
            self.keys.push((instance.to_string(), id.to_string()));
            self.keys.len() - 1
        });
        at as SourceKey
    }

    /// The instance and source ID of `key`.
    pub fn lookup(&self, key: SourceKey) -> Option<(&str, &str)> {
        let (i, s) = self.keys.get(key as usize)?;
        Some((i, s))
    }

    /// The keys of `instance`'s sources.
    fn of<'a>(&'a self, instance: &'a str) -> impl Iterator<Item = SourceKey> + 'a {
        (self.keys.iter().enumerate())
            .filter(move |(_, (i, _))| i == instance)
            .map(|(k, _)| k as SourceKey)
    }

    /// `key`'s items, once it answered.
    pub fn items(&self, key: SourceKey) -> Option<&[Item]> {
        self.items.get(&key).map(Vec::as_slice)
    }

    /// Whether `key`'s items are to be asked for at `now`: none, and
    /// none asked (or the ask is past its deadline).
    pub fn wants_items(&self, key: SourceKey, now: Instant) -> bool {
        !self.items.contains_key(&key) && !self.loading(key, now)
    }

    /// Whether `key`'s items were asked for and may still come at `now`.
    pub fn loading(&self, key: SourceKey, now: Instant) -> bool {
        self.asking.get(&key).is_some_and(|(_, due)| now <= *due)
    }

    /// `key`'s items were asked for with request `id`, due at
    /// `deadline`. A plugin has one open at a time: any other of its
    /// instance's is superseded.
    pub fn asked(&mut self, key: SourceKey, id: RequestId, deadline: Instant) {
        let instance = self.lookup(key).map(|(i, _)| i.to_string());
        let siblings: Vec<SourceKey> = (instance.iter())
            .flat_map(|i| self.of(i).collect::<Vec<_>>())
            .collect();
        for k in siblings {
            self.asking.remove(&k);
        }
        self.asking.insert(key, (id, deadline));
    }

    /// The key whose items request `id` is, taking it off the open ones.
    fn take_asking(&mut self, id: RequestId) -> Option<SourceKey> {
        let key = *self.asking.iter().find(|(_, (r, _))| *r == id)?.0;
        self.asking.remove(&key);
        Some(key)
    }

    /// The answer to request `id` for items (an error as none): the key
    /// it was for, if it was one's.
    pub fn answered(
        &mut self,
        id: RequestId,
        answer: Result<Vec<Item>, String>,
    ) -> Option<SourceKey> {
        let key = self.take_asking(id)?;
        self.items.insert(key, answer.unwrap_or_default());
        Some(key)
    }

    /// The answer to request `id` came too late, and was dropped: the
    /// items are asked for again when they are needed. The key it was
    /// for, if it was one's.
    pub fn dropped(&mut self, id: RequestId) -> Option<SourceKey> {
        self.take_asking(id)
    }

    /// `instance` changed (it said so, reloaded or failed): its items are
    /// asked for again when they are needed.
    pub fn forget(&mut self, instance: &str) {
        let keys: Vec<SourceKey> = self.of(instance).collect();
        for k in keys {
            self.items.remove(&k);
            self.asking.remove(&k);
        }
    }

    /// A query `query` for the search of `key`: its generation. A search
    /// of another source starts with no results.
    pub fn begin(&mut self, key: SourceKey, query: &str) -> u64 {
        self.generation += 1;
        let generation = self.generation;
        match &mut self.search {
            Some(s) if s.key == key => {
                s.generation = generation;
                s.query = query.to_string();
            }
            _ => {
                self.search = Some(Search {
                    key,
                    generation,
                    query: query.to_string(),
                    results: Vec::new(),
                })
            }
        }
        generation
    }

    /// The search to send for `generation`, if it is still the latest:
    /// its source and query.
    pub fn due(&self, generation: u64) -> Option<(SourceKey, &str)> {
        let s = self
            .search
            .as_ref()
            .filter(|s| s.generation == generation)?;
        Some((s.key, &s.query))
    }

    /// The source `key` emitted `items` for `generation`: they are the
    /// results if that is the latest query's; true if they are.
    pub fn emitted(&mut self, key: SourceKey, generation: u64, items: Vec<Item>) -> bool {
        match &mut self.search {
            Some(s) if s.key == key && s.generation == generation => {
                s.results = items;
                true
            }
            _ => false,
        }
    }

    /// The search's results, if it is of `key`.
    pub fn results(&self, key: SourceKey) -> Option<&[Item]> {
        let s = self.search.as_ref().filter(|s| s.key == key)?;
        Some(&s.results)
    }

    /// The search is over (the launcher left the scope, or closed).
    pub fn end(&mut self) {
        self.search = None;
    }

    /// `key`'s item `id`: among the search's results, else its items.
    pub fn item(&self, key: SourceKey, id: &str) -> Option<&Item> {
        let list = self.results(key).or_else(|| self.items(key))?;
        list.iter().find(|i| i.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_lua::{ItemKind, Request, RequestKind};

    fn item(id: &str) -> Item {
        Item {
            id: id.into(),
            title: id.into(),
            subtitle: None,
            icon: None,
            kind: ItemKind::Item,
            data: serde_json::Value::Null,
        }
    }

    fn request() -> RequestId {
        Request::new(RequestKind::Label, std::time::Duration::from_secs(1))
            .0
            .id
    }

    #[test]
    fn keys_stay() {
        let mut s = Sources::default();
        let ssh = s.key("plugin:a", "ssh");
        let calc = s.key("plugin:a", "calc");
        let other = s.key("plugin:b", "ssh");
        assert_eq!(s.key("plugin:a", "ssh"), ssh);
        assert!(ssh != calc && ssh != other);
        assert_eq!(s.lookup(other), Some(("plugin:b", "ssh")));
        assert_eq!(s.lookup(99), None);
    }

    #[test]
    fn items_are_kept_until_forgotten() {
        let mut s = Sources::default();
        let ssh = s.key("plugin:a", "ssh");
        let hosts = s.key("plugin:a", "hosts");
        let now = Instant::now();
        let due = now + std::time::Duration::from_secs(2);
        assert!(s.wants_items(ssh, now) && s.items(ssh).is_none());
        let (r1, r2) = (request(), request());
        s.asked(ssh, r1, due);
        assert!(!s.wants_items(ssh, now) && s.loading(ssh, now));
        // Past its deadline, it is asked again.
        let later = due + std::time::Duration::from_millis(1);
        assert!(s.wants_items(ssh, later) && !s.loading(ssh, later));
        // A second ask of the plugin's supersedes the first.
        s.asked(hosts, r2, due);
        assert!(s.wants_items(ssh, now));
        assert_eq!(s.answered(r1, Ok(vec![item("x")])), None);
        assert_eq!(s.answered(r2, Ok(vec![item("h")])), Some(hosts));
        assert_eq!(s.items(hosts).unwrap()[0].id, "h");
        assert!(!s.wants_items(hosts, now) && !s.loading(hosts, now));
        assert_eq!(s.item(hosts, "h").map(|i| i.id.as_str()), Some("h"));
        // An error is no items, kept as well.
        let r3 = request();
        s.asked(ssh, r3, due);
        assert_eq!(s.answered(r3, Err("x".into())), Some(ssh));
        assert!(s.items(ssh).unwrap().is_empty());
        // A late answer is dropped: asked for again.
        s.forget("plugin:a");
        let r4 = request();
        s.asked(ssh, r4, due);
        assert_eq!(s.dropped(r4), Some(ssh));
        assert!(s.wants_items(ssh, now));
        s.items.insert(hosts, vec![item("h")]);
        s.forget("plugin:b");
        assert!(s.items(hosts).is_some());
        s.forget("plugin:a");
        assert!(s.wants_items(ssh, now) && s.wants_items(hosts, now));
    }

    /// Typing fast: only the latest query's results count, whenever the
    /// older ones arrive.
    #[test]
    fn older_queries_never_show() {
        let mut s = Sources::default();
        let calc = s.key("plugin:a", "calc");
        let g1 = s.begin(calc, "1");
        let g2 = s.begin(calc, "1+");
        let g3 = s.begin(calc, "1+1");
        // Only the latest is sent once the typing settles.
        assert_eq!(s.due(g1), None);
        assert_eq!(s.due(g2), None);
        assert_eq!(s.due(g3), Some((calc, "1+1")));
        // The latest answers first; the older ones come after it.
        assert!(s.emitted(calc, g3, vec![item("2")]));
        assert!(!s.emitted(calc, g1, vec![item("1")]));
        assert!(!s.emitted(calc, g2, vec![item("1+")]));
        assert_eq!(s.results(calc).unwrap()[0].id, "2");
        // A newer query keeps them shown until its own come.
        let g4 = s.begin(calc, "1+1+");
        assert!(!s.emitted(calc, g3, vec![item("late")]));
        assert_eq!(s.results(calc).unwrap()[0].id, "2");
        assert!(s.emitted(calc, g4, Vec::new()));
        assert!(s.results(calc).unwrap().is_empty());
        // Another source starts empty; this one's are dropped.
        let other = s.key("plugin:b", "calc");
        let g5 = s.begin(other, "");
        assert!(!s.emitted(calc, g4, vec![item("x")]));
        assert_eq!(s.results(calc), None);
        assert!(s.results(other).unwrap().is_empty());
        s.end();
        assert_eq!(s.due(g5), None);
        assert!(!s.emitted(other, g5, vec![item("x")]));
    }
}
