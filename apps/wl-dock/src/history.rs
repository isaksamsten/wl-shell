//! The launch history: which apps were opened from the dock or the
//! launcher, how often and when last, to rank the launcher's matches by.
//! The parsing and the scores are pure, unit tested.
//!
//! ```text
//! $XDG_STATE_HOME/wl-dock/history      (default ~/.local/state/wl-dock/history)
//! ```
//!
//! One app per line: `<count> <last> <id>`, where `last` is in seconds
//! since the epoch and `id` is the app's desktop ID without `.desktop`
//! (its app ID if it has no desktop entry). Lines that don't parse are
//! skipped. The file is written atomically after every launch.
//!
//! An app's frecency is its count, halved for every week since it was
//! last opened; its boost is logarithmic in that, so a few launches
//! matter and a thousand don't drown the match itself.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The frecency's half-life, seconds.
const HALF_LIFE: f64 = 7.0 * 24.0 * 3600.0;
/// Score points per e-fold of frecency.
const WEIGHT: f64 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub count: u32,
    /// Seconds since the epoch.
    pub last: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct History {
    apps: BTreeMap<String, Record>,
}

/// Now, in seconds since the epoch.
pub fn now() -> u64 {
    (SystemTime::now().duration_since(UNIX_EPOCH)).map_or(0, |d| d.as_secs())
}

impl History {
    /// The history in a file's text.
    pub fn parse(text: &str) -> Self {
        let mut apps = BTreeMap::new();
        for line in text.lines() {
            let mut parts = line.trim().splitn(3, ' ');
            let (Some(count), Some(last), Some(id)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Ok(count), Ok(last)) = (count.parse(), last.parse()) else {
                continue;
            };
            let id = id.trim();
            if !id.is_empty() {
                apps.insert(id.to_string(), Record { count, last });
            }
        }
        Self { apps }
    }

    /// The file's text.
    pub fn format(&self) -> String {
        let mut out = String::from("# wl-dock: launches, as <count> <last> <app>\n");
        for (id, r) in &self.apps {
            out.push_str(&format!("{} {} {id}\n", r.count, r.last));
        }
        out
    }

    /// `id` was opened at `now`.
    pub fn record(&mut self, id: &str, now: u64) {
        if id.is_empty() || id.contains('\n') {
            return;
        }
        let r = (self.apps.entry(id.to_string())).or_insert(Record {
            count: 0,
            last: now,
        });
        r.count = r.count.saturating_add(1);
        r.last = r.last.max(now);
    }

    pub fn get(&self, id: &str) -> Option<Record> {
        self.apps.get(id).copied()
    }

    /// `id`'s count, decayed by the time since it was last opened.
    pub fn frecency(&self, id: &str, now: u64) -> f64 {
        self.get(id).map_or(0.0, |r| {
            let age = now.saturating_sub(r.last) as f64;
            r.count as f64 * 0.5f64.powf(age / HALF_LIFE)
        })
    }

    /// What `id`'s history adds to its match score.
    pub fn boost(&self, id: &str, now: u64) -> f64 {
        WEIGHT * self.frecency(id, now).ln_1p()
    }
}

/// The history file, next to the pins.
pub fn path() -> Option<PathBuf> {
    Some(crate::pins::path()?.with_file_name("history"))
}

/// Read the history at `path`; empty if it doesn't exist.
pub fn read(path: &Path) -> io::Result<History> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(History::parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(History::default()),
        Err(e) => Err(e),
    }
}

/// Write `history` to `path` atomically, creating its directory.
pub fn write(path: &Path, history: &History) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".history.{}.tmp", std::process::id()));
    std::fs::write(&tmp, history.format())
        .and_then(|()| std::fs::rename(&tmp, path))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WEEK: u64 = 7 * 24 * 3600;

    #[test]
    fn round_trip() {
        let mut h = History::default();
        h.record("firefox", 100);
        h.record("firefox", 200);
        h.record("org.gnome.Nautilus", 150);
        h.record("", 1);
        h.record("bad\nid", 1);
        assert_eq!(
            h.get("firefox"),
            Some(Record {
                count: 2,
                last: 200
            })
        );
        assert_eq!(History::parse(&h.format()), h);
        let text = "# comment\n3 10 a b c\nx 1 y\n1 2\n\n 2 5 z \n";
        let p = History::parse(text);
        assert_eq!(p.get("a b c"), Some(Record { count: 3, last: 10 }));
        assert_eq!(p.get("z"), Some(Record { count: 2, last: 5 }));
        assert_eq!(p.apps.len(), 2);
    }

    #[test]
    fn frecency_decays() {
        let mut h = History::default();
        for _ in 0..4 {
            h.record("a", 0);
        }
        h.record("b", 2 * WEEK);
        assert_eq!(h.frecency("a", 0), 4.0);
        assert_eq!(h.frecency("a", WEEK), 2.0);
        assert_eq!(h.frecency("a", 2 * WEEK), 1.0);
        assert_eq!(h.frecency("b", 2 * WEEK), 1.0);
        assert_eq!(h.frecency("nope", 0), 0.0);
        // More launches, or more recent ones, boost more, but slowly.
        assert!(h.boost("a", 0) > h.boost("b", 0));
        assert_eq!(h.boost("nope", 0), 0.0);
        let mut many = History::default();
        for _ in 0..1000 {
            many.record("x", 0);
        }
        assert!(many.boost("x", 0) < 100.0);
    }
}
