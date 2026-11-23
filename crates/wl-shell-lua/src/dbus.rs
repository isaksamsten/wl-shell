//! `dock.dbus`: a generic D-Bus client, so services are Lua code.
//!
//! ```lua
//! local bus = dock.dbus.session()                 -- or dock.dbus.system()
//! bus:call{dest = ..., path = ..., iface = ..., method = ..., signature = 'ox',
//!   args = {...}, timeout = 5000, on_reply = function(ret, err) end}
//! bus:get(dest, path, iface, name, function(value, err) end)
//! bus:get_all(dest, path, iface, function(props, err) end)
//! bus:set(dest, path, iface, name, signature, value, function(err) end)
//! local sub = bus:watch{sender = ..., path = ..., iface = ..., member = ..., arg0 = ...,
//!   on_signal = function(args, msg) end}         -- msg: sender, path, iface, member
//! bus:watch_props(dest, path, iface, function(changed, invalidated) end)
//! bus:watch_names(prefix, function(name, owner) end)  -- owner nil: gone
//! sub:cancel()
//! ```
//!
//! - Each bus has a thread of its own, shared by every state: it owns
//!   the connection (made when first used, and again after a failure)
//!   and runs the calls and watches as tasks. A host function only
//!   queues a command, so a plugin never waits on the bus; replies and
//!   signals come back as jobs on the plugin's own thread, like a
//!   process's lines.
//! - Types. D-Bus to Lua is fixed: variants unwrap, integers (`x` and
//!   `t` too) are Lua integers, doubles numbers, `a{..}` tables keyed by
//!   the key, other arrays and structs lists, object paths and
//!   signatures strings, `ay` a string. Lua to D-Bus takes the
//!   `signature`; a value that doesn't fit it is an error on the call. A
//!   `v` is inferred: booleans `b`, integers `x`, numbers `d`, strings
//!   `s`, lists `as`, `ax`, `ad`, `ab` or `av`, other tables `a{sv}`. File
//!   descriptors (`h`) aren't supported. (A body of one struct reads as
//!   that struct's fields, as if they were the arguments.)
//! - Lifecycle. Watches belong to the state that made them: it dropping
//!   (a reload, a restart, an unload) cancels them, and a reply to a
//!   call of an older generation is dropped. A call answers
//!   `on_reply(nil, err)` on failure, or after its `timeout` (5 s).
//! - A watch with a well-known `sender` takes only that name's owner's
//!   signals (the bus delivers the matches of every rule to the
//!   connection, so it is checked here too).

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures_lite::StreamExt;
use mlua::{Function, Lua, Table, UserData, UserDataMethods, Value as LuaValue};
use wl_shell_log::warn;
use zbus::message::Type as MessageType;
use zbus::zvariant::{
    Array, Dict, ObjectPath, OwnedValue, Signature, Str, Structure, StructureBuilder, Value,
};
use zbus::{Connection, MatchRule, MessageStream, OwnedMatchRule};

use crate::runtime::{Job, lock};
use crate::state::{BUS, host};

/// How long a call waits for its reply by default.
const TIMEOUT: Duration = Duration::from_secs(5);
/// The longest a call may wait.
const MAX_TIMEOUT: Duration = Duration::from_secs(120);
/// How long after failing to connect the bus is tried again.
const RETRY: Duration = Duration::from_secs(10);
/// Signals a watch keeps while its plugin is busy.
const QUEUE: usize = 256;

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum BusKind {
    Session,
    System,
}

impl BusKind {
    fn name(self) -> &'static str {
        match self {
            BusKind::Session => "session",
            BusKind::System => "system",
        }
    }
}

/// A signal, as a watch hands it on.
#[derive(Debug)]
pub(crate) struct Signal {
    pub args: Vec<OwnedValue>,
    pub sender: Option<String>,
    pub path: Option<String>,
    pub iface: Option<String>,
    pub member: Option<String>,
}

/// What the bus tells a state.
#[derive(Debug)]
pub(crate) enum BusEvent {
    /// A call's reply: its arguments, or the error.
    Reply(Result<Vec<OwnedValue>, String>),
    Signal(Signal),
    /// A watched name's owner changed (None: it is gone).
    Name {
        name: String,
        owner: Option<String>,
    },
}

/// A call to make.
struct Call {
    dest: Option<String>,
    path: String,
    iface: Option<String>,
    method: String,
    body: Option<Structure<'static>>,
    timeout: Duration,
}

type Reply = Box<dyn FnOnce(Result<Vec<OwnedValue>, String>) + Send>;
/// Hands something on; false once nobody listens.
type Deliver<T> = Box<dyn Fn(T) -> bool + Send + Sync>;

enum Command {
    Call {
        call: Call,
        reply: Reply,
    },
    Watch {
        id: u64,
        rule: OwnedMatchRule,
        /// The rule's sender, when it is a well-known name.
        name: Option<String>,
        deliver: Deliver<Signal>,
    },
    Names {
        id: u64,
        prefix: String,
        deliver: Deliver<(String, Option<String>)>,
    },
    Cancel(u64),
}

/// A bus's thread, by its queue.
struct Bus {
    commands: async_channel::Sender<Command>,
}

/// The bus `kind`, its thread started when first asked for.
fn bus(kind: BusKind) -> &'static Bus {
    static SESSION: OnceLock<Bus> = OnceLock::new();
    static SYSTEM: OnceLock<Bus> = OnceLock::new();
    let cell = match kind {
        BusKind::Session => &SESSION,
        BusKind::System => &SYSTEM,
    };
    cell.get_or_init(|| {
        let (commands, inbox) = async_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name(format!("lua-dbus-{}", kind.name()))
            .spawn(move || serve(kind, inbox));
        if let Err(e) = spawned {
            warn!("D-Bus {} bus: {e}", kind.name());
        }
        Bus { commands }
    })
}

impl Bus {
    fn send(&self, command: Command) {
        if let Err(e) = self.commands.try_send(command)
            && let Command::Call { reply, .. } = e.into_inner()
        {
            reply(Err("no D-Bus thread".into()));
        }
    }
}

/// Stop the watch `id` on `kind`.
pub(crate) fn cancel(kind: BusKind, id: u64) {
    bus(kind).send(Command::Cancel(id));
}

/// A bus's thread: connect, and run its commands as tasks.
fn serve(kind: BusKind, inbox: async_channel::Receiver<Command>) {
    let ex = Arc::new(async_executor::Executor::new());
    let spawner = ex.clone();
    async_io::block_on(ex.run(async move {
        let mut conn: Option<Connection> = None;
        let mut failed: Option<(Instant, String)> = None;
        let mut tasks: HashMap<u64, async_executor::Task<()>> = HashMap::new();
        while let Ok(command) = inbox.recv().await {
            tasks.retain(|_, t| !t.is_finished());
            if let Command::Cancel(id) = command {
                // Dropping the task stops it (and its match rule goes).
                tasks.remove(&id);
                continue;
            }
            if conn.is_none() && failed.as_ref().is_none_or(|(at, _)| at.elapsed() >= RETRY) {
                let made = match kind {
                    BusKind::Session => Connection::session().await,
                    BusKind::System => Connection::system().await,
                };
                match made {
                    Ok(c) => {
                        conn = Some(c);
                        failed = None;
                    }
                    Err(e) => {
                        let e = format!("the {} bus: {e}", kind.name());
                        warn!("D-Bus: {e}");
                        failed = Some((Instant::now(), e));
                    }
                }
            }
            let Some(c) = &conn else {
                if let Command::Call { reply, .. } = command {
                    let e = (failed.as_ref()).map_or_else(|| "no bus".into(), |(_, e)| e.clone());
                    reply(Err(e));
                }
                continue;
            };
            match command {
                Command::Call { call, reply } => {
                    spawner.spawn(run_call(c.clone(), call, reply)).detach()
                }
                Command::Watch {
                    id,
                    rule,
                    name,
                    deliver,
                } => {
                    let task = spawner.spawn(watch(c.clone(), rule, name, deliver));
                    tasks.insert(id, task);
                }
                Command::Names {
                    id,
                    prefix,
                    deliver,
                } => {
                    let task = spawner.spawn(names(c.clone(), prefix, deliver));
                    tasks.insert(id, task);
                }
                Command::Cancel(_) => {}
            }
        }
    }));
}

/// A D-Bus error as text: its name and message.
fn error_text(e: zbus::Error) -> String {
    match e {
        zbus::Error::MethodError(name, Some(text), _) => format!("{name}: {text}"),
        zbus::Error::MethodError(name, None, _) => name.to_string(),
        e => e.to_string(),
    }
}

/// A message's arguments.
fn arguments(msg: &zbus::Message) -> Result<Vec<OwnedValue>, String> {
    let body = msg.body();
    if matches!(body.signature(), Signature::Unit) {
        return Ok(Vec::new());
    }
    let s: Structure = body.deserialize().map_err(|e| e.to_string())?;
    (s.fields().iter())
        .map(|v| v.try_to_owned().map_err(|e| e.to_string()))
        .collect()
}

async fn run_call(conn: Connection, call: Call, reply: Reply) {
    let called = async {
        let msg = match &call.body {
            Some(body) => {
                conn.call_method(
                    call.dest.as_deref(),
                    call.path.as_str(),
                    call.iface.as_deref(),
                    call.method.as_str(),
                    body,
                )
                .await
            }
            None => {
                conn.call_method(
                    call.dest.as_deref(),
                    call.path.as_str(),
                    call.iface.as_deref(),
                    call.method.as_str(),
                    &(),
                )
                .await
            }
        };
        arguments(&msg.map_err(error_text)?)
    };
    let timeout = async {
        async_io::Timer::after(call.timeout).await;
        Err(format!("{}: no reply in time", call.method))
    };
    reply(futures_lite::future::or(called, timeout).await);
}

/// The unique name owning `name` now.
async fn name_owner(conn: &Connection, name: &str) -> Option<String> {
    let reply = conn
        .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(name,))
        .await
        .ok()?;
    reply.body().deserialize::<String>().ok()
}

async fn watch(
    conn: Connection,
    rule: OwnedMatchRule,
    name: Option<String>,
    deliver: Deliver<Signal>,
) {
    let mut stream = match MessageStream::for_match_rule(rule, &conn, Some(QUEUE)).await {
        Ok(s) => s,
        Err(e) => return warn!("D-Bus watch: {e}"),
    };
    let mut owner: Option<String> = None;
    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { continue };
        let hdr = msg.header();
        let sender = hdr.sender().map(|s| s.to_string());
        if let Some(name) = &name
            && sender != owner
        {
            owner = name_owner(&conn, name).await;
            if sender != owner {
                continue;
            }
        }
        let Ok(args) = arguments(&msg) else { continue };
        let signal = Signal {
            args,
            sender,
            path: hdr.path().map(|p| p.to_string()),
            iface: hdr.interface().map(|i| i.to_string()),
            member: hdr.member().map(|m| m.to_string()),
        };
        if !deliver(signal) {
            break;
        }
    }
}

async fn names(conn: Connection, prefix: String, deliver: Deliver<(String, Option<String>)>) {
    let ran = async {
        let rule = MatchRule::builder()
            .msg_type(MessageType::Signal)
            .sender(DBUS)?
            .path(DBUS_PATH)?
            .interface(DBUS)?
            .member("NameOwnerChanged")?
            .build();
        // Listening first, so none is missed between the two.
        let mut stream = MessageStream::for_match_rule(rule, &conn, Some(QUEUE)).await?;
        let listed =
            (conn.call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "ListNames", &())).await?;
        let listed: Vec<String> = listed.body().deserialize()?;
        let wanted = |name: &str| name.starts_with(prefix.as_str()) && !name.starts_with(':');
        for name in listed.into_iter().filter(|n| wanted(n)) {
            let owner = name_owner(&conn, &name).await;
            if owner.is_some() && !deliver((name, owner)) {
                return Ok(());
            }
        }
        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { continue };
            let Ok((name, _, new)) = msg.body().deserialize::<(String, String, String)>() else {
                continue;
            };
            if wanted(&name) && !deliver((name, (!new.is_empty()).then_some(new))) {
                break;
            }
        }
        Ok::<_, zbus::Error>(())
    };
    if let Err(e) = ran.await {
        warn!("D-Bus watch_names: {e}");
    }
}

// -- Lua to D-Bus ----------------------------------------------------------

fn kind_of(v: &LuaValue) -> &'static str {
    v.type_name()
}

fn mismatch(v: &LuaValue, sig: &Signature) -> String {
    format!("expected {sig}, got {}", kind_of(v))
}

/// `v` as the integer `sig` takes, in `min..=max`.
fn integer(v: &LuaValue, sig: &Signature, min: i128, max: i128) -> Result<i128, String> {
    let n = match v {
        LuaValue::Integer(i) => *i as i128,
        LuaValue::Number(n) if n.is_finite() && n.fract() == 0.0 => *n as i128,
        v => return Err(mismatch(v, sig)),
    };
    match (min..=max).contains(&n) {
        true => Ok(n),
        false => Err(format!("{n} is out of range for {sig}")),
    }
}

fn string(v: &LuaValue, sig: &Signature) -> Result<String, String> {
    match v {
        LuaValue::String(s) => Ok(s.to_str().map_err(|e| e.to_string())?.to_string()),
        v => Err(mismatch(v, sig)),
    }
}

fn table<'v>(v: &'v LuaValue, sig: &Signature) -> Result<&'v Table, String> {
    match v {
        LuaValue::Table(t) => Ok(t),
        v => Err(mismatch(v, sig)),
    }
}

/// The list `t`'s length if it is one (keys 1 to n and nothing else).
fn list_len(t: &Table) -> Option<usize> {
    let n = t.raw_len();
    let count = t.pairs::<LuaValue, LuaValue>().count();
    (n > 0 && count == n).then_some(n)
}

/// The signature a `v` takes for `v`.
fn infer(v: &LuaValue) -> Result<Signature, String> {
    Ok(match v {
        LuaValue::Boolean(_) => Signature::Bool,
        LuaValue::Integer(_) => Signature::I64,
        LuaValue::Number(_) => Signature::F64,
        LuaValue::String(_) => Signature::Str,
        LuaValue::Table(t) => match list_len(t) {
            Some(_) => {
                let items: Vec<LuaValue> = t
                    .sequence_values::<LuaValue>()
                    .collect::<mlua::Result<_>>()
                    .map_err(|e| e.to_string())?;
                let all = |f: fn(&LuaValue) -> bool| items.iter().all(f);
                let child = if all(|v| matches!(v, LuaValue::String(_))) {
                    Signature::Str
                } else if all(|v| matches!(v, LuaValue::Integer(_))) {
                    Signature::I64
                } else if all(|v| matches!(v, LuaValue::Integer(_) | LuaValue::Number(_))) {
                    Signature::F64
                } else if all(|v| matches!(v, LuaValue::Boolean(_))) {
                    Signature::Bool
                } else {
                    Signature::Variant
                };
                Signature::array(child)
            }
            None => Signature::dict(Signature::Str, Signature::Variant),
        },
        v => return Err(format!("can't send {} as a variant", kind_of(v))),
    })
}

/// `v` as a D-Bus value of type `sig`.
pub(crate) fn to_dbus(v: &LuaValue, sig: &Signature) -> Result<Value<'static>, String> {
    use Signature as S;
    let lua = |e: mlua::Error| e.to_string();
    let dbus = |e: zbus::zvariant::Error| e.to_string();
    Ok(match sig {
        S::U8 => Value::U8(integer(v, sig, 0, u8::MAX as i128)? as u8),
        S::Bool => match v {
            LuaValue::Boolean(b) => Value::Bool(*b),
            v => return Err(mismatch(v, sig)),
        },
        S::I16 => Value::I16(integer(v, sig, i16::MIN as i128, i16::MAX as i128)? as i16),
        S::U16 => Value::U16(integer(v, sig, 0, u16::MAX as i128)? as u16),
        S::I32 => Value::I32(integer(v, sig, i32::MIN as i128, i32::MAX as i128)? as i32),
        S::U32 => Value::U32(integer(v, sig, 0, u32::MAX as i128)? as u32),
        S::I64 => Value::I64(integer(v, sig, i64::MIN as i128, i64::MAX as i128)? as i64),
        S::U64 => Value::U64(integer(v, sig, 0, u64::MAX as i128)? as u64),
        S::F64 => match v {
            LuaValue::Integer(i) => Value::F64(*i as f64),
            LuaValue::Number(n) => Value::F64(*n),
            v => return Err(mismatch(v, sig)),
        },
        S::Str => Value::Str(Str::from(string(v, sig)?)),
        S::ObjectPath => {
            let s = string(v, sig)?;
            Value::ObjectPath(
                ObjectPath::try_from(s.clone())
                    .map_err(|_| format!("{s:?} is not an object path"))?,
            )
        }
        S::Signature => {
            let s = string(v, sig)?;
            Value::Signature(
                Signature::from_str(&s).map_err(|_| format!("{s:?} is not a signature"))?,
            )
        }
        S::Variant => Value::Value(Box::new(to_dbus(v, &infer(v)?)?)),
        S::Array(child) => {
            let child: &Signature = child;
            let mut a = Array::new(child);
            if let (S::U8, LuaValue::String(s)) = (child, v) {
                for b in s.as_bytes().iter() {
                    a.append(Value::U8(*b)).map_err(dbus)?;
                }
                return Ok(Value::Array(a));
            }
            let t = table(v, sig)?;
            for item in t.sequence_values::<LuaValue>() {
                a.append(to_dbus(&item.map_err(lua)?, child)?)
                    .map_err(dbus)?;
            }
            Value::Array(a)
        }
        S::Dict { key, value } => {
            let (key, value): (&Signature, &Signature) = (key, value);
            let t = table(v, sig)?;
            let mut d = Dict::new(key, value);
            for pair in t.pairs::<LuaValue, LuaValue>() {
                let (k, val) = pair.map_err(lua)?;
                d.append(to_dbus(&k, key)?, to_dbus(&val, value)?)
                    .map_err(dbus)?;
            }
            Value::Dict(d)
        }
        S::Structure(fields) => {
            let t = table(v, sig)?;
            let n = fields.iter().count();
            if t.raw_len() != n {
                return Err(format!(
                    "expected {n} fields for {sig}, got {}",
                    t.raw_len()
                ));
            }
            let mut b = StructureBuilder::new();
            for (i, field) in fields.iter().enumerate() {
                let item: LuaValue = t.raw_get(i + 1).map_err(lua)?;
                b = b.append_field(to_dbus(&item, field)?);
            }
            Value::Structure(b.build().map_err(dbus)?)
        }
        S::Fd => return Err("file descriptors (h) aren't supported".into()),
        S::Unit => return Err("no type".into()),
        #[allow(unreachable_patterns)]
        _ => return Err(format!("{sig} isn't supported")),
    })
}

/// `args` as a body of `signature` (a list of complete types, e.g.
/// `ox`): None if there are none.
fn body(
    signature: Option<&str>,
    args: Option<&Table>,
) -> Result<Option<Structure<'static>>, String> {
    let signature = signature.map(str::trim).unwrap_or_default();
    let given = args.map_or(0, |t| t.raw_len());
    if signature.is_empty() {
        return match given {
            0 => Ok(None),
            _ => Err("arguments need a signature".into()),
        };
    }
    let Ok(Signature::Structure(fields)) = Signature::from_str(&format!("({signature})")) else {
        return Err(format!("bad signature {signature:?}"));
    };
    let n = fields.iter().count();
    if given != n {
        return Err(format!(
            "the signature {signature:?} takes {n} arguments, got {given}"
        ));
    }
    let mut b = StructureBuilder::new();
    for (i, field) in fields.iter().enumerate() {
        let v: LuaValue = match args {
            Some(t) => t.raw_get(i + 1).map_err(|e| e.to_string())?,
            None => LuaValue::Nil,
        };
        let v = to_dbus(&v, field).map_err(|e| format!("argument {}: {e}", i + 1))?;
        b = b.append_field(v);
    }
    b.build().map(Some).map_err(|e| e.to_string())
}

// -- D-Bus to Lua ----------------------------------------------------------

/// `v` as a Lua value.
pub(crate) fn to_lua(lua: &Lua, v: &Value) -> mlua::Result<LuaValue> {
    Ok(match v {
        Value::U8(n) => LuaValue::Integer(*n as i64),
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::I16(n) => LuaValue::Integer(*n as i64),
        Value::U16(n) => LuaValue::Integer(*n as i64),
        Value::I32(n) => LuaValue::Integer(*n as i64),
        Value::U32(n) => LuaValue::Integer(*n as i64),
        Value::I64(n) => LuaValue::Integer(*n),
        Value::U64(n) => LuaValue::Integer(*n as i64),
        Value::F64(n) => LuaValue::Number(*n),
        Value::Str(s) => LuaValue::String(lua.create_string(s.as_str())?),
        Value::Signature(s) => LuaValue::String(lua.create_string(s.to_string_no_parens())?),
        Value::ObjectPath(p) => LuaValue::String(lua.create_string(p.as_str())?),
        Value::Value(inner) => to_lua(lua, inner)?,
        Value::Array(a) if matches!(a.element_signature(), Signature::U8) => {
            let bytes: Vec<u8> = (a.inner().iter())
                .map(|v| match v {
                    Value::U8(b) => *b,
                    _ => 0,
                })
                .collect();
            LuaValue::String(lua.create_string(bytes)?)
        }
        Value::Array(a) => list(lua, a.inner())?,
        Value::Dict(d) => {
            let t = lua.create_table()?;
            for (k, v) in d.iter() {
                t.raw_set(to_lua(lua, k)?, to_lua(lua, v)?)?;
            }
            LuaValue::Table(t)
        }
        Value::Structure(s) => list(lua, s.fields())?,
        _ => LuaValue::Nil,
    })
}

fn list(lua: &Lua, values: &[Value]) -> mlua::Result<LuaValue> {
    let t = lua.create_table_with_capacity(values.len(), 0)?;
    for (i, v) in values.iter().enumerate() {
        t.raw_set(i + 1, to_lua(lua, v)?)?;
    }
    Ok(LuaValue::Table(t))
}

fn args_list(lua: &Lua, values: &[OwnedValue]) -> mlua::Result<LuaValue> {
    let t = lua.create_table_with_capacity(values.len(), 0)?;
    for (i, v) in values.iter().enumerate() {
        t.raw_set(i + 1, to_lua(lua, v)?)?;
    }
    Ok(LuaValue::Table(t))
}

// -- the Lua side ----------------------------------------------------------

/// Add `ns.dbus`.
pub(crate) fn install(lua: &Lua, ns: &Table) -> mlua::Result<()> {
    let dbus = lua.create_table()?;
    dbus.set(
        "session",
        lua.create_function(|_, ()| Ok(BusHandle(BusKind::Session)))?,
    )?;
    dbus.set(
        "system",
        lua.create_function(|_, ()| Ok(BusHandle(BusKind::System)))?,
    )?;
    ns.set("dbus", dbus)
}

/// What a callback is called with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// `on_reply(ret, err)`: the reply's arguments as a list.
    Reply,
    /// `fn(value, err)`: the first argument (a property's value).
    First,
    /// `fn(err)`.
    Done,
    /// `on_signal(args, msg)`.
    Signal,
    /// `fn(changed, invalidated)`.
    Props,
    /// `fn(name, owner)`.
    Name,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Reply => "reply",
            Shape::First => "first",
            Shape::Done => "done",
            Shape::Signal => "signal",
            Shape::Props => "props",
            Shape::Name => "name",
        }
    }

    fn of(name: &str) -> Option<Shape> {
        [
            Shape::Reply,
            Shape::First,
            Shape::Done,
            Shape::Signal,
            Shape::Props,
            Shape::Name,
        ]
        .into_iter()
        .find(|s| s.name() == name)
    }
}

/// Keep `f` in the state's callbacks; its id (unique in the process,
/// since a bus's watches of every state are kept by it).
fn register(lua: &Lua, f: Option<Function>, shape: Shape) -> mlua::Result<u64> {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    if let Some(f) = f {
        let entry = lua.create_table()?;
        entry.set("fn", f)?;
        entry.set("shape", shape.name())?;
        let callbacks: Table = lua.named_registry_value(BUS)?;
        callbacks.set(id, entry)?;
    }
    Ok(id)
}

/// Make `call`, answering callback `id`.
fn send_call(lua: &Lua, kind: BusKind, call: Call, id: u64) -> mlua::Result<()> {
    let host = host(lua)?;
    let (link, epoch) = (host.link.clone(), host.epoch);
    let reply: Reply = Box::new(move |result| {
        link.push(Job::Bus {
            epoch,
            id,
            event: BusEvent::Reply(result),
        });
    });
    bus(kind).send(Command::Call { call, reply });
    Ok(())
}

/// Start a watch for callback `id`; its handle.
fn send_watch(
    lua: &Lua,
    kind: BusKind,
    id: u64,
    rule: OwnedMatchRule,
    name: Option<String>,
) -> mlua::Result<Subscription> {
    let host = host(lua)?;
    let (link, epoch) = (host.link.clone(), host.epoch);
    let deliver: Deliver<Signal> = Box::new(move |signal| {
        link.push(Job::Bus {
            epoch,
            id,
            event: BusEvent::Signal(signal),
        })
    });
    lock(&host.subs).push((kind, id));
    bus(kind).send(Command::Watch {
        id,
        rule,
        name,
        deliver,
    });
    Ok(Subscription { kind, id })
}

fn runtime(e: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

/// A match rule for signals; with the well-known name among them, if
/// `sender` is one.
fn rule(
    sender: Option<&str>,
    path: Option<&str>,
    iface: Option<&str>,
    member: Option<&str>,
    arg0: Option<&str>,
) -> mlua::Result<(OwnedMatchRule, Option<String>)> {
    let mut b = MatchRule::builder().msg_type(MessageType::Signal);
    if let Some(s) = sender {
        b = b
            .sender(s)
            .map_err(|e| runtime(format!("sender {s:?}: {e}")))?;
    }
    if let Some(p) = path {
        b = b.path(p).map_err(|e| runtime(format!("path {p:?}: {e}")))?;
    }
    if let Some(i) = iface {
        b = b
            .interface(i)
            .map_err(|e| runtime(format!("iface {i:?}: {e}")))?;
    }
    if let Some(m) = member {
        b = b
            .member(m)
            .map_err(|e| runtime(format!("member {m:?}: {e}")))?;
    }
    if let Some(a) = arg0 {
        b = b.arg(0, a).map_err(runtime)?;
    }
    let rule = b.build().to_owned().into();
    let name = sender.filter(|s| !s.starts_with(':')).map(str::to_string);
    Ok((rule, name))
}

fn timeout(ms: Option<f64>) -> Duration {
    match ms {
        Some(ms) if ms.is_finite() && ms > 0.0 => {
            Duration::from_secs_f64(ms / 1000.0).min(MAX_TIMEOUT)
        }
        _ => TIMEOUT,
    }
}

/// `dock.dbus.session()` / `system()`.
struct BusHandle(BusKind);

impl UserData for BusHandle {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("call", |lua, this, t: Table| {
            let method: String =
                (t.get::<Option<String>>("method")?).ok_or_else(|| runtime("call: no method"))?;
            let path: String = t
                .get::<Option<String>>("path")?
                .unwrap_or_else(|| "/".into());
            let args: Option<Table> = t.get("args")?;
            let signature: Option<String> = t.get("signature")?;
            let body = body(signature.as_deref(), args.as_ref())
                .map_err(|e| runtime(format!("call {method}: {e}")))?;
            let call = Call {
                dest: t.get("dest")?,
                path,
                iface: t.get("iface")?,
                method,
                body,
                timeout: timeout(t.get("timeout")?),
            };
            let id = register(lua, t.get("on_reply")?, Shape::Reply)?;
            send_call(lua, this.0, call, id)
        });
        methods.add_method(
            "get",
            |lua, this, (dest, path, iface, name, f): (String, String, String, String, Option<Function>)| {
                let call = Call {
                    dest: Some(dest),
                    path,
                    iface: Some(PROPERTIES.into()),
                    method: "Get".into(),
                    body: body(Some("ss"), Some(&lua.create_sequence_from([iface, name])?)).map_err(runtime)?,
                    timeout: TIMEOUT,
                };
                let id = register(lua, f, Shape::First)?;
                send_call(lua, this.0, call, id)
            },
        );
        methods.add_method(
            "get_all",
            |lua, this, (dest, path, iface, f): (String, String, String, Option<Function>)| {
                let call = Call {
                    dest: Some(dest),
                    path,
                    iface: Some(PROPERTIES.into()),
                    method: "GetAll".into(),
                    body: body(Some("s"), Some(&lua.create_sequence_from([iface])?))
                        .map_err(runtime)?,
                    timeout: TIMEOUT,
                };
                let id = register(lua, f, Shape::First)?;
                send_call(lua, this.0, call, id)
            },
        );
        methods.add_method(
            "set",
            |lua,
             this,
             (dest, path, iface, name, signature, value, f): (
                String,
                String,
                String,
                String,
                String,
                LuaValue,
                Option<Function>,
            )| {
                let sig = Signature::from_str(signature.trim())
                    .map_err(|_| runtime(format!("set {name}: bad signature {signature:?}")))?;
                let value =
                    to_dbus(&value, &sig).map_err(|e| runtime(format!("set {name}: {e}")))?;
                let body = StructureBuilder::new()
                    .add_field(iface)
                    .add_field(name)
                    .append_field(Value::Value(Box::new(value)))
                    .build()
                    .map_err(runtime)?;
                let call = Call {
                    dest: Some(dest),
                    path,
                    iface: Some(PROPERTIES.into()),
                    method: "Set".into(),
                    body: Some(body),
                    timeout: TIMEOUT,
                };
                let id = register(lua, f, Shape::Done)?;
                send_call(lua, this.0, call, id)
            },
        );
        methods.add_method("watch", |lua, this, t: Table| {
            let get = |k: &str| t.get::<Option<String>>(k);
            let (rule, name) = rule(
                get("sender")?.as_deref(),
                get("path")?.as_deref(),
                get("iface")?.as_deref(),
                get("member")?.as_deref(),
                get("arg0")?.as_deref(),
            )?;
            let id = register(lua, t.get("on_signal")?, Shape::Signal)?;
            send_watch(lua, this.0, id, rule, name)
        });
        methods.add_method(
            "watch_props",
            |lua, this, (dest, path, iface, f): (String, String, String, Function)| {
                let (rule, name) = rule(
                    Some(&dest),
                    Some(&path),
                    Some(PROPERTIES),
                    Some("PropertiesChanged"),
                    Some(&iface),
                )?;
                let id = register(lua, Some(f), Shape::Props)?;
                send_watch(lua, this.0, id, rule, name)
            },
        );
        methods.add_method(
            "watch_names",
            |lua, this, (prefix, f): (String, Function)| {
                let id = register(lua, Some(f), Shape::Name)?;
                let host = host(lua)?;
                let (link, epoch) = (host.link.clone(), host.epoch);
                let deliver: Deliver<(String, Option<String>)> = Box::new(move |(name, owner)| {
                    link.push(Job::Bus {
                        epoch,
                        id,
                        event: BusEvent::Name { name, owner },
                    })
                });
                lock(&host.subs).push((this.0, id));
                bus(this.0).send(Command::Names {
                    id,
                    prefix,
                    deliver,
                });
                Ok(Subscription { kind: this.0, id })
            },
        );
    }
}

/// A watch, from `watch`, `watch_props` or `watch_names`.
struct Subscription {
    kind: BusKind,
    id: u64,
}

impl UserData for Subscription {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("cancel", |lua, this, ()| {
            let callbacks: Table = lua.named_registry_value(BUS)?;
            callbacks.set(this.id, LuaValue::Nil)?;
            let host = host(lua)?;
            let mut subs = lock(&host.subs);
            if let Some(i) = subs.iter().position(|s| *s == (this.kind, this.id)) {
                subs.remove(i);
                cancel(this.kind, this.id);
            }
            Ok(())
        });
    }
}

/// Call `entry`'s function (see `register`) about `event`.
pub(crate) fn deliver(lua: &Lua, entry: &Table, event: BusEvent) -> mlua::Result<()> {
    let f: Function = entry.get("fn")?;
    let shape: String = entry.get("shape")?;
    let shape = Shape::of(&shape).ok_or_else(|| runtime("bad callback"))?;
    let nil = LuaValue::Nil;
    match (shape, event) {
        (Shape::Reply, BusEvent::Reply(Ok(args))) => f.call((args_list(lua, &args)?, nil)),
        (Shape::First, BusEvent::Reply(Ok(args))) => {
            let v = match args.first() {
                Some(v) => to_lua(lua, v)?,
                None => nil.clone(),
            };
            f.call((v, nil))
        }
        (Shape::Reply | Shape::First, BusEvent::Reply(Err(e))) => f.call((nil, e)),
        (Shape::Done, BusEvent::Reply(result)) => f.call(result.err()),
        (Shape::Signal, BusEvent::Signal(s)) => {
            let msg = lua.create_table()?;
            msg.set("sender", s.sender)?;
            msg.set("path", s.path)?;
            msg.set("iface", s.iface)?;
            msg.set("member", s.member)?;
            f.call((args_list(lua, &s.args)?, msg))
        }
        (Shape::Props, BusEvent::Signal(s)) => {
            let arg = |i: usize| match s.args.get(i) {
                Some(v) => to_lua(lua, v),
                None => Ok(LuaValue::Table(lua.create_table()?)),
            };
            f.call((arg(1)?, arg(2)?))
        }
        (Shape::Name, BusEvent::Name { name, owner }) => f.call((name, owner)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::serialized::{Context, Format};
    use zbus::zvariant::{LE, to_bytes};

    /// `code`'s value sent as `sig`, through the wire format, and back.
    fn round_trip(lua: &Lua, sig: &str, code: &str) -> Result<LuaValue, String> {
        let v: LuaValue = lua.load(code).eval().unwrap();
        let args = lua.create_sequence_from([v]).unwrap();
        let body = body(Some(sig), Some(&args))?.unwrap();
        let ctxt = Context::new(Format::DBus, LE, 0);
        let data = to_bytes(ctxt, &body).map_err(|e| e.to_string())?;
        let signature = body.signature().clone();
        let (back, _): (Structure, _) = data
            .deserialize_for_dynamic_signature(&signature)
            .map_err(|e| e.to_string())?;
        assert_eq!(back.fields().len(), 1);
        to_lua(lua, &back.fields()[0]).map_err(|e| e.to_string())
    }

    #[test]
    fn every_type_round_trips() {
        let lua = Lua::new();
        let same = |sig: &str, code: &str| {
            let back = round_trip(&lua, sig, code).unwrap_or_else(|e| panic!("{sig}: {e}"));
            let want: LuaValue = lua.load(code).eval().unwrap();
            let eq: Function = lua
                .load(
                    "local function eq(a, b)
                       if type(a) ~= type(b) then return false end
                       if type(a) ~= 'table' then return a == b or (a ~= a and b ~= b) end
                       for k, v in pairs(a) do if not eq(v, b[k]) then return false end end
                       for k in pairs(b) do if a[k] == nil then return false end end
                       return true
                     end
                     return eq",
                )
                .eval()
                .unwrap();
            assert!(
                eq.call::<bool>((back.clone(), want)).unwrap(),
                "{sig}: {back:?}"
            );
            back
        };
        same("y", "200");
        same("b", "true");
        same("n", "-300");
        same("q", "60000");
        same("i", "-70000");
        same("u", "4000000000");
        same("x", "-9007199254740993");
        same("t", "9007199254740993");
        same("d", "2.5");
        same("s", "'héllo'");
        same("o", "'/org/mpris/MediaPlayer2'");
        same("g", "'a{sv}'");
        same("ay", "'bytes\\0and more'");
        same("as", "{'a', 'b'}");
        same("ax", "{1, -2, 3}");
        same("a(si)", "{{'a', 1}, {'b', 2}}");
        same(
            "a{sv}",
            "{title = 'x', length = 120, rate = 1.5, art = {'a'}, on = false}",
        );
        same("a{is}", "{[1] = 'one', [5] = 'five'}");
        same("(sa{sv}as)", "{'iface', {a = 1}, {'x'}}");
        same("aas", "{{'a'}, {}, {'b', 'c'}}");
        // Variants unwrap; integers are integers.
        same("v", "'s'");
        same("v", "42");
        same("v", "{1.5, 2}");
        same("av", "{1, 'a', true}");
        assert!(matches!(
            round_trip(&lua, "v", "7").unwrap(),
            LuaValue::Integer(7)
        ));
        assert!(matches!(
            round_trip(&lua, "d", "7").unwrap(),
            LuaValue::Number(7.0)
        ));
        // Doubles that are whole numbers fit integer types.
        assert!(matches!(
            round_trip(&lua, "i", "7.0").unwrap(),
            LuaValue::Integer(7)
        ));
    }

    #[test]
    fn values_that_do_not_fit_are_errors() {
        let lua = Lua::new();
        let err = |sig: &str, code: &str| round_trip(&lua, sig, code).unwrap_err();
        assert!(err("y", "256").contains("out of range"));
        assert!(err("u", "-1").contains("out of range"));
        assert!(err("i", "1.5").contains("expected i"));
        assert!(err("s", "1").contains("expected s"));
        assert!(err("b", "'x'").contains("expected b"));
        assert!(err("o", "'not a path'").contains("object path"));
        assert!(err("g", "'a{'").contains("signature"));
        assert!(err("(si)", "{'a'}").contains("fields"));
        assert!(err("h", "1").contains("file descriptors"));
        assert!(err("as", "{1}").contains("expected s"));
        assert!(err("v", "function() end").contains("variant"));
        assert!(
            body(Some("a{"), None)
                .unwrap_err()
                .contains("bad signature")
        );
        let two = lua.create_sequence_from([1, 2]).unwrap();
        assert!(body(Some("i"), Some(&two)).unwrap_err().contains("takes 1"));
        assert!(body(None, Some(&two)).unwrap_err().contains("signature"));
        assert!(body(None, None).unwrap().is_none());
        assert_eq!(
            body(Some("ox"), Some(&lua.load("{'/a', 5}").eval().unwrap()))
                .unwrap()
                .unwrap()
                .signature()
                .to_string_no_parens(),
            "ox"
        );
    }
}
