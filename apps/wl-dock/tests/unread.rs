//! The bundled unread plugin against Unity LauncherEntry updates on a
//! private bus (skipped without `dbus-daemon`): the count badge, the
//! progress arc, the urgent badge, updates for other apps, and the
//! publisher going away.

use std::collections::HashMap;
use std::f32::consts::PI;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::ui::{Cmd, Kind, Node};
use wl_shell_lua::{
    AppInfo, Axis, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source, Tile,
};
use zbus::zvariant::Value;

const UNREAD: &str = include_str!("../plugins/unread.lua");
const WAIT: Duration = Duration::from_secs(10);
const URI: &str = "application://chat.desktop";

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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

/// What the icon shows.
#[derive(Debug, PartialEq)]
struct Shown {
    badge: Option<String>,
    /// The filled arc's sweep, 0–1 of a turn; None: no arc.
    progress: Option<f32>,
}

fn shown(tree: &Node) -> Shown {
    let nodes = tree.walk();
    let badge = nodes.iter().find_map(|n| match &n.kind {
        Kind::Text { text, .. } => Some(text.clone()),
        _ => None,
    });
    let progress = nodes.iter().find_map(|n| match &n.kind {
        Kind::Canvas { commands, .. } => {
            let sweep = (commands.iter())
                .filter_map(|c| match c {
                    Cmd::Arc { start, end, .. } if *start != 0.0 => {
                        Some((end - start) / (2.0 * PI))
                    }
                    _ => None,
                })
                .next();
            Some(sweep.unwrap_or(0.0))
        }
        _ => None,
    });
    Shown { badge, progress }
}

struct Plugin {
    rt: Runtime,
    recv: Receiver<Event>,
}

impl Plugin {
    fn icon(&self) -> Shown {
        let tile = Tile {
            w: 96.0,
            h: 96.0,
            span: wl_shell_lua::Span::Normal,
            axis: Axis::Horizontal,
            scale: 1,
        };
        let ticket = self.rt.request("u", RequestKind::Icon(tile), WAIT).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            match self
                .recv
                .recv_timeout(end - Instant::now())
                .expect("no answer")
            {
                Event::Answer(a) if a.id == ticket.id => match a.result {
                    Ok(Reply::Icon(Some(tree))) => return shown(&tree),
                    r => panic!("{r:?}"),
                },
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
    }

    /// Until the icon shows `badge` and `progress` (to a percent),
    /// sending `update` again meanwhile: the plugin's watch may not be in
    /// place yet, and updates are idempotent.
    fn wait(&self, update: impl Fn(), badge: Option<&str>, progress: Option<f32>) {
        let end = Instant::now() + WAIT;
        loop {
            update();
            let got = self.icon();
            let near = match (got.progress, progress) {
                (Some(a), Some(b)) => (a - b).abs() < 0.005,
                (a, b) => a.is_none() && b.is_none(),
            };
            if got.badge.as_deref() == badge && near {
                return;
            }
            assert!(
                Instant::now() < end,
                "{got:?}, want {badge:?} and {progress:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Send an Update for `uri` from `conn`.
fn update(conn: &zbus::Connection, uri: &str, props: &[(&str, Value<'_>)]) {
    let props: HashMap<&str, &Value<'_>> = props.iter().map(|(k, v)| (*k, v)).collect();
    async_io::block_on(conn.emit_signal(
        None::<()>,
        "/chat",
        "com.canonical.Unity.LauncherEntry",
        "Update",
        &(uri, props),
    ))
    .unwrap();
}

fn connect(address: &str) -> zbus::Connection {
    async_io::block_on(zbus::connection::Builder::address(address).unwrap().build()).unwrap()
}

#[test]
fn launcher_entry_updates_show_on_the_icon() {
    let Some((_daemon, address)) = daemon() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    // The runtime's session bus is this one.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let rt = Runtime::new(Settings::default(), move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    rt.configure(vec![InstanceSpec {
        id: "u".into(),
        plugin: "unread".into(),
        source: Source::Code {
            name: "unread".into(),
            code: Arc::from(UNREAD),
        },
        role: Role::Widget,
        app: Some(AppInfo::new("chat")),
        options: serde_json::json!({}),
        budget: 1_000_000,
    }]);
    let p = Plugin { rt, recv };
    let app = connect(&address);

    // Nothing published: the plain icon.
    assert_eq!(
        p.icon(),
        Shown {
            badge: None,
            progress: None
        }
    );

    // A visible count.
    let count = |n: i64, visible: bool| {
        update(
            &app,
            URI,
            &[
                ("count", Value::I64(n)),
                ("count-visible", Value::Bool(visible)),
            ],
        )
    };
    p.wait(|| count(3, true), Some("3"), None);
    p.wait(|| count(120, true), Some("99+"), None);

    // Progress, without repeating the count: it stays.
    let progress = |v: f64, visible: bool| {
        update(
            &app,
            URI,
            &[
                ("progress", Value::F64(v)),
                ("progress-visible", Value::Bool(visible)),
            ],
        )
    };
    p.wait(|| progress(0.42, true), Some("99+"), Some(0.42));
    // Out of range: clamped.
    p.wait(|| progress(1.5, true), Some("99+"), Some(1.0));
    // Nothing done yet: the track alone.
    p.wait(|| progress(0.0, true), Some("99+"), Some(0.0));
    p.wait(|| progress(0.5, false), Some("99+"), None);

    // Urgent: a "!" without a count, the count with one.
    let urgent = |on: bool| update(&app, URI, &[("urgent", Value::Bool(on))]);
    p.wait(|| urgent(true), Some("99+"), None);
    p.wait(|| count(0, false), Some("!"), None);
    p.wait(|| urgent(false), None, None);

    // Another app's updates don't show.
    update(
        &app,
        "application://other.desktop",
        &[
            ("count", Value::I64(7)),
            ("count-visible", Value::Bool(true)),
        ],
    );
    p.wait(|| progress(0.25, true), None, Some(0.25));

    // The publisher gone: the plain icon again.
    drop(app);
    p.wait(|| {}, None, None);
}
