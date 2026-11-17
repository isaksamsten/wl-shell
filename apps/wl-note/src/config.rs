//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-note/config.toml` (default
//! `~/.config/wl-note/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! Colors live in `[appearance]`; `[appearance.dark]` and
//! `[appearance.light]` override its keys in the desktop's dark or light
//! mode (or the one `mode` forces). Paper stays paper in dark mode: only
//! `dark-dim` darkens it a little.

use std::path::PathBuf;

use serde::{Deserialize, Deserializer, de::Error as _};
use wl_shell_config::{ConfigFile, Mode, Scheme, de};
use wl_shell_notes::Color;
use wl_shell_render::{paper, shadow};

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Where the notes are; `~/` expands.
    pub notes_dir: String,
    /// Prints the ID of each window that gains focus, one per line.
    pub mru_event_stream: Option<String>,
    /// `<output> <id>` lines, for `output = "focused"` (not used yet).
    pub output_event_stream: Option<String>,
    /// `focused`, `all`, or an output's name.
    pub output: String,
    pub edge: Edge,
    pub layer: NoteLayer,
    /// How much of a tucked note shows.
    #[serde(deserialize_with = "tuck")]
    pub tuck: u32,
    /// Between the slivers.
    #[serde(deserialize_with = "de::px")]
    pub tab_gap: u32,
    /// A sliver's length along the edge.
    #[serde(deserialize_with = "tab_height")]
    pub tab_height: u32,
    /// How much of a peeking note shows.
    #[serde(deserialize_with = "peek")]
    pub peek: u32,
    #[serde(deserialize_with = "focus_width")]
    pub focus_width: u32,
    /// Of the output's height, at most.
    #[serde(deserialize_with = "de::fraction")]
    pub focus_height: f64,
    #[serde(deserialize_with = "de::duration")]
    pub reveal_delay: u32,
    #[serde(deserialize_with = "de::duration")]
    pub hide_delay: u32,
    /// How long a window has focus before the notes follow it.
    #[serde(deserialize_with = "de::duration")]
    pub settle: u32,
    /// Spring response, ms.
    #[serde(deserialize_with = "response")]
    pub response: u32,
    /// The most a note is tilted, degrees.
    #[serde(deserialize_with = "tilt")]
    pub tilt: f64,
    #[serde(deserialize_with = "de::duration")]
    pub save_delay: u32,
    /// `{path}`.
    pub editor_command: String,
    /// Opens links and files: `{path}`.
    pub open_command: String,
    /// `{paths}`.
    pub trash_command: String,
    pub attach_mode: AttachMode,
    /// For new notes and notes without `color`.
    #[serde(deserialize_with = "color_name")]
    pub default_color: Color,
    pub font: String,
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
    pub appearance: wl_shell_config::Appearance<Theme>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notes_dir: "~/Notes/wl-note".into(),
            mru_event_stream: None,
            output_event_stream: None,
            output: "focused".into(),
            edge: Edge::default(),
            layer: NoteLayer::default(),
            tuck: 10,
            tab_gap: 4,
            tab_height: 64,
            peek: 180,
            focus_width: 420,
            focus_height: 0.7,
            reveal_delay: 120,
            hide_delay: 400,
            settle: 120,
            response: 150,
            tilt: 1.5,
            save_delay: 500,
            editor_command: "xdg-open {path}".into(),
            open_command: "xdg-open {path}".into(),
            trash_command: "gio trash -- {paths}".into(),
            attach_mode: AttachMode::default(),
            default_color: Color::Preset("yellow"),
            font: "sans-serif".into(),
            font_size: 13,
            appearance: wl_shell_config::Appearance::default(),
        }
    }
}

/// The screen edge the notes are tucked behind.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Edge {
    Left,
    #[default]
    Right,
}

impl From<Edge> for paper::Edge {
    fn from(e: Edge) -> Self {
        match e {
            Edge::Left => paper::Edge::Left,
            Edge::Right => paper::Edge::Right,
        }
    }
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum NoteLayer {
    #[default]
    Top,
    Overlay,
}

/// What dropping files on a note does.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AttachMode {
    /// Link the files where they are.
    #[default]
    Link,
    /// Copy them next to the note, and link the copies.
    Copy,
}

/// Colors: the keys of `[appearance]`, and of its `dark` and `light`
/// tables, which override them.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Theme {
    #[serde(deserialize_with = "de::color")]
    pub paper_yellow: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_yellow_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_orange: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_orange_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_pink: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_pink_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_purple: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_purple_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_blue: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_blue_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_green: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_green_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_gray: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_gray_text: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_white: u32,
    #[serde(deserialize_with = "de::color")]
    pub paper_white_text: u32,
    /// The paper's brightness in dark mode.
    #[serde(deserialize_with = "de::unit")]
    pub dark_dim: f64,
    #[serde(deserialize_with = "de::color")]
    pub link_color: u32,
    #[serde(deserialize_with = "de::color")]
    pub chip_background: u32,
    /// Alpha 0 disables the notes' shadows.
    #[serde(deserialize_with = "de::color")]
    pub shadow_color: u32,
    #[serde(deserialize_with = "de::px")]
    pub shadow_softness: u32,
    /// `[away from the edge, down]`.
    #[serde(deserialize_with = "de::offset")]
    pub shadow_offset: [i32; 2],
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            paper_yellow: 0xfff2a8ff,
            paper_yellow_text: 0x3d3420ff,
            paper_orange: 0xffd6a5ff,
            paper_orange_text: 0x4a3218ff,
            paper_pink: 0xffcfe0ff,
            paper_pink_text: 0x4a2433ff,
            paper_purple: 0xe4d4f7ff,
            paper_purple_text: 0x34264aff,
            paper_blue: 0xcfe5ffff,
            paper_blue_text: 0x1f3349ff,
            paper_green: 0xd5f0c4ff,
            paper_green_text: 0x263d1fff,
            paper_gray: 0xe4e4e0ff,
            paper_gray_text: 0x333333ff,
            paper_white: 0xfafaf7ff,
            paper_white_text: 0x2b2b2bff,
            dark_dim: 0.9,
            link_color: 0x1a5fb4ff,
            chip_background: 0x0000000f,
            shadow_color: 0x00000059,
            shadow_softness: 8,
            shadow_offset: [2, 3],
        }
    }
}

impl Theme {
    /// A preset's paper and text colors.
    pub fn preset(&self, name: &str) -> (u32, u32) {
        match name {
            "orange" => (self.paper_orange, self.paper_orange_text),
            "pink" => (self.paper_pink, self.paper_pink_text),
            "purple" => (self.paper_purple, self.paper_purple_text),
            "blue" => (self.paper_blue, self.paper_blue_text),
            "green" => (self.paper_green, self.paper_green_text),
            "gray" => (self.paper_gray, self.paper_gray_text),
            "white" => (self.paper_white, self.paper_white_text),
            _ => (self.paper_yellow, self.paper_yellow_text),
        }
    }
}

impl Config {
    /// The colors in use.
    pub fn theme(&self) -> &Theme {
        self.appearance.theme()
    }

    /// The notes directory, `~/` expanded.
    pub fn notes_dir(&self) -> PathBuf {
        let dir = self.notes_dir.trim();
        let dir = if dir == "~" { "~/" } else { dir };
        wl_shell_render::icons::expand_home(dir).into()
    }

    /// The paper of a note in `color` (None: the default color), as
    /// 0xRRGGBBAA, dimmed in dark mode.
    pub fn paper_color(&self, color: Option<&Color>) -> u32 {
        let theme = self.theme();
        let rgba = match color.unwrap_or(&self.default_color) {
            Color::Preset(name) => theme.preset(name).0,
            Color::Rgb(rgb) => rgb << 8 | 0xff,
        };
        match self.appearance.scheme {
            Scheme::Dark => dim(rgba, theme.dark_dim),
            Scheme::Light => rgba,
        }
    }

    /// The notes' shadow; None if disabled.
    pub fn shadow(&self) -> Option<shadow::IconShadow> {
        let t = self.theme();
        (t.shadow_color & 0xff != 0).then(|| shadow::IconShadow {
            color: t.shadow_color,
            softness: t.shadow_softness,
            offset: (t.shadow_offset[0], t.shadow_offset[1]),
        })
    }
}

/// `rgba` with its color channels times `f`.
fn dim(rgba: u32, f: f64) -> u32 {
    let [r, g, b, a] = rgba.to_be_bytes();
    let d = |c: u8| (c as f64 * f).round().clamp(0.0, 255.0) as u8;
    u32::from_be_bytes([d(r), d(g), d(b), a])
}

impl ConfigFile for Config {
    const DIR: &'static str = "wl-note";

    /// The desktop's scheme is asked for even if the dark and light
    /// themes are the same, since `dark-dim` only applies in dark mode.
    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme) {
        let a = &mut self.appearance;
        a.scheme = match a.mode {
            Mode::Dark => Scheme::Dark,
            Mode::Light => Scheme::Light,
            Mode::Auto => desktop(),
        };
    }
}

fn tuck<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 2, 100)
}

fn tab_height<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 16, 1000)
}

fn peek<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 20, 2000)
}

fn focus_width<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 100, 4000)
}

fn response<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 10_000)
}

fn tilt<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = f64::deserialize(d)?;
    if (0.0..=10.0).contains(&v) {
        Ok(v)
    } else {
        Err(D::Error::custom(format!("expected 0-10, got {v}")))
    }
}

fn color_name<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
    Color::parse(String::deserialize(d)?.trim()).map_err(D::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Config, String> {
        Config::parse(s)
    }

    #[test]
    fn defaults() {
        let c = parse("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.edge, Edge::Right);
        assert_eq!(c.default_color, Color::Preset("yellow"));
    }

    #[test]
    fn keys() {
        let c = parse(
            "notes-dir = \"/n\"\nedge = \"left\"\ntuck = 12\nsettle = 0\ndefault-color = \"#102030\"\n\
             mru-event-stream = \"x\"\n[appearance]\npaper-pink = \"#ff0000\"\n[appearance.dark]\ndark-dim = 1.0\n",
        )
        .unwrap();
        assert_eq!(c.notes_dir(), PathBuf::from("/n"));
        assert_eq!((c.edge, c.tuck, c.settle), (Edge::Left, 12, 0));
        assert_eq!(c.default_color, Color::Rgb(0x102030));
        assert_eq!(c.appearance.dark.paper_pink, 0xff0000ff);
        assert_eq!(c.appearance.dark.dark_dim, 1.0);
        assert_eq!(c.appearance.light.dark_dim, 0.9);
    }

    #[test]
    fn invalid() {
        for bad in [
            "edge = \"top\"",
            "tuck = 0",
            "default-color = \"teal\"",
            "unknown = 1",
            "tilt = 20.0",
            "[appearance]\nunknown = 1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn paper_colors() {
        let mut c = parse("[appearance]\nmode = \"light\"").unwrap();
        c.apply_scheme(&mut || unreachable!());
        assert_eq!(c.paper_color(None), 0xfff2a8ff);
        assert_eq!(c.paper_color(Some(&Color::Preset("blue"))), 0xcfe5ffff);
        assert_eq!(c.paper_color(Some(&Color::Rgb(0x123456))), 0x123456ff);
        // Dark mode dims the paper.
        let mut c = parse("[appearance.dark]\ndark-dim = 0.5").unwrap();
        c.apply_scheme(&mut || Scheme::Dark);
        assert_eq!(c.paper_color(Some(&Color::Rgb(0x804020))), 0x402010ff);
        c.apply_scheme(&mut || Scheme::Light);
        assert_eq!(c.paper_color(Some(&Color::Rgb(0x804020))), 0x804020ff);
    }
}
