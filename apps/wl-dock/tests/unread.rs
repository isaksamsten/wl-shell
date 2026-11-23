use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{mpsc::channel, Arc};
use std::time::{Duration, Instant};

use wl_shell_lua::ui::Kind;
use wl_shell_lua::{
    AppInfo, Axis, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source, Tile,
};
use zbus::zvariant::Value;

#[test]
fn launcher_count_draws_and_clears_the_app_badge() {
    let Ok(mut daemon) = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address=1"])
        .stdout(Stdio::piped())
        .spawn()
    else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut address = String::new();
        BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        // This integration test is the only test in its process.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address.trim()) };

        let publisher = async_io::block_on(zbus::Connection::session()).unwrap();
        let (tx, rx) = channel();
        let runtime = Runtime::new(Settings::default(), move |event| {
            let _ = tx.send(event);
        })
        .unwrap();
        runtime.configure(vec![InstanceSpec {
            id: "mail".into(),
            plugin: "unread".into(),
            source: Source::Code {
                name: "unread".into(),
                code: Arc::from(include_str!("../plugins/unread.lua")),
            },
            role: Role::Widget,
            app: Some(AppInfo::new("example.Mail")),
            options: serde_json::json!({}),
            budget: 1_000_000,
        }]);

        let icon = || {
            let timeout = Duration::from_secs(2);
            let tile = Tile {
                w: 48.0,
                h: 48.0,
                span: 1.0,
                axis: Axis::Horizontal,
                scale: 1,
            };
            let ticket = runtime
                .request("mail", RequestKind::Icon(tile), timeout)
                .unwrap();
            loop {
                match rx.recv_timeout(timeout).unwrap() {
                    Event::Answer(answer) if answer.id == ticket.id => match answer.result.unwrap()
                    {
                        Reply::Icon(tree) => return tree,
                        reply => panic!("unexpected reply: {reply:?}"),
                    },
                    Event::Failed { error, .. } => panic!("{error}"),
                    _ => {}
                }
            }
        };
        let emit = |uri: &str, properties: HashMap<&str, Value<'_>>| {
            async_io::block_on(publisher.emit_signal(
                None::<&str>,
                "/example/Mail/LauncherEntry",
                "com.canonical.Unity.LauncherEntry",
                "Update",
                &(uri, properties),
            ))
            .unwrap();
        };
        let count = |n| {
            HashMap::from([
                ("count", Value::I64(n)),
                ("count-visible", Value::Bool(true)),
            ])
        };
        let mail = "application://example.Mail.desktop";
        assert!(icon().is_none());

        // Allow for the asynchronous subscription registration.
        let deadline = Instant::now() + Duration::from_secs(5);
        let tree = loop {
            emit(mail, count(120));
            if let Some(tree) = icon() {
                break tree;
            }
            assert!(Instant::now() < deadline, "badge did not appear");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(matches!(tree.children[0].kind, Kind::AppIcon { .. }));
        assert!(
            matches!(&tree.children[1].children[0].children[0].kind, Kind::Text { text, .. } if text == "99+")
        );

        emit("application://other.desktop", count(1));
        emit(mail, HashMap::from([("count-visible", Value::Bool(false))]));
        let deadline = Instant::now() + Duration::from_secs(5);
        while icon().is_some() {
            assert!(Instant::now() < deadline, "badge did not clear");
            std::thread::sleep(Duration::from_millis(20));
        }
    }));
    let _ = daemon.kill();
    let _ = daemon.wait();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
