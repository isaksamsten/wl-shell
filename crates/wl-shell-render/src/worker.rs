//! A worker thread for everything slow: loading fonts, desktop entries and
//! icons, and rendering labels, title pills, text lines, letter tiles,
//! speech bubbles, sheets of paper and documents. Results come back
//! through a calloop channel, and the main thread turns them into shm
//! buffers.
//!
//! The worker lives as long as the daemon, so its caches (font system,
//! desktop entries, decoded icons) stay warm. The daemon asks it for every
//! app's icon and label ahead of time, so a show has them on its first
//! frame.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use calloop::channel;

use crate::bubble::{self, Bubble};
use crate::doc::{self, Doc};
use crate::files::{self, FileEntry, FolderRequest};
use crate::icons::{self, Icons};
use crate::image::Image;
use crate::mime::MimeDb;
use crate::paper;
use crate::text::{self, Text};

enum Job {
    /// The icon for `key`; a letter tile in `font` if there is none.
    Icon { key: icons::Key, font: String },
    /// An icon by name alone; nothing if there is none.
    Named(icons::NamedIcon),
    /// An app's name label.
    Label(LabelKey),
    /// A pill with arbitrary text (a window title).
    Pill { tag: u64, request: PillRequest },
    /// A line of text without a pill (a menu row).
    Line { tag: u64, request: PillRequest },
    /// A speech bubble.
    Bubble { tag: u64, bubble: Bubble },
    /// What it takes to launch an app.
    Entry(String),
    /// Every installed app to list.
    Apps,
    /// Reload the desktop entries if they changed.
    Refresh,
    /// A folder's entries.
    Folder { tag: u64, request: FolderRequest },
    /// The icons of a file (or folder): its thumbnail, or its type's.
    FileIcons { path: std::path::PathBuf, dir: bool },
    /// A file's type, its parents, and the apps associated with types.
    FileType { path: std::path::PathBuf, dir: bool },
    /// A sheet of paper (wl-note).
    Paper(paper::Paper),
    /// A document (wl-note).
    Doc { tag: u64, doc: Doc },
    /// Forget decoded icons, e.g. because the config changed.
    ClearIcons,
}

/// What a label is rendered for.
#[derive(Clone, Debug, PartialEq)]
pub struct LabelKey {
    pub app_id: String,
    /// The name to show instead of the desktop entry's.
    pub name: Option<String>,
    /// Logical pixels.
    pub max_width: i32,
    pub scale: i32,
    pub style: text::Style,
}

/// What a pill is rendered for. A result is only used while it is still
/// what the overlay wants.
#[derive(Clone, Debug, PartialEq)]
pub struct PillRequest {
    pub text: String,
    /// Byte ranges of `text` in other colors than the style's.
    pub spans: Vec<(std::ops::Range<usize>, u32)>,
    /// Logical pixels.
    pub max_width: i32,
    pub scale: i32,
    pub style: text::Style,
}

pub enum Done {
    Icon {
        key: icons::Key,
        image: Arc<Image>,
    },
    /// An icon by name; None if there is none.
    Named {
        key: icons::NamedIcon,
        image: Option<Arc<Image>>,
    },
    Label {
        key: LabelKey,
        pill: text::Pill,
    },
    /// The desktop entries changed, so cached icons, names and entries
    /// may be stale.
    IconsReloaded,
    /// An app's desktop entry; None if it has none.
    Entry {
        app_id: String,
        entry: Option<icons::AppEntry>,
    },
    /// Every installed app to list (`Assets::apps`), sorted by name.
    Apps(Arc<Vec<icons::IndexedApp>>),
    /// A pill or a line of text (`Assets::line`).
    Pill {
        tag: u64,
        request: PillRequest,
        pill: text::Pill,
    },
    Bubble {
        tag: u64,
        bubble: Bubble,
        image: Image,
    },
    /// A folder's entries (`Assets::folder`), or why it can't be read.
    Folder {
        tag: u64,
        request: FolderRequest,
        entries: Result<Vec<FileEntry>, String>,
    },
    /// A file's icons (`Assets::file_icons`): its thumbnail's path, else
    /// theme icon names for its type; best first.
    FileIcons {
        path: std::path::PathBuf,
        icons: Vec<String>,
    },
    /// A file's type (`Assets::file_type`): the type and every type it
    /// is a kind of, nearest first (empty if unknown), and the
    /// `mimeapps.list` associations.
    FileType {
        path: std::path::PathBuf,
        types: Vec<String>,
        assoc: Arc<crate::mimeapps::MimeApps>,
    },
    /// A sheet of paper (`Assets::paper`).
    Paper {
        paper: paper::Paper,
        rendered: paper::Rendered,
    },
    /// A document (`Assets::doc`).
    Doc {
        tag: u64,
        doc: Doc,
        rendered: doc::Rendered,
    },
}

/// The worker, and what was requested from it and not received yet.
pub struct Assets {
    jobs: mpsc::Sender<Job>,
    icons: RefCell<Vec<icons::Key>>,
    named: RefCell<Vec<icons::NamedIcon>>,
    labels: RefCell<Vec<LabelKey>>,
    entries: RefCell<Vec<String>>,
}

impl Assets {
    /// Start the worker thread; it loads the fonts when idle. Insert the
    /// returned channel into the event loop and pass its results to
    /// `received`.
    pub fn spawn() -> std::io::Result<(Self, channel::Channel<Done>)> {
        let (jobs, inbox) = mpsc::channel();
        let (done, results) = channel::channel();
        std::thread::Builder::new()
            .name(format!("{}-worker", wl_shell_log::name()))
            .spawn(move || run(inbox, done))?;
        let assets = Self {
            jobs,
            icons: RefCell::new(Vec::new()),
            named: RefCell::new(Vec::new()),
            labels: RefCell::new(Vec::new()),
            entries: RefCell::new(Vec::new()),
        };
        Ok((assets, results))
    }

    /// Fire and forget: if the worker died, there are no icons or labels.
    fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// Request the icon for `key`, unless it is on its way already. It
    /// arrives as `Done::Icon`.
    pub fn icon(&self, key: &icons::Key, font: &str) {
        let mut pending = self.icons.borrow_mut();
        if !pending.contains(key) {
            pending.push(key.clone());
            self.send(Job::Icon {
                key: key.clone(),
                font: font.to_string(),
            });
        }
    }

    /// Request the icon named `key.name`, unless it is on its way already.
    /// It arrives as `Done::Named`.
    pub fn named_icon(&self, key: &icons::NamedIcon) {
        let mut pending = self.named.borrow_mut();
        if !pending.contains(key) {
            pending.push(key.clone());
            self.send(Job::Named(key.clone()));
        }
    }

    /// Request the label for `key`, unless it is on its way already. It
    /// arrives as `Done::Label`.
    pub fn label(&self, key: &LabelKey) {
        let mut pending = self.labels.borrow_mut();
        if !pending.contains(key) {
            pending.push(key.clone());
            self.send(Job::Label(key.clone()));
        }
    }

    /// Request `app_id`'s desktop entry, unless it is on its way already.
    /// It arrives as `Done::Entry`.
    pub fn entry(&self, app_id: &str) {
        let mut pending = self.entries.borrow_mut();
        if !pending.iter().any(|a| a == app_id) {
            pending.push(app_id.to_string());
            self.send(Job::Entry(app_id.to_string()));
        }
    }

    pub fn pill(&self, tag: u64, request: PillRequest) {
        self.send(Job::Pill { tag, request });
    }

    /// `request.text` on one line, without the pill's padding (its
    /// background shows if it isn't transparent). Arrives as `Done::Pill`.
    pub fn line(&self, tag: u64, request: PillRequest) {
        self.send(Job::Line { tag, request });
    }

    /// Render `bubble`; arrives as `Done::Bubble`.
    pub fn bubble(&self, tag: u64, bubble: Bubble) {
        self.send(Job::Bubble { tag, bubble });
    }

    /// List a folder; arrives as `Done::Folder`.
    pub fn folder(&self, tag: u64, request: FolderRequest) {
        self.send(Job::Folder { tag, request });
    }

    /// Look up the icons of `path` (a folder if `dir`); arrive as
    /// `Done::FileIcons`.
    pub fn file_icons(&self, path: &std::path::Path, dir: bool) {
        self.send(Job::FileIcons {
            path: path.to_path_buf(),
            dir,
        });
    }

    /// Look up the type of `path` (a folder if `dir`) and the apps
    /// associated with types; arrives as `Done::FileType`.
    pub fn file_type(&self, path: &std::path::Path, dir: bool) {
        self.send(Job::FileType {
            path: path.to_path_buf(),
            dir,
        });
    }

    /// Render a sheet of paper; arrives as `Done::Paper`.
    pub fn paper(&self, paper: &paper::Paper) {
        self.send(Job::Paper(paper.clone()));
    }

    /// Render a document; arrives as `Done::Doc`.
    pub fn doc(&self, tag: u64, doc: Doc) {
        self.send(Job::Doc { tag, doc });
    }

    /// Check for changed desktop entries (answered with `IconsReloaded` if so).
    pub fn refresh(&self) {
        self.send(Job::Refresh);
    }

    /// List the installed apps; arrives as `Done::Apps`.
    pub fn apps(&self) {
        self.send(Job::Apps);
    }

    /// Forget all decoded icons in the worker.
    pub fn clear_icons(&self) {
        self.send(Job::ClearIcons);
    }

    /// Book-keeping for a result from the worker.
    pub fn received(&self, done: &Done) {
        match done {
            Done::Icon { key, .. } => self.icons.borrow_mut().retain(|k| k != key),
            Done::Named { key, .. } => self.named.borrow_mut().retain(|k| k != key),
            Done::Label { key, .. } => self.labels.borrow_mut().retain(|k| k != key),
            Done::Entry { app_id, .. } => self.entries.borrow_mut().retain(|a| a != app_id),
            Done::IconsReloaded
            | Done::Pill { .. }
            | Done::Bubble { .. }
            | Done::Folder { .. }
            | Done::FileIcons { .. }
            | Done::FileType { .. }
            | Done::Apps(_)
            | Done::Paper { .. }
            | Done::Doc { .. } => {}
        }
    }
}

fn run(inbox: mpsc::Receiver<Job>, done: channel::Sender<Done>) {
    // Loading the system fonts takes a while: do it when first needed, or
    // as soon as there is nothing else to do, so icons don't wait for it.
    let mut text: Option<Text> = None;
    let mut icons = Icons::new();
    // Letter tiles for apps without an icon, by icon key and font.
    let mut letters: HashMap<(icons::Key, String), Arc<Image>> = HashMap::new();
    // File types, loaded with the first folder.
    let mut mime: Option<MimeDb> = None;
    loop {
        let job = match inbox.try_recv() {
            Ok(job) => job,
            Err(mpsc::TryRecvError::Disconnected) => return,
            Err(mpsc::TryRecvError::Empty) => {
                text.get_or_insert_with(Text::new);
                match inbox.recv() {
                    Ok(job) => job,
                    Err(_) => return,
                }
            }
        };
        let result = match job {
            Job::Icon { key, font } => {
                icons.load_entries();
                // Decoded (or lettered) without the shadow, which is drawn
                // on top.
                let plain = key.plain();
                let image = match icons.get(&plain) {
                    Some(image) => image,
                    None => letters
                        .entry((plain, font))
                        .or_insert_with_key(|(key, font)| {
                            let text = text.get_or_insert_with(Text::new);
                            Arc::new(text.letter(&key.app_id, key.size, font))
                        })
                        .clone(),
                };
                let image = match &key.shadow {
                    Some(s) => Arc::new(crate::shadow::icon(&image, s, key.scale)),
                    None => image,
                };
                Done::Icon { key, image }
            }
            Job::Named(key) => {
                // Decoded without the shadow, which is drawn on top.
                let image = icons.named(&key.plain()).map(|image| match &key.shadow {
                    Some(s) => Arc::new(crate::shadow::icon(&image, s, key.scale)),
                    None => image,
                });
                Done::Named { key, image }
            }
            Job::Label(key) => {
                let name = match &key.name {
                    Some(name) => name.clone(),
                    None => icons.name(&key.app_id),
                };
                let pill = text.get_or_insert_with(Text::new).pill(
                    &name,
                    &key.style,
                    key.max_width,
                    key.scale,
                );
                Done::Label { key, pill }
            }
            Job::Pill { tag, request } => {
                let pill = text.get_or_insert_with(Text::new).pill_spans(
                    &request.text,
                    &request.spans,
                    &request.style,
                    request.max_width,
                    request.scale,
                );
                Done::Pill { tag, request, pill }
            }
            Job::Line { tag, request } => {
                let pill = text.get_or_insert_with(Text::new).line_spans(
                    &request.text,
                    &request.spans,
                    &request.style,
                    request.max_width,
                    request.scale,
                );
                Done::Pill { tag, request, pill }
            }
            Job::Bubble { tag, bubble } => {
                let image = bubble::render(&bubble);
                Done::Bubble { tag, bubble, image }
            }
            Job::Entry(app_id) => {
                let entry = icons.entry(&app_id);
                Done::Entry { app_id, entry }
            }
            Job::Apps => Done::Apps(Arc::new(icons.index())),
            Job::Refresh => {
                if !icons.refresh() {
                    continue;
                }
                letters.clear();
                Done::IconsReloaded
            }
            Job::Folder { tag, request } => {
                let mime = mime.get_or_insert_with(MimeDb::load);
                let entries = files::list(&request, mime);
                Done::Folder {
                    tag,
                    request,
                    entries,
                }
            }
            Job::FileIcons { path, dir } => {
                let mime = mime.get_or_insert_with(MimeDb::load);
                let icons = files::icons(&path, dir, mime);
                Done::FileIcons { path, icons }
            }
            Job::FileType { path, dir } => {
                let mime = mime.get_or_insert_with(MimeDb::load);
                let types = (mime.type_of_path(&path, dir))
                    .map(|t| mime.lineage(&t))
                    .unwrap_or_default();
                let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
                    .unwrap_or_default()
                    .split(':')
                    .filter(|d| !d.is_empty())
                    .map(String::from)
                    .collect();
                let assoc = Arc::new(crate::mimeapps::MimeApps::load(&desktops));
                Done::FileType { path, types, assoc }
            }
            Job::Paper(paper) => {
                let rendered = match &paper.content {
                    Some(c) => {
                        let doc = text.get_or_insert_with(Text::new).doc(&c.doc);
                        paper::render_with(&paper, Some(&doc))
                    }
                    None => paper::render(&paper),
                };
                Done::Paper { paper, rendered }
            }
            Job::Doc { tag, doc } => {
                let rendered = text.get_or_insert_with(Text::new).doc(&doc);
                Done::Doc { tag, doc, rendered }
            }
            Job::ClearIcons => {
                icons.clear();
                letters.clear();
                continue;
            }
        };
        if done.send(result).is_err() {
            return;
        }
    }
}
