//! Templates: notes made from the files of `notes-dir/.templates/`. Pure,
//! unit tested.
//!
//! A template is a note itself (frontmatter and markdown), kept in a dot
//! directory so it is never shown as one. A note made from it is a copy
//! with `{date}`, `{time}`, `{app}` and `{window}` filled in, in its text
//! and in the globs of its `apps` and `match` (escaped there, so they
//! match as they are). It is attached to what those name, and nothing
//! else: a template without matchers or tags makes a global note. A
//! matcher whose placeholder has no value (`{app}` without a window) is
//! left out.

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, TableLike, Value};
use wl_shell_notes::due::{Local, Zone};
use wl_shell_notes::{Note, matcher, toml_edit};

use crate::files::Dropped;

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
    replace(text, vars, str::to_owned)
}

/// `glob` with its placeholders filled in, escaped so each matches as it
/// is. None if one of them has no value: the glob would match more than
/// meant.
fn fill_glob(glob: &str, vars: &Vars) -> Option<String> {
    let mut missing = false;
    let out = replace(glob, vars, |value| {
        missing |= value.is_empty();
        matcher::escape(value)
    });
    (!missing).then_some(out)
}

/// `text` with each placeholder replaced by `f` of its value.
fn replace(text: &str, vars: &Vars, mut f: impl FnMut(&str) -> String) -> String {
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
                out.push_str(&f(value));
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

/// Fill in a matcher's glob `v` (a string; anything else stays), keeping
/// its decor. False if it is to be left out (see `fill_glob`).
fn fill_value(v: &mut Value, vars: &Vars) -> bool {
    let Some(s) = v.as_str() else { return true };
    match fill_glob(s, vars) {
        Some(filled) if filled == s => true,
        Some(filled) => {
            let mut new = Value::from(filled);
            *new.decor_mut() = v.decor().clone();
            *v = new;
            true
        }
        None => false,
    }
}

/// Fill in a `match` table's `app` and `title`. False if it is to be left
/// out.
fn fill_table(t: &mut dyn TableLike, vars: &Vars) -> bool {
    ["app", "title"].into_iter().all(|k| {
        t.get_mut(k)
            .and_then(Item::as_value_mut)
            .is_none_or(|v| fill_value(v, vars))
    })
}

/// Fill in the globs of `doc`'s `apps` and `match`, leaving out the
/// matchers that are to be. A key left empty by that goes.
fn fill_matchers(doc: &mut DocumentMut, vars: &Vars) {
    if let Some(apps) = doc.get_mut("apps").and_then(Item::as_array_mut) {
        let keep: Vec<bool> = apps.iter_mut().map(|v| fill_value(v, vars)).collect();
        if keep.contains(&false) {
            let mut keep = keep.into_iter();
            apps.retain(|_| keep.next().unwrap_or(true));
            if apps.is_empty() {
                doc.remove("apps");
            }
        }
    }
    let Some(item) = doc.get_mut("match") else {
        return;
    };
    let emptied = if let Some(aot) = item.as_array_of_tables_mut() {
        let keep: Vec<bool> = aot.iter_mut().map(|t| fill_table(t, vars)).collect();
        let dropped = keep.contains(&false);
        let mut keep = keep.into_iter();
        aot.retain(|_| keep.next().unwrap_or(true));
        dropped && aot.is_empty()
    } else if let Some(array) = item.as_array_mut() {
        let keep: Vec<bool> = (array.iter_mut())
            .map(|v| v.as_inline_table_mut().is_none_or(|t| fill_table(t, vars)))
            .collect();
        let dropped = keep.contains(&false);
        let mut keep = keep.into_iter();
        array.retain(|_| keep.next().unwrap_or(true));
        dropped && array.is_empty()
    } else {
        false
    };
    if emptied {
        doc.remove("match");
    }
}

/// The text of a new note made from `template`: its frontmatter copied
/// (comments, order and unknown keys too), the text and the matchers'
/// globs filled in, and with what `add` brings (links to files, or text)
/// at the end of the text.
pub fn text(template: &Note, vars: &Vars, add: Option<&Dropped>) -> Result<String, String> {
    if let Some(e) = &template.error {
        return Err(format!("the template's {e}"));
    }
    // The body is the end of the file: what comes before it is the
    // frontmatter, as it is.
    let source = template.text();
    let front = &source[..source.len() - template.body().len()];
    let mut note = Note::parse(format!("{front}{}", fill(template.body(), vars)));
    // Only a frontmatter that may hold a placeholder is written again.
    if note.frontmatter().is_some_and(|f| f.contains('{')) {
        note.edit(|doc| {
            fill_matchers(doc, vars);
            Ok(())
        })?;
    }
    let mut text = note.text().to_owned();
    if let Some(add) = add {
        let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
        if note.body().is_empty() && !text.is_empty() && !text.ends_with('\n') {
            text.push_str(newline);
        }
        text.push_str(&add.append(note.body(), newline));
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
    fn a_template_without_matchers_makes_a_global_note() {
        let template = parse(
            "+++\n# how meetings go\ncolor = \"blue\"\n+++\n# Meeting {date}\n\n## {app}: {window}\n\n- [ ] agenda at {time}\n",
        );
        let t = text(&template, &vars(), None).unwrap();
        // The frontmatter as it was (its comment too), even with a window.
        assert_eq!(
            t,
            "+++\n# how meetings go\ncolor = \"blue\"\n+++\n# Meeting 2026-11-19\n\n## firefox: Docs - Mozilla Firefox\n\n- [ ] agenda at 13:28\n"
        );
        let note = Note::parse(t);
        assert!(note.error.is_none() && note.warnings.is_empty());
        assert_eq!(note.title(), Some("Meeting 2026-11-19"));
        assert!(note.global());
        // And without a window.
        let none = Vars::at(&Fixed(60), 1_795_091_280, "", "");
        let plain = parse("# Notes {date}\n\ntext\n");
        let t = text(&plain, &none, None).unwrap();
        assert_eq!(t, "# Notes 2026-11-19\n\ntext\n");
        assert!(Note::parse(t).global());
    }

    #[test]
    fn placeholders_in_matchers_are_filled_in_escaped() {
        let mut v = vars();
        v.app = "org.x[1]*".into();
        v.window = "say \"hi\" \\ {date}".into();
        let template = parse(
            "+++\napps = [\"{app}\", \"foot\"] # mine\nextra = \"{app}\"\n[[match]]\napp = \"{app}\"\ntitle = \"*{window}*\"\n+++\n# {app}\n",
        );
        let t = text(&template, &v, None).unwrap();
        let note = Note::parse(t.clone());
        assert!(note.error.is_none() && note.warnings.is_empty(), "{t}");
        // Other keys, and the comment, stay as they are.
        assert!(
            t.contains("extra = \"{app}\"") && t.contains("# mine"),
            "{t}"
        );
        assert!(note.body().starts_with("# org.x[1]*"));
        // The app ID matches as it is, not as a glob.
        assert!(note.matches(Some(&Window::new("org.x[1]*", ""))));
        assert!(note.matches(Some(&Window::new("foot", ""))));
        assert!(!note.matches(Some(&Window::new("org.x1-other", ""))));
        // The title's own globs stay globs; the window's title is as it is.
        let only_match = parse("+++\n[[match]]\napp = \"{app}\"\ntitle = \"*{window}*\"\n+++\nx\n");
        let note = Note::parse(text(&only_match, &v, None).unwrap());
        let title = "a say \"hi\" \\ {date} b";
        assert!(note.matches(Some(&Window::new("org.x[1]*", title))));
        assert!(!note.matches(Some(&Window::new("org.x[1]*", "say hi"))));
        assert!(!note.matches(Some(&Window::new("org.x1", title))));
        // An inline table, too.
        let inline = parse("+++\nmatch = [{ title = \"{window}\" }]\n+++\nx\n");
        let note = Note::parse(text(&inline, &v, None).unwrap());
        assert!(note.matches(Some(&Window::new("any", "say \"hi\" \\ {date}"))));
        assert!(!note.matches(Some(&Window::new("any", "other"))));
    }

    #[test]
    fn a_matcher_whose_placeholder_has_no_value_is_left_out() {
        let none = Vars::at(&Fixed(60), 1_795_091_280, "", "");
        // Its only matcher gone, the note is global.
        let app = parse("+++\ncolor = \"blue\"\napps = [\"{app}\"]\n+++\nx\n");
        let t = text(&app, &none, None).unwrap();
        assert_eq!(t, "+++\ncolor = \"blue\"\n+++\nx\n");
        let only = parse("+++\napps = [\"{app}\"]\n+++\nx\n");
        assert_eq!(text(&only, &none, None).unwrap(), "x\n");
        // The others stay.
        let some = parse(
            "+++\napps = [\"{app}\", \"foot\"]\n[[match]]\napp = \"{app}\"\n[[match]]\ntitle = \"*wl-shell*\"\n+++\nx\n",
        );
        let note = Note::parse(text(&some, &none, None).unwrap());
        assert!(note.matches(Some(&Window::new("foot", ""))));
        assert!(note.matches(Some(&Window::new("any", "~/wl-shell"))));
        assert!(!note.matches(Some(&Window::new("", ""))));
        assert!(!note.matches(Some(&Window::new("any", ""))));
        // A table goes if either of its globs has no value: a window with
        // an app ID but no title.
        let mut untitled = vars();
        untitled.window.clear();
        let both = parse("+++\nmatch = [{ app = \"{app}\", title = \"*{window}*\" }]\n+++\nx\n");
        let t = text(&both, &untitled, None).unwrap();
        assert_eq!(t, "x\n");
        // A tag keeps it from being global.
        let tagged = parse("+++\ntags = [\"t\"]\napps = [\"{app}\"]\n+++\nx\n");
        assert!(!Note::parse(text(&tagged, &none, None).unwrap()).global());
    }

    #[test]
    fn a_templates_own_matchers_are_kept_as_they_are() {
        for same in [
            "+++\napps = [\"thunderbird\"]\n+++\nreply\n",
            "+++\n[[match]]\ntitle = \"*wl-shell*\"\n+++\nx\n",
            "+++\ntags = [\"browser\"]\n+++\nx\n",
            "+++\napps = []\n+++\nx\n",
            "+++\napps = [\"{unknown}\"]\n+++\nx\n",
        ] {
            let template = parse(same);
            assert_eq!(text(&template, &vars(), None).unwrap(), same);
        }
        let note = Note::parse(
            text(
                &parse("+++\napps = [\"thunderbird\"]\n+++\nx\n"),
                &vars(),
                None,
            )
            .unwrap(),
        );
        assert!(note.matches(Some(&Window::new("thunderbird", ""))));
        assert!(!note.matches(Some(&Window::new("firefox", ""))));
    }

    #[test]
    fn title_placeholders_are_filled_in_the_body_only() {
        let template = parse("+++\ntitle = \"{date}\"\n+++\n# Standup {date}\n");
        let t = text(&template, &vars(), None).unwrap();
        assert!(t.contains("title = \"{date}\""));
        assert_eq!(Note::parse(t).title(), Some("Standup 2026-11-19"));
    }

    #[test]
    fn a_due_time_and_other_keys_are_copied() {
        let template = parse("+++\ndue = 2026-12-01T09:00\nextra = \"kept\"\n+++\nbody {date}\n");
        let t = text(&template, &vars(), None).unwrap();
        let note = Note::parse(t.clone());
        assert!(note.due().is_some());
        assert!(t.contains("extra = \"kept\""));
        assert!(t.ends_with("+++\nbody 2026-11-19\n"));
    }

    #[test]
    fn a_broken_template_is_an_error() {
        let broken = parse("+++\ntitle = \n+++\nx\n");
        assert!(broken.error.is_some());
        assert!(text(&broken, &vars(), None).is_err());
        let unterminated = parse("+++\ntitle = \"x\"\n");
        assert!(text(&unterminated, &vars(), None).is_err());
    }

    #[test]
    fn dropped_files_are_linked_at_the_end() {
        let template = parse("# Trip {date}\n\npacking\n");
        let files = Dropped::Files(vec![PathBuf::from("/tmp/ticket.pdf")]);
        let t = text(&template, &vars(), Some(&files)).unwrap();
        let note = Note::parse(t.clone());
        assert!(note.body().starts_with("# Trip 2026-11-19\n\npacking\n"));
        assert!(note.body().contains("ticket.pdf"));
        assert!(t.contains("file:///tmp/ticket.pdf"));
        // A template with no text at all gets only the files.
        let empty = parse("+++\ncolor = \"pink\"\n+++\n");
        let t = text(&empty, &vars(), Some(&files)).unwrap();
        assert!(Note::parse(t).body().contains("ticket.pdf"));
        // Without files, nothing is added.
        assert_eq!(
            text(&template, &vars(), None).unwrap(),
            "# Trip 2026-11-19\n\npacking\n"
        );
        // Dropped text is a block of its own at the end.
        let dropped = Dropped::Text("\u{a0}call Anna\r\nabout SP1 \n".into());
        assert_eq!(
            text(&template, &vars(), Some(&dropped)).unwrap(),
            "# Trip 2026-11-19\n\npacking\n\ncall Anna\nabout SP1\n"
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
