//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-dock/config.toml` (default
//! `~/.config/wl-dock/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! Layout keys can be overridden per output in `[output.<name>]` tables
//! (see `OutputOverride`); `outputs` picks the outputs that get a dock.
//!
//! Colors live in `[appearance]`; `[appearance.dark]` and
//! `[appearance.light]` override its keys in the desktop's dark or light
//! mode (or the one `mode` forces).

use std::collections::BTreeMap;
use std::fmt;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use wl_shell_config::{ConfigFile, Scheme, de};
use wl_shell_render::{shadow, text};

pub type Appearance = wl_shell_config::Appearance<Theme>;

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// The outputs that get a dock.
    pub outputs: Outputs,
    /// The screen edge the dock sits on.
    pub position: Position,
    /// The layer-shell layer of the dock.
    pub layer: DockLayer,
    /// App icon size at rest.
    #[serde(deserialize_with = "icon_size")]
    pub icon_size: u32,
    /// Space between icons.
    #[serde(deserialize_with = "de::px")]
    pub icon_gap: u32,
    /// Space between the dock and its screen edge.
    #[serde(deserialize_with = "de::px")]
    pub margin: u32,
    /// Space between the panel's edges and the icons.
    #[serde(deserialize_with = "de::px")]
    pub padding: u32,
    /// Scale of the icon under the pointer; 1 disables magnification.
    #[serde(deserialize_with = "magnification")]
    pub magnification: f64,
    /// How many icons on each side of the pointer grow with it.
    #[serde(deserialize_with = "magnification_range")]
    pub magnification_range: f64,
    /// Keep windows out of the resting dock's area (exclusive zone).
    pub reserve: bool,
    /// Milliseconds the pointer rests on an app before its name shows.
    #[serde(deserialize_with = "de::duration")]
    pub hover_delay: u32,
    /// The hovered app's name.
    pub labels: bool,
    /// What marks running apps.
    pub indicator: Indicator,
    /// Mark the focused app (needs `mru-event-stream`).
    pub active_indicator: bool,
    /// Spring response for movement, in milliseconds.
    #[serde(deserialize_with = "response")]
    pub response: u32,
    /// Font family for labels (and letter icons).
    pub font: String,
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
    /// Focuses a window: `{id}` is its ext toplevel identifier.
    pub activate_command: Option<String>,
    /// Launches apps instead of their desktop entry's `Exec=`: `{exec}`,
    /// `{desktop-id}`, `{app-id}`.
    pub launch_command: Option<String>,
    /// Runs `Terminal=true` entries: their command is appended.
    pub terminal: String,
    /// Milliseconds a launched app's icon bounces at most.
    #[serde(deserialize_with = "launch_timeout")]
    pub launch_timeout: u32,
    /// Prints the ID of each window that gains focus, one per line.
    pub mru_event_stream: Option<String>,
    /// Prints `<output> <id>` whenever a window opens on or moves to an
    /// output.
    pub output_event_stream: Option<String>,
    /// Which windows a dock shows.
    pub windows: Windows,
    /// Milliseconds a new window the output stream hasn't placed yet is
    /// held back.
    #[serde(deserialize_with = "de::duration")]
    pub output_grace: u32,
    /// Per-output overrides, by output name.
    pub output: BTreeMap<String, OutputOverride>,
    pub appearance: Appearance,
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
            response: 150,
            font: "sans-serif".into(),
            font_size: 13,
            activate_command: None,
            launch_command: None,
            terminal: "foot".into(),
            launch_timeout: 10_000,
            mru_event_stream: None,
            output_event_stream: None,
            windows: Windows::default(),
            output_grace: 250,
            output: BTreeMap::new(),
            appearance: Appearance::default(),
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
    pub windows: Option<Windows>,
}

/// The layout of one output's dock: the global keys with its
/// `[output.<name>]` table over them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DockLayout {
    pub position: Position,
    pub icon_size: u32,
    pub magnification: f64,
    pub reserve: bool,
    pub margin: u32,
    pub windows: Windows,
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
            shadow_color: 0x00000066,
            shadow_softness: 24,
            shadow_spread: 0,
            shadow_offset: [0, 4],
            icon_shadow_color: 0x00000059,
            icon_shadow_softness: 6,
            icon_shadow_offset: [0, 2],
            icon_theme: None,
        }
    }
}

impl Config {
    /// The colors in use.
    pub fn theme(&self) -> &Theme {
        self.appearance.theme()
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
            margin: o.margin.unwrap_or(self.margin),
            windows: o.windows.unwrap_or(self.windows),
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
            font: self.font.clone(),
            size: self.font_size as f64,
            color: self.theme().label_color,
            background: self.theme().label_background,
        })
    }
}

impl ConfigFile for Config {
    const DIR: &'static str = "wl-dock";

    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme) {
        self.appearance.apply(desktop);
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

/// Bouncing at most, in milliseconds.
fn launch_timeout<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 60_000)
}

/// Spring response, in milliseconds.
fn response<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 10_000)
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
            response = 200
            font = "Inter"
            font-size = 12
            active-indicator = false
            activate-command = "niri msg action focus-window --id {id}"
            launch-command = "uwsm app -- {desktop-id}"
            terminal = "alacritty -e"
            launch-timeout = 5000
            mru-event-stream = "focus.sh"
            output-event-stream = "outputs.sh"
            windows = "output"
            output-grace = 100

            [appearance]
            panel-color = "00000080"
            panel-radius = 20
            indicator-color = "ffffff"
            label-color = "eeeeee"
            label-background = "00000080"
            icon-theme = "Papirus"
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
        assert_eq!((c.hover_delay, c.response), (100, 200));
        assert_eq!(c.indicator, Indicator::Dots);
        assert_eq!((c.font.as_str(), c.font_size), ("Inter", 12));
        assert!(!c.active_indicator);
        assert_eq!(
            c.activate_command.as_deref(),
            Some("niri msg action focus-window --id {id}")
        );
        assert_eq!(
            c.launch_command.as_deref(),
            Some("uwsm app -- {desktop-id}")
        );
        assert_eq!(
            (c.terminal.as_str(), c.launch_timeout),
            ("alacritty -e", 5000)
        );
        assert_eq!(c.mru_event_stream.as_deref(), Some("focus.sh"));
        assert_eq!(c.output_event_stream.as_deref(), Some("outputs.sh"));
        assert_eq!((c.windows, c.output_grace), (Windows::Output, 100));
        let t = c.theme();
        assert_eq!((t.panel_color, t.indicator_color), (0x00000080, 0xffffffff));
        assert_eq!(
            (t.label_color, t.label_background),
            (0xeeeeeeff, 0x00000080)
        );
        assert_eq!(t.panel_radius, 20);
        assert_eq!(t.icon_theme.as_deref(), Some("Papirus"));
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
            windows = "output"

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
        assert!(edp.reserve);
        assert_eq!(
            (global.windows, edp.windows),
            (Windows::All, Windows::Output)
        );
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
        assert!(err("windows = \"some\""));
        assert!(err("launch-timeout = 100000"));
        assert!(err("outputs = \"some\""));
        assert!(err("outputs = 1"));
        assert!(err("unknown = 1"));
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
