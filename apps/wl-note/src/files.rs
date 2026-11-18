//! File drop payloads and their representation in a note's Markdown.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use wl_shell_render::files::{file_uri, uri_list, uri_path};

pub const URI_LIST: &str = "text/uri-list";

pub fn offer(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)> {
    let plain = paths
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        (URI_LIST.into(), uri_list(paths).into_bytes()),
        ("text/plain".into(), plain.into_bytes()),
    ]
}

fn linked(body: &str) -> HashSet<PathBuf> {
    Parser::new(body)
        .filter_map(|event| match event {
            Event::Start(Tag::Link { dest_url, .. }) => uri_path(&dest_url),
            _ => None,
        })
        .collect()
}

pub fn keep_unlinked(paths: &mut Vec<PathBuf>, body: &str) {
    let mut known = linked(body);
    paths.retain(|path| known.insert(path.clone()));
}

fn link(path: &Path) -> String {
    let name = crate::bar::name(path);
    let mut label = String::new();
    for c in name.chars() {
        if c.is_control() {
            label.push(' ');
        } else {
            if c.is_ascii_punctuation() {
                label.push('\\');
            }
            label.push(c);
        }
    }
    // CommonMark decodes character references even in angle destinations.
    let uri = file_uri(path).replace('&', "%26");
    let label = if label.trim().is_empty() {
        "file"
    } else {
        &label
    };
    format!("[{label}](<{uri}>)")
}

/// The last block is an unordered list whose items contain just file links.
/// Use its marker and indentation, without rewriting any existing lines.
fn trailing_list(body: &str) -> Option<&str> {
    let (mut last, mut start, mut depth) = (None, None, 0);
    for (event, range) in Parser::new(body).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                if depth == 0 {
                    last = None;
                    start = matches!(tag, Tag::List(None)).then_some(range.start);
                }
                depth += 1;
            }
            Event::End(tag) => {
                depth -= 1;
                if depth == 0 && tag == TagEnd::List(false) {
                    last = Some(start?..range.end);
                }
            }
            _ if depth == 0 => last = None,
            _ => {}
        }
    }
    let range = last?;
    // Definitions and other invisible source after the list aren't a
    // place to continue it, even if the parser emits no events for them.
    if !body[range.end..].trim().is_empty() {
        return None;
    }
    let start = body[..range.start].rfind('\n').map_or(0, |i| i + 1);
    let marker = &body[start..range.start + 1];
    if !matches!(marker.as_bytes().last(), Some(b'-' | b'+' | b'*')) {
        return None;
    }
    for line in body[start..range.end]
        .lines()
        .filter(|l| !l.trim().is_empty())
    {
        let rest = line.strip_prefix(marker)?.strip_prefix([' ', '\t'])?;
        let mut events = Parser::new(rest.trim());
        if !matches!(events.next(), Some(Event::Start(Tag::Paragraph)))
            || !matches!(events.next(), Some(Event::Start(Tag::Link { dest_url, .. })) if uri_path(&dest_url).is_some())
            || !events.any(|e| e == Event::End(TagEnd::Link))
            || events.next() != Some(Event::End(TagEnd::Paragraph))
        {
            return None;
        }
    }
    Some(marker)
}

fn lines(paths: &[PathBuf], marker: &str, newline: &str) -> String {
    paths
        .iter()
        .map(|p| format!("{marker} {}{newline}", link(p)))
        .collect()
}

/// Append a block (or extend the final file list), preserving all old bytes.
pub fn append(body: &str, paths: &[PathBuf], newline: &str) -> String {
    let trailing = trailing_list(body);
    let mut out = String::new();
    if !body.is_empty() && !body.ends_with('\n') {
        out.push_str(newline);
    }
    if trailing.is_none()
        && !body.trim().is_empty()
        && body
            .lines()
            .last()
            .is_some_and(|line| !line.trim().is_empty())
    {
        out.push_str(newline);
    }
    out.push_str(&lines(paths, trailing.unwrap_or("-"), newline));
    out
}

/// Insert at the editor's byte offset, keeping the file links in their own block.
pub fn insert(body: &str, at: usize, paths: &[PathBuf]) -> String {
    let (before, after) = body.split_at(at);
    let mut text = append(before, paths, "\n");
    if !after.trim().is_empty()
        && !after
            .split('\n')
            .next()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_render::files::parse_uri_list;

    #[test]
    fn grows_only_a_trailing_file_list() {
        let paths = [PathBuf::from("/tmp/b [2](#.pdf")];
        for body in [
            "ordinary text",
            "- ordinary item\n",
            "- ordinary item\n\n- [a](file:///tmp/a)\n",
            "```\n- [a](file:///tmp/a)\n```\n",
        ] {
            let suffix = append(body, &paths, "\n");
            assert!(
                suffix.starts_with(if body.ends_with('\n') {
                    "\n- "
                } else {
                    "\n\n- "
                }),
                "{suffix:?}"
            );
            assert!(linked(&format!("{body}{suffix}")).contains(&paths[0]));
        }
        let body = "paragraph\r\n\r\n* [a](file:///tmp/a)\r\n";
        let suffix = append(body, &paths, "\r\n");
        assert!(suffix.starts_with("* "), "{suffix:?}");
        assert!(suffix.ends_with("\r\n"));
        assert_eq!(linked(&format!("{body}{suffix}")).len(), 2);
    }

    #[test]
    fn places_links_at_the_editor_offset_and_undoes_them_together() {
        let paths = [PathBuf::from("/tmp/file.pdf")];
        let body = "åbcdef\n";
        let at = "åbc".len();
        let mut editor = crate::editor::Editor::new(body);
        editor.set_caret(at, false);
        editor.paste(&insert(body, at, &paths));
        assert!(editor.text().starts_with("åbc\n\n- "));
        assert!(editor.text().ends_with("\n\ndef\n"));
        assert_eq!(linked(editor.text()), HashSet::from(paths));
        editor.undo();
        assert_eq!(editor.text(), body);
    }

    #[test]
    fn local_drop_round_trip_and_no_duplicate_links() {
        let paths = vec![PathBuf::from("/tmp/a b.pdf"), PathBuf::from("/tmp/c#d.pdf")];
        let payload = offer(&paths);
        assert_eq!(payload[0].0, URI_LIST);
        let data = std::str::from_utf8(&payload[0].1).unwrap();
        assert_eq!(data, "file:///tmp/a%20b.pdf\r\nfile:///tmp/c%23d.pdf\r\n");
        assert_eq!(parse_uri_list(data), Some(paths.clone()));
        assert!(parse_uri_list(&format!("{data}file://elsewhere/tmp/a\r\n")).is_none());
        assert_eq!(
            payload[1],
            ("text/plain".into(), b"/tmp/a b.pdf\n/tmp/c#d.pdf".to_vec())
        );
        let mut dropped = paths.clone();
        dropped.push(paths[1].clone());
        keep_unlinked(&mut dropped, "[existing](file:///tmp/a%20b.pdf)");
        assert_eq!(dropped, paths[1..]);
    }
}
