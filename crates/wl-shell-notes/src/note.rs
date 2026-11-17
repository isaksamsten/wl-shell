//! One note: a markdown file with an optional TOML frontmatter between
//! `+++` lines at its very start. Pure, unit tested.
//!
//! ```markdown
//! +++
//! title = "Release checklist"
//! apps  = ["firefox"]
//! color = "yellow"
//! +++
//! - [ ] bump the version
//! ```
//!
//! Every key is optional, and a file without a frontmatter is a note too:
//! a global one, in the default color. Unknown keys are kept. A
//! frontmatter that doesn't parse leaves the note global (with the error
//! in `Note::error`), and `set` refuses to touch it. Smaller problems (a
//! glob or a color that doesn't parse) are `Note::warnings`.
//!
//! `set` edits the frontmatter with `toml_edit`, so comments, key order and
//! formatting survive, and never touches the body. The line endings of the
//! file (LF or CRLF) are kept.

use std::ops::Range;

use toml_edit::{DocumentMut, Item, TableLike};

use crate::matcher::{Matcher, Window};

/// The paper colors every program knows by name.
pub const PRESETS: [&str; 8] = [
    "yellow", "orange", "pink", "purple", "blue", "green", "gray", "white",
];

/// A note's `color`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Color {
    /// One of `PRESETS`.
    Preset(&'static str),
    /// `#rrggbb`, as 0xRRGGBB.
    Rgb(u32),
}

impl Color {
    pub fn parse(s: &str) -> Result<Self, String> {
        if let Some(name) = PRESETS.iter().find(|p| **p == s) {
            return Ok(Color::Preset(name));
        }
        if let Some(hex) = s.strip_prefix('#')
            && hex.len() == 6
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
            && let Ok(v) = u32::from_str_radix(hex, 16)
        {
            return Ok(Color::Rgb(v));
        }
        Err(format!(
            "color '{s}': expected #rrggbb or one of {}",
            PRESETS.join(", ")
        ))
    }
}

/// What the frontmatter says.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub title: Option<String>,
    /// `apps` and `match`, in that order.
    pub matchers: Vec<Matcher>,
    pub tags: Vec<String>,
    pub color: Option<Color>,
}

impl Meta {
    /// Shown everywhere: it names no app, window or tag.
    pub fn global(&self) -> bool {
        self.matchers.is_empty() && self.tags.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    /// The whole file.
    source: String,
    /// The frontmatter's TOML (between the fences), if it has one.
    front: Option<Range<usize>>,
    /// Where the body starts.
    body: usize,
    pub meta: Meta,
    /// Why the frontmatter doesn't parse; the note is global then.
    pub error: Option<String>,
    /// Parts of the frontmatter that were left out: globs and colors that
    /// don't parse.
    pub warnings: Vec<String>,
}

/// Where the parts of a file are.
enum Split {
    /// No frontmatter: all body.
    None,
    /// An opening fence without a closing one.
    Unterminated,
    Front {
        toml: Range<usize>,
        body: usize,
    },
}

/// The byte ranges of `s`'s lines from `start`: (line, without its line
/// break).
fn lines(s: &str, start: usize) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut at = start;
    std::iter::from_fn(move || {
        if at >= s.len() {
            return None;
        }
        let end = s[at..].find('\n').map_or(s.len(), |i| at + i + 1);
        let line = at..end;
        at = end;
        Some(line)
    })
}

/// Whether the line is a `+++` fence (trailing blanks and CR allowed).
fn fence(line: &str) -> bool {
    line.trim_end_matches(['\n', '\r', ' ', '\t']) == "+++"
}

fn split(s: &str) -> Split {
    let start = if s.starts_with('\u{feff}') { 3 } else { 0 };
    let mut lines = lines(s, start);
    match lines.next() {
        Some(first) if fence(&s[first.clone()]) => {
            for line in lines {
                if fence(&s[line.clone()]) {
                    return Split::Front {
                        toml: first.end..line.start,
                        body: line.end,
                    };
                }
            }
            Split::Unterminated
        }
        _ => Split::None,
    }
}

impl Note {
    /// Parse a file's text. Never fails: what doesn't parse is reported in
    /// `error` and `warnings`.
    pub fn parse(source: impl Into<String>) -> Self {
        let source = source.into();
        let mut note = Note {
            source,
            ..Default::default()
        };
        match split(&note.source) {
            Split::None => {
                note.body = if note.source.starts_with('\u{feff}') {
                    3
                } else {
                    0
                };
            }
            Split::Unterminated => {
                note.error = Some("frontmatter: no closing +++ line".into());
            }
            Split::Front { toml, body } => {
                note.body = body;
                match parse_meta(&note.source[toml.clone()]) {
                    Ok((meta, warnings)) => {
                        note.meta = meta;
                        note.warnings = warnings;
                    }
                    Err(e) => note.error = Some(e),
                }
                note.front = Some(toml);
            }
        }
        note
    }

    /// The whole file.
    pub fn text(&self) -> &str {
        &self.source
    }

    /// The markdown after the frontmatter.
    pub fn body(&self) -> &str {
        &self.source[self.body..]
    }

    /// The frontmatter's TOML, if it has one.
    pub fn frontmatter(&self) -> Option<&str> {
        self.front.clone().map(|r| &self.source[r])
    }

    /// Whether to warn about it: its frontmatter doesn't parse, or parts
    /// of it.
    pub fn troubled(&self) -> bool {
        self.error.is_some() || !self.warnings.is_empty()
    }

    /// Shown for every window, and without one.
    pub fn global(&self) -> bool {
        self.meta.global()
    }

    /// Whether the note belongs to `window`: it is global, or one of its
    /// matchers matches. With no window, only global notes do.
    pub fn matches(&self, window: Option<&Window>) -> bool {
        self.global() || window.is_some_and(|w| self.meta.matchers.iter().any(|m| m.matches(w)))
    }

    /// The title: the frontmatter's, else the first heading's.
    pub fn title(&self) -> Option<&str> {
        (self.meta.title.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .or_else(|| heading(self.body()))
    }

    /// The title, or `name` (the file's) without one.
    pub fn title_or<'a>(&'a self, name: &'a str) -> &'a str {
        self.title().unwrap_or(name)
    }

    /// Whether the file breaks its lines with CRLF.
    fn crlf(&self) -> bool {
        self.source
            .find('\n')
            .is_some_and(|i| self.source[..i].ends_with('\r'))
    }

    /// Set the frontmatter's `key` to `value`, or remove it (None), in
    /// place: everything else in the file stays as it is. A file without
    /// a frontmatter gets one; one left empty goes. Fails if the
    /// frontmatter doesn't parse.
    pub fn set(&mut self, key: &str, value: Option<Item>) -> Result<(), String> {
        if let Some(e) = &self.error {
            return Err(format!("not editing a broken frontmatter: {e}"));
        }
        let nl = if self.crlf() { "\r\n" } else { "\n" };
        let bom = if self.source.starts_with('\u{feff}') {
            "\u{feff}"
        } else {
            ""
        };
        let mut doc = match self.frontmatter() {
            Some(toml) => toml.parse::<DocumentMut>().map_err(|e| e.to_string())?,
            None => DocumentMut::new(),
        };
        match value {
            // A new value keeps the old one's comment and spacing.
            Some(Item::Value(mut v)) => {
                if let Some(old) = doc.get(key).and_then(Item::as_value) {
                    *v.decor_mut() = old.decor().clone();
                }
                doc[key] = Item::Value(v);
            }
            Some(v) => doc[key] = v,
            None => {
                doc.remove(key);
            }
        }
        let mut front = doc.to_string().replace("\r\n", "\n");
        if nl != "\n" {
            front = front.replace('\n', nl);
        }
        let body = self.body();
        let text = if front.trim().is_empty() {
            format!("{bom}{body}")
        } else {
            if !front.ends_with('\n') {
                front.push_str(nl);
            }
            format!("{bom}+++{nl}{front}+++{nl}{body}")
        };
        *self = Note::parse(text);
        Ok(())
    }

    /// `set` with a plain value: a string, a number, an array, ….
    pub fn set_value(
        &mut self,
        key: &str,
        value: impl Into<toml_edit::Value>,
    ) -> Result<(), String> {
        self.set(key, Some(toml_edit::value(value)))
    }
}

/// The text of the first ATX heading (`# Title`) outside code blocks.
fn heading(body: &str) -> Option<&str> {
    let mut fenced: Option<&str> = None;
    for line in body.lines() {
        let t = line.trim_start();
        if line.len() - t.len() > 3 {
            continue;
        }
        let marker = ["```", "~~~"].into_iter().find(|m| t.starts_with(m));
        match (fenced, marker) {
            (None, Some(m)) => {
                fenced = Some(m);
                continue;
            }
            (Some(open), Some(m)) if open == m => {
                fenced = None;
                continue;
            }
            (Some(_), _) => continue,
            (None, None) => {}
        }
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        let rest = &t[hashes..];
        if (1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t'])) {
            let text = rest.trim().trim_end_matches('#').trim_end();
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

/// The frontmatter's keys, and the warnings about parts left out.
fn parse_meta(toml: &str) -> Result<(Meta, Vec<String>), String> {
    let doc: DocumentMut = toml.parse().map_err(|e: toml_edit::TomlError| {
        // The frontmatter starts on the file's second line.
        let line = e
            .span()
            .map(|s| toml[..s.start.min(toml.len())].matches('\n').count() + 2);
        let message = e.message().trim_end();
        match line {
            Some(line) => format!("frontmatter, line {line}: {message}"),
            None => format!("frontmatter: {message}"),
        }
    })?;
    let mut meta = Meta::default();
    let mut warnings = Vec::new();
    if let Some(item) = doc.get("title") {
        meta.title = Some(string(item, "title")?);
    }
    if let Some(item) = doc.get("apps") {
        for app in strings(item, "apps")? {
            let (m, errors) = Matcher::new(Some(&app), None);
            warnings.extend(errors.into_iter().map(|e| format!("apps: {e}")));
            meta.matchers.push(m);
        }
    }
    if let Some(item) = doc.get("match") {
        for table in tables(item)? {
            let get = |key: &str| -> Result<Option<String>, String> {
                (table.get(key))
                    .map(|i| string(i, &format!("match.{key}")))
                    .transpose()
            };
            let (app, title) = (get("app")?, get("title")?);
            if let Some((key, _)) = table.iter().find(|(k, _)| !["app", "title"].contains(k)) {
                return Err(format!(
                    "match: unknown key '{key}' (expected app or title)"
                ));
            }
            if app.is_none() && title.is_none() {
                return Err("match: a matcher needs app or title".into());
            }
            let (m, errors) = Matcher::new(app.as_deref(), title.as_deref());
            warnings.extend(errors.into_iter().map(|e| format!("match: {e}")));
            meta.matchers.push(m);
        }
    }
    if let Some(item) = doc.get("tags") {
        meta.tags = strings(item, "tags")?;
    }
    if let Some(item) = doc.get("color") {
        match Color::parse(string(item, "color")?.trim()) {
            Ok(c) => meta.color = Some(c),
            Err(e) => warnings.push(e),
        }
    }
    Ok((meta, warnings))
}

fn string(item: &Item, key: &str) -> Result<String, String> {
    (item.as_str())
        .map(String::from)
        .ok_or_else(|| format!("{key}: expected a string"))
}

fn strings(item: &Item, key: &str) -> Result<Vec<String>, String> {
    let wrong = || format!("{key}: expected a list of strings");
    let array = item.as_array().ok_or_else(wrong)?;
    (array.iter())
        .map(|v| v.as_str().map(String::from).ok_or_else(wrong))
        .collect()
}

/// `[[match]]` tables, or `match = [{ … }, …]`.
fn tables(item: &Item) -> Result<Vec<&dyn TableLike>, String> {
    let wrong = || "match: expected a list of tables".to_string();
    if let Some(aot) = item.as_array_of_tables() {
        return Ok(aot.iter().map(|t| t as &dyn TableLike).collect());
    }
    let array = item.as_array().ok_or_else(wrong)?;
    (array.iter())
        .map(|v| {
            v.as_inline_table()
                .map(|t| t as &dyn TableLike)
                .ok_or_else(wrong)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(app: &str, title: &str) -> Window {
        Window::new(app, title)
    }

    #[test]
    fn without_frontmatter() {
        let n = Note::parse("# Groceries\n- milk\n");
        assert!(n.error.is_none() && n.frontmatter().is_none());
        assert!(n.global());
        assert_eq!(n.body(), "# Groceries\n- milk\n");
        assert_eq!(n.title(), Some("Groceries"));
        assert!(n.matches(None));
        assert!(n.matches(Some(&w("firefox", ""))));
        assert_eq!(Note::parse("").body(), "");
        assert_eq!(Note::parse("text").title_or("name"), "name");
    }

    #[test]
    fn with_frontmatter() {
        let n = Note::parse(
            "+++\ntitle = \"Release\"\napps = [\"firefox\"]\ncolor = \"pink\"\n+++\nbody\n",
        );
        assert!(n.error.is_none(), "{:?}", n.error);
        assert_eq!(n.title(), Some("Release"));
        assert_eq!(n.meta.color, Some(Color::Preset("pink")));
        assert_eq!(n.body(), "body\n");
        assert!(!n.global());
        assert!(n.matches(Some(&w("firefox", "x"))));
        assert!(!n.matches(Some(&w("foot", "x"))));
        assert!(!n.matches(None));
    }

    #[test]
    fn match_tables() {
        let n = Note::parse(
            "+++\n[[match]]\napp = \"foot\"\ntitle = \"*wl-shell*\"\n\n[[match]]\ntitle = \"*Jira*\"\n+++\n",
        );
        assert!(n.error.is_none(), "{:?}", n.error);
        assert_eq!(n.meta.matchers.len(), 2);
        assert!(n.matches(Some(&w("foot", "~/projects/wl-shell"))));
        assert!(!n.matches(Some(&w("foot", "~/projects/other"))));
        assert!(n.matches(Some(&w("firefox", "JIRA-12 - jira"))));
        let inline = Note::parse("+++\nmatch = [{ app = \"foot\" }]\n+++\n");
        assert!(inline.matches(Some(&w("foot", ""))));
    }

    #[test]
    fn broken_frontmatter_is_a_global_note() {
        let n = Note::parse("+++\napps = [\"firefox\"\ncolor = \"pink\"\n+++\nbody\n");
        let e = n.error.as_deref().unwrap();
        assert!(e.starts_with("frontmatter, line "), "{e}");
        assert!(n.global() && n.troubled());
        assert_eq!(n.meta, Meta::default());
        assert_eq!(n.body(), "body\n");
        // Wrong types and unknown matcher keys break it too.
        for bad in [
            "apps = \"firefox\"",
            "title = 3",
            "[[match]]\napps = \"x\"",
            "[[match]]",
            "match = 1",
        ] {
            let n = Note::parse(format!("+++\n{bad}\n+++\n"));
            assert!(n.error.is_some(), "{bad}");
            assert!(n.global(), "{bad}");
        }
        // No closing fence: all body.
        let n = Note::parse("+++\ntitle = \"x\"\nbody\n");
        assert!(n.error.is_some());
        assert_eq!(n.body(), "+++\ntitle = \"x\"\nbody\n");
    }

    #[test]
    fn small_problems_are_warnings() {
        let n = Note::parse("+++\napps = [\"fire[fox\", \"foot\"]\ncolor = \"teal\"\n+++\n");
        assert!(n.error.is_none());
        assert_eq!(n.warnings.len(), 2, "{:?}", n.warnings);
        assert_eq!(n.meta.color, None);
        // The broken glob never matches, the other still does.
        assert!(n.matches(Some(&w("foot", ""))));
        assert!(!n.matches(Some(&w("fire[fox", ""))));
        assert!(!n.global());
        assert_eq!(
            Note::parse("+++\ncolor = \"#A0b0C0\"\n+++\n").meta.color,
            Some(Color::Rgb(0xa0b0c0))
        );
    }

    #[test]
    fn tags_alone_are_not_global() {
        let n = Note::parse("+++\ntags = [\"browser\"]\n+++\n");
        assert!(!n.global());
        assert!(!n.matches(Some(&w("firefox", ""))));
    }

    #[test]
    fn crlf() {
        let n = Note::parse("+++\r\ntitle = \"Win\"\r\napps = [\"a\"]\r\n+++\r\nbody\r\n");
        assert!(n.error.is_none(), "{:?}", n.error);
        assert_eq!(n.title(), Some("Win"));
        assert_eq!(n.body(), "body\r\n");
        let mut n = n;
        n.set_value("color", "blue").unwrap();
        assert_eq!(
            n.text(),
            "+++\r\ntitle = \"Win\"\r\napps = [\"a\"]\r\ncolor = \"blue\"\r\n+++\r\nbody\r\n"
        );
    }

    #[test]
    fn fences_in_the_body_are_body() {
        let n = Note::parse("+++\ntitle = \"a\"\n+++\nfirst\n+++\nsecond\n");
        assert!(n.error.is_none());
        assert_eq!(n.body(), "first\n+++\nsecond\n");
        // Only a fence on the first line opens a frontmatter.
        let n = Note::parse("text\n+++\ntitle = \"a\"\n+++\n");
        assert!(n.frontmatter().is_none() && n.error.is_none());
        assert_eq!(n.title(), None);
        // A fence with trailing blanks still is one.
        let n = Note::parse("+++  \ntitle = \"a\"\n+++\t\nb");
        assert_eq!((n.title(), n.body()), (Some("a"), "b"));
    }

    #[test]
    fn set_keeps_comments_order_and_unknown_keys() {
        let text = "+++\n# who reads this\ntitle = \"T\"   # the title\nmine = { a = 1 }\napps = [\"firefox\"]\n+++\n# Body\n\n- [ ] x\n";
        let mut n = Note::parse(text);
        n.set_value("color", "green").unwrap();
        assert_eq!(
            n.text(),
            "+++\n# who reads this\ntitle = \"T\"   # the title\nmine = { a = 1 }\napps = [\"firefox\"]\ncolor = \"green\"\n+++\n# Body\n\n- [ ] x\n"
        );
        assert_eq!(n.meta.color, Some(Color::Preset("green")));
        n.set_value("color", "#112233").unwrap();
        assert!(n.text().contains("color = \"#112233\"\n"));
        n.set("color", None).unwrap();
        assert_eq!(n.text(), text);
        let mut apps = toml_edit::Array::new();
        apps.push("firefox");
        apps.push("foot");
        n.set_value("apps", apps).unwrap();
        assert!(n.matches(Some(&w("foot", ""))));
        n.set_value("title", "New").unwrap();
        assert!(
            n.text().contains("title = \"New\"   # the title\n"),
            "{}",
            n.text()
        );
        assert!(n.text().ends_with("+++\n# Body\n\n- [ ] x\n"));
    }

    #[test]
    fn set_adds_and_drops_the_frontmatter() {
        let mut n = Note::parse("just text\n");
        n.set_value("color", "blue").unwrap();
        assert_eq!(n.text(), "+++\ncolor = \"blue\"\n+++\njust text\n");
        n.set("color", None).unwrap();
        assert_eq!(n.text(), "just text\n");
        let mut n = Note::parse("\u{feff}text");
        n.set_value("title", "x").unwrap();
        assert_eq!(n.text(), "\u{feff}+++\ntitle = \"x\"\n+++\ntext");
        assert_eq!(n.title(), Some("x"));
    }

    #[test]
    fn set_refuses_broken_frontmatter() {
        let text = "+++\ntitle = \n+++\nbody";
        let mut n = Note::parse(text);
        assert!(n.set_value("color", "blue").is_err());
        assert_eq!(n.text(), text);
    }

    #[test]
    fn headings() {
        assert_eq!(heading("intro\n## Second #\n# First"), Some("Second"));
        assert_eq!(heading("```\n# not\n```\n# yes"), Some("yes"));
        assert_eq!(heading("#hashtag\n#\n    # code"), None);
        assert_eq!(heading("~~~\n# a\n```\n# b\n~~~\n# c"), Some("c"));
        let n = Note::parse("+++\ntitle = \"  \"\n+++\n# Heading");
        assert_eq!(n.title(), Some("Heading"));
    }
}
