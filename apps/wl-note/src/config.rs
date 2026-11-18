//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-note/config.toml` (default
//! `~/.config/wl-note/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! General settings live at the root; column, sheet, editor, files and
//! compositor integration settings have their own tables.
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
    /// `focused` (the context's output), `all`, or an output's name.
    pub output: String,
    pub edge: Edge,
    pub layer: NoteLayer,
    pub column: ColumnConfig,
    pub sheet: SheetConfig,
    pub editor: EditorConfig,
    pub files: FilesConfig,
    pub integration: IntegrationConfig,
    pub appearance: wl_shell_config::Appearance<Theme>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            notes_dir: "~/Notes/wl-note".into(),
            output: "focused".into(),
            edge: Edge::default(),
            layer: NoteLayer::default(),
            column: ColumnConfig::default(),
            sheet: SheetConfig::default(),
            editor: EditorConfig::default(),
            files: FilesConfig::default(),
            integration: IntegrationConfig::default(),
            appearance: wl_shell_config::Appearance::default(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ColumnConfig {
    /// How far a note's marker comes out of the edge.
    #[serde(deserialize_with = "marker_width")]
    pub marker_width: u32,
    /// A marker's length along the edge.
    #[serde(deserialize_with = "marker_length")]
    pub marker_length: u32,
    #[serde(deserialize_with = "de::px")]
    pub marker_radius: u32,
    /// Between markers, at least (until they are crowded).
    #[serde(deserialize_with = "de::px")]
    pub marker_gap: u32,
    /// Kept free at both ends of the column.
    #[serde(deserialize_with = "de::px")]
    pub marker_padding: u32,
    /// The band along the whole edge where the pointer brings the
    /// headers out (0: only the markers do).
    #[serde(deserialize_with = "edge_band")]
    pub edge_band: u32,
    /// Keep windows off the edge band (an exclusive zone).
    pub reserve_edge_band: bool,
    /// How far a header (a note's margin) comes out of the edge.
    #[serde(deserialize_with = "header")]
    pub header: u32,
    /// A header's length along the edge.
    #[serde(deserialize_with = "header_length")]
    pub header_length: u32,
    #[serde(deserialize_with = "de::duration")]
    pub reveal_delay: u32,
    #[serde(deserialize_with = "de::duration")]
    pub hide_delay: u32,
    /// Spring response, ms.
    #[serde(deserialize_with = "response")]
    pub response: u32,
}

impl Default for ColumnConfig {
    fn default() -> Self {
        Self {
            marker_width: 6,
            marker_length: 28,
            marker_radius: 3,
            marker_gap: 8,
            marker_padding: 12,
            edge_band: 2,
            reserve_edge_band: false,
            header: 28,
            header_length: 96,
            reveal_delay: 120,
            hide_delay: 400,
            response: 150,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct SheetConfig {
    #[serde(deserialize_with = "sheet_width")]
    pub width: u32,
    #[serde(deserialize_with = "sheet_height")]
    pub height: u32,
    /// The bar of file icons along a sheet's bottom.
    #[serde(deserialize_with = "file_bar")]
    pub file_bar: u32,
    /// The most a note is tilted, degrees.
    #[serde(deserialize_with = "tilt")]
    pub tilt: f64,
    /// For new notes and notes without `color`.
    #[serde(deserialize_with = "color_name")]
    pub default_color: Color,
    pub font: String,
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
}

impl Default for SheetConfig {
    fn default() -> Self {
        Self {
            width: 300,
            height: 220,
            file_bar: 24,
            tilt: 1.5,
            default_color: Color::Preset("yellow"),
            font: "sans-serif".into(),
            font_size: 13,
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct EditorConfig {
    #[serde(deserialize_with = "de::duration")]
    pub save_delay: u32,
    /// Opens a note in an external editor: `{path}`.
    pub command: String,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            save_delay: 500,
            command: "xdg-open {path}".into(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct FilesConfig {
    /// Opens links and files: `{path}`.
    pub open_command: String,
    /// Moves notes to the Trash: `{paths}`.
    pub trash_command: String,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            open_command: "xdg-open {path}".into(),
            trash_command: "gio trash -- {paths}".into(),
        }
    }
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct IntegrationConfig {
    /// Prints the ID of each window that gains focus, one per line.
    pub mru_event_stream: Option<String>,
    /// `<output> <id>` lines: which output each window is on, for
    /// `output = "focused"`.
    pub output_event_stream: Option<String>,
    /// How long a window has focus before the notes follow it.
    #[serde(deserialize_with = "de::duration")]
    pub settle: u32,
}

impl Default for IntegrationConfig {
    fn default() -> Self {
        Self {
            mru_event_stream: None,
            output_event_stream: None,
            settle: 120,
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
        let rgba = match color.unwrap_or(&self.sheet.default_color) {
            Color::Preset(name) => theme.preset(name).0,
            Color::Rgb(rgb) => rgb << 8 | 0xff,
        };
        match self.appearance.scheme {
            Scheme::Dark => dim(rgba, theme.dark_dim),
            Scheme::Light => rgba,
        }
    }

    /// The text on a note in `color` (None: the default color): the
    /// preset's, or dark or light for a color of its own.
    pub fn text_color(&self, color: Option<&Color>) -> u32 {
        match color.unwrap_or(&self.sheet.default_color) {
            Color::Preset(name) => self.theme().preset(name).1,
            Color::Rgb(rgb) => {
                let [_, r, g, b] = rgb.to_be_bytes();
                let luma = 0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64;
                match luma > 140.0 {
                    true => 0x2b2b2bff,
                    false => 0xf2f2f2ff,
                }
            }
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

    /// The shadow of a lifted or focused note: larger, softer and a
    /// little stronger.
    pub fn lifted_shadow(&self) -> Option<shadow::IconShadow> {
        self.shadow().map(|s| {
            let a = ((s.color & 0xff) as f64 * 1.3).round().min(255.0) as u32;
            shadow::IconShadow {
                color: (s.color & !0xff) | a,
                softness: (s.softness * 2).max(4),
                offset: (s.offset.0 * 2, s.offset.1 * 2 + 2),
            }
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

fn marker_width<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 2, 100)
}

fn marker_length<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 8, 1000)
}

fn edge_band<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 64)
}

fn header<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 12, 200)
}

fn header_length<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 24, 1000)
}

fn sheet_width<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 120, 4000)
}

fn sheet_height<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 80, 4000)
}

fn file_bar<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 16, 100)
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
        assert_eq!(c.sheet.default_color, Color::Preset("yellow"));
    }

    #[test]
    fn tables() {
        let c = parse(
            r##"
notes-dir = "/n"
edge = "left"

[column]
marker-width = 12

[sheet]
height = 300
default-color = "#102030"

[editor]
save-delay = 0
command = "foot nvim {path}"

[files]
open-command = "open {path}"

[integration]
mru-event-stream = "focus-stream"
output-event-stream = "output-stream"
settle = 0

[appearance]
paper-pink = "#ff0000"

[appearance.dark]
dark-dim = 1.0
"##,
        )
        .unwrap();
        assert_eq!(c.notes_dir(), PathBuf::from("/n"));
        assert_eq!(c.edge, Edge::Left);
        assert_eq!(c.column.marker_width, 12);
        assert_eq!(c.column.hide_delay, 400);
        assert_eq!(c.sheet.height, 300);
        assert_eq!(c.sheet.default_color, Color::Rgb(0x102030));
        assert_eq!(c.editor.save_delay, 0);
        assert_eq!(c.editor.command, "foot nvim {path}");
        assert_eq!(c.files.open_command, "open {path}");
        assert_eq!(
            c.integration.mru_event_stream.as_deref(),
            Some("focus-stream")
        );
        assert_eq!(
            c.integration.output_event_stream.as_deref(),
            Some("output-stream")
        );
        assert_eq!(c.integration.settle, 0);
        assert_eq!(c.appearance.dark.paper_pink, 0xff0000ff);
        assert_eq!(c.appearance.dark.dark_dim, 1.0);
        assert_eq!(c.appearance.light.dark_dim, 0.9);
    }

    #[test]
    fn invalid() {
        for bad in [
            "edge = \"top\"",
            "marker-width = 12", // Moved keys aren't accepted at the root.
            "[column]\nmarker-width = 0",
            "[column]\nheader = 1000",
            "[sheet]\ndefault-color = \"teal\"",
            "unknown = 1",
            "[sheet]\ntilt = 20.0",
            "[files]\nunknown = 1",
            "[appearance]\nunknown = 1",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn example_config() {
        assert_eq!(
            parse(include_str!("../config.example.toml")).unwrap(),
            Config::default(),
        );
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
