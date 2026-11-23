//! The bundled player plugin (and its `mpris` module) against fake MPRIS
//! players on a private bus (skipped without `dbus-daemon`): matching
//! players to apps by their names or the `player` option, a standalone
//! widget following whichever plays last, players coming and going, and
//! the popup's buttons and seek bar reaching the player.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::channel::PopupEvent;
use wl_shell_lua::ui::{Kind, Node};
use wl_shell_lua::{
    AppInfo, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source,
};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

const PLAYER: &str = include_str!("../plugins/player.lua");
const MODULES: &[(&str, &str)] = &[("mpris", include_str!("../plugins/mpris.lua"))];
const PATH: &str = "/org/mpris/MediaPlayer2";
const WAIT: Duration = Duration::from_secs(10);

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

type Log = Arc<Mutex<Vec<String>>>;

struct Root {
    identity: String,
    desktop: String,
}

#[interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {}

    #[zbus(property)]
    fn identity(&self) -> String {
        self.identity.clone()
    }

    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        self.desktop.clone()
    }

    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
}

struct Player {
    title: String,
    playing: bool,
    position: i64,
    log: Log,
}

impl Player {
    fn log(&self, what: String) {
        self.log.lock().unwrap().push(what);
    }
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn play_pause(
        &mut self,
        #[zbus(signal_emitter)] e: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.playing = !self.playing;
        self.log("PlayPause".into());
        self.playback_status_changed(&e).await?;
        Ok(())
    }

    async fn next(
        &mut self,
        #[zbus(signal_emitter)] e: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.log("Next".into());
        self.title = format!("{} (next)", self.title);
        self.position = 0;
        self.metadata_changed(&e).await?;
        Ok(())
    }

    fn previous(&self) {
        self.log("Previous".into());
    }

    async fn set_position(
        &mut self,
        track: ObjectPath<'_>,
        position: i64,
        #[zbus(signal_emitter)] e: SignalEmitter<'_>,
    ) -> zbus::fdo::Result<()> {
        self.log(format!("SetPosition {track} {position}"));
        self.position = position;
        Self::seeked(&e, position).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn seeked(e: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        if self.playing { "Playing" } else { "Paused" }.into()
    }

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let v = |v: Value| v.try_to_owned().unwrap();
        HashMap::from([
            (
                "mpris:trackid".into(),
                v(Value::ObjectPath(ObjectPath::try_from("/track/1").unwrap())),
            ),
            ("xesam:title".into(), v(Value::from(self.title.as_str()))),
            (
                "xesam:artist".into(),
                v(Value::from(vec!["Artist", "Band"])),
            ),
            ("mpris:length".into(), v(Value::I64(180_000_000))),
            (
                "mpris:artUrl".into(),
                v(Value::from("https://example.com/art.png")),
            ),
        ])
    }

    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.position
    }

    #[zbus(property)]
    fn rate(&self) -> f64 {
        1.0
    }

    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_play(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_pause(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_seek(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

/// A fake player on the bus as `org.mpris.MediaPlayer2.<suffix>`, until
/// the sender is dropped.
fn fake(
    address: &str,
    suffix: &str,
    desktop: &str,
    title: &str,
    playing: bool,
    log: Log,
) -> Sender<()> {
    let (stop, stopped) = channel::<()>();
    let (ready, is_ready) = channel();
    let address = address.to_string();
    let name = format!("org.mpris.MediaPlayer2.{suffix}");
    let (desktop, title) = (desktop.to_string(), title.to_string());
    std::thread::spawn(move || {
        let conn = async_io::block_on(async {
            zbus::connection::Builder::address(address.as_str())?
                .name(name.as_str())?
                .serve_at(
                    PATH,
                    Root {
                        identity: title.clone(),
                        desktop,
                    },
                )?
                .serve_at(
                    PATH,
                    Player {
                        title,
                        playing,
                        position: 5_000_000,
                        log,
                    },
                )?
                .build()
                .await
        })
        .expect("a fake player");
        ready.send(()).unwrap();
        // Serving (on the connection's own thread) until told to stop.
        let _ = stopped.recv();
        drop(conn);
    });
    is_ready.recv_timeout(WAIT).unwrap();
    stop
}

/// Call `method` on player `suffix`, from outside.
fn call(address: &str, suffix: &str, method: &str) {
    async_io::block_on(async {
        let conn = zbus::connection::Builder::address(address)?.build().await?;
        conn.call_method(
            Some(format!("org.mpris.MediaPlayer2.{suffix}").as_str()),
            PATH,
            Some("org.mpris.MediaPlayer2.Player"),
            method,
            &(),
        )
        .await?;
        Ok::<_, zbus::Error>(())
    })
    .unwrap();
}

fn spec(id: &str, app: Option<AppInfo>, options: serde_json::Value) -> InstanceSpec {
    InstanceSpec {
        id: id.into(),
        plugin: "player".into(),
        source: Source::Code {
            name: "player".into(),
            code: Arc::from(PLAYER),
        },
        role: Role::Widget,
        app,
        options,
        budget: 1_000_000,
    }
}

struct Plugins {
    rt: Runtime,
    recv: Receiver<Event>,
}

impl Plugins {
    fn request(&self, id: &str, kind: RequestKind) -> Result<Reply, String> {
        let ticket = self.rt.request(id, kind, WAIT).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            match self
                .recv
                .recv_timeout(end - Instant::now())
                .expect("no answer")
            {
                Event::Answer(a) if a.id == ticket.id => return a.result,
                Event::Failed { instance, error } => panic!("{instance}: {error}"),
                _ => {}
            }
        }
    }

    fn label(&self, id: &str) -> Option<String> {
        match self.request(id, RequestKind::Label) {
            Ok(Reply::Label(l)) => l,
            r => panic!("{r:?}"),
        }
    }

    /// Until `id`'s label is `want`.
    fn wait_label(&self, id: &str, want: Option<&str>) {
        let end = Instant::now() + WAIT;
        loop {
            let got = self.label(id);
            if got.as_deref() == want {
                return;
            }
            assert!(Instant::now() < end, "{id}: {got:?}, want {want:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn popup(&self, id: &str) -> Option<Arc<Node>> {
        match self.request(id, RequestKind::Popup) {
            Ok(Reply::Popup(tree)) => tree,
            r => panic!("{r:?}"),
        }
    }

    fn interact(&self, id: &str, key: &str, event: PopupEvent) {
        let kind = RequestKind::Interaction {
            key: key.into(),
            event,
        };
        assert_eq!(self.request(id, kind), Ok(Reply::Done));
    }
}

fn texts(tree: &Node) -> Vec<String> {
    (tree.walk().into_iter())
        .filter_map(|n| match &n.kind {
            Kind::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Until `log` has `want`.
fn wait_log(log: &Log, want: &str) {
    let end = Instant::now() + WAIT;
    while !log.lock().unwrap().iter().any(|l| l == want) {
        assert!(
            Instant::now() < end,
            "{want:?} not in {:?}",
            log.lock().unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn players_are_matched_followed_and_controlled() {
    let Some((_daemon, address)) = daemon() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    // The runtime's session bus is this one.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    let spotify_log = Log::default();
    let chromium_log = Log::default();
    let _spotify = fake(
        &address,
        "spotify",
        "spotify",
        "Song",
        true,
        spotify_log.clone(),
    );
    let chromium = fake(
        &address,
        "chromium.instance4242",
        "chromium",
        "Video",
        false,
        chromium_log.clone(),
    );
    let (send, recv) = channel();
    let send = Mutex::new(send);
    let settings = Settings {
        modules: MODULES,
        ..Settings::default()
    };
    let rt = Runtime::new(settings, move |e| {
        let _ = send.lock().unwrap().send(e);
    })
    .unwrap();
    let none = serde_json::json!({});
    // A Flatpak's app ID the player doesn't go by: matched once the
    // dock knows its desktop entry.
    let flatpak = AppInfo::new("org.chromium.Chromium");
    rt.configure(vec![
        spec("spotify", Some(AppInfo::new("Spotify")), none.clone()),
        spec("flatpak", Some(flatpak.clone()), none.clone()),
        spec(
            "option",
            Some(AppInfo::new("foot")),
            serde_json::json!({ "player": "chrom*" }),
        ),
        spec("nowplaying", None, none.clone()),
        spec("other", Some(AppInfo::new("other")), none),
    ]);
    let p = Plugins { rt, recv };

    // By the bus name, ignoring case.
    p.wait_label("spotify", Some("Song – Artist, Band"));
    // By the `player` option.
    p.wait_label("option", Some("Video – Artist, Band"));
    // Nothing matches: the plain app, no popup.
    p.wait_label("other", None);
    assert!(p.popup("other").is_none());
    // Not by its app ID; by its desktop entry, once known.
    assert_eq!(p.label("flatpak"), None);
    p.rt.set_app(
        "flatpak",
        AppInfo {
            desktop: Some("chromium".into()),
            ..flatpak
        },
    );
    p.wait_label("flatpak", Some("Video – Artist, Band"));
    // Standalone: the one playing.
    p.wait_label("nowplaying", Some("Song – Artist, Band"));
    // Both playing: the one that changed last.
    call(&address, "chromium.instance4242", "PlayPause");
    p.wait_label("nowplaying", Some("Video – Artist, Band"));
    // Paused again: back to the one still playing.
    call(&address, "chromium.instance4242", "PlayPause");
    p.wait_label("nowplaying", Some("Song – Artist, Band"));

    // The popup: cover, title, time, and the buttons reach the player.
    let tree = p.popup("spotify").expect("a popup");
    let t = texts(&tree);
    assert!(t.contains(&"Song".to_string()), "{t:?}");
    assert!(t.contains(&"Artist, Band".to_string()), "{t:?}");
    assert!(t.contains(&"3:00".to_string()), "{t:?}");
    assert!(tree.shows_image("https://example.com/art.png"));
    let slider = (tree.walk().into_iter())
        .find_map(|n| match n.kind {
            Kind::Slider { max, live, .. } => Some((max, live)),
            _ => None,
        })
        .unwrap();
    assert_eq!(slider.0, 180.0);
    assert_eq!(
        slider.1.map(|l| l.rate),
        Some(1.0),
        "it moves while playing"
    );
    p.interact("spotify", "play", PopupEvent::Click);
    wait_log(&spotify_log, "PlayPause");
    p.interact("spotify", "seek", PopupEvent::Commit(30.0));
    wait_log(&spotify_log, "SetPosition /track/1 30000000");
    p.interact("spotify", "next", PopupEvent::Click);
    wait_log(&spotify_log, "Next");
    p.wait_label("spotify", Some("Song (next) – Artist, Band"));
    // Paused now: the seek bar holds still.
    let tree = p.popup("spotify").unwrap();
    let live = (tree.walk().into_iter()).find_map(|n| match n.kind {
        Kind::Slider { live, .. } => Some(live),
        _ => None,
    });
    assert_eq!(live, Some(None));

    // Gone: the widgets it backed are the plain apps again; back, and
    // they show it again.
    drop(chromium);
    p.wait_label("option", None);
    p.wait_label("flatpak", None);
    let _chromium = fake(
        &address,
        "chromium.instance7",
        "chromium",
        "Again",
        false,
        chromium_log,
    );
    p.wait_label("option", Some("Again – Artist, Band"));
    p.wait_label("flatpak", Some("Again – Artist, Band"));
}
