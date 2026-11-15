//! Config files for wl-shell programs. Each program defines its own keys
//! and theme; this crate provides what they share: file lookup and
//! reloading, the `[appearance]` table with dark and light overrides, the
//! desktop's color scheme, watching files with inotify, and validating
//! deserializers.

pub mod appearance;
pub mod de;
pub mod desktop;
pub mod file;
pub mod reload;
pub mod watch;

pub use appearance::{Appearance, Mode, Scheme};
pub use file::ConfigFile;
pub use reload::Reloader;
pub use watch::{watch_dir, watch_file};
