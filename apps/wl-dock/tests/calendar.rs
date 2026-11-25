//! The bundled calendar plugin against a fake Evolution Data Server on a
//! private bus (skipped without `dbus-daemon`): the calendars to show come
//! from the source registry, their events from a view, and the view's
//! changes show up; times in other zones are read in them, including a
//! zone only EDS has the definition of.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::ui::{Kind, Node};
use wl_shell_lua::{Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source};
use zbus::interface;
use zbus::zvariant::OwnedObjectPath;

const CALENDAR: &str = include_str!("../plugins/calendar.lua");
const WAIT: Duration = Duration::from_secs(10);
const SOURCES: &str = "/org/gnome/evolution/dataserver/SourceManager";
const VIEW_IFACE: &str = "org.gnome.evolution.dataserver.CalendarView";

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

/// A source in the registry.
struct SourceObject {
    uid: String,
    data: String,
}

#[interface(name = "org.gnome.evolution.dataserver.Source")]
impl SourceObject {
    #[zbus(property, name = "UID")]
    fn uid(&self) -> String {
        self.uid.clone()
    }

    #[zbus(property)]
    fn data(&self) -> String {
        self.data.clone()
    }
}

/// Opens the calendar of a source UID at `/cal/<uid>`.
struct Factory;

#[interface(name = "org.gnome.evolution.dataserver.CalendarFactory")]
impl Factory {
    fn open_calendar(&self, uid: String) -> (String, String) {
        (
            format!("/cal/{uid}"),
            "org.gnome.evolution.dataserver.Calendar8".into(),
        )
    }
}

/// A calendar, its view at `/view/<uid>`.
struct Calendar {
    uid: String,
    opened: Arc<Mutex<Vec<String>>>,
}

#[interface(name = "org.gnome.evolution.dataserver.Calendar")]
impl Calendar {
    fn open(&self) -> Vec<String> {
        self.opened.lock().unwrap().push(self.uid.clone());
        Vec::new()
    }

    fn get_view(&self, _query: String) -> OwnedObjectPath {
        OwnedObjectPath::try_from(format!("/view/{}", self.uid)).unwrap()
    }

    fn refresh(&self) {}

    /// The one zone only the invites define: three hours east of UTC.
    fn get_timezone(&self, tzid: String) -> zbus::fdo::Result<String> {
        match tzid.as_str() {
            "Customized Time Zone" => Ok("BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone\r\n\
                BEGIN:STANDARD\r\nDTSTART:16010101T000000\r\nTZOFFSETFROM:+0300\r\n\
                TZOFFSETTO:+0300\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n"
                .into()),
            _ => Err(zbus::fdo::Error::Failed("Object not found".into())),
        }
    }
}

/// A view: its events come with `ObjectsAdded`, sent by the test.
struct View;

#[interface(name = "org.gnome.evolution.dataserver.CalendarView")]
impl View {
    fn start(&self) {}
    fn dispose(&self) {}
}

/// Tomorrow, local time, as an iCalendar DATE.
fn tomorrow() -> String {
    let out = Command::new("date")
        .args(["-d", "tomorrow", "+%Y%m%d"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// An all-day event tomorrow, so the agenda shows it.
fn vevent(uid: &str, title: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART;VALUE=DATE:{}\r\nSUMMARY:{title}\r\nEND:VEVENT\r\n",
        tomorrow()
    )
}

/// An hour from noon tomorrow in a zone only EDS knows.
fn custom(uid: &str, title: &str) -> String {
    let day = tomorrow();
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART;TZID=Customized Time Zone:{day}T120000\r\n\
         DTEND;TZID=Customized Time Zone:{day}T130000\r\nSUMMARY:{title}\r\nEND:VEVENT\r\n"
    )
}

/// An hour from 18:00 in Tokyo, tomorrow and the day after, with the zone
/// by its Windows name, as Outlook writes it.
fn tokyo(uid: &str, title: &str) -> String {
    let day = tomorrow();
    format!(
        "BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTART;TZID=\"Tokyo Standard Time\":{day}T180000\r\n\
         DTEND;TZID=\"Tokyo Standard Time\":{day}T190000\r\nRRULE:FREQ=DAILY;COUNT=2\r\nSUMMARY:{title}\r\nEND:VEVENT\r\n"
    )
}

fn source_data(name: &str, calendar: &str) -> String {
    format!("[Data Source]\nDisplayName={name}\nEnabled=true\nParent=\n\n{calendar}")
}

/// The fake EDS: a registry with a calendar shown, one hidden in GNOME
/// Calendar, and an address book.
fn fake_eds(address: &str, opened: Arc<Mutex<Vec<String>>>) -> zbus::Connection {
    let sources = [
        (
            "work",
            source_data(
                "Work",
                "[Calendar]\nBackendName=caldav\nColor=#ff0000\nSelected=true\n",
            ),
        ),
        (
            "hidden",
            source_data("Hidden", "[Calendar]\nBackendName=local\nSelected=false\n"),
        ),
        (
            "people",
            source_data("People", "[Address Book]\nBackendName=local\n"),
        ),
    ];
    async_io::block_on(async {
        let mut b = zbus::connection::Builder::address(address)?
            .name("org.gnome.evolution.dataserver.Sources5")?
            .name("org.gnome.evolution.dataserver.Calendar8")?
            .serve_at(SOURCES, zbus::fdo::ObjectManager)?
            .serve_at("/org/gnome/evolution/dataserver/CalendarFactory", Factory)?;
        for (i, (uid, data)) in sources.into_iter().enumerate() {
            let source = SourceObject {
                uid: uid.into(),
                data,
            };
            b = b.serve_at(format!("{SOURCES}/Source_{i}"), source)?;
            let calendar = Calendar {
                uid: uid.into(),
                opened: opened.clone(),
            };
            b = b
                .serve_at(format!("/cal/{uid}"), calendar)?
                .serve_at(format!("/view/{uid}"), View)?;
        }
        b.build().await
    })
    .expect("a fake EDS")
}

fn emit(conn: &zbus::Connection, uid: &str, member: &str, items: Vec<String>) {
    async_io::block_on(conn.emit_signal(
        None::<&str>,
        format!("/view/{uid}").as_str(),
        VIEW_IFACE,
        member,
        &(items,),
    ))
    .unwrap();
}

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

    /// Until the popup has `want` and not `gone`, doing `poke` (a signal
    /// that may come before the plugin listens) each time.
    fn wait(&self, want: &str, gone: Option<&str>, poke: impl Fn()) {
        let end = Instant::now() + WAIT;
        loop {
            poke();
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

#[test]
fn shown_calendars_are_read_and_followed() {
    let Some((_daemon, address)) = daemon() else {
        eprintln!("no dbus-daemon: skipped");
        return;
    };
    // The runtime's session bus is this one.
    unsafe { std::env::set_var("DBUS_SESSION_BUS_ADDRESS", &address) };
    // Local time is UTC, for the event in Tokyo.
    unsafe { std::env::set_var("TZ", "UTC") };
    let opened = Arc::new(Mutex::new(Vec::new()));
    let eds = fake_eds(&address, opened.clone());

    let (send, recv) = channel();
    let send = Mutex::new(send);
    let rt = Runtime::new(Settings::default(), move |e| {
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
        options: serde_json::json!({}),
        budget: 1_000_000,
    }]);
    let p = Plugin { rt, recv };

    // The view's events.
    p.wait("Alpha", None, || {
        emit(
            &eds,
            "work",
            "ObjectsAdded",
            vec![
                vevent("a", "Alpha"),
                vevent("b", "Beta"),
                tokyo("t", "Tokyo"),
                custom("c", "Custom"),
            ],
        )
    });
    // 18:00 in Tokyo is 09:00 here, on both days.
    let got = p.popup();
    let after = |title: &str| -> Vec<&str> {
        (got.iter().enumerate())
            .filter(|(_, t)| *t == title)
            .map(|(i, _)| got[i + 1].as_str())
            .collect()
    };
    assert_eq!(
        after("Tokyo"),
        ["09:00 – 10:00", "09:00 – 10:00"],
        "{got:?}"
    );
    // Noon three hours east is 09:00 too, once EDS said what the zone is.
    assert_eq!(after("Custom"), ["09:00 – 10:00"], "{got:?}");
    // Only the calendar shown in GNOME Calendar was opened.
    assert_eq!(*opened.lock().unwrap(), ["work"]);
    // Changed: the new version replaces the old.
    p.wait("Gamma", Some("Alpha"), || {
        emit(&eds, "work", "ObjectsModified", vec![vevent("a", "Gamma")])
    });
    // Removed.
    p.wait("Beta", Some("Gamma"), || {
        emit(&eds, "work", "ObjectsRemoved", vec!["a\n".into()])
    });
}
