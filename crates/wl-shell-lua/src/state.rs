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
use crate::channel::InstanceId;
use crate::host;
use crate::runtime::{Job, Link, Settings, lock};
use crate::spec::{InstanceSpec, Role};

/// The budget is counted in steps of this many instructions.
const STEP: u32 = 1000;
/// The longest label, in characters.
const MAX_LABEL: usize = 200;

/// Named registry tables: armed timers by id, spawned processes'
/// callbacks by id, and modules `require` loaded.
pub(crate) const TIMERS: &str = "wl_shell_timers";
pub(crate) const PROCS: &str = "wl_shell_procs";
pub(crate) const LOADED: &str = "wl_shell_loaded";

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
    }
}

impl State {
    /// Load `spec`'s plugin and set it up (as generation `epoch`).
    pub fn load(
        spec: &InstanceSpec,
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
        });
        lua.set_app_data(host.clone());
        host::install(&lua, settings.namespace).map_err(LoadError::fixed)?;

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
}
