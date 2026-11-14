use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// A dock: pinned and running apps, one icon per app ID, magnified around
/// the pointer. Start it with the compositor. The subcommands edit the
/// pins ($XDG_STATE_HOME/wl-dock/pinned), which a running dock picks up.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Options {
    /// Config file (default: $XDG_CONFIG_HOME/wl-dock/config.toml)
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Pin an app (by app ID) to the end of the dock
    Pin {
        #[arg(value_name = "APP-ID")]
        item: String,
    },
    /// Unpin an app
    Unpin {
        #[arg(value_name = "APP-ID")]
        item: String,
    },
    /// Print the pins, in dock order
    Pins,
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
        Options::try_parse_from([&["wl-dock"], args].concat())
    }

    #[test]
    fn cli() {
        Options::command().debug_assert();
        let o = parse(&[]).unwrap();
        assert!(o.config.is_none() && o.command.is_none());
        let o = parse(&["--config", "c.toml"]).unwrap();
        assert_eq!(o.config.as_deref(), Some(std::path::Path::new("c.toml")));
        assert!(parse(&["next"]).is_err());
    }

    #[test]
    fn pin_commands() {
        let cmd = |args: &[&str]| parse(args).unwrap().command;
        assert_eq!(
            cmd(&["pin", "firefox"]),
            Some(Cmd::Pin {
                item: "firefox".into()
            })
        );
        assert_eq!(
            cmd(&["unpin", "firefox"]),
            Some(Cmd::Unpin {
                item: "firefox".into()
            })
        );
        assert_eq!(cmd(&["pins"]), Some(Cmd::Pins));
        assert!(parse(&["pin"]).is_err());
        assert!(parse(&["pins", "x"]).is_err());
    }
}
