//! Which apps open a file type: the `mimeapps.list` files (defaults,
//! added and removed associations) over the apps' own `MimeType=`, as in
//! the freedesktop MIME applications spec. Parsing and choosing are pure,
//! unit tested.
//!
//! The lists are read most important first: `$XDG_CONFIG_HOME`, then
//! `$XDG_CONFIG_DIRS`, then `$XDG_DATA_HOME/applications` and
//! `$XDG_DATA_DIRS/applications`, each with the desktop-specific
//! `<desktop>-mimeapps.list` before `mimeapps.list` (and the data
//! directories' legacy `defaults.list` last).
//!
//! For a file whose type and its parents are `types` (nearest first):
//!
//! - The **candidates**, for each type in turn: the added associations of
//!   each list, then the apps that list the type in `MimeType=`. An app
//!   removed in a list is left out of that list's and every less
//!   important list's associations, and of the `MimeType=` apps.
//! - The **default** is the first installed app among a type's defaults,
//!   in any list, else its first candidate; the nearest type that has
//!   one decides. It comes first among the candidates.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::icons::IndexedApp;

/// One `mimeapps.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct List {
    /// Type to desktop IDs (without `.desktop`), in order.
    defaults: HashMap<String, Vec<String>>,
    added: HashMap<String, Vec<String>>,
    removed: HashMap<String, Vec<String>>,
}

impl List {
    /// The list in a file's text.
    pub fn parse(text: &str) -> Self {
        let mut list = Self::default();
        let mut section: Option<&mut HashMap<String, Vec<String>>> = None;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                section = match line {
                    "[Default Applications]" => Some(&mut list.defaults),
                    "[Added Associations]" => Some(&mut list.added),
                    "[Removed Associations]" => Some(&mut list.removed),
                    _ => None,
                };
                continue;
            }
            let (Some(map), Some((mime, value))) = (section.as_deref_mut(), line.split_once('='))
            else {
                continue;
            };
            let ids = map.entry(mime.trim().to_string()).or_default();
            for id in ids_in(value) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        list
    }
}

/// The desktop IDs in `a.desktop;b.desktop;`, without `.desktop`.
fn ids_in(value: &str) -> Vec<String> {
    (value.split(';').map(str::trim))
        .filter(|id| !id.is_empty())
        .map(|id| id.trim_end_matches(".desktop").to_string())
        .collect()
}

/// Every list, most important first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeApps {
    lists: Vec<List>,
}

impl MimeApps {
    /// The lists of the user and the system, for the desktops named
    /// (`$XDG_CURRENT_DESKTOP`).
    pub fn load(desktops: &[String]) -> Self {
        let mut apps = Self::default();
        for path in paths(desktops) {
            if let Ok(text) = std::fs::read_to_string(&path) {
                apps.push(List::parse(&text));
            }
        }
        apps
    }

    /// Add `list`, less important than those before.
    pub fn push(&mut self, list: List) {
        self.lists.push(list);
    }

    /// The `apps` that open a file whose type and its parents are `types`
    /// (nearest first), as indices: the default first, then the others
    /// in order of preference.
    pub fn candidates(&self, types: &[String], apps: &[IndexedApp]) -> Vec<usize> {
        let index: HashMap<&str, usize> = (apps.iter().enumerate())
            .map(|(i, a)| (a.id.as_str(), i))
            .collect();
        let mut out: Vec<usize> = Vec::new();
        let mut default = None;
        for mime in types {
            let found = self.for_type(mime, apps, &index);
            if default.is_none() {
                default = self.default_for(mime, &index).or(found.first().copied());
            }
            for i in found {
                if !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        if let Some(d) = default {
            out.retain(|&i| i != d);
            out.insert(0, d);
        }
        out
    }

    /// The default app for `types` (see `candidates`).
    pub fn default_app(&self, types: &[String], apps: &[IndexedApp]) -> Option<usize> {
        self.candidates(types, apps).first().copied()
    }

    /// The first installed default for `mime`, in any list.
    fn default_for(&self, mime: &str, index: &HashMap<&str, usize>) -> Option<usize> {
        (self.lists.iter())
            .filter_map(|l| l.defaults.get(mime))
            .flatten()
            .find_map(|id| index.get(id.as_str()).copied())
    }

    /// The candidates for `mime` alone, in order.
    fn for_type(
        &self,
        mime: &str,
        apps: &[IndexedApp],
        index: &HashMap<&str, usize>,
    ) -> Vec<usize> {
        let mut out = Vec::new();
        let mut removed: Vec<&str> = Vec::new();
        for list in &self.lists {
            for id in list.added.get(mime).into_iter().flatten() {
                if let Some(&i) = index.get(id.as_str())
                    && !removed.contains(&id.as_str())
                    && !out.contains(&i)
                {
                    out.push(i);
                }
            }
            removed.extend(
                list.removed
                    .get(mime)
                    .into_iter()
                    .flatten()
                    .map(String::as_str),
            );
        }
        for (i, app) in apps.iter().enumerate() {
            if app.mime_types.iter().any(|m| m == mime)
                && !removed.contains(&app.id.as_str())
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        out
    }
}

/// The `mimeapps.list` files, most important first.
fn paths(desktops: &[String]) -> Vec<PathBuf> {
    let env_dir = |var: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let env_dirs = |var: &str, default: &str| -> Vec<PathBuf> {
        let value = std::env::var(var).ok().filter(|v| !v.is_empty());
        (value.as_deref().unwrap_or(default).split(':'))
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
            .collect()
    };
    let config_home =
        env_dir("XDG_CONFIG_HOME").or_else(|| home.as_ref().map(|h| h.join(".config")));
    let data_home =
        env_dir("XDG_DATA_HOME").or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let config: Vec<PathBuf> = (config_home.into_iter())
        .chain(env_dirs("XDG_CONFIG_DIRS", "/etc/xdg"))
        .collect();
    let data: Vec<PathBuf> = (data_home.into_iter())
        .chain(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share"))
        .map(|d| d.join("applications"))
        .collect();
    let names: Vec<String> = (desktops.iter())
        .map(|d| format!("{}-mimeapps.list", d.to_lowercase()))
        .chain(std::iter::once("mimeapps.list".to_string()))
        .collect();
    let mut out = Vec::new();
    for dir in &config {
        out.extend(names.iter().map(|n| dir.join(n)));
    }
    for dir in &data {
        out.extend(names.iter().map(|n| dir.join(n)));
        out.push(dir.join("defaults.list"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::AppEntry;

    fn app(id: &str, types: &[&str]) -> IndexedApp {
        IndexedApp {
            id: id.into(),
            name: id.into(),
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
            app("firefox", &["text/html", "application/pdf"]),
            app("gedit", &["text/plain"]),
            app("nvim", &["text/plain", "text/x-csrc"]),
            app("nautilus", &["inode/directory"]),
            app("okular", &[]),
        ]
    }

    fn types(t: &[&str]) -> Vec<String> {
        t.iter().map(|t| t.to_string()).collect()
    }

    fn ids(m: &MimeApps, t: &[&str]) -> Vec<String> {
        let apps = apps();
        let found = m.candidates(&types(t), &apps);
        found.into_iter().map(|i| apps[i].id.clone()).collect()
    }

    #[test]
    fn parsing() {
        let list = List::parse(
            "# comment\n[Default Applications]\napplication/pdf=okular.desktop;evince.desktop\n\
             [Added Associations]\ntext/plain=nvim.desktop;;gedit.desktop;nvim.desktop;\n\
             [Other]\ntext/html=x.desktop\n[Removed Associations]\n text/html = firefox.desktop;\n",
        );
        assert_eq!(list.defaults["application/pdf"], ["okular", "evince"]);
        assert_eq!(list.added["text/plain"], ["nvim", "gedit"]);
        assert_eq!(list.removed["text/html"], ["firefox"]);
        assert_eq!(
            list.defaults.len() + list.added.len() + list.removed.len(),
            3
        );
    }

    #[test]
    fn from_mime_types_alone() {
        let m = MimeApps::default();
        assert_eq!(ids(&m, &["application/pdf"]), ["evince", "firefox"]);
        // Parents count, after the type itself.
        assert_eq!(ids(&m, &["text/x-csrc", "text/plain"]), ["nvim", "gedit"]);
        assert!(ids(&m, &["image/png"]).is_empty());
    }

    #[test]
    fn the_default_comes_first() {
        let mut m = MimeApps::default();
        m.push(List::parse(
            "[Default Applications]\napplication/pdf=missing.desktop;firefox.desktop\n",
        ));
        assert_eq!(ids(&m, &["application/pdf"]), ["firefox", "evince"]);
        // A default for a parent counts after the nearer type's candidates.
        let mut m = MimeApps::default();
        m.push(List::parse(
            "[Default Applications]\ntext/plain=gedit.desktop\n",
        ));
        assert_eq!(ids(&m, &["text/x-csrc", "text/plain"]), ["nvim", "gedit"]);
        assert_eq!(ids(&m, &["text/markdown", "text/plain"]), ["gedit", "nvim"]);
        // Even an app that doesn't list the type can be the default.
        let mut m = MimeApps::default();
        m.push(List::parse(
            "[Default Applications]\napplication/pdf=okular.desktop\n",
        ));
        assert_eq!(
            ids(&m, &["application/pdf"]),
            ["okular", "evince", "firefox"]
        );
        assert_eq!(
            m.default_app(&types(&["application/pdf"]), &apps()),
            Some(5)
        );
    }

    #[test]
    fn added_and_removed() {
        let mut m = MimeApps::default();
        m.push(List::parse(
            "[Added Associations]\napplication/pdf=okular.desktop\n\
             [Removed Associations]\napplication/pdf=evince.desktop\n",
        ));
        // Removed in a more important list: gone from a less important one.
        m.push(List::parse(
            "[Added Associations]\napplication/pdf=evince.desktop;gedit.desktop\n",
        ));
        assert_eq!(
            ids(&m, &["application/pdf"]),
            ["okular", "gedit", "firefox"]
        );
        // Removed only below: a more important list's addition stays.
        let mut m = MimeApps::default();
        m.push(List::parse(
            "[Added Associations]\ntext/html=nvim.desktop\n",
        ));
        m.push(List::parse(
            "[Removed Associations]\ntext/html=nvim.desktop;firefox.desktop\n",
        ));
        assert_eq!(ids(&m, &["text/html"]), ["nvim"]);
    }
}
