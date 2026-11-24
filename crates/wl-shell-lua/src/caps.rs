//! What a plugin provides, read from the table its file returns, and the
//! host's registry of it.
//!
//! A plugin file returns one table. Widget hooks (`icon`, `label`,
//! `popup`, `on_open`, `on_close`, `on_click`) sit at its top level or
//! under `widget`, next to `span` and `popup_on`; `setup` at the top
//! level (or under `widget`).
//! `actions` and `sources` are lists of tables for the launcher. An
//! action has an `id`, a `name`, an `icon`, a `when` filter (see
//! `subject.rs`), a `run(s, choice)` function and, optionally,
//! `choices(s)`. A source has an `id`, a `name`, an `icon`, a `trigger`
//! (one character), either `items()` (a fixed list the host ranks) or
//! `search(query, emit)` (live results per query), and optionally
//! `run(item)`. `api`, if given, must be 1.

use std::collections::HashMap;

use mlua::{Table, Value};

use crate::channel::InstanceId;
use crate::subject::{Subject, When};

/// The API version plugins are written against.
pub const API: i64 = 1;

/// The widget hooks a plugin has.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WidgetCaps {
    pub icon: bool,
    pub label: bool,
    pub popup: bool,
    pub on_open: bool,
    pub on_close: bool,
    pub on_click: bool,
    /// The tile's length along the dock, per axis.
    pub span: Span,
    /// What opens the popup of a standalone widget (an app's always
    /// opens on hover, since a click is the app's).
    pub popup_on: PopupOn,
}

/// What opens a widget's popup: `popup_on = 'hover' | 'click'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PopupOn {
    /// Hovering the item for its popup delay; the label doesn't show.
    #[default]
    Hover,
    /// A left click; hovering shows the label.
    Click,
}

/// A tile's length along the dock, in icons: 1, 1.5 or 2.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub horizontal: f32,
    pub vertical: f32,
}

impl Default for Span {
    fn default() -> Self {
        Span {
            horizontal: 1.0,
            vertical: 1.0,
        }
    }
}

/// The steps a span snaps to.
pub const SPANS: [f32; 3] = [1.0, 1.5, 2.0];

/// `v` rounded down to a step (at least 1).
pub fn span_step(v: f64) -> f32 {
    (SPANS.iter().rev())
        .copied()
        .find(|&s| v >= s as f64)
        .unwrap_or(1.0)
}

/// A launcher action a plugin declares.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionDecl {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// Which subjects it applies to.
    pub when: When,
    /// It has `choices(s)`: choosing it is a level of choices, which
    /// `run(s, choice)` then gets one of.
    pub choices: bool,
}

/// A launcher source a plugin declares: a scope of its own, entered by
/// typing its trigger first.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceDecl {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    /// Typed first, it enters the source.
    pub trigger: char,
    /// It answers `search(query, emit)` per query; else `items()` gives
    /// a fixed list the host ranks.
    pub search: bool,
    /// It has `run(item)` for Enter.
    pub run: bool,
}

/// Everything a loaded plugin declared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Capabilities {
    /// Has a `setup` hook.
    pub setup: bool,
    /// None: no widget hooks.
    pub widget: Option<WidgetCaps>,
    pub actions: Vec<ActionDecl>,
    pub sources: Vec<SourceDecl>,
}

impl Capabilities {
    /// Whether the launcher has anything to take from it.
    pub fn launcher(&self) -> bool {
        !self.actions.is_empty() || !self.sources.is_empty()
    }

    /// Read what `plugin` (the table a plugin file returned) provides.
    pub fn read(plugin: &Table) -> Result<Self, String> {
        let e = |e: mlua::Error| e.to_string();
        match plugin.get::<Value>("api").map_err(e)? {
            Value::Nil => {}
            Value::Integer(API) => {}
            Value::Number(n) if n == API as f64 => {}
            v => {
                return Err(format!(
                    "api {} is not supported (expected {API})",
                    show(&v)
                ));
            }
        }
        let widget = widget_table(plugin).map_err(e)?;
        let is_fn = |t: &Table, k: &str| matches!(t.get::<Value>(k), Ok(Value::Function(_)));
        let setup = is_fn(plugin, "setup") || is_fn(&widget, "setup");
        let caps = WidgetCaps {
            icon: is_fn(&widget, "icon"),
            label: is_fn(&widget, "label"),
            popup: is_fn(&widget, "popup"),
            on_open: is_fn(&widget, "on_open"),
            on_close: is_fn(&widget, "on_close"),
            on_click: is_fn(&widget, "on_click"),
            span: span(&widget.get::<Value>("span").map_err(e)?)?,
            popup_on: popup_on(&widget.get::<Value>("popup_on").map_err(e)?)?,
        };
        let any = caps.icon || caps.label || caps.popup || caps.on_click;
        let actions = list(plugin, "actions", |t| {
            let id = required(t, "id")?;
            if !is_fn(t, "run") {
                return Err("run: expected a function".into());
            }
            let choices = match t.get::<Value>("choices").map_err(e)? {
                Value::Nil => false,
                Value::Function(_) => true,
                _ => return Err("choices: expected a function".into()),
            };
            Ok(ActionDecl {
                name: optional(t, "name")?.unwrap_or_else(|| id.clone()),
                icon: optional(t, "icon")?,
                when: When::read(&t.get::<Value>("when").map_err(e)?)?,
                choices,
                id,
            })
        })?;
        for (i, a) in actions.iter().enumerate() {
            if actions[..i].iter().any(|b| b.id == a.id) {
                return Err(format!("actions[{}]: the id {:?} is taken", i + 1, a.id));
            }
        }
        let sources = list(plugin, "sources", |t| {
            let id = required(t, "id")?;
            let f = |k: &str| match t.get::<Value>(k).map_err(e)? {
                Value::Nil => Ok(false),
                Value::Function(_) => Ok(true),
                _ => Err(format!("{k}: expected a function")),
            };
            let search = match (f("items")?, f("search")?) {
                (true, false) => false,
                (false, true) => true,
                (true, true) => return Err("give items or search, not both".into()),
                (false, false) => return Err("items or search is missing".into()),
            };
            Ok(SourceDecl {
                name: optional(t, "name")?.unwrap_or_else(|| id.clone()),
                icon: optional(t, "icon")?,
                trigger: trigger(&required(t, "trigger")?)?,
                search,
                run: f("run")?,
                id,
            })
        })?;
        for (i, s) in sources.iter().enumerate() {
            if sources[..i].iter().any(|o| o.id == s.id) {
                return Err(format!("sources[{}]: the id {:?} is taken", i + 1, s.id));
            }
        }
        Ok(Capabilities {
            setup,
            widget: any.then_some(caps),
            actions,
            sources,
        })
    }
}

/// A source's `trigger`: one character, not a space.
fn trigger(s: &str) -> Result<char, String> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_whitespace() && !c.is_control() => Ok(c),
        _ => Err(format!("trigger: expected one character, got {s:?}")),
    }
}

/// The table holding the widget hooks: `widget` if it is one, else the
/// plugin's own.
pub(crate) fn widget_table(plugin: &Table) -> mlua::Result<Table> {
    Ok(match plugin.get::<Value>("widget")? {
        Value::Table(t) => t,
        _ => plugin.clone(),
    })
}

/// `popup_on = 'hover'` (the default) or `'click'`.
pub(crate) fn popup_on(v: &Value) -> Result<PopupOn, String> {
    match v {
        Value::Nil => Ok(PopupOn::Hover),
        Value::String(s) if s.as_bytes() == b"hover" => Ok(PopupOn::Hover),
        Value::String(s) if s.as_bytes() == b"click" => Ok(PopupOn::Click),
        v => Err(format!(
            "popup_on: expected 'hover' or 'click', got {}",
            show(v)
        )),
    }
}

/// `span = 2`, or `span = { horizontal = 2, vertical = 1 }`.
pub(crate) fn span(v: &Value) -> Result<Span, String> {
    let num = |v: &Value| match v {
        Value::Integer(i) => Some(*i as f64),
        Value::Number(n) => Some(*n),
        _ => None,
    };
    match v {
        Value::Nil => Ok(Span::default()),
        Value::Table(t) => {
            let axis = |k: &str| -> Result<f32, String> {
                match t.get::<Value>(k).map_err(|e| e.to_string())? {
                    Value::Nil => Ok(1.0),
                    v => num(&v)
                        .map(span_step)
                        .ok_or_else(|| format!("span.{k}: expected a number")),
                }
            };
            Ok(Span {
                horizontal: axis("horizontal")?,
                vertical: axis("vertical")?,
            })
        }
        v => {
            let s = num(v)
                .map(span_step)
                .ok_or("span: expected a number or a table")?;
            Ok(Span {
                horizontal: s,
                vertical: s,
            })
        }
    }
}

fn list<T>(
    plugin: &Table,
    key: &str,
    item: impl Fn(&Table) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    match plugin.get::<Value>(key).map_err(|e| e.to_string())? {
        Value::Nil => Ok(Vec::new()),
        Value::Table(t) => (t.sequence_values::<Value>())
            .enumerate()
            .map(|(i, v)| match v {
                Ok(Value::Table(t)) => item(&t).map_err(|e| format!("{key}[{}]: {e}", i + 1)),
                Ok(_) => Err(format!("{key}[{}]: expected a table", i + 1)),
                Err(e) => Err(e.to_string()),
            })
            .collect(),
        _ => Err(format!("{key}: expected a list of tables")),
    }
}

fn optional(t: &Table, key: &str) -> Result<Option<String>, String> {
    match t.get::<Value>(key).map_err(|e| e.to_string())? {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(s.to_string_lossy())),
        _ => Err(format!("{key}: expected a string")),
    }
}

fn required(t: &Table, key: &str) -> Result<String, String> {
    optional(t, key)?.ok_or_else(|| format!("{key} is missing"))
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{:?}", s.to_string_lossy()),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        v => v.type_name().to_string(),
    }
}

/// The host's view of what its instances provide: filled as they load,
/// emptied as they fail or go. Widgets, launcher actions and launcher
/// sources are looked up here.
#[derive(Debug, Default)]
pub struct Registry {
    loaded: HashMap<InstanceId, Capabilities>,
}

impl Registry {
    pub fn insert(&mut self, instance: &str, caps: Capabilities) {
        self.loaded.insert(instance.to_string(), caps);
    }

    pub fn remove(&mut self, instance: &str) -> Option<Capabilities> {
        self.loaded.remove(instance)
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.loaded.retain(|i, _| keep(i));
    }

    pub fn get(&self, instance: &str) -> Option<&Capabilities> {
        self.loaded.get(instance)
    }

    /// The widget hooks of `instance`, if it is loaded and has any.
    pub fn widget(&self, instance: &str) -> Option<&WidgetCaps> {
        self.loaded.get(instance)?.widget.as_ref()
    }

    /// The loaded instances with widget hooks.
    pub fn widgets(&self) -> impl Iterator<Item = (&str, &WidgetCaps)> {
        (self.loaded.iter()).filter_map(|(i, c)| Some((i.as_str(), c.widget.as_ref()?)))
    }

    /// The launcher actions declared, with their instances.
    pub fn actions(&self) -> impl Iterator<Item = (&str, &ActionDecl)> {
        (self.loaded.iter()).flat_map(|(i, c)| c.actions.iter().map(move |a| (i.as_str(), a)))
    }

    /// The actions of the instances `keep` takes whose `when` holds for
    /// `subject`: by instance, each's in the order it declared them.
    pub fn actions_for(
        &self,
        subject: &Subject,
        mut keep: impl FnMut(&str) -> bool,
    ) -> Vec<(&str, &ActionDecl)> {
        let mut instances: Vec<&InstanceId> = (self.loaded.keys()).filter(|i| keep(i)).collect();
        instances.sort();
        (instances.into_iter())
            .flat_map(|i| self.loaded[i].actions.iter().map(move |a| (i.as_str(), a)))
            .filter(|(_, a)| a.when.matches(subject))
            .collect()
    }

    /// The launcher sources declared, with their instances.
    pub fn sources(&self) -> impl Iterator<Item = (&str, &SourceDecl)> {
        (self.loaded.iter()).flat_map(|(i, c)| c.sources.iter().map(move |s| (i.as_str(), s)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mlua::Lua;

    fn caps(code: &str) -> Result<Capabilities, String> {
        let lua = Lua::new();
        let t: Table = lua.load(code).eval().map_err(|e| e.to_string())?;
        Capabilities::read(&t)
    }

    #[test]
    fn top_level_and_nested_hooks() {
        let c = caps("return { setup = function() end, label = function() end }").unwrap();
        assert!(c.setup);
        let w = c.widget.unwrap();
        assert!(w.label && !w.icon && !w.popup);
        let c = caps("return { widget = { icon = function() end, span = 2 } }").unwrap();
        let w = c.widget.clone().unwrap();
        assert!(w.icon && !w.label);
        assert_eq!(
            w.span,
            Span {
                horizontal: 2.0,
                vertical: 2.0
            }
        );
        assert!(!c.setup && !c.launcher());
        // No hooks: no widget.
        assert_eq!(caps("return {}").unwrap(), Capabilities::default());
    }

    #[test]
    fn spans_snap_down() {
        assert_eq!(span_step(0.5), 1.0);
        assert_eq!(span_step(1.7), 1.5);
        assert_eq!(span_step(2.0), 2.0);
        assert_eq!(span_step(9.0), 2.0);
        let c = caps("return { label = function() end, span = { horizontal = 2 } }").unwrap();
        let s = c.widget.unwrap().span;
        assert_eq!((s.horizontal, s.vertical), (2.0, 1.0));
        assert!(caps("return { label = function() end, span = 'wide' }").is_err());
    }

    #[test]
    fn popup_on() {
        let c = caps("return { popup = function() end }").unwrap();
        assert_eq!(c.widget.unwrap().popup_on, PopupOn::Hover);
        let c = caps("return { widget = { popup = function() end, popup_on = 'click' } }").unwrap();
        assert_eq!(c.widget.unwrap().popup_on, PopupOn::Click);
        let e = caps("return { popup = function() end, popup_on = 'press' }").unwrap_err();
        assert!(e.contains("popup_on"), "{e}");
    }

    #[test]
    fn launcher_actions_and_sources() {
        let c = caps(
            "return { actions = { { id = 'shrink', name = 'Shrink', icon = 'image-x-generic',
                                    when = { kind = 'file', mime = 'image/*' },
                                    choices = function() end, run = function() end },
                                  { id = 'logs', run = function() end } },
                      sources = { { id = 'ssh', trigger = '>', items = print } } }",
        )
        .unwrap();
        assert!(c.launcher() && c.widget.is_none());
        let shrink = &c.actions[0];
        assert_eq!(shrink.name, "Shrink");
        assert_eq!(shrink.icon.as_deref(), Some("image-x-generic"));
        assert!(shrink.choices && !c.actions[1].choices);
        assert_eq!(shrink.when.mime.as_deref(), Some("image/*"));
        assert_eq!(c.actions[1].name, "logs");
        assert_eq!(c.actions[1].when, When::default());
        assert_eq!(c.sources[0].name, "ssh");
        assert_eq!(c.sources[0].trigger, '>');
        assert!(!c.sources[0].search && !c.sources[0].run);
        let err = |code: &str| caps(code).unwrap_err();
        assert!(err("return { actions = { 1 } }").contains("actions[1]"));
        assert!(err("return { actions = { { name = 'x', run = print } } }").contains("id"));
        assert!(err("return { actions = { { id = 'x' } } }").contains("run"));
        let e = err("return { actions = { { id = 'x', run = print, choices = {} } } }");
        assert!(e.contains("choices"), "{e}");
        let e = err("return { actions = { { id = 'x', run = print, when = { kind = 'dir' } } } }");
        assert!(e.contains("when.kind"), "{e}");
        let e =
            err("return { actions = { { id = 'x', run = print }, { id = 'x', run = print } } }");
        assert!(e.contains("taken"), "{e}");
    }

    #[test]
    fn launcher_sources() {
        let c = caps(
            "return { sources = {
               { id = 'calc', name = 'Calculator', icon = 'accessories-calculator',
                 trigger = '=', search = print, run = print },
               { id = 'ssh', trigger = '>', items = print } } }",
        )
        .unwrap();
        assert!(c.launcher() && c.actions.is_empty());
        let calc = &c.sources[0];
        assert_eq!((calc.name.as_str(), calc.trigger), ("Calculator", '='));
        assert_eq!(calc.icon.as_deref(), Some("accessories-calculator"));
        assert!(calc.search && calc.run);
        let err = |code: &str| caps(code).unwrap_err();
        let e = err("return { sources = { { id = 'x', trigger = '>' } } }");
        assert!(e.contains("items or search"), "{e}");
        let e = err(
            "return { sources = { { id = 'x', trigger = '>', items = print, search = print } } }",
        );
        assert!(e.contains("not both"), "{e}");
        let e = err("return { sources = { { id = 'x', items = print } } }");
        assert!(e.contains("trigger"), "{e}");
        for bad in ["''", "'>>'", "' '"] {
            let e = err(&format!(
                "return {{ sources = {{ {{ id = 'x', trigger = {bad}, items = print }} }} }}"
            ));
            assert!(e.contains("one character"), "{e}");
        }
        let e = err("return { sources = { { id = 'x', trigger = '>', items = {} } } }");
        assert!(e.contains("items: expected a function"), "{e}");
        let e = err(
            "return { sources = { { id = 'x', trigger = '>', items = print },
                                  { id = 'x', trigger = '<', items = print } } }",
        );
        assert!(e.contains("taken"), "{e}");
    }

    #[test]
    fn api_version() {
        assert!(caps("return { api = 1 }").is_ok());
        assert!(caps("return { api = 2 }").unwrap_err().contains("api 2"));
    }

    #[test]
    fn registry() {
        let mut r = Registry::default();
        r.insert("app:a", caps("return { label = function() end }").unwrap());
        r.insert(
            "plugin:b",
            caps(
                "return { actions = { { id = 'x', run = print },
                                      { id = 'y', run = print, when = { kind = 'app' } } } }",
            )
            .unwrap(),
        );
        r.insert(
            "plugin:a",
            caps("return { actions = { { id = 'z', run = print } } }").unwrap(),
        );
        assert!(r.widget("app:a").is_some_and(|w| w.label));
        assert!(r.widget("plugin:b").is_none());
        assert_eq!(r.widgets().count(), 1);
        assert_eq!(r.actions().count(), 3);
        // By instance, in order, filtered.
        let file = Subject::File {
            path: "/a".into(),
            name: "a".into(),
            dir: false,
            mime: Vec::new(),
        };
        let found = |keep: &dyn Fn(&str) -> bool| {
            (r.actions_for(&file, keep).into_iter())
                .map(|(i, a)| format!("{i}/{}", a.id))
                .collect::<Vec<_>>()
        };
        assert_eq!(found(&|_| true), ["plugin:a/z", "plugin:b/x"]);
        assert_eq!(found(&|i| i != "plugin:a"), ["plugin:b/x"]);
        r.retain(|i| i != "app:a");
        assert!(r.get("app:a").is_none());
    }
}
