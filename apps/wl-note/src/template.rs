//! Templates: notes made from the files of `notes-dir/.templates/`. Pure,
//! unit tested.
//!
//! A template is a note itself (frontmatter and markdown), kept in a dot
//! directory so it is never shown as one. A note made from it is a copy
//! with `{date}`, `{time}`, `{app}` and `{window}` filled in, in its title
//! and its text. It is attached the way `+` attaches a note (to the
//! context's app, or global without one) unless the template names an
//! `apps`, `match` or `tags` of its own: those are what it is for, so
//! they win over the context.

use std::path::{Path, PathBuf};

use wl_shell_notes::due::{Local, Zone};
use wl_shell_notes::{Note, matcher, toml_edit};

use crate::files;

/// The templates' directory, in the notes directory.
pub const DIR: &str = ".templates";

/// Where the templates are, for the notes directory `notes`.
pub fn dir(notes: &Path) -> PathBuf {
    notes.join(DIR)
}

/// What the placeholders stand for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Vars {
    /// `{date}`: 2026-11-19.
    pub date: String,
    /// `{time}`: 13:28.
    pub time: String,
    /// `{app}`: the context window's app ID.
    pub app: String,
    /// `{window}`: its title.
    pub window: String,
}

impl Vars {
    /// What they are at `unix` seconds in `zone`.
    pub fn at(zone: &dyn Zone, unix: i64, app: &str, window: &str) -> Self {
        let c = zone.to_civil(unix);
        Self {
            date: format!("{:04}-{:02}-{:02}", c.year, c.month, c.day),
            time: format!("{:02}:{:02}", c.hour, c.minute),
            app: app.to_owned(),
            window: window.to_owned(),
        }
    }

    /// What they are now, here.
    pub fn now(app: &str, window: &str) -> Self {
        Self::at(&Local, wl_shell_notes::due::now(), app, window)
    }

    fn get(&self, name: &str) -> Option<&str> {
        match name {
            "date" => Some(&self.date),
            "time" => Some(&self.time),
            "app" => Some(&self.app),
            "window" => Some(&self.window),
            _ => None,
        }
    }
}

/// `text` with its placeholders filled in. Anything else in braces stays
/// as it is, and so does what a value holds (it is not filled in again).
pub fn fill(text: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let value = rest
            .find('}')
            .and_then(|close| Some((vars.get(&rest[1..close])?, close + 1)));
        match value {
            Some((value, used)) => {
                out.push_str(value);
                rest = &rest[used..];
            }
            None => {
                out.push('{');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text of a new note made from `template`: its frontmatter copied
/// (comments, order and unknown keys too), the title and text filled in,
/// attached to `context` (an app ID) if the template has no matchers of
/// its own, and with links to `files` at the end of the text.
pub fn text(
    template: &Note,
    context: Option<&str>,
    vars: &Vars,
    files: &[PathBuf],
) -> Result<String, String> {
    if let Some(e) = &template.error {
        return Err(format!("the template's {e}"));
    }
    // The body is the end of the file: what comes before it is the
    // frontmatter, as it is.
    let source = template.text();
    let front = &source[..source.len() - template.body().len()];
    let mut note = Note::parse(format!("{front}{}", fill(template.body(), vars)));
    if let Some(title) = template.meta.title.as_deref() {
        match fill(title, vars).trim() {
            "" => note.set("title", None)?,
            title => note.set_value("title", title)?,
        }
    }
    if template.global()
        && let Some(app) = context.filter(|a| !a.is_empty())
    {
        let mut apps = toml_edit::Array::new();
        apps.push(matcher::escape(app));
        note.set_value("apps", apps)?;
    }
    let mut text = note.text().to_owned();
    if !files.is_empty() {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        if note.body().is_empty() && !text.is_empty() && !text.ends_with('\n') {
            text.push_str(newline);
        }
        text.push_str(&files::append(note.body(), files, newline));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_notes::Window;
    use wl_shell_notes::due::Fixed;

    fn vars() -> Vars {
        // 2026-11-19 12:28 UTC, an hour east: 13:28.
        let unix = 1_795_091_280;
        Vars::at(&Fixed(60), unix, "firefox", "Docs - Mozilla Firefox")
    }

    fn parse(text: &str) -> Note {
        Note::parse(text)
    }

    #[test]
    fn the_placeholders_are_time_app_and_window() {
        let v = vars();
        assert_eq!((v.date.as_str(), v.time.as_str()), ("2026-11-19", "13:28"));
        assert_eq!(
            fill("{date} {time} in {app}: {window}", &v),
            "2026-11-19 13:28 in firefox: Docs - Mozilla Firefox"
        );
        assert_eq!(fill("{date}{date}", &v), "2026-11-192026-11-19");
        // The date at the other side of midnight is the other day's.
        let late = Vars::at(&Fixed(60), 1_795_091_280 + 11 * 3600, "", "");
        assert_eq!(
            (late.date.as_str(), late.time.as_str()),
            ("2026-11-20", "00:28")
        );
    }

    #[test]
    fn other_braces_stay_as_they_are() {
        let v = vars();
        for same in [
            "",
            "plain",
            "{}",
            "{unknown}",
            "{ date }",
            "{DATE}",
            "{",
            "}",
            "{{",
            "a { b",
            "fn main() { x }",
            "{date",
        ] {
            let filled = fill(same, &v);
            assert_eq!(filled, same.replace("{date}", &v.date), "{same}");
        }
        // Braces around a placeholder, and a lone one before it.
        assert_eq!(fill("{{date}}", &v), "{2026-11-19}");
        assert_eq!(fill("{ {time}", &v), "{ 13:28");
        assert_eq!(fill("é {app} ü", &v), "é firefox ü");
    }

    #[test]
    fn a_value_is_not_filled_in_again() {
        let mut v = vars();
        v.window = "{date} and {app}".into();
        assert_eq!(fill("{window}", &v), "{date} and {app}");
    }

    #[test]
    fn a_note_from_a_template_for_the_context() {
        let template = parse(
            "+++\n# how meetings go\ntitle = \"Meeting {date}\"\ncolor = \"blue\"\n+++\n## {app}: {window}\n\n- [ ] agenda at {time}\n",
        );
        let t = text(&template, Some("firefox"), &vars(), &[]).unwrap();
        // The frontmatter as it was (its comment too), plus the app.
        assert_eq!(
            t,
            "+++\n# how meetings go\ntitle = \"Meeting 2026-11-19\"\ncolor = \"blue\"\napps = [\"firefox\"]\n+++\n## firefox: Docs - Mozilla Firefox\n\n- [ ] agenda at 13:28\n"
        );
        let note = Note::parse(t);
        assert!(note.error.is_none() && note.warnings.is_empty());
        assert_eq!(note.title(), Some("Meeting 2026-11-19"));
        assert!(note.matches(Some(&Window::new("firefox", ""))));
        assert!(!note.matches(Some(&Window::new("foot", ""))));
    }

    #[test]
    fn without_a_context_it_is_global() {
        let template = parse("# Notes {date}\n\ntext\n");
        for context in [None, Some("")] {
            let t = text(&template, context, &vars(), &[]).unwrap();
            assert_eq!(t, "# Notes 2026-11-19\n\ntext\n");
            assert!(Note::parse(t).global());
        }
        // The app ID is escaped, as a new note's is.
        let t = text(&template, Some("org.x[1]*"), &vars(), &[]).unwrap();
        let note = Note::parse(t);
        assert!(note.matches(Some(&Window::new("org.x[1]*", ""))));
        assert!(!note.matches(Some(&Window::new("org.x1-other", ""))));
    }

    #[test]
    fn a_templates_own_matchers_win_over_the_context() {
        let apps = parse("+++\napps = [\"thunderbird\"]\n+++\nreply\n");
        let t = text(&apps, Some("firefox"), &vars(), &[]).unwrap();
        assert_eq!(t, apps.text());
        let note = Note::parse(t);
        assert!(note.matches(Some(&Window::new("thunderbird", ""))));
        assert!(!note.matches(Some(&Window::new("firefox", ""))));
        // A title match and a tag are matchers of its own, too.
        let by_title = parse("+++\n[[match]]\ntitle = \"*wl-shell*\"\n+++\nx\n");
        let t = text(&by_title, Some("foot"), &vars(), &[]).unwrap();
        assert_eq!(t, by_title.text());
        assert!(!Note::parse(t).matches(Some(&Window::new("foot", ""))));
        let tagged = parse("+++\ntags = [\"browser\"]\n+++\nx\n");
        let t = text(&tagged, Some("foot"), &vars(), &[]).unwrap();
        assert_eq!(t, tagged.text());
        assert!(!Note::parse(t).global());
        // An empty list names nothing: the context's.
        let empty = parse("+++\napps = []\n+++\nx\n");
        let t = text(&empty, Some("foot"), &vars(), &[]).unwrap();
        assert!(Note::parse(t).matches(Some(&Window::new("foot", ""))));
    }

    #[test]
    fn the_title_is_filled_in_alone() {
        // A heading title is part of the text.
        let heading = parse("+++\ncolor = \"pink\"\n+++\n# Standup {date}\n");
        let t = text(&heading, None, &vars(), &[]).unwrap();
        assert_eq!(Note::parse(t).title(), Some("Standup 2026-11-19"));
        // A title that is nothing once filled in goes.
        let blank = parse("+++\ntitle = \"{window}\"\n+++\nx\n");
        let mut v = vars();
        v.window = String::new();
        let t = text(&blank, None, &v, &[]).unwrap();
        assert_eq!(t, "x\n");
        // Quotes in a window's title stay in the TOML.
        let quoted = parse("+++\ntitle = \"{window}\"\n+++\nx\n");
        let mut v = vars();
        v.window = "say \"hi\" \\ there".into();
        let note = Note::parse(text(&quoted, None, &v, &[]).unwrap());
        assert!(note.error.is_none());
        assert_eq!(note.title(), Some("say \"hi\" \\ there"));
    }

    #[test]
    fn a_due_time_and_other_keys_are_copied() {
        let template = parse("+++\ndue = 2026-12-01T09:00\nextra = \"kept\"\n+++\nbody {date}\n");
        let t = text(&template, Some("foot"), &vars(), &[]).unwrap();
        let note = Note::parse(t.clone());
        assert!(note.due().is_some());
        assert!(t.contains("extra = \"kept\""));
        assert!(t.ends_with("+++\nbody 2026-11-19\n"));
    }

    #[test]
    fn a_broken_template_is_an_error() {
        let broken = parse("+++\ntitle = \n+++\nx\n");
        assert!(broken.error.is_some());
        assert!(text(&broken, None, &vars(), &[]).is_err());
        let unterminated = parse("+++\ntitle = \"x\"\n");
        assert!(text(&unterminated, None, &vars(), &[]).is_err());
    }

    #[test]
    fn dropped_files_are_linked_at_the_end() {
        let template = parse("# Trip {date}\n\npacking\n");
        let files = [PathBuf::from("/tmp/ticket.pdf")];
        let t = text(&template, Some("firefox"), &vars(), &files).unwrap();
        let note = Note::parse(t.clone());
        assert!(note.body().starts_with("# Trip 2026-11-19\n\npacking\n"));
        assert!(note.body().contains("ticket.pdf"));
        assert!(t.contains("file:///tmp/ticket.pdf"));
        // A template with no text at all gets only the files.
        let empty = parse("+++\ntitle = \"T\"\n+++\n");
        let t = text(&empty, None, &vars(), &files).unwrap();
        assert!(Note::parse(t).body().contains("ticket.pdf"));
        // Without files, nothing is added.
        assert_eq!(
            text(&template, None, &vars(), &[]).unwrap(),
            "# Trip 2026-11-19\n\npacking\n"
        );
    }

    #[test]
    fn the_directory_is_a_dot_directory_of_the_notes() {
        let d = dir(Path::new("/home/u/Notes/wl-note"));
        assert_eq!(d, Path::new("/home/u/Notes/wl-note/.templates"));
        // The index leaves it out, so a template is never shown as a note.
        let tmp = std::env::temp_dir().join(format!("wl-note-templates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(dir(&tmp)).unwrap();
        std::fs::write(dir(&tmp).join("meeting.md"), "# Meeting\n").unwrap();
        std::fs::write(tmp.join("note.md"), "# Note\n").unwrap();
        let (notes, _) = wl_shell_notes::Index::load(&tmp);
        assert_eq!(notes.len(), 1);
        let (templates, _) = wl_shell_notes::Index::load(dir(&tmp));
        assert_eq!(templates.len(), 1);
        assert!(templates.get(Path::new("meeting.md")).is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
