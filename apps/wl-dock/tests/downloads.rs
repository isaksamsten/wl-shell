//! The bundled downloads plugin watching a folder: Firefox's `.part` and
//! Chromium's `.crdownload` files show as downloads in progress, a
//! finished one lists in the popup and opens on a click, and cancelled or
//! deleted ones go away.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::channel::PopupEvent;
use wl_shell_lua::ui::{Kind, Node};
use wl_shell_lua::{
    AppInfo, Axis, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source, Tile,
};

const DOWNLOADS: &str = include_str!("../plugins/downloads.lua");
const WAIT: Duration = Duration::from_secs(10);

fn texts(tree: &Node) -> Vec<String> {
    (tree.walk().into_iter())
        .filter_map(|n| match &n.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

struct Plugin {
    rt: Runtime,
    recv: Receiver<Event>,
}

impl Plugin {
    fn ask(&self, kind: RequestKind) -> Reply {
        let ticket = self.rt.request("d", kind, WAIT).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            match self
                .recv
                .recv_timeout(end - Instant::now())
                .expect("no answer")
            {
                Event::Answer(a) if a.id == ticket.id => return a.result.unwrap(),
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
    }

    fn label(&self) -> Option<String> {
        match self.ask(RequestKind::Label) {
            Reply::Label(l) => l,
            r => panic!("{r:?}"),
        }
    }

    fn popup(&self) -> Vec<String> {
        match self.ask(RequestKind::Popup) {
            Reply::Popup(Some(tree)) => texts(&tree),
            r => panic!("{r:?}"),
        }
    }

    /// Whether the icon has a progress ring.
    fn ring(&self) -> bool {
        let tile = Tile {
            w: 96.0,
            h: 96.0,
            span: wl_shell_lua::Span::Normal,
            axis: Axis::Horizontal,
            scale: 1,
        };
        match self.ask(RequestKind::Icon(tile)) {
            Reply::Icon(Some(tree)) => {
                (tree.walk().iter()).any(|n| matches!(n.kind, Kind::Canvas { .. }))
            }
            r => panic!("{r:?}"),
        }
    }

    /// Until `check` holds.
    fn wait<T: std::fmt::Debug>(
        &self,
        what: &str,
        get: impl Fn(&Self) -> T,
        check: impl Fn(&T) -> bool,
    ) {
        let end = Instant::now() + WAIT;
        loop {
            let got = get(self);
            if check(&got) {
                return;
            }
            assert!(Instant::now() < end, "{what}: {got:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn label_has(&self, want: &str) {
        self.wait(want, Self::label, |l| {
            l.as_deref().is_some_and(|l| l.contains(want))
        });
    }

    fn idle(&self) {
        self.wait("no label", Self::label, Option::is_none);
    }

    fn popup_has(&self, want: &str, has: bool) {
        self.wait(want, Self::popup, |t| t.iter().any(|t| t == want) == has);
    }
}

fn start(dir: &Path) -> Plugin {
    start_with(
        None,
        serde_json::json!({
            "folders": [dir.to_string_lossy()],
            "interval": 250,
            "open": ["touch", dir.join(".opened").to_string_lossy()],
        }),
    )
}

fn start_with(app: Option<AppInfo>, options: serde_json::Value) -> Plugin {
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
        id: "d".into(),
        plugin: "downloads".into(),
        source: Source::Code {
            name: "downloads".into(),
            code: Arc::from(DOWNLOADS),
        },
        role: Role::Widget,
        app,
        options,
        budget: 1_000_000,
    }]);
    Plugin { rt, recv }
}

#[test]
fn downloads_in_the_folder_show_and_finish() {
    let dir = std::env::temp_dir().join(format!("wl-dock-downloads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("old.txt"), "here before").unwrap();
    let p = start(&dir);
    p.popup_has("No downloads", true);
    assert_eq!(p.label(), None);

    // Firefox: a placeholder and the .part file, growing.
    std::fs::write(dir.join("big.iso"), "").unwrap();
    std::fs::write(dir.join("big.iso.part"), vec![0u8; 1000]).unwrap();
    p.label_has("big.iso");
    assert!(p.ring());
    std::fs::write(dir.join("big.iso.part"), vec![0u8; 300_000]).unwrap();
    p.label_has("/s");

    // Done: renamed over the placeholder.
    std::fs::rename(dir.join("big.iso.part"), dir.join("big.iso")).unwrap();
    p.popup_has("big.iso", true);
    p.idle();
    assert!(p.popup().iter().any(|t| t == "Recent"));

    // Chromium names a download once it knows the name.
    std::fs::write(dir.join("Unconfirmed 123.crdownload"), vec![0u8; 10]).unwrap();
    p.label_has("Download");
    std::fs::rename(
        dir.join("Unconfirmed 123.crdownload"),
        dir.join("film.mp4.crdownload"),
    )
    .unwrap();
    p.label_has("film.mp4");
    std::fs::rename(dir.join("film.mp4.crdownload"), dir.join("film.mp4")).unwrap();
    p.popup_has("film.mp4", true);
    p.idle();

    // Cancelled: nothing finishes.
    std::fs::write(dir.join("gone.zip.part"), vec![0u8; 10]).unwrap();
    p.label_has("gone.zip");
    std::fs::remove_file(dir.join("gone.zip.part")).unwrap();
    p.idle();
    p.popup_has("gone.zip", false);
    assert!(!p.popup().iter().any(|t| t == "old.txt"));

    // A click opens a finished download.
    let key = format!("open:{}", dir.join("film.mp4").display());
    p.popup();
    assert_eq!(
        p.ask(RequestKind::Interaction {
            key,
            event: PopupEvent::Click
        }),
        Reply::Done
    );
    let end = Instant::now() + WAIT;
    while !dir.join(".opened").exists() {
        assert!(Instant::now() < end, "not opened");
        std::thread::sleep(Duration::from_millis(50));
    }

    // Deleted: off the list.
    std::fs::remove_file(dir.join("big.iso")).unwrap();
    p.popup_has("big.iso", false);
    p.popup_has("film.mp4", true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn on_a_browser_only_its_own_downloads_show() {
    let dir = std::env::temp_dir().join(format!("wl-dock-downloads-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p = start_with(
        Some(AppInfo::new("chromium")),
        serde_json::json!({ "folders": [dir.to_string_lossy()], "interval": 250, "style": "bar" }),
    );
    // Idle: the app's own icon, label and no popup.
    assert_eq!(p.label(), None);
    assert_eq!(p.ask(RequestKind::Popup), Reply::Popup(None));

    // Firefox's file isn't Chromium's.
    std::fs::write(dir.join("a.zip.part"), vec![0u8; 10]).unwrap();
    std::fs::write(dir.join("b.zip.crdownload"), vec![0u8; 10]).unwrap();
    p.label_has("b.zip —");
    assert!(p.ring());
    std::fs::remove_file(dir.join("b.zip.crdownload")).unwrap();
    p.idle();
    std::fs::remove_dir_all(dir).unwrap();
}
