//! `dock.dbus` against a private bus (skipped without `dbus-daemon`):
//! every type through an echo service and back, signals, names, errors
//! and properties.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_lite::StreamExt;
use wl_shell_lua::{Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source};
use zbus::message::Type;
use zbus::zvariant::{Signature, Structure};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private session bus; its address.
fn daemon() -> Option<(Daemon, String)> {
    let mut child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    let address = line.trim().to_string();
    (!address.is_empty()).then(|| (Daemon(child), address))
}

/// `name` (e.g. `org.example.Echo`): `Echo` returns its arguments, and
/// sends them on as the signal `Echoed`.
fn echo(address: String, name: &'static str) {
    std::thread::spawn(move || {
        async_io::block_on(async move {
            let conn = zbus::connection::Builder::address(address.as_str())?
                .name(name)?
                .build()
                .await?;
            let mut stream = zbus::MessageStream::from(&conn);
            while let Some(msg) = stream.next().await {
                let Ok(msg) = msg else { continue };
                let hdr = msg.header();
                if msg.message_type() != Type::MethodCall
                    || hdr.member().map(|m| m.as_str()) != Some("Echo")
                {
                    continue;
                }
                let body = msg.body();
                let reply = zbus::Message::method_return(&hdr)?;
                let signal =
                    zbus::Message::signal("/org/example/Echo", "org.example.Echo", "Echoed")?;
                let (reply, signal) = match body.signature() {
                    Signature::Unit => (reply.build(&())?, signal.build(&())?),
                    _ => {
                        let args: Structure = body.deserialize()?;
                        (reply.build(&args)?, signal.build(&args)?)
                    }
                };
                conn.send(&reply).await?;
                conn.send(&signal).await?;
            }
            Ok::<_, zbus::Error>(())
        })
        .expect("the echo service");
    });
}

const PLUGIN: &str = r#"
local bus = dock.dbus.session()
local got = {}
local function put(k, v) got[k] = v; dock.redraw() end

local function eq(a, b)
  if type(a) ~= type(b) then return false end
  if type(a) ~= 'table' then return a == b end
  for k, v in pairs(a) do if not eq(v, b[k]) then return false end end
  for k in pairs(b) do if a[k] == nil then return false end end
  return true
end

local SIG = 'ybnqiuxtdsogayasa{sv}a{ix}(si)vav'
local ARGS = {
  255, true, -32768, 65535, -2147483648, 4294967295, math.mininteger, 9007199254740993,
  -0.5, 'text ✓', '/a/b', 'a{sv}', 'by\0tes', {'x', 'y'},
  {title = 'T', length = 1000000, art = 'https://x/y.png', live = true, rate = 1.0, list = {'a'}},
  {[3] = 9, [-1] = 0}, {'s', 7}, 'variant', {1, 'two', false, 2.5},
}

local function echo()
  bus:call{dest = 'org.example.Echo', path = '/org/example/Echo', iface = 'org.example.Echo',
    method = 'Echo', signature = SIG, args = ARGS,
    on_reply = function(ret, err) put('reply', err or (eq(ret, ARGS) and 'same' or 'different')) end}
end

local ticking
return {
  setup = function()
    bus:watch_names('org.example.', function(name, owner)
      if name == 'org.example.Echo' and owner then put('name', owner:sub(1, 1)) end
    end)
    bus:watch{sender = 'org.example.Echo', iface = 'org.example.Echo', member = 'Echoed',
      on_signal = function(args, msg)
        if #args == 0 then return end -- the echo of nothing
        if args[1] == 'other' then put('leak', msg.sender) return end
        local from = msg.member == 'Echoed' and msg.path == '/org/example/Echo'
          and msg.iface == 'org.example.Echo' and msg.sender:sub(1, 1) == ':'
        put('signal', eq(args, ARGS) and from and 'same' or 'different')
        if ticking then ticking:cancel(); ticking = nil end
      end}
    -- Another sender's signals come to the connection too (for this
    -- watch), but not to the one above.
    local others
    bus:watch{member = 'Echoed', on_signal = function(args)
      if args[1] == 'other' then
        put('all', 'yes')
        if others then others:cancel(); others = nil end
      end
    end}
    local function other()
      bus:call{dest = 'org.example.Other', path = '/org/example/Echo', iface = 'org.example.Echo',
        method = 'Echo', signature = 's', args = {'other'}}
    end
    other()
    others = dock.every(100, other)
    -- Again until the signal is in (the watch may start after the first).
    echo()
    ticking = dock.every(100, echo)
    bus:call{dest = 'org.example.Nobody', path = '/', method = 'Nothing',
      on_reply = function(ret, err) put('error', ret == nil and err:match('ServiceUnknown') and 'yes' or err) end}
    bus:call{dest = 'org.example.Echo', path = '/org/example/Echo', iface = 'org.example.Echo',
      method = 'Echo', on_reply = function(ret, err) put('none', err or #ret) end}
    bus:get('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'Features',
      function(v, err) put('get', type(v)) end)
    bus:get_all('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus',
      function(v, err) put('get_all', type(v and v.Interfaces)) end)
    bus:set('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus', 'Features', 'as', {},
      function(err) put('set', err and 'refused' or 'set') end)
    local ok, err = pcall(bus.call, bus, {method = 'X', signature = 'u', args = {-1}})
    put('bad', not ok and tostring(err):match('out of range') and 'yes' or tostring(err))
  end,
  -- (Labels are short.)
  label = function()
    local short = {}
    for k, v in pairs(got) do short[k] = type(v) == 'string' and v:sub(1, 12) or v end
    return dock.json.encode(short)
  end,
}
"#;

#[test]
fn every_type_round_trips_through_a_bus() {
    let Some((_daemon, address)) = daemon() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    // The runtime's session bus is this one.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    echo(address.clone(), "org.example.Echo");
    echo(address, "org.example.Other");
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let rt = Runtime::new(Settings::default(), move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    rt.configure(vec![InstanceSpec {
        id: "a".into(),
        plugin: "a".into(),
        source: Source::Code {
            name: "a".into(),
            code: Arc::from(PLUGIN),
        },
        role: Role::Widget,
        app: None,
        options: serde_json::json!({}),
        budget: 10_000_000,
    }]);
    let want = serde_json::json!({
        "reply": "same",
        "signal": "same",
        "name": ":",
        "error": "yes",
        "none": 0,
        "get": "table",
        "get_all": "table",
        "set": "refused",
        "bad": "yes",
        "all": "yes",
    });
    let end = Instant::now() + Duration::from_secs(10);
    let mut got = serde_json::Value::Null;
    while got != want {
        let left = end.saturating_duration_since(Instant::now());
        let e = recv
            .recv_timeout(left)
            .unwrap_or_else(|_| panic!("got {got}, want {want}"));
        match e {
            Event::Failed { error, .. } => panic!("{error}"),
            Event::Loaded { .. } | Event::Redraw(_) => {
                let ticket = rt.request("a", RequestKind::Label, Duration::from_secs(5));
                let ticket = ticket.unwrap();
                loop {
                    let e = recv.recv_timeout(Duration::from_secs(5)).unwrap();
                    if let Event::Answer(a) = e
                        && a.id == ticket.id
                    {
                        if let Ok(Reply::Label(Some(l))) = a.result {
                            got = serde_json::from_str(&l).unwrap();
                        }
                        break;
                    }
                }
            }
            _ => {}
        }
    }
}
