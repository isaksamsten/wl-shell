//! A single suggestion worker. The UI never waits on HTTP.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use wl_shell_log::warn;

use crate::engines::{self, Engine};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Query {
    pub engine: String,
    pub text: String,
}

struct Request {
    session: u64,
    query: Query,
    url: String,
}

pub struct Answer {
    session: u64,
    query: Query,
    values: Vec<String>,
}

struct Queue<T> {
    next: Option<T>,
    closed: bool,
}

type Shared = (Mutex<Queue<Request>>, Condvar);

pub struct Worker(Arc<Shared>);

impl Worker {
    pub fn start(send: impl Fn(Answer) + Send + 'static) -> std::io::Result<Self> {
        let worker = Self(Arc::new((
            Mutex::new(Queue {
                next: None,
                closed: false,
            }),
            Condvar::new(),
        )));
        let queue = worker.0.clone();
        std::thread::Builder::new()
            .name("web-suggest".into())
            .spawn(move || {
                let agent = ureq::AgentBuilder::new()
                    .timeout(Duration::from_millis(1500))
                    .redirects(0)
                    .build();
                let mut warned = false;
                loop {
                    let request = {
                        let (lock, ready) = &*queue;
                        let mut q = ready
                            .wait_while(lock.lock().unwrap(), |q| !q.closed && q.next.is_none())
                            .unwrap();
                        if q.closed {
                            return;
                        }
                        q.next.take().unwrap()
                    };
                    let values = fetch(&agent, &request).unwrap_or_else(|e| {
                        if !warned {
                            warn!("web suggestions unavailable: {e}");
                            warned = true;
                        }
                        Vec::new()
                    });
                    send(Answer {
                        session: request.session,
                        query: request.query,
                        values,
                    });
                }
            })?;
        Ok(worker)
    }

    fn replace(&self, request: Option<Request>) {
        self.0.0.lock().unwrap().next = request;
        self.0.1.notify_one();
    }

    pub fn clear(&self) {
        self.replace(None);
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().closed = true;
        self.0.1.notify_one();
    }
}

pub struct Session {
    id: u64,
    active: Option<Query>,
    cache: HashMap<Query, Vec<String>>,
}

impl Session {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            active: None,
            cache: HashMap::new(),
        }
    }

    pub fn query(&mut self, worker: &Worker, engine: Option<&Engine>, text: &str) {
        let text = text.trim();
        let next = engine
            .filter(|e| !e.suggest.is_empty() && !text.is_empty())
            .map(|e| Query {
                engine: e.keyword.clone(),
                text: text.into(),
            });
        if self.active == next {
            return;
        }
        self.active = next;
        let request = self
            .active
            .as_ref()
            .zip(engine)
            .filter(|(q, _)| !self.cache.contains_key(q))
            .map(|(q, e)| Request {
                session: self.id,
                query: q.clone(),
                url: engines::url(&e.suggest, text),
            });
        worker.replace(request);
    }

    pub fn values(&self) -> &[String] {
        self.active
            .as_ref()
            .and_then(|q| self.cache.get(q))
            .map_or(&[], Vec::as_slice)
    }

    pub fn accept(&mut self, answer: Answer) -> bool {
        if self.id != answer.session || self.active.as_ref() != Some(&answer.query) {
            return false;
        }
        if self.cache.len() >= 128 {
            self.cache.clear();
        }
        self.cache.insert(answer.query, answer.values);
        true
    }
}

fn fetch(agent: &ureq::Agent, request: &Request) -> Result<Vec<String>, &'static str> {
    let mut body = Vec::new();
    let response = agent
        .get(&request.url)
        .call()
        .map_err(|_| "HTTP request failed")?;
    if response.status() != 200 {
        return Err("unexpected HTTP status");
    }
    response
        .into_reader()
        .take(256 * 1024 + 1)
        .read_to_end(&mut body)
        .map_err(|_| "could not read response")?;
    if body.len() > 256 * 1024 {
        return Err("response too large");
    }
    parse(&body, &request.query.text).ok_or("invalid OpenSearch response")
}

fn parse(body: &[u8], typed: &str) -> Option<Vec<String>> {
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    let entries = json.get(1)?.as_array()?;
    let mut values = Vec::new();
    for entry in entries {
        if let Some(text) = entry.as_str().map(str::trim)
            && !text.is_empty()
            && text != typed
            && text.len() <= 4096
            && !text.chars().any(char::is_control)
            && !values.iter().any(|v| v == text)
        {
            values.push(text.to_owned());
            if values.len() == 32 {
                break;
            }
        }
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opensearch_completions() {
        assert_eq!(
            parse(
                br#"["usb",["usb","usb cable","usb c","usb cable"],[],[]]"#,
                "usb"
            )
            .unwrap(),
            ["usb cable", "usb c"]
        );
        assert!(parse(br#"{"suggestions":[{"value":"usb cable"}]}"#, "usb").is_none());
    }

    #[test]
    fn latest_waiting_query_wins_and_cached_queries_do_not_enqueue() {
        // A worker without a running thread: take the first request as
        // though it were in flight, then observe only the latest one.
        let worker = Worker(Arc::new((
            Mutex::new(Queue {
                next: None,
                closed: false,
            }),
            Condvar::new(),
        )));
        let engine = Engine {
            keyword: "g".into(),
            suggest: "https://example.org/?q={query}".into(),
            ..Engine::default()
        };
        let mut session = Session::new(1);
        let take = || worker.0.0.lock().unwrap().next.take().unwrap();
        session.query(&worker, Some(&engine), "a");
        let first = take();
        assert!(session.accept(Answer {
            session: 1,
            query: first.query,
            values: vec!["apple".into()]
        }));
        session.query(&worker, Some(&engine), "ab");
        session.query(&worker, Some(&engine), "abc");
        assert_eq!(take().query.text, "abc");
        session.query(&worker, Some(&engine), "abcd");
        session.query(&worker, Some(&engine), "a");
        assert!(worker.0.0.lock().unwrap().next.is_none());
        assert_eq!(session.values(), ["apple"]);
    }

    #[test]
    fn app_queries_reach_the_queue_only_with_fallback_suggest() {
        use crate::engines::{Place, Web};
        use crate::launcher::Scope;
        let worker = Worker(Arc::new((
            Mutex::new(Queue {
                next: None,
                closed: false,
            }),
            Condvar::new(),
        )));
        let mut web = Web {
            fallback: Some("g".into()),
            engines: vec![Engine {
                keyword: "g".into(),
                name: "Google".into(),
                url: "https://example.org/?q={query}".into(),
                suggest: "https://example.org/s?q={query}".into(),
                ..Engine::default()
            }],
            ..Web::default()
        };
        let mut session = Session::new(1);
        let mut ask = |web: &Web, text: &str| {
            let engine = web.fallback_suggester(Scope::Apps, Place::Front);
            session.query(&worker, engine, text);
            worker.0.0.lock().unwrap().next.take().map(|r| r.query.text)
        };
        assert_eq!(ask(&web, "how tall"), None);
        web.fallback_suggest = true;
        assert_eq!(ask(&web, "how tall"), Some("how tall".into()));
        // Back to nothing typed: the queue is emptied.
        assert_eq!(ask(&web, " "), None);
    }

    #[test]
    fn stale_answers_and_cache() {
        let mut session = Session::new(5);
        let a = Query {
            engine: "g".into(),
            text: "a".into(),
        };
        let b = Query {
            text: "ab".into(),
            ..a.clone()
        };
        let answer = |session, query| Answer {
            session,
            query,
            values: vec!["abc".into()],
        };
        session.active = Some(a.clone());
        assert!(!session.accept(answer(4, a.clone())));
        assert!(session.accept(answer(5, a.clone())));
        session.active = Some(b);
        assert!(!session.accept(answer(5, a.clone())));
        assert!(session.values().is_empty());
        session.active = Some(a);
        assert_eq!(session.values(), ["abc"]);
    }
}
