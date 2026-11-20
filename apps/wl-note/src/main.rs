//! wl-note: sticky notes along a screen edge, belonging to apps. See
//! PLAN.md for the design.

mod alarm;
mod app;
mod bar;
mod column;
mod config;
mod context;
mod create;
mod deck;
mod draft;
mod drawer;
mod drawerview;
mod editor;
mod files;
mod find;
mod input;
mod ipc;
mod list;
mod markdown;
mod options;
mod outputs;
mod papers;
mod reorder;
mod row;
mod scratch;
mod template;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wl_shell_config::ConfigFile;
use wl_shell_core::{Error, Shell};
use wl_shell_log::warn;
use wl_shell_notes::due::{self, Local};
use wl_shell_notes::{state, toml_edit, Color, Index, Note, State, Window};

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
            due,
        } => {
            if let Some(Err(e)) = color.as_deref().map(|c| Color::parse(c.trim())) {
                warn!("{e}");
                return ExitCode::FAILURE;
            }
            let due = match due.as_deref().map(|when| resolve_when(opts, when)) {
                Some(Ok(due)) => Some(due.datetime(&Local).to_string()),
                Some(Err(e)) => {
                    warn!("{e}");
                    return ExitCode::FAILURE;
                }
                None => None,
            };
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
                due,
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
        Cmd::Outputs => {
            return match outputs::run(opts.config.as_deref()) {
                Ok(list) => {
                    print!("{list}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    warn!("{e}");
                    ExitCode::FAILURE
                }
            };
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
        Cmd::New { .. } | Cmd::Focus { .. } | Cmd::Keep { .. } | Cmd::Path | Cmd::Outputs => {
            unreachable!("done above")
        }
        Cmd::Due { note, when } => find_note(&index, note).and_then(|path| {
            let due = match when.trim().eq_ignore_ascii_case("none") {
                true => None,
                false => Some(resolve_when(opts, when)?),
            };
            set_due(&dir.join(&path), &path, due)
        }),
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
            let now = due::now();
            match json {
                true => print!("{}", list::json(&notes, now)),
                false => print!("{}", list::text(&notes, now)),
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

/// `when` as `wl-note new --due` and `wl-note due` take it, by the config's
/// morning and evening.
fn resolve_when(opts: &Options, when: &str) -> Result<due::Due, String> {
    let config = Config::read(opts.config.as_deref()).map_err(|e| format!("config: {e}"))?;
    let (morning, evening) = (config.due.morning, config.due.evening);
    due::parse_when(when, due::now(), &Local, morning, evening)
}

/// Write `due` (None: none) into the note at `file`, whose name in the
/// notes directory is `name`.
fn set_due(file: &Path, name: &Path, due: Option<due::Due>) -> Result<(), String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut note = Note::parse(text);
    let datetime: Option<toml_edit::Datetime> = due.map(|d| d.datetime(&Local));
    note.set_due(datetime)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    wl_shell_notes::write_atomic(file, note.text())
        .map_err(|e| format!("{}: {e}", file.display()))?;
    match datetime {
        Some(dt) => println!("{} is due {dt}", name.display()),
        None => println!("{} has no due time", name.display()),
    }
    Ok(())
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

/// Run until the compositor disconnects or the session asks us to quit.
fn run(opts: &Options) -> Result<(), Error> {
    use calloop::signals::{Signal, Signals};

    // Block these signals before starting any worker threads.
    let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP])?;
    let note = WlNote::new(opts.config.clone())?;
    let (mut shell, mut event_loop) = Shell::connect(note)?;
    let signal = event_loop.get_signal();
    let quitting = signal.clone();
    event_loop
        .handle()
        .insert_source(signals, move |_, _, _| quitting.stop())?;
    shell.program.started(&mut shell.core)?;
    let result = event_loop.run(None, &mut shell, |shell| {
        shell.program.after_dispatch(&mut shell.core);
    });
    shell.program.shutdown();
    result.map_err(|e| format!("wayland connection error: {e}").into())
}
