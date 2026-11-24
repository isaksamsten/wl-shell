//! One plugin's Lua state: loading it into the sandbox, the instruction
//! budget, and calling its hooks and callbacks.

use std::collections::HashMap;
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::chunk::ChunkMode;
use mlua::{Function, HookTriggers, Lua, LuaOptions, StdLib, Table, Value, VmState};

use crate::caps::{self, Capabilities};
use crate::channel::{InstanceId, Tile};
use crate::host;
use crate::runtime::{Job, Link, Settings, lock};
use crate::spec::{AppInfo, InstanceSpec, Role};
use crate::subject::{Choice, Item, MAX_CHOICES, MAX_ITEMS, MAX_RESULTS, Subject};
use crate::ui::{Builder, Node};

/// The budget is counted in steps of this many instructions.
const STEP: u32 = 1000;
/// The longest label, in characters.
const MAX_LABEL: usize = 200;

/// Named registry tables: armed timers by id, spawned processes'
/// callbacks by id, and modules `require` loaded.
pub(crate) const TIMERS: &str = "wl_shell_timers";
pub(crate) const PROCS: &str = "wl_shell_procs";
pub(crate) const LOADED: &str = "wl_shell_loaded";
/// Handlers of the trees shown, by topic (`icon`), then by node key.
pub(crate) const HANDLERS: &str = "wl_shell_handlers";
/// D-Bus callbacks by id: calls waiting for replies, and watches.
pub(crate) const BUS: &str = "wl_shell_bus";
/// File watch callbacks by id.
pub(crate) const WATCHES: &str = "wl_shell_watches";

/// What a call into a state came to.
pub(crate) enum Outcome<T> {
    Ok(T),
    /// The hook failed; the state goes on.
    Error(String),
    /// The state can't go on (budget, memory): it is restarted.
    Fatal(String),
}

pub(crate) struct LoadError {
    pub message: String,
    /// Worth starting again later (else it waits for the file to change).
    pub retry: bool,
}

impl LoadError {
    fn fixed(message: impl ToString) -> Self {
        LoadError {
            message: message.to_string(),
            retry: false,
        }
    }
}

/// Instructions left for the hook call running.
pub(crate) struct Budget {
    limit: u64,
    used: AtomicU64,
    exceeded: AtomicBool,
}

impl Budget {
    /// Count a fresh hook call on `lua` (and stop the escalated hook of
    /// the last one).
    fn reset(self: &Arc<Self>, lua: &Lua) -> mlua::Result<()> {
        self.used.store(0, Ordering::Relaxed);
        if self.exceeded.swap(false, Ordering::Relaxed) {
            self.watch(lua)?;
        }
        Ok(())
    }

    /// Count `lua`'s instructions, `STEP` at a time.
    fn watch(self: &Arc<Self>, lua: &Lua) -> mlua::Result<()> {
        let b = self.clone();
        let triggers = HookTriggers::new().every_nth_instruction(STEP);
        lua.set_global_hook(triggers, move |lua, _| b.step(lua))
    }

    /// Every `STEP` instructions. Once over, the hook fails on every
    /// instruction and call, so the error escapes any `pcall` (a count
    /// hook alone may keep landing inside the protected function).
    fn step(&self, lua: &Lua) -> mlua::Result<VmState> {
        let used = self.used.fetch_add(STEP as u64, Ordering::Relaxed) + STEP as u64;
        if used > self.limit {
            self.exceeded.store(true, Ordering::Relaxed);
            let every = HookTriggers::new().every_nth_instruction(1).on_calls();
            lua.set_global_hook(every, |_, _| Err(over()))?;
            return Err(over());
        }
        Ok(VmState::Continue)
    }
}

fn over() -> mlua::Error {
    mlua::Error::runtime("instruction budget exceeded")
}

/// What the host functions of a state reach, kept in its app data.
pub(crate) struct Host {
    pub instance: InstanceId,
    pub plugin: String,
    pub epoch: u64,
    pub link: Link,
    pub redraw: AtomicBool,
    next: AtomicU64,
    /// Running processes, killed with the state.
    pub procs: Mutex<HashMap<u64, Arc<Mutex<Child>>>>,
    /// The plugin's folder, for `require`.
    pub dir: Option<std::path::PathBuf>,
    /// What `span(n)` set.
    pub span: Mutex<Option<crate::caps::Span>>,
    /// Loaded and set up: `span(n)` is news now.
    pub loaded: AtomicBool,
    /// D-Bus watches, cancelled with the state.
    pub subs: Mutex<Vec<(crate::dbus::BusKind, u64)>>,
    /// File watches, cancelled with the state.
    pub watches: Mutex<Vec<u64>>,
}

impl Host {
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// The host of the state `lua` belongs to.
pub(crate) fn host(lua: &Lua) -> mlua::Result<Arc<Host>> {
    (lua.app_data_ref::<Arc<Host>>())
        .map(|h| h.clone())
        .ok_or_else(|| mlua::Error::runtime("no host"))
}

/// `dock.app`: the names the dock knows the app by; nil without one.
fn app_table(lua: &Lua, app: Option<&AppInfo>) -> mlua::Result<Value> {
    let Some(app) = app else {
        return Ok(Value::Nil);
    };
    let t = lua.create_table()?;
    t.set("id", app.id.as_str())?;
    t.set("desktop", app.desktop.as_deref())?;
    t.set("wm_class", app.wm_class.as_deref())?;
    t.set("name", app.name.as_deref().unwrap_or(&app.id))?;
    Ok(Value::Table(t))
}

pub(crate) struct State {
    lua: Lua,
    plugin: Table,
    widget: Table,
    host: Arc<Host>,
    budget: Arc<Budget>,
}

impl Drop for State {
    fn drop(&mut self) {
        for (_, child) in lock(&self.host.procs).drain() {
            let _ = lock(&child).kill();
        }
        for (kind, id) in lock(&self.host.subs).drain(..) {
            crate::dbus::cancel(kind, id);
        }
        for id in lock(&self.host.watches).drain(..) {
            crate::watch::cancel(id);
        }
    }
}

impl State {
    /// Load `spec`'s plugin (backing `app`) and set it up (as generation
    /// `epoch`).
    pub fn load(
        spec: &InstanceSpec,
        app: Option<&AppInfo>,
        epoch: u64,
        link: Link,
        settings: &Settings,
    ) -> Result<(State, Capabilities), LoadError> {
        let code = match &spec.source {
            crate::Source::File(path) => std::fs::read_to_string(path)
                .map_err(|e| LoadError::fixed(format!("{}: {e}", path.display())))?,
            crate::Source::Code { code, .. } => code.to_string(),
        };
        let libs = StdLib::COROUTINE
            | StdLib::TABLE
            | StdLib::STRING
            | StdLib::UTF8
            | StdLib::MATH
            | StdLib::OS;
        let lua = Lua::new_with(libs, LuaOptions::new()).map_err(LoadError::fixed)?;
        lua.set_memory_limit(settings.memory)
            .map_err(LoadError::fixed)?;
        let budget = Arc::new(Budget {
            limit: spec.budget.max(STEP as u64),
            used: AtomicU64::new(0),
            exceeded: AtomicBool::new(false),
        });
        budget.watch(&lua).map_err(LoadError::fixed)?;
        let host = Arc::new(Host {
            instance: spec.id.clone(),
            plugin: spec.plugin.clone(),
            epoch,
            link,
            redraw: AtomicBool::new(false),
            next: AtomicU64::new(1),
            procs: Mutex::default(),
            dir: spec.source.dir(),
            span: Mutex::default(),
            loaded: AtomicBool::new(false),
            subs: Mutex::default(),
            watches: Mutex::default(),
        });
        lua.set_app_data(host.clone());
        host::install(&lua, settings).map_err(LoadError::fixed)?;
        // The app the widget backs, nil for a standalone one.
        (lua.globals().get::<Table>(settings.namespace))
            .and_then(|ns| ns.set("app", app_table(&lua, app)?))
            .map_err(LoadError::fixed)?;

        budget.reset(&lua).map_err(LoadError::fixed)?;
        let chunk = (lua.load(code))
            .set_name(spec.source.chunk_name())
            .set_mode(ChunkMode::Text);
        let plugin = match chunk.eval::<Value>() {
            Ok(Value::Table(t)) => t,
            Ok(v) => {
                return Err(LoadError::fixed(format!(
                    "the file must return a table, not {}",
                    v.type_name()
                )));
            }
            Err(_) if budget.exceeded.load(Ordering::Relaxed) => {
                return Err(LoadError::fixed("loading: instruction budget exceeded"));
            }
            Err(e) => return Err(LoadError::fixed(e)),
        };
        let caps = Capabilities::read(&plugin).map_err(LoadError::fixed)?;
        let widget = caps::widget_table(&plugin).map_err(LoadError::fixed)?;
        let state = State {
            lua,
            plugin,
            widget,
            host,
            budget,
        };
        if spec.role == Role::Widget || caps.launcher() {
            match state.setup(&spec.options) {
                Outcome::Ok(()) => {}
                Outcome::Error(message) | Outcome::Fatal(message) => {
                    return Err(LoadError {
                        message,
                        retry: true,
                    });
                }
            }
        }
        state.host.loaded.store(true, Ordering::Relaxed);
        Ok((state, caps))
    }

    /// Run `f` with a fresh budget; `what` names it in errors.
    fn enter<R>(&self, what: &str, f: impl FnOnce() -> mlua::Result<R>) -> Outcome<R> {
        if let Err(e) = self.budget.reset(&self.lua) {
            return Outcome::Fatal(format!("{what}: {e}"));
        }
        let r = f();
        if self.budget.exceeded.load(Ordering::Relaxed) {
            return Outcome::Fatal(format!(
                "{what}: instruction budget of {} exceeded",
                self.budget.limit
            ));
        }
        match r {
            Ok(v) => Outcome::Ok(v),
            Err(mlua::Error::MemoryError(m)) => Outcome::Fatal(format!("{what}: {m}")),
            Err(e) => Outcome::Error(format!("{what}: {e}")),
        }
    }

    /// The hook `name`, from the widget table or the plugin's.
    fn hook(&self, name: &str) -> Option<Function> {
        [&self.widget, &self.plugin]
            .into_iter()
            .find_map(|t| match t.get::<Value>(name) {
                Ok(Value::Function(f)) => Some(f),
                _ => None,
            })
    }

    fn setup(&self, options: &serde_json::Value) -> Outcome<()> {
        let Some(setup) = self.hook("setup") else {
            return Outcome::Ok(());
        };
        self.enter("setup", || {
            let opts = host::to_lua(&self.lua, options)?;
            setup.call::<()>(opts)
        })
    }

    /// Whether `redraw()` was called since the last time.
    pub fn take_redraw(&self) -> bool {
        self.host.redraw.swap(false, Ordering::Relaxed)
    }

    /// `label()`: the text, or None for the default.
    pub fn label(&self) -> Outcome<Option<String>> {
        let Some(label) = self.hook("label") else {
            return Outcome::Ok(None);
        };
        self.enter("label", || {
            let text = match label.call::<Value>(())? {
                Value::Nil | Value::Boolean(false) => return Ok(None),
                Value::String(s) => s.to_string_lossy(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => n.to_string(),
                v => {
                    return Err(mlua::Error::runtime(format!(
                        "expected a string or nil, got {}",
                        v.type_name()
                    )));
                }
            };
            let text = text.trim();
            Ok((!text.is_empty()).then(|| text.chars().take(MAX_LABEL).collect()))
        })
    }

    /// What `span(n)` set, if it was called.
    pub fn span(&self) -> Option<crate::caps::Span> {
        *lock(&self.host.span)
    }

    /// `icon(tile)`: the tree, or None for the app's own icon. Its
    /// handlers replace the last icon's.
    pub fn icon(&self, tile: &Tile) -> Outcome<Option<Node>> {
        let Some(icon) = self.hook("icon") else {
            return Outcome::Ok(None);
        };
        self.enter("icon", || {
            let t = self.lua.create_table()?;
            t.set("w", tile.w)?;
            t.set("h", tile.h)?;
            t.set("span", tile.span)?;
            t.set("axis", tile.axis.name())?;
            t.set("scale", tile.scale)?;
            let v = icon.call::<Value>(t)?;
            let mut builder = Builder::new(&self.lua)?;
            let tree = builder.root(v).map_err(mlua::Error::runtime)?;
            let handlers: Table = self.lua.named_registry_value(HANDLERS)?;
            handlers.set("icon", builder.handlers())?;
            Ok(tree)
        })
    }

    pub fn popup(&self) -> Outcome<Option<Node>> {
        let Some(popup) = self.hook("popup") else {
            return Outcome::Ok(None);
        };
        self.enter("popup", || {
            let mut builder = Builder::new(&self.lua)?;
            let tree = builder
                .root(popup.call::<Value>(())?)
                .map_err(mlua::Error::runtime)?;
            let handlers: Table = self.lua.named_registry_value(HANDLERS)?;
            handlers.set("popup", builder.handlers())?;
            Ok(tree)
        })
    }

    pub fn visibility(&self, open: bool) -> Outcome<()> {
        let name = if open { "on_open" } else { "on_close" };
        let Some(f) = self.hook(name) else {
            return Outcome::Ok(());
        };
        self.enter(name, || f.call::<()>(()))
    }

    /// A standalone widget's item was clicked: `on_click(button)`.
    pub fn click(&self, button: &str) -> Outcome<()> {
        let Some(f) = self.hook("on_click") else {
            return Outcome::Ok(());
        };
        self.enter("on_click", || f.call::<()>(button))
    }

    /// The launcher action `id`'s table.
    fn action(&self, id: &str) -> mlua::Result<Table> {
        let actions: Table = self.plugin.get("actions")?;
        for t in actions.sequence_values::<Table>() {
            let t = t?;
            if t.get::<Option<String>>("id")?.as_deref() == Some(id) {
                return Ok(t);
            }
        }
        Err(mlua::Error::runtime(format!("no action {id:?}")))
    }

    /// The launcher action `action`'s `choices(subject)`: a list of
    /// tables with an `id`, a `name` and an `icon` (nil: none).
    pub fn choices(&self, action: &str, subject: &Subject) -> Outcome<Vec<Choice>> {
        self.enter("choices", || {
            let f: Function = self.action(action)?.get("choices")?;
            let list = match f.call::<Value>(subject.to_lua(&self.lua)?)? {
                Value::Nil => return Ok(Vec::new()),
                Value::Table(t) => t,
                v => {
                    return Err(mlua::Error::runtime(format!(
                        "expected a list of choices, got {}",
                        v.type_name()
                    )));
                }
            };
            let mut choices: Vec<Choice> = Vec::new();
            for (i, v) in list.sequence_values::<Value>().enumerate() {
                let Value::Table(t) = v? else {
                    return Err(mlua::Error::runtime(format!(
                        "choice {}: expected a table",
                        i + 1
                    )));
                };
                let c = Choice::read(&self.lua, &t)
                    .map_err(|e| mlua::Error::runtime(format!("choice {}: {e}", i + 1)))?;
                // The first of an id counts.
                if !choices.iter().any(|o| o.id == c.id) {
                    choices.push(c);
                }
                if choices.len() == MAX_CHOICES {
                    break;
                }
            }
            Ok(choices)
        })
    }

    /// The launcher action `action`'s `run(subject, choice)`.
    pub fn run(&self, action: &str, subject: &Subject, choice: Option<&Choice>) -> Outcome<()> {
        self.enter("run", || {
            let f: Function = self.action(action)?.get("run")?;
            let choice = match choice {
                Some(c) => c.to_lua(&self.lua)?,
                None => Value::Nil,
            };
            f.call::<()>((subject.to_lua(&self.lua)?, choice))
        })
    }

    /// The launcher source `id`'s table.
    fn source(&self, id: &str) -> mlua::Result<Table> {
        let sources: Table = self.plugin.get("sources")?;
        for t in sources.sequence_values::<Table>() {
            let t = t?;
            if t.get::<Option<String>>("id")?.as_deref() == Some(id) {
                return Ok(t);
            }
        }
        Err(mlua::Error::runtime(format!("no source {id:?}")))
    }

    /// The source `source`'s `items()`: the list of its items.
    pub fn items(&self, source: &str) -> Outcome<Vec<Item>> {
        self.enter("items", || {
            let f: Function = self.source(source)?.get("items")?;
            let v = f.call::<Value>(())?;
            Item::list(&self.lua, v, MAX_ITEMS).map_err(mlua::Error::runtime)
        })
    }

    /// The source `source`'s `search(query, emit)` for the query of
    /// `generation`. Each `emit(items)` (during the call or later, from a
    /// timer or a process) is an [`Event::Emit`](crate::Event::Emit); a
    /// list `search` returns is emitted too.
    pub fn search(&self, source: &str, query: &str, generation: u64) -> Outcome<()> {
        self.enter("search", || {
            let f: Function = self.source(source)?.get("search")?;
            let id = source.to_string();
            let emit = self.lua.create_function(move |lua, v: Value| {
                let items = Item::list(lua, v, MAX_RESULTS)
                    .map_err(|e| mlua::Error::runtime(format!("emit: {e}")))?;
                let host = host(lua)?;
                host.link.emit(crate::Event::Emit {
                    instance: host.instance.clone(),
                    source: id.clone(),
                    generation,
                    items,
                });
                Ok(())
            })?;
            match f.call::<Value>((query, emit.clone()))? {
                v @ Value::Table(_) => emit.call::<()>(v),
                _ => Ok(()),
            }
        })
    }

    /// The source `source`'s `run(item)`.
    pub fn open(&self, source: &str, item: &Item) -> Outcome<()> {
        self.enter("run", || {
            let f: Function = self.source(source)?.get("run")?;
            f.call::<()>(item.to_lua(&self.lua)?)
        })
    }

    pub fn interaction(&self, key: &str, event: crate::channel::PopupEvent) -> Outcome<()> {
        use crate::channel::PopupEvent::*;
        let name = match event {
            Click => "on_click",
            Commit(_) => "on_commit",
            _ => "on_change",
        };
        self.enter(name, || {
            let handlers: Table = self.lua.named_registry_value(HANDLERS)?;
            let Some(popup) = handlers.get::<Option<Table>>("popup")? else {
                return Ok(());
            };
            let Some(node) = popup.get::<Option<Table>>(key)? else {
                return Ok(());
            };
            let Some(f) = node.get::<Option<Function>>(name)? else {
                return Ok(());
            };
            match event {
                Click => f.call::<()>(()),
                Toggle(v) => f.call::<()>(v),
                Change(v) | Commit(v) => f.call::<()>(v),
            }
        })
    }

    /// Timer `id`, due at `due`, fired.
    pub fn timer(&self, id: u64, due: Instant) -> Outcome<()> {
        let found = (|| -> mlua::Result<Option<Function>> {
            let timers: Table = self.lua.named_registry_value(TIMERS)?;
            let Some(entry) = timers.get::<Option<Table>>(id)? else {
                return Ok(None);
            };
            match entry.get::<Option<u64>>("every")? {
                Some(ms) => {
                    let next = (due + Duration::from_millis(ms)).max(Instant::now());
                    let epoch = self.host.epoch;
                    (self.host.link).at(
                        next,
                        Job::Timer {
                            epoch,
                            id,
                            due: next,
                        },
                    );
                }
                None => timers.set(id, Value::Nil)?,
            }
            entry.get("fn")
        })();
        match found {
            Ok(Some(f)) => self.enter("timer", || f.call::<()>(())),
            Ok(None) => Outcome::Ok(()),
            Err(e) => Outcome::Error(format!("timer: {e}")),
        }
    }

    /// The callbacks of process `proc`.
    fn proc_entry(&self, proc: u64) -> mlua::Result<Option<Table>> {
        let procs: Table = self.lua.named_registry_value(PROCS)?;
        procs.get(proc)
    }

    /// A line from process `proc`'s output.
    pub fn line(&self, proc: u64, line: String) -> Outcome<()> {
        match self.proc_entry(proc).and_then(|e| match e {
            Some(e) => e.get::<Option<Function>>("on_line"),
            None => Ok(None),
        }) {
            Ok(Some(f)) => self.enter("on_line", || f.call::<()>(line)),
            Ok(None) => Outcome::Ok(()),
            Err(e) => Outcome::Error(format!("on_line: {e}")),
        }
    }

    /// Process `proc` exited (None: by a signal).
    pub fn exit(&self, proc: u64, code: Option<i32>) -> Outcome<()> {
        lock(&self.host.procs).remove(&proc);
        let found = (|| -> mlua::Result<Option<Function>> {
            let procs: Table = self.lua.named_registry_value(PROCS)?;
            let Some(entry) = procs.get::<Option<Table>>(proc)? else {
                return Ok(None);
            };
            procs.set(proc, Value::Nil)?;
            entry.get("on_exit")
        })();
        match found {
            Ok(Some(f)) => self.enter("on_exit", || f.call::<()>(code)),
            Ok(None) => Outcome::Ok(()),
            Err(e) => Outcome::Error(format!("on_exit: {e}")),
        }
    }

    /// The dock knows the app by other names now: `dock.app` changes (in
    /// place, so tables kept from it see the change), `on_app()` hears
    /// of it, and the plugin redraws.
    pub fn set_app(&self, app: &AppInfo, namespace: &str) -> Outcome<()> {
        let set = (|| {
            let ns: Table = self.lua.globals().get(namespace)?;
            let Value::Table(new) = app_table(&self.lua, Some(app))? else {
                return Ok(());
            };
            match ns.get::<Value>("app")? {
                Value::Table(old) => {
                    for key in ["id", "desktop", "wm_class", "name"] {
                        old.set(key, new.get::<Value>(key)?)?;
                    }
                    Ok(())
                }
                _ => ns.set("app", new),
            }
        })();
        if let Err(e) = set {
            return Outcome::Error(format!("app: {e}"));
        }
        self.host.redraw.store(true, Ordering::Relaxed);
        match self.hook("on_app") {
            Some(f) => self.enter("on_app", || f.call::<()>(())),
            None => Outcome::Ok(()),
        }
    }

    /// Something from D-Bus for callback `id`.
    pub fn bus(&self, id: u64, event: crate::dbus::BusEvent) -> Outcome<()> {
        let found = (|| -> mlua::Result<Option<Table>> {
            let callbacks: Table = self.lua.named_registry_value(BUS)?;
            let entry = callbacks.get::<Option<Table>>(id)?;
            // A call is answered once.
            if matches!(event, crate::dbus::BusEvent::Reply(_)) {
                callbacks.set(id, Value::Nil)?;
            }
            Ok(entry)
        })();
        match found {
            Ok(Some(entry)) => {
                self.enter("dbus", || crate::dbus::deliver(&self.lua, &entry, event))
            }
            Ok(None) => Outcome::Ok(()),
            Err(e) => Outcome::Error(format!("dbus: {e}")),
        }
    }

    /// Watch `id` saw `paths` change.
    pub fn watch(&self, id: u64, paths: Vec<String>) -> Outcome<()> {
        let found = (|| -> mlua::Result<Option<Function>> {
            let callbacks: Table = self.lua.named_registry_value(WATCHES)?;
            callbacks.get(id)
        })();
        match found {
            Ok(Some(f)) => self.enter("watch", || {
                let paths = self.lua.create_sequence_from(paths)?;
                f.call::<()>(paths)
            }),
            Ok(None) => Outcome::Ok(()),
            Err(e) => Outcome::Error(format!("watch: {e}")),
        }
    }
}
