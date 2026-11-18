//! wl-note: sticky notes along a screen edge, belonging to apps. See
//! PLAN.md for the design.

mod app;
mod bar;
mod column;
mod config;
mod context;
mod create;
mod deck;
mod draft;
mod editor;
mod files;
mod find;
mod ipc;
mod list;
mod markdown;
mod options;
mod papers;
mod reorder;
mod scratch;
mod title;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wl_shell_config::ConfigFile;
use wl_shell_core::{Error, Shell};
use wl_shell_log::warn;
use wl_shell_notes::{Color, Index, State, Window, state};

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

/// `new`, `keep`, `focus` and `show --focus` ask the running wl-note.
/// Other commands only read or change files.
fn command(opts: &Options, cmd: &Cmd) -> ExitCode {
    match cmd {
        Cmd::New {
            global,
            window,
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
                window: *window,
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
        Cmd::Keep { note } => {
            return wl_shell_daemon::client_to(&NoteRequest::Keep(note.clone()), MISSING);
        }
        Cmd::List {
            scratch: true,
            json,
            ..
        } => {
            if let Some(dir) = state::dir()
                .map(|dir| dir.join("scratch"))
                .filter(|dir| dir.exists())
            {
                let (index, errors) = Index::load(dir);
                for e in errors {
                    warn!("{e}");
                }
                let windows = wl_shell_core::list::toplevels().unwrap_or_default();
                print!("{}", scratch::listing(&index, *json, &windows));
            } else if *json {
                println!("[]");
            }
            return ExitCode::SUCCESS;
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
    if let Cmd::Path = cmd {
        println!("{}", dir.display());
        return ExitCode::SUCCESS;
    }
    let (index, errors) = Index::load(&dir);
    for e in errors {
        warn!("{e}");
    }
    let Some(state_dir) = state::dir() else {
        warn!("neither XDG_STATE_HOME nor HOME is set: no order or hidden notes");
        return ExitCode::FAILURE;
    };
    let (mut state, errors) = State::load(&state_dir);
    for e in errors {
        warn!("{e}");
    }
    let result = match cmd {
        Cmd::New { .. } | Cmd::Focus { .. } | Cmd::Keep { .. } | Cmd::Path => {
            unreachable!("done above")
        }
        Cmd::List {
            app,
            title,
            hidden,
            all,
            json,
            ..
        } => {
            let window = (app.is_some() || title.is_some()).then(|| {
                Window::new(
                    app.clone().unwrap_or_default(),
                    title.clone().unwrap_or_default(),
                )
            });
            let which = match (hidden, all) {
                (true, _) => list::Which::Hidden,
                (_, true) => list::Which::All,
                _ => list::Which::Shown,
            };
            let notes = list::notes(&index, &state, window.as_ref(), which);
            for (path, note, _) in &notes {
                let problems = note.error.iter().chain(&note.warnings);
                for p in problems {
                    warn!("{}: {p}", path.display());
                }
            }
            match json {
                true => print!("{}", list::json(&notes)),
                false => print!("{}", list::text(&notes)),
            }
            Ok(())
        }
        Cmd::Hide { note } => find_note(&index, note).and_then(|path| {
            if !state.hidden.push(&path) {
                println!("{} is hidden already", path.display());
                return Ok(());
            }
            save(state.save_hidden(&state_dir), &state_dir)
        }),
        Cmd::Show { focus, note } => find_note(&index, note).and_then(|path| {
            match state.hidden.remove(&path) {
                true => save(state.save_hidden(&state_dir), &state_dir)?,
                false if !*focus => println!("{} isn't hidden", path.display()),
                false => {}
            }
            if *focus {
                let req = NoteRequest::Focus(Some(path.to_string_lossy().into_owned()));
                return match wl_shell_daemon::client_to(&req, MISSING) {
                    code if code == ExitCode::SUCCESS => Ok(()),
                    _ => Err(String::new()),
                };
            }
            Ok(())
        }),
        Cmd::Prune => {
            let (order, hidden) = state.prune(&index);
            let mut result = Ok(());
            if order > 0 {
                result = result.and(save(state.save_order(&state_dir), &state_dir));
            }
            if hidden > 0 {
                result = result.and(save(state.save_hidden(&state_dir), &state_dir));
            }
            println!("dropped {order} lines from the order and {hidden} from the hidden notes");
            result
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if !e.is_empty() {
                warn!("{e}");
            }
            ExitCode::FAILURE
        }
    }
}

/// The note `query` names (see find.rs), relative to the notes directory.
fn find_note(index: &Index, query: &str) -> Result<PathBuf, String> {
    let notes: Vec<(&Path, Option<&str>)> =
        (index.iter()).map(|(p, e)| (p, e.note.title())).collect();
    find::find(index.dir(), &notes, query).map(Path::to_path_buf)
}

/// A state file written, or the error saying where.
fn save(result: std::io::Result<()>, dir: &Path) -> Result<(), String> {
    result.map_err(|e| format!("{}: {e}", dir.display()))
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
