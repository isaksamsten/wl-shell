//! `wl-note list`: the notes, as text or JSON. Pure, unit tested.
//!
//! Text is one note per line: its path (relative to the notes directory),
//! its title, and where it shows, tab-separated:
//!
//! ```text
//! work/release.md<TAB>Release checklist<TAB>firefox, foot "*wl-shell*"
//! groceries.md<TAB>Groceries<TAB>everywhere
//! ```

use std::path::Path;

use wl_shell_notes::{Color, Index, Matcher, Note, Window};

/// The notes to list: all of them, or those shown for `window`, in
/// column order.
pub fn notes<'a>(index: &'a Index, window: Option<&Window>) -> Vec<(&'a Path, &'a Note)> {
    let paths = match window {
        Some(w) => index.shown(Some(w)),
        None => index.ordered(),
    };
    (paths.into_iter())
        .filter_map(|p| Some((p, &index.get(p)?.note)))
        .collect()
}

/// The file's name without `.md`, as titles fall back to.
pub fn name(path: &Path) -> String {
    (path.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// One line per note.
pub fn text(notes: &[(&Path, &Note)]) -> String {
    let field = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::new();
    for (path, note) in notes {
        let name = name(path);
        out.push_str(&format!(
            "{}\t{}\t{}\n",
            field(&path.to_string_lossy()),
            field(note.title_or(&name)),
            field(&shows(note)),
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
/// default), `global`, `match` (`app` and `title`, null if not given),
/// `tags`, `error` and `warnings`.
pub fn json(notes: &[(&Path, &Note)]) -> String {
    let mut out = String::from("[");
    for (i, (path, note)) in notes.iter().enumerate() {
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
            "\n  {{\"path\":{},\"title\":{},\"color\":{},\"global\":{},\"match\":{},\"tags\":{},\"error\":{},\"warnings\":{}}}",
            string(&path.to_string_lossy()),
            string(note.title_or(&name)),
            color,
            note.global(),
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
fn string(s: &str) -> String {
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
            (Path::new("work/release.md"), &a),
            (Path::new("groceries.md"), &b),
        ];
        assert_eq!(
            text(&notes),
            "work/release.md\tRelease\tfirefox, foot \"*wl-shell*\", \"*Jira*\"\n\
             groceries.md\tgroceries\teverywhere\n"
        );
    }

    #[test]
    fn as_json() {
        let a = note(
            "+++\ntitle = \"Say \\\"hi\\\"\"\ncolor = \"#0a0b0c\"\napps = [\"ff\"]\ntags = [\"t\"]\n+++\n",
        );
        let b = note("+++\nbroken =\n+++\n");
        let notes = [(Path::new("a.md"), &a), (Path::new("b.md"), &b)];
        let out = json(&notes);
        assert!(out.starts_with("[\n  {\"path\":\"a.md\",\"title\":\"Say \\\"hi\\\"\",\"color\":\"#0a0b0c\",\"global\":false,\"match\":[{\"app\":\"ff\",\"title\":null}],\"tags\":[\"t\"],\"error\":null,\"warnings\":[]},\n"), "{out}");
        assert!(out.contains("{\"path\":\"b.md\",\"title\":\"b\",\"color\":null,\"global\":true,\"match\":[],\"tags\":[],\"error\":\"frontmatter, line 2: "), "{out}");
        assert!(out.ends_with("}\n]\n"));
        assert_eq!(json(&[]), "[]\n");
        assert_eq!(string("a\u{1}\n"), "\"a\\u0001\\n\"");
    }
}
