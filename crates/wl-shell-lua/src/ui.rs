//! `dock.ui`: the constructors a plugin builds trees with, and turning
//! what a hook returns into a [`Node`] (wl-shell-render's UI tree).
//!
//! A constructor takes a table of props with the children (or a text's
//! string) at its positions, and marks it with its kind; nothing is
//! checked until the hook returns. Then the tree is read into plain data:
//! props are validated (an error names the node, `col[2].text: ...`),
//! `nil` children are skipped (`cond and ui.x{} or nil`), a canvas's
//! `draw(c)` is run to record its commands, and handlers (`on_click`,
//! `on_change`, `on_commit`) are kept in the state's registry by the
//! node's key (given, or its path), so only keys leave the thread.
//!
//! `dock.theme` holds the theme's color names (`dock.theme.accent ==
//! 'accent'`): colors are resolved when the tree is painted, so a theme
//! change needs no new tree.

use mlua::{AnyUserData, Function, Lua, Table, UserData, UserDataMethods, Value};
use wl_shell_render::ui::canvas::MAX_COMMANDS;
/// The tree and everything in it, from wl-shell-render.
pub use wl_shell_render::ui::*;

/// The constructors, by kind.
pub const KINDS: [&str; 17] = [
    "col", "row", "grid", "stack", "box", "scroll", "spacer", "divider", "text", "icon",
    "app_icon", "image", "canvas", "button", "slider", "toggle", "progress",
];

/// The most nodes a tree may have, and how deep it may be.
const MAX_NODES: usize = 5000;
const MAX_DEPTH: usize = 64;
/// The longest text kept.
const MAX_TEXT: usize = 10_000;

/// Add `ns.ui` and `ns.theme`.
pub(crate) fn install(lua: &Lua, ns: &Table) -> mlua::Result<()> {
    let ui = lua.create_table()?;
    let kinds = lua.create_sequence_from(KINDS)?;
    lua.load(
        r#"
        local ui, kinds = ...
        local setmetatable, type = setmetatable, type
        for _, k in ipairs(kinds) do
          local mt = { __ui = k }
          ui[k] = function(t)
            if t == nil then t = {}
            elseif type(t) ~= 'table' then t = { t } end
            return setmetatable(t, mt)
          end
        end
        "#,
    )
    .set_name("=ui")
    .call::<()>((ui.clone(), kinds))?;
    ns.set("ui", ui)?;
    let theme = lua.create_table()?;
    for (name, _) in ThemeColor::ALL {
        theme.set(name, name)?;
    }
    ns.set("theme", theme)?;
    Ok(())
}

/// Reads a tree, keeping its handlers.
pub(crate) struct Builder<'a> {
    lua: &'a Lua,
    /// Handlers by node key: `{ on_click = fn, ... }`.
    handlers: Table,
    nodes: usize,
}

type R<T> = Result<T, String>;

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) if n.is_finite() => Some(*n),
        _ => None,
    }
}

/// Reading props off one node's table, with errors naming them.
struct Props<'t> {
    t: &'t Table,
    path: &'t str,
}

impl Props<'_> {
    fn err(&self, key: &str, what: &str) -> String {
        format!("{}.{key}: {what}", self.path)
    }

    fn get(&self, key: &str) -> R<Value> {
        self.t
            .raw_get(key)
            .map_err(|e| self.err(key, &e.to_string()))
    }

    fn num(&self, key: &str) -> R<Option<f64>> {
        match self.get(key)? {
            Value::Nil => Ok(None),
            v => num(&v)
                .map(Some)
                .ok_or_else(|| self.err(key, &format!("expected a number, got {}", v.type_name()))),
        }
    }

    fn f32(&self, key: &str) -> R<Option<f32>> {
        Ok(self.num(key)?.map(|v| v as f32))
    }

    fn bool(&self, key: &str) -> R<Option<bool>> {
        match self.get(key)? {
            Value::Nil => Ok(None),
            Value::Boolean(b) => Ok(Some(b)),
            v => Err(self.err(key, &format!("expected a boolean, got {}", v.type_name()))),
        }
    }

    fn string(&self, key: &str) -> R<Option<String>> {
        string(&self.get(key)?).map_err(|e| self.err(key, &e))
    }

    fn color(&self, key: &str) -> R<Option<Color>> {
        match self.string(key)? {
            None => Ok(None),
            Some(s) => Color::parse(&s).map(Some).map_err(|e| self.err(key, &e)),
        }
    }

    /// One of `names`, by name.
    fn pick<T: Copy>(&self, key: &str, names: &[(&str, T)]) -> R<Option<T>> {
        match self.string(key)? {
            None => Ok(None),
            Some(s) => (names.iter())
                .find(|(n, _)| *n == s)
                .map(|(_, v)| Some(*v))
                .ok_or_else(|| {
                    let all: Vec<&str> = names.iter().map(|(n, _)| *n).collect();
                    self.err(key, &format!("{s:?} is not one of {}", all.join(", ")))
                }),
        }
    }

    /// A number (pixels) or a string like `"50%"`.
    fn length(&self, key: &str) -> R<Option<Length>> {
        match self.get(key)? {
            Value::Nil => Ok(None),
            Value::String(s) => {
                let s = s.to_string_lossy();
                (s.trim().strip_suffix('%'))
                    .and_then(|p| p.trim().parse::<f32>().ok())
                    .filter(|p| p.is_finite())
                    .map(|p| Some(Length::Percent(p)))
                    .ok_or_else(|| self.err(key, &format!("{s:?} is not a size")))
            }
            v => num(&v)
                .map(|v| Some(Length::Px(v.max(0.0) as f32)))
                .ok_or_else(|| self.err(key, "expected a number or a percentage")),
        }
    }

    /// `8`, `{ 4, 8 }` (vertical, horizontal), `{ 1, 2, 3, 4 }` (top,
    /// right, bottom, left), or `{ top = 1, ... }`.
    fn edges(&self, key: &str) -> R<Option<Edges>> {
        match self.get(key)? {
            Value::Nil => Ok(None),
            Value::Table(t) => {
                let at = |v: mlua::Result<Value>| -> R<f32> {
                    match v.map_err(|e| e.to_string())? {
                        Value::Nil => Ok(0.0),
                        v => num(&v)
                            .map(|v| v as f32)
                            .ok_or_else(|| self.err(key, "expected numbers")),
                    }
                };
                let i = |n: i64| at(t.raw_get::<Value>(n));
                let s = |n: &str| at(t.raw_get::<Value>(n));
                Ok(Some(match t.raw_len() {
                    0 => Edges {
                        top: s("top")?,
                        right: s("right")?,
                        bottom: s("bottom")?,
                        left: s("left")?,
                    },
                    1 => Edges::all(i(1)?),
                    2 | 3 => Edges::sym(i(1)?, i(2)?),
                    _ => Edges {
                        top: i(1)?,
                        right: i(2)?,
                        bottom: i(3)?,
                        left: i(4)?,
                    },
                }))
            }
            v => num(&v)
                .map(|v| Some(Edges::all(v as f32)))
                .ok_or_else(|| self.err(key, "expected a number or a table")),
        }
    }
}

fn string(v: &Value) -> Result<Option<String>, String> {
    match v {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_string_lossy())),
        Value::Integer(i) => Ok(Some(i.to_string())),
        Value::Number(n) => Ok(Some(n.to_string())),
        v => Err(format!("expected a string, got {}", v.type_name())),
    }
}

const ALIGNS: [(&str, Align); 4] = [
    ("start", Align::Start),
    ("center", Align::Center),
    ("end", Align::End),
    ("stretch", Align::Stretch),
];

const JUSTIFIES: [(&str, Justify); 6] = [
    ("start", Justify::Start),
    ("center", Justify::Center),
    ("end", Justify::End),
    ("between", Justify::Between),
    ("around", Justify::Around),
    ("evenly", Justify::Evenly),
];

const PLACES: [(&str, (Align, Align)); 10] = [
    ("center", (Align::Center, Align::Center)),
    ("stretch", (Align::Stretch, Align::Stretch)),
    ("top", (Align::Center, Align::Start)),
    ("bottom", (Align::Center, Align::End)),
    ("left", (Align::Start, Align::Center)),
    ("right", (Align::End, Align::Center)),
    ("top-left", (Align::Start, Align::Start)),
    ("top-right", (Align::End, Align::Start)),
    ("bottom-left", (Align::Start, Align::End)),
    ("bottom-right", (Align::End, Align::End)),
];

const TEXT_ALIGNS: [(&str, TextAlign); 5] = [
    ("start", TextAlign::Start),
    ("left", TextAlign::Start),
    ("center", TextAlign::Center),
    ("end", TextAlign::End),
    ("right", TextAlign::End),
];

const CAPS: [(&str, Cap); 3] = [
    ("butt", Cap::Butt),
    ("round", Cap::Round),
    ("square", Cap::Square),
];

/// The look props of `p`.
fn look(p: &Props) -> R<Look> {
    let border = match p.get("border")? {
        Value::Nil => None,
        Value::Table(t) => {
            let b = Props {
                t: &t,
                path: p.path,
            };
            Some(Border {
                width: b.f32("width")?.unwrap_or(1.0),
                color: b
                    .color("color")?
                    .unwrap_or(Color::Theme(ThemeColor::Border)),
            })
        }
        v => match num(&v) {
            Some(w) => Some(Border {
                width: w as f32,
                color: Color::Theme(ThemeColor::Border),
            }),
            None => Some(Border {
                width: 1.0,
                color: p
                    .color("border")?
                    .unwrap_or(Color::Theme(ThemeColor::Border)),
            }),
        },
    };
    Ok(Look {
        background: p.color("background")?,
        radius: p.f32("radius")?,
        border,
        opacity: p.f32("opacity")?.map(|o| o.clamp(0.0, 1.0)),
    })
}

/// `live = { rate = 1, at = t }`.
fn live(p: &Props) -> R<Option<Live>> {
    match p.get("live")? {
        Value::Nil | Value::Boolean(false) => Ok(None),
        Value::Table(t) => {
            let l = Props {
                t: &t,
                path: p.path,
            };
            Ok(Some(Live {
                rate: l.num("rate")?.unwrap_or(1.0),
                at: l.num("at")?.unwrap_or(0.0),
            }))
        }
        _ => Err(p.err("live", "expected a table with `rate` and `at`")),
    }
}

impl<'a> Builder<'a> {
    pub fn new(lua: &'a Lua) -> mlua::Result<Self> {
        Ok(Builder {
            lua,
            handlers: lua.create_table()?,
            nodes: 0,
        })
    }

    /// The handlers kept while building.
    pub fn handlers(self) -> Table {
        self.handlers
    }

    /// The tree `v` (None for `nil`).
    pub fn root(&mut self, v: Value) -> R<Option<Node>> {
        match v {
            Value::Nil | Value::Boolean(false) => Ok(None),
            v => self.node(v, "root", "1", 0).map(Some),
        }
    }

    fn node(&mut self, v: Value, name: &str, at: &str, depth: usize) -> R<Node> {
        let Value::Table(t) = v else {
            return Err(format!(
                "{name}: expected a node (from dock.ui), got {}",
                v.type_name()
            ));
        };
        let kind: Option<String> = t
            .metatable()
            .and_then(|m| m.raw_get::<Option<String>>("__ui").ok().flatten());
        let Some(kind) = kind else {
            return Err(format!(
                "{name}: expected a node (from dock.ui), got a table"
            ));
        };
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(format!("the tree has more than {MAX_NODES} nodes"));
        }
        if depth > MAX_DEPTH {
            return Err(format!("the tree is deeper than {MAX_DEPTH}"));
        }
        let path = match name {
            "root" => kind.clone(),
            _ => format!("{name}.{kind}"),
        };
        let p = Props { t: &t, path: &path };
        // Leaves take their string at [1]; the others their children.
        let positional_text = matches!(kind.as_str(), "text" | "icon");
        let mut node = Node::new(self.kind(&kind, &p)?);
        let l = &mut node.layout;
        l.width = p.length("width")?;
        l.height = p.length("height")?;
        l.min_width = p.length("min_width")?;
        l.min_height = p.length("min_height")?;
        l.max_width = p.length("max_width")?;
        l.max_height = p.length("max_height")?;
        l.grow = match p.get("grow")? {
            Value::Boolean(true) => 1.0,
            _ => p.f32("grow").unwrap_or(None).unwrap_or(0.0).max(0.0),
        };
        l.gap = p.f32("gap")?;
        l.row_gap = p.f32("row_gap")?;
        l.padding = p.edges("padding")?;
        if kind != "stack" {
            l.align = p.pick("align", &ALIGNS)?;
        }
        l.justify = p.pick("justify", &JUSTIFIES)?;
        node.look = look(&p)?;
        node.hover = match p.get("hover")? {
            Value::Nil => None,
            Value::Table(h) => Some(look(&Props {
                t: &h,
                path: &format!("{path}.hover"),
            })?),
            _ => return Err(p.err("hover", "expected a table of props")),
        };
        node.key = match p.get("key")? {
            Value::Nil => None,
            v => Some(
                string(&v)
                    .map_err(|e| p.err("key", &e))?
                    .unwrap_or_default(),
            ),
        };
        // Handlers, kept by key.
        let mut found = Vec::new();
        for (field, flag) in [("on_click", 0usize), ("on_change", 1), ("on_commit", 2)] {
            match p.get(field)? {
                Value::Nil => {}
                Value::Function(f) => found.push((field, flag, f)),
                v => {
                    return Err(p.err(
                        field,
                        &format!("expected a function, got {}", v.type_name()),
                    ));
                }
            }
        }
        if node.key.is_none() && (node.kind.interactive() || !found.is_empty()) {
            node.key = Some(format!("#{at}"));
        }
        if !found.is_empty() {
            let key = node.key.clone().unwrap_or_default();
            let entry = self.lua.create_table().map_err(|e| e.to_string())?;
            for (field, flag, f) in found {
                entry.raw_set(field, f).map_err(|e| e.to_string())?;
                match flag {
                    0 => node.handlers.click = true,
                    1 => node.handlers.change = true,
                    _ => node.handlers.commit = true,
                }
            }
            (self.handlers.raw_set(key, entry)).map_err(|e| e.to_string())?;
        }
        // Children: the values at 1, 2, ... (nil and false skipped).
        let mut children = Vec::new();
        if !positional_text {
            let last = (t.pairs::<Value, Value>())
                .filter_map(|kv| match kv {
                    Ok((Value::Integer(i), _)) if i >= 1 => Some(i),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            for i in 1..=last {
                let v: Value = t.raw_get(i).map_err(|e| e.to_string())?;
                if matches!(v, Value::Nil | Value::Boolean(false)) {
                    continue;
                }
                let name = format!("{path}[{i}]");
                let at = format!("{at}.{i}");
                children.push(self.node(v, &name, &at, depth + 1)?);
            }
        }
        // A button's `icon` and `label` come first.
        if kind == "button" {
            let mut own = Vec::new();
            if let Some(name) = p.string("icon")? {
                own.push(Node::new(Kind::Icon {
                    name,
                    size: None,
                    color: None,
                }));
            }
            if let Some(label) = p.string("label")? {
                own.push(Node::text(label));
            }
            own.extend(children);
            children = own;
        }
        node.children = children;
        Ok(node)
    }

    fn kind(&mut self, kind: &str, p: &Props) -> R<Kind> {
        Ok(match kind {
            "col" => Kind::Col,
            "row" => Kind::Row,
            "grid" => Kind::Grid {
                columns: self.columns(p)?,
            },
            "stack" => {
                let (x, y) = p
                    .pick("align", &PLACES)?
                    .unwrap_or((Align::Center, Align::Center));
                Kind::Stack {
                    place: Place { x, y },
                }
            }
            "box" => Kind::Box {
                variant: p.pick(
                    "variant",
                    &[
                        ("card", BoxVariant::Card),
                        ("inset", BoxVariant::Inset),
                        ("badge", BoxVariant::Badge),
                    ],
                )?,
            },
            "scroll" => Kind::Scroll,
            "spacer" => Kind::Spacer {
                size: p.f32("size")?,
            },
            "divider" => Kind::Divider,
            "text" => {
                let text = match p.get("text")? {
                    Value::Nil => string(&p.t.raw_get::<Value>(1).map_err(|e| e.to_string())?),
                    v => string(&v),
                }
                .map_err(|e| p.err("text", &e))?
                .unwrap_or_default();
                Kind::Text {
                    text: text.chars().take(MAX_TEXT).collect(),
                    style: p
                        .pick(
                            "style",
                            &[
                                ("title", TextStyle::Title),
                                ("body", TextStyle::Body),
                                ("muted", TextStyle::Muted),
                                ("caption", TextStyle::Caption),
                            ],
                        )?
                        .unwrap_or_default(),
                    weight: match p.get("weight")? {
                        Value::Nil => None,
                        Value::String(s) => match &*s.to_string_lossy() {
                            "bold" => Some(700),
                            "normal" | "regular" => Some(400),
                            "light" => Some(300),
                            "medium" => Some(500),
                            s => return Err(p.err("weight", &format!("{s:?} is not a weight"))),
                        },
                        v => Some(
                            num(&v)
                                .ok_or_else(|| p.err("weight", "expected a name or a number"))?
                                .clamp(100.0, 900.0) as u16,
                        ),
                    },
                    color: p.color("color")?,
                    size: p.f32("size")?.filter(|s| *s > 0.0),
                    align: p.pick("align", &TEXT_ALIGNS)?.unwrap_or_default(),
                    max_lines: p.num("max_lines")?.map(|n| n.max(0.0) as u32),
                }
            }
            "icon" => {
                let name = match p.get("name")? {
                    Value::Nil => string(&p.t.raw_get::<Value>(1).map_err(|e| e.to_string())?),
                    v => string(&v),
                }
                .map_err(|e| p.err("name", &e))?
                .unwrap_or_default();
                Kind::Icon {
                    name,
                    size: p.f32("size")?,
                    color: p.color("color")?,
                }
            }
            "app_icon" => Kind::AppIcon {
                app: p.string("app")?,
                size: p.f32("size")?,
            },
            "image" => Kind::Image {
                src: p.string("src")?.unwrap_or_default(),
                fit: p
                    .pick(
                        "fit",
                        &[
                            ("cover", Fit::Cover),
                            ("contain", Fit::Contain),
                            ("fill", Fit::Fill),
                        ],
                    )?
                    .unwrap_or_default(),
                radius: p.f32("radius")?.unwrap_or(0.0).max(0.0),
                blur: p.f32("blur")?.unwrap_or(0.0).clamp(0.0, 100.0),
                opacity: p.f32("opacity")?.unwrap_or(1.0).clamp(0.0, 1.0),
            },
            "canvas" => self.canvas(p)?,
            "button" => Kind::Button {
                variant: p
                    .pick(
                        "variant",
                        &[
                            ("plain", ButtonVariant::Plain),
                            ("filled", ButtonVariant::Filled),
                            ("ghost", ButtonVariant::Ghost),
                        ],
                    )?
                    .unwrap_or_default(),
                enabled: p.bool("enabled")?.unwrap_or(true),
                selected: p.bool("selected")?.unwrap_or(false),
                tooltip: p.string("tooltip")?,
            },
            "slider" => Kind::Slider {
                value: p.num("value")?.unwrap_or(0.0),
                min: p.num("min")?.unwrap_or(0.0),
                max: p.num("max")?.unwrap_or(1.0),
                enabled: p.bool("enabled")?.unwrap_or(true),
                live: live(p)?,
            },
            "toggle" => Kind::Toggle {
                value: p.bool("value")?.unwrap_or(false),
                enabled: p.bool("enabled")?.unwrap_or(true),
            },
            "progress" => Kind::Progress {
                value: p.num("value")?.unwrap_or(0.0),
                max: p.num("max")?.unwrap_or(1.0),
                live: live(p)?,
            },
            k => return Err(format!("{}: no such node {k:?}", p.path)),
        })
    }

    /// `columns = 7`, or a list of sizes: numbers (pixels), `"1fr"`,
    /// `"auto"`.
    fn columns(&self, p: &Props) -> R<Vec<Track>> {
        match p.get("columns")? {
            Value::Nil => Ok(vec![Track::Fr(1.0)]),
            Value::Table(t) => {
                let mut out = Vec::new();
                for v in t.sequence_values::<Value>() {
                    let v = v.map_err(|e| e.to_string())?;
                    out.push(match &v {
                        Value::String(s) => {
                            let s = s.to_string_lossy();
                            let s = s.trim();
                            match s {
                                "auto" => Track::Auto,
                                s => (s.strip_suffix("fr"))
                                    .and_then(|f| f.trim().parse::<f32>().ok())
                                    .filter(|f| f.is_finite() && *f > 0.0)
                                    .map(Track::Fr)
                                    .ok_or_else(|| {
                                        p.err("columns", &format!("{s:?} is not a size"))
                                    })?,
                            }
                        }
                        v => Track::Px(
                            num(v)
                                .ok_or_else(|| p.err("columns", "expected sizes"))?
                                .max(0.0) as f32,
                        ),
                    });
                }
                match out.is_empty() {
                    true => Err(p.err("columns", "no columns")),
                    false => Ok(out),
                }
            }
            v => match num(&v) {
                Some(n) if (1.0..=64.0).contains(&n) => Ok(vec![Track::Fr(1.0); n as usize]),
                _ => Err(p.err("columns", "expected 1 to 64, or a list of sizes")),
            },
        }
    }

    /// A canvas: its `draw(c)` runs now, recording.
    fn canvas(&self, p: &Props) -> R<Kind> {
        let viewbox = match p.get("viewbox")? {
            Value::Nil => (100.0, 100.0),
            Value::Table(t) => {
                let w = num(&t.raw_get(1).map_err(|e| e.to_string())?);
                let h = num(&t.raw_get(2).map_err(|e| e.to_string())?);
                match (w, h) {
                    (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (w as f32, h as f32),
                    _ => return Err(p.err("viewbox", "expected { width, height }")),
                }
            }
            _ => return Err(p.err("viewbox", "expected { width, height }")),
        };
        let commands = match p.get("draw")? {
            Value::Nil => Vec::new(),
            Value::Function(f) => record(self.lua, &f).map_err(|e| p.err("draw", &e))?,
            _ => return Err(p.err("draw", "expected a function")),
        };
        Ok(Kind::Canvas { viewbox, commands })
    }
}

/// Run `draw(c)` with a recorder, and take its commands.
fn record(lua: &Lua, draw: &Function) -> R<Vec<Cmd>> {
    let c: AnyUserData = lua
        .create_userdata(Recorder::default())
        .map_err(|e| e.to_string())?;
    draw.call::<()>(c.clone()).map_err(|e| e.to_string())?;
    let r: Recorder = c.take().map_err(|e| e.to_string())?;
    match r.error {
        Some(e) => Err(e),
        None => Ok(r.cmds),
    }
}

/// The `c` a canvas's `draw` gets.
#[derive(Default)]
struct Recorder {
    cmds: Vec<Cmd>,
    error: Option<String>,
}

impl Recorder {
    fn push(&mut self, cmd: Cmd) -> mlua::Result<()> {
        if self.cmds.len() >= MAX_COMMANDS {
            return Err(mlua::Error::runtime(format!(
                "a canvas draws at most {MAX_COMMANDS} commands"
            )));
        }
        self.cmds.push(cmd);
        Ok(())
    }
}

/// A canvas method's props, as numbers and an ink.
fn shape(t: &Table, what: &str) -> mlua::Result<(impl Fn(&str) -> mlua::Result<f32>, Ink)> {
    let p = Props { t, path: what };
    let e = mlua::Error::runtime;
    let ink = Ink {
        fill: p.color("fill").map_err(e)?,
        stroke: p.color("stroke").map_err(e)?,
        width: p.f32("width").map_err(e)?.unwrap_or(1.0),
        cap: p.pick("cap", &CAPS).map_err(e)?.unwrap_or_default(),
    };
    let t = t.clone();
    let what = what.to_string();
    let get = move |k: &str| -> mlua::Result<f32> {
        let p = Props { t: &t, path: &what };
        Ok(p.f32(k).map_err(e)?.unwrap_or(0.0))
    };
    Ok((get, ink))
}

impl UserData for Recorder {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method_mut("save", |_, r, ()| r.push(Cmd::Save));
        m.add_method_mut("restore", |_, r, ()| r.push(Cmd::Restore));
        m.add_method_mut("translate", |_, r, (x, y): (f32, f32)| {
            r.push(Cmd::Translate(x, y))
        });
        m.add_method_mut("rotate", |_, r, a: f32| r.push(Cmd::Rotate(a)));
        m.add_method_mut("scale", |_, r, (x, y): (f32, Option<f32>)| {
            r.push(Cmd::Scale(x, y.unwrap_or(x)))
        });
        m.add_method_mut("line", |_, r, t: Table| {
            let (n, ink) = shape(&t, "line")?;
            r.push(Cmd::Line {
                from: (n("x1")?, n("y1")?),
                to: (n("x2")?, n("y2")?),
                ink,
            })
        });
        m.add_method_mut("rect", |_, r, t: Table| {
            let (n, ink) = shape(&t, "rect")?;
            r.push(Cmd::Rect {
                x: n("x")?,
                y: n("y")?,
                w: n("w")?,
                h: n("h")?,
                radius: n("radius")?,
                ink,
            })
        });
        m.add_method_mut("circle", |_, r, t: Table| {
            let (n, ink) = shape(&t, "circle")?;
            r.push(Cmd::Circle {
                x: n("x")?,
                y: n("y")?,
                r: n("r")?,
                ink,
            })
        });
        m.add_method_mut("arc", |_, r, t: Table| {
            let (n, ink) = shape(&t, "arc")?;
            let pie = matches!(t.raw_get::<Value>("pie")?, Value::Boolean(true));
            r.push(Cmd::Arc {
                x: n("x")?,
                y: n("y")?,
                r: n("r")?,
                start: n("from")?,
                end: n("to")?,
                pie,
                ink,
            })
        });
        m.add_method_mut("path", |_, r, t: Table| {
            let (_, ink) = shape(&t, "path")?;
            let d: Value = match t.raw_get::<Value>("d")? {
                Value::Nil => t.raw_get(1)?,
                v => v,
            };
            let d = string(&d)
                .map_err(mlua::Error::runtime)?
                .unwrap_or_default();
            let segs = canvas::parse_path(&d).map_err(mlua::Error::runtime)?;
            r.push(Cmd::Path { segs, ink })
        });
        m.add_method_mut("text", |_, r, t: Table| {
            let p = Props {
                t: &t,
                path: "text",
            };
            let e = mlua::Error::runtime;
            let text = match t.raw_get::<Value>("text")? {
                Value::Nil => t.raw_get(1)?,
                v => v,
            };
            r.push(Cmd::Text {
                x: p.f32("x").map_err(e)?.unwrap_or(0.0),
                y: p.f32("y").map_err(e)?.unwrap_or(0.0),
                text: string(&text).map_err(e)?.unwrap_or_default(),
                size: p.f32("size").map_err(e)?.unwrap_or(12.0),
                color: p.color("color").map_err(e)?,
                bold: p.bool("bold").map_err(e)?.unwrap_or(false),
                align: p
                    .pick("align", &TEXT_ALIGNS)
                    .map_err(e)?
                    .unwrap_or_default(),
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(code: &str) -> (Lua, R<Option<Node>>, Table) {
        let lua = Lua::new();
        let ns = lua.create_table().unwrap();
        install(&lua, &ns).unwrap();
        lua.globals().set("dock", ns).unwrap();
        let v: Value = lua.load(code).eval().unwrap();
        let mut b = Builder::new(&lua).unwrap();
        let tree = b.root(v);
        let handlers = b.handlers();
        (lua, tree, handlers)
    }

    #[test]
    fn a_tree_with_handlers() {
        let (_lua, tree, handlers) = build(
            "local ui = dock.ui
             return ui.col{ gap = 4, padding = {2, 6}, align = 'center',
               ui.text{'Hi', style = 'title', max_lines = 1},
               false,
               ui.row{ justify = 'between',
                 ui.button{icon = 'go-next', label = 'Next', on_click = function() end},
                 ui.slider{key = 'seek', value = 3, max = 10, on_commit = function(v) end},
               },
               ui.box{variant = 'badge', hover = {background = dock.theme.hover}, ui.text{7}},
             }",
        );
        let tree = tree.unwrap().unwrap();
        assert_eq!(tree.kind, Kind::Col);
        assert_eq!(tree.layout.gap, Some(4.0));
        assert_eq!(tree.layout.padding, Some(Edges::sym(2.0, 6.0)));
        // `false` is skipped.
        assert_eq!(tree.children.len(), 3);
        let row = &tree.children[1];
        assert_eq!(row.layout.justify, Some(Justify::Between));
        let button = &row.children[0];
        // Its icon and label became children; its key is its path.
        let kinds: Vec<&str> = button.children.iter().map(|c| c.kind.name()).collect();
        assert_eq!(kinds, ["icon", "text"]);
        assert_eq!(button.key.as_deref(), Some("#1.3.1"));
        assert!(button.handlers.click);
        let slider = &row.children[1];
        assert_eq!(slider.key.as_deref(), Some("seek"));
        assert!(slider.handlers.commit && !slider.handlers.click);
        assert!(handlers.raw_get::<Table>("seek").is_ok());
        assert!(handlers.raw_get::<Table>("#1.3.1").is_ok());
        let badge = &tree.children[2];
        assert_eq!(
            badge.hover.unwrap().background,
            Some(Color::Theme(ThemeColor::Hover))
        );
        assert!(matches!(&badge.children[0].kind, Kind::Text { text, .. } if text == "7"));
    }

    #[test]
    fn canvases_record() {
        let (_lua, tree, _) = build(
            "return dock.ui.canvas{ viewbox = {100, 50}, draw = function(c)
               c:save(); c:translate(50, 25); c:rotate(1)
               c:line{x1 = 0, y1 = 0, x2 = 0, y2 = -20, stroke = 'accent', width = 2, cap = 'round'}
               c:restore()
               c:circle{x = 50, y = 25, r = 3, fill = '#ff0000'}
               c:arc{x = 1, y = 2, r = 3, from = 0, to = 1, pie = true, fill = 'text'}
               c:path{d = 'M 0 0 L 10 10 Z', stroke = 'muted'}
               c:text{'12', x = 50, y = 10, size = 8, align = 'center'}
               c:scale(2)
             end }",
        );
        let tree = tree.unwrap().unwrap();
        let Kind::Canvas { viewbox, commands } = tree.kind else {
            panic!()
        };
        assert_eq!(viewbox, (100.0, 50.0));
        assert_eq!(commands.len(), 10);
        assert_eq!(commands[2], Cmd::Rotate(1.0));
        assert!(matches!(
            commands[3],
            Cmd::Line {
                ink: Ink {
                    cap: Cap::Round,
                    width: 2.0,
                    ..
                },
                ..
            }
        ));
        assert_eq!(commands[9], Cmd::Scale(2.0, 2.0));
    }

    #[test]
    fn errors_name_the_node() {
        let err = |code: &str| build(code).1.unwrap_err();
        assert!(err("return {}").contains("expected a node"));
        assert!(
            err("return dock.ui.col{ dock.ui.text{'a', color = 'nope'} }")
                .contains("col[1].text.color")
        );
        assert!(err("return dock.ui.row{ gap = 'x' }").contains("row.gap: expected a number"));
        assert!(err("return dock.ui.stack{ align = 'diagonal' }").contains("not one of"));
        assert!(
            err("return dock.ui.canvas{ draw = function(c) c:path{d = 'M 1'} end }")
                .contains("canvas.draw")
        );
        assert!(err("return dock.ui.grid{ columns = 0 }").contains("columns"));
        assert!(
            err("local t = dock.ui.col{} for i = 1, 6000 do t[i] = dock.ui.spacer{} end return t")
                .contains("nodes")
        );
        // nil: no tree.
        assert_eq!(build("return nil").1, Ok(None));
    }

    #[test]
    fn sizes_and_grids() {
        let (_lua, tree, _) = build(
            "return dock.ui.grid{ columns = {40, '2fr', 'auto'}, width = '50%', height = 20,
               dock.ui.image{src = '~/a.png', fit = 'contain', radius = 4},
               dock.ui.stack{ align = 'top-right' } }",
        );
        let tree = tree.unwrap().unwrap();
        assert_eq!(
            tree.kind,
            Kind::Grid {
                columns: vec![Track::Px(40.0), Track::Fr(2.0), Track::Auto]
            }
        );
        assert_eq!(tree.layout.width, Some(Length::Percent(50.0)));
        assert_eq!(tree.layout.height, Some(Length::Px(20.0)));
        assert!(matches!(
            tree.children[1].kind,
            Kind::Stack {
                place: Place {
                    x: Align::End,
                    y: Align::Start
                }
            }
        ));
    }
}
