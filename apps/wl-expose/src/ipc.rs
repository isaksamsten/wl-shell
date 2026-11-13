//! wl-expose's request to its daemon, in the shared line protocol (see
//! `wl_shell_daemon::ipc`). Pure, unit tested.

use wl_shell_daemon::ipc::{self, line, payload};

/// What to show; the command line minus appearance (that is the daemon's
/// config).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub ids: Vec<String>,
    pub select: Option<String>,
    pub live: bool,
    pub cursors: bool,
    pub timeout_ms: u32,
    /// The output to show on, by name.
    pub output: Option<String>,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            select: None,
            live: true,
            cursors: false,
            timeout_ms: 500,
            output: None,
        }
    }
}

impl ipc::Request for Request {
    fn encode_lines(&self) -> String {
        let mut out = String::new();
        for id in &self.ids {
            out += &line("id", Some(id));
        }
        if let Some(select) = &self.select {
            out += &line("select", Some(select));
        }
        if !self.live {
            out += &line("static", None);
        }
        if self.cursors {
            out += &line("cursors", None);
        }
        out += &line("timeout", Some(&self.timeout_ms.to_string()));
        if let Some(output) = &self.output {
            out += &line("output", Some(output));
        }
        out
    }

    fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
        match (word, p) {
            ("id", p) => self.ids.push(payload(p, text)?),
            ("select", p) => self.select = Some(payload(p, text)?),
            ("static", None) => self.live = false,
            ("cursors", None) => self.cursors = true,
            ("timeout", p) => {
                self.timeout_ms = payload(p, text)?
                    .parse()
                    .map_err(|_| format!("'{text}': bad timeout"))?
            }
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
    fn request_roundtrip() {
        let req = Request {
            ids: vec!["1".into(), "x y".into(), "a\nb".into()],
            select: Some("x y".into()),
            live: false,
            cursors: true,
            timeout_ms: 250,
            output: Some("DP-1".into()),
        };
        assert_eq!(Request::decode(req.encode().as_bytes()), Ok(Some(req)));
        let default = Request::default();
        assert_eq!(default.encode(), "timeout 500\nend\n");
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
        assert!(Request::decode(b"timeout x\nend\n").is_err());
        assert!(Request::decode(b"\xff\nend\n").is_err());
    }
}
