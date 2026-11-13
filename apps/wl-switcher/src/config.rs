//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-switcher/config.toml` (default
//! `~/.config/wl-switcher/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! Colors live in `[appearance]`; `[appearance.dark]` and
//! `[appearance.light]` override its keys in the desktop's dark or light
//! mode (or the one `mode` forces).

use serde::{Deserialize, Deserializer};
use wl_shell_config::{ConfigFile, Scheme, de};
use wl_shell_render::{shadow, text};

pub type Appearance = wl_shell_config::Appearance<Theme>;

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Which modifier's release picks.
    pub modifier: Modifier,
    /// Milliseconds before anything is visible, so a quick tap shows
    /// nothing.
    #[serde(deserialize_with = "de::duration")]
    pub show_delay: u32,
    /// App icon size in the bar.
    #[serde(deserialize_with = "icon_size")]
    pub icon_size: u32,
    /// Space between icons.
    #[serde(deserialize_with = "de::px")]
    pub icon_gap: u32,
    /// Vertical center of the bar, as a fraction of the output height.
    #[serde(deserialize_with = "de::unit")]
    pub bar_position: f64,
    /// Widest the bar gets, as a fraction of the output width.
    #[serde(deserialize_with = "de::fraction")]
    pub max_width: f64,
    /// The selected app's name under its icon.
    pub labels: bool,
    /// Dots under the icons of apps with several windows.
    pub window_dots: bool,
    /// Window previews in the strip.
    pub previews: Previews,
    /// Strip card height, as a fraction of the output height.
    #[serde(deserialize_with = "de::fraction")]
    pub strip_card_height: f64,
    /// Widest a strip card gets, as a fraction of the output width.
    #[serde(deserialize_with = "de::fraction")]
    pub strip_max_width: f64,
    /// Zoom factor of the selected card; 1 disables zooming.
    #[serde(deserialize_with = "de::zoom")]
    pub zoom: f64,
    /// Spring response for movement, in milliseconds.
    #[serde(deserialize_with = "response")]
    pub response: u32,
    /// Spring response for the apps/windows morph, in milliseconds.
    #[serde(deserialize_with = "response")]
    pub morph_response: u32,
    /// Alpha of a full-screen black tint behind the switcher; 0: none.
    #[serde(deserialize_with = "de::unit")]
    pub dim: f64,
    /// Font family for labels and titles (and letter icons).
    pub font: String,
    /// Label and title font size.
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
    /// Command (run with `sh -c`) that prints the ID of each window that
    /// gains focus, one per line.
    pub mru_event_stream: Option<String>,
    pub appearance: Appearance,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            modifier: Modifier::default(),
            show_delay: 100,
            icon_size: 96,
            icon_gap: 16,
            bar_position: 0.5,
            max_width: 0.9,
            labels: true,
            window_dots: true,
            previews: Previews::default(),
            strip_card_height: 0.22,
            strip_max_width: 0.5,
            zoom: 1.09,
            response: 180,
            morph_response: 260,
            dim: 0.0,
            font: "sans-serif".into(),
            font_size: 14,
            mru_event_stream: None,
            appearance: Appearance::default(),
        }
    }
}

/// The modifier whose release picks.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Modifier {
    /// Whichever non-Shift modifiers are held when the switcher opens.
    #[default]
    Auto,
    Alt,
    Super,
    Ctrl,
}

#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Previews {
    /// Live while the strip is open.
    #[default]
    Live,
    /// One frame per window.
    Static,
    /// Icons and titles only.
    Off,
}

/// Colors: the keys of `[appearance]`, and of its `dark` and `light`
/// tables, which override them.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Theme {
    /// The bar's background.
    #[serde(deserialize_with = "de::color")]
    pub panel_color: u32,
    /// Behind the selected icon.
    #[serde(deserialize_with = "de::color")]
    pub highlight_color: u32,
    /// The window dots under icons.
    #[serde(deserialize_with = "de::color")]
    pub dot_color: u32,
    #[serde(deserialize_with = "de::color")]
    pub label_color: u32,
    #[serde(deserialize_with = "de::color")]
    pub label_background: u32,
    /// Color of windows without a preview.
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
    #[serde(deserialize_with = "de::color")]
    pub title_color: u32,
    #[serde(deserialize_with = "de::color")]
    pub title_background: u32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            panel_color: 0x262626e6,
            highlight_color: 0xffffff2e,
            dot_color: 0xffffffb3,
            label_color: 0xffffffff,
            label_background: 0x00000000,
            placeholder: 0x3a3a3aff,
            shadow_color: 0x00000080,
            shadow_softness: 30,
            shadow_spread: 0,
            shadow_offset: [0, 6],
            icon_shadow_color: 0x00000059,
            icon_shadow_softness: 6,
            icon_shadow_offset: [0, 2],
            icon_theme: None,
            title_color: 0xffffffff,
            title_background: 0x000000b3,
        }
    }
}

impl Config {
    /// The colors in use.
    pub fn theme(&self) -> &Theme {
        self.appearance.theme()
    }

    /// The shadow settings; None if shadows are disabled.
    pub fn shadow(&self) -> Option<shadow::Settings> {
        let t = self.theme();
        (t.shadow_color & 0xff != 0).then(|| shadow::Settings {
            color: t.shadow_color,
            softness: t.shadow_softness as f64,
            spread: t.shadow_spread,
            offset: (t.shadow_offset[0], t.shadow_offset[1]),
        })
    }

    /// The icon shadow settings; None if icon shadows are disabled.
    pub fn icon_shadow(&self) -> Option<shadow::IconShadow> {
        let t = self.theme();
        (t.icon_shadow_color & 0xff != 0).then(|| shadow::IconShadow {
            color: t.icon_shadow_color,
            softness: t.icon_shadow_softness,
            offset: (t.icon_shadow_offset[0], t.icon_shadow_offset[1]),
        })
    }

    /// The style of the app name label; None if labels are disabled.
    pub fn label_style(&self) -> Option<text::Style> {
        self.labels.then(|| text::Style {
            font: self.font.clone(),
            size: self.font_size as f64,
            color: self.theme().label_color,
            background: self.theme().label_background,
        })
    }

    /// The style of a window's title pill.
    pub fn title_style(&self) -> text::Style {
        text::Style {
            font: self.font.clone(),
            size: self.font_size as f64,
            color: self.theme().title_color,
            background: self.theme().title_background,
        }
    }
}

impl ConfigFile for Config {
    const DIR: &'static str = "wl-switcher";

    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme) {
        self.appearance.apply(desktop);
    }
}

/// App icon size in the bar.
fn icon_size<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 16, 512)
}

/// Spring response, in milliseconds.
fn response<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 1, 10_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(<Config as ConfigFile>::parse(""), Ok(Config::default()));
    }

    #[test]
    fn plan_example() {
        let c = <Config as ConfigFile>::parse(
            r#"mru-event-stream = "~/.config/wl-switcher/focus-stream.sh""#,
        );
        assert_eq!(
            c.unwrap().mru_event_stream.as_deref(),
            Some("~/.config/wl-switcher/focus-stream.sh")
        );
    }

    #[test]
    fn full() {
        let c = <Config as ConfigFile>::parse(
            r##"
            modifier = "super"
            show-delay = 0
            icon-size = 64
            icon-gap = 8
            bar-position = 0.3
            max-width = 0.5
            labels = false
            window-dots = false
            previews = "static"
            strip-card-height = 0.3
            strip-max-width = 0.4
            zoom = 1
            response = 100
            morph-response = 300
            dim = 0.2
            font = "Inter"
            font-size = 12

            [appearance]
            mode = "light"
            panel-color = "#00000080"
            highlight-color = "ffffff40"
            dot-color = "ffffff"
            label-color = "eeeeee"
            label-background = "00000080"
            placeholder = "#222222"
            shadow-color = "ff000080"
            shadow-softness = 0
            shadow-spread = -3
            shadow-offset = [2, -4]
            icon-shadow-color = "00000040"
            icon-shadow-softness = 4
            icon-shadow-offset = [1, 3]
            icon-theme = "Papirus"
            title-color = "eeeeee"
            title-background = "00000080"
            "##,
        )
        .unwrap();
        assert_eq!(c.modifier, Modifier::Super);
        assert_eq!(c.show_delay, 0);
        assert_eq!((c.icon_size, c.icon_gap), (64, 8));
        assert_eq!((c.bar_position, c.max_width), (0.3, 0.5));
        assert!(!c.labels && c.label_style().is_none() && !c.window_dots);
        assert_eq!(c.previews, Previews::Static);
        assert_eq!((c.strip_card_height, c.strip_max_width), (0.3, 0.4));
        assert_eq!((c.zoom, c.response, c.morph_response), (1.0, 100, 300));
        assert_eq!(c.dim, 0.2);
        assert_eq!((c.font.as_str(), c.font_size), ("Inter", 12));
        assert_eq!(c.appearance.mode, wl_shell_config::Mode::Light);
        // Without overrides, both schemes are the same.
        assert_eq!(c.appearance.dark, c.appearance.light);
        let t = c.theme();
        assert_eq!(t.panel_color, 0x00000080);
        assert_eq!(t.highlight_color, 0xffffff40);
        assert_eq!(t.dot_color, 0xffffffff);
        assert_eq!(
            (t.label_color, t.label_background),
            (0xeeeeeeff, 0x00000080)
        );
        assert_eq!(t.placeholder, 0x222222ff);
        assert_eq!(t.shadow_color, 0xff000080);
        assert_eq!(t.shadow_softness, 0);
        assert_eq!(t.shadow_spread, -3);
        assert_eq!(t.shadow_offset, [2, -4]);
        assert_eq!(t.icon_shadow_color, 0x00000040);
        assert_eq!(t.icon_shadow_softness, 4);
        assert_eq!(t.icon_shadow_offset, [1, 3]);
        assert_eq!(t.icon_theme.as_deref(), Some("Papirus"));
        assert_eq!(
            (t.title_color, t.title_background),
            (0xeeeeeeff, 0x00000080)
        );
    }

    #[test]
    fn schemes() {
        let mut c = <Config as ConfigFile>::parse(
            r##"
            [appearance]
            panel-color = "101010"
            placeholder = "222222"

            [appearance.light]
            panel-color = "f0f0f0"
            "##,
        )
        .unwrap();
        let a = &c.appearance;
        assert_eq!(a.mode, wl_shell_config::Mode::Auto);
        assert_eq!(a.dark.panel_color, 0x101010ff);
        assert_eq!(a.light.panel_color, 0xf0f0f0ff);
        assert_eq!(
            (a.dark.placeholder, a.light.placeholder),
            (0x222222ff, 0x222222ff)
        );

        c.appearance.mode = wl_shell_config::Mode::Light;
        c.appearance.apply(|| unreachable!());
        assert_eq!(c.theme().panel_color, 0xf0f0f0ff);
        c.appearance.mode = wl_shell_config::Mode::Dark;
        c.appearance.apply(|| unreachable!());
        assert_eq!(c.theme().panel_color, 0x101010ff);
    }

    #[test]
    fn invalid() {
        let err = |s: &str| <Config as ConfigFile>::parse(s).is_err();
        assert!(err("icon-gap = -1"));
        assert!(err("icon-gap = 100001"));
        assert!(err("icon-size = 8"));
        assert!(err("icon-size = 2000"));
        assert!(err("modifier = \"hyper\""));
        assert!(err("previews = \"sometimes\""));
        assert!(err("bar-position = 1.5"));
        assert!(err("max-width = 0"));
        assert!(err("strip-card-height = 2"));
        assert!(err("dim = -0.1"));
        assert!(err("zoom = 0.9"));
        assert!(err("zoom = 3"));
        assert!(err("response = 0"));
        assert!(err("show-delay = 10001"));
        assert!(err("unknown = 1"));
        assert!(err("font-size = 0"));
        assert!(err("labels = 1"));
        // Appearance keys only go in [appearance].
        assert!(err("placeholder = \"222222\""));
        assert!(err("[appearance]\nicon-gap = 1"));
        assert!(err("[appearance]\nplaceholder = \"red\""));
        assert!(err("[appearance]\nshadow-spread = 100001"));
        assert!(err("[appearance]\nshadow-offset = [1]"));
        assert!(err("[appearance]\nshadow-softness = -1"));
        assert!(err("[appearance]\nmode = \"dim\""));
        assert!(err("[appearance]\ndark = 1"));
        assert!(err("[appearance.dark]\nplaceholder = \"red\""));
        assert!(err("[appearance.light]\nunknown = 1"));
        assert!(err("[appearance.light.dark]"));
        assert!(
            <Config as ConfigFile>::parse("[appearance]\nshadow-color = \"00000000\"")
                .unwrap()
                .shadow()
                .is_none()
        );
        assert!(
            <Config as ConfigFile>::parse("[appearance]\nicon-shadow-color = \"ffffff00\"")
                .unwrap()
                .icon_shadow()
                .is_none()
        );
    }
}
