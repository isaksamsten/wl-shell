//! The bundled calendar plugin reading a folder of `.ics` files, and
//! reading it again when its files change (`dock.watch`), with no polling.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::ui::{Kind, Node};
use wl_shell_lua::{Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source};

const CALENDAR: &str = include_str!("../plugins/calendar.lua");
const WAIT: Duration = Duration::from_secs(10);

fn texts(tree: &Node) -> Vec<String> {
    (tree.walk().into_iter())
        .filter_map(|n| match &n.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// An all-day event tomorrow (local time), so the agenda shows it.
fn ics(title: &str) -> String {
    let out = Command::new("date")
        .args(["-d", "tomorrow", "+%Y%m%d"])
        .output()
        .unwrap();
    let day = String::from_utf8(out.stdout).unwrap();
    format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:{title}\r\nDTSTART;VALUE=DATE:{}\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        day.trim()
    )
}

struct Calendar {
    rt: Runtime,
    recv: Receiver<Event>,
}

impl Calendar {
    fn popup(&self) -> Vec<String> {
        let ticket = self.rt.request("c", RequestKind::Popup, WAIT).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            match self
                .recv
                .recv_timeout(end - Instant::now())
                .expect("no answer")
            {
                Event::Answer(a) if a.id == ticket.id => match a.result {
                    Ok(Reply::Popup(Some(tree))) => return texts(&tree),
                    r => panic!("{r:?}"),
                },
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
    }

    /// Until the popup has `want` and not `gone`.
    fn wait(&self, want: &str, gone: Option<&str>) {
        let end = Instant::now() + WAIT;
        loop {
            let got = self.popup();
            let has = |t: &str| got.iter().any(|g| g == t);
            if has(want) && !gone.is_some_and(has) {
                return;
            }
            assert!(Instant::now() < end, "want {want:?}, not {gone:?}: {got:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn start(dir: &Path) -> Calendar {
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let settings = Settings {
        threads: 2,
        ..Settings::default()
    };
    let rt = Runtime::new(settings, move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    rt.configure(vec![InstanceSpec {
        id: "c".into(),
        plugin: "calendar".into(),
        source: Source::Code {
            name: "calendar".into(),
            code: Arc::from(CALENDAR),
        },
        role: Role::Widget,
        app: None,
        options: serde_json::json!({ "sources": [dir.to_string_lossy()] }),
        budget: 1_000_000,
    }]);
    Calendar { rt, recv }
}

#[test]
fn a_watched_folder_is_read_again_when_it_changes() {
    let dir = std::env::temp_dir().join(format!("wl-dock-calendar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("work")).unwrap();
    std::fs::write(dir.join("work/a.ics"), ics("Alpha")).unwrap();
    let cal = start(&dir);
    cal.wait("Alpha", None);

    // A new file in a subfolder.
    std::fs::write(dir.join("work/b.ics"), ics("Beta")).unwrap();
    cal.wait("Beta", None);

    // A file replaced by a rename, as Evolution saves.
    std::fs::write(dir.join("work/a.tmp"), ics("Gamma")).unwrap();
    std::fs::rename(dir.join("work/a.tmp"), dir.join("work/a.ics")).unwrap();
    cal.wait("Gamma", Some("Alpha"));

    // A calendar folder made later.
    std::fs::create_dir(dir.join("home")).unwrap();
    std::fs::write(dir.join("home/c.ics"), ics("Delta")).unwrap();
    cal.wait("Delta", None);

    // Removed.
    std::fs::remove_file(dir.join("work/b.ics")).unwrap();
    cal.wait("Delta", Some("Beta"));
    std::fs::remove_dir_all(dir).unwrap();
}
