//! What the launcher can do to a subject (Tab on a match: a file or an
//! app). Pure, unit tested; launcher.rs keeps the levels and app.rs runs
//! the choice.
//!
//! The actions are a table per kind of subject (`FILE_ACTIONS`,
//! `APP_ACTIONS`), one entry each: an ID, a name, an icon (fixed, or
//! looked up from the subject), whether it applies to a subject, and
//! what it does, which is either something that closes the launcher
//! (opening a path with `open-command`, or opening the file with an app)
//! or a list of further choices, a level deeper. Adding an action is
//! adding an entry; nothing else in the launcher knows the list.
//!
//! For files, in this order:
//!
//! - **Open file** (Open folder) with `open-command`; its icon is the
//!   default app's for the file's type (a folder's is `folder-open`).
//! - **Open with…**: the apps for the file's type (mimeapps.rs: its
//!   `MimeType=` apps and parents, with the `mimeapps.list`
//!   associations), the default first, then by history. A folder only
//!   gets it if an app besides the default (the file manager) takes it.
//! - **Open containing directory**: `open-command` on the parent; its icon
//!   is the generic `folder-open`.
//! - **Open terminal here**: `open-terminal-command` in the folder (a
//!   file's parent), unless that is empty.
//! - **Copy path** to the clipboard, if the compositor has one.
//! - **Keep in Dock** (Remove from Dock, if it is): folders only.
//! - **Move to Trash**: `trash-command`, unless it is in the Trash.
//!
//! For apps, as in the dock's menu (menu.rs), in this order:
//!
//! - **Windows…**, with several: its windows, by title, the most recently
//!   focused first; Enter activates one.
//! - The desktop entry's actions (`[Desktop Action …]`, e.g. New Private
//!   Window), unless the app's `desktop-actions` is off.
//! - The app actions of its `[app.<id>]` table (`when = "running"` ones
//!   only while it has windows).
//! - **Keep in Dock** (Remove from Dock, if it is).
//! - **New Window**, while it has windows and can be launched.
//! - **Close** (Close All Windows, with several) while it has windows,
//!   with `close-command`.
//!
//! For windows (found by the launcher's `@` search), in this order:
//!
//! - **Activate** the window.
//! - **Close Window** with `close-command`, hidden without it.
//! - Its app's rows as above, if it has a desktop entry, but not
//!   Windows… (the search lists them already) and not a second Close
//!   when this is the app's only window.
//!
//! The desktop and `[app.<id>]` actions aren't in a table: they are the
//! app's own. Windows… is the table `APP_WINDOWS`, ahead of them; the
//! rest is `APP_ACTIONS`, after them. A window's own are
//! `WINDOW_ACTIONS`, ahead of its app's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wl_shell_render::files::FOLDER_ICONS;
use wl_shell_render::icons::{DesktopAction, IndexedApp};
use wl_shell_render::mimeapps::MimeApps;

use crate::config::{AppAction, When};
use crate::trash::EMPTY_ICON;

/// What the actions act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    File(File),
    App(App),
    Window(WindowSubject),
}

/// A file or folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub dir: bool,
}

/// An installed app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// Its desktop ID (without `.desktop`).
    pub id: String,
    pub name: String,
    /// Its desktop entry's actions to offer (none if turned off).
    pub desktop: Vec<DesktopAction>,
    /// The actions of its `[app.<id>]` table.
    pub commands: Vec<AppAction>,
    /// Its windows, the most recently focused first.
    pub windows: Vec<Window>,
    /// It has a desktop entry or a launch command.
    pub launchable: bool,
}

/// One of an app's windows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub id: String,
    pub title: String,
}

/// A window found by the launcher's window search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSubject {
    pub id: String,
    /// Its title; empty if it has none.
    pub title: String,
    /// Its app's ID as the compositor reports it.
    pub app_id: String,
    /// Its app's name, which an untitled window goes by.
    pub app_name: String,
    /// Its app, if it has a desktop entry (and so rows of its own);
    /// `windows` are all of its windows.
    pub app: Option<App>,
}

impl App {
    fn running(&self) -> bool {
        !self.windows.is_empty()
    }

    fn window_ids(&self) -> Vec<String> {
        self.windows.iter().map(|w| w.id.clone()).collect()
    }
}

impl Subject {
    /// Its name: the file's, the app's, or the window's title.
    pub fn name(&self) -> String {
        match self {
            Subject::File(File { path, .. }) => (path.file_name())
                .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
                .into_owned(),
            Subject::App(app) => app.name.clone(),
            Subject::Window(w) => match w.title.trim() {
                "" => w.app_name.clone(),
                title => title.to_string(),
            },
        }
    }
}

/// A file's type, from the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    /// The type and every type it is a kind of, nearest first; empty if
    /// unknown.
    pub types: Vec<String>,
    pub assoc: Arc<MimeApps>,
}

/// What the actions are worked out from.
pub struct Context<'a> {
    /// The installed apps.
    pub apps: &'a [IndexedApp],
    /// The subject's type; None for an app.
    pub file_type: Option<&'a FileType>,
    /// What an app's history (by desktop ID) is worth, for ordering.
    pub history: &'a dyn Fn(&str) -> f64,
    /// Whether the Trash takes the subject (it isn't in it).
    pub trash: bool,
    /// Whether the subject is kept in the dock.
    pub pinned: bool,
    /// Whether there is an `open-terminal-command`.
    pub terminal: bool,
    /// Whether the clipboard can be set.
    pub clipboard: bool,
    /// Whether there is a `close-command`.
    pub close: bool,
}

impl Context<'_> {
    /// The apps that open the subject's type, the default first.
    fn candidates(&self) -> Vec<usize> {
        (self.file_type).map_or_else(Vec::new, |ft| ft.assoc.candidates(&ft.types, self.apps))
    }
}

/// A choice's icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Theme icon names, best first.
    Names(Vec<String>),
    /// The app's (a desktop ID).
    App(String),
    /// An `Icon=` (a theme name or a path), else the app's.
    Entry { app: String, icon: Option<String> },
    /// The window's capture (its ID), the app's (a desktop ID) icon until
    /// it has a frame or without previews.
    Window { app: String, window: String },
}

/// What a choice does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Does {
    /// `open-command` on the path; closes the launcher.
    Open(PathBuf),
    /// Open the file with the app (a desktop ID); closes the launcher.
    OpenWith { app: String, path: PathBuf },
    /// `open-terminal-command` in the directory; closes the launcher.
    Terminal(PathBuf),
    /// Put the path on the clipboard; closes the launcher.
    CopyPath(PathBuf),
    /// Keep the folder in the dock (true) or not; closes the launcher.
    Pin { path: PathBuf, pinned: bool },
    /// Keep the app (a desktop ID) in the dock (true) or not; closes the
    /// launcher.
    PinApp { app: String, pinned: bool },
    /// Launch the app's (a desktop ID) desktop action; closes the
    /// launcher.
    Desktop { app: String, action: DesktopAction },
    /// Run an `[app.<id>]` action's command for the app (a desktop ID);
    /// closes the launcher.
    Command { app: String, command: String },
    /// Launch the app (a desktop ID) again; closes the launcher.
    NewWindow(String),
    /// Close these windows (with `close-command`); closes the launcher.
    Close(Vec<String>),
    /// Activate the app's (a desktop ID) window; closes the launcher.
    Activate { app: String, window: String },
    /// Activate the window (its ID) of the app with this ID as the
    /// compositor reports it; closes the launcher.
    ActivateWindow { app_id: String, window: String },
    /// `trash-command` on the path; closes the launcher.
    Trash(PathBuf),
    /// Further choices, a level deeper.
    Choose(Level),
}

/// One thing on the drum while choosing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    /// Unique in its level.
    pub id: String,
    pub name: String,
    pub icon: Icon,
    pub does: Does,
}

/// A level of choices: the subject's actions, or an action's choices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    /// Its name: the subject's, or the action's. Not shown (the pill
    /// shows icons), but there for logs.
    pub name: String,
    /// Shown while nothing is typed: for the subject's actions, the
    /// default's (the first) name.
    pub placeholder: String,
    pub choices: Vec<Choice>,
}

/// An entry of a table, for subjects `S`.
struct Action<S> {
    id: &'static str,
    name: fn(&S, &Context) -> String,
    icon: fn(&S, &Context) -> Icon,
    applies: fn(&S, &Context) -> bool,
    does: fn(&S, &Context) -> Does,
}

fn names(names: &[&str]) -> Icon {
    Icon::Names(names.iter().map(|n| n.to_string()).collect())
}

/// Keep in Dock's name, for a file or an app.
fn keep_name(cx: &Context) -> String {
    match cx.pinned {
        true => "Remove from Dock".into(),
        false => "Keep in Dock".into(),
    }
}

/// Keep in Dock's icon, for a file or an app.
fn keep_icon(cx: &Context) -> Icon {
    match cx.pinned {
        true => names(&["builtin:dock-remove"]),
        false => names(&["builtin:dock-keep"]),
    }
}

/// A file's actions, in the order they are shown.
const FILE_ACTIONS: &[Action<File>] = &[
    Action {
        id: "open",
        name: |f, _| match f.dir {
            true => "Open folder".into(),
            false => "Open file".into(),
        },
        // A folder's default app is the file manager, whose icon says
        // little: a folder instead.
        icon: |f, cx| match (f.dir, cx.candidates().first()) {
            (true, _) => names(&["folder-open", FOLDER_ICONS[0]]),
            (_, Some(&i)) => Icon::App(cx.apps[i].id.clone()),
            (_, None) => names(&["document-open", "text-x-generic"]),
        },
        applies: |_, _| true,
        does: |f, _| Does::Open(f.path.clone()),
    },
    Action {
        id: "open-with",
        name: |_, _| "Open with…".into(),
        icon: |_, _| names(&["document-open", "system-run", "application-x-executable"]),
        applies: |f, cx| match f.dir {
            // Something besides the file manager.
            true => cx.candidates().len() > 1,
            false => !cx.candidates().is_empty(),
        },
        does: |f, cx| {
            Does::Choose(Level {
                name: "Open with".into(),
                placeholder: "Apps".into(),
                choices: open_with(&f.path, cx),
            })
        },
    },
    Action {
        id: "open-directory",
        name: |_, _| "Open containing directory".into(),
        icon: |_, _| names(&["folder-open", FOLDER_ICONS[0]]),
        applies: |f, _| parent(&f.path).is_some(),
        does: |f, _| Does::Open(parent(&f.path).unwrap_or(&f.path).to_path_buf()),
    },
    Action {
        id: "open-terminal",
        name: |_, _| "Open terminal here".into(),
        icon: |_, _| names(&["utilities-terminal", "terminal", "org.gnome.Terminal"]),
        applies: |f, cx| cx.terminal && terminal_dir(f).is_some(),
        does: |f, _| Does::Terminal(terminal_dir(f).unwrap_or(&f.path).to_path_buf()),
    },
    Action {
        id: "copy-path",
        name: |_, _| "Copy path".into(),
        icon: |_, _| names(&["edit-copy", "xclipboard", "edit-copy-symbolic"]),
        applies: |_, cx| cx.clipboard,
        does: |f, _| Does::CopyPath(f.path.clone()),
    },
    Action {
        id: "pin",
        name: |_, cx| keep_name(cx),
        icon: |_, cx| keep_icon(cx),
        // Only folders are kept.
        applies: |f, _| f.dir,
        does: |f, cx| Does::Pin {
            path: f.path.clone(),
            pinned: !cx.pinned,
        },
    },
    Action {
        id: "trash",
        name: |_, _| "Move to Trash".into(),
        icon: |_, _| names(&[EMPTY_ICON]),
        // Not the root.
        applies: |f, cx| cx.trash && parent(&f.path).is_some(),
        does: |f, _| Does::Trash(f.path.clone()),
    },
];

/// An app's actions ahead of its own.
const APP_WINDOWS: &[Action<App>] = &[Action {
    id: "windows",
    name: |_, _| "Windows…".into(),
    icon: |_, _| names(&["preferences-system-windows", "window", "view-grid"]),
    // One is what Enter on the app activates already.
    applies: |a, _| a.windows.len() > 1,
    does: |a, _| {
        Does::Choose(Level {
            name: "Windows".into(),
            placeholder: "Windows".into(),
            choices: windows(a),
        })
    },
}];

/// An app's actions, in the order they are shown, after its own.
const APP_ACTIONS: &[Action<App>] = &[
    Action {
        id: "pin",
        name: |_, cx| keep_name(cx),
        icon: |_, cx| keep_icon(cx),
        applies: |_, _| true,
        does: |a, cx| Does::PinApp {
            app: a.id.clone(),
            pinned: !cx.pinned,
        },
    },
    Action {
        id: "new-window",
        name: |_, _| "New Window".into(),
        icon: |_, _| names(&["builtin:window-new"]),
        applies: |a, _| a.running() && a.launchable,
        does: |a, _| Does::NewWindow(a.id.clone()),
    },
    Action {
        id: "close",
        name: |a, _| match a.windows.len() {
            1 => "Close".into(),
            _ => "Close All Windows".into(),
        },
        icon: |a, _| names(&[match a.windows.len() {
            1 => "builtin:window-close",
            _ => "builtin:window-close-all",
        }]),
        applies: |a, cx| cx.close && a.running(),
        does: |a, _| Does::Close(a.window_ids()),
    },
];

/// A window's actions, ahead of its app's.
const WINDOW_ACTIONS: &[Action<WindowSubject>] = &[
    Action {
        id: "activate",
        name: |_, _| "Activate".into(),
        // The app's icon, by its desktop ID if it has one.
        icon: |w, _| Icon::App(w.app.as_ref().map_or(&w.app_id, |a| &a.id).clone()),
        applies: |_, _| true,
        does: |w, _| Does::ActivateWindow {
            app_id: w.app_id.clone(),
            window: w.id.clone(),
        },
    },
    Action {
        id: "close-window",
        name: |_, _| "Close Window".into(),
        icon: |_, _| names(&["builtin:window-close"]),
        applies: |_, cx| cx.close,
        does: |w, _| Does::Close(vec![w.id.clone()]),
    },
];

/// `path`'s directory, if it has one.
fn parent(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

/// Where a terminal opens for `file`: a folder, or a file's directory.
fn terminal_dir(file: &File) -> Option<&Path> {
    match file.dir {
        true => Some(&file.path),
        false => parent(&file.path),
    }
}

/// The actions that apply to `subject`.
pub fn actions(subject: &Subject, cx: &Context) -> Level {
    let choices = match subject {
        Subject::File(f) => choices(FILE_ACTIONS, f, cx),
        Subject::App(a) => {
            let mut all = choices(APP_WINDOWS, a, cx);
            all.extend(desktop_actions(a));
            all.extend(commands(a));
            all.extend(choices(APP_ACTIONS, a, cx));
            all
        }
        Subject::Window(w) => {
            let mut all = choices(WINDOW_ACTIONS, w, cx);
            if let Some(a) = &w.app {
                all.extend(desktop_actions(a));
                all.extend(commands(a));
                let mut own = choices(APP_ACTIONS, a, cx);
                // Close Window does it already for the app's only window.
                if a.windows.len() <= 1 {
                    own.retain(|c| c.id != "close");
                }
                all.extend(own);
            }
            all
        }
    };
    Level {
        name: subject.name(),
        placeholder: choices.first().map(|c| c.name.clone()).unwrap_or_default(),
        choices,
    }
}

/// `app`'s windows, in its order (the most recently focused first), by
/// title (an untitled one by the app's name).
fn windows(app: &App) -> Vec<Choice> {
    (app.windows.iter())
        .map(|w| Choice {
            id: format!("window:{}", w.id),
            name: match w.title.trim() {
                "" => app.name.clone(),
                title => title.to_string(),
            },
            icon: Icon::Window {
                app: app.id.clone(),
                window: w.id.clone(),
            },
            does: Does::Activate {
                app: app.id.clone(),
                window: w.id.clone(),
            },
        })
        .collect()
}

/// `app`'s desktop actions. Their IDs are prefixed, so they never meet
/// the table's.
fn desktop_actions(app: &App) -> Vec<Choice> {
    (app.desktop.iter())
        .map(|a| Choice {
            id: format!("desktop:{}", a.id),
            name: a.name.clone(),
            icon: Icon::Entry {
                app: app.id.clone(),
                icon: a.icon.clone(),
            },
            does: Does::Desktop {
                app: app.id.clone(),
                action: a.clone(),
            },
        })
        .collect()
}

/// `app`'s `[app.<id>]` actions that can run now, as the menu's: named,
/// with a command, and `when = "running"` ones only while it runs.
/// Their IDs are prefixed and numbered (names needn't be unique).
fn commands(app: &App) -> Vec<Choice> {
    (app.commands.iter())
        .filter(|a| app.running() || a.when == When::Always)
        .filter(|a| !a.name.trim().is_empty() && !a.command.trim().is_empty())
        .enumerate()
        .map(|(i, a)| Choice {
            id: format!("command:{i}"),
            name: a.name.trim().to_string(),
            icon: Icon::Entry {
                app: app.id.clone(),
                icon: a.icon.clone(),
            },
            does: Does::Command {
                app: app.id.clone(),
                command: a.command.clone(),
            },
        })
        .collect()
}

/// The entries of `table` that apply to `s`.
fn choices<S>(table: &[Action<S>], s: &S, cx: &Context) -> Vec<Choice> {
    (table.iter())
        .filter(|a| (a.applies)(s, cx))
        .map(|a| Choice {
            id: a.id.to_string(),
            name: (a.name)(s, cx),
            icon: (a.icon)(s, cx),
            does: (a.does)(s, cx),
        })
        .collect()
}

/// The apps that open `path`: the default first, then by history (ties
/// in order of preference).
fn open_with(path: &Path, cx: &Context) -> Vec<Choice> {
    let mut found = cx.candidates();
    if found.len() > 1 {
        let rest = &mut found[1..];
        let score = |i: &usize| (cx.history)(&cx.apps[*i].id);
        rest.sort_by(|a, b| score(b).total_cmp(&score(a)));
    }
    (found.into_iter())
        .map(|i| &cx.apps[i])
        .map(|a| Choice {
            id: a.id.clone(),
            name: a.name.clone(),
            icon: Icon::App(a.id.clone()),
            does: Does::OpenWith {
                app: a.id.clone(),
                path: path.to_path_buf(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_render::icons::AppEntry;
    use wl_shell_render::mime::DIRECTORY;
    use wl_shell_render::mimeapps::List;

    fn app(id: &str, types: &[&str]) -> IndexedApp {
        IndexedApp {
            id: id.into(),
            name: id.to_uppercase(),
            generic_name: None,
            keywords: Vec::new(),
            categories: Vec::new(),
            wm_class: None,
            program: None,
            mime_types: types.iter().map(|t| t.to_string()).collect(),
            entry: AppEntry::default(),
        }
    }

    fn apps() -> Vec<IndexedApp> {
        vec![
            app("evince", &["application/pdf"]),
            app("firefox", &["application/pdf", "text/html"]),
            app("nautilus", &["inode/directory"]),
            app("xournal", &["application/pdf"]),
        ]
    }

    fn file_type(types: &[&str], lists: &str) -> FileType {
        let mut assoc = MimeApps::default();
        assoc.push(List::parse(lists));
        FileType {
            types: types.iter().map(|t| t.to_string()).collect(),
            assoc: Arc::new(assoc),
        }
    }

    /// What `Context` knows besides the apps and the type.
    #[derive(Default, Clone, Copy)]
    struct Env {
        trash: bool,
        pinned: bool,
        terminal: bool,
        clipboard: bool,
        close: bool,
    }

    fn level(subject: &Subject, apps: &[IndexedApp], ft: &FileType) -> Level {
        level_in(subject, apps, ft, Env::default())
    }

    fn level_in(subject: &Subject, apps: &[IndexedApp], ft: &FileType, env: Env) -> Level {
        level_of(subject, apps, Some(ft), env)
    }

    fn level_of(
        subject: &Subject,
        apps: &[IndexedApp],
        file_type: Option<&FileType>,
        env: Env,
    ) -> Level {
        let history = |id: &str| if id == "xournal" { 5.0 } else { 0.0 };
        let cx = Context {
            apps,
            file_type,
            history: &history,
            trash: env.trash,
            pinned: env.pinned,
            terminal: env.terminal,
            clipboard: env.clipboard,
            close: env.close,
        };
        actions(subject, &cx)
    }

    const ALL: Env = Env {
        trash: true,
        pinned: false,
        terminal: true,
        clipboard: true,
        close: true,
    };

    fn does<'a>(level: &'a Level, id: &str) -> &'a Does {
        let choice = level.choices.iter().find(|c| c.id == id);
        &choice.unwrap_or_else(|| panic!("no {id}")).does
    }

    fn ids(level: &Level) -> Vec<&str> {
        level.choices.iter().map(|c| c.id.as_str()).collect()
    }

    fn file(path: &str, dir: bool) -> Subject {
        Subject::File(File {
            path: path.into(),
            dir,
        })
    }

    const PDF_DEFAULT: &str = "[Default Applications]\napplication/pdf=firefox.desktop\n\
                               inode/directory=nautilus.desktop\n";

    #[test]
    fn a_files_actions() {
        let apps = apps();
        let ft = file_type(&["application/pdf"], PDF_DEFAULT);
        let l = level(&file("/d/report.pdf", false), &apps, &ft);
        assert_eq!(l.name, "report.pdf");
        assert_eq!(l.placeholder, "Open file");
        assert_eq!(ids(&l), ["open", "open-with", "open-directory"]);
        let names: Vec<&str> = l.choices.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Open file", "Open with…", "Open containing directory"]
        );
        // Open file shows what will open it; the directory, a folder.
        assert_eq!(l.choices[0].icon, Icon::App("firefox".into()));
        assert_eq!(l.choices[0].does, Does::Open("/d/report.pdf".into()));
        assert_eq!(
            l.choices[2].icon,
            super::names(&["folder-open", FOLDER_ICONS[0]])
        );
        assert_eq!(l.choices[2].does, Does::Open("/d".into()));
    }

    #[test]
    fn open_with_the_default_first_then_by_history() {
        let apps = apps();
        let ft = file_type(&["application/pdf"], PDF_DEFAULT);
        let l = level(&file("/d/report.pdf", false), &apps, &ft);
        let Does::Choose(with) = &l.choices[1].does else {
            panic!("Open with… has choices");
        };
        assert_eq!(with.name, "Open with");
        assert_eq!(ids(with), ["firefox", "xournal", "evince"]);
        assert_eq!(with.choices[0].name, "FIREFOX");
        assert_eq!(
            with.choices[1].does,
            Does::OpenWith {
                app: "xournal".into(),
                path: "/d/report.pdf".into()
            }
        );
    }

    #[test]
    fn a_folders_actions() {
        let apps = apps();
        let ft = file_type(&[DIRECTORY], "");
        let l = level(&file("/d/sub", true), &apps, &ft);
        // Only the file manager takes folders: no Open with….
        assert_eq!(ids(&l), ["open", "open-directory", "pin"]);
        assert_eq!(l.choices[0].name, "Open folder");
        assert_eq!(l.placeholder, "Open folder");
        assert_eq!(
            l.choices[0].icon,
            super::names(&["folder-open", FOLDER_ICONS[0]])
        );
        // Another app that takes folders brings it back.
        let mut more = apps.clone();
        more.push(app("code", &[DIRECTORY]));
        let l = level(&file("/d/sub", true), &more, &ft);
        assert_eq!(ids(&l), ["open", "open-with", "open-directory", "pin"]);
    }

    #[test]
    fn unknown_types_and_the_root() {
        let apps = apps();
        // Nothing opens it: only opening, with a generic icon.
        let ft = file_type(&[], "");
        let l = level(&file("/blob", false), &apps, &ft);
        assert_eq!(ids(&l), ["open", "open-directory"]);
        assert!(matches!(l.choices[0].icon, Icon::Names(_)));
        assert_eq!(l.choices[1].does, Does::Open("/".into()));
        // The root has no directory to open.
        let ft = file_type(&[DIRECTORY], "");
        let l = level(&file("/", true), &apps, &ft);
        assert_eq!(ids(&l), ["open", "pin"]);
        assert_eq!(l.name, "/");
        // It can't be trashed, but a terminal opens in it.
        let l = level_in(&file("/", true), &apps, &ft, ALL);
        assert_eq!(ids(&l), ["open", "open-terminal", "copy-path", "pin"]);
        assert_eq!(does(&l, "open-terminal"), &Does::Terminal("/".into()));
    }

    #[test]
    fn a_files_other_actions() {
        let apps = apps();
        let ft = file_type(&["application/pdf"], PDF_DEFAULT);
        let f = file("/d/report.pdf", false);
        let l = level_in(&f, &apps, &ft, ALL);
        // Files aren't kept in the dock.
        assert_eq!(
            ids(&l),
            [
                "open",
                "open-with",
                "open-directory",
                "open-terminal",
                "copy-path",
                "trash"
            ]
        );
        // The terminal opens in its directory.
        assert_eq!(does(&l, "open-terminal"), &Does::Terminal("/d".into()));
        let path = PathBuf::from("/d/report.pdf");
        assert_eq!(does(&l, "copy-path"), &Does::CopyPath(path.clone()));
        assert_eq!(does(&l, "trash"), &Does::Trash(path));
        let names: Vec<&str> = l.choices[3..].iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Open terminal here", "Copy path", "Move to Trash"]);
        // Each goes when what it needs isn't there.
        for (env, gone) in [
            (
                Env {
                    trash: false,
                    ..ALL
                },
                "trash",
            ),
            (
                Env {
                    terminal: false,
                    ..ALL
                },
                "open-terminal",
            ),
            (
                Env {
                    clipboard: false,
                    ..ALL
                },
                "copy-path",
            ),
        ] {
            let l = level_in(&f, &apps, &ft, env);
            assert!(!ids(&l).contains(&gone), "{gone}");
            assert_eq!(l.choices.len(), 5);
        }
    }

    #[test]
    fn keeping_a_folder_in_the_dock() {
        let apps = apps();
        let ft = file_type(&[DIRECTORY], "");
        let sub = file("/d/sub", true);
        let l = level_in(&sub, &apps, &ft, ALL);
        let pin = l.choices.iter().find(|c| c.id == "pin").unwrap();
        assert_eq!(pin.name, "Keep in Dock");
        let path = PathBuf::from("/d/sub");
        let (p, pinned) = (path.clone(), true);
        assert_eq!(pin.does, Does::Pin { path: p, pinned });
        // A kept one comes off.
        let l = level_in(
            &sub,
            &apps,
            &ft,
            Env {
                pinned: true,
                ..ALL
            },
        );
        let pin = l.choices.iter().find(|c| c.id == "pin").unwrap();
        assert_eq!(pin.name, "Remove from Dock");
        let pinned = false;
        assert_eq!(pin.does, Does::Pin { path, pinned });
        // The trash comes last.
        assert_eq!(ids(&l).last(), Some(&"trash"));
    }

    /// Firefox, not running, with nothing of its own.
    fn firefox() -> App {
        App {
            id: "firefox".into(),
            name: "Firefox".into(),
            desktop: Vec::new(),
            commands: Vec::new(),
            windows: Vec::new(),
            launchable: true,
        }
    }

    #[test]
    fn keeping_an_app_in_the_dock() {
        let apps = apps();
        let firefox = Subject::App(firefox());
        let l = level_of(&firefox, &apps, None, ALL);
        assert_eq!(
            (l.name.as_str(), l.placeholder.as_str()),
            ("Firefox", "Keep in Dock")
        );
        assert_eq!(ids(&l), ["pin"]);
        let app = String::from("firefox");
        let pinned = true;
        assert_eq!(does(&l, "pin"), &Does::PinApp { app, pinned });
        let pin_icon = super::names(&["builtin:dock-keep"]);
        assert_eq!(l.choices[0].icon, pin_icon);
        // A kept one comes off.
        let env = Env {
            pinned: true,
            ..ALL
        };
        let l = level_of(&firefox, &apps, None, env);
        assert_eq!(l.choices[0].name, "Remove from Dock");
        let unpin_icon = super::names(&["builtin:dock-remove"]);
        assert_eq!(l.choices[0].icon, unpin_icon);
        let (app, pinned) = ("firefox".into(), false);
        assert_eq!(does(&l, "pin"), &Does::PinApp { app, pinned });
    }

    #[test]
    fn an_apps_desktop_actions_come_first() {
        let apps = apps();
        let action = |id: &str, name: &str, icon: Option<&str>| DesktopAction {
            id: id.into(),
            name: name.into(),
            icon: icon.map(Into::into),
            exec: format!("firefox --{id}"),
        };
        let private = action("private", "New Private Window", Some("private"));
        let firefox = Subject::App(App {
            desktop: vec![action("new-window", "New Window", None), private.clone()],
            ..firefox()
        });
        let l = level_of(&firefox, &apps, None, ALL);
        assert_eq!(ids(&l), ["desktop:new-window", "desktop:private", "pin"]);
        // Tab, Enter runs the first.
        assert_eq!(l.placeholder, "New Window");
        let app = String::from("firefox");
        assert_eq!(
            does(&l, "desktop:private"),
            &Does::Desktop {
                app: app.clone(),
                action: private
            }
        );
        // Its own icon, else the app's.
        let icon = |icon: Option<&str>| Icon::Entry {
            app: app.clone(),
            icon: icon.map(Into::into),
        };
        assert_eq!(l.choices[0].icon, icon(None));
        assert_eq!(l.choices[1].icon, icon(Some("private")));
    }

    #[test]
    fn an_apps_commands_and_windows() {
        let apps = apps();
        let command = |name: &str, when| AppAction {
            name: name.into(),
            command: format!("{name} {{app-id}}"),
            icon: None,
            when,
        };
        let stopped = App {
            commands: vec![
                command("profile", When::Always),
                command("hide", When::Running),
                // Nothing to run: never shown.
                AppAction {
                    command: " ".into(),
                    ..command("blank", When::Always)
                },
            ],
            ..firefox()
        };
        let l = level_of(&Subject::App(stopped.clone()), &apps, None, ALL);
        // Not running: no running-only commands, New Window or Close.
        assert_eq!(ids(&l), ["command:0", "pin"]);
        assert_eq!(l.placeholder, "profile");
        let app = String::from("firefox");
        assert_eq!(
            does(&l, "command:0"),
            &Does::Command {
                app: app.clone(),
                command: "profile {app-id}".into()
            }
        );
        // Running: all of it.
        let one = App {
            windows: vec![window("w1", "Inbox")],
            ..stopped.clone()
        };
        let l = level_of(&Subject::App(one.clone()), &apps, None, ALL);
        assert_eq!(
            ids(&l),
            ["command:0", "command:1", "pin", "new-window", "close"]
        );
        assert_eq!(does(&l, "new-window"), &Does::NewWindow(app));
        let close = l.choices.iter().find(|c| c.id == "close").unwrap();
        assert_eq!(close.name, "Close");
        assert_eq!(close.does, Does::Close(vec!["w1".into()]));
        let two = App {
            windows: vec![window("w1", "Inbox"), window("w2", "")],
            ..one.clone()
        };
        let l = level_of(&Subject::App(two.clone()), &apps, None, ALL);
        let close = l.choices.iter().find(|c| c.id == "close").unwrap();
        assert_eq!(close.name, "Close All Windows");
        assert_eq!(close.does, Does::Close(vec!["w1".into(), "w2".into()]));
        // With several, Windows… comes first.
        assert_eq!(ids(&l)[0], "windows");
        let Does::Choose(windows) = does(&l, "windows") else {
            panic!("Windows… has choices");
        };
        assert_eq!(ids(windows), ["window:w1", "window:w2"]);
        // An untitled one goes by the app's name.
        let names: Vec<&str> = windows.choices.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Inbox", "Firefox"]);
        let (app, window) = (String::from("firefox"), String::from("w2"));
        assert_eq!(
            windows.choices[1].icon,
            Icon::Window {
                app: app.clone(),
                window: window.clone()
            }
        );
        assert_eq!(windows.choices[1].does, Does::Activate { app, window });
        // Without a launch, or `close-command`, those go.
        let unlaunchable = App {
            launchable: false,
            ..one.clone()
        };
        let no_close = Env {
            close: false,
            ..ALL
        };
        let l = level_of(&Subject::App(unlaunchable), &apps, None, no_close);
        assert_eq!(ids(&l), ["command:0", "command:1", "pin"]);
    }

    fn window(id: &str, title: &str) -> Window {
        Window {
            id: id.into(),
            title: title.into(),
        }
    }

    /// A window `w1` of Firefox, which has `windows`.
    fn found(app: Option<App>, windows: &[(&str, &str)]) -> Subject {
        let app = app.map(|app| App {
            windows: windows.iter().map(|(id, t)| window(id, t)).collect(),
            ..app
        });
        Subject::Window(WindowSubject {
            id: "w1".into(),
            title: "Inbox".into(),
            app_id: "org.mozilla.firefox".into(),
            app_name: "Firefox".into(),
            app,
        })
    }

    #[test]
    fn a_windows_actions() {
        let apps = apps();
        let l = level_of(
            &found(Some(firefox()), &[("w1", "Inbox")]),
            &apps,
            None,
            ALL,
        );
        assert_eq!(
            (l.name.as_str(), l.placeholder.as_str()),
            ("Inbox", "Activate")
        );
        // Its own first, then its app's: with one window, one Close.
        assert_eq!(ids(&l), ["activate", "close-window", "pin", "new-window"]);
        let (app_id, window) = ("org.mozilla.firefox".to_string(), "w1".to_string());
        assert_eq!(
            does(&l, "activate"),
            &Does::ActivateWindow { app_id, window }
        );
        assert_eq!(l.choices[0].icon, Icon::App("firefox".into()));
        assert_eq!(does(&l, "close-window"), &Does::Close(vec!["w1".into()]));
        assert_eq!(l.choices[1].name, "Close Window");
        // The app's pin is the app's.
        let app = String::from("firefox");
        let pinned = true;
        assert_eq!(does(&l, "pin"), &Does::PinApp { app, pinned });
    }

    #[test]
    fn a_windows_apps_rows_without_windows() {
        let apps = apps();
        let two = [("w1", "Inbox"), ("w2", "Docs")];
        let l = level_of(&found(Some(firefox()), &two), &apps, None, ALL);
        // The search lists the windows already: no Windows…. With
        // several, closing the app's windows is another thing.
        assert_eq!(
            ids(&l),
            ["activate", "close-window", "pin", "new-window", "close"]
        );
        let close = l.choices.iter().find(|c| c.id == "close").unwrap();
        assert_eq!(close.name, "Close All Windows");
        assert_eq!(close.does, Does::Close(vec!["w1".into(), "w2".into()]));
    }

    #[test]
    fn a_windows_app_rows_follow_its_own() {
        let apps = apps();
        let action = DesktopAction {
            id: "private".into(),
            name: "New Private Window".into(),
            icon: None,
            exec: "firefox --private".into(),
        };
        let app = App {
            desktop: vec![action],
            commands: vec![AppAction {
                name: "profile".into(),
                command: "profile {app-id}".into(),
                icon: None,
                when: When::Always,
            }],
            ..firefox()
        };
        let l = level_of(&found(Some(app), &[("w1", "Inbox")]), &apps, None, ALL);
        assert_eq!(
            ids(&l),
            [
                "activate",
                "close-window",
                "desktop:private",
                "command:0",
                "pin",
                "new-window"
            ]
        );
    }

    #[test]
    fn a_window_without_a_desktop_entry() {
        let apps = apps();
        let l = level_of(&found(None, &[]), &apps, None, ALL);
        assert_eq!(ids(&l), ["activate", "close-window"]);
        // Its icon is the app ID's.
        assert_eq!(l.choices[0].icon, Icon::App("org.mozilla.firefox".into()));
        // Without a `close-command`, only activating is left.
        let no_close = Env {
            close: false,
            ..ALL
        };
        let l = level_of(&found(None, &[]), &apps, None, no_close);
        assert_eq!(ids(&l), ["activate"]);
        assert_eq!(l.placeholder, "Activate");
        // An untitled window goes by its app's name.
        let Subject::Window(w) = found(None, &[]) else {
            unreachable!();
        };
        let untitled = Subject::Window(WindowSubject {
            title: "  ".into(),
            ..w
        });
        assert_eq!(untitled.name(), "Firefox");
    }
}
