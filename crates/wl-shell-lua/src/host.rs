//! The sandbox and the host functions a plugin calls.
//!
//! | Function                          | Does                                                                  |
//! | --------------------------------- | --------------------------------------------------------------------- |
//! | `redraw()`                        | Asks the host to ask again for what the plugin shows.                 |
//! | `invalidate()`                    | Says the plugin's launcher items changed.                             |
//! | `timer(ms, fn)` / `every(ms, fn)` | Calls `fn` once after `ms`, or every `ms`; returns a handle with `cancel()`. |
//! | `spawn(argv, {on_line, on_exit})` | Runs a process, calling `on_line(line)` per line of its output and `on_exit(code)`; returns a handle with `kill()`. |
//! | `run(argv)`                       | Runs a process detached; true, or nil and the error.                  |
//! | `json.decode(s)` / `json.encode(v)` | JSON; `null` is `nil`.                                              |
//! | `read(path)`                      | A file's text (`~/` expands), or nil and the error.                   |
//! | `clipboard(text)`                 | Copies `text`.                                                        |
//! | `span(n)`                         | Sets the tile's length along the dock: 1, 1.5 or 2 (or `{horizontal = 2, vertical = 1}`). |
//! | `ui.*`, `theme.*`                 | The UI constructors and the theme's color names (`ui.rs`).            |
//!
//! `argv` is a list of strings, or one string run with `sh -c`.

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::chunk::ChunkMode;
use mlua::serde::{DeserializeOptions, SerializeOptions};
use mlua::{Function, Lua, LuaSerdeExt, MultiValue, Table, UserData, UserDataMethods, Value};
use wl_shell_log::warn;

use crate::runtime::{Event, Job, lock};
use crate::state::{HANDLERS, LOADED, PROCS, TIMERS, host};

/// The largest file `read` reads.
const MAX_READ: u64 = 16 << 20;
/// The shortest interval of `every`.
const MIN_EVERY: u64 = 10;
/// How often a process whose output closed is checked for its exit.
const EXIT_POLL: Duration = Duration::from_millis(50);

/// Sandbox the globals of `lua` and add the host functions as
/// `namespace`.
pub(crate) fn install(lua: &Lua, namespace: &str) -> mlua::Result<()> {
    let g = lua.globals();
    for name in ["dofile", "loadfile"] {
        g.set(name, Value::Nil)?;
    }
    let string: Table = g.get("string")?;
    string.set("dump", Value::Nil)?;
    // `os` keeps only the clock and the environment.
    let os: Table = g.get("os")?;
    let safe = lua.create_table()?;
    for name in ["clock", "date", "difftime", "getenv", "time"] {
        safe.set(name, os.get::<Value>(name)?)?;
    }
    g.set("os", safe)?;
    for name in [TIMERS, PROCS, LOADED, HANDLERS] {
        lua.set_named_registry_value(name, lua.create_table()?)?;
    }
    // `load` takes text only; `print` goes to the log.
    let log = lua.create_function(|lua, text: String| {
        let host = host(lua)?;
        warn!("plugin {}: {text}", host.plugin);
        Ok(())
    })?;
    lua.load(
        r#"
        local load, log, tostring, select, concat = load, ...
        _G.load = function(chunk, name, mode, ...) return load(chunk, name, 't', ...) end
        _G.print = function(...)
          local t = {}
          for i = 1, select('#', ...) do t[i] = tostring((select(i, ...))) end
          log(concat(t, '\t'))
        end
        "#,
    )
    .set_name("=sandbox")
    .call::<()>((
        log,
        g.get::<Value>("tostring")?,
        g.get::<Value>("select")?,
        {
            let table: Table = g.get("table")?;
            table.get::<Value>("concat")?
        },
    ))?;
    g.set("require", lua.create_function(require)?)?;

    let ns = lua.create_table()?;
    ns.set(
        "redraw",
        lua.create_function(|lua, _: MultiValue| {
            host(lua)?
                .redraw
                .store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        })?,
    )?;
    ns.set(
        "invalidate",
        lua.create_function(|lua, _: MultiValue| {
            let host = host(lua)?;
            host.link.emit(Event::Invalidate(host.instance.clone()));
            Ok(())
        })?,
    )?;
    ns.set(
        "timer",
        lua.create_function(|lua, (ms, f): (f64, Function)| arm(lua, ms, f, false))?,
    )?;
    ns.set(
        "every",
        lua.create_function(|lua, (ms, f): (f64, Function)| arm(lua, ms, f, true))?,
    )?;
    ns.set("spawn", lua.create_function(spawn)?)?;
    ns.set("run", lua.create_function(run)?)?;
    ns.set("read", lua.create_function(read)?)?;
    ns.set(
        "clipboard",
        lua.create_function(|lua, text: String| {
            let host = host(lua)?;
            let instance = host.instance.clone();
            host.link.emit(Event::Clipboard { instance, text });
            Ok(())
        })?,
    )?;
    let json = lua.create_table()?;
    json.set(
        "decode",
        lua.create_function(|lua, text: String| {
            let v: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| mlua::Error::runtime(format!("json.decode: {e}")))?;
            to_lua(lua, &v)
        })?,
    )?;
    json.set(
        "encode",
        lua.create_function(|lua, v: Value| {
            let options = DeserializeOptions::new().deny_unsupported_types(false);
            let v: serde_json::Value = (lua.from_value_with(v, options))
                .map_err(|e| mlua::Error::runtime(format!("json.encode: {e}")))?;
            Ok(v.to_string())
        })?,
    )?;
    ns.set("json", json)?;
    ns.set(
        "span",
        lua.create_function(|lua, v: Value| {
            let span = crate::caps::span(&v).map_err(mlua::Error::runtime)?;
            let host = host(lua)?;
            let changed = lock(&host.span).replace(span) != Some(span);
            // While loading, it goes with the capabilities.
            if changed && host.loaded.load(std::sync::atomic::Ordering::Relaxed) {
                let instance = host.instance.clone();
                host.link.emit(Event::Span { instance, span });
            }
            Ok(())
        })?,
    )?;
    crate::ui::install(lua, &ns)?;
    g.set(namespace, ns)?;
    Ok(())
}

/// `v` as a Lua value, with `null` as `nil`.
pub(crate) fn to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<Value> {
    let options = SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false)
        .set_array_metatable(false);
    lua.to_value_with(v, options)
}

/// `require(name)`: `name.lua` (dots are folders) in the plugin's folder.
fn require(lua: &Lua, name: String) -> mlua::Result<Value> {
    let loaded: Table = lua.named_registry_value(LOADED)?;
    match loaded.get::<Value>(name.as_str())? {
        Value::Nil => {}
        v => return Ok(v),
    }
    let valid = !name.is_empty()
        && !name.starts_with('.')
        && !name.contains("..")
        && (name.chars()).all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    if !valid {
        return Err(mlua::Error::runtime(format!(
            "require: bad module name {name:?}"
        )));
    }
    let host = host(lua)?;
    let dir = (host.dir.as_ref())
        .ok_or_else(|| mlua::Error::runtime("require: this plugin has no folder"))?;
    let path = dir.join(format!("{}.lua", name.replace('.', "/")));
    let code = std::fs::read_to_string(&path)
        .map_err(|e| mlua::Error::runtime(format!("require: {}: {e}", path.display())))?;
    let v = (lua.load(code))
        .set_name(format!("@{}", path.display()))
        .set_mode(ChunkMode::Text)
        .call::<Value>(name.as_str())?;
    let v = match v {
        Value::Nil => Value::Boolean(true),
        v => v,
    };
    loaded.set(name, v.clone())?;
    Ok(v)
}

/// A timer `timer` or `every` armed.
struct TimerHandle(u64);

impl UserData for TimerHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |lua, this, ()| {
            let timers: Table = lua.named_registry_value(TIMERS)?;
            timers.set(this.0, Value::Nil)
        });
    }
}

fn arm(lua: &Lua, ms: f64, f: Function, every: bool) -> mlua::Result<TimerHandle> {
    if !ms.is_finite() || ms < 0.0 {
        return Err(mlua::Error::runtime(format!(
            "expected milliseconds, got {ms}"
        )));
    }
    let host = host(lua)?;
    let ms = match every {
        true => (ms as u64).max(MIN_EVERY),
        false => ms as u64,
    };
    let id = host.next_id();
    let entry = lua.create_table()?;
    entry.set("fn", f)?;
    if every {
        entry.set("every", ms)?;
    }
    let timers: Table = lua.named_registry_value(TIMERS)?;
    timers.set(id, entry)?;
    let due = Instant::now() + Duration::from_millis(ms);
    let epoch = host.epoch;
    host.link.at(due, Job::Timer { epoch, id, due });
    Ok(TimerHandle(id))
}

/// A process `spawn` started.
struct ProcHandle(u64);

impl UserData for ProcHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("kill", |lua, this, ()| {
            let child = lock(&host(lua)?.procs).get(&this.0).cloned();
            if let Some(child) = child {
                let _ = lock(&child).kill();
            }
            Ok(())
        });
    }
}

/// `argv`: a list of strings, or a string for `sh -c`.
fn argv(v: Value) -> mlua::Result<Vec<String>> {
    let argv = match v {
        Value::String(s) => vec!["sh".into(), "-c".into(), s.to_str()?.to_string()],
        Value::Table(t) => (t.sequence_values::<Value>())
            .map(|v| match v? {
                Value::String(s) => Ok(s.to_str()?.to_string()),
                Value::Integer(i) => Ok(i.to_string()),
                Value::Number(n) => Ok(n.to_string()),
                v => Err(mlua::Error::runtime(format!(
                    "argv: expected strings, got {}",
                    v.type_name()
                ))),
            })
            .collect::<mlua::Result<_>>()?,
        v => {
            return Err(mlua::Error::runtime(format!(
                "argv: expected a list or a string, got {}",
                v.type_name()
            )));
        }
    };
    match argv.is_empty() {
        true => Err(mlua::Error::runtime("argv is empty")),
        false => Ok(argv),
    }
}

fn spawn(lua: &Lua, (args, opts): (Value, Option<Table>)) -> mlua::Result<ProcHandle> {
    let args = argv(args)?;
    let host = host(lua)?;
    let mut child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| mlua::Error::runtime(format!("spawn {}: {e}", args[0])))?;
    let id = host.next_id();
    let entry = lua.create_table()?;
    if let Some(opts) = opts {
        entry.set("on_line", opts.get::<Option<Function>>("on_line")?)?;
        entry.set("on_exit", opts.get::<Option<Function>>("on_exit")?)?;
    }
    let procs: Table = lua.named_registry_value(PROCS)?;
    procs.set(id, entry)?;
    let stdout = child.stdout.take();
    let child = Arc::new(Mutex::new(child));
    lock(&host.procs).insert(id, child.clone());
    let (link, epoch) = (host.link.clone(), host.epoch);
    let read = move || {
        if let Some(out) = stdout {
            let mut out = BufReader::new(out);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match out.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                while buf.last().is_some_and(|b| matches!(b, b'\n' | b'\r')) {
                    buf.pop();
                }
                let line = String::from_utf8_lossy(&buf).into_owned();
                if !link.push(Job::Line {
                    epoch,
                    proc: id,
                    line,
                }) {
                    let _ = lock(&child).kill();
                    break;
                }
            }
        }
        let code = loop {
            match lock(&child).try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if link.alive() => {}
                // Gone: nobody to tell.
                Ok(None) => {
                    let _ = lock(&child).kill();
                }
                Err(_) => break None,
            }
            std::thread::sleep(EXIT_POLL);
        };
        link.push(Job::Exit {
            epoch,
            proc: id,
            code,
        });
    };
    if let Err(e) = std::thread::Builder::new()
        .name("lua-proc".into())
        .spawn(read)
    {
        warn!("plugin {}: {e}", host.plugin);
    }
    Ok(ProcHandle(id))
}

fn run(lua: &Lua, args: Value) -> mlua::Result<(Option<bool>, Option<String>)> {
    let args = argv(args)?;
    let spawned = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .process_group(0)
        .spawn();
    match spawned {
        Ok(mut child) => {
            // Reaped when it exits.
            let _ = std::thread::Builder::new()
                .name("lua-run".into())
                .spawn(move || child.wait());
            Ok((Some(true), None))
        }
        Err(e) => {
            let host = host(lua)?;
            let e = format!("run {}: {e}", args[0]);
            warn!("plugin {}: {e}", host.plugin);
            Ok((None, Some(e)))
        }
    }
}

/// `path` with a leading `~` or `~/` expanded.
fn expand(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME");
    match (path, home) {
        ("~", Some(home)) => PathBuf::from(home),
        (p, Some(home)) if p.starts_with("~/") => Path::new(&home).join(&p[2..]),
        (p, _) => PathBuf::from(p),
    }
}

fn read(_: &Lua, path: String) -> mlua::Result<(Option<String>, Option<String>)> {
    let path = expand(&path);
    let text = std::fs::metadata(&path).and_then(|m| match m.len() > MAX_READ {
        true => Err(std::io::Error::other("too large")),
        false => std::fs::read_to_string(&path),
    });
    match text {
        Ok(text) => Ok((Some(text), None)),
        Err(e) => Ok((None, Some(format!("{}: {e}", path.display())))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expanding() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand("~"), PathBuf::from(&home));
            assert_eq!(expand("~/a/b"), Path::new(&home).join("a/b"));
        }
        assert_eq!(expand("/a/~"), PathBuf::from("/a/~"));
        assert_eq!(expand("~x"), PathBuf::from("~x"));
    }

    #[test]
    fn argvs() {
        let lua = Lua::new();
        let v = |code: &str| argv(lua.load(code).eval().unwrap());
        assert_eq!(v("return {'ls', '-l', 3}").unwrap(), ["ls", "-l", "3"]);
        assert_eq!(v("return 'echo hi'").unwrap(), ["sh", "-c", "echo hi"]);
        assert!(v("return {}").is_err());
        assert!(v("return {{}}").is_err());
        assert!(v("return 1").is_err());
    }
}
