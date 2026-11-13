//! The `[appearance]` table: a program's theme keys, with `[appearance.dark]`
//! and `[appearance.light]` overriding them in the desktop's dark or light
//! mode (or the one `mode` forces).

use serde::{Deserialize, Deserializer, de::DeserializeOwned, de::Error as _};

/// Which of `[appearance.dark]` and `[appearance.light]` applies.
#[derive(Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    /// Follow the desktop's color scheme.
    #[default]
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scheme {
    #[default]
    Dark,
    Light,
}

impl Scheme {
    pub fn new(prefers_dark: bool) -> Self {
        if prefers_dark {
            Scheme::Dark
        } else {
            Scheme::Light
        }
    }
}

/// The `[appearance]` table, resolved for both schemes. `T` is the
/// program's theme: a table of optional keys with defaults.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Appearance<T> {
    pub mode: Mode,
    /// `[appearance]` with `[appearance.dark]` over it.
    pub dark: T,
    /// `[appearance]` with `[appearance.light]` over it.
    pub light: T,
    /// The scheme in use; set by `apply`, not read from the file.
    pub scheme: Scheme,
}

impl<T: PartialEq> Appearance<T> {
    /// The theme of the scheme in use.
    pub fn theme(&self) -> &T {
        match self.scheme {
            Scheme::Dark => &self.dark,
            Scheme::Light => &self.light,
        }
    }

    /// Mutable access to both themes, e.g. to resolve paths in them.
    pub fn themes_mut(&mut self) -> [&mut T; 2] {
        [&mut self.dark, &mut self.light]
    }

    /// Pick the scheme: `mode`, or with `auto`, the desktop's, which
    /// `desktop` gives (only asked if the themes differ).
    pub fn apply(&mut self, desktop: impl FnOnce() -> Scheme) {
        self.scheme = match self.mode {
            Mode::Dark => Scheme::Dark,
            Mode::Light => Scheme::Light,
            Mode::Auto if self.dark == self.light => Scheme::Dark,
            Mode::Auto => desktop(),
        };
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Appearance<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut base = toml::Table::deserialize(d)?;
        let mode = match base.remove("mode") {
            Some(v) => v
                .try_into()
                .map_err(|e| D::Error::custom(format!("mode: {e}")))?,
            None => Mode::default(),
        };
        let mut overrides = |name: &str| match base.remove(name) {
            None => Ok(toml::Table::new()),
            Some(toml::Value::Table(t)) => Ok(t),
            Some(_) => Err(D::Error::custom(format!("{name}: expected a table"))),
        };
        let (dark, light) = (overrides("dark")?, overrides("light")?);
        // Report errors in the shared keys once, without a table name.
        T::deserialize(base.clone()).map_err(D::Error::custom)?;
        let theme = |name: &str, overrides: toml::Table| {
            let mut table = base.clone();
            table.extend(overrides);
            T::deserialize(table).map_err(|e| D::Error::custom(format!("{name}: {e}")))
        };
        Ok(Self {
            mode,
            dark: theme("dark", dark)?,
            light: theme("light", light)?,
            scheme: Scheme::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Debug, Clone, PartialEq)]
    #[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
    struct Theme {
        panel: u32,
        text: u32,
    }

    impl Default for Theme {
        fn default() -> Self {
            Self { panel: 1, text: 2 }
        }
    }

    fn parse(s: &str) -> Result<Appearance<Theme>, String> {
        toml::from_str(s).map_err(|e| e.to_string())
    }

    #[test]
    fn overrides() {
        let mut a = parse("panel = 5\n[light]\npanel = 7\n").unwrap();
        assert_eq!(a.mode, Mode::Auto);
        assert_eq!(a.dark, Theme { panel: 5, text: 2 });
        assert_eq!(a.light, Theme { panel: 7, text: 2 });
        a.mode = Mode::Light;
        a.apply(|| unreachable!());
        assert_eq!(a.theme().panel, 7);
        a.mode = Mode::Auto;
        a.apply(|| Scheme::Dark);
        assert_eq!(a.theme().panel, 5);
    }

    #[test]
    fn same_themes_skip_the_desktop() {
        let mut a = parse("").unwrap();
        a.apply(|| unreachable!());
        assert_eq!(a.scheme, Scheme::Dark);
    }

    #[test]
    fn invalid() {
        assert!(parse("mode = \"dim\"").is_err());
        assert!(parse("dark = 1").is_err());
        assert!(parse("[dark]\nunknown = 1").is_err());
        assert!(parse("[light.dark]").is_err());
    }
}
