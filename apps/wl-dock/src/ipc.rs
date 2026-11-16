//! What `wl-dock --launcher` asks the running dock, in the shared line
//! protocol (see `wl_shell_daemon::ipc`). Pure, unit tested.
//!
//! The dock answers `picked <app>` once the launcher opened an app, and
//! `cancelled` when it closed without (or a second `--launcher` closed
//! it), or an error, e.g. without a bottom dock on the focused output.

use wl_shell_daemon::ipc::{Request, line};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DockRequest {
    /// Nothing asked (an empty request).
    #[default]
    Nothing,
    /// Open the launcher, or close it if it is open.
    Launcher,
}

impl Request for DockRequest {
    fn encode_lines(&self) -> String {
        match self {
            DockRequest::Nothing => String::new(),
            DockRequest::Launcher => line("launcher", None),
        }
    }

    fn decode_line(&mut self, word: &str, payload: Option<&str>, text: &str) -> Result<(), String> {
        match (word, payload) {
            ("launcher", None) => *self = DockRequest::Launcher,
            _ => return Err(format!("unknown request line '{text}'")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for req in [DockRequest::Nothing, DockRequest::Launcher] {
            assert_eq!(DockRequest::decode(req.encode().as_bytes()), Ok(Some(req)));
        }
        assert_eq!(DockRequest::Launcher.encode(), "launcher\nend\n");
        assert!(DockRequest::decode(b"launcher now\nend\n").is_err());
        assert!(DockRequest::decode(b"apps\nend\n").is_err());
    }
}
