use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// A dock: pinned and running apps, one icon per app ID, magnified around
/// the pointer, and pinned folders. Start it with the compositor. The
/// subcommands edit the pins ($XDG_STATE_HOME/wl-dock/pinned), which a
/// running dock picks up.
#[derive(Parser, Debug)]
#[command(version, args_conflicts_with_subcommands = true)]
pub struct Options {
    /// Config file (default: $XDG_CONFIG_HOME/wl-dock/config.toml)
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Turn the running dock on the focused output into a search bar for
    /// apps (or close it if open). Exits 0 once an app was opened, 1 if
    /// cancelled. Needs a bottom dock there.
    #[arg(long, conflicts_with = "config")]
    pub launcher: bool,

    /// Keyboard activation, for a compositor binding Super+N: act on dock
    /// item N (1-9, and 0 or 10 for the tenth) while the modifier is
    /// held. Without N, only show the items' numbers. Exits at once.
    #[arg(
        long,
        value_name = "N",
        num_args = 0..=1,
        value_parser = clap::value_parser!(u8).range(0..=10),
        conflicts_with_all = ["config", "launcher"],
    )]
    pub activate: Option<Option<u8>>,

    #[command(subcommand)]
    pub command: Option<Cmd>,
}

#[derive(Subcommand, Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Pin an app (by app ID), a standalone widget (`@name`, of the
    /// config's `[widget.<name>]`) or a folder (by path) to the end of its
    /// part of the dock; files can't be pinned
    Pin {
        #[arg(value_name = "APP-ID|@WIDGET|FOLDER")]
        item: String,
    },
    /// Unpin an app or a folder, or remove a widget
    Unpin {
        #[arg(value_name = "APP-ID|@WIDGET|FOLDER")]
        item: String,
    },
    /// Print the pins, in dock order
    Pins,
    /// List the outputs, what to call them in the config, and what the
    /// config makes of each
    Outputs {
        /// Config file (default: $XDG_CONFIG_HOME/wl-dock/config.toml)
        #[arg(long, value_name = "PATH")]
        config: Option<PathBuf>,
    },
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

    /// The item `--activate` asks for, if it was given: Some(None) to
    /// only show the numbers; 0 is the tenth.
    pub fn activate(&self) -> Option<Option<u8>> {
        self.activate
            .map(|n| n.map(|n| if n == 0 { 10 } else { n }))
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
    fn launcher() {
        let o = parse(&["--launcher"]).unwrap();
        assert!(o.launcher && o.command.is_none());
        assert!(!parse(&[]).unwrap().launcher);
        assert!(parse(&["--launcher", "--config", "c.toml"]).is_err());
        assert!(parse(&["--launcher", "pins"]).is_err());
    }

    #[test]
    fn activate() {
        let a = |args: &[&str]| parse(args).map(|o| o.activate());
        assert_eq!(a(&[]).unwrap(), None);
        assert_eq!(a(&["--activate"]).unwrap(), Some(None));
        assert_eq!(a(&["--activate", "2"]).unwrap(), Some(Some(2)));
        assert_eq!(a(&["--activate", "0"]).unwrap(), Some(Some(10)));
        assert_eq!(a(&["--activate", "10"]).unwrap(), Some(Some(10)));
        assert!(parse(&["--activate", "11"]).is_err());
        assert!(parse(&["--activate", "x"]).is_err());
        assert!(parse(&["--activate", "1", "--launcher"]).is_err());
        assert!(parse(&["--activate", "pins"]).is_err());
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
        assert_eq!(cmd(&["outputs"]), Some(Cmd::Outputs { config: None }));
        assert_eq!(
            cmd(&["outputs", "--config", "c.toml"]),
            Some(Cmd::Outputs {
                config: Some("c.toml".into())
            })
        );
        assert!(parse(&["pin"]).is_err());
        assert!(parse(&["pins", "x"]).is_err());
    }
}
