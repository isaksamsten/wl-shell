//! Notes as wl-note keeps them, for every program that reads them (wl-note
//! itself, and later wl-dock's launcher). Pure, apart from reading the
//! notes directory in [`index`] and writing notes in [`write`].
//!
//! - [`note`]: one note: a markdown file with an optional TOML frontmatter
//!   between `+++` lines, parsed and edited in place.
//! - [`matcher`]: which windows a note belongs to (app ID and title
//!   globs).
//! - [`due`]: when a note is due, the presets that set it, and `<when>` as
//!   the command line takes it.
//! - [`index`]: every note in a directory, kept up to date by rescanning.
//! - [`state`]: how the notes are arranged (their order, and which are
//!   hidden), in state files outside them.
//! - [`write`]: writing notes atomically, and new ones without replacing
//!   any.

pub mod due;
pub mod index;
pub mod matcher;
pub mod note;
pub mod state;
pub mod write;

pub use due::{Due, TimeOfDay};
pub use index::{Changes, Entry, Index};
pub use matcher::{Matcher, Window};
pub use note::{Color, Meta, Note, PRESETS};
pub use state::{PathList, State};
pub use write::{create_new, write_atomic};

/// The TOML editing crate `Note::set` takes values of.
pub use toml_edit;
