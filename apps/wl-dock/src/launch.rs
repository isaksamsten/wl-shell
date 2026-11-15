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
//! `{ids}` (all of the app's windows). Everything is shell quoted.

use wl_shell_core::spawn::{fill, join, quote, uses};
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
}
