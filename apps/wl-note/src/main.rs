//! wl-note: sticky notes along a screen edge, belonging to apps. See
//! PLAN.md for the design.

mod app;
mod bar;
mod column;
mod config;
mod context;
mod create;
mod deck;
mod find;
mod ipc;
mod list;
mod markdown;
mod options;
mod papers;
mod title;

use std::io::Read;
use std::process::ExitCode;

use wl_shell_config::ConfigFile;
use wl_shell_core::{Error, Shell};
use wl_shell_log::warn;
use wl_shell_notes::{Color, Index, Window};

use crate::app::WlNote;
use crate::config::Config;
use crate::ipc::{NewNote, NoteRequest};
use crate::options::{Cmd, Options};

/// What the commands that need it say without a running wl-note.
const MISSING: &str = "wl-note isn't running (start it with the compositor)";

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

/// The subcommands: `new` and `focus` ask the running wl-note; the
/// others only read the notes.
fn command(opts: &Options, cmd: &Cmd) -> ExitCode {
    match cmd {
        Cmd::New {
            global,
            color,
            title,
            text,
        } => {
            if let Some(Err(e)) = color.as_deref().map(|c| Color::parse(c.trim())) {
                warn!("{e}");
                return ExitCode::FAILURE;
            }
            let text = match text.as_deref() {
                Some("-") => {
                    let mut s = String::new();
                    if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                        warn!("stdin: {e}");
                        return ExitCode::FAILURE;
                    }
                    Some(s)
                }
                other => other.map(String::from),
            };
            let req = NoteRequest::New(NewNote {
                global: *global,
                color: color.clone(),
                title: title.clone(),
                text,
            });
            return wl_shell_daemon::client_to(&req, MISSING);
        }
        Cmd::Focus { note } => {
            let req = NoteRequest::Focus(note.clone());
            return wl_shell_daemon::client_to(&req, MISSING);
        }
        _ => {}
    }
    let config = match Config::read(opts.config.as_deref()) {
        Ok(config) => config,
        Err(e) => {
            warn!("config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dir = config.notes_dir();
    match cmd {
        Cmd::New { .. } | Cmd::Focus { .. } => unreachable!("asked above"),
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
