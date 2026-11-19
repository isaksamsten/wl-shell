//! New notes: their text and file name. Pure (but for reading the clock),
//! unit tested.
//!
//! A new note is attached to an app by its app ID (`apps = ["firefox"]`,
//! the ID's glob characters escaped), or global. It is named after the
//! time it was made: `2026-11-17-2059.md`.

use wl_shell_notes::{Color, Due, Note, matcher, toml_edit};

/// A new note's frontmatter (`app`, `color`, `due`), then its Markdown
/// body with a leading heading if `title` is given.
pub fn text(
    app: Option<&str>,
    color: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
    due: Option<&str>,
) -> Result<String, String> {
    let mut body = body.unwrap_or("").to_string();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    let title = title
        .unwrap_or("")
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !title.is_empty() {
        let newline = if body.contains("\r\n") { "\r\n" } else { "\n" };
        // Protect literal trailing hashes from being read as a closing sequence.
        let close = if title.ends_with('#') { " #" } else { "" };
        let gap = if body.is_empty() { "" } else { newline };
        body = format!("# {title}{close}{newline}{gap}{body}");
    }
    // Not taken for a frontmatter of its own.
    if body.starts_with("+++") {
        body.insert(0, '\n');
    }
    let mut note = Note::parse(body);
    if let Some(app) = app.filter(|a| !a.is_empty()) {
        let mut apps = toml_edit::Array::new();
        apps.push(matcher::escape(app));
        note.set_value("apps", apps)?;
    }
    if let Some(color) = color {
        let color = color.trim();
        Color::parse(color)?;
        note.set_value("color", color)?;
    }
    if let Some(due) = due.map(str::trim).filter(|d| !d.is_empty()) {
        let datetime: toml_edit::Datetime = due
            .parse()
            .map_err(|_| format!("due '{due}': expected a TOML date or datetime"))?;
        Due::from_datetime(&datetime)?;
        note.set_due(Some(datetime))?;
    }
    Ok(note.text().to_string())
}

/// A file name (without `.md`) for a note made at this local time.
pub fn stem(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> String {
    format!("{year:04}-{month:02}-{day:02}-{hour:02}{minute:02}")
}

/// `stem` for now.
pub fn now_stem() -> String {
    // SAFETY: time and localtime_r only write to what they are given.
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    stem(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_notes::Window;

    #[test]
    fn texts() {
        assert_eq!(text(None, None, None, None, None).unwrap(), "");
        assert_eq!(
            text(None, None, None, Some("milk"), None).unwrap(),
            "milk\n"
        );
        let t = text(
            Some("firefox"),
            Some("pink"),
            Some(" Release "),
            Some("- [ ] tag\n"),
            None,
        )
        .unwrap();
        assert_eq!(
            t,
            "+++\napps = [\"firefox\"]\ncolor = \"pink\"\n+++\n# Release\n\n- [ ] tag\n"
        );
        let n = Note::parse(t);
        assert!(n.error.is_none() && n.warnings.is_empty());
        assert_eq!(n.title(), Some("Release"));
        assert!(n.matches(Some(&Window::new("firefox", ""))));
        assert!(!n.matches(Some(&Window::new("foot", ""))));
        // App IDs are matched as they are.
        let n = Note::parse(text(Some("org.x[1]*"), None, None, None, None).unwrap());
        assert!(n.matches(Some(&Window::new("org.x[1]*", ""))));
        assert!(!n.matches(Some(&Window::new("org.x1-other", ""))));
        assert!(text(None, Some("teal"), None, None, None).is_err());
        // A body that looks like a frontmatter stays body.
        let n = Note::parse(text(Some("a"), None, None, Some("+++\nx\n+++\n"), None).unwrap());
        assert!(n.error.is_none());
        assert_eq!(n.body(), "\n+++\nx\n+++\n");
        let n = Note::parse(text(None, None, Some("First\r\nSecond #"), None, None).unwrap());
        assert_eq!(n.title(), Some("First Second #"));
    }

    #[test]
    fn stems() {
        assert_eq!(stem(2026, 11, 7, 9, 5), "2026-11-07-0905");
        let now = now_stem();
        assert_eq!(now.len(), "2026-11-17-2059".len(), "{now}");
    }

    #[test]
    fn a_due_time() {
        let t = text(
            Some("firefox"),
            None,
            None,
            Some("call\n"),
            Some("2026-11-20T09:00"),
        )
        .unwrap();
        assert_eq!(
            t,
            "+++\napps = [\"firefox\"]\ndue = 2026-11-20T09:00\n+++\ncall\n"
        );
        assert!(Note::parse(t).due().is_some());
        let t = text(None, None, None, None, Some(" 2026-11-20 ")).unwrap();
        assert_eq!(t, "+++\ndue = 2026-11-20\n+++\n");
        // Nothing given, or only blanks: no due.
        assert_eq!(text(None, None, None, None, Some(" ")).unwrap(), "");
        for bad in ["tomorrow", "09:00", "2026-13-40"] {
            assert!(text(None, None, None, None, Some(bad)).is_err(), "{bad}");
        }
    }
}
