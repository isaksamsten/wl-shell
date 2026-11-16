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
//!   default app's for the file's type.
//! - **Open with…**: the apps for the file's type (mimeapps.rs: its
//!   `MimeType=` apps and parents, with the `mimeapps.list`
//!   associations), the default first, then by history. A folder only
//!   gets it if an app besides the default (the file manager) takes it.
//! - **Open containing directory**: `open-command` on the parent; its icon
//!   is the default app's for folders, else `folder-open`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wl_shell_render::files::FOLDER_ICONS;
use wl_shell_render::icons::IndexedApp;
use wl_shell_render::mime::DIRECTORY;
use wl_shell_render::mimeapps::MimeApps;

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
}

impl Context<'_> {
    /// The apps that open the subject's type, the default first.
    fn candidates(&self) -> Vec<usize> {
        (self.file_type.assoc).candidates(&self.file_type.types, self.apps)
    }

    /// The default app for `types`, by desktop ID.
    fn default_for(&self, types: &[String]) -> Option<String> {
        let i = self.file_type.assoc.default_app(types, self.apps)?;
        Some(self.apps[i].id.clone())
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
    /// Shown while nothing is typed.
    pub placeholder: &'static str,
    pub choices: Vec<Choice>,
}

/// An entry of the table.
struct Action {
    id: &'static str,
    name: fn(&Subject) -> String,
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
        name: |s| match s {
            Subject::File { dir: true, .. } => "Open folder".into(),
            Subject::File { .. } => "Open file".into(),
        },
        icon: |s, cx| match (cx.candidates().first(), s) {
            (Some(&i), _) => Icon::App(cx.apps[i].id.clone()),
            (None, Subject::File { dir: true, .. }) => names(&["folder-open", FOLDER_ICONS[0]]),
            (None, _) => names(&["document-open", "text-x-generic"]),
        },
        applies: |_, _| true,
        does: |s, _| match s {
            Subject::File { path, .. } => Does::Open(path.clone()),
        },
    },
    Action {
        id: "open-with",
        name: |_| "Open with…".into(),
        icon: |_, _| names(&["document-open", "system-run", "application-x-executable"]),
        applies: |s, cx| match s {
            // Something besides the file manager.
            Subject::File { dir: true, .. } => cx.candidates().len() > 1,
            Subject::File { .. } => !cx.candidates().is_empty(),
        },
        does: |s, cx| match s {
            Subject::File { path, .. } => Does::Choose(Level {
                name: "Open with".into(),
                placeholder: "Apps",
                choices: open_with(path, cx),
            }),
        },
    },
    Action {
        id: "open-directory",
        name: |_| "Open containing directory".into(),
        icon: |_, cx| match cx.default_for(&[DIRECTORY.to_string()]) {
            Some(id) => Icon::App(id),
            None => names(&["folder-open", FOLDER_ICONS[0]]),
        },
        applies: |s, _| match s {
            Subject::File { path, .. } => parent(path).is_some(),
        },
        does: |s, _| match s {
            Subject::File { path, .. } => Does::Open(parent(path).unwrap_or(path).to_path_buf()),
        },
    },
];

/// `path`'s directory, if it has one.
fn parent(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

/// The actions that apply to `subject`.
pub fn actions(subject: &Subject, cx: &Context) -> Level {
    let choices = (ACTIONS.iter())
        .filter(|a| (a.applies)(subject, cx))
        .map(|a| Choice {
            id: a.id.to_string(),
            name: (a.name)(subject),
            icon: (a.icon)(subject, cx),
            does: (a.does)(subject, cx),
        })
        .collect();
    Level {
        name: subject.name(),
        placeholder: "Actions",
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

    fn level(subject: &Subject, apps: &[IndexedApp], ft: &FileType) -> Level {
        let history = |id: &str| if id == "xournal" { 5.0 } else { 0.0 };
        let cx = Context {
            apps,
            file_type: ft,
            history: &history,
        };
        actions(subject, &cx)
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
        assert_eq!(ids(&l), ["open", "open-with", "open-directory"]);
        let names: Vec<&str> = l.choices.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["Open file", "Open with…", "Open containing directory"]
        );
        // Open file shows what will open it; the directory, the file manager.
        assert_eq!(l.choices[0].icon, Icon::App("firefox".into()));
        assert_eq!(l.choices[0].does, Does::Open("/d/report.pdf".into()));
        assert_eq!(l.choices[2].icon, Icon::App("nautilus".into()));
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
        assert_eq!(ids(&l), ["open", "open-directory"]);
        assert_eq!(l.choices[0].name, "Open folder");
        assert_eq!(l.choices[0].icon, Icon::App("nautilus".into()));
        // Another app that takes folders brings it back.
        let mut more = apps.clone();
        more.push(app("code", &[DIRECTORY]));
        let l = level(&file("/d/sub", true), &more, &ft);
        assert_eq!(ids(&l), ["open", "open-with", "open-directory"]);
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
        assert_eq!(ids(&l), ["open"]);
        assert_eq!(l.name, "/");
    }
}
