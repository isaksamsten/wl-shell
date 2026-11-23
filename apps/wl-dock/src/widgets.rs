//! Widgets: Lua plugins (wl-shell-lua) behind the dock's items.
//!
//! Which plugins run comes from the config (`specs`): one state per
//! `[app.<id>]` table with a `widget`, one per standalone widget
//! (`[widget.<name>]`) placed in the pins file (`@name`), and one per
//! `[plugin.<name>]` table (kept only if the plugin has launcher
//! capabilities). A plugin name is looked up as `plugins/<name>.lua`
//! next to the config file, then among the bundled plugins (`BUNDLED`);
//! `[plugin.<name>] path` overrides that. A placement's options are its
//! plugin table's, with its own over them.
//!
//! `Widgets` is the dock's side of the runtime: what each instance
//! provides (the registry), its open requests, and what the widgets show
//! now, by the key of their item (an app's ID, or a standalone widget's
//! `dock::widget_key`): a widget's `label()` replaces the item's name, its
//! `icon(tile)` tree the item's icon, and its span lengthens its tile.
//! Labels and icons are asked for once the plugin loads and whenever it
//! calls `redraw()` (icons also when the tile changes); an answer after
//! its deadline is dropped, and a plugin that fails gives the app its
//! own name, icon and span back (a standalone widget shows a placeholder).
//!
//! The tile `icon(tile)` gets is the first dock's, magnified, at the
//! largest scale of any dock: the tree is laid out again for each dock's
//! own size when it is painted, so one tree serves them all.
//!
//! An app's widget knows the app by its names (`dock.app`): its ID and
//! configured name from the start, and the desktop entry it matched (and
//! that entry's `StartupWMClass` and name) once the worker has looked it
//! up (`set_app`). Plugins `require` the bundled modules (`MODULES`) when
//! their own folder has no such file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wl_shell_log::warn;
use wl_shell_lua::ui::Node;
use wl_shell_lua::{
    AppInfo, Axis, Event, InstanceSpec, Pending, Registry, Reply, RequestKind, Role, Runtime,
    Settings, Source, Span, Tile, Topic,
};
use wl_shell_render::icons::AppEntry;

use crate::config::{Config, plugin_name_ok};
use crate::dock;

/// Plugins built into the dock, by name.
pub const BUNDLED: &[(&str, &str)] = &[
    ("clock", include_str!("../plugins/clock.lua")),
    ("player", include_str!("../plugins/player.lua")),
];

/// Modules built into the dock that plugins `require`, by name.
pub const MODULES: &[(&str, &str)] = &[("mpris", include_str!("../plugins/mpris.lua"))];

/// How long a label may take.
const LABEL_TIMEOUT: Duration = Duration::from_millis(500);
/// How long an icon may take.
const ICON_TIMEOUT: Duration = Duration::from_millis(1000);

/// The instance of the widget on the app `app_id`.
pub fn app_instance(app_id: &str) -> String {
    format!("app:{app_id}")
}

/// The instance of the standalone widget `name`.
fn widget_instance(name: &str) -> String {
    format!("widget:{name}")
}

/// The key of the item an instance is the widget of: its app's ID, or
/// the standalone widget's key. None for a plugin table's.
pub fn instance_key(instance: &str) -> Option<String> {
    match instance.split_once(':')? {
        ("app", app) => Some(app.to_string()),
        ("widget", name) => Some(dock::widget_key(name)),
        _ => None,
    }
}

/// The instance behind the item with key `key`.
pub fn key_instance(key: &str) -> String {
    match dock::widget_name(key) {
        Some(name) => widget_instance(name),
        None => app_instance(key),
    }
}

fn plugin_instance(name: &str) -> String {
    format!("plugin:{name}")
}

/// Where plugins live: `plugins` next to the config file.
pub fn plugin_dir(config_path: Option<&Path>) -> Option<PathBuf> {
    Some(config_path?.parent()?.join("plugins"))
}

/// The file plugin `name` is looked for in first: `[plugin.<name>]
/// path` (`~/` expands; relative to the config's folder), else
/// `<dir>/<name>.lua`.
fn user_path(config: &Config, dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let explicit = config.plugin.get(name).and_then(|p| p.path.as_deref());
    match explicit {
        Some(path) => {
            let base = dir.and_then(Path::parent);
            let home = std::env::var_os("HOME");
            Some(wl_shell_config::file::resolve(
                Path::new(path.trim()),
                base,
                home.as_deref(),
            ))
        }
        None => Some(dir?.join(format!("{name}.lua"))),
    }
}

/// Where plugin `name`'s code comes from: its file if there is one (or
/// `path` names it), else the bundled plugin, else the file it would be
/// (which fails to load until it exists).
pub fn source(config: &Config, dir: Option<&Path>, name: &str) -> Option<Source> {
    let user = user_path(config, dir, name);
    let explicit = (config.plugin.get(name)).is_some_and(|p| p.path.is_some());
    if let Some(path) = &user
        && (explicit || path.exists())
    {
        return Some(Source::File(path.clone()));
    }
    match BUNDLED.iter().find(|(n, _)| *n == name) {
        Some((n, code)) => Some(Source::Code {
            name: n.to_string(),
            code: Arc::from(*code),
        }),
        None => user.map(Source::File),
    }
}

/// The files to watch: each plugin's own file, whether or not it exists
/// yet (one appearing replaces the bundled plugin).
pub fn watched(config: &Config, dir: Option<&Path>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = names(config)
        .filter_map(|name| user_path(config, dir, name))
        .collect();
    paths.sort();
    paths.dedup();
    paths
}

/// The plugins the config names.
fn names(config: &Config) -> impl Iterator<Item = &str> {
    let placed = (config.app.values()).filter_map(|a| Some(a.widget.as_ref()?.plugin.as_str()));
    let standalone = (config.widget.iter()).map(|(name, w)| w.plugin_for(name));
    (config.plugin.keys().map(String::as_str))
        .chain(placed)
        .chain(standalone)
        .filter(|n| plugin_name_ok(n))
}

/// Whether the standalone widget `name` is shown when it is pinned: it
/// has a table, and widgets are on.
pub fn standalone(config: &Config, name: &str) -> bool {
    config.widgets.enabled && plugin_name_ok(name) && config.widget.contains_key(name)
}

/// `[plugin.<name>]`'s options for plugin `plugin`, with `own` over them.
fn merged(config: &Config, plugin: &str, own: &toml::Table) -> serde_json::Value {
    let mut options = (config.plugin.get(plugin))
        .map(|p| p.options.clone())
        .unwrap_or_default();
    options.extend(own.clone());
    json(&options)
}

/// `t` as JSON, for `setup(opts)`.
fn json(t: &toml::Table) -> serde_json::Value {
    serde_json::to_value(t).unwrap_or_else(|_| serde_json::Value::Object(Default::default()))
}

/// The names app `app_id` goes by: its ID, its configured name, and
/// what its desktop `entry` says (if it is known).
pub fn app_info(config: &Config, app_id: &str, entry: Option<&AppEntry>) -> AppInfo {
    let own = (config.app(app_id)).and_then(|a| a.name.clone());
    let text = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    AppInfo {
        desktop: entry.and_then(|e| text(e.desktop_id.trim_end_matches(".desktop"))),
        wm_class: entry.and_then(|e| e.wm_class.as_deref().and_then(text)),
        name: own
            .and_then(|n| text(&n))
            .or_else(|| entry.and_then(|e| text(&e.name))),
        ..AppInfo::new(app_id)
    }
}

/// The instances the config asks for, with the standalone widgets
/// `placed` (their names, as pinned); plugins in `dir`.
pub fn specs(config: &Config, dir: Option<&Path>, placed: &[String]) -> Vec<InstanceSpec> {
    if !config.widgets.enabled {
        return Vec::new();
    }
    let budget = config.widgets.budget as u64;
    let mut specs = Vec::new();
    for (name, plugin) in &config.plugin {
        if !plugin_name_ok(name) {
            warn!("[plugin.{name}]: bad plugin name (letters, digits, - and _)");
            continue;
        }
        let Some(source) = source(config, dir, name) else {
            continue;
        };
        specs.push(InstanceSpec {
            id: plugin_instance(name),
            plugin: name.clone(),
            source,
            role: Role::Plugin,
            app: None,
            options: json(&plugin.options),
            budget,
        });
    }
    for (app_id, app) in &config.app {
        let Some(widget) = &app.widget else { continue };
        let Some(source) = source(config, dir, &widget.plugin) else {
            warn!("[app.{app_id}]: no plugin {:?}", widget.plugin);
            continue;
        };
        specs.push(InstanceSpec {
            id: app_instance(app_id),
            plugin: widget.plugin.clone(),
            source,
            role: Role::Widget,
            // Its desktop entry's names come later (`set_app`).
            app: Some(app_info(config, app_id, None)),
            options: merged(config, &widget.plugin, &widget.options),
            budget,
        });
    }
    for name in placed {
        let Some(widget) = config.widget.get(name) else {
            warn!("@{name} is pinned, but there is no [widget.{name}]: not shown");
            continue;
        };
        if !plugin_name_ok(name) {
            warn!("[widget.{name}]: bad widget name (letters, digits, - and _)");
            continue;
        }
        let plugin = widget.plugin_for(name);
        let Some(source) = source(config, dir, plugin) else {
            warn!("[widget.{name}]: no plugin {plugin:?}");
            continue;
        };
        specs.push(InstanceSpec {
            id: widget_instance(name),
            plugin: plugin.to_string(),
            source,
            role: Role::Widget,
            app: None,
            options: merged(config, plugin, &widget.options),
            budget,
        });
    }
    specs
}

/// What the dock does about a runtime event.
#[derive(Debug, Default, PartialEq)]
pub struct Effect {
    /// The widgets' labels changed.
    pub labels: bool,
    /// Their icons changed.
    pub icons: bool,
    /// Their spans changed: the items do.
    pub spans: bool,
    pub clipboard: Option<String>,
    pub popup: Option<(String, Option<Arc<Node>>)>,
}

impl Effect {
    #[cfg(test)]
    pub fn any(&self) -> bool {
        self.labels || self.icons || self.spans || self.clipboard.is_some()
    }
}

/// What a widget's tile is, but its span: the icon's magnified size, the
/// dock's axis, and the scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TileBase {
    pub size: f64,
    pub axis: Axis,
    pub scale: i32,
}

/// A widget's icon: its tree, and a version that changes with it.
#[derive(Clone, Debug, PartialEq)]
pub struct WidgetIcon {
    pub version: u64,
    pub tree: Arc<Node>,
}

/// The dock's side of the plugin runtime.
#[derive(Default)]
pub struct Widgets {
    runtime: Option<Runtime>,
    registry: Registry,
    pending: Pending,
    /// The item whose popup is open (or opening), by key.
    open: Option<String>,
    /// The widgets' labels, by their items' keys.
    labels: BTreeMap<String, String>,
    /// Their icons, by their items' keys.
    icons: BTreeMap<String, WidgetIcon>,
    /// Their spans, by their items' keys (the ones that aren't 1).
    spans: BTreeMap<String, Span>,
    versions: u64,
    /// The tile icons are drawn for; None until there is a dock.
    tile: Option<TileBase>,
    /// Plugin files changed since they were last reloaded.
    pub touched: Vec<PathBuf>,
    /// The files watched now.
    pub watching: Vec<PathBuf>,
}

impl Widgets {
    /// Start the runtime; its events go to `sink`.
    pub fn start(&mut self, sink: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<()> {
        let settings = Settings {
            modules: MODULES,
            ..Settings::default()
        };
        self.runtime = Some(Runtime::new(settings, sink)?);
        Ok(())
    }

    /// Run `specs`; true if labels, icons or spans went (their widgets
    /// did).
    pub fn configure(&mut self, specs: Vec<InstanceSpec>) -> bool {
        let Some(runtime) = &self.runtime else {
            return false;
        };
        let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
        runtime.configure(specs);
        let kept = |i: &str| ids.iter().any(|id| id == i);
        self.registry.retain(kept);
        self.pending.retain(kept);
        let before = (self.labels.len(), self.icons.len(), self.spans.len());
        self.labels.retain(|key, _| kept(&key_instance(key)));
        self.icons.retain(|key, _| kept(&key_instance(key)));
        self.spans.retain(|key, _| kept(&key_instance(key)));
        before != (self.labels.len(), self.icons.len(), self.spans.len())
    }

    /// The file at `path` changed.
    pub fn reload(&self, path: &Path) {
        if let Some(runtime) = &self.runtime {
            runtime.reload(path);
        }
    }

    /// The widget on app `app.id` (if there is one) knows it by `app`'s
    /// names now.
    pub fn set_app(&self, app: AppInfo) {
        if let Some(runtime) = &self.runtime {
            let instance = app_instance(&app.id);
            runtime.set_app(&instance, app);
        }
    }

    /// The remote image `src` arrived: the icons that show it are new
    /// (with new versions, so they are painted again); true if any are.
    pub fn image_arrived(&mut self, src: &str) -> bool {
        let mut any = false;
        for icon in self.icons.values_mut() {
            if icon.tree.shows_image(src) {
                self.versions += 1;
                icon.version = self.versions;
                any = true;
            }
        }
        any
    }

    /// The widgets' labels, by their items' keys.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    /// The widgets' icons, by their items' keys.
    pub fn icons(&self) -> &BTreeMap<String, WidgetIcon> {
        &self.icons
    }

    /// The span of the tile of the item `key` on a dock along `axis`.
    pub fn span(&self, key: &str, axis: Axis) -> f32 {
        self.spans.get(key).map_or(1.0, |s| match axis {
            Axis::Horizontal => s.horizontal,
            Axis::Vertical => s.vertical,
        })
    }

    /// The docks' tile changed: icons are asked for again.
    pub fn set_tile(&mut self, tile: Option<TileBase>) {
        if tile == self.tile {
            return;
        }
        self.tile = tile;
        let instances: Vec<String> = (self.registry.widgets())
            .filter(|(_, w)| w.icon)
            .map(|(i, _)| i.to_string())
            .collect();
        for instance in instances {
            self.ask_icon(&instance);
        }
    }

    /// Ask `instance` for its icon, if it has one and there is a tile.
    fn ask_icon(&mut self, instance: &str) {
        let wants = self.registry.widget(instance).is_some_and(|w| w.icon);
        let (Some(key), Some(base), true) = (instance_key(instance), self.tile, wants) else {
            return;
        };
        let span = self.span(&key, base.axis) as f64;
        let (w, h) = match base.axis {
            Axis::Horizontal => (base.size * span, base.size),
            Axis::Vertical => (base.size, base.size * span),
        };
        let tile = Tile {
            w,
            h,
            span: span as f32,
            axis: base.axis,
            scale: base.scale,
        };
        if let Some(runtime) = &self.runtime
            && let Some(ticket) = runtime.request(instance, RequestKind::Icon(tile), ICON_TIMEOUT)
        {
            self.pending.insert(instance, ticket);
        }
    }

    /// Set (or with None, drop) the icon of `instance`'s item; true if it
    /// changed.
    fn set_icon(&mut self, instance: &str, tree: Option<Arc<Node>>) -> bool {
        let Some(key) = instance_key(instance) else {
            return false;
        };
        match tree {
            Some(t) if self.icons.get(&key).is_some_and(|i| i.tree == t) => false,
            Some(tree) => {
                self.versions += 1;
                let version = self.versions;
                self.icons.insert(key, WidgetIcon { version, tree });
                true
            }
            None => self.icons.remove(&key).is_some(),
        }
    }

    /// Set the span of `instance`'s item; true if it changed.
    fn set_span(&mut self, instance: &str, span: Option<Span>) -> bool {
        let Some(key) = instance_key(instance) else {
            return false;
        };
        let span = span.filter(|s| (s.horizontal, s.vertical) != (1.0, 1.0));
        let old = match span {
            Some(s) => self.spans.insert(key, s),
            None => self.spans.remove(&key),
        };
        old != span
    }

    /// Ask `instance` for its label, if it has one.
    fn ask_label(&mut self, instance: &str) {
        let wants = self.registry.widget(instance).is_some_and(|w| w.label);
        if let (true, Some(runtime)) = (wants && instance_key(instance).is_some(), &self.runtime)
            && let Some(ticket) = runtime.request(instance, RequestKind::Label, LABEL_TIMEOUT)
        {
            self.pending.insert(instance, ticket);
        }
    }

    /// Whether the widget behind the item `key` has a popup.
    pub fn supports_popup(&self, key: &str) -> bool {
        (self.registry.widget(&key_instance(key))).is_some_and(|w| w.popup)
    }

    /// Whether the widget behind the item `key` takes its clicks
    /// (`on_click`), so a click doesn't toggle its popup.
    pub fn takes_clicks(&self, key: &str) -> bool {
        (self.registry.widget(&key_instance(key))).is_some_and(|w| w.on_click)
    }

    /// Tell the widget behind the item `key` about `kind`.
    pub fn notify(&self, key: &str, kind: RequestKind) {
        if let Some(rt) = &self.runtime {
            rt.request(&key_instance(key), kind, ICON_TIMEOUT);
        }
    }

    /// The popup of the item `key` opens (None: the open one closes).
    pub fn select_popup(&mut self, key: Option<&str>) {
        if let Some(old) = self.open.take() {
            self.notify(&old, RequestKind::Visibility(false));
        }
        self.open = key.map(str::to_owned);
        if let Some(key) = key {
            self.notify(key, RequestKind::Visibility(true));
            self.ask_popup(&key_instance(key));
        }
    }

    fn ask_popup(&mut self, instance: &str) {
        if self.open.is_some()
            && instance_key(instance) == self.open
            && let Some(rt) = &self.runtime
            && let Some(ticket) = rt.request(instance, RequestKind::Popup, ICON_TIMEOUT)
        {
            self.pending.insert(instance, ticket);
        }
    }

    /// Set (or with None, drop) the label of `instance`'s item; true if
    /// it changed.
    fn set_label(&mut self, instance: &str, label: Option<String>) -> bool {
        let Some(key) = instance_key(instance) else {
            return false;
        };
        match label {
            Some(l) if self.labels.get(&key) != Some(&l) => {
                self.labels.insert(key, l);
                true
            }
            Some(_) => false,
            None => self.labels.remove(&key).is_some(),
        }
    }

    /// Take in a runtime event.
    pub fn event(&mut self, event: Event) -> Effect {
        let mut fx = Effect::default();
        match event {
            Event::Loaded { instance, caps } => {
                let widget = caps.widget.clone().unwrap_or_default();
                self.registry.insert(&instance, caps);
                fx.spans = self.set_span(&instance, Some(widget.span));
                self.ask_label(&instance);
                self.ask_icon(&instance);
                self.ask_popup(&instance);
                // A reloaded plugin may have no label or icon anymore.
                fx.labels = !widget.label && self.set_label(&instance, None);
                fx.icons = !widget.icon && self.set_icon(&instance, None);
            }
            Event::Failed { instance, .. } => {
                self.registry.remove(&instance);
                if let Some(key) = instance_key(&instance) {
                    fx.popup = Some((key, None));
                }
                fx.labels = self.set_label(&instance, None);
                fx.icons = self.set_icon(&instance, None);
                fx.spans = self.set_span(&instance, None);
            }
            Event::Redraw(instance) => {
                self.ask_label(&instance);
                self.ask_icon(&instance);
                self.ask_popup(&instance);
            }
            Event::Span { instance, span } => {
                fx.spans = self.set_span(&instance, Some(span));
                // Drawn for the new length.
                if fx.spans {
                    self.ask_icon(&instance);
                }
            }
            Event::Answer(answer) => {
                let Some((topic, answer)) = self.pending.accept_topic(answer, Instant::now())
                else {
                    return fx;
                };
                let instance = &answer.instance;
                match (answer.result, topic) {
                    (Ok(Reply::Label(label)), _) => fx.labels = self.set_label(instance, label),
                    (Ok(Reply::Icon(tree)), _) => fx.icons = self.set_icon(instance, tree),
                    (result, Topic::Popup) if instance_key(instance) == self.open => {
                        if let Some(app) = &self.open {
                            fx.popup = Some((
                                app.clone(),
                                match result {
                                    Ok(Reply::Popup(tree)) => tree,
                                    _ => None,
                                },
                            ));
                        }
                    }
                    // The app's own name or icon, while it fails.
                    (Err(_), Topic::Label) => fx.labels = self.set_label(instance, None),
                    (Err(_), Topic::Icon) => fx.icons = self.set_icon(instance, None),
                    _ => {}
                }
            }
            Event::Clipboard { text, .. } => fx.clipboard = Some(text),
            // For the launcher's sources (not yet).
            Event::Invalidate(_) => {}
        }
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_config::ConfigFile;

    fn parse(s: &str) -> Config {
        <Config as ConfigFile>::parse(s).unwrap()
    }

    #[test]
    fn placements_and_plugin_tables() {
        let c = parse(
            r#"
            [plugin.clock]
            format = "%H"
            zone = "UTC"

            [app.foot]
            widget = { plugin = "clock", format = "%M" }

            [app.firefox]
            widget = "clock"
            "#,
        );
        let dir = Path::new("/nonexistent/plugins");
        let specs = specs(&c, Some(dir), &[]);
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["plugin:clock", "app:firefox", "app:foot"]);
        // The bundled clock, since there is no file.
        assert!(matches!(&specs[0].source, Source::Code { name, .. } if name == "clock"));
        assert_eq!(specs[0].role, Role::Plugin);
        assert_eq!(specs[1].role, Role::Widget);
        // The placement's options over the plugin table's.
        assert_eq!(specs[1].options["format"], "%H");
        assert_eq!(specs[2].options["format"], "%M");
        assert_eq!(specs[2].options["zone"], "UTC");
        assert_eq!(specs[2].budget, 1_000_000);
        assert_eq!(watched(&c, Some(dir)), [dir.join("clock.lua")]);
        // Off: nothing runs.
        let off = parse("[widgets]\nenabled = false\n[app.foot]\nwidget = \"clock\"");
        assert!(super::specs(&off, Some(dir), &[]).is_empty());
    }

    #[test]
    fn standalone_widgets_run_while_placed() {
        let c = parse(
            r#"
            [plugin.clock]
            format = "%H"

            [widget.clock]
            name = "Clock"
            popup-delay = 600
            format = "%M"

            [widget.utc]
            plugin = "clock"
            zone = "UTC"
            "#,
        );
        let dir = Path::new("/nonexistent/plugins");
        assert_eq!(c.widget["clock"].name.as_deref(), Some("Clock"));
        assert_eq!(c.widget["clock"].popup_delay, Some(600));
        assert_eq!(c.widget["utc"].plugin_for("utc"), "clock");
        // Only the placed ones, and only those with a table.
        let placed = ["utc".to_string(), "gone".to_string(), "clock".to_string()];
        let specs = specs(&c, Some(dir), &placed);
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["plugin:clock", "widget:utc", "widget:clock"]);
        assert_eq!((specs[1].role, specs[1].app.as_ref()), (Role::Widget, None));
        assert_eq!(specs[1].plugin, "clock");
        assert_eq!(specs[1].options["format"], "%H");
        assert_eq!(specs[1].options["zone"], "UTC");
        // Its own keys but `plugin`, `name` and `popup-delay` are options.
        assert_eq!(specs[2].options["format"], "%M");
        assert!(specs[2].options.get("name").is_none());
        assert!(specs[2].options.get("popup-delay").is_none());
        assert!(standalone(&c, "clock") && !standalone(&c, "gone"));
        // Instances and items' keys.
        let key = dock::widget_key("utc");
        assert_eq!(key_instance(&key), "widget:utc");
        assert_eq!(instance_key("widget:utc"), Some(key));
        assert_eq!(instance_key("app:a:b").as_deref(), Some("a:b"));
        assert_eq!(key_instance("a:b"), "app:a:b");
        assert_eq!(instance_key("plugin:clock"), None);
        // Bad tables are config errors.
        let err = |s: &str| <Config as ConfigFile>::parse(s).unwrap_err();
        assert!(err("[widget.a]\nplugin = 1").contains("plugin"));
        assert!(err("[widget.a]\nplugin = '../x'").contains("bad plugin name"));
        assert!(err("[widget.a]\npopup-delay = 'x'").contains("popup-delay"));
    }

    #[test]
    fn lookup() {
        let dir = std::env::temp_dir().join(format!("wl-dock-plugins-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let c = parse(
            r#"
            [plugin.mine]
            path = "elsewhere/mine.lua"
            "#,
        );
        // A user file replaces the bundled one.
        assert!(matches!(
            source(&c, Some(&dir), "clock"),
            Some(Source::Code { .. })
        ));
        std::fs::write(dir.join("clock.lua"), "return {}").unwrap();
        assert_eq!(
            source(&c, Some(&dir), "clock"),
            Some(Source::File(dir.join("clock.lua")))
        );
        // `path` is relative to the config's folder, even if missing.
        let base = dir.parent().unwrap();
        assert_eq!(
            source(&c, Some(&dir), "mine"),
            Some(Source::File(base.join("elsewhere/mine.lua")))
        );
        // Unknown: the file it would be.
        assert_eq!(
            source(&c, Some(&dir), "other"),
            Some(Source::File(dir.join("other.lua")))
        );
        assert_eq!(source(&c, None, "other"), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_names_an_app_goes_by() {
        let c = parse(
            "[app.firefox]\nwidget = 'player'\n[app.foot]\nname = ' Terminal '\nwidget = 'player'",
        );
        // Before its entry is known: its ID (and configured name).
        let specs = specs(&c, None, &[]);
        assert_eq!(specs[0].app, Some(AppInfo::new("firefox")));
        assert_eq!(
            specs[1].app.as_ref().unwrap().name.as_deref(),
            Some("Terminal")
        );
        let entry = AppEntry {
            desktop_id: "org.mozilla.firefox.desktop".into(),
            name: "Firefox".into(),
            wm_class: Some("firefox".into()),
            ..Default::default()
        };
        assert_eq!(
            app_info(&c, "firefox", Some(&entry)),
            AppInfo {
                id: "firefox".into(),
                desktop: Some("org.mozilla.firefox".into()),
                wm_class: Some("firefox".into()),
                name: Some("Firefox".into()),
            }
        );
        // The configured name goes over the entry's.
        assert_eq!(
            app_info(&c, "foot", Some(&entry)).name.as_deref(),
            Some("Terminal")
        );
    }

    #[test]
    fn arriving_images_repaint_the_icons_that_show_them() {
        let mut w = Widgets::default();
        let image = |src: &str| {
            let mut n = Node::new(wl_shell_render::ui::Kind::Col);
            n.children.push(Node::new(wl_shell_render::ui::Kind::Image {
                src: src.into(),
                fit: Default::default(),
                radius: 0.0,
                blur: 0.0,
                opacity: 1.0,
            }));
            Arc::new(n)
        };
        w.icons.insert(
            "a".into(),
            WidgetIcon {
                version: 1,
                tree: image("https://x/a.png"),
            },
        );
        w.icons.insert(
            "b".into(),
            WidgetIcon {
                version: 2,
                tree: image("https://x/b.png"),
            },
        );
        w.versions = 2;
        assert!(w.image_arrived("https://x/a.png"));
        assert!(w.icons["a"].version > 2 && w.icons["b"].version == 2);
        assert!(!w.image_arrived("https://x/c.png"));
    }

    /// The bundled player loads (whatever the session bus has), with its
    /// `mpris` module.
    #[test]
    fn the_bundled_player_loads() {
        let (send, recv) = std::sync::mpsc::channel();
        let send = std::sync::Mutex::new(send);
        let mut w = Widgets::default();
        w.start(move |e| {
            let _ = send.lock().unwrap().send(e);
        })
        .unwrap();
        let c = parse("[widget.nowplaying]\nplugin = 'player'\n[app.spotify]\nwidget = 'player'");
        w.configure(specs(&c, None, &["nowplaying".to_string()]));
        let end = Instant::now() + Duration::from_secs(5);
        let mut loaded = 0;
        while loaded < 2 {
            match recv.recv_timeout(end - Instant::now()).expect("not loaded") {
                Event::Failed { error, .. } => panic!("{error}"),
                e @ Event::Loaded { .. } => {
                    loaded += 1;
                    w.event(e);
                }
                _ => {}
            }
        }
        let key = dock::widget_key("nowplaying");
        assert!(w.supports_popup(&key) && w.supports_popup("spotify"));
    }

    #[test]
    fn widget_keys() {
        let err = |s: &str| <Config as ConfigFile>::parse(s).unwrap_err();
        assert!(err("[app.a]\nwidget = 1").contains("widget"));
        assert!(err("[app.a]\nwidget = { format = 'x' }").contains("plugin"));
        assert!(err("[app.a]\nwidget = '../x'").contains("bad plugin name"));
        assert!(<Config as ConfigFile>::parse("[widgets]\nbudget = 5").is_err());
        let c = parse("[app.a]\nwidget = 'clock'\npopup-delay = 400\n[widgets]\npopup-delay = 0");
        let a = c.app("a").unwrap();
        assert_eq!(a.widget.as_ref().unwrap().plugin, "clock");
        assert_eq!((a.popup_delay, c.widgets.popup_delay), (Some(400), 0));
    }

    #[test]
    fn kitchen_sink_popup_loads() {
        let (send, recv) = std::sync::mpsc::channel();
        let mut w = Widgets::default();
        w.start(move |e| {
            let _ = send.send(e);
        })
        .unwrap();
        // Not bundled: loaded from its file under tests/.
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/plugins/kitchen-sink.lua"
        );
        let c = parse(&format!(
            "[plugin.kitchen-sink]\npath = '{path}'\n[app.demo]\nwidget = 'kitchen-sink'"
        ));
        let specs = specs(&c, None, &[]);
        w.configure(specs);
        let end = Instant::now() + Duration::from_secs(5);
        while !w.supports_popup("demo") {
            w.event(recv.recv_timeout(end - Instant::now()).unwrap());
        }
        w.select_popup(Some("demo"));
        loop {
            let e = recv.recv_timeout(end - Instant::now()).unwrap();
            if let Event::Answer(a) = &e {
                assert!(a.result.is_ok(), "{:?}", a.result);
            }
            if let Some((_, tree)) = w.event(e).popup {
                let tree = tree.unwrap();
                let nodes = tree.walk();
                assert!(nodes.iter().any(|n| n.handlers.commit));
                // A live seek bar and progress bar, for their own layers.
                use wl_shell_render::ui::Kind;
                let live = (nodes.iter())
                    .filter(|n| {
                        matches!(
                            n.kind,
                            Kind::Slider { live: Some(_), .. }
                                | Kind::Progress { live: Some(_), .. }
                        )
                    })
                    .count();
                assert_eq!(live, 2);
                break;
            }
        }
    }

    /// The bundled clock, through the runtime and back into the labels.
    #[test]
    fn the_bundled_clock_labels_its_app() {
        let (send, recv) = std::sync::mpsc::channel();
        let send = std::sync::Mutex::new(send);
        let mut w = Widgets::default();
        w.start(move |e| {
            let _ = send.lock().unwrap().send(e);
        })
        .unwrap();
        let c = parse("[app.foot]\nwidget = { plugin = 'clock', format = 'at %Y' }");
        w.configure(specs(&c, None, &[]));
        let year = std::process::Command::new("date")
            .arg("+%Y")
            .output()
            .unwrap();
        let want = format!("at {}", String::from_utf8_lossy(&year.stdout).trim());
        let end = Instant::now() + Duration::from_secs(5);
        while w.labels().get("foot") != Some(&want) {
            let e = recv.recv_timeout(end - Instant::now()).expect("no label");
            w.event(e);
        }
    }

    /// The bundled clock standalone: its face, its label and its popup,
    /// by its item's key; on an app, neither face nor popup.
    #[test]
    fn the_bundled_clock_stands_alone() {
        let (send, recv) = std::sync::mpsc::channel();
        let send = std::sync::Mutex::new(send);
        let mut w = Widgets::default();
        w.start(move |e| {
            let _ = send.lock().unwrap().send(e);
        })
        .unwrap();
        w.set_tile(Some(TileBase {
            size: 48.0,
            axis: Axis::Horizontal,
            scale: 1,
        }));
        let c = parse("[widget.clock]\n[app.foot]\nwidget = 'clock'");
        w.configure(specs(&c, None, &["clock".to_string()]));
        let key = dock::widget_key("clock");
        let end = Instant::now() + Duration::from_secs(5);
        while !(w.icons().contains_key(&key) && w.labels().contains_key(&key)) {
            w.event(recv.recv_timeout(end - Instant::now()).expect("no icon"));
        }
        assert!(w.supports_popup(&key) && !w.takes_clicks(&key));
        // Its popup comes with a tree; the app's with none.
        for (item, some) in [(key.as_str(), true), ("foot", false)] {
            w.select_popup(Some(item));
            let tree = loop {
                let e = recv.recv_timeout(end - Instant::now()).expect("no popup");
                if let Some((k, tree)) = w.event(e).popup {
                    assert_eq!(k, item);
                    break tree;
                }
            };
            assert_eq!(tree.is_some(), some, "{item:?}");
        }
        assert!(!w.icons().contains_key("foot"));
    }

    #[test]
    fn labels_follow_answers_and_failures() {
        use wl_shell_lua::{Answer, Capabilities, Request, WidgetCaps};
        let mut w = Widgets::default();
        let caps = Capabilities {
            widget: Some(WidgetCaps {
                label: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        // Without a runtime nothing is asked; answers to nothing are dropped.
        assert!(
            !w.event(Event::Loaded {
                instance: "app:a".into(),
                caps
            })
            .any()
        );
        let (req, ticket) = Request::new(RequestKind::Label, LABEL_TIMEOUT);
        w.pending.insert("app:a", ticket);
        let answer = |id| Answer {
            instance: "app:a".into(),
            id,
            result: Ok(Reply::Label(Some("12:00".into()))),
        };
        assert!(w.event(Event::Answer(answer(req.id))).labels);
        assert_eq!(w.labels().get("a").map(String::as_str), Some("12:00"));
        // Answered already: dropped.
        assert!(!w.event(Event::Answer(answer(req.id))).any());
        let failed = Event::Failed {
            instance: "app:a".into(),
            error: "x".into(),
        };
        assert!(w.event(failed).labels);
        assert!(w.labels().is_empty());
    }

    #[test]
    fn icons_and_spans_follow_answers_and_failures() {
        use wl_shell_lua::{Answer, Capabilities, Request, WidgetCaps};
        let mut w = Widgets::default();
        let caps = Capabilities {
            widget: Some(WidgetCaps {
                icon: true,
                span: Span {
                    horizontal: 2.0,
                    vertical: 1.0,
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let fx = w.event(Event::Loaded {
            instance: "app:a".into(),
            caps,
        });
        assert!(fx.spans);
        assert_eq!(w.span("a", Axis::Horizontal), 2.0);
        assert_eq!(w.span("a", Axis::Vertical), 1.0);
        assert_eq!(w.span("b", Axis::Horizontal), 1.0);
        let tile = Tile {
            w: 192.0,
            h: 96.0,
            span: 2.0,
            axis: Axis::Horizontal,
            scale: 1,
        };
        let tree = Arc::new(Node::text("x"));
        let answer = |w: &mut Widgets, tree: Option<Arc<Node>>| {
            let (req, ticket) = Request::new(RequestKind::Icon(tile), ICON_TIMEOUT);
            w.pending.insert("app:a", ticket);
            w.event(Event::Answer(Answer {
                instance: "app:a".into(),
                id: req.id,
                result: Ok(Reply::Icon(tree)),
            }))
        };
        assert!(answer(&mut w, Some(tree.clone())).icons);
        let v1 = w.icons()["a"].version;
        // The same tree again: nothing new.
        assert!(!answer(&mut w, Some(tree)).icons);
        assert!(answer(&mut w, Some(Arc::new(Node::text("y")))).icons);
        assert!(w.icons()["a"].version > v1);
        // nil: the app's own icon.
        assert!(answer(&mut w, None).icons);
        assert!(w.icons().is_empty());
        answer(&mut w, Some(Arc::new(Node::text("z"))));
        let span = Event::Span {
            instance: "app:a".into(),
            span: Span {
                horizontal: 1.5,
                vertical: 1.5,
            },
        };
        assert!(w.event(span).spans);
        assert_eq!(w.span("a", Axis::Vertical), 1.5);
        let fx = w.event(Event::Failed {
            instance: "app:a".into(),
            error: "x".into(),
        });
        assert!(fx.icons && fx.spans);
        assert!(w.icons().is_empty());
        assert_eq!(w.span("a", Axis::Horizontal), 1.0);
    }

    /// The bundled clock's face, through the runtime, painted as the
    /// worker would.
    #[test]
    fn the_bundled_clock_draws_its_face() {
        let (send, recv) = std::sync::mpsc::channel();
        let send = std::sync::Mutex::new(send);
        let mut w = Widgets::default();
        w.start(move |e| {
            let _ = send.lock().unwrap().send(e);
        })
        .unwrap();
        w.set_tile(Some(TileBase {
            size: 96.0,
            axis: Axis::Horizontal,
            scale: 2,
        }));
        let c = parse("[app.foot]\nwidget = { plugin = 'clock', face = true }");
        w.configure(specs(&c, None, &[]));
        let end = Instant::now() + Duration::from_secs(5);
        while !w.icons().contains_key("foot") {
            let e = recv.recv_timeout(end - Instant::now()).expect("no icon");
            w.event(e);
        }
        let tree = w.icons()["foot"].tree.clone();
        let req = wl_shell_render::ui::Request {
            app: Some("foot".into()),
            ..wl_shell_render::ui::Request::new(
                tree,
                wl_shell_render::ui::Bounds::Exact(96.0, 96.0),
                2,
                Arc::new(c.ui_theme()),
            )
        };
        let mut text = wl_shell_render::text::Text::new();
        let r = wl_shell_render::ui::render(&req, &mut text, &mut wl_shell_render::ui::NoResources);
        assert_eq!((r.image.width, r.image.height), (192, 192));
        // Opaque in the middle, clear in the corners.
        let alpha = |x: u32, y: u32| r.image.data[((y * 192 + x) * 4 + 3) as usize];
        assert!(alpha(96, 96) > 200 && alpha(1, 1) == 0);
    }
}
