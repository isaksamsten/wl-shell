use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Sticky notes along a screen edge, belonging to apps: the notes shown
/// follow the focused window. Start it with the compositor. `new`,
/// `focus` and `show --focus` ask the running wl-note; the others only
/// read and change the files, so they work without it.
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
    /// Print the notes, in column order: all of them but the hidden ones,
    /// or with --app or --title those shown for such a window (global
    /// notes too)
    List {
        /// The window's app ID
        #[arg(long, value_name = "ID")]
        app: Option<String>,
        /// The window's title
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// Only the hidden notes
        #[arg(long, conflicts_with = "all")]
        hidden: bool,
        /// The hidden notes too
        #[arg(long)]
        all: bool,
        /// As a JSON array
        #[arg(long)]
        json: bool,
    },
    /// Print the notes directory
    Path,
    /// Make a note for the focused app (or shown everywhere), and focus
    /// it; without --text, open it in the editor
    New {
        /// Shown everywhere, not only for the focused app
        #[arg(long)]
        global: bool,
        /// A preset (yellow, orange, pink, purple, blue, green, gray,
        /// white) or #rrggbb
        #[arg(long, value_name = "COLOR")]
        color: Option<String>,
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,
        /// The note's text; - reads it from stdin
        #[arg(long, value_name = "TEXT")]
        text: Option<String>,
    },
    /// Focus a note (a path, file name or title), showing it again if it
    /// is hidden; without one, the first note shown, or tuck the focused
    /// one back
    Focus {
        #[arg(value_name = "NOTE")]
        note: Option<String>,
    },
    /// Hide a note: it stays on disk, but isn't shown until `show`
    Hide {
        #[arg(value_name = "NOTE")]
        note: String,
    },
    /// Show a hidden note again
    Show {
        /// Also focus it (asks the running wl-note)
        #[arg(long)]
        focus: bool,
        #[arg(value_name = "NOTE")]
        note: String,
    },
    /// Drop the lines naming no note from the order and hidden files
    Prune,
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
                hidden: false,
                all: false,
                json: false
            })
        );
        let o = parse(&["list", "--json", "--title", "x y", "--all"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::List {
                app: None,
                title: Some("x y".into()),
                hidden: false,
                all: true,
                json: true
            })
        );
        assert!(parse(&["list", "--hidden", "--all"]).is_err());
        assert_eq!(parse(&["path"]).unwrap().command, Some(Cmd::Path));
        assert!(parse(&["path", "x"]).is_err());
    }

    #[test]
    fn new_and_focus() {
        let o = parse(&["new", "--global", "--color", "pink", "--text", "-"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::New {
                global: true,
                color: Some("pink".into()),
                title: None,
                text: Some("-".into()),
            })
        );
        let o = parse(&["focus"]).unwrap();
        assert_eq!(o.command, Some(Cmd::Focus { note: None }));
        let o = parse(&["focus", "work/release.md"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::Focus {
                note: Some("work/release.md".into())
            })
        );
        assert!(parse(&["focus", "a", "b"]).is_err());
    }

    #[test]
    fn hide_show_prune() {
        let o = parse(&["hide", "groceries"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::Hide {
                note: "groceries".into()
            })
        );
        assert!(parse(&["hide"]).is_err());
        let o = parse(&["show", "--focus", "groceries"]).unwrap();
        assert_eq!(
            o.command,
            Some(Cmd::Show {
                focus: true,
                note: "groceries".into()
            })
        );
        assert_eq!(parse(&["prune"]).unwrap().command, Some(Cmd::Prune));
    }
}
