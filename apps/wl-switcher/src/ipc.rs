//! wl-switcher's request to its daemon, in the shared line protocol (see
//! `wl_shell_daemon::ipc`). Pure, unit tested.
//!
//! A request opens the switcher. Once open, the switcher handles all keys
//! itself, so a request while it is open is answered with an error.

use wl_shell_daemon::ipc::{self, line, payload};

use crate::switcher::Step;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The step the switcher opens with.
    pub step: Step,
    /// Ext toplevel identifiers of the windows to show; empty: all. A
    /// filter: the daemon orders them by focus.
    pub ids: Vec<String>,
    /// The output to show on, by name.
    pub output: Option<String>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            step: Step::NextApp,
            ids: Vec::new(),
            output: None,
        }
    }
}

pub fn step_name(step: Step) -> &'static str {
    match step {
        Step::NextApp => "next",
        Step::PrevApp => "prev",
        Step::NextWindow => "next-window",
        Step::PrevWindow => "prev-window",
    }
}

pub fn step_from_name(name: &str) -> Option<Step> {
    Some(match name {
        "next" => Step::NextApp,
        "prev" => Step::PrevApp,
        "next-window" => Step::NextWindow,
        "prev-window" => Step::PrevWindow,
        _ => return None,
    })
}

impl ipc::Request for Request {
    fn encode_lines(&self) -> String {
        let mut out = line("command", Some(step_name(self.step)));
        for id in &self.ids {
            out += &line("id", Some(id));
        }
        if let Some(output) = &self.output {
            out += &line("output", Some(output));
        }
        out
    }

    fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
        match (word, p) {
            ("command", p) => {
                let name = payload(p, text)?;
                self.step = step_from_name(&name).ok_or(format!("'{text}': unknown command"))?;
            }
            ("id", p) => self.ids.push(payload(p, text)?),
            ("output", p) => self.output = Some(payload(p, text)?),
            _ => return Err(format!("unknown request line '{text}'")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_daemon::Request as _;

    #[test]
    fn steps() {
        for s in [
            Step::NextApp,
            Step::PrevApp,
            Step::NextWindow,
            Step::PrevWindow,
        ] {
            assert_eq!(step_from_name(step_name(s)), Some(s));
        }
        assert_eq!(step_from_name("toggle"), None);
        assert_eq!(step_from_name("cancel"), None);
    }

    #[test]
    fn request_roundtrip() {
        let req = Request {
            step: Step::PrevWindow,
            ids: vec!["1".into(), "x y".into(), "a\nb".into()],
            output: Some("DP-1".into()),
        };
        assert_eq!(Request::decode(req.encode().as_bytes()), Ok(Some(req)));
        let default = Request::default();
        assert_eq!(default.encode(), "command next\nend\n");
        assert_eq!(
            Request::decode(default.encode().as_bytes()),
            Ok(Some(default))
        );
    }

    #[test]
    fn request_incomplete_or_bad() {
        assert_eq!(Request::decode(b""), Ok(None));
        assert_eq!(Request::decode(b"id 1\nen"), Ok(None));
        assert!(Request::decode(b"bogus\nend\n").is_err());
        assert!(Request::decode(b"id\nend\n").is_err());
        assert!(Request::decode(b"command sideways\nend\n").is_err());
        assert!(Request::decode(b"\xff\nend\n").is_err());
    }
}
