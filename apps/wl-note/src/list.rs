//! `wl-note list`: the notes, as text or JSON. Pure, unit tested.
//!
//! Text is one note per line: its path (relative to the notes directory),
//! its title, and where it shows (`hidden` too, if it is), tab-separated:
//!
//! ```text
//! work/release.md<TAB>Release checklist<TAB>firefox, foot "*wl-shell*"
//! groceries.md<TAB>Groceries<TAB>everywhere
//! old.md<TAB>Old<TAB>everywhere, hidden
//! ```

use std::path::Path;

use wl_shell_notes::{Color, Index, Matcher, Note, State, Window};

/// Which notes to list, as to hiding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    /// Those not hidden.
    Shown,
    Hidden,
    All,
}

/// A note listed: its path, the note, and whether it is hidden.
pub type Listed<'a> = (&'a Path, &'a Note, bool);

/// The notes to list: all of them, or those shown for `window`, in
/// column order (for a window, the global ones first), hidden or not as
/// `which` says.
pub fn notes<'a>(
    index: &'a Index,
    state: &State,
    window: Option<&Window>,
    which: Which,
) -> Vec<Listed<'a>> {
    let mut out: Vec<Listed> = (state.arrange(index).into_iter())
        .filter_map(|p| {
            let note = &index.get(p)?.note;
            let hidden = state.is_hidden(p);
            let wanted = match which {
                Which::Shown => !hidden,
                Which::Hidden => hidden,
                Which::All => true,
            };
            let matches = window.is_none_or(|w| note.matches(Some(w)));
            (wanted && matches).then_some((p, note, hidden))
        })
        .collect();
    if window.is_some() {
        // Stable: each group keeps its order.
        out.sort_by_key(|(_, note, _)| !note.global());
    }
    out
}

/// The file's name without `.md`, as titles fall back to.
pub fn name(path: &Path) -> String {
    (path.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One line per note.
pub fn text(notes: &[Listed]) -> String {
    let field = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::new();
    for (path, note, hidden) in notes {
        let name = name(path);
        let mut shows = shows(note);
        if *hidden {
            shows.push_str(", hidden");
        }
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            field(&path.to_string_lossy()),
            field(note.title_or(&name)),
            field(&shows),
        ));
    }
    out
}

/// Where a note shows, in words.
fn shows(note: &Note) -> String {
    if note.global() {
        return "everywhere".into();
    }
    let matchers = note.meta.matchers.iter().map(|m| match (&m.app, &m.title) {
        (Some(a), Some(t)) => format!("{} \"{}\"", a.text(), t.text()),
        (Some(a), None) => a.text().to_string(),
        (None, Some(t)) => format!("\"{}\"", t.text()),
        (None, None) => String::new(),
    });
    let tags = note.meta.tags.iter().map(|t| format!("#{t}"));
    matchers.chain(tags).collect::<Vec<_>>().join(", ")
}

/// A JSON array of objects: `path`, `title`, `color` (null: the
/// default), `global`, `hidden`, `match` (`app` and `title`, null if not
/// given), `tags`, `error` and `warnings`.
pub fn json(notes: &[Listed]) -> String {
    let mut out = String::from("[");
    for (i, (path, note, hidden)) in notes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let name = name(path);
        let color = match &note.meta.color {
            Some(Color::Preset(p)) => string(p),
            Some(Color::Rgb(v)) => string(&format!("#{v:06x}")),
            None => "null".into(),
        };
        let matcher = |m: &Matcher| {
            let opt = |p: Option<&str>| p.map_or("null".into(), string);
            format!(
                "{{\"app\":{},\"title\":{}}}",
                opt(m.app.as_ref().map(|p| p.text())),
                opt(m.title.as_ref().map(|p| p.text())),
            )
        };
        let list = |items: Vec<String>| format!("[{}]", items.join(","));
        out.push_str(&format!(
            "\n  {{\"path\":{},\"title\":{},\"color\":{},\"global\":{},\"hidden\":{},\"match\":{},\"tags\":{},\"error\":{},\"warnings\":{}}}",
            string(&path.to_string_lossy()),
            string(note.title_or(&name)),
            color,
            note.global(),
            hidden,
            list(note.meta.matchers.iter().map(matcher).collect()),
            list(note.meta.tags.iter().map(|t| string(t)).collect()),
            note.error.as_deref().map_or("null".into(), string),
            list(note.warnings.iter().map(|w| string(w)).collect()),
        ));
    }
    if !notes.is_empty() {
        out.push('\n');
    }
    out.push_str("]\n");
    out
}

/// A JSON string.
pub(crate) fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str) -> Note {
        Note::parse(text)
    }

    #[test]
    fn as_text() {
        let a = note(
            "+++\napps = [\"firefox\"]\n[[match]]\napp = \"foot\"\ntitle = \"*wl-shell*\"\n[[match]]\ntitle = \"*Jira*\"\n+++\n# Release\n",
        );
        let b = note("groceries\twith a tab");
        let notes = [
            (Path::new("work/release.md"), &a, false),
            (Path::new("groceries.md"), &b, true),
        ];
        assert_eq!(
            text(&notes),
            "work/release.md\tRelease\tfirefox, foot \"*wl-shell*\", \"*Jira*\"\n\
             groceries.md\tgroceries\teverywhere, hidden\n"
        );
    }

    #[test]
    fn as_json() {
        let a = note(
            "+++\ntitle = \"Say \\\"hi\\\"\"\ncolor = \"#0a0b0c\"\napps = [\"ff\"]\ntags = [\"t\"]\n+++\n",
        );
        let b = note("+++\nbroken =\n+++\n");
        let notes = [
            (Path::new("a.md"), &a, false),
            (Path::new("b.md"), &b, true),
        ];
        let out = json(&notes);
        assert!(out.starts_with("[\n  {\"path\":\"a.md\",\"title\":\"Say \\\"hi\\\"\",\"color\":\"#0a0b0c\",\"global\":false,\"hidden\":false,\"match\":[{\"app\":\"ff\",\"title\":null}],\"tags\":[\"t\"],\"error\":null,\"warnings\":[]},\n"), "{out}");
        assert!(out.contains("{\"path\":\"b.md\",\"title\":\"b\",\"color\":null,\"global\":true,\"hidden\":true,\"match\":[],\"tags\":[],\"error\":\"frontmatter, line 2: "), "{out}");
        assert!(out.ends_with("}\n]\n"));
        assert_eq!(json(&[]), "[]\n");
        assert_eq!(string("a\u{1}\n"), "\"a\\u0001\\n\"");
    }

    #[test]
    fn choosing() {
        let dir = std::env::temp_dir().join(format!("wl-note-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in [
            ("a.md", "a"),
            ("b.md", "+++\napps = [\"firefox\"]\n+++\nb"),
            ("c.md", "c"),
        ] {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let (index, _) = Index::load(&dir);
        let mut state = State::default();
        state.order.set(["c.md", "b.md", "a.md"].map(Into::into));
        state.hidden.push(Path::new("a.md"));
        let paths = |which, window: Option<&Window>| -> Vec<String> {
            (notes(&index, &state, window, which).iter())
                .map(|(p, _, _)| p.display().to_string())
                .collect()
        };
        assert_eq!(paths(Which::Shown, None), ["c.md", "b.md"]);
        assert_eq!(paths(Which::Hidden, None), ["a.md"]);
        assert_eq!(paths(Which::All, None), ["c.md", "b.md", "a.md"]);
        let foot = Window::new("foot", "");
        assert_eq!(paths(Which::All, Some(&foot)), ["c.md", "a.md"]);
        // For a window, as the column shows them: the global ones first.
        let firefox = Window::new("firefox", "");
        assert_eq!(paths(Which::All, Some(&firefox)), ["c.md", "a.md", "b.md"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
