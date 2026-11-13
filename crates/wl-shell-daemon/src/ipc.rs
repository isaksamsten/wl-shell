//! Client/daemon protocol over a Unix socket. Pure, unit tested.
//!
//! Both directions are lines of `<word>[ <escaped payload>]`, where the
//! payload escapes `\` as `\\` and newlines as `\n`. The client sends a
//! request (the program's own lines) terminated by `end` and keeps the
//! connection open: closing it cancels the show. The daemon answers with any
//! number of `warn` lines and one final `picked`, `cancelled` or `error`
//! line, then closes.

use std::path::PathBuf;

/// A program's request: what the client asks the daemon to show.
pub trait Request: Sized + Default {
    /// The request's lines, without `end`; see `line`.
    fn encode_lines(&self) -> String;

    /// Apply one decoded request line (`word` and raw `payload`; `text` is
    /// the whole line, for errors). See `payload`.
    fn decode_line(&mut self, word: &str, payload: Option<&str>, text: &str) -> Result<(), String>;

    fn encode(&self) -> String {
        self.encode_lines() + &line("end", None)
    }

    /// Parse a request from the start of `buf`. `Ok(None)`: no `end` line yet.
    fn decode(buf: &[u8]) -> Result<Option<Self>, String> {
        let mut req = Self::default();
        let mut rest = buf;
        while let Some(nl) = rest.iter().position(|&b| b == b'\n') {
            let text = std::str::from_utf8(&rest[..nl]).map_err(|_| "request is not UTF-8")?;
            rest = &rest[nl + 1..];
            match split(text) {
                ("end", None) => return Ok(Some(req)),
                (word, p) => req.decode_line(word, p, text)?,
            }
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Warn(String),
    /// The show ended with this window picked.
    Picked(String),
    Cancelled,
    Error(String),
}

/// `$XDG_RUNTIME_DIR/<program>-$WAYLAND_DISPLAY.sock`, so nested or parallel
/// compositors each get their own daemon.
pub fn socket_path(program: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?);
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    socket_path_in(dir, program, &display)
}

fn socket_path_in(dir: PathBuf, program: &str, display: &str) -> Option<PathBuf> {
    // WAYLAND_DISPLAY may be an absolute path.
    let name = display.replace('/', "_");
    dir.is_absolute()
        .then(|| dir.join(format!("{program}-{name}.sock")))
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n")
}

fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            _ => return Err(format!("bad escape in '{s}'")),
        }
    }
    Ok(out)
}

/// One protocol line: `word`, and the escaped `payload` if any.
pub fn line(word: &str, payload: Option<&str>) -> String {
    match payload {
        Some(p) => format!("{word} {}\n", escape(p)),
        None => format!("{word}\n"),
    }
}

fn split(line: &str) -> (&str, Option<&str>) {
    match line.split_once(' ') {
        Some((word, payload)) => (word, Some(payload)),
        None => (line, None),
    }
}

/// The unescaped payload of the line `text`; an error if it has none.
pub fn payload(payload: Option<&str>, text: &str) -> Result<String, String> {
    unescape(payload.ok_or(format!("'{text}': missing value"))?)
}

impl Reply {
    pub fn encode(&self) -> String {
        match self {
            Reply::Warn(m) => line("warn", Some(m)),
            Reply::Picked(id) => line("picked", Some(id)),
            Reply::Cancelled => line("cancelled", None),
            Reply::Error(m) => line("error", Some(m)),
        }
    }

    /// Parse one line, without its newline.
    pub fn decode(text: &str) -> Result<Self, String> {
        let p = |p: Option<&str>| payload(p, text);
        Ok(match split(text) {
            ("warn", pl) => Reply::Warn(p(pl)?),
            ("picked", pl) => Reply::Picked(p(pl)?),
            ("cancelled", None) => Reply::Cancelled,
            ("error", pl) => Reply::Error(p(pl)?),
            _ => return Err(format!("unknown reply '{text}'")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, PartialEq)]
    struct Req {
        ids: Vec<String>,
        flag: bool,
    }

    impl Request for Req {
        fn encode_lines(&self) -> String {
            let mut out: String = self.ids.iter().map(|id| line("id", Some(id))).collect();
            if self.flag {
                out += &line("flag", None);
            }
            out
        }

        fn decode_line(&mut self, word: &str, p: Option<&str>, text: &str) -> Result<(), String> {
            match (word, p) {
                ("id", p) => self.ids.push(payload(p, text)?),
                ("flag", None) => self.flag = true,
                _ => return Err(format!("unknown request line '{text}'")),
            }
            Ok(())
        }
    }

    #[test]
    fn escapes() {
        for s in [
            "",
            "plain",
            "a b",
            "back\\slash",
            "new\nline",
            "\\n literal",
            "end",
        ] {
            assert_eq!(unescape(&escape(s)).as_deref(), Ok(s));
            assert!(!escape(s).contains('\n'));
        }
        assert!(unescape("trailing\\").is_err());
        assert!(unescape("\\t").is_err());
    }

    #[test]
    fn request_roundtrip() {
        let req = Req {
            ids: vec!["1".into(), "x y".into(), "a\nb".into()],
            flag: true,
        };
        assert_eq!(Req::decode(req.encode().as_bytes()), Ok(Some(req)));
        assert_eq!(Req::default().encode(), "end\n");
    }

    #[test]
    fn request_incomplete_or_bad() {
        assert_eq!(Req::decode(b""), Ok(None));
        assert_eq!(Req::decode(b"id 1\nen"), Ok(None));
        assert!(Req::decode(b"bogus\nend\n").is_err());
        assert!(Req::decode(b"id\nend\n").is_err());
        assert!(Req::decode(b"\xff\nend\n").is_err());
    }

    #[test]
    fn reply_roundtrip() {
        for r in [
            Reply::Warn("no window with ID '3'".into()),
            Reply::Picked("a\\b".into()),
            Reply::Cancelled,
            Reply::Error("no windows to show".into()),
        ] {
            let enc = r.encode();
            assert_eq!(Reply::decode(enc.strip_suffix('\n').unwrap()), Ok(r));
        }
        assert!(Reply::decode("picked").is_err());
        assert!(Reply::decode("hello").is_err());
    }

    #[test]
    fn paths() {
        let p = |dir: &str, d: &str| socket_path_in(PathBuf::from(dir), "wl-x", d);
        assert_eq!(
            p("/run/user/1000", "wayland-1"),
            Some("/run/user/1000/wl-x-wayland-1.sock".into())
        );
        assert_eq!(p("/run", "/tmp/w"), Some("/run/wl-x-_tmp_w.sock".into()));
        assert_eq!(p("relative", "wayland-0"), None);
    }
}
