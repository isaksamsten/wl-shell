//! The request channel between a host (the dock, later the launcher) and
//! its plugin states. Pure but for the clock; unit tested.
//!
//! The host never waits on Lua: every question is a [`Request`] with an
//! id and a deadline, sent to an instance's queue, and answered with an
//! [`Answer`] carrying the id back. The host keeps one open request per
//! instance and [`Topic`] in [`Pending`]: asking again supersedes (and
//! cancels) the last one, and an answer that comes after its deadline, or
//! for a request no longer open, is dropped. A worker skips requests that
//! were cancelled or are past their deadline before running them.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::ui::Node;

/// Which plugin state a request goes to: one per widget placement, one
/// per `[plugin.<name>]` table.
pub type InstanceId = String;

/// A request's id, unique in the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl RequestId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        RequestId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Which way the dock runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Axis {
    /// A bottom dock.
    Horizontal,
    /// A left or right dock.
    Vertical,
}

impl Axis {
    pub fn name(self) -> &'static str {
        match self {
            Axis::Horizontal => "horizontal",
            Axis::Vertical => "vertical",
        }
    }
}

/// The tile `icon(tile)` draws for: the magnified size in logical pixels
/// (`span` icons long along the dock), and the output's scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub w: f64,
    pub h: f64,
    pub span: f32,
    pub axis: Axis,
    pub scale: i32,
}

/// What is asked. Widgets ask for their label and icon now; popups and
/// events, and the launcher's actions and sources, are added as kinds.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum RequestKind {
    /// The widget's `label()`.
    Label,
    /// The widget's `icon(tile)`.
    Icon(Tile),
    Popup,
    Interaction {
        key: String,
        event: PopupEvent,
    },
    Visibility(bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PopupEvent {
    Click,
    Toggle(bool),
    Change(f64),
    Commit(f64),
}

impl RequestKind {
    pub fn topic(&self) -> Topic {
        match self {
            RequestKind::Label => Topic::Label,
            RequestKind::Icon(_) => Topic::Icon,
            RequestKind::Popup => Topic::Popup,
            RequestKind::Interaction { .. } | RequestKind::Visibility(_) => Topic::Interaction,
        }
    }
}

/// What a request is about: a newer request of the same topic for the
/// same instance supersedes an older one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Topic {
    Label,
    Icon,
    Popup,
    Interaction,
}

/// An answer's payload.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Reply {
    /// None: the default label.
    Label(Option<String>),
    /// None: the app's own icon.
    Icon(Option<Arc<Node>>),
    Popup(Option<Arc<Node>>),
    Done,
}

/// A request as the worker sees it.
#[derive(Debug)]
pub struct Request {
    pub id: RequestId,
    pub kind: RequestKind,
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl Request {
    /// A request for `kind` that is due in `timeout`, and the host's
    /// handle on it.
    pub fn new(kind: RequestKind, timeout: Duration) -> (Request, Ticket) {
        let id = RequestId::next();
        let deadline = Instant::now() + timeout;
        let cancelled = Arc::new(AtomicBool::new(false));
        let ticket = Ticket {
            id,
            topic: kind.topic(),
            deadline,
            cancelled: cancelled.clone(),
        };
        let request = Request {
            id,
            kind,
            deadline,
            cancelled,
        };
        (request, ticket)
    }

    /// Still worth running at `now`: not cancelled, not past its deadline.
    pub fn live(&self, now: Instant) -> bool {
        !self.cancelled.load(Ordering::Relaxed) && now <= self.deadline
    }
}

/// The host's handle on a request it sent.
#[derive(Clone, Debug)]
pub struct Ticket {
    pub id: RequestId,
    pub topic: Topic,
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl Ticket {
    /// Tell the worker not to bother.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

/// An answer to a request, from the instance it went to.
#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub instance: InstanceId,
    pub id: RequestId,
    /// Err: the hook failed, or the instance isn't running.
    pub result: Result<Reply, String>,
}

/// The host's open requests: at most one per instance and topic.
#[derive(Debug, Default)]
pub struct Pending {
    open: HashMap<(InstanceId, Topic), Ticket>,
}

impl Pending {
    /// Keep `ticket` open for `instance`; the request it supersedes is
    /// cancelled.
    pub fn insert(&mut self, instance: &str, ticket: Ticket) {
        if let Some(old) = self
            .open
            .insert((instance.to_string(), ticket.topic), ticket)
        {
            old.cancel();
        }
    }

    /// The answer, if its request is open and the answer is on time at
    /// `now`; else None (late, superseded or cancelled: dropped). Either
    /// way the request is closed.
    pub fn accept(&mut self, answer: Answer, now: Instant) -> Option<Answer> {
        self.accept_topic(answer, now).map(|(_, a)| a)
    }

    /// As `accept`, with the topic the request was about.
    pub fn accept_topic(&mut self, answer: Answer, now: Instant) -> Option<(Topic, Answer)> {
        let key = (self.open.iter())
            .find(|((instance, _), t)| *instance == answer.instance && t.id == answer.id)
            .map(|(k, _)| k.clone())?;
        let ticket = self.open.remove(&key)?;
        (now <= ticket.deadline).then_some((key.1, answer))
    }

    /// Whether a request of `topic` is open for `instance`.
    pub fn is_open(&self, instance: &str, topic: Topic) -> bool {
        self.open.contains_key(&(instance.to_string(), topic))
    }

    /// Cancel and forget the requests of instances `keep` says no to.
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.open.retain(|(instance, _), t| {
            let kept = keep(instance);
            if !kept {
                t.cancel();
            }
            kept
        });
    }

    /// Forget the requests past their deadline at `now` (their answers
    /// would be dropped anyway).
    pub fn expire(&mut self, now: Instant) {
        self.open.retain(|_, t| now <= t.deadline);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(instance: &str, id: RequestId, text: &str) -> Answer {
        Answer {
            instance: instance.into(),
            id,
            result: Ok(Reply::Label(Some(text.into()))),
        }
    }

    #[test]
    fn an_answer_on_time_is_accepted_once() {
        let mut p = Pending::default();
        let (req, ticket) = Request::new(RequestKind::Label, Duration::from_secs(1));
        p.insert("a", ticket);
        assert!(p.is_open("a", Topic::Label));
        let now = Instant::now();
        assert!(req.live(now));
        assert!(p.accept(label("a", req.id, "x"), now).is_some());
        assert!(!p.is_open("a", Topic::Label));
        // Closed: a second answer is dropped.
        assert!(p.accept(label("a", req.id, "x"), now).is_none());
    }

    #[test]
    fn a_late_answer_is_dropped() {
        let mut p = Pending::default();
        let (req, ticket) = Request::new(RequestKind::Label, Duration::from_millis(10));
        p.insert("a", ticket);
        let late = req.deadline + Duration::from_millis(1);
        assert!(!req.live(late));
        assert!(p.accept(label("a", req.id, "x"), late).is_none());
        // And the request is closed.
        assert!(!p.is_open("a", Topic::Label));
    }

    #[test]
    fn asking_again_supersedes_and_cancels() {
        let mut p = Pending::default();
        let timeout = Duration::from_secs(1);
        let (old, t1) = Request::new(RequestKind::Label, timeout);
        p.insert("a", t1);
        let (new, t2) = Request::new(RequestKind::Label, timeout);
        p.insert("a", t2);
        let now = Instant::now();
        assert!(!old.live(now), "the worker skips the superseded request");
        assert!(p.accept(label("a", old.id, "old"), now).is_none());
        assert_eq!(
            p.accept(label("a", new.id, "new"), now).map(|a| a.result),
            Some(Ok(Reply::Label(Some("new".into()))))
        );
    }

    #[test]
    fn instances_are_apart() {
        let mut p = Pending::default();
        let timeout = Duration::from_secs(1);
        let (a, ta) = Request::new(RequestKind::Label, timeout);
        let (b, tb) = Request::new(RequestKind::Label, timeout);
        p.insert("a", ta);
        p.insert("b", tb);
        let now = Instant::now();
        // The id must match the instance it was sent to.
        assert!(p.accept(label("b", a.id, "x"), now).is_none());
        assert!(p.accept(label("a", a.id, "x"), now).is_some());
        p.retain(|i| i != "b");
        assert!(!b.live(now));
        assert!(p.accept(label("b", b.id, "x"), now).is_none());
    }

    #[test]
    fn expiring() {
        let mut p = Pending::default();
        let (req, ticket) = Request::new(RequestKind::Label, Duration::from_millis(5));
        p.insert("a", ticket);
        p.expire(req.deadline + Duration::from_millis(1));
        assert!(!p.is_open("a", Topic::Label));
    }
}
