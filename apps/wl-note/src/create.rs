//! New notes: their text and file name. Pure (but for reading the clock),
//! unit tested.
//!
//! A new note is attached to an app by its app ID (`apps = ["firefox"]`,
//! the ID's glob characters escaped), or global. It is named after the
//! time it was made: `2026-11-17-2059.md`.

use wl_shell_notes::{Color, Note, matcher, toml_edit};

/// A new note's text: the frontmatter for `app`, `color` and `title`
/// (each if given), then `body`.
pub fn text(
    app: Option<&str>,
    color: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<String, String> {
    let mut body = body.unwrap_or("").to_string();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    // Not taken for a frontmatter of its own.
    if body.starts_with("+++") {
        body.insert(0, '\n');
    }
    let mut note = Note::parse(body);
    if let Some(title) = title.map(str::trim).filter(|t| !t.is_empty()) {
        note.set_value("title", title)?;
    }
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
        assert_eq!(text(None, None, None, None).unwrap(), "");
        assert_eq!(text(None, None, None, Some("milk")).unwrap(), "milk\n");
        let t = text(
            Some("firefox"),
            Some("pink"),
            Some(" Release "),
            Some("- [ ] tag\n"),
        )
        .unwrap();
        assert_eq!(
            t,
            "+++\ntitle = \"Release\"\napps = [\"firefox\"]\ncolor = \"pink\"\n+++\n- [ ] tag\n"
        );
        let n = Note::parse(t);
        assert!(n.error.is_none() && n.warnings.is_empty());
        assert!(n.matches(Some(&Window::new("firefox", ""))));
        assert!(!n.matches(Some(&Window::new("foot", ""))));
        // App IDs are matched as they are.
        let n = Note::parse(text(Some("org.x[1]*"), None, None, None).unwrap());
        assert!(n.matches(Some(&Window::new("org.x[1]*", ""))));
        assert!(!n.matches(Some(&Window::new("org.x1-other", ""))));
        assert!(text(None, Some("teal"), None, None).is_err());
        // A body that looks like a frontmatter stays body.
        let n = Note::parse(text(Some("a"), None, None, Some("+++\nx\n+++\n")).unwrap());
        assert!(n.error.is_none());
        assert_eq!(n.body(), "\n+++\nx\n+++\n");
    }

    #[test]
    fn stems() {
        assert_eq!(stem(2026, 11, 7, 9, 5), "2026-11-07-0905");
        let now = now_stem();
        assert_eq!(now.len(), "2026-11-17-2059".len(), "{now}");
    }
}
