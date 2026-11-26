//! The thread pool, the timer thread, and each instance's queue of jobs.
//!
//! An instance is a [`Slot`]: its spec, a queue of jobs, and its Lua
//! state. Pushing a job onto an idle slot puts the slot on the pool's
//! ready queue; a worker takes it, runs a batch of its jobs in order, and
//! puts it back if more are waiting, so a state runs on one thread at a
//! time and a busy one doesn't starve the others. Timers (and restarts)
//! wait on the timer thread, which pushes their jobs when they are due.
//!
//! Each (re)start of a state is a new generation: jobs of an older one
//! (a timer it armed, a line from a process it spawned) are ignored.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wl_shell_log::warn;

use crate::caps::Capabilities;
use crate::channel::{Answer, InstanceId, Reply, Request, RequestKind, Ticket};
use crate::spec::{AppInfo, InstanceSpec, Role};
use crate::state::{LoadError, Outcome, State};

/// Jobs a worker runs of one slot before it lets the others go first.
const BATCH: usize = 16;
/// The first restart's delay; each failure in a row doubles it.
const BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// A state that ran this long before failing starts over at `BACKOFF`.
const HEALTHY: Duration = Duration::from_secs(60);

/// What the runtime tells its host.
#[derive(Debug)]
pub enum Event {
    /// An instance loaded (and was set up); this is what it provides.
    Loaded {
        instance: InstanceId,
        caps: Capabilities,
    },
    /// An instance failed to load or crashed: what it provided is gone
    /// (until it loads again).
    Failed {
        instance: InstanceId,
        error: String,
    },
    Answer(Answer),
    /// The plugin's state changed (`redraw()`): ask again for what it
    /// shows.
    Redraw(InstanceId),
    /// The plugin's items changed (`invalidate()`), for the launcher.
    Invalidate(InstanceId),
    /// A source's `search` emitted results for the query of
    /// `generation` (the request's): they replace what it emitted for
    /// it before. The host drops those of an older query.
    Emit {
        instance: InstanceId,
        source: String,
        generation: u64,
        items: Vec<crate::subject::Item>,
    },
    /// `clipboard(text)`.
    Clipboard {
        instance: InstanceId,
        text: String,
    },
    /// `span(s)` changed the widget's span (after it loaded; before, it
    /// is in the capabilities).
    Span {
        instance: InstanceId,
        span: crate::caps::Spans,
    },
}

/// How the runtime runs.
#[derive(Clone, Debug)]
pub struct Settings {
    /// Worker threads.
    pub threads: usize,
    /// The global table of host functions (`dock`).
    pub namespace: &'static str,
    /// Bytes of memory per state.
    pub memory: usize,
    /// Modules `require` finds after the plugin's own folder: (name,
    /// code).
    pub modules: &'static [(&'static str, &'static str)],
}

impl Default for Settings {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map_or(2, |n| n.get());
        Settings {
            threads: cpus.clamp(2, 4),
            namespace: "dock",
            memory: 64 << 20,
            modules: &[],
        }
    }
}

type Sink = Box<dyn Fn(Event) + Send + Sync>;

pub(crate) struct Shared {
    sink: Sink,
    pub(crate) settings: Settings,
    run: Mutex<RunQueue>,
    run_cv: Condvar,
    timers: Mutex<TimerQueue>,
    timer_cv: Condvar,
    slots: Mutex<HashMap<InstanceId, Arc<Slot>>>,
}

#[derive(Default)]
struct RunQueue {
    ready: VecDeque<Arc<Slot>>,
    quit: bool,
}

#[derive(Default)]
struct TimerQueue {
    heap: BinaryHeap<Reverse<Timed>>,
    seq: u64,
    quit: bool,
}

/// A job waiting for its time.
struct Timed {
    due: Instant,
    seq: u64,
    slot: Weak<Slot>,
    job: Job,
}

impl PartialEq for Timed {
    fn eq(&self, other: &Self) -> bool {
        (self.due, self.seq) == (other.due, other.seq)
    }
}
impl Eq for Timed {}
impl PartialOrd for Timed {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timed {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.due, self.seq).cmp(&(other.due, other.seq))
    }
}

/// Lock, through a poisoning (a panic in a hook is caught and handled).
pub(crate) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn emit(&self, event: Event) {
        (self.sink)(event);
    }

    fn ready(&self, slot: Arc<Slot>) {
        lock(&self.run).ready.push_back(slot);
        self.run_cv.notify_one();
    }

    fn at(&self, due: Instant, slot: Weak<Slot>, job: Job) {
        let mut q = lock(&self.timers);
        q.seq += 1;
        let seq = q.seq;
        q.heap.push(Reverse(Timed {
            due,
            seq,
            slot,
            job,
        }));
        drop(q);
        self.timer_cv.notify_one();
    }
}

pub(crate) enum Job {
    /// (Re)load the state; `fresh` forgets earlier failures.
    Start {
        fresh: bool,
    },
    Stop,
    /// A restart after a failure of generation `epoch`.
    Restart {
        epoch: u64,
    },
    Request(Request),
    Timer {
        epoch: u64,
        id: u64,
        due: Instant,
    },
    Line {
        epoch: u64,
        proc: u64,
        line: String,
    },
    Exit {
        epoch: u64,
        proc: u64,
        code: Option<i32>,
    },
    /// D-Bus news for callback `id`.
    Bus {
        epoch: u64,
        id: u64,
        event: crate::dbus::BusEvent,
    },
    /// Files watched by `id` changed.
    Watch {
        epoch: u64,
        id: u64,
        paths: Vec<String>,
    },
    /// The app is known by other names now.
    App(AppInfo),
}

/// A state's way back to its slot and the runtime, from host functions
/// and the threads reading its processes.
#[derive(Clone)]
pub(crate) struct Link {
    slot: Weak<Slot>,
    shared: Weak<Shared>,
}

impl Link {
    /// Queue `job` for the state; false if it is gone.
    pub(crate) fn push(&self, job: Job) -> bool {
        match (self.slot.upgrade(), self.shared.upgrade()) {
            (Some(slot), Some(shared)) => {
                Slot::push(&slot, &shared, job);
                true
            }
            _ => false,
        }
    }

    /// Queue `job` for the state at `due`.
    pub(crate) fn at(&self, due: Instant, job: Job) {
        if let Some(shared) = self.shared.upgrade() {
            shared.at(due, self.slot.clone(), job);
        }
    }

    pub(crate) fn emit(&self, event: Event) {
        if let Some(shared) = self.shared.upgrade() {
            shared.emit(event);
        }
    }

    /// Whether the slot still exists.
    pub(crate) fn alive(&self) -> bool {
        self.slot.strong_count() > 0
    }
}

pub(crate) struct Slot {
    id: InstanceId,
    spec: Mutex<InstanceSpec>,
    /// What `set_app` learned, over the spec's.
    app: Mutex<Option<AppInfo>>,
    jobs: Mutex<Jobs>,
    life: Mutex<Life>,
}

#[derive(Default)]
struct Jobs {
    queue: VecDeque<Job>,
    /// On the ready queue, or being run.
    scheduled: bool,
}

#[derive(Default)]
struct Life {
    state: Option<State>,
    epoch: u64,
    /// Failures in a row, for the backoff.
    failures: u32,
    started: Option<Instant>,
    removed: bool,
    /// The last error logged, so a repeating one is said once.
    last_error: Option<String>,
}

impl Slot {
    fn push(slot: &Arc<Slot>, shared: &Shared, job: Job) {
        let mut jobs = lock(&slot.jobs);
        jobs.queue.push_back(job);
        if !jobs.scheduled {
            jobs.scheduled = true;
            drop(jobs);
            shared.ready(slot.clone());
        }
    }

    fn link(self: &Arc<Self>, shared: &Arc<Shared>) -> Link {
        Link {
            slot: Arc::downgrade(self),
            shared: Arc::downgrade(shared),
        }
    }

    fn run(self: &Arc<Self>, shared: &Arc<Shared>, job: Job) {
        let mut life = lock(&self.life);
        let life = &mut *life;
        match job {
            Job::Start { fresh } => {
                if fresh {
                    life.failures = 0;
                    life.last_error = None;
                }
                self.start(life, shared);
            }
            Job::Restart { epoch } => {
                if life.epoch == epoch && life.state.is_none() && !life.removed {
                    self.start(life, shared);
                }
            }
            Job::Stop => {
                life.removed = true;
                life.state = None;
                life.epoch += 1;
            }
            Job::Request(request) => {
                if !request.live(Instant::now()) {
                    return;
                }
                let result = match &life.state {
                    None => Err("not running".to_string()),
                    Some(state) => match request.kind {
                        RequestKind::Label => {
                            let outcome = state.label();
                            self.settle(life, shared, outcome).map(Reply::Label)
                        }
                        RequestKind::Icon(tile) => {
                            let outcome = state.icon(&tile);
                            (self.settle(life, shared, outcome))
                                .map(|tree| Reply::Icon(tree.map(Arc::new)))
                        }
                        RequestKind::Popup => {
                            let outcome = state.popup();
                            self.settle(life, shared, outcome)
                                .map(|tree| Reply::Popup(tree.map(Arc::new)))
                        }
                        RequestKind::Interaction { key, event } => {
                            let outcome = state.interaction(&key, event);
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                        RequestKind::Visibility(open) => {
                            let outcome = state.visibility(open);
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                        RequestKind::Click(button) => {
                            let outcome = state.click(&button);
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                        RequestKind::Choices { action, subject } => {
                            let outcome = state.choices(&action, &subject);
                            self.settle(life, shared, outcome).map(Reply::Choices)
                        }
                        RequestKind::Run {
                            action,
                            subject,
                            choice,
                        } => {
                            let outcome = state.run(&action, &subject, choice.as_ref());
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                        RequestKind::Items { source } => {
                            let outcome = state.items(&source);
                            self.settle(life, shared, outcome).map(Reply::Items)
                        }
                        RequestKind::Search {
                            source,
                            query,
                            generation,
                        } => {
                            let outcome = state.search(&source, &query, generation);
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                        RequestKind::Open { source, item } => {
                            let outcome = state.open(&source, &item);
                            self.settle(life, shared, outcome).map(|()| Reply::Done)
                        }
                    },
                };
                shared.emit(Event::Answer(Answer {
                    instance: self.id.clone(),
                    id: request.id,
                    result,
                }));
            }
            Job::Timer { epoch, id, due } => {
                if let Some(state) = life.state.as_ref().filter(|_| life.epoch == epoch) {
                    let outcome = state.timer(id, due);
                    let _ = self.settle(life, shared, outcome);
                }
            }
            Job::Line { epoch, proc, line } => {
                if let Some(state) = life.state.as_ref().filter(|_| life.epoch == epoch) {
                    let outcome = state.line(proc, line);
                    let _ = self.settle(life, shared, outcome);
                }
            }
            Job::Exit { epoch, proc, code } => {
                if let Some(state) = life.state.as_ref().filter(|_| life.epoch == epoch) {
                    let outcome = state.exit(proc, code);
                    let _ = self.settle(life, shared, outcome);
                }
            }
            Job::Bus { epoch, id, event } => {
                if let Some(state) = life.state.as_ref().filter(|_| life.epoch == epoch) {
                    let outcome = state.bus(id, event);
                    let _ = self.settle(life, shared, outcome);
                }
            }
            Job::Watch { epoch, id, paths } => {
                if let Some(state) = life.state.as_ref().filter(|_| life.epoch == epoch) {
                    let outcome = state.watch(id, paths);
                    let _ = self.settle(life, shared, outcome);
                }
            }
            Job::App(app) => {
                if let Some(state) = &life.state {
                    let outcome = state.set_app(&app, shared.settings.namespace);
                    let _ = self.settle(life, shared, outcome);
                }
            }
        }
        if life.state.as_ref().is_some_and(State::take_redraw) {
            shared.emit(Event::Redraw(self.id.clone()));
        }
    }

    /// Load the state anew (dropping the old one).
    fn start(self: &Arc<Self>, life: &mut Life, shared: &Arc<Shared>) {
        life.state = None;
        life.epoch += 1;
        if life.removed {
            return;
        }
        let spec = lock(&self.spec).clone();
        let app = lock(&self.app).clone().or_else(|| spec.app.clone());
        match State::load(
            &spec,
            app.as_ref(),
            life.epoch,
            self.link(shared),
            &shared.settings,
        ) {
            Ok((state, mut caps)) => {
                life.started = Some(Instant::now());
                // `span(s)` during setup is the widget's span.
                if let (Some(w), Some(span)) = (caps.widget.as_mut(), state.span()) {
                    w.span = span;
                }
                // A plugin table's state is only for the launcher.
                if spec.role == Role::Widget || caps.launcher() {
                    life.state = Some(state);
                }
                shared.emit(Event::Loaded {
                    instance: self.id.clone(),
                    caps,
                });
            }
            Err(LoadError { message, retry }) => {
                let instance = self.id.clone();
                match retry {
                    true => self.crashed(life, shared, &spec, message),
                    false => {
                        warn!("plugin {}: {message}", spec.plugin);
                        shared.emit(Event::Failed {
                            instance,
                            error: message,
                        });
                    }
                }
            }
        }
    }

    /// The result of a call into the state; a fatal error drops it.
    fn settle<T>(
        self: &Arc<Self>,
        life: &mut Life,
        shared: &Arc<Shared>,
        outcome: Outcome<T>,
    ) -> Result<T, String> {
        match outcome {
            Outcome::Ok(v) => {
                life.last_error = None;
                Ok(v)
            }
            Outcome::Error(e) => {
                if life.last_error.as_ref() != Some(&e) {
                    warn!("plugin {}: {e}", lock(&self.spec).plugin);
                    life.last_error = Some(e.clone());
                }
                Err(e)
            }
            Outcome::Fatal(e) => {
                let spec = lock(&self.spec).clone();
                self.crashed(life, shared, &spec, e.clone());
                Err(e)
            }
        }
    }

    /// Drop the state and start it again after the backoff.
    fn crashed(
        self: &Arc<Self>,
        life: &mut Life,
        shared: &Arc<Shared>,
        spec: &InstanceSpec,
        error: String,
    ) {
        life.state = None;
        if life.started.is_some_and(|t| t.elapsed() >= HEALTHY) {
            life.failures = 0;
        }
        life.started = None;
        let delay = BACKOFF
            .saturating_mul(1 << life.failures.min(16))
            .min(MAX_BACKOFF);
        life.failures += 1;
        warn!(
            "plugin {}: {error}; restarting in {}s",
            spec.plugin,
            delay.as_secs()
        );
        shared.at(
            Instant::now() + delay,
            Arc::downgrade(self),
            Job::Restart { epoch: life.epoch },
        );
        shared.emit(Event::Failed {
            instance: self.id.clone(),
            error,
        });
    }
}

/// Lua states on a thread pool.
pub struct Runtime {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
}

impl Runtime {
    /// Start the threads; events go to `sink` (from any of them).
    pub fn new(
        settings: Settings,
        sink: impl Fn(Event) + Send + Sync + 'static,
    ) -> std::io::Result<Self> {
        let workers = settings.threads.max(1);
        let shared = Arc::new(Shared {
            sink: Box::new(sink),
            settings,
            run: Mutex::default(),
            run_cv: Condvar::new(),
            timers: Mutex::default(),
            timer_cv: Condvar::new(),
            slots: Mutex::default(),
        });
        let mut runtime = Runtime {
            shared: shared.clone(),
            threads: Vec::new(),
        };
        for i in 0..workers {
            let shared = shared.clone();
            let thread = std::thread::Builder::new()
                .name(format!("lua-{i}"))
                .spawn(move || work(shared))?;
            runtime.threads.push(thread);
        }
        let thread = std::thread::Builder::new()
            .name("lua-timers".into())
            .spawn(move || tick(shared))?;
        runtime.threads.push(thread);
        Ok(runtime)
    }

    /// Run exactly these instances: new ones start, changed ones
    /// restart, the others stop.
    pub fn configure(&self, specs: Vec<InstanceSpec>) {
        let shared = &self.shared;
        let mut slots = lock(&shared.slots);
        slots.retain(|id, slot| {
            let keep = specs.iter().any(|s| s.id == *id);
            if !keep {
                Slot::push(slot, shared, Job::Stop);
            }
            keep
        });
        for spec in specs {
            match slots.get(&spec.id) {
                Some(slot) => {
                    let mut current = lock(&slot.spec);
                    if *current != spec {
                        *current = spec;
                        drop(current);
                        Slot::push(slot, shared, Job::Start { fresh: true });
                    }
                }
                None => {
                    let slot = Arc::new(Slot {
                        id: spec.id.clone(),
                        spec: Mutex::new(spec.clone()),
                        app: Mutex::default(),
                        jobs: Mutex::default(),
                        life: Mutex::default(),
                    });
                    Slot::push(&slot, shared, Job::Start { fresh: true });
                    slots.insert(spec.id, slot);
                }
            }
        }
    }

    /// The file at `path` changed: restart the instances running it.
    pub fn reload(&self, path: &Path) {
        let slots = lock(&self.shared.slots);
        for slot in slots.values() {
            if lock(&slot.spec).source.path() == Some(path) {
                Slot::push(slot, &self.shared, Job::Start { fresh: true });
            }
        }
    }

    /// Ask `instance` for `kind`, due in `timeout`; None if there is no
    /// such instance.
    pub fn request(&self, instance: &str, kind: RequestKind, timeout: Duration) -> Option<Ticket> {
        let slot = lock(&self.shared.slots).get(instance).cloned()?;
        let (request, ticket) = Request::new(kind, timeout);
        Slot::push(&slot, &self.shared, Job::Request(request));
        Some(ticket)
    }

    /// `instance`'s app is known by these names now (`dock.app`); a
    /// running state hears of it, and a (re)started one starts with it.
    pub fn set_app(&self, instance: &str, app: AppInfo) {
        let Some(slot) = lock(&self.shared.slots).get(instance).cloned() else {
            return;
        };
        let mut known = lock(&slot.app);
        let current = known.clone().or_else(|| lock(&slot.spec).app.clone());
        if current.as_ref() == Some(&app) {
            return;
        }
        *known = Some(app.clone());
        drop(known);
        Slot::push(&slot, &self.shared, Job::App(app));
    }

    /// The instances configured.
    pub fn instances(&self) -> Vec<InstanceId> {
        lock(&self.shared.slots).keys().cloned().collect()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        lock(&self.shared.run).quit = true;
        self.shared.run_cv.notify_all();
        lock(&self.shared.timers).quit = true;
        self.shared.timer_cv.notify_all();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        // The states go (and kill their processes).
        let slots: Vec<_> = lock(&self.shared.slots).drain().map(|(_, s)| s).collect();
        for slot in slots {
            lock(&slot.life).state = None;
        }
        lock(&self.shared.run).ready.clear();
        lock(&self.shared.timers).heap.clear();
    }
}

/// A worker: run the ready slots' jobs.
fn work(shared: Arc<Shared>) {
    loop {
        let slot = {
            let mut q = lock(&shared.run);
            loop {
                if q.quit {
                    return;
                }
                if let Some(slot) = q.ready.pop_front() {
                    break slot;
                }
                q = shared.run_cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        for _ in 0..BATCH {
            let Some(job) = lock(&slot.jobs).queue.pop_front() else {
                break;
            };
            let ran = std::panic::catch_unwind(AssertUnwindSafe(|| slot.run(&shared, job)));
            if ran.is_err() {
                // The state may be broken: start over, as after a crash.
                let mut life = lock(&slot.life);
                let spec = lock(&slot.spec).clone();
                slot.crashed(&mut life, &shared, &spec, "panicked".into());
            }
        }
        let mut jobs = lock(&slot.jobs);
        match jobs.queue.is_empty() {
            true => jobs.scheduled = false,
            false => {
                drop(jobs);
                shared.ready(slot);
            }
        }
    }
}

/// The timer thread: push the jobs that are due.
fn tick(shared: Arc<Shared>) {
    let mut q = lock(&shared.timers);
    loop {
        if q.quit {
            return;
        }
        let now = Instant::now();
        match q.heap.peek().map(|Reverse(t)| t.due) {
            Some(due) if due <= now => {
                let Some(Reverse(t)) = q.heap.pop() else {
                    continue;
                };
                drop(q);
                if let Some(slot) = t.slot.upgrade() {
                    Slot::push(&slot, &shared, t.job);
                }
                q = lock(&shared.timers);
            }
            Some(due) => {
                q = (shared.timer_cv.wait_timeout(q, due - now))
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            None => q = shared.timer_cv.wait(q).unwrap_or_else(|e| e.into_inner()),
        }
    }
}
