//! The bundled recent plugin reading a recently-used.xbel: an app's own
//! files only, newest first, without those that are gone, with their
//! thumbnails from the cache; following the file as it changes
//! (`dock.watch`); and opening a file on click.

use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wl_shell_lua::channel::PopupEvent;
use wl_shell_lua::ui::{Kind, Node};
use wl_shell_lua::{
    AppInfo, Event, InstanceSpec, Reply, RequestKind, Role, Runtime, Settings, Source,
};
use wl_shell_render::files::{file_uri, thumbnail_name};

const RECENT: &str = include_str!("../plugins/recent.lua");
const WAIT: Duration = Duration::from_secs(10);

/// What the popup shows: each row's file name and image (if any), and the
/// row's key.
#[derive(Debug, PartialEq)]
struct Row {
    name: String,
    image: Option<String>,
    key: String,
}

fn rows(tree: &Node) -> Vec<Row> {
    (tree.walk().into_iter())
        .filter(|n| n.key.as_deref().is_some_and(|k| k.starts_with("file")))
        .map(|n| {
            let nodes = n.walk();
            Row {
                name: (nodes.iter())
                    .find_map(|c| match &c.kind {
                        Kind::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap(),
                image: nodes.iter().find_map(|c| match &c.kind {
                    Kind::Image { src, .. } => Some(src.clone()),
                    _ => None,
                }),
                key: n.key.clone().unwrap(),
            }
        })
        .collect()
}

/// One bookmark: `path`, used by `app` (`name`, `exec`) at `time`.
fn bookmark(path: &Path, mime: &str, name: &str, exec: &str, time: &str) -> String {
    let uri = file_uri(path);
    format!(
        r#"  <bookmark href="{uri}" added="{time}" modified="{time}" visited="{time}">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type type="{mime}"/>
        <bookmark:applications>
          <bookmark:application name="{name}" exec="{exec}" modified="{time}" count="1"/>
        </bookmark:applications>
      </metadata>
    </info>
  </bookmark>
"#
    )
}

fn xbel(bookmarks: &[String]) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\"\n      \
         xmlns:bookmark=\"http://www.freedesktop.org/standards/desktop-bookmarks\"\n      \
         xmlns:mime=\"http://www.freedesktop.org/standards/shared-mime-info\">\n{}</xbel>\n",
        bookmarks.concat()
    )
}

struct Recent {
    rt: Runtime,
    recv: Receiver<Event>,
}

impl Recent {
    fn request(&self, kind: RequestKind) -> Reply {
        let ticket = self.rt.request("r", kind, WAIT).unwrap();
        let end = Instant::now() + WAIT;
        loop {
            match (self.recv)
                .recv_timeout(end - Instant::now())
                .expect("no answer")
            {
                Event::Answer(a) if a.id == ticket.id => return a.result.unwrap(),
                Event::Failed { error, .. } => panic!("{error}"),
                _ => {}
            }
        }
    }

    fn popup(&self) -> Vec<Row> {
        match self.request(RequestKind::Popup) {
            Reply::Popup(Some(tree)) => rows(&tree),
            Reply::Popup(None) => Vec::new(),
            r => panic!("{r:?}"),
        }
    }

    /// Until the popup lists the files `names`, in order.
    fn wait(&self, names: &[&str]) -> Vec<Row> {
        let end = Instant::now() + WAIT;
        loop {
            let got = self.popup();
            if got
                .iter()
                .map(|r| r.name.as_str())
                .eq(names.iter().copied())
            {
                return got;
            }
            assert!(Instant::now() < end, "want {names:?}: {got:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn start(file: &Path, out: &Path) -> Recent {
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
    let app = AppInfo {
        id: "org.gnome.gedit".into(),
        desktop: Some("org.gnome.gedit".into()),
        wm_class: None,
        name: Some("Text Editor".into()),
    };
    rt.configure(vec![InstanceSpec {
        id: "r".into(),
        plugin: "recent".into(),
        source: Source::Code {
            name: "recent".into(),
            code: Arc::from(RECENT),
        },
        role: Role::Widget,
        app: Some(app),
        options: serde_json::json!({
            "file": file.to_string_lossy(),
            "open": ["cp", "{path}", out.to_string_lossy()],
        }),
        budget: 1_000_000,
    }]);
    Recent { rt, recv }
}

#[test]
fn an_apps_recent_files() {
    let dir = std::env::temp_dir().join(format!("wl-dock-recent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let docs = dir.join("docs dir");
    let thumbs = dir.join("cache/thumbnails/normal");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::create_dir_all(&thumbs).unwrap();
    // The only test in this binary, before any thread reads it.
    unsafe { std::env::set_var("XDG_CACHE_HOME", dir.join("cache")) };

    let a = docs.join("a & b.txt");
    let b = docs.join("b.txt");
    let gone = docs.join("gone.txt");
    let other = docs.join("other.txt");
    std::fs::write(&a, "a").unwrap();
    std::fs::write(&b, "b").unwrap();
    std::fs::write(&other, "o").unwrap();
    // A thumbnail made after the file, so it is current.
    let thumb = thumbs.join(thumbnail_name(&a));
    std::fs::write(&thumb, "png").unwrap();

    let gedit = |p: &Path, time: &str| {
        // As GTK records it: by name, with the command line quoted.
        bookmark(p, "text/plain", "Text Editor", "&apos;gedit %u&apos;", time)
    };
    let mut marks = vec![
        gedit(&a, "2024-05-01T10:00:00.123456Z"),
        gedit(&b, "2024-05-02T10:00:00Z"),
        gedit(&gone, "2024-05-03T10:00:00Z"),
        bookmark(
            &other,
            "text/plain",
            "Firefox",
            "&apos;firefox %u&apos;",
            "2024-05-04T10:00:00Z",
        ),
    ];
    let file = dir.join("recently-used.xbel");
    std::fs::write(&file, xbel(&marks)).unwrap();
    let out = dir.join("opened");
    let recent = start(&file, &out);

    // Newest first; not Firefox's, not the one that is gone.
    let rows = recent.wait(&["b.txt", "a & b.txt"]);
    assert_eq!(rows[0].image, None);
    assert_eq!(rows[1].image.as_deref(), Some(thumb.to_str().unwrap()));

    // Opened again, recorded by its command (a reverse-DNS ID's last part
    // matches it), replacing the file by a rename as GLib saves it.
    let c = docs.join("c.txt");
    std::fs::write(&c, "c").unwrap();
    marks.push(bookmark(
        &c,
        "text/plain",
        "gedit-ish",
        "gedit %f",
        "2024-05-05T10:00:00Z",
    ));
    std::fs::write(dir.join("xbel.tmp"), xbel(&marks)).unwrap();
    std::fs::rename(dir.join("xbel.tmp"), &file).unwrap();
    let rows = recent.wait(&["c.txt", "b.txt", "a & b.txt"]);

    // Clicking a row opens its file.
    let key = rows[2].key.clone();
    let event = PopupEvent::Click;
    assert_eq!(
        recent.request(RequestKind::Interaction { key, event }),
        Reply::Done
    );
    let end = Instant::now() + WAIT;
    while std::fs::read_to_string(&out).ok().as_deref() != Some("a") {
        assert!(Instant::now() < end, "not opened");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::remove_dir_all(dir).unwrap();
}
