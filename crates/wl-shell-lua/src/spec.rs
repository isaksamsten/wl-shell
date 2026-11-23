//! What the host asks the runtime to run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::channel::InstanceId;

/// Where a plugin's code comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// A file, read on every (re)start.
    File(PathBuf),
    /// Code built into the host (a bundled plugin).
    Code { name: String, code: Arc<str> },
}

impl Source {
    /// The file, if it is one (to watch, and to reload).
    pub fn path(&self) -> Option<&Path> {
        match self {
            Source::File(p) => Some(p),
            Source::Code { .. } => None,
        }
    }

    /// The chunk name in Lua's messages.
    pub(crate) fn chunk_name(&self) -> String {
        match self {
            Source::File(p) => format!("@{}", p.display()),
            Source::Code { name, .. } => format!("@{name}.lua (bundled)"),
        }
    }

    /// The folder `require` loads modules from.
    pub(crate) fn dir(&self) -> Option<PathBuf> {
        self.path()?.parent().map(Path::to_path_buf)
    }
}

/// What an instance is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// A widget placement: always set up.
    Widget,
    /// A `[plugin.<name>]` table: holds the plugin's launcher
    /// capabilities, so it is only set up (and kept) if it declares
    /// actions or sources.
    Plugin,
}

/// One plugin state to run.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceSpec {
    pub id: InstanceId,
    /// The plugin's name, for messages.
    pub plugin: String,
    pub source: Source,
    pub role: Role,
    /// The app a widget backs (`dock.app`); None for a standalone widget
    /// or a plugin table.
    pub app: Option<String>,
    /// Passed to `setup(opts)`.
    pub options: serde_json::Value,
    /// Lua instructions per hook call.
    pub budget: u64,
}
