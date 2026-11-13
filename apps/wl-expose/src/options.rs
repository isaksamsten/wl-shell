use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use crate::ipc::Request;

/// Show the given windows (ext toplevel identifiers; default: all) side by
/// side, let the user pick one, print its ID, and exit 0. Escape or a click on
/// the background exits 1. The overlay is shown by a daemon (--daemon), which
/// must be running; appearance is set in its config file
/// ($XDG_CONFIG_HOME/wl-expose/config.toml).
#[derive(Parser, Debug)]
#[command(version)]
pub struct Options {
    /// Ext toplevel identifiers of the windows to show
    #[arg(value_name = "ID")]
    pub ids: Vec<String>,

    /// Print identifier<TAB>app_id<TAB>title per window
    #[arg(short, long)]
    pub list: bool,

    /// Start with this window selected (default: the most recently focused,
    /// with an mru-event-stream; else the first ID)
    #[arg(short, long, value_name = "ID")]
    pub select: Option<String>,

    /// Capture one frame per window (default: live)
    #[arg(long = "static")]
    pub static_: bool,

    /// Paint cursors into thumbnails
    #[arg(short, long)]
    pub cursors: bool,

    /// Max wait for window sizes before showing
    #[arg(short, long, value_name = "MS", default_value = "500", value_parser = parse_ms)]
    pub timeout: u32,

    /// Output to show on (a wl_output name, e.g. DP-1); default: the only
    /// output, if there is just one
    #[arg(short, long, value_name = "NAME")]
    pub output: Option<String>,

    /// Config file for the daemon (default:
    /// $XDG_CONFIG_HOME/wl-expose/config.toml)
    #[arg(long, value_name = "PATH", requires = "daemon")]
    pub config: Option<PathBuf>,

    /// Run in the background and show the overlay for later invocations
    #[arg(long, conflicts_with_all = ["ids", "list", "select", "static_", "cursors", "timeout", "output"])]
    pub daemon: bool,
}

impl Options {
    /// Parse the command line. On `--help`, `--version` or an error, prints
    /// and returns the exit code (0 or 1).
    pub fn parse_args() -> Result<Self, ExitCode> {
        Self::try_parse().map_err(|e| {
            let _ = e.print();
            if e.use_stderr() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        })
    }

    /// What the daemon should show.
    pub fn request(&self) -> Request {
        Request {
            ids: self.ids.clone(),
            select: self.select.clone(),
            live: !self.static_,
            cursors: self.cursors,
            timeout_ms: self.timeout,
            output: self.output.clone(),
        }
    }
}

fn parse_ms(s: &str) -> Result<u32, String> {
    s.parse()
        .ok()
        .filter(|&v| v <= 100_000)
        .ok_or_else(|| format!("expected 0-100000, got '{s}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn numbers() {
        assert_eq!(parse_ms("24"), Ok(24));
        assert!(parse_ms("-1").is_err());
        assert!(parse_ms("100001").is_err());
    }

    #[test]
    fn cli() {
        Options::command().debug_assert();
        let o =
            Options::try_parse_from(["wl-expose", "--static", "-s", "2", "-o", "DP-1", "1", "2"])
                .unwrap();
        assert!(!o.request().live);
        assert_eq!(o.select.as_deref(), Some("2"));
        assert_eq!(o.request().output.as_deref(), Some("DP-1"));
        assert_eq!(o.ids, ["1", "2"]);
        assert_eq!(o.timeout, 500);
        assert!(o.config.is_none());
        assert!(!o.daemon);
    }

    #[test]
    fn daemon() {
        let parse = |args: &[&str]| Options::try_parse_from([&["wl-expose"], args].concat());
        assert!(parse(&["--daemon", "--config", "c.toml"]).unwrap().daemon);
        assert!(parse(&["--daemon", "1"]).is_err());
        assert!(parse(&["--daemon", "--static"]).is_err());
        assert!(parse(&["--daemon", "-t", "100"]).is_err());
        assert!(parse(&["--daemon", "-o", "DP-1"]).is_err());
        assert!(parse(&["--config", "c.toml"]).is_err());
        assert!(parse(&["--no-daemon"]).is_err());
    }
}
