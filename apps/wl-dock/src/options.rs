use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// A dock: running apps, one icon per app ID, magnified around
/// the pointer. Hovering an app with several windows shows a strip of
/// their previews and titles. Start it with the compositor.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Options {
    /// Config file (default: $XDG_CONFIG_HOME/wl-dock/config.toml)
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
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
        assert!(parse(&[]).unwrap().config.is_none());
        let o = parse(&["--config", "c.toml"]).unwrap();
        assert_eq!(o.config.as_deref(), Some(std::path::Path::new("c.toml")));
        assert!(parse(&["next"]).is_err());
    }
}
