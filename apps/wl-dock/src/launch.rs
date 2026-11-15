//! Launching apps and running commands: what to run. Pure, unit tested;
//! app.rs runs them detached (`wl_shell_core::spawn`).
//!
//! An app is launched from the desktop entry found for its app ID: its
//! `Exec=` split into argv with the field codes removed, run directly, or
//! under `terminal` for `Terminal=true` entries. `launch-command` (the
//! app's own, else the global one), if set, runs instead with `sh -c`,
//! with `{exec}` (the cleaned Exec line, shell quoted), `{desktop-id}` and
//! `{app-id}` filled in. Without a desktop entry, only a `launch-command`
//! that needs neither `{exec}` nor `{desktop-id}` can launch the app. A
//! desktop action runs the same way, with its `Exec=` for the entry's.
//!
//! Window commands (`activate-command`, `close-command`) get the window's
//! `{id}`; app commands (menu actions, middle click, scrolling) get
//! `{app-id}`, `{id}` (the most recent window, empty without one) and
//! `{ids}` (all of the app's windows); `open-command` gets `{path}`;
//! `copy-command` and `move-command` get `{paths}` and `{dest}`,
//! `trash-command` `{paths}`. Everything is shell quoted.
//!
//! Files dropped on an app open with its `[app.<id>] drop` command
//! (`{files}`), else with a `launch-command` that has `{files}`, else with
//! its entry's `Exec=` if that has a file field code: one launch for all
//! of them with `%F` or `%U`, one per file with `%f` or `%u` (`%u` and
//! `%U` get `file://` URIs). An app that has none of these can't open
//! files.

use std::path::{Path, PathBuf};

use wl_shell_core::spawn::{fill, join, quote, uses};
use wl_shell_render::files::file_uri;
use wl_shell_render::icons::AppEntry;

/// A command to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// With `sh -c`.
    Shell(String),
    /// As is.
    Argv(Vec<String>),
}

/// How to launch `app_id`, whose desktop entry is `entry` (None: it has
/// none).
pub fn launch(
    app_id: &str,
    entry: Option<&AppEntry>,
    launch_command: Option<&str>,
    terminal: &str,
) -> Result<Command, String> {
    let exec = entry.map(|e| exec(e, terminal)).transpose()?;
    let Some(template) = launch_command.map(str::trim).filter(|c| !c.is_empty()) else {
        return exec.ok_or_else(|| format!("no desktop entry found for '{app_id}'"));
    };
    let exec = match (&exec, uses(template, "exec")) {
        (Some(Command::Argv(argv)), _) => join(argv),
        (Some(Command::Shell(line)), _) => line.clone(),
        (None, true) => return Err(format!("no desktop entry found for '{app_id}'")),
        (None, false) => String::new(),
    };
    let desktop_id = match (entry, uses(template, "desktop-id")) {
        (Some(e), _) => quote(&e.desktop_id),
        (None, true) => return Err(format!("no desktop entry found for '{app_id}'")),
        (None, false) => String::new(),
    };
    let app_id = quote(app_id);
    Ok(Command::Shell(fill(
        template,
        &[
            ("exec", &exec),
            ("desktop-id", &desktop_id),
            ("app-id", &app_id),
        ],
    )))
}

/// The entry's own command: its argv, or under `terminal` (a command
/// line the argv is appended to) if it wants one.
fn exec(entry: &AppEntry, terminal: &str) -> Result<Command, String> {
    let argv = entry.argv(&[])?;
    match entry.terminal {
        true => Ok(Command::Shell(format!(
            "{} {}",
            terminal.trim(),
            join(&argv)
        ))),
        false => Ok(Command::Argv(argv)),
    }
}

/// `activate-command` or `close-command` for the window `id`.
pub fn window_command(template: &str, id: &str) -> String {
    fill(template, &[("id", &quote(id))])
}

/// `open-command` for `path` (or a URI, such as `trash:///`).
pub fn path_command(template: &str, path: &str) -> String {
    fill(template, &[("path", &quote(path))])
}

/// `paths` shell quoted, separated by spaces.
fn quoted(paths: &[PathBuf]) -> String {
    let quoted: Vec<String> = (paths.iter())
        .map(|p| quote(&p.to_string_lossy()))
        .collect();
    quoted.join(" ")
}

/// `copy-command`, `move-command` or `trash-command` for `paths`, into
/// `dest` if it has one.
pub fn paths_command(template: &str, paths: &[PathBuf], dest: Option<&Path>) -> String {
    let dest = dest.map_or(String::new(), |d| quote(&d.to_string_lossy()));
    fill(template, &[("paths", &quoted(paths)), ("dest", &dest)])
}

/// How `app_id` (with the desktop entry `entry`) opens `files`: its
/// `drop` command, else `launch_command` if it has `{files}`, else its
/// entry's `Exec=`. Err if it can't open files.
pub fn open_with(
    app_id: &str,
    entry: Option<&AppEntry>,
    drop: Option<&str>,
    launch_command: Option<&str>,
    terminal: &str,
    files: &[PathBuf],
) -> Result<Vec<Command>, String> {
    if files.is_empty() {
        return Err("nothing to open".into());
    }
    let files_arg = quoted(files);
    if let Some(drop) = drop.map(str::trim).filter(|d| !d.is_empty()) {
        let values = [("files", files_arg.as_str()), ("app-id", &quote(app_id))];
        return Ok(vec![Command::Shell(fill(drop, &values))]);
    }
    if let Some(template) = launch_command.filter(|t| uses(t, "files")) {
        return match launch(app_id, entry, Some(template), terminal)? {
            Command::Shell(line) => Ok(vec![Command::Shell(fill(&line, &[("files", &files_arg)]))]),
            Command::Argv(_) => Err(format!("cannot open files with '{app_id}'")),
        };
    }
    let entry = entry.ok_or_else(|| format!("no desktop entry found for '{app_id}'"))?;
    let code = (entry.file_code()).ok_or_else(|| format!("'{app_id}' doesn't open files"))?;
    let args: Vec<String> = (files.iter())
        .map(|f| match code {
            'u' | 'U' => file_uri(f),
            _ => f.to_string_lossy().into_owned(),
        })
        .collect();
    let runs: Vec<Vec<String>> = match code {
        'F' | 'U' => vec![args],
        _ => args.into_iter().map(|a| vec![a]).collect(),
    };
    (runs.iter())
        .map(|files| {
            let argv = entry.argv(files)?;
            Ok(match entry.terminal {
                true => Command::Shell(format!("{} {}", terminal.trim(), join(&argv))),
                false => Command::Argv(argv),
            })
        })
        .collect()
}

/// An app command (a menu action, middle click or scroll) of `app_id`,
/// whose most recent window is `id` and whose windows are `ids`.
pub fn app_command(template: &str, app_id: &str, id: Option<&str>, ids: &[String]) -> String {
    let ids: Vec<String> = ids.iter().map(|i| quote(i)).collect();
    fill(
        template,
        &[
            ("app-id", &quote(app_id)),
            ("id", &id.map(quote).unwrap_or_default()),
            ("ids", &ids.join(" ")),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(exec: &str, terminal: bool) -> AppEntry {
        AppEntry {
            desktop_id: "org.example.App.desktop".into(),
            name: "App".into(),
            exec: Some(exec.into()),
            terminal,
            ..Default::default()
        }
    }

    fn argv(args: &[&str]) -> Command {
        Command::Argv(args.iter().map(|a| a.to_string()).collect())
    }

    #[test]
    fn from_the_desktop_entry() {
        let e = entry("app --new %U", false);
        assert_eq!(
            launch("app", Some(&e), None, "foot"),
            Ok(argv(&["app", "--new"]))
        );
        let e = entry("htop", true);
        assert_eq!(
            launch("htop", Some(&e), None, "alacritty -e "),
            Ok(Command::Shell("alacritty -e htop".into()))
        );
        assert!(launch("nope", None, None, "foot").is_err());
    }

    #[test]
    fn with_a_launch_command() {
        let e = entry(r#""/opt/My App/app" %u"#, false);
        assert_eq!(
            launch("my-app", Some(&e), Some("uwsm app -- {desktop-id}"), "foot"),
            Ok(Command::Shell("uwsm app -- org.example.App.desktop".into()))
        );
        assert_eq!(
            launch(
                "my-app",
                Some(&e),
                Some("niri msg action spawn -- {exec}"),
                "foot"
            ),
            Ok(Command::Shell(
                "niri msg action spawn -- '/opt/My App/app'".into()
            ))
        );
        // Without an entry, only if it doesn't need one.
        assert_eq!(
            launch("my app", None, Some("gtk-launch {app-id}"), "foot"),
            Ok(Command::Shell("gtk-launch 'my app'".into()))
        );
        assert!(launch("x", None, Some("run {exec}"), "foot").is_err());
        assert!(launch("x", None, Some("run {desktop-id}"), "foot").is_err());
        // Blank: as if unset.
        assert_eq!(
            launch("app", Some(&entry("app", false)), Some(" "), "foot"),
            Ok(argv(&["app"]))
        );
    }

    #[test]
    fn desktop_actions() {
        let mut e = entry("app %U", true);
        e.actions.push(wl_shell_render::icons::DesktopAction {
            id: "new".into(),
            name: "New Window".into(),
            icon: None,
            exec: "app --new-window %u".into(),
        });
        let action = e.with_action(&e.actions[0]);
        assert_eq!(
            launch("app", Some(&action), None, "foot"),
            Ok(Command::Shell("foot app --new-window".into()))
        );
        assert_eq!(
            launch("app", Some(&action), Some("spawn -- {exec}"), "foot"),
            Ok(Command::Shell("spawn -- foot app --new-window".into()))
        );
    }

    #[test]
    fn window_commands() {
        assert_eq!(
            window_command("niri msg action focus-window --id {id}", "42"),
            "niri msg action focus-window --id 42"
        );
        assert_eq!(window_command("focus {id}", "a'b c"), r"focus 'a'\''b c'");
        assert_eq!(
            path_command("xdg-open {path}", "trash:///"),
            "xdg-open trash:///"
        );
        assert_eq!(path_command("open {path}", "/a b"), "open '/a b'");
    }

    #[test]
    fn app_commands() {
        let ids = ["1".to_string(), "a b".to_string()];
        assert_eq!(
            app_command("x {app-id} {id} [{ids}]", "my app", Some("a b"), &ids),
            "x 'my app' 'a b' [1 'a b']"
        );
        assert_eq!(
            app_command("playerctl -p spotify next {id}", "spotify", None, &[]),
            "playerctl -p spotify next "
        );
    }

    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn paths_commands() {
        let p = files(&["/a b", "/c"]);
        assert_eq!(
            paths_command("mv -n -- {paths} {dest}", &p, Some(Path::new("/d e"))),
            "mv -n -- '/a b' /c '/d e'"
        );
        assert_eq!(
            paths_command("gio trash -- {paths}", &p, None),
            "gio trash -- '/a b' /c"
        );
    }

    #[test]
    fn opening_files() {
        let f = files(&["/a b.txt", "/c"]);
        let open = |e: &AppEntry, drop: Option<&str>, launch: Option<&str>| {
            open_with("app", Some(e), drop, launch, "foot", &f)
        };
        // All at once with %F, as paths; %U gets URIs.
        assert_eq!(
            open(&entry("app %F", false), None, None),
            Ok(vec![argv(&["app", "/a b.txt", "/c"])])
        );
        assert_eq!(
            open(&entry("app --new %U", false), None, None),
            Ok(vec![argv(&[
                "app",
                "--new",
                "file:///a%20b.txt",
                "file:///c"
            ])])
        );
        // One launch per file with %f.
        assert_eq!(
            open(&entry("app %f", false), None, None),
            Ok(vec![argv(&["app", "/a b.txt"]), argv(&["app", "/c"])])
        );
        // In a terminal.
        assert_eq!(
            open(&entry("vim %F", true), None, None),
            Ok(vec![Command::Shell("foot vim '/a b.txt' /c".into())])
        );
        // Without a field code: it can't.
        assert!(open(&entry("app --new-window", false), None, None).is_err());
        assert!(open_with("app", None, None, None, "foot", &f).is_err());
        // A drop command, or a launch command with {files}, wins.
        assert_eq!(
            open(&entry("app", false), Some("open-in {app-id} {files}"), None),
            Ok(vec![Command::Shell("open-in app '/a b.txt' /c".into())])
        );
        assert_eq!(
            open(
                &entry("app %U", false),
                None,
                Some("uwsm app -- {desktop-id} {files}")
            ),
            Ok(vec![Command::Shell(
                "uwsm app -- org.example.App.desktop '/a b.txt' /c".into()
            )])
        );
        // A launch command without {files} doesn't open them.
        assert_eq!(
            open(
                &entry("app %F", false),
                None,
                Some("uwsm app -- {desktop-id}")
            ),
            Ok(vec![argv(&["app", "/a b.txt", "/c"])])
        );
        assert!(open_with("app", None, None, None, "foot", &[]).is_err());
    }
}
