use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Sticky notes along a screen edge, belonging to apps: the notes shown
/// follow the focused window. Start it with the compositor. The
/// subcommands only read the notes, so they work without it.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Options {
    /// Config file (default: $XDG_CONFIG_HOME/wl-note/config.toml)
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Print the notes, in column order: all of them, or with --app or
    /// --title those shown for such a window (global notes too)
    List {
        /// The window's app ID
        #[arg(long, value_name = "ID")]
        app: Option<String>,
        /// The window's title
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// As a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Print the notes directory
    Path,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Options, clap::Error> {
        Options::try_parse_from([&["wl-note"], args].concat())
    }

    #[test]
    fn cli() {
        Options::command().debug_assert();
        let o = parse(&[]).unwrap();
        assert!(o.config.is_none() && o.command.is_none());
        let o = parse(&["--config", "c.toml"]).unwrap();
        assert_eq!(o.config.as_deref(), Some(std::path::Path::new("c.toml")));
        assert!(parse(&["nope"]).is_err());
    }

    #[test]
    fn list() {
        let o = parse(&["list", "--app", "firefox", "--config", "c.toml"]).unwrap();
        assert!(o.config.is_some());
        assert_eq!(
            o.command,
            Some(Cmd::List {
                app: Some("firefox".into()),
                title: None,
                json: false
            })
        );
        let o = parse(&["list", "--json", "--title", "x y"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::List {
                app: None,
                title: Some("x y".into()),
                json: true
            })
        );
        assert_eq!(parse(&["path"]).unwrap().command, Some(Cmd::Path));
        assert!(parse(&["path", "x"]).is_err());
    }
}
