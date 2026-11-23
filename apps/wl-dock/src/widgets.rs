//! Widgets: Lua plugins (wl-shell-lua) behind the dock's items.
//!
//! Which plugins run comes from the config (`specs`): one state per
//! `[app.<id>]` table with a `widget`, and one per `[plugin.<name>]`
//! table (kept only if the plugin has launcher capabilities). A plugin
//! name is looked up as `plugins/<name>.lua` next to the config file,
//! then among the bundled plugins (`BUNDLED`); `[plugin.<name>] path`
//! overrides that. A placement's options are its plugin table's, with
//! its own over them.
//!
//! `Widgets` is the dock's side of the runtime: what each instance
//! provides (the registry), its open requests, and what the widgets show
//! now. For now that is labels: an app widget's `label()` replaces the
//! app's name. It is asked for once the plugin loads and whenever it
//! calls `redraw()`; an answer after its deadline is dropped, and a
//! plugin that fails gives the app its own name back.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use wl_shell_log::warn;
use wl_shell_lua::{
    Event, InstanceSpec, Pending, Registry, Reply, RequestKind, Role, Runtime, Settings, Source,
};

use crate::config::{Config, plugin_name_ok};

/// Plugins built into the dock, by name.
pub const BUNDLED: &[(&str, &str)] = &[("clock", include_str!("../plugins/clock.lua"))];

/// How long a label may take.
const LABEL_TIMEOUT: Duration = Duration::from_millis(500);

/// The instance of the widget on the app `app_id`.
pub fn app_instance(app_id: &str) -> String {
    format!("app:{app_id}")
}

/// The app an instance is the widget of.
pub fn instance_app(instance: &str) -> Option<&str> {
    instance.strip_prefix("app:")
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
    (config.plugin.keys().map(String::as_str))
        .chain(placed)
        .filter(|n| plugin_name_ok(n))
}

/// `t` as JSON, for `setup(opts)`.
fn json(t: &toml::Table) -> serde_json::Value {
    serde_json::to_value(t).unwrap_or_else(|_| serde_json::Value::Object(Default::default()))
}

/// The instances the config asks for; plugins in `dir`.
pub fn specs(config: &Config, dir: Option<&Path>) -> Vec<InstanceSpec> {
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
        let mut options = (config.plugin.get(&widget.plugin))
            .map(|p| p.options.clone())
            .unwrap_or_default();
        options.extend(widget.options.clone());
        specs.push(InstanceSpec {
            id: app_instance(app_id),
            plugin: widget.plugin.clone(),
            source,
            role: Role::Widget,
            options: json(&options),
            budget,
        });
    }
    specs
}

/// What the dock does about a runtime event.
pub enum Effect {
    /// The widgets' labels changed.
    Labels,
    Clipboard(String),
}

/// The dock's side of the plugin runtime.
#[derive(Default)]
pub struct Widgets {
    runtime: Option<Runtime>,
    registry: Registry,
    pending: Pending,
    /// The widgets' labels, by app ID.
    labels: BTreeMap<String, String>,
    /// Plugin files changed since they were last reloaded.
    pub touched: Vec<PathBuf>,
    /// The files watched now.
    pub watching: Vec<PathBuf>,
}

impl Widgets {
    /// Start the runtime; its events go to `sink`.
    pub fn start(&mut self, sink: impl Fn(Event) + Send + Sync + 'static) -> std::io::Result<()> {
        self.runtime = Some(Runtime::new(Settings::default(), sink)?);
        Ok(())
    }

    /// Run `specs`; true if labels went (their widgets did).
    pub fn configure(&mut self, specs: Vec<InstanceSpec>) -> bool {
        let Some(runtime) = &self.runtime else {
            return false;
        };
        let ids: Vec<String> = specs.iter().map(|s| s.id.clone()).collect();
        runtime.configure(specs);
        let kept = |i: &str| ids.iter().any(|id| id == i);
        self.registry.retain(kept);
        self.pending.retain(kept);
        let before = self.labels.len();
        self.labels.retain(|app, _| kept(&app_instance(app)));
        self.labels.len() != before
    }

    /// The file at `path` changed.
    pub fn reload(&self, path: &Path) {
        if let Some(runtime) = &self.runtime {
            runtime.reload(path);
        }
    }

    /// The widgets' labels, by app ID.
    pub fn labels(&self) -> &BTreeMap<String, String> {
        &self.labels
    }

    /// Ask `instance` for its label, if it has one.
    fn ask_label(&mut self, instance: &str) {
        let wants = self.registry.widget(instance).is_some_and(|w| w.label);
        if let (true, Some(runtime)) = (wants && instance_app(instance).is_some(), &self.runtime)
            && let Some(ticket) = runtime.request(instance, RequestKind::Label, LABEL_TIMEOUT)
        {
            self.pending.insert(instance, ticket);
        }
    }

    /// Set (or with None, drop) the label of `instance`'s app; true if
    /// it changed.
    fn set_label(&mut self, instance: &str, label: Option<String>) -> bool {
        let Some(app) = instance_app(instance) else {
            return false;
        };
        match label {
            Some(l) if self.labels.get(app) != Some(&l) => {
                self.labels.insert(app.to_string(), l);
                true
            }
            Some(_) => false,
            None => self.labels.remove(app).is_some(),
        }
    }

    /// Take in a runtime event.
    pub fn event(&mut self, event: Event) -> Option<Effect> {
        match event {
            Event::Loaded { instance, caps } => {
                let labels = caps.widget.as_ref().is_some_and(|w| w.label);
                self.registry.insert(&instance, caps);
                self.ask_label(&instance);
                // A reloaded plugin may have no label anymore.
                (!labels && self.set_label(&instance, None)).then_some(Effect::Labels)
            }
            Event::Failed { instance, .. } => {
                self.registry.remove(&instance);
                self.set_label(&instance, None).then_some(Effect::Labels)
            }
            Event::Redraw(instance) => {
                self.ask_label(&instance);
                None
            }
            Event::Answer(answer) => {
                let answer = self.pending.accept(answer, Instant::now())?;
                let label = match answer.result {
                    Ok(Reply::Label(label)) => label,
                    // The app's own name, while it fails.
                    Err(_) => None,
                    Ok(_) => return None,
                };
                (self.set_label(&answer.instance, label)).then_some(Effect::Labels)
            }
            Event::Clipboard { text, .. } => Some(Effect::Clipboard(text)),
            // For the launcher's sources (not yet).
            Event::Invalidate(_) => None,
        }
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
        let specs = specs(&c, Some(dir));
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
        assert!(super::specs(&off, Some(dir)).is_empty());
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
        w.configure(specs(&c, None));
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
            w.event(Event::Loaded {
                instance: "app:a".into(),
                caps
            })
            .is_none()
        );
        let (req, ticket) = Request::new(RequestKind::Label, LABEL_TIMEOUT);
        w.pending.insert("app:a", ticket);
        let answer = |id| Answer {
            instance: "app:a".into(),
            id,
            result: Ok(Reply::Label(Some("12:00".into()))),
        };
        assert!(matches!(
            w.event(Event::Answer(answer(req.id))),
            Some(Effect::Labels)
        ));
        assert_eq!(w.labels().get("a").map(String::as_str), Some("12:00"));
        // Answered already: dropped.
        assert!(w.event(Event::Answer(answer(req.id))).is_none());
        let failed = Event::Failed {
            instance: "app:a".into(),
            error: "x".into(),
        };
        assert!(matches!(w.event(failed), Some(Effect::Labels)));
        assert!(w.labels().is_empty());
    }
}
