//! The Wayland runtime shared by wl-shell programs. See [`shell`] for how a
//! program plugs in.

pub mod capture;
pub mod dmabuf;
pub mod focus;
pub mod globals;
pub mod input;
pub mod list;
pub mod mru;
pub mod shell;
pub mod toplevel;

pub use shell::{Core, Error, Program, Shell};

/// The crates programs build on, so they use the same versions.
pub mod reexports {
    pub use calloop;
    pub use smithay_client_toolkit;
    pub use wayland_client;
    pub use wayland_protocols;
}
