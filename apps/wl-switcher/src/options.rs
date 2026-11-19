use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use crate::ipc::Request;
use crate::switcher::Step;

/// Switch between apps (Alt+Tab) and between an app's windows (Alt+`).
/// Bind Alt+Tab, Alt+Shift+Tab, Alt+` and Alt+Shift+` to `next`, `prev`,
/// `next-window` and `prev-window`. Each one opens the switcher; it then
/// grabs the keyboard and handles all further keys itself while the
/// modifier is held. Releasing the modifier (or clicking) picks: the
/// window's ID is printed (exit 0). Alt+Escape exits 1.
///
/// Requires a running daemon (`wl-switcher --daemon`, started with the
/// compositor): it tracks focus order and keeps icons ready.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Options {
    /// The step the switcher opens with
    #[arg(value_enum, required_unless_present = "daemon")]
    pub action: Option<Action>,

    /// Ext toplevel identifiers of the windows to show (default: all). The
    /// order is the daemon's focus order; unfocused windows keep this order
    #[arg(value_name = "ID")]
    pub ids: Vec<String>,

    /// Config file for the daemon (default:
    /// $XDG_CONFIG_HOME/wl-switcher/config.toml)
    #[arg(long, value_name = "PATH", conflicts_with = "action")]
    pub config: Option<PathBuf>,

    /// Run the daemon: track focus order and show the switcher for later
    /// invocations
    #[arg(long, conflicts_with_all = ["action", "ids"])]
    pub daemon: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Action {
    /// Next app (Alt+Tab)
    Next,
    /// Previous app (Alt+Shift+Tab)
    Prev,
    /// Next window of the front app (Alt+`)
    NextWindow,
    /// Previous window of the front app (Alt+Shift+`)
    PrevWindow,
}

impl Action {
    pub fn step(self) -> Step {
        match self {
            Action::Next => Step::NextApp,
            Action::Prev => Step::PrevApp,
            Action::NextWindow => Step::NextWindow,
            Action::PrevWindow => Step::PrevWindow,
        }
    }
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

    pub fn request(&self) -> Request {
        Request {
            step: self.action.unwrap_or(Action::Next).step(),
            ids: self.ids.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Options, clap::Error> {
        Options::try_parse_from([&["wl-switcher"], args].concat())
    }

    #[test]
    fn cli() {
        Options::command().debug_assert();
        let o = parse(&["next-window", "3", "1"]).unwrap();
        assert_eq!(o.request().step, Step::NextWindow);
        assert_eq!(o.request().ids, ["3", "1"]);
        assert!(!o.daemon);
        assert!(parse(&["next", "-o", "DP-1"]).is_err());
        assert_eq!(parse(&["prev"]).unwrap().request().step, Step::PrevApp);
    }

    #[test]
    fn modes() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["sideways"]).is_err());
        assert!(parse(&["--daemon"]).unwrap().daemon);
        assert!(parse(&["--daemon", "next"]).is_err());
        assert!(parse(&["cancel"]).is_err());
        assert!(parse(&["--list"]).is_err());
        assert!(parse(&["--daemon", "--config", "c.toml"]).unwrap().daemon);
        assert!(parse(&["next", "--config", "c.toml"]).is_err());
        assert!(parse(&["next", "--no-daemon"]).is_err());
    }
}
