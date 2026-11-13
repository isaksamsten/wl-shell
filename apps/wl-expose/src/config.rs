//! Settings from the config file (TOML).
//!
//! Looked up at `$XDG_CONFIG_HOME/wl-expose/config.toml` (default
//! `~/.config/wl-expose/config.toml`) unless `--config` names a file. A
//! missing default file means the defaults; every key is optional.
//!
//! Colors and backgrounds live in `[appearance]`; `[appearance.dark]` and
//! `[appearance.light]` override its keys in the desktop's dark or light
//! mode (or the one `mode` forces).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};
use wl_shell_config::{ConfigFile, Scheme, de, file::resolve};
use wl_shell_render::{shadow, text};

pub type Appearance = wl_shell_config::Appearance<Theme>;

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Opening transition duration in milliseconds; 0: none.
    #[serde(deserialize_with = "de::duration")]
    pub open_duration: u32,
    /// Closing fade-out duration in milliseconds; 0: none.
    #[serde(deserialize_with = "de::duration")]
    pub close_duration: u32,
    /// Card size relative to the output.
    #[serde(deserialize_with = "de::fraction")]
    pub card_scale: f64,
    /// Minimum space between thumbnails.
    #[serde(deserialize_with = "de::px")]
    pub gap: u32,
    /// Space between the card edges and the layout; negative lets
    /// thumbnails extend past the card (but never off the output).
    #[serde(deserialize_with = "de::signed_px")]
    pub margin: i32,
    /// Zoom factor of the selected thumbnail; 1 disables zooming.
    #[serde(deserialize_with = "de::zoom")]
    pub zoom: f64,
    /// Zoom animation duration in milliseconds; 0: instant.
    #[serde(deserialize_with = "de::duration")]
    pub zoom_duration: u32,
    /// Max thumbnail scale.
    #[serde(deserialize_with = "de::positive")]
    pub max_scale: f64,
    /// App icon size; 0 disables icons.
    #[serde(deserialize_with = "icon_size")]
    pub icon_size: u32,
    /// Show the selected window's title in a pill below it.
    pub titles: bool,
    /// Font family for titles (and letter icons).
    pub font: String,
    /// Title font size.
    #[serde(deserialize_with = "de::font_size")]
    pub font_size: u32,
    /// A command the daemon runs with `sh -c`, printing the ID of each
    /// window that gains focus, one per line; windows are then shown most
    /// recently focused first (see mru.rs).
    pub mru_event_stream: Option<String>,
    pub appearance: Appearance,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            open_duration: 400,
            close_duration: 150,
            card_scale: 0.9,
            gap: 24,
            margin: 48,
            zoom: 1.08,
            zoom_duration: 150,
            max_scale: 1.0,
            icon_size: 48,
            titles: true,
            font: "sans-serif".into(),
            font_size: 14,
            mru_event_stream: None,
            appearance: Appearance::default(),
        }
    }
}

/// Colors and backgrounds: the keys of `[appearance]`, and of its `dark`
/// and `light` tables, which override them.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Theme {
    /// Color outside the card, made opaque over black. With a background
    /// image, the tint over its blurred copy instead.
    #[serde(deserialize_with = "de::color")]
    pub background: u32,
    /// Image scaled to cover the output, shown on the card.
    /// `~/` is expanded; relative paths are relative to the config file.
    pub background_image: Option<PathBuf>,
    /// Blur of the background image outside the card; 0: none.
    #[serde(deserialize_with = "de::px")]
    pub background_blur: u32,
    /// Card color without a background image.
    #[serde(deserialize_with = "de::color")]
    pub card_color: u32,
    /// Color of windows without a thumbnail.
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
            background: 0x000000b3,
            background_image: None,
            background_blur: 0,
            card_color: 0x303030ff,
            placeholder: 0x3a3a3aff,
            shadow_color: 0x00000077,
            shadow_softness: 30,
            shadow_spread: 5,
            shadow_offset: [0, 5],
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
    /// The colors and backgrounds in use.
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

    /// The title pill style; None if titles are disabled.
    pub fn title_style(&self) -> Option<text::Style> {
        self.titles.then(|| text::Style {
            font: self.font.clone(),
            size: self.font_size as f64,
            color: self.theme().title_color,
            background: self.theme().title_background,
        })
    }
}

impl ConfigFile for Config {
    const DIR: &'static str = "wl-expose";

    fn apply_scheme(&mut self, desktop: &mut dyn FnMut() -> Scheme) {
        self.appearance.apply(desktop);
    }

    /// Background images are relative to the config file.
    fn loaded(&mut self, path: &Path) {
        let home = std::env::var_os("HOME");
        for theme in self.appearance.themes_mut() {
            if let Some(image) = &mut theme.background_image {
                *image = resolve(image, path.parent(), home.as_deref());
            }
        }
    }
}

/// App icon size; 0 disables icons.
fn icon_size<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    de::bounded(d, 0, 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_default() {
        assert_eq!(<Config as ConfigFile>::parse(""), Ok(Config::default()));
    }

    #[test]
    fn readme_example() {
        let readme = include_str!("../README.md");
        let start = readme.find("```toml\n").unwrap() + "```toml\n".len();
        let len = readme[start..].find("```").unwrap();
        let c = <Config as ConfigFile>::parse(&readme[start..start + len]).unwrap();
        assert_eq!(c.appearance.dark, Theme::default());
        assert_eq!(c.appearance.light.card_color, 0xf0f0f0ff);
    }

    #[test]
    fn full() {
        let c = <Config as ConfigFile>::parse(
            r##"
            open-duration = 0
            close-duration = 0
            card-scale = 0.8
            gap = 10
            margin = -12
            max-scale = 2
            zoom = 1.1
            zoom-duration = 0
            icon-size = 0
            titles = false
            font = "Inter"
            font-size = 12

            [appearance]
            mode = "light"
            background = "#00000080"
            background-image = "wall.png"
            background-blur = 40
            card-color = "101010"
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
        assert_eq!((c.open_duration, c.close_duration), (0, 0));
        assert_eq!(c.card_scale, 0.8);
        assert_eq!((c.gap, c.margin), (10, -12));
        assert_eq!((c.zoom, c.zoom_duration), (1.1, 0));
        assert_eq!(c.max_scale, 2.0);
        assert_eq!(c.icon_size, 0);
        assert!(!c.titles && c.title_style().is_none());
        assert_eq!((c.font.as_str(), c.font_size), ("Inter", 12));
        assert_eq!(c.appearance.mode, wl_shell_config::Mode::Light);
        // Without overrides, both schemes are the same.
        assert_eq!(c.appearance.dark, c.appearance.light);
        let t = c.theme();
        assert_eq!(t.background, 0x00000080);
        assert_eq!(t.background_image.as_deref(), Some(Path::new("wall.png")));
        assert_eq!(t.background_blur, 40);
        assert_eq!(t.card_color, 0x101010ff);
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
            card-color = "101010"
            placeholder = "222222"

            [appearance.dark]
            background-image = "dark.png"

            [appearance.light]
            card-color = "f0f0f0"
            "##,
        )
        .unwrap();
        let a = &c.appearance;
        assert_eq!(a.mode, wl_shell_config::Mode::Auto);
        assert_eq!(a.dark.card_color, 0x101010ff);
        assert_eq!(a.light.card_color, 0xf0f0f0ff);
        assert_eq!(
            (a.dark.placeholder, a.light.placeholder),
            (0x222222ff, 0x222222ff)
        );
        assert_eq!(
            a.dark.background_image.as_deref(),
            Some(Path::new("dark.png"))
        );
        assert_eq!(a.light.background_image, None);

        c.appearance.mode = wl_shell_config::Mode::Light;
        c.appearance.apply(|| unreachable!());
        assert_eq!(c.theme().card_color, 0xf0f0f0ff);
        c.appearance.mode = wl_shell_config::Mode::Dark;
        c.appearance.apply(|| unreachable!());
        assert_eq!(c.theme().card_color, 0x101010ff);
    }

    #[test]
    fn invalid() {
        let err = |s: &str| <Config as ConfigFile>::parse(s).is_err();
        assert!(err("gap = -1"));
        assert!(err("margin = -100001"));
        assert!(err("gap = 100001"));
        assert!(err("max-scale = 0"));
        assert!(err("zoom = 0.9"));
        assert!(err("zoom = 3"));
        assert!(err("zoom-duration = -1"));
        assert!(err("card-scale = 0"));
        assert!(err("card-scale = 1.5"));
        assert!(err("open-duration = 10001"));
        assert!(err("close-duration = -1"));
        assert!(err("unknown = 1"));
        assert!(err("font-size = 0"));
        assert!(err("icon-size = 2000"));
        assert!(err("titles = 1"));
        // Appearance keys only go in [appearance].
        assert!(err("placeholder = \"222222\""));
        assert!(err("[appearance]\ngap = 1"));
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
