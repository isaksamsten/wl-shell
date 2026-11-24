//! What launcher actions act on, and which actions apply: subjects as
//! plain tables (part of `api = 1`), `when` filters evaluated in Rust
//! (so building a Tab level never waits on Lua), and the choices an
//! action's `choices(s)` answers.
//!
//! | `kind`   | Fields                                                                  |
//! | -------- | ----------------------------------------------------------------------- |
//! | `file`   | `path`, `name`, `dir`, `mime` (the type and its parents, nearest first) |
//! | `app`    | `id`, `name`, `running`, `pinned`, `windows` (`id`, `title`)            |
//! | `window` | `id`, `title`, `app_id`, `app_name`                                     |
//! | `item`   | `source`, `id`, `title`, `data` (an item from a plugin source)          |
//!
//! A `when` table holds any of these; all that are given must hold:
//!
//! | Key       | Holds for                                                             |
//! | --------- | --------------------------------------------------------------------- |
//! | `kind`    | a subject of this kind (or of one of a list of kinds)                 |
//! | `mime`    | a file whose type, or a parent of it, matches the glob (`image/*`)    |
//! | `path`    | a file whose path matches the glob (`~/` expands)                     |
//! | `dir`     | a folder (true) or a file (false)                                     |
//! | `app`     | an app whose ID, or a window whose app ID, matches the glob           |
//! | `running` | an app with windows (true) or without (false); a window is running    |
//! | `source`  | an item from the source with this ID                                  |
//!
//! Globs have `*` (any run of characters, `/` too) and `?` (one); `mime`
//! and `app` ignore case.

use mlua::{Lua, LuaSerdeExt, Table, Value};

/// What kind of thing a subject is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubjectKind {
    File,
    App,
    Window,
    Item,
}

impl SubjectKind {
    pub fn name(self) -> &'static str {
        match self {
            SubjectKind::File => "file",
            SubjectKind::App => "app",
            SubjectKind::Window => "window",
            SubjectKind::Item => "item",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Self::File, Self::App, Self::Window, Self::Item]
            .into_iter()
            .find(|k| k.name() == s)
    }
}

/// One of an app's windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectWindow {
    pub id: String,
    pub title: String,
}

/// What a launcher action acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    File {
        path: String,
        name: String,
        dir: bool,
        /// The type and every type it is a kind of, nearest first.
        mime: Vec<String>,
    },
    App {
        /// Its desktop ID.
        id: String,
        name: String,
        running: bool,
        pinned: bool,
        /// The most recently focused first.
        windows: Vec<SubjectWindow>,
    },
    Window {
        id: String,
        title: String,
        app_id: String,
        app_name: String,
    },
    /// An item from a plugin source (reserved: sources aren't searched
    /// yet).
    Item {
        source: String,
        id: String,
        title: String,
        data: serde_json::Value,
    },
}

impl Subject {
    pub fn kind(&self) -> SubjectKind {
        match self {
            Subject::File { .. } => SubjectKind::File,
            Subject::App { .. } => SubjectKind::App,
            Subject::Window { .. } => SubjectKind::Window,
            Subject::Item { .. } => SubjectKind::Item,
        }
    }

    /// The table a plugin gets.
    pub(crate) fn to_lua(&self, lua: &Lua) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("kind", self.kind().name())?;
        match self {
            Subject::File {
                path,
                name,
                dir,
                mime,
            } => {
                t.set("path", path.as_str())?;
                t.set("name", name.as_str())?;
                t.set("dir", *dir)?;
                t.set(
                    "mime",
                    lua.create_sequence_from(mime.iter().map(String::as_str))?,
                )?;
            }
            Subject::App {
                id,
                name,
                running,
                pinned,
                windows,
            } => {
                t.set("id", id.as_str())?;
                t.set("name", name.as_str())?;
                t.set("running", *running)?;
                t.set("pinned", *pinned)?;
                let list = lua.create_table()?;
                for w in windows {
                    let wt = lua.create_table()?;
                    wt.set("id", w.id.as_str())?;
                    wt.set("title", w.title.as_str())?;
                    list.push(wt)?;
                }
                t.set("windows", list)?;
            }
            Subject::Window {
                id,
                title,
                app_id,
                app_name,
            } => {
                t.set("id", id.as_str())?;
                t.set("title", title.as_str())?;
                t.set("app_id", app_id.as_str())?;
                t.set("app_name", app_name.as_str())?;
            }
            Subject::Item {
                source,
                id,
                title,
                data,
            } => {
                t.set("source", source.as_str())?;
                t.set("id", id.as_str())?;
                t.set("title", title.as_str())?;
                t.set("data", crate::host::to_lua(lua, data)?)?;
            }
        }
        Ok(t)
    }
}

/// An action's `when`: which subjects it applies to.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct When {
    /// Empty: any kind.
    pub kinds: Vec<SubjectKind>,
    pub mime: Option<String>,
    pub path: Option<String>,
    pub dir: Option<bool>,
    pub app: Option<String>,
    pub running: Option<bool>,
    pub source: Option<String>,
}

impl When {
    /// Whether it holds for `s`. A key that doesn't apply to the
    /// subject's kind (`mime` for an app) doesn't hold.
    pub fn matches(&self, s: &Subject) -> bool {
        if !self.kinds.is_empty() && !self.kinds.contains(&s.kind()) {
            return false;
        }
        let file = match s {
            Subject::File {
                path, dir, mime, ..
            } => Some((path, *dir, mime)),
            _ => None,
        };
        if let Some(pattern) = &self.mime {
            let pattern = pattern.to_lowercase();
            let any = file
                .is_some_and(|(_, _, mime)| mime.iter().any(|m| glob(&pattern, &m.to_lowercase())));
            if !any {
                return false;
            }
        }
        if let Some(pattern) = &self.path {
            let pattern = crate::host::expand(pattern);
            let pattern = pattern.to_string_lossy();
            if !file.is_some_and(|(path, _, _)| glob(&pattern, path)) {
                return false;
            }
        }
        if let Some(want) = self.dir
            && file.is_none_or(|(_, dir, _)| dir != want)
        {
            return false;
        }
        if let Some(pattern) = &self.app {
            let id = match s {
                Subject::App { id, .. } => Some(id),
                Subject::Window { app_id, .. } => Some(app_id),
                _ => None,
            };
            let pattern = pattern.to_lowercase();
            if !id.is_some_and(|id| glob(&pattern, &id.to_lowercase())) {
                return false;
            }
        }
        if let Some(want) = self.running {
            let running = match s {
                Subject::App { running, .. } => Some(*running),
                Subject::Window { .. } => Some(true),
                _ => None,
            };
            if running != Some(want) {
                return false;
            }
        }
        if let Some(want) = &self.source {
            match s {
                Subject::Item { source, .. } if source == want => {}
                _ => return false,
            }
        }
        true
    }

    /// Read `when` (nil: every subject).
    pub(crate) fn read(v: &Value) -> Result<Self, String> {
        let t = match v {
            Value::Nil => return Ok(When::default()),
            Value::Table(t) => t,
            _ => return Err("when: expected a table".into()),
        };
        let mut when = When::default();
        for pair in t.pairs::<Value, Value>() {
            let (k, v) = pair.map_err(|e| e.to_string())?;
            let Value::String(k) = k else {
                return Err("when: expected named keys".into());
            };
            let k = k.to_string_lossy();
            let string = |v: &Value| match v {
                Value::String(s) => Ok(s.to_string_lossy()),
                _ => Err(format!("when.{k}: expected a string")),
            };
            let boolean = |v: &Value| match v {
                Value::Boolean(b) => Ok(*b),
                _ => Err(format!("when.{k}: expected true or false")),
            };
            let kind = |v: &Value| {
                let s = string(v)?;
                SubjectKind::parse(&s).ok_or_else(|| {
                    format!("when.kind: {s:?} is not 'file', 'app', 'window' or 'item'")
                })
            };
            match k.as_str() {
                "kind" => {
                    when.kinds = match &v {
                        Value::Table(list) => (list.sequence_values::<Value>())
                            .map(|v| kind(&v.map_err(|e| e.to_string())?))
                            .collect::<Result<_, _>>()?,
                        v => vec![kind(v)?],
                    }
                }
                "mime" => when.mime = Some(string(&v)?),
                "path" => when.path = Some(string(&v)?),
                "dir" => when.dir = Some(boolean(&v)?),
                "app" => when.app = Some(string(&v)?),
                "running" => when.running = Some(boolean(&v)?),
                "source" => when.source = Some(string(&v)?),
                _ => return Err(format!("when.{k}: unknown key")),
            }
        }
        Ok(when)
    }
}

/// Whether `text` matches `pattern`: `*` is any run of characters, `?`
/// any one.
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    // The last `*`, and where in the text it took over.
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(&c) if c == '?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                // The `*` takes one more character.
                Some((sp, st)) => {
                    star = Some((sp, st + 1));
                    pi = sp + 1;
                    ti = st + 1;
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// The most choices an answer keeps.
pub const MAX_CHOICES: usize = 500;

/// One of an action's choices, as `choices(s)` answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub id: String,
    pub name: String,
    /// A theme icon name or a path.
    pub icon: Option<String>,
    /// The whole table (without its functions), handed back to
    /// `run(s, choice)`.
    pub data: serde_json::Value,
}

impl Choice {
    /// Read the choice `t`.
    pub(crate) fn read(lua: &Lua, t: &Table) -> Result<Self, String> {
        let text = |k: &str| -> Result<Option<String>, String> {
            match t.get::<Value>(k).map_err(|e| e.to_string())? {
                Value::Nil => Ok(None),
                Value::String(s) => Ok(Some(s.to_string_lossy())),
                Value::Integer(i) => Ok(Some(i.to_string())),
                Value::Number(n) => Ok(Some(n.to_string())),
                _ => Err(format!("{k}: expected a string")),
            }
        };
        let id = text("id")?;
        let name = text("name")?;
        let (id, name) = match (id, name) {
            (Some(id), Some(name)) => (id, name),
            (Some(id), None) => (id.clone(), id),
            (None, Some(name)) => (name.clone(), name),
            (None, None) => return Err("a choice needs an id or a name".into()),
        };
        let options = mlua::serde::DeserializeOptions::new().deny_unsupported_types(false);
        let data = (lua.from_value_with(Value::Table(t.clone()), options))
            .unwrap_or_else(|_| serde_json::json!({ "id": id, "name": name }));
        Ok(Choice {
            icon: text("icon")?,
            id,
            name,
            data,
        })
    }

    /// The table `run(s, choice)` gets: what `choices` gave.
    pub(crate) fn to_lua(&self, lua: &Lua) -> mlua::Result<Value> {
        match crate::host::to_lua(lua, &self.data)? {
            Value::Table(t) => Ok(Value::Table(t)),
            _ => {
                let t = lua.create_table()?;
                t.set("id", self.id.as_str())?;
                t.set("name", self.name.as_str())?;
                t.set("icon", self.icon.as_deref())?;
                Ok(Value::Table(t))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("image/*", "image/png"));
        assert!(!glob("image/*", "text/plain"));
        assert!(glob("*", ""));
        assert!(glob("a?c", "abc") && !glob("a?c", "ac"));
        assert!(glob("*.tar.*", "x.tar.gz"));
        assert!(glob("/home/*/notes/*.md", "/home/i/notes/a/b.md"));
        assert!(!glob("*.md", "a.mdx"));
        assert!(glob("firefox*", "firefox-esr"));
        assert!(glob("**a", "bba"));
    }

    fn when(code: &str) -> Result<When, String> {
        let lua = Lua::new();
        let v: Value = lua.load(code).eval().map_err(|e| e.to_string())?;
        When::read(&v)
    }

    fn file(path: &str, dir: bool, mime: &[&str]) -> Subject {
        Subject::File {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            dir,
            mime: mime.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn app(id: &str, running: bool) -> Subject {
        Subject::App {
            id: id.into(),
            name: id.into(),
            running,
            pinned: false,
            windows: Vec::new(),
        }
    }

    #[test]
    fn filters() {
        let png = file(
            "/p/a.png",
            false,
            &["image/png", "application/octet-stream"],
        );
        let svg = file("/p/b.svg", false, &["image/svg+xml", "text/xml"]);
        let dir = file("/p", true, &["inode/directory"]);
        let foot = app("foot", true);
        let idle = app("org.gnome.Calendar", false);
        let win = Subject::Window {
            id: "w".into(),
            title: "t".into(),
            app_id: "foot".into(),
            app_name: "Foot".into(),
        };
        let all = [&png, &svg, &dir, &foot, &idle, &win];
        let hits = |w: &str| {
            let w = when(w).unwrap();
            all.iter().map(|s| w.matches(s)).collect::<Vec<_>>()
        };
        assert_eq!(hits("return nil"), [true; 6]);
        assert_eq!(
            hits("return { kind = 'file', mime = 'image/*' }"),
            [true, true, false, false, false, false]
        );
        // Parents count; case doesn't.
        assert_eq!(
            hits("return { mime = 'TEXT/xml' }"),
            [false, true, false, false, false, false]
        );
        assert_eq!(
            hits("return { dir = true }"),
            [false, false, true, false, false, false]
        );
        assert_eq!(
            hits("return { path = '/p/*.png' }"),
            [true, false, false, false, false, false]
        );
        assert_eq!(
            hits("return { kind = 'app', running = true }"),
            [false, false, false, true, false, false]
        );
        // A window's app is its app ID; a window runs.
        assert_eq!(
            hits("return { app = 'FOOT' }"),
            [false, false, false, true, false, true]
        );
        assert_eq!(
            hits("return { running = false }"),
            [false, false, false, false, true, false]
        );
        assert_eq!(
            hits("return { kind = { 'app', 'window' } }"),
            [false, false, false, true, true, true]
        );
        let item = Subject::Item {
            source: "ssh".into(),
            id: "h".into(),
            title: "h".into(),
            data: serde_json::Value::Null,
        };
        assert!(when("return { source = 'ssh' }").unwrap().matches(&item));
        assert!(!when("return { source = 'ssh' }").unwrap().matches(&foot));
    }

    #[test]
    fn bad_filters() {
        assert!(
            when("return { kind = 'folder' }")
                .unwrap_err()
                .contains("kind")
        );
        assert!(
            when("return { mine = 'x' }")
                .unwrap_err()
                .contains("unknown")
        );
        assert!(
            when("return { running = 'yes' }")
                .unwrap_err()
                .contains("running")
        );
        assert!(when("return 'file'").is_err());
    }

    #[test]
    fn tilde_paths() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let path = std::path::Path::new(&home).join("notes/a.md");
        let s = file(&path.to_string_lossy(), false, &[]);
        assert!(when("return { path = '~/notes/*' }").unwrap().matches(&s));
    }

    #[test]
    fn subjects_and_choices_in_lua() {
        let lua = Lua::new();
        let s = Subject::App {
            id: "foot".into(),
            name: "Foot".into(),
            running: true,
            pinned: false,
            windows: vec![SubjectWindow {
                id: "w1".into(),
                title: "~".into(),
            }],
        };
        lua.globals().set("s", s.to_lua(&lua).unwrap()).unwrap();
        let ok: bool = lua
            .load(
                "return s.kind == 'app' and s.running and not s.pinned and s.windows[1].id == 'w1'",
            )
            .eval()
            .unwrap();
        assert!(ok);
        let t: Table = lua
            .load("return { id = 50, name = '50%', size = 0.5, f = function() end }")
            .eval()
            .unwrap();
        let c = Choice::read(&lua, &t).unwrap();
        assert_eq!((c.id.as_str(), c.name.as_str()), ("50", "50%"));
        // Its own fields come back.
        lua.globals().set("c", c.to_lua(&lua).unwrap()).unwrap();
        let size: f64 = lua.load("return c.size").eval().unwrap();
        assert_eq!(size, 0.5);
        let t: Table = lua.load("return { name = 'x' }").eval().unwrap();
        assert_eq!(Choice::read(&lua, &t).unwrap().id, "x");
        let t: Table = lua.load("return { icon = 'x' }").eval().unwrap();
        assert!(Choice::read(&lua, &t).is_err());
    }
}
