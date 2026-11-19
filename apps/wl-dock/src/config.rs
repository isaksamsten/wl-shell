//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-dock/config.toml` (default
//! `~/.config/wl-dock/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! General dock keys live at the root; feature settings have their own
//! tables. Output overrides mirror that structure under `[output.<name>]`.
//! `outputs` picks the outputs that get a dock.
//! Per-app settings (menu actions, middle click and scroll commands, the
//! icon, name and launch command, and what dropping files does) live in
//! `[app."<app-id>"]` tables (see `AppConfig`), and per-folder ones (how
//! its popup lists it, its icon) in `[folder."<path>"]` tables (see
//! `FolderConfig`).
//!
//! Colors live in `[appearance]`; `[appearance.dark]` and
//! `[appearance.light]` override its keys in the desktop's dark or light
//! mode (or the one `mode` forces). The menu's look is there too.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use wl_shell_config::{ConfigFile, Scheme, de};
use wl_shell_render::{files, shadow, text};

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// The outputs that get a dock.
    pub outputs: Outputs,
    pub position: Position,
    pub layer: DockLayer,
    /// App icon size at rest.
    #[serde(deserialize_with = "icon_size")]
    pub icon_size: u32,
    #[serde(deserialize_with = "de::px")]
    pub icon_gap: u32,
    /// Space between the dock and its screen edge.
    #[serde(deserialize_with = "de::px")]
    pub margin: u32,
    #[serde(deserialize_with = "de::px")]
    pub padding: u32,
    #[serde(deserialize_with = "magnification")]
    pub magnification: f64,
    #[serde(deserialize_with = "magnification_range")]
    pub magnification_range: f64,
    /// Keep windows out of the resting dock's area.
    pub reserve: bool,
    #[serde(deserialize_with = "de::duration")]
    pub hover_delay: u32,
    pub labels: bool,
    pub indicator: Indicator,
    pub active_indicator: bool,
    #[serde(deserialize_with = "drag_threshold")]
    pub drag_threshold: u32,
    /// Keyboard activation (`wl-dock --activate`).
    pub activate: ActivateConfig,
    pub autohide: AutohideConfig,
    pub windows: WindowsConfig,
    pub launcher: LauncherConfig,
    pub integration: IntegrationConfig,
    pub files: FilesConfig,
    pub trash: TrashConfig,
    pub appearance: Appearance,
    pub output: BTreeMap<String, OutputOverride>,
    pub app: BTreeMap<String, AppConfig>,
    pub folder: BTreeMap<String, FolderConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            outputs: Outputs::All,
            position: Position::default(),
            layer: DockLayer::default(),
            icon_size: 48,
            icon_gap: 6,
            margin: 6,
            padding: 6,
            magnification: 1.8,
            magnification_range: 3.0,
            reserve: true,
            hover_delay: 300,
            labels: true,
            indicator: Indicator::default(),
            active_indicator: true,
            drag_threshold: 6,
            activate: ActivateConfig::default(),
            autohide: AutohideConfig::default(),
            windows: WindowsConfig::default(),
            launcher: LauncherConfig::default(),
            integration: IntegrationConfig::default(),
            files: FilesConfig::default(),
            trash: TrashConfig::default(),
            appearance: Appearance::default(),
            output: BTreeMap::new(),
            app: BTreeMap::new(),
            folder: BTreeMap::new(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ActivateConfig {
    /// Number the items while the mode is on.
    pub badges: bool,
    /// How long the modifier is held before the dock shows the mode (the
    /// badges, the strip, the magnification), so quick switching shows
    /// nothing.
    #[serde(deserialize_with = "activate_delay")]
    pub delay: u32,
}

impl Default for ActivateConfig {
    fn default() -> Self {
        Self {
            badges: true,
            delay: 150,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AutohideConfig {
    pub enabled: bool,
    #[serde(deserialize_with = "de::duration")]
    pub reveal_delay: u32,
    #[serde(deserialize_with = "de::duration")]
    pub hide_delay: u32,
    /// Width of the edge reveal band in pixels.
    #[serde(deserialize_with = "autohide_trigger")]
    pub trigger: u32,
}

impl Default for AutohideConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            reveal_delay: 150,
            hide_delay: 500,
            trigger: 2,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct WindowsConfig {
    pub scope: Windows,
    #[serde(deserialize_with = "de::duration")]
    pub output_grace: u32,
    /// Clicking an app opens the strip instead of activating its latest window.
    pub strip: bool,
    #[serde(deserialize_with = "strip_min_windows")]
    pub min_windows: u32,
    /// Cards along the dock; 0 means unlimited.
    #[serde(deserialize_with = "strip_cap")]
    pub max_columns: u32,
    /// Rows away from the dock; 0 means unlimited.
    #[serde(deserialize_with = "strip_cap")]
    pub max_rows: u32,
    /// Fraction of output height.
    #[serde(deserialize_with = "de::fraction")]
    pub card_height: f64,
    /// Fraction of output length; also caps individual card width.
    #[serde(deserialize_with = "de::fraction")]
    pub max_width: f64,
    pub previews: Previews,
    #[serde(deserialize_with = "de::zoom")]
    pub zoom: f64,
}

impl Default for WindowsConfig {
    fn default() -> Self {
        Self {
            scope: Windows::default(),
            output_grace: 250,
            strip: true,
            min_windows: 2,
            max_columns: 3,
            max_rows: 3,
            card_height: 0.18,
            max_width: 0.6,
            previews: Previews::default(),
            zoom: 1.04,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LauncherConfig {
    #[serde(deserialize_with = "de::fraction")]
    pub width: f64,
    #[serde(deserialize_with = "launcher_icon_scale")]
    pub icon_scale: f64,
    #[serde(deserialize_with = "some_font_size")]
    pub font_size: Option<u32>,
    #[serde(deserialize_with = "launcher_matches")]
    pub matches: u32,
    #[serde(deserialize_with = "launcher_far_scale")]
    pub far_scale: f64,
    pub enter: LauncherEnter,
    pub history: bool,
    pub web: crate::engines::Web,
    /// File search (`[launcher.files]`).
    pub files: LauncherFiles,
    /// Window search (`[launcher.windows]`).
    pub windows: LauncherWindows,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            width: 0.4,
            icon_scale: 1.25,
            font_size: None,
            matches: 5,
            far_scale: 0.5,
            enter: LauncherEnter::default(),
            history: true,
            web: crate::engines::Web::default(),
            files: LauncherFiles::default(),
            windows: LauncherWindows::default(),
        }
    }
}

/// The launcher's window search.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LauncherWindows {
    /// Typed first, switches to window search; None: there is none.
    #[serde(deserialize_with = "files_trigger")]
    pub trigger: Option<char>,
}

impl Default for LauncherWindows {
    fn default() -> Self {
        Self { trigger: Some('@') }
    }
}

/// The launcher's file search.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct LauncherFiles {
    /// Typed first, switches to file search; None: there is none.
    #[serde(deserialize_with = "files_trigger")]
    pub trigger: Option<char>,
    /// The directory walked; `~/` expands.
    pub root: String,
    /// Gitignore-style globs, relative to the root.
    pub exclude: Vec<String>,
    /// Include hidden files and directories.
    pub hidden: bool,
    /// Honor `.gitignore`, `.ignore` and git's excludes.
    pub gitignore: bool,
    /// Entries walked at most.
    #[serde(deserialize_with = "files_max")]
    pub max: u32,
}

impl Default for LauncherFiles {
    fn default() -> Self {
        Self {
            trigger: Some('\''),
            root: "~".into(),
            exclude: ["node_modules", "target", "__pycache__"]
                .map(String::from)
                .to_vec(),
            hidden: false,
            gitignore: true,
            max: 1_000_000,
        }
    }
}

impl LauncherFiles {
    /// The root, with `~` expanded.
    pub fn root(&self) -> std::path::PathBuf {
        let root = self.root.trim();
        let root = match root {
            "~" => "~/",
            r => r,
        };
        wl_shell_render::icons::expand_home(root).into()
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct IntegrationConfig {
    /// Window commands accept `{id}`.
    pub activate_command: Option<String>,
    pub close_command: Option<String>,
    /// Accepts `{exec}`, `{desktop-id}`, and `{app-id}`.
    pub launch_command: Option<String>,
    pub terminal: String,
    /// Opens a terminal in a folder (the launcher's Open terminal here):
    /// `{dir}`. Empty: no such action.
    pub open_terminal_command: String,
    #[serde(deserialize_with = "launch_timeout")]
    pub launch_timeout: u32,
    pub mru_event_stream: Option<String>,
    pub output_event_stream: Option<String>,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            activate_command: None,
            close_command: None,
            launch_command: None,
            terminal: "foot".into(),
            open_terminal_command: "cd {dir} && exec foot".into(),
            launch_timeout: 10_000,
            mru_event_stream: None,
            output_event_stream: None,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FilesConfig {
    /// Opens files, folders, or `trash:///`: `{path}`.
    pub open_command: String,
    /// Drop commands accept `{paths}` and `{dest}`.
    pub copy_command: String,
    pub move_command: String,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            open_command: "xdg-open {path}".into(),
            copy_command: "cp -rn -- {paths} {dest}".into(),
            move_command: "mv -n -- {paths} {dest}".into(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TrashConfig {
    pub enabled: bool,
    /// Moves dropped files to the Trash: `{paths}`.
    pub command: String,
    pub empty_command: String,
}

impl Default for TrashConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "gio trash -- {paths}".into(),
            empty_command: "gio trash --empty".into(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Appearance {
    pub font: String,
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
    #[serde(deserialize_with = "response")]
    pub response: u32,
    // The shared parser validates theme keys, including dark/light overrides.
    #[serde(flatten)]
    pub theme: wl_shell_config::Appearance<Theme>,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            font: "sans-serif".into(),
            font_size: 13,
            response: 150,
            theme: wl_shell_config::Appearance::default(),
        }
    }
}

/// The screen edge the dock sits on.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    Bottom,
    Left,
    Right,
}

/// The layer-shell layer of the dock.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum DockLayer {
    #[default]
    Top,
    Overlay,
}

/// What marks running apps.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Indicator {
    /// One dot.
    #[default]
    Dot,
    /// One dot per window, at most three.
    Dots,
    None,
}

/// Which windows a dock shows.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Windows {
    /// Every window.
    #[default]
    All,
    /// The windows on its output, as `output-event-stream` reports them.
    Output,
}

/// Window previews in the strip.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Previews {
    /// Live while the strip is open.
    #[default]
    Live,
    /// One frame per window.
    Static,
    /// The app's icon and the titles only.
    Off,
}

/// What Enter does in the launcher.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum LauncherEnter {
    /// Activate the app's most recent window if it runs, else launch it.
    #[default]
    Activate,
    /// Always launch a new instance.
    Launch,
}

/// The outputs that get a dock: `"all"`, or a list of output names.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Outputs {
    #[default]
    All,
    Only(Vec<String>),
}

impl<'de> Deserialize<'de> for Outputs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Outputs;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("\"all\" or a list of output names")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Outputs, E> {
                match s {
                    "all" => Ok(Outputs::All),
                    _ => Err(E::custom(format!(
                        "expected \"all\" or a list of output names, got \"{s}\""
                    ))),
                }
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Outputs, A::Error> {
                let mut names = Vec::new();
                while let Some(name) = seq.next_element::<String>()? {
                    names.push(name);
                }
                Ok(Outputs::Only(names))
            }
        }
        d.deserialize_any(V)
    }
}

/// An `[output.<name>]` table: the layout keys, for one output.
#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct OutputOverride {
    /// False: no dock on this output.
    pub enabled: Option<bool>,
    pub position: Option<Position>,
    #[serde(deserialize_with = "some_icon_size")]
    pub icon_size: Option<u32>,
    #[serde(deserialize_with = "some_magnification")]
    pub magnification: Option<f64>,
    pub reserve: Option<bool>,
    #[serde(deserialize_with = "some_px")]
    pub margin: Option<u32>,
    pub autohide: OutputAutohide,
    pub windows: OutputWindows,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct OutputAutohide {
    pub enabled: Option<bool>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct OutputWindows {
    pub scope: Option<Windows>,
    #[serde(deserialize_with = "some_strip_cap")]
    pub max_columns: Option<u32>,
    #[serde(deserialize_with = "some_strip_cap")]
    pub max_rows: Option<u32>,
}

/// An `[app."<app-id>"]` table.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct AppConfig {
    /// Rows of the app's menu.
    pub actions: Vec<AppAction>,
    /// Runs on a middle click instead of launching a new instance.
    pub middle_click: Option<String>,
    /// Run on scrolling over the icon, one per wheel notch.
    pub scroll_up: Option<String>,
    pub scroll_down: Option<String>,
    /// An icon name or a path, instead of the desktop entry's.
    pub icon: Option<String>,
    /// The name to show instead of the desktop entry's.
    pub name: Option<String>,
    /// Launches the app instead of `launch-command` (or its desktop
    /// entry).
    pub launch_command: Option<String>,
    /// The desktop entry's actions in the menu.
    pub desktop_actions: bool,
    /// Opens files dropped on the icon: `{files}`, `{app-id}`.
    pub drop: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
            middle_click: None,
            scroll_up: None,
            scroll_down: None,
            icon: None,
            name: None,
            launch_command: None,
            desktop_actions: true,
            drop: None,
        }
    }
}

/// A `[folder."<path>"]` table.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FolderConfig {
    /// How the popup sorts the entries.
    pub sort: FolderSort,
    /// Entries the popup shows at most.
    #[serde(deserialize_with = "folder_count")]
    pub count: u32,
    /// How the popup shows them.
    pub view: FolderView,
    /// An icon name or a path, instead of the folder's.
    pub icon: Option<String>,
}

impl Default for FolderConfig {
    fn default() -> Self {
        Self {
            sort: FolderSort::default(),
            count: 10,
            view: FolderView::default(),
            icon: None,
        }
    }
}

/// How a folder's popup sorts its entries.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FolderSort {
    /// Most recently modified first.
    #[default]
    Modified,
    /// Most recently created first.
    Added,
    Name,
}

impl From<FolderSort> for files::Sort {
    fn from(s: FolderSort) -> Self {
        match s {
            FolderSort::Modified => files::Sort::Modified,
            FolderSort::Added => files::Sort::Added,
            FolderSort::Name => files::Sort::Name,
        }
    }
}

/// How a folder's popup shows its entries.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FolderView {
    /// A menu with a row per entry.
    List,
    /// Large icons with their names, fanned out of the folder's icon
    /// (stack.rs).
    #[default]
    Stack,
}

/// A row of an app's menu that runs a command.
#[derive(Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct AppAction {
    pub name: String,
    /// Run with `sh -c`: `{app-id}`, `{id}` (the most recent window) and
    /// `{ids}` (all of the app's windows) are filled in.
    pub command: String,
    /// An icon name or a path.
    pub icon: Option<String>,
    #[serde(default)]
    pub when: When,
}

/// When an app action's row shows.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum When {
    #[default]
    Always,
    /// Only while the app has windows.
    Running,
}

/// The layout of one output's dock: the global keys with its
/// `[output.<name>]` table over them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DockLayout {
    pub position: Position,
    pub icon_size: u32,
    pub magnification: f64,
    pub reserve: bool,
    pub autohide: bool,
    pub margin: u32,
    pub windows: Windows,
    pub strip_max_columns: u32,
    pub strip_max_rows: u32,
}

/// Colors: the keys of `[appearance]`, and of its `dark` and `light`
/// tables, which override them.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Theme {
    /// The dock's background.
    #[serde(deserialize_with = "de::color")]
    pub panel_color: u32,
    /// The panel's corner radius.
    #[serde(deserialize_with = "de::px")]
    pub panel_radius: u32,
    /// The dots under running apps.
    #[serde(deserialize_with = "de::color")]
    pub indicator_color: u32,
    /// The focused app's indicator.
    #[serde(deserialize_with = "de::color")]
    pub active_indicator_color: u32,
    /// The hovered app's name.
    #[serde(deserialize_with = "de::color")]
    pub label_color: u32,
    /// The pill behind it.
    #[serde(deserialize_with = "de::color")]
    pub label_background: u32,
    /// Window titles in the strip.
    #[serde(deserialize_with = "de::color")]
    pub title_color: u32,
    /// The pill behind them.
    #[serde(deserialize_with = "de::color")]
    pub title_background: u32,
    /// A strip card until its window's first frame arrives.
    #[serde(deserialize_with = "de::color")]
    pub placeholder: u32,
    /// Shadow color; alpha 0 disables shadows.
    #[serde(deserialize_with = "de::color")]
    pub shadow_color: u32,
    /// Shadow blur radius.
    #[serde(deserialize_with = "de::px")]
    pub shadow_softness: u32,
    /// How much the shadow box is grown (or shrunk, if negative).
    #[serde(deserialize_with = "de::signed_px")]
    pub shadow_spread: i32,
    /// How far the shadow is moved, `[x, y]`.
    #[serde(deserialize_with = "de::offset")]
    pub shadow_offset: [i32; 2],
    /// Icon shadow color; alpha 0 disables icon shadows.
    #[serde(deserialize_with = "de::color")]
    pub icon_shadow_color: u32,
    /// Icon shadow blur radius.
    #[serde(deserialize_with = "de::px")]
    pub icon_shadow_softness: u32,
    /// How far the icon shadow is moved, `[x, y]`.
    #[serde(deserialize_with = "de::offset")]
    pub icon_shadow_offset: [i32; 2],
    /// Icon theme (a directory name); default: gsettings, then hicolor.
    pub icon_theme: Option<String>,
    /// The menu's bubble.
    #[serde(deserialize_with = "de::color")]
    pub menu_background: u32,
    /// Its 1 px inner border.
    #[serde(deserialize_with = "de::color")]
    pub menu_border: u32,
    #[serde(deserialize_with = "de::px")]
    pub menu_radius: u32,
    /// The tail's base width and height, `[w, h]`.
    #[serde(deserialize_with = "menu_tail")]
    pub menu_tail: [u32; 2],
    #[serde(deserialize_with = "de::color")]
    pub menu_text: u32,
    /// Under the hovered row.
    #[serde(deserialize_with = "de::color")]
    pub menu_highlight: u32,
    /// The hovered row's text.
    #[serde(deserialize_with = "de::color")]
    pub menu_highlight_text: u32,
    /// The line before the dock's rows.
    #[serde(deserialize_with = "de::color")]
    pub menu_separator: u32,
    #[serde(deserialize_with = "menu_row_height")]
    pub menu_row_height: u32,
    /// Space around the rows.
    #[serde(deserialize_with = "de::px")]
    pub menu_padding: u32,
    /// Space between groups of rows.
    #[serde(deserialize_with = "de::px")]
    pub menu_group_gap: u32,
    /// The widest the menu gets; longer rows are cut.
    #[serde(deserialize_with = "menu_max_width")]
    pub menu_max_width: u32,
    /// The menu's shadow; alpha 0 disables it.
    #[serde(deserialize_with = "de::color")]
    pub menu_shadow_color: u32,
    #[serde(deserialize_with = "de::px")]
    pub menu_shadow_softness: u32,
    #[serde(deserialize_with = "de::offset")]
    pub menu_shadow_offset: [i32; 2],
    /// The line between the apps and the Trash.
    #[serde(deserialize_with = "de::color")]
    pub separator_color: u32,
    /// The launcher's bar.
    #[serde(deserialize_with = "de::color")]
    pub launcher_background: u32,
    /// The query.
    #[serde(deserialize_with = "de::color")]
    pub launcher_text: u32,
    /// The rest of the selected app's name, and the placeholder.
    #[serde(deserialize_with = "de::color")]
    pub launcher_completion: u32,
    /// What the query matched in the names and paths shown.
    #[serde(deserialize_with = "de::color")]
    pub launcher_match: u32,
    #[serde(deserialize_with = "de::color")]
    pub launcher_caret: u32,
    /// The digit of an item's badge during keyboard activation.
    #[serde(deserialize_with = "de::color")]
    pub badge_color: u32,
    /// The badge's dot.
    #[serde(deserialize_with = "de::color")]
    pub badge_background: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel_color: 0x262626b3,
            panel_radius: 16,
            indicator_color: 0xffffffb3,
            active_indicator_color: 0xffffffff,
            label_color: 0xffffffff,
            label_background: 0x262626e6,
            title_color: 0xffffffff,
            title_background: 0x000000b3,
            placeholder: 0x3a3a3aff,
            shadow_color: 0x00000066,
            shadow_softness: 24,
            shadow_spread: 0,
            shadow_offset: [0, 4],
            icon_shadow_color: 0x00000059,
            icon_shadow_softness: 6,
            icon_shadow_offset: [0, 2],
            icon_theme: None,
            menu_background: 0x262626eb,
            menu_border: 0xffffff1f,
            menu_radius: 12,
            menu_tail: [10, 7],
            menu_text: 0xffffffff,
            menu_highlight: 0xffffff2e,
            menu_highlight_text: 0xffffffff,
            menu_separator: 0xffffff26,
            menu_row_height: 28,
            menu_padding: 6,
            menu_group_gap: 8,
            menu_max_width: 320,
            menu_shadow_color: 0x00000066,
            menu_shadow_softness: 18,
            menu_shadow_offset: [0, 4],
            separator_color: 0xffffff40,
            launcher_background: 0x262626e6,
            launcher_text: 0xffffffff,
            // Adwaita's dark style: the dim opacity (55%), and the blue
            // accent's standalone color for the matches.
            launcher_completion: 0xffffff8c,
            launcher_match: 0x81d0ffff,
            launcher_caret: 0xffffffcc,
            badge_color: 0x1f1f1fff,
            badge_background: 0xffffffe6,
        }
    }
}

impl Config {
    /// The colors in use.
    pub fn theme(&self) -> &Theme {
        self.appearance.theme.theme()
    }

    /// The layout of the dock on the output named `name`; None if that
    /// output gets no dock.
    pub fn dock(&self, name: &str) -> Option<DockLayout> {
        let listed = match &self.outputs {
            Outputs::All => true,
            Outputs::Only(names) => names.iter().any(|n| n == name),
        };
        let o = self.output.get(name).cloned().unwrap_or_default();
        if !o.enabled.unwrap_or(listed) {
            return None;
        }
        Some(DockLayout {
            position: o.position.unwrap_or(self.position),
            icon_size: o.icon_size.unwrap_or(self.icon_size),
            magnification: o.magnification.unwrap_or(self.magnification),
            reserve: o.reserve.unwrap_or(self.reserve),
            autohide: o.autohide.enabled.unwrap_or(self.autohide.enabled),
            margin: o.margin.unwrap_or(self.margin),
            windows: o.windows.scope.unwrap_or(self.windows.scope),
            strip_max_columns: o.windows.max_columns.unwrap_or(self.windows.max_columns),
            strip_max_rows: o.windows.max_rows.unwrap_or(self.windows.max_rows),
        })
    }

    /// The panel shadow; None if shadows are disabled.
    pub fn shadow(&self) -> Option<shadow::Settings> {
        let t = self.theme();
        (t.shadow_color & 0xff != 0).then(|| shadow::Settings {
            color: t.shadow_color,
            softness: t.shadow_softness as f64,
            spread: t.shadow_spread,
            offset: (t.shadow_offset[0], t.shadow_offset[1]),
        })
    }

    /// The icon shadow; None if icon shadows are disabled.
    pub fn icon_shadow(&self) -> Option<shadow::IconShadow> {
        let t = self.theme();
        (t.icon_shadow_color & 0xff != 0).then(|| shadow::IconShadow {
            color: t.icon_shadow_color,
            softness: t.icon_shadow_softness,
            offset: (t.icon_shadow_offset[0], t.icon_shadow_offset[1]),
        })
    }

    /// The style of the hovered app's name; None if labels are disabled.
    pub fn label_style(&self) -> Option<text::Style> {
        self.labels.then(|| text::Style {
            font: self.appearance.font.clone(),
            size: self.appearance.font_size as f64,
            color: self.theme().label_color,
            background: self.theme().label_background,
        })
    }

    /// The style of a window's title pill in the strip.
    pub fn title_style(&self) -> text::Style {
        text::Style {
            font: self.appearance.font.clone(),
            size: self.appearance.font_size as f64,
            color: self.theme().title_color,
            background: self.theme().title_background,
        }
    }

    /// The style of the badges' digits (on their dot, without a pill);
    /// None if badges are off.
    pub fn badge_style(&self) -> Option<text::Style> {
        self.activate.badges.then(|| text::Style {
            font: self.appearance.font.clone(),
            size: (self.appearance.font_size as f64 * 0.85).round().max(8.0),
            color: self.theme().badge_color,
            background: 0,
        })
    }

    /// The settings of `app_id`, if it has an `[app.<id>]` table.
    pub fn app(&self, app_id: &str) -> Option<&AppConfig> {
        self.app.get(app_id)
    }

    /// The settings of the folder pinned as `pin` (keys and pins match
    /// with `~/` expanded and trailing slashes dropped).
    pub fn folder(&self, pin: &str) -> Option<&FolderConfig> {
        let norm = |p: &str| {
            let p = wl_shell_render::icons::expand_home(p.trim());
            match p.trim_end_matches('/') {
                "" => "/".to_string(),
                t => t.to_string(),
            }
        };
        let pin = norm(pin);
        (self.folder.iter()).find_map(|(k, f)| (norm(k) == pin).then_some(f))
    }

    /// The menu's shadow; None if disabled.
    pub fn menu_shadow(&self) -> Option<shadow::Settings> {
        let t = self.theme();
        (t.menu_shadow_color & 0xff != 0).then(|| shadow::Settings {
            color: t.menu_shadow_color,
            softness: t.menu_shadow_softness as f64,
            spread: 0,
            offset: (t.menu_shadow_offset[0], t.menu_shadow_offset[1]),
        })
    }

    /// The style of the menu's rows (no background: the bubble has it).
    pub fn menu_text_style(&self) -> text::Style {
        text::Style {
            font: self.appearance.font.clone(),
            size: self.appearance.font_size as f64,
            color: self.theme().menu_text,
            background: 0,
        }
    }

    /// The launcher's query font size.
    pub fn launcher_font_size(&self) -> f64 {
        match self.launcher.font_size {
            Some(size) => size as f64,
            None => (self.icon_size as f64 * 0.42)
                .round()
                .max(self.appearance.font_size as f64),
        }
    }
}

impl ConfigFile for Config {
    const DIR: &'static str = "wl-dock";

    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme) {
        self.appearance.theme.apply(desktop);
    }
}

/// App icon size at rest.
fn icon_size<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 16, 256)
}

fn some_icon_size<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    icon_size(d).map(Some)
}

fn some_px<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    de::px(d).map(Some)
}

/// Magnification of the hovered icon, 1 to 3.
fn magnification<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    ranged(d, 1.0, 3.0)
}

fn some_magnification<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    magnification(d).map(Some)
}

/// Icons on each side that grow, 1 to 10.
fn magnification_range<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    ranged(d, 1.0, 10.0)
}

/// A number from `min` to `max`.
fn ranged<'de, D: Deserializer<'de>>(d: D, min: f64, max: f64) -> Result<f64, D::Error> {
    use serde::de::Error;
    let v = f64::deserialize(d)?;
    if (min..=max).contains(&v) {
        Ok(v)
    } else {
        Err(D::Error::custom(format!(
            "expected {min} to {max}, got {v}"
        )))
    }
}

/// Windows an app needs for its strip, 1 to 100.
fn strip_min_windows<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 100)
}

/// Strip columns or rows at most, 0 (no limit) to 100.
fn strip_cap<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 100)
}

fn some_strip_cap<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    strip_cap(d).map(Some)
}

/// Bouncing at most, in milliseconds.
fn launch_timeout<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 60_000)
}

/// Pixels before a press drags, 1 to 100.
fn drag_threshold<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 100)
}

/// Entries in a folder's popup, 1 to 100.
fn folder_count<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 100)
}

/// The autohide trigger band, 1 to 50 px.
fn autohide_trigger<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 50)
}

/// Spring response, in milliseconds.
fn response<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 10_000)
}

/// Keyboard activation's delay, 0 to 2000 ms.
fn activate_delay<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    use serde::de::Error;
    let v = de::duration(d)?;
    match v <= 2000 {
        true => Ok(v),
        false => Err(D::Error::custom(format!("expected 0 to 2000 ms, got {v}"))),
    }
}

/// A menu row's height, 12 to 200.
fn menu_row_height<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 12, 200)
}

/// The menu's widest, 80 to 4000.
fn menu_max_width<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 80, 4000)
}

/// `[w, h]` of the menu's tail, each 0 to 100.
fn menu_tail<'de, D: Deserializer<'de>>(d: D) -> Result<[u32; 2], D::Error> {
    use serde::de::Error;
    let [w, h] = <[i64; 2]>::deserialize(d)?;
    let check = |v: i64| {
        u32::try_from(v)
            .ok()
            .filter(|&v| v <= 100)
            .ok_or_else(|| D::Error::custom(format!("expected 0-100, got {v}")))
    };
    Ok([check(w)?, check(h)?])
}

fn some_font_size<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    de::font_size(d).map(Some)
}

/// The launcher's icon scale, 1 to 3.
fn launcher_icon_scale<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    ranged(d, 1.0, 3.0)
}

/// Matches on the drum, 0 to 12.
fn launcher_matches<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 12)
}

/// The farthest match's scale, 0.25 to 1.
fn launcher_far_scale<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    ranged(d, 0.25, 1.0)
}

/// The file search's trigger: one character, not a space; `""` turns file
/// search off.
pub(crate) fn files_trigger<'de, D: Deserializer<'de>>(d: D) -> Result<Option<char>, D::Error> {
    use serde::de::Error;
    let s = String::deserialize(d)?;
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (None, _) => Ok(None),
        (Some(c), None) if !c.is_whitespace() && !c.is_control() => Ok(Some(c)),
        _ => Err(D::Error::custom(format!(
            "expected one character that isn't a space, or \"\", got {s:?}"
        ))),
    }
}

/// Entries the file search walks at most, 1 to 100 million.
fn files_max<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 100_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config, String> {
        <Config as ConfigFile>::parse(s)
    }

    #[test]
    fn empty_is_default() {
        assert_eq!(parse(""), Ok(Config::default()));
    }

    #[test]
    fn full() {
        let c = parse(
            r##"
            outputs = ["DP-1", "eDP-1"]
            position = "left"
            layer = "overlay"
            icon-size = 64
            icon-gap = 4
            margin = 0
            padding = 8
            magnification = 2.5
            magnification-range = 2
            reserve = false
            hover-delay = 100
            labels = false
            indicator = "dots"
            active-indicator = false

            [autohide]
            enabled = true
            reveal-delay = 0
            hide-delay = 1000
            trigger = 4

            [integration]
            activate-command = "niri msg action focus-window --id {id}"
            launch-command = "uwsm app -- {desktop-id}"
            terminal = "alacritty -e"
            open-terminal-command = "alacritty --working-directory {dir}"
            launch-timeout = 5000
            mru-event-stream = "focus.sh"
            output-event-stream = "outputs.sh"

            [windows]
            scope = "output"
            output-grace = 100
            strip = false
            min-windows = 3
            max-columns = 4
            max-rows = 0
            card-height = 0.25
            max-width = 0.5
            previews = "static"
            zoom = 1

            [appearance]
            response = 200
            font = "Inter"
            font-size = 12
            panel-color = "00000080"
            panel-radius = 20
            indicator-color = "ffffff"
            label-color = "eeeeee"
            label-background = "00000080"
            icon-theme = "Papirus"
            title-color = "eeeeee"
            title-background = "00000080"
            placeholder = "#222222"
            "##,
        )
        .unwrap();
        assert_eq!(
            c.outputs,
            Outputs::Only(vec!["DP-1".into(), "eDP-1".into()])
        );
        assert_eq!((c.position, c.layer), (Position::Left, DockLayer::Overlay));
        assert_eq!(
            (c.icon_size, c.icon_gap, c.margin, c.padding),
            (64, 4, 0, 8)
        );
        assert_eq!((c.magnification, c.magnification_range), (2.5, 2.0));
        assert!(!c.reserve && !c.labels && c.label_style().is_none());
        assert!(c.autohide.enabled);
        assert_eq!(
            (
                c.autohide.reveal_delay,
                c.autohide.hide_delay,
                c.autohide.trigger
            ),
            (0, 1000, 4)
        );
        assert_eq!((c.hover_delay, c.appearance.response), (100, 200));
        assert_eq!(c.indicator, Indicator::Dots);
        assert_eq!(
            (c.appearance.font.as_str(), c.appearance.font_size),
            ("Inter", 12)
        );
        assert!(!c.active_indicator);
        assert_eq!(
            c.integration.activate_command.as_deref(),
            Some("niri msg action focus-window --id {id}")
        );
        assert_eq!(
            c.integration.launch_command.as_deref(),
            Some("uwsm app -- {desktop-id}")
        );
        assert_eq!(
            (
                c.integration.terminal.as_str(),
                c.integration.launch_timeout
            ),
            ("alacritty -e", 5000)
        );
        assert_eq!(
            c.integration.open_terminal_command,
            "alacritty --working-directory {dir}"
        );
        assert_eq!(c.integration.mru_event_stream.as_deref(), Some("focus.sh"));
        assert_eq!(
            c.integration.output_event_stream.as_deref(),
            Some("outputs.sh")
        );
        assert_eq!(
            (c.windows.scope, c.windows.output_grace),
            (Windows::Output, 100)
        );
        assert!(!c.windows.strip);
        assert_eq!(
            (
                c.windows.min_windows,
                c.windows.max_columns,
                c.windows.max_rows
            ),
            (3, 4, 0)
        );
        assert_eq!((c.windows.card_height, c.windows.max_width), (0.25, 0.5));
        assert_eq!(
            (c.windows.previews, c.windows.zoom),
            (Previews::Static, 1.0)
        );
        let t = c.theme();
        assert_eq!((t.panel_color, t.indicator_color), (0x00000080, 0xffffffff));
        assert_eq!(
            (t.label_color, t.label_background),
            (0xeeeeeeff, 0x00000080)
        );
        assert_eq!(t.panel_radius, 20);
        assert_eq!(t.icon_theme.as_deref(), Some("Papirus"));
        assert_eq!(
            (t.title_color, t.title_background, t.placeholder),
            (0xeeeeeeff, 0x00000080, 0x222222ff)
        );
    }

    #[test]
    fn apps() {
        let c = parse(
            r#"
            [integration]
            close-command = "niri msg action close-window --id {id}"

            [app.spotify]
            actions = [
              { name = "Play/Pause", command = "playerctl play-pause", icon = "media-playback-start" },
              { name = "Next", command = "playerctl next", when = "running" },
            ]
            middle-click = "playerctl play-pause"
            scroll-up = "playerctl next"
            scroll-down = "playerctl previous"

            [app."org.gnome.Nautilus"]
            icon = "~/.local/share/icons/files.svg"
            name = "Files"
            launch-command = "nautilus --new-window"
            desktop-actions = false
            "#,
        )
        .unwrap();
        assert_eq!(
            c.integration.close_command.as_deref(),
            Some("niri msg action close-window --id {id}")
        );
        let s = c.app("spotify").unwrap();
        assert_eq!(s.actions.len(), 2);
        assert_eq!(s.actions[0].icon.as_deref(), Some("media-playback-start"));
        assert_eq!(
            (s.actions[0].when, s.actions[1].when),
            (When::Always, When::Running)
        );
        assert_eq!(s.middle_click.as_deref(), Some("playerctl play-pause"));
        assert_eq!(
            (s.scroll_up.as_deref(), s.scroll_down.as_deref()),
            (Some("playerctl next"), Some("playerctl previous"))
        );
        assert!(s.desktop_actions && s.icon.is_none());
        let n = c.app("org.gnome.Nautilus").unwrap();
        assert_eq!(n.name.as_deref(), Some("Files"));
        assert_eq!(n.icon.as_deref(), Some("~/.local/share/icons/files.svg"));
        assert_eq!(n.launch_command.as_deref(), Some("nautilus --new-window"));
        assert!(!n.desktop_actions);
        assert!(c.app("firefox").is_none());
    }

    #[test]
    fn dragging_and_the_trash() {
        let d = Config::default();
        assert_eq!((d.drag_threshold, d.trash.enabled), (6, true));
        assert_eq!(d.files.open_command, "xdg-open {path}");
        assert_eq!(d.trash.empty_command, "gio trash --empty");
        let c = parse(
            r#"
            drag-threshold = 10
            [trash]
            enabled = false
            empty-command = "trash-empty"

            [files]
            open-command = "gio open {path}"

            [appearance]
            separator-color = "ff000080"
            "#,
        )
        .unwrap();
        assert_eq!((c.drag_threshold, c.trash.enabled), (10, false));
        assert_eq!(c.files.open_command, "gio open {path}");
        assert_eq!(c.trash.empty_command, "trash-empty");
        assert_eq!(c.theme().separator_color, 0xff000080);
    }

    #[test]
    fn drops_and_folders() {
        let d = Config::default();
        assert_eq!(d.trash.command, "gio trash -- {paths}");
        assert!(d.files.copy_command.contains("{dest}") && d.files.move_command.contains("{dest}"));
        let c = parse(
            r#"
            [trash]
            command = "trash-put -- {paths}"

            [files]
            copy-command = "gio copy -- {paths} {dest}"
            move-command = "gio move -- {paths} {dest}"

            [app.mpv]
            drop = "mpv --player-operation-mode=pseudo-gui -- {files}"

            [folder."~/Downloads/"]
            sort = "added"
            count = 30
            view = "list"
            icon = "folder-download"

            [folder."/srv"]
            "#,
        )
        .unwrap();
        assert_eq!(c.trash.command, "trash-put -- {paths}");
        assert_eq!(c.files.copy_command, "gio copy -- {paths} {dest}");
        assert_eq!(c.files.move_command, "gio move -- {paths} {dest}");
        assert!(c.app("mpv").unwrap().drop.is_some());
        assert!(c.app("x").is_none_or(|a| a.drop.is_none()));
        // Matched with `~/` expanded and without trailing slashes.
        let dl = c.folder("~/Downloads").unwrap();
        assert_eq!(
            (dl.sort, dl.count, dl.view),
            (FolderSort::Added, 30, FolderView::List)
        );
        assert_eq!(dl.icon.as_deref(), Some("folder-download"));
        if let Ok(home) = std::env::var("HOME") {
            assert!(c.folder(&format!("{home}/Downloads")).is_some());
        }
        assert_eq!(c.folder("/srv/").unwrap(), &FolderConfig::default());
        assert!(c.folder("/srv/x").is_none());
        let err = |s: &str| parse(s).is_err();
        assert!(err("[folder.x]\ncount = 0"));
        assert!(err("[folder.x]\nsort = \"size\""));
        assert!(err("[folder.x]\nview = \"fan\""));
        let stack = parse("[folder.x]\nview = \"stack\"").unwrap();
        assert_eq!(stack.folder("x").unwrap().view, FolderView::Stack);
        assert!(err("[folder.x]\nnope = 1"));
    }

    #[test]
    fn menu_appearance() {
        let c = parse(
            r#"
            [appearance]
            menu-background = "101010cc"
            menu-radius = 8
            menu-tail = [12, 6]
            menu-row-height = 24
            menu-max-width = 400
            menu-shadow-color = "00000000"

            [appearance.light]
            menu-text = "000000"
            "#,
        )
        .unwrap();
        let t = &c.appearance.theme.dark;
        assert_eq!((t.menu_background, t.menu_radius), (0x101010cc, 8));
        assert_eq!(t.menu_tail, [12, 6]);
        assert_eq!((t.menu_row_height, t.menu_max_width), (24, 400));
        assert_eq!((t.menu_padding, t.menu_group_gap), (6, 8));
        assert_eq!(t.menu_text, 0xffffffff);
        assert_eq!(c.appearance.theme.light.menu_text, 0x000000ff);
        assert!(c.menu_shadow().is_none());
        assert!(Config::default().menu_shadow().is_some());
    }

    #[test]
    fn launcher() {
        let d = Config::default();
        assert_eq!((d.launcher.width, d.launcher.icon_scale), (0.4, 1.25));
        assert_eq!((d.launcher.matches, d.launcher.far_scale), (5, 0.5));
        assert_eq!(
            (d.launcher.enter, d.launcher.history),
            (LauncherEnter::Activate, true)
        );
        assert_eq!(d.launcher_font_size(), 20.0);
        let c = parse(
            r#"
            [launcher]
            width = 0.5
            icon-scale = 1.5
            font-size = 24
            matches = 0
            far-scale = 0.75
            enter = "launch"
            history = false

            [appearance]
            launcher-background = "000000"
            launcher-caret = "ff0000"
            "#,
        )
        .unwrap();
        assert_eq!((c.launcher.width, c.launcher.icon_scale), (0.5, 1.5));
        assert_eq!((c.launcher_font_size(), c.launcher.matches), (24.0, 0));
        assert_eq!(c.launcher.far_scale, 0.75);
        assert_eq!(
            (c.launcher.enter, c.launcher.history),
            (LauncherEnter::Launch, false)
        );
        let t = c.theme();
        assert_eq!(
            (t.launcher_background, t.launcher_caret),
            (0x000000ff, 0xff0000ff)
        );
        assert_eq!(t.launcher_text, 0xffffffff);
        let err = |s: &str| parse(s).is_err();
        assert!(err("[launcher]\nwidth = 0"));
        assert!(err("[launcher]\nmatches = 13"));
        assert!(err("[launcher]\nfar-scale = 0.1"));
        assert!(err("[launcher]\nicon-scale = 0.5"));
        assert!(err("[launcher]\nenter = \"open\""));
    }

    #[test]
    fn badges() {
        let d = Config::default();
        assert!(d.activate.badges);
        let s = d.badge_style().unwrap();
        assert_eq!((s.size, s.color, s.background), (11.0, 0x1f1f1fff, 0));
        let c = parse(
            r#"
            [activate]
            badges = false
            delay = 0
            [appearance]
            badge-color = "ffffff"
            badge-background = "e0383b"
            "#,
        )
        .unwrap();
        assert!(!c.activate.badges && c.badge_style().is_none());
        assert_eq!((d.activate.delay, c.activate.delay), (150, 0));
        assert!(parse("[activate]\ndelay = 5000").is_err());
        assert!(parse("badges = true").is_err());
        let t = c.theme();
        assert_eq!(
            (t.badge_color, t.badge_background),
            (0xffffffff, 0xe0383bff)
        );
        assert!(parse("[appearance]\nbadge-color = \"red\"").is_err());
    }

    #[test]
    fn file_search() {
        let d = Config::default().launcher.files;
        assert_eq!((d.trigger, d.root.as_str()), (Some('\''), "~"));
        assert_eq!(d.exclude, ["node_modules", "target", "__pycache__"]);
        assert!(!d.hidden && d.gitignore && d.max == 1_000_000);
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(d.root(), std::path::Path::new(&home));
        }
        let c = parse(
            r#"
            [launcher.files]
            trigger = "/"
            root = "/srv"
            exclude = ["*.o"]
            hidden = true
            gitignore = false
            max = 5000
            "#,
        )
        .unwrap()
        .launcher
        .files;
        assert_eq!((c.trigger, c.root().to_str()), (Some('/'), Some("/srv")));
        assert_eq!(c.exclude, ["*.o"]);
        assert!(c.hidden && !c.gitignore && c.max == 5000);
        let off = parse("[launcher.files]\ntrigger = \"\"").unwrap();
        assert_eq!(off.launcher.files.trigger, None);
        let err = |s: &str| parse(s).is_err();
        assert!(err("[launcher.files]\ntrigger = \" \""));
        assert!(err("[launcher.files]\ntrigger = \"ab\""));
        assert!(err("[launcher.files]\nmax = 0"));
        assert!(err("[launcher.files]\nunknown = 1"));
        assert!(err("files-trigger = \"'\""));
    }

    #[test]
    fn window_search() {
        let d = Config::default().launcher.windows;
        assert_eq!(d.trigger, Some('@'));
        let c = parse("[launcher.windows]\ntrigger = \"/\"").unwrap();
        assert_eq!(c.launcher.windows.trigger, Some('/'));
        let off = parse("[launcher.windows]\ntrigger = \"\"").unwrap();
        assert_eq!(off.launcher.windows.trigger, None);
        let err = |s: &str| parse(s).is_err();
        assert!(err("[launcher.windows]\ntrigger = \" \""));
        assert!(err("[launcher.windows]\ntrigger = \"ab\""));
        assert!(err("[launcher.windows]\nunknown = 1"));
    }

    #[test]
    fn per_output() {
        let c = parse(
            r#"
            position = "bottom"
            icon-size = 40

            [output.eDP-1]
            position = "left"
            magnification = 1
            margin = 2
            [output.eDP-1.autohide]
            enabled = true
            [output.eDP-1.windows]
            scope = "output"
            max-rows = 1

            [output.HDMI-A-1]
            enabled = false
            "#,
        )
        .unwrap();
        let global = c.dock("DP-1").unwrap();
        assert_eq!(global.position, Position::Bottom);
        assert_eq!((global.icon_size, global.magnification), (40, 1.8));
        let edp = c.dock("eDP-1").unwrap();
        assert_eq!(edp.position, Position::Left);
        assert_eq!((edp.icon_size, edp.magnification, edp.margin), (40, 1.0, 2));
        assert!(edp.reserve && edp.autohide && !global.autohide);
        assert_eq!(
            (global.windows, edp.windows),
            (Windows::All, Windows::Output)
        );
        assert_eq!((global.strip_max_columns, global.strip_max_rows), (3, 3));
        assert_eq!((edp.strip_max_columns, edp.strip_max_rows), (3, 1));
        assert!(c.dock("HDMI-A-1").is_none());
    }

    #[test]
    fn listed_outputs_only() {
        let c = parse(
            r#"
            outputs = ["DP-1"]
            [output.eDP-1]
            enabled = true
            [output.DP-1]
            reserve = false
            "#,
        )
        .unwrap();
        assert!(!c.dock("DP-1").unwrap().reserve);
        assert!(c.dock("eDP-1").is_some());
        assert!(c.dock("HDMI-A-1").is_none());
        assert_eq!(parse("outputs = \"all\"").unwrap().outputs, Outputs::All);
    }

    #[test]
    fn sections_are_optional_and_strict() {
        let sections = [
            "activate",
            "autohide",
            "windows",
            "launcher",
            "integration",
            "files",
            "trash",
            "appearance",
        ];
        let empty = sections
            .iter()
            .map(|name| format!("[{name}]\n"))
            .collect::<String>();
        assert_eq!(parse(&empty), Ok(Config::default()));
        for section in sections {
            assert!(
                parse(&format!("[{section}]\nunknown = 1")).is_err(),
                "{section}"
            );
            assert!(parse(&format!("{section} = 1")).is_err(), "{section}");
        }
        for field in [
            "[output.X.autohide]\nreveal-delay = 10",
            "[output.X.windows]\npreviews = 'off'",
            "[output.X.windows]\nmax-columns = 101",
            "[output.X.launcher]\nwidth = 0.5",
            "[appearance]\nfont-size = 0",
            "[appearance]\nresponse = 0",
            "[files]\nopen-command = false",
            "[integration]\nterminal = false",
        ] {
            assert!(parse(field).is_err(), "{field}");
        }
    }

    #[test]
    fn old_flat_keys_are_rejected() {
        for old in [
            "autohide = true",
            "reveal-delay = 150",
            "hide-delay = 500",
            "autohide-trigger = 2",
            "windows = 'all'",
            "output-grace = 250",
            "strip = true",
            "strip-min-windows = 2",
            "strip-max-columns = 3",
            "strip-max-rows = 3",
            "strip-card-height = 0.18",
            "strip-max-width = 0.6",
            "previews = 'live'",
            "zoom = 1.04",
            "launcher-width = 0.4",
            "launcher-icon-scale = 1.25",
            "launcher-font-size = 20",
            "launcher-matches = 5",
            "launcher-far-scale = 0.5",
            "launcher-enter = 'activate'",
            "history = true",
            "activate-command = 'focus {id}'",
            "close-command = 'close {id}'",
            "launch-command = 'launch {exec}'",
            "terminal = 'foot'",
            "launch-timeout = 10000",
            "mru-event-stream = 'focus.sh'",
            "output-event-stream = 'outputs.sh'",
            "open-command = 'xdg-open {path}'",
            "copy-command = 'cp {paths} {dest}'",
            "move-command = 'mv {paths} {dest}'",
            "trash = true",
            "trash-command = 'gio trash {paths}'",
            "trash-empty-command = 'gio trash --empty'",
            "font = 'sans-serif'",
            "font-size = 13",
            "response = 150",
            "[output.X]\nautohide = true",
            "[output.X]\nwindows = 'all'",
            "[output.X]\nstrip-max-columns = 3",
            "[output.X]\nstrip-max-rows = 3",
        ] {
            assert!(parse(old).is_err(), "accepted old key: {old}");
        }
        assert!(parse("launcher-width = 0.4\n[launcher]\nwidth = 0.5").is_err());
    }

    #[test]
    fn readme_config_parses() {
        let readme = include_str!("../README.md");
        let mut config = String::new();
        for block in readme.split("```toml\n").skip(1) {
            config.push_str(block.split("```").next().unwrap());
            config.push('\n');
        }
        parse(&config).unwrap();
    }

    #[test]
    fn example_config_parses() {
        let example = include_str!("../config.example.toml");
        // As shipped, everything is commented out: the defaults.
        assert_eq!(parse(example), Ok(Config::default()));
        // Settings are commented as `#key`, prose as `# text`; uncommenting
        // every setting must still be a valid config.
        let uncommented = (example.lines())
            .map(|l| match l.strip_prefix('#') {
                Some(rest) if !rest.is_empty() && !rest.starts_with(' ') => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        parse(&uncommented).unwrap();
    }

    #[test]
    fn invalid() {
        let err = |s: &str| parse(s).is_err();
        assert!(err("position = \"top-ish\""));
        assert!(err("layer = \"bottom\""));
        assert!(err("icon-size = 8"));
        assert!(err("icon-size = 1000"));
        assert!(err("magnification = 0.5"));
        assert!(err("magnification = 4"));
        assert!(err("magnification-range = 0"));
        assert!(err("indicator = \"bar\""));
        assert!(err("[windows]\nscope = \"some\""));
        assert!(err("[integration]\nlaunch-timeout = 100000"));
        assert!(err("[autohide]\ntrigger = 0"));
        assert!(err("[output.X]\nhide-delay = 1"));
        assert!(err("[windows]\nmin-windows = 0"));
        assert!(err("drag-threshold = 0"));
        assert!(err("[trash]\nenabled = \"yes\""));
        assert!(err("strip-direction = \"vertical\""));
        assert!(err("[windows]\nmax-rows = 101"));
        assert!(err("[windows]\nmax-columns = -1"));
        assert!(err("[windows]\npreviews = \"sometimes\""));
        assert!(err("[windows]\nzoom = 0.9"));
        assert!(err("[windows]\ncard-height = 2"));
        assert!(err("outputs = \"some\""));
        assert!(err("outputs = 1"));
        assert!(err("unknown = 1"));
        assert!(err("[app.x]\nunknown = 1"));
        assert!(err("[app.x]\nactions = [{ name = \"a\" }]"));
        assert!(err(
            "[app.x]\nactions = [{ name = \"a\", command = \"b\", when = \"never\" }]"
        ));
        assert!(err("[appearance]\nmenu-tail = [10]"));
        assert!(err("[appearance]\nmenu-tail = [10, 101]"));
        assert!(err("[appearance]\nmenu-row-height = 4"));
        assert!(err("[appearance]\nicon-size = 1"));
        assert!(err("[output.X]\nicon-size = 8"));
        assert!(err("[output.X]\nfont = \"Inter\""));
        assert!(
            parse("[appearance]\nshadow-color = \"00000000\"")
                .unwrap()
                .shadow()
                .is_none()
        );
    }
}
