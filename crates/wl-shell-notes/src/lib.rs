//! Notes as wl-note keeps them, for every program that reads them (wl-note
//! itself, and later wl-dock's launcher). Pure, apart from reading the
//! notes directory in [`index`].
//!
//! - [`note`]: one note: a markdown file with an optional TOML frontmatter
//!   between `+++` lines, parsed and edited in place.
//! - [`matcher`]: which windows a note belongs to (app ID and title
//!   globs).
//! - [`index`]: every note in a directory, kept up to date by rescanning.

pub mod index;
pub mod matcher;
pub mod note;

pub use index::{Changes, Entry, Index};
pub use matcher::{Matcher, Window};
pub use note::{Color, Meta, Note, PRESETS};

/// The TOML editing crate `Note::set` takes values of.
pub use toml_edit;
