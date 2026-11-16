//! What the launcher can do to a subject (Tab on a file match). Pure,
//! unit tested; launcher.rs keeps the levels and app.rs runs the choice.
//!
//! The actions are a table (`ACTIONS`), one entry each: an ID, a name,
//! an icon (fixed, or looked up from the subject), whether it applies to
//! a subject, and what it does, which is either something that closes
//! the launcher (opening a path with `open-command`, or opening the file
//! with an app) or a list of further choices, a level deeper. Adding an
//! action is adding an entry; nothing else in the launcher knows the
//! list.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wl_shell_render::files::FOLDER_ICONS;
use wl_shell_render::icons::IndexedApp;
use wl_shell_render::mimeapps::MimeApps;

use crate::trash::EMPTY_ICON;

/// What the actions act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    File { path: PathBuf, dir: bool },
}

impl Subject {
    /// Its name: the file's.
    pub fn name(&self) -> String {
        match self {
            Subject::File { path, .. } => (path.file_name())
                .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
                .into_owned(),
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
    /// The subject's type.
    pub file_type: &'a FileType,
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
}

impl Context<'_> {
    /// The apps that open the subject's type, the default first.
    fn candidates(&self) -> Vec<usize> {
        (self.file_type.assoc).candidates(&self.file_type.types, self.apps)
    }
}

/// A choice's icon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Icon {
    /// Theme icon names, best first.
    Names(Vec<String>),
    /// The app's (a desktop ID).
    App(String),
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

/// An entry of the table.
struct Action {
    id: &'static str,
    name: fn(&Subject, &Context) -> String,
    icon: fn(&Subject, &Context) -> Icon,
    applies: fn(&Subject, &Context) -> bool,
    does: fn(&Subject, &Context) -> Does,
}

fn names(names: &[&str]) -> Icon {
    Icon::Names(names.iter().map(|n| n.to_string()).collect())
}

/// The actions, in the order they are shown.
const ACTIONS: &[Action] = &[
    Action {
        id: "open",
        name: |s, _| match s {
            Subject::File { dir: true, .. } => "Open folder".into(),
            Subject::File { .. } => "Open file".into(),
        },
        // A folder's default app is the file manager, whose icon says
        // little: a folder instead.
        icon: |s, cx| match (s, cx.candidates().first()) {
            (Subject::File { dir: true, .. }, _) => names(&["folder-open", FOLDER_ICONS[0]]),
            (_, Some(&i)) => Icon::App(cx.apps[i].id.clone()),
            (_, None) => names(&["document-open", "text-x-generic"]),
        },
        applies: |_, _| true,
        does: |s, _| match s {
            Subject::File { path, .. } => Does::Open(path.clone()),
        },
    },
    Action {
        id: "open-with",
        name: |_, _| "Open with…".into(),
        icon: |_, _| names(&["document-open", "system-run", "application-x-executable"]),
        applies: |s, cx| match s {
            // Something besides the file manager.
            Subject::File { dir: true, .. } => cx.candidates().len() > 1,
            Subject::File { .. } => !cx.candidates().is_empty(),
        },
        does: |s, cx| match s {
            Subject::File { path, .. } => Does::Choose(Level {
                name: "Open with".into(),
                placeholder: "Apps".into(),
                choices: open_with(path, cx),
            }),
        },
    },
    Action {
        id: "open-directory",
        name: |_, _| "Open containing directory".into(),
        icon: |_, _| names(&["folder-open", FOLDER_ICONS[0]]),
        applies: |s, _| match s {
            Subject::File { path, .. } => parent(path).is_some(),
        },
        does: |s, _| match s {
            Subject::File { path, .. } => Does::Open(parent(path).unwrap_or(path).to_path_buf()),
        },
    },
    Action {
        id: "open-terminal",
        name: |_, _| "Open terminal here".into(),
        icon: |_, _| names(&["utilities-terminal", "terminal", "org.gnome.Terminal"]),
        applies: |s, cx| cx.terminal && terminal_dir(s).is_some(),
        does: |s, _| match s {
            Subject::File { path, .. } => {
                Does::Terminal(terminal_dir(s).unwrap_or(path).to_path_buf())
            }
        },
    },
    Action {
        id: "copy-path",
        name: |_, _| "Copy path".into(),
        icon: |_, _| names(&["edit-copy", "xclipboard", "edit-copy-symbolic"]),
        applies: |_, cx| cx.clipboard,
        does: |s, _| match s {
            Subject::File { path, .. } => Does::CopyPath(path.clone()),
        },
    },
    Action {
        id: "pin",
        name: |_, cx| match cx.pinned {
            true => "Remove from Dock".into(),
            false => "Keep in Dock".into(),
        },
        icon: |_, cx| match cx.pinned {
            true => names(&["view-unpin", "list-remove", "builtin:unpin"]),
            false => names(&["view-pin", "pin", "builtin:pin"]),
        },
        // Only folders are kept.
        applies: |s, _| matches!(s, Subject::File { dir: true, .. }),
        does: |s, cx| match s {
            Subject::File { path, .. } => Does::Pin {
                path: path.clone(),
                pinned: !cx.pinned,
            },
        },
    },
    Action {
        id: "trash",
        name: |_, _| "Move to Trash".into(),
        icon: |_, _| names(&[EMPTY_ICON]),
        applies: |s, cx| match s {
            // Not the root.
            Subject::File { path, .. } => cx.trash && parent(path).is_some(),
        },
        does: |s, _| match s {
            Subject::File { path, .. } => Does::Trash(path.clone()),
        },
    },
];

/// `path`'s directory, if it has one.
fn parent(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

/// Where a terminal opens for `subject`: a folder, or a file's directory.
fn terminal_dir(subject: &Subject) -> Option<&Path> {
    match subject {
        Subject::File { path, dir: true } => Some(path),
        Subject::File { path, .. } => parent(path),
    }
}

/// The actions that apply to `subject`.
pub fn actions(subject: &Subject, cx: &Context) -> Level {
    let choices: Vec<Choice> = (ACTIONS.iter())
        .filter(|a| (a.applies)(subject, cx))
        .map(|a| Choice {
            id: a.id.to_string(),
            name: (a.name)(subject, cx),
            icon: (a.icon)(subject, cx),
            does: (a.does)(subject, cx),
        })
        .collect();
    Level {
        name: subject.name(),
        placeholder: choices.first().map(|c| c.name.clone()).unwrap_or_default(),
        choices,
    }
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
    }

    fn level(subject: &Subject, apps: &[IndexedApp], ft: &FileType) -> Level {
        level_in(subject, apps, ft, Env::default())
    }

    fn level_in(subject: &Subject, apps: &[IndexedApp], ft: &FileType, env: Env) -> Level {
        let history = |id: &str| if id == "xournal" { 5.0 } else { 0.0 };
        let cx = Context {
            apps,
            file_type: ft,
            history: &history,
            trash: env.trash,
            pinned: env.pinned,
            terminal: env.terminal,
            clipboard: env.clipboard,
        };
        actions(subject, &cx)
    }

    const ALL: Env = Env {
        trash: true,
        pinned: false,
        terminal: true,
        clipboard: true,
    };

    fn does<'a>(level: &'a Level, id: &str) -> &'a Does {
        let choice = level.choices.iter().find(|c| c.id == id);
        &choice.unwrap_or_else(|| panic!("no {id}")).does
    }

    fn ids(level: &Level) -> Vec<&str> {
        level.choices.iter().map(|c| c.id.as_str()).collect()
    }

    fn file(path: &str, dir: bool) -> Subject {
        Subject::File {
            path: path.into(),
            dir,
        }
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
}
