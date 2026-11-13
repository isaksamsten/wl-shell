//! A daemon's config: reloaded when its file changes, with the desktop's
//! color scheme applied.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use calloop::LoopHandle;

use crate::desktop::DesktopScheme;
use crate::file::ConfigFile;

pub fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

pub struct Reloader<C> {
    /// `--config`, if given.
    explicit: Option<PathBuf>,
    config: C,
    mtime: Option<SystemTime>,
    desktop: DesktopScheme,
}

impl<C: ConfigFile> Reloader<C> {
    /// Read the config; `refresh` applies the scheme.
    pub fn load(explicit: Option<PathBuf>) -> Result<Self, String> {
        let config = C::read(explicit.as_deref())?;
        let mtime = C::path(explicit.as_deref()).as_deref().and_then(mtime);
        Ok(Self {
            explicit,
            config,
            mtime,
            desktop: DesktopScheme::default(),
        })
    }

    /// The config in use.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// Reload the config if its file changed and apply the desktop's color
    /// scheme. Errors keep the previous config and are reported to `warn`.
    /// Returns whether the config (or the scheme in use) changed.
    pub fn refresh<D: 'static>(
        &mut self,
        handle: &LoopHandle<'static, D>,
        warn: &mut dyn FnMut(String),
    ) -> bool {
        let mtime = C::path(self.explicit.as_deref()).as_deref().and_then(mtime);
        let mut config = None;
        if mtime != self.mtime {
            match C::read(self.explicit.as_deref()) {
                Ok(c) => {
                    config = Some(c);
                    self.mtime = mtime;
                }
                Err(e) => warn(format!("config: {e} (keeping the previous config)")),
            }
        }
        let mut config = config.unwrap_or_else(|| self.config.clone());
        // Only watches the desktop if the config cares.
        let desktop = &mut self.desktop;
        config.apply_scheme(&mut || desktop.get(handle, &mut *warn));
        let changed = config != self.config;
        self.config = config;
        changed
    }

    /// Whether the desktop switched schemes since the last call.
    pub fn scheme_changed(&self) -> bool {
        self.desktop.take_changed()
    }
}
