//! What `wl-dock --launcher` and `--activate` ask the running dock, in the
//! shared line protocol (see `wl_shell_daemon::ipc`). Pure, unit tested.
//!
//! For the launcher, the dock answers `picked <app>` once the launcher
//! opened an app, and `cancelled` when it closed without (or a second
//! `--launcher` closed it), or an error, e.g. without a bottom dock on
//! the focused output. `activate` is answered `done` as soon as the dock
//! has it: the mode goes on without the client.

use wl_shell_daemon::ipc::{Request, line, payload};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DockRequest {
    /// Nothing asked (an empty request).
    #[default]
    Nothing,
    /// Open the launcher, or close it if it is open.
    Launcher,
    /// Keyboard activation: act on item n (1–10), or only show the
    /// numbers (None).
    Activate(Option<u8>),
}

impl Request for DockRequest {
    fn encode_lines(&self) -> String {
        match self {
            DockRequest::Nothing => String::new(),
            DockRequest::Launcher => line("launcher", None),
            DockRequest::Activate(None) => line("activate", None),
            DockRequest::Activate(Some(n)) => line("activate", Some(&n.to_string())),
        }
    }

    fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
        *self = match (word, p) {
            ("launcher", None) => DockRequest::Launcher,
            ("activate", None) => DockRequest::Activate(None),
            ("activate", p) => {
                let n = payload(p, text)?;
                match n.parse::<u8>() {
                    Ok(n @ 1..=10) => DockRequest::Activate(Some(n)),
                    _ => return Err(format!("'{text}': expected 1 to 10")),
                }
            }
            _ => return Err(format!("unknown request line '{text}'")),
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for req in [
            DockRequest::Nothing,
            DockRequest::Launcher,
            DockRequest::Activate(None),
            DockRequest::Activate(Some(1)),
            DockRequest::Activate(Some(10)),
        ] {
            assert_eq!(DockRequest::decode(req.encode().as_bytes()), Ok(Some(req)));
        }
        assert_eq!(DockRequest::Launcher.encode(), "launcher\nend\n");
        assert_eq!(DockRequest::Activate(Some(2)).encode(), "activate 2\nend\n");
        assert!(DockRequest::decode(b"launcher now\nend\n").is_err());
        assert!(DockRequest::decode(b"apps\nend\n").is_err());
        assert!(DockRequest::decode(b"activate 0\nend\n").is_err());
        assert!(DockRequest::decode(b"activate 11\nend\n").is_err());
        assert!(DockRequest::decode(b"activate x\nend\n").is_err());
    }
}
