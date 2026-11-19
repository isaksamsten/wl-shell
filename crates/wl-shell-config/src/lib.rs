//! Config files for wl-shell programs. Each program defines its own keys
//! and theme; this crate provides what they share: file lookup and
//! reloading, the `[appearance]` table with dark and light overrides, the
//! desktop's color scheme, watching files and trees with inotify, the
//! `output` and `[outputs.<pattern>]` keys, and validating deserializers.

pub mod appearance;
pub mod de;
pub mod desktop;
pub mod file;
pub mod output;
pub mod reload;
pub mod tree;
pub mod watch;

pub use appearance::{Appearance, Mode, Scheme};
pub use file::ConfigFile;
pub use reload::Reloader;
pub use tree::{TreeEvent, watch_tree};
pub use watch::{watch_dir, watch_file};
