//! wl-note: sticky notes along a screen edge, belonging to apps. See
//! PLAN.md for the design.

mod app;
mod column;
mod config;
mod context;
mod deck;
mod list;
mod options;
mod papers;

use std::process::ExitCode;

use wl_shell_config::ConfigFile;
use wl_shell_core::{Error, Shell};
use wl_shell_log::warn;
use wl_shell_notes::{Index, Window};

use crate::app::WlNote;
use crate::config::Config;
use crate::options::{Cmd, Options};

fn main() -> ExitCode {
    wl_shell_log::set_name("wl-note");
    let opts = match Options::parse_args() {
        Ok(opts) => opts,
        Err(code) => return code,
    };
    if let Some(cmd) = &opts.command {
        return command(&opts, cmd);
    }
    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The subcommands only read the notes: they don't need wl-note running.
fn command(opts: &Options, cmd: &Cmd) -> ExitCode {
    let config = match Config::read(opts.config.as_deref()) {
        Ok(config) => config,
        Err(e) => {
            warn!("config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = config.notes_dir();
    match cmd {
        Cmd::Path => println!("{}", dir.display()),
        Cmd::List { app, title, json } => {
            let (index, errors) = Index::load(&dir);
            for e in errors {
                warn!("{e}");
            }
            let window = (app.is_some() || title.is_some()).then(|| {
                Window::new(
                    app.clone().unwrap_or_default(),
                    title.clone().unwrap_or_default(),
                )
            });
            let notes = list::notes(&index, window.as_ref());
            for (path, note) in &notes {
                let problems = note.error.iter().chain(&note.warnings);
                for p in problems {
                    warn!("{}: {p}", path.display());
                }
            }
            match json {
                true => print!("{}", list::json(&notes)),
                false => print!("{}", list::text(&notes)),
            }
        }
    }
    ExitCode::SUCCESS
}

/// wl-note is always shown: it runs its own loop until the connection
/// fails.
fn run(opts: &Options) -> Result<(), Error> {
    let note = WlNote::new(opts.config.clone())?;
    let (mut shell, mut event_loop) = Shell::connect(note)?;
    shell.program.started(&mut shell.core)?;
    loop {
        event_loop
            .dispatch(None, &mut shell)
            .map_err(|e| format!("wayland connection error: {e}"))?;
        let Shell { core, program } = &mut shell;
        program.after_dispatch(core);
    }
}
