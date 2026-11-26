//! The bundled downloads plugin watching a folder: Firefox's `.part` and
//! Chromium's `.crdownload` files show as downloads in progress, with a
//! disc at the icon's corner that turns, fills or waits; a finished one
//! lists in the popup, opens on a click and shows in its folder through
//! FileManager1; cancelled or deleted ones go away, and Clear empties the
//! list.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::channel::PopupEvent;
use wl_shell_lua::ui::{Drive, Kind, Node};
use wl_shell_lua::{
    AppInfo, Axis, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source, Tile,
};
use zbus::interface;
use zbus::zvariant::Value;

const DOWNLOADS: &str = include_str!("../plugins/downloads.lua");
const MODULES: &[(&str, &str)] = &[("anim", include_str!("../plugins/anim.lua"))];
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
            // An app's widget without downloads has no popup.
            Reply::Popup(None) => Vec::new(),
            r => panic!("{r:?}"),
        }
    }
    /// The icon's progress canvas: None if there is none, else what
    /// drives it (None: it stands still).
    fn disc(&self) -> Option<Option<Drive>> {
        let tile = Tile {
            w: 96.0,
            h: 96.0,
            span: wl_shell_lua::Span::Normal,
            axis: Axis::Horizontal,
            scale: 1,
        };
        match self.ask(RequestKind::Icon(tile)) {
            Reply::Icon(Some(tree)) => (tree.walk().iter()).find_map(|n| match &n.kind {
                Kind::Canvas { anim, .. } => Some(anim.as_ref().map(|a| a.drive)),
                _ => None,
            }),
            Reply::Icon(None) => None,
            r => panic!("{r:?}"),
        }
    }

    fn click(&self, key: String) {
        let event = PopupEvent::Click;
        assert_eq!(self.ask(RequestKind::Interaction { key, event }), Reply::Done);
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
        modules: MODULES,
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

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("wl-dock-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn turning(d: &Option<Option<Drive>>) -> bool {
    matches!(d, Some(Some(Drive::Loop { .. })))
}

#[test]
fn downloads_in_the_folder_show_and_finish() {
    let dir = temp("downloads");
    std::fs::write(dir.join("old.txt"), "here before").unwrap();
    let p = start(&dir);
    p.popup_has("No downloads", true);
    assert_eq!(p.label(), None);

    // Firefox: a placeholder and the .part file, growing. Its size isn't
    // known: the disc turns.
    std::fs::write(dir.join("big.iso"), "").unwrap();
    std::fs::write(dir.join("big.iso.part"), vec![0u8; 1000]).unwrap();
    p.label_has("big.iso");
    assert!(turning(&p.disc()), "{:?}", p.disc());
    std::fs::write(dir.join("big.iso.part"), vec![0u8; 300_000]).unwrap();
    p.label_has("/s");

    // Done: renamed over the placeholder. The disc is full (still) for a
    // moment, then the standalone widget's own icon is left.
    std::fs::rename(dir.join("big.iso.part"), dir.join("big.iso")).unwrap();
    p.idle();
    p.popup_has("big.iso", true);
    assert!(matches!(p.disc(), Some(None) | None));
    p.wait("the disc goes", Plugin::disc, Option::is_none);
    // Only finished ones: no section headers.
    let texts = p.popup();
    assert!(!texts.iter().any(|t| t == "Recent" || t == "In progress"));

    // Chromium names a download once it knows the name.
    std::fs::write(dir.join("Unconfirmed 123.crdownload"), vec![0u8; 10]).unwrap();
    p.label_has("Starting…");
    p.popup_has("Starting…", true);
    // Both sections: both headers.
    p.popup_has("In progress", true);
    p.popup_has("Recent", true);
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
    p.popup();
    p.click(format!("open:{}", dir.join("film.mp4").display()));
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

/// A download that stops growing waits: "Waiting" for its speed, and the
/// disc stands still. Clear empties the list, and what finishes after it
/// shows up again.
#[test]
fn waiting_downloads_and_clear() {
    let dir = temp("downloads-wait");
    let p = start(&dir);
    std::fs::write(dir.join("slow.iso.part"), vec![0u8; 10]).unwrap();
    p.label_has("slow.iso");
    // Nothing arrives for a few seconds.
    p.label_has("Waiting");
    assert!(!p.label().unwrap().contains("/s"));
    assert!(p.popup().iter().any(|t| t == "10 B · Waiting"));
    assert_eq!(p.disc(), Some(None), "still");

    std::fs::rename(dir.join("slow.iso.part"), dir.join("slow.iso")).unwrap();
    p.idle();
    p.popup_has("slow.iso", true);
    p.popup();
    p.click("clear".into());
    p.popup_has("slow.iso", false);
    p.popup_has("No downloads", true);

    std::fs::write(dir.join("next.bin.part"), vec![0u8; 10]).unwrap();
    p.label_has("next.bin");
    std::fs::rename(dir.join("next.bin.part"), dir.join("next.bin")).unwrap();
    p.idle();
    p.popup_has("next.bin", true);
    assert!(!p.popup().iter().any(|t| t == "slow.iso"));
    std::fs::remove_dir_all(dir).unwrap();
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A private session bus.
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

type Log = Arc<Mutex<Vec<String>>>;

/// A file manager that notes what it is asked to show.
struct FileManager(Log);

#[interface(name = "org.freedesktop.FileManager1")]
impl FileManager {
    fn show_items(&self, uris: Vec<String>, _startup_id: String) {
        self.0.lock().unwrap().extend(uris);
    }
}

/// On a browser (with a private session bus, if there is `dbus-daemon`;
/// this is the only test here that uses the bus, which the runtime
/// connects to once): only its own kind of downloads; the progress it
/// publishes (LauncherEntry) fills the bar, easing; and a finished one
/// shows in its folder.
#[test]
fn on_a_browser_only_its_own_downloads_show() {
    let bus = daemon();
    if let Some((_, address)) = &bus {
        // The runtime's session bus is this one.
        unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", address) };
    } else {
        eprintln!("no dbus-daemon: D-Bus parts skipped");
    }
    let shown = Log::default();
    let conns = bus.as_ref().map(|(_, address)| {
        async_io::block_on(async {
            let files = zbus::connection::Builder::address(address.as_str())?
                .name("org.freedesktop.FileManager1")?
                .serve_at("/org/freedesktop/FileManager1", FileManager(shown.clone()))?
                .build()
                .await?;
            let browser = zbus::connection::Builder::address(address.as_str())?
                .build()
                .await?;
            Ok::<_, zbus::Error>((files, browser))
        })
        .expect("a fake file manager")
    });
    let dir = temp("downloads-app");
    let p = start_with(
        Some(AppInfo::new("chromium")),
        serde_json::json!({ "folders": [dir.to_string_lossy()], "interval": 250, "style": "bar" }),
    );
    // Idle: the app's own icon, label and no popup.
    assert_eq!(p.label(), None);
    assert_eq!(p.ask(RequestKind::Popup), Reply::Popup(None));
    assert_eq!(p.disc(), None);

    // Firefox's file isn't Chromium's.
    std::fs::write(dir.join("a.zip.part"), vec![0u8; 10]).unwrap();
    std::fs::write(dir.join("b z.zip.crdownload"), vec![0u8; 10]).unwrap();
    p.label_has("b z.zip —");
    assert!(turning(&p.disc()), "the bar sweeps: {:?}", p.disc());

    if let Some((_, browser)) = &conns {
        // The browser publishes its progress: the bar fills to it, easing.
        let progress = HashMap::from([
            ("progress-visible", Value::Bool(true)),
            ("progress", Value::F64(0.42)),
        ]);
        let body = ("application://chromium.desktop", progress);
        let end = Instant::now() + WAIT;
        while !p.label().is_some_and(|l| l.contains("42%")) {
            assert!(Instant::now() < end, "no progress: {:?}", p.label());
            async_io::block_on(browser.emit_signal(
                None::<&str>,
                "/",
                "com.canonical.Unity.LauncherEntry",
                "Update",
                &body,
            ))
            .unwrap();
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(
            p.disc(),
            Some(Some(Drive::Value {
                value: 0.42,
                transition: 300.0,
                live: None
            }))
        );

        // Finished: Show in folder asks the file manager for its URI.
        std::fs::rename(dir.join("b z.zip.crdownload"), dir.join("b z.zip")).unwrap();
        p.idle();
        p.popup_has("b z.zip", true);
        p.click(format!("show:{}", dir.join("b z.zip").display()));
        let want = format!("file://{}", dir.join("b%20z.zip").display());
        let end = Instant::now() + WAIT;
        while !shown.lock().unwrap().contains(&want) {
            assert!(Instant::now() < end, "{want} not in {:?}", shown.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        std::fs::remove_file(dir.join("b z.zip.crdownload")).unwrap();
    }
    p.idle();
    std::fs::remove_dir_all(dir).unwrap();
}
