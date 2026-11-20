//! What drops and pastes bring (links to files, text, links with their
//! titles, or an image kept with the notes), and its representation in a
//! note's Markdown.
//!
//! Only a file list, a link, images and text are ever read. A drag's
//! other types stay unread: GTK sources list `DELETE`, and a read of it
//! is how a move's target tells the source to delete the original.

use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use wl_shell_render::files::{file_uri, uri_list, uri_path};

pub const URI_LIST: &str = "text/uri-list";

/// A link and its title, as Firefox drags one (and maybe other browsers):
/// UTF-16, the URL, a newline, the title.
const LINK: &str = "text/x-moz-url";

/// Titles longer than this many characters are cut at a word.
const MAX_TITLE: usize = 80;

/// The text types, best first: Thunderbird's bare `text/plain` escapes
/// what isn't ASCII (`\C3\A4` for `ä`), so UTF-8 comes first.
pub const TEXT: [&str; 3] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"];

/// The image types taken, best first, with their files' extensions. PNG
/// first, as it loses nothing of what the source decoded.
const IMAGES: [(&str, &str); 4] = [
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/webp", "webp"),
    ("image/gif", "gif"),
];

/// Where dropped and pasted images are kept, in the notes directory: a
/// dot directory, so the index leaves it out, but synced with the notes.
pub const IMAGES_DIR: &str = ".files";

fn is_image(mime: &str) -> bool {
    IMAGES.iter().any(|(m, _)| m.eq_ignore_ascii_case(mime))
}

fn is(mime: &str, of: &str) -> bool {
    mime.eq_ignore_ascii_case(of)
}

/// The types to read of those `mimes` offers, best first.
///
/// With a file list or a link: those, then the images, then one text
/// type. A browser offers an image on a page with the link around it;
/// the image itself is what was dragged, the link its source. Without
/// either: the text, then the images, so a text selection from a source
/// that may also offer a picture of it stays text.
pub fn readable(mimes: &[String]) -> Vec<String> {
    let find = |mime: &str| mimes.iter().find(|m| is(m, mime)).cloned();
    let links: Vec<_> = find(URI_LIST).into_iter().chain(find(LINK)).collect();
    let images = IMAGES.iter().filter_map(|(m, _)| find(m));
    let text = TEXT.into_iter().find_map(find);
    match links.is_empty() {
        false => links.into_iter().chain(images).chain(text).collect(),
        true => text.into_iter().chain(images).collect(),
    }
}

/// What one read of a drop's or paste's types gave.
#[derive(Debug, PartialEq)]
pub enum Read {
    /// Take this.
    Take(Dropped),
    /// An image, to save with this extension and link (`keep_image`),
    /// with what waits in the fallback as its source.
    Image(&'static str),
    /// Nothing usable: read the next type.
    Next,
}

/// What `data` of type `mime` gives, `rest` being the types still to
/// read.
///
/// A web link (from the list, or with its title) waits in `fallback`
/// while an image or a link with a title may still come: it is an
/// image's source, a later link replaces it, and a text type (the same
/// URL, from a browser) takes it instead. Once nothing else comes, take
/// `fallback`.
pub fn read(mime: &str, data: &[u8], rest: &[String], fallback: &mut Option<Dropped>) -> Read {
    if is_image(mime) {
        return match image_ext(mime, data) {
            Some(ext) => Read::Image(ext),
            None => Read::Next,
        };
    }
    if TEXT.iter().any(|t| is(mime, t))
        && let Some(link) = fallback.take()
    {
        return Read::Take(link);
    }
    let parsed = match is(mime, LINK) {
        true => titled_link(data).map(Dropped::Text),
        false => Dropped::parse(mime, data),
    };
    let more = rest.iter().any(|m| is_image(m) || is(m, LINK));
    match parsed {
        Some(link @ Dropped::Text(_)) if (is(mime, URI_LIST) || is(mime, LINK)) && more => {
            *fallback = Some(link);
            Read::Next
        }
        Some(dropped) => Read::Take(dropped),
        None => Read::Next,
    }
}

/// A `text/x-moz-url` as Markdown: `[title](<url>)`, or `<url>` if it
/// has no title. One web link only.
fn titled_link(data: &[u8]) -> Option<String> {
    let utf16 = data.len().is_multiple_of(2) && data.iter().skip(1).step_by(2).any(|b| *b == 0);
    let text = match utf16 {
        true => {
            let units: Vec<u16> = (data.as_chunks::<2>().0.iter())
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            String::from_utf16(&units).ok()?
        }
        false => String::from_utf8(data.to_vec()).ok()?,
    };
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.split('\n').map(|l| l.trim_matches(['\r', '\0']));
    let url = lines.next()?.trim();
    let title = lines.next().unwrap_or_default();
    // More links than one: not one link (the text type has them).
    if lines.any(|l| !l.trim().is_empty()) {
        return None;
    }
    let auto = crate::row::paste_text(url).filter(|s| s.starts_with('<'))?;
    let title = short_title(title);
    if title.is_empty() || title == url || url.contains('\\') {
        return Some(auto);
    }
    let title = label(&escape_references(&title));
    Some(format!("[{title}](<{}>)", escape_references(url)))
}

/// `title` on one line, its spaces collapsed, cut at a word if long.
fn short_title(title: &str) -> String {
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.chars().count() <= MAX_TITLE {
        return title;
    }
    let cut: String = title.chars().take(MAX_TITLE).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > MAX_TITLE / 2 => &cut[..i],
        _ => &cut,
    };
    format!(
        "{}…",
        cut.trim_end_matches([' ', ',', ';', ':', '-', '|', '·'])
    )
}

/// `text` for a link's text or destination, where CommonMark decodes
/// `&name;` and `&#123;`: such an `&` is written `&amp;`.
fn escape_references(url: &str) -> String {
    let mut out = String::new();
    for (i, c) in url.char_indices() {
        let reference = || {
            let rest = &url[i + 1..];
            rest.find(';').is_some_and(|end| {
                end > 0
                    && rest[..end]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '#')
            })
        };
        match c {
            '&' if reference() => out.push_str("&amp;"),
            c => out.push(c),
        }
    }
    out
}

/// The extension for an image of type `mime`, if `data` starts as such
/// an image does (sources offer types they then send nothing for).
fn image_ext(mime: &str, data: &[u8]) -> Option<&'static str> {
    let (_, ext) = IMAGES.iter().find(|(m, _)| m.eq_ignore_ascii_case(mime))?;
    let ok = match *ext {
        "png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "jpg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        "gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        _ => false,
    };
    ok.then_some(ext)
}

/// Write an image into `dir` (made if missing) as `stem.ext`, or
/// `stem-2.ext` and so on if taken. Never replaces a file.
pub fn save_image(dir: &Path, stem: &str, ext: &str, data: &[u8]) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for n in 1..1000 {
        let name = match n {
            1 => format!("{stem}.{ext}"),
            n => format!("{stem}-{n}.{ext}"),
        };
        let path = dir.join(name);
        let mut file = match std::fs::File::create_new(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        if let Err(e) = file.write_all(data).and_then(|()| file.sync_all()) {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
        return Ok(path);
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("{stem}.{ext}: every name is taken"),
    ))
}

/// Keep an image that came as data (`Read::Image`) in the notes
/// directory `notes`, named after the time, and give it as a file to
/// link, with the link it came from (`read`'s fallback) as its source.
pub fn keep_image(
    notes: &Path,
    ext: &str,
    data: &[u8],
    source: Option<Dropped>,
) -> Result<Dropped, String> {
    let dir = notes.join(IMAGES_DIR);
    let path = save_image(&dir, &crate::create::now_stem(), ext, data)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(match source {
        Some(Dropped::Text(source)) => Dropped::Image { path, source },
        _ => Dropped::Files(vec![path]),
    })
}

/// What a drop or paste brings.
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    /// Local files, to link.
    Files(Vec<PathBuf>),
    /// Text (a lone URL already as `<url>`, a titled one as a link).
    Text(String),
    /// An image kept with the notes, and the link it came from.
    Image { path: PathBuf, source: String },
}

impl Dropped {
    /// What `data` of type `mime` brings; None if nothing usable, so the
    /// next type can be tried. A uri-list of local files gives files, of
    /// one URL gives it as text; anything else is text.
    pub fn parse(mime: &str, data: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(data).ok()?;
        if !mime.eq_ignore_ascii_case(URI_LIST) {
            return crate::row::paste_text(text).map(Dropped::Text);
        }
        if let Some(paths) = wl_shell_render::files::parse_uri_list(text) {
            return Some(Dropped::Files(paths));
        }
        let mut urls = (text.lines())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'));
        let first = urls.next()?;
        match urls.next() {
            None => crate::row::paste_text(first).map(Dropped::Text),
            Some(_) => None,
        }
    }

    /// What to add at the end of `body` for it, preserving all old bytes.
    /// An image's source is a paragraph after it.
    pub fn append(&self, body: &str, newline: &str) -> String {
        match self {
            Dropped::Files(paths) => append(body, paths, newline),
            Dropped::Text(text) => append_text(body, text, newline),
            Dropped::Image { path, source } => {
                let mut out = append(body, std::slice::from_ref(path), newline);
                out += &append_text(&format!("{body}{out}"), source, newline);
                out
            }
        }
    }

    /// What to paste at the editor's byte offset `at` in `body`: files in
    /// their own block, text right there.
    pub fn insert(&self, body: &str, at: usize) -> String {
        match self {
            Dropped::Files(paths) => insert(body, at, paths),
            Dropped::Text(text) => text_lines(text).collect::<Vec<_>>().join("\n"),
            Dropped::Image { .. } => insert_block(body, at, |before| self.append(before, "\n")),
        }
    }
}

/// What starts a new block after `body`: ending its last line, then a
/// blank line.
fn block_start(body: &str, newline: &str) -> String {
    let mut out = String::new();
    if !body.is_empty() && !body.ends_with('\n') {
        out.push_str(newline);
    }
    if !body.trim().is_empty()
        && body
            .lines()
            .last()
            .is_some_and(|line| !line.trim().is_empty())
    {
        out.push_str(newline);
    }
    out
}

/// The lines of dropped text, without the blank space around it or at
/// their ends (a non-breaking space too, which apps add).
fn text_lines(text: &str) -> impl Iterator<Item = &str> {
    text.trim().lines().map(str::trim_end)
}

/// `text` as a block of its own after `body`, in `newline`s.
fn append_text(body: &str, text: &str, newline: &str) -> String {
    let mut out = block_start(body, newline);
    for line in text_lines(text) {
        out.push_str(line);
        out.push_str(newline);
    }
    out
}

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

/// `title` as a link's text, escaping only what would make Markdown of
/// it, so the note stays readable as it is.
fn label(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if matches!(c, '\\' | '[' | ']' | '*' | '_' | '`' | '<' | '>' | '~') {
            out.push('\\');
        }
        out.push(c);
    }
    out
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
    insert_block(body, at, |before| append(before, paths, "\n"))
}

/// Insert what `block` gives for the text before the editor's byte
/// offset `at`, ending the line after it if more text follows there.
fn insert_block(body: &str, at: usize, block: impl FnOnce(&str) -> String) -> String {
    let (before, after) = body.split_at(at);
    let mut text = block(before);
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

    fn strings(m: &[&str]) -> Vec<String> {
        m.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn images_come_before_a_list_s_url_and_after_plain_text() {
        // Firefox dragging an image (from the probe), DELETE and all.
        let firefox = strings(&[
            "text/x-moz-url",
            "_NETSCAPE_URL",
            "text/html",
            "text/plain",
            "text/plain; charset=utf-8",
            "UTF8_STRING",
            "application/x-moz-nativeimage",
            "image/png",
            "image/jpeg",
            "image/jpg",
            "image/gif",
            "application/x-moz-file-promise",
            "text/uri-list",
            "text/plain;charset=utf-8",
            "DELETE",
        ]);
        assert_eq!(
            readable(&firefox),
            [
                "text/uri-list",
                "text/x-moz-url",
                "image/png",
                "image/jpeg",
                "image/gif",
                "text/plain;charset=utf-8"
            ]
        );
        // No list: a selection offering a picture of itself stays text.
        let office = strings(&["image/png", "text/plain;charset=utf-8"]);
        assert_eq!(readable(&office), ["text/plain;charset=utf-8", "image/png"]);
        assert_eq!(readable(&strings(&["image/jpeg"])), ["image/jpeg"]);
        // A link dragged in Firefox: no list, but a titled link first.
        let link = strings(&[
            "text/x-moz-url",
            "_NETSCAPE_URL",
            "text/plain;charset=utf-8",
        ]);
        assert_eq!(
            readable(&link),
            ["text/x-moz-url", "text/plain;charset=utf-8"]
        );
    }

    fn utf16(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn titled_links_from_the_browser() {
        // Probed: a link in a search result.
        let fast =
            utf16("https://fast.com/\nFast.com: Internet Speed Test Fast.com https://fast.com");
        assert_eq!(
            titled_link(&fast).as_deref(),
            Some("[Fast.com: Internet Speed Test Fast.com https://fast.com](<https://fast.com/>)")
        );
        // Probed: an image result's link, its title the text around it.
        let long = utf16(
            "https://oxfordlearning.com/common-types-of-test-takers/\nCommon Types of Test \
             Takers (& Study Tips For Each) | Oxford LearningCommon Types of Test Takers (& \
             Study Tips For Each) | Oxford Learning1,500 × 1,0001,500 × 1,000",
        );
        let link = titled_link(&long).unwrap();
        let (title, url) = link.split_once("](").unwrap();
        assert!(
            title.starts_with("[Common Types of Test Takers (& Study Tips"),
            "{link}"
        );
        assert!(
            title.ends_with('…') && title.chars().count() <= MAX_TITLE + 2,
            "{link}"
        );
        assert_eq!(
            url,
            "<https://oxfordlearning.com/common-types-of-test-takers/>)"
        );
        // Markdown in a title, and references, stay as they read.
        let marked = utf16("https://a.se/?q=1&amp;x\n[*Big*] &copy; deal\n");
        assert_eq!(
            titled_link(&marked).as_deref(),
            Some(r"[\[\*Big\*\] &amp;copy; deal](<https://a.se/?q=1&amp;amp;x>)")
        );
        // No title, or the URL as it: `<url>`. UTF-8 is read too.
        assert_eq!(
            titled_link(b"https://a.se/\n").as_deref(),
            Some("<https://a.se/>")
        );
        let same = utf16("https://a.se/\nhttps://a.se/");
        assert_eq!(titled_link(&same).as_deref(), Some("<https://a.se/>"));
        // Not a web link, or more than one.
        assert_eq!(titled_link(&utf16("file:///tmp/a\na")), None);
        assert_eq!(
            titled_link(&utf16("https://a.se/\na\nhttps://b.se/\nb")),
            None
        );
    }

    #[test]
    fn an_image_keeps_the_link_it_came_from() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n rest";
        let rest = strings(&["text/x-moz-url", "image/png", "text/plain"]);
        let mut fallback = None;
        let list = read(URI_LIST, b"https://a.se/page\r\n", &rest, &mut fallback);
        assert_eq!(list, Read::Next);
        // The titled link replaces the bare one.
        let titled = utf16("https://a.se/page\nThe page");
        assert_eq!(read(LINK, &titled, &rest[1..], &mut fallback), Read::Next);
        let source = Dropped::Text("[The page](<https://a.se/page>)".into());
        assert_eq!(fallback, Some(source.clone()));
        assert_eq!(
            read("image/png", PNG, &rest[2..], &mut fallback),
            Read::Image("png")
        );
        // No image after all: the text type (the same URL) takes the link.
        let mut waiting = Some(source.clone());
        let plain = read("text/plain", b"https://a.se/page", &[], &mut waiting);
        assert_eq!(plain, Read::Take(source.clone()));
        // The source is a paragraph after the image, in an edit too.
        let image = Dropped::Image {
            path: "/n/.files/x.png".into(),
            source: "[The page](<https://a.se/page>)".into(),
        };
        let link = "- [x\\.png](<file:///n/.files/x.png>)\n";
        let expected = format!("\n{link}\n[The page](<https://a.se/page>)\n");
        assert_eq!(image.append("text\n", "\n"), expected);
        assert_eq!(image.insert("text\n", 5), expected);
        // More text after the drop point: it starts on a line of its own.
        assert_eq!(image.insert("text\nmore", 5), format!("{expected}\n"));
    }

    #[test]
    fn a_list_s_web_url_waits_for_the_image() {
        const PNG: &[u8] = b"\x89PNG\r\n\x1a\n rest";
        let url = b"https://example.com/cat\r\n";
        let link = Dropped::Text("<https://example.com/cat>".into());
        let mut fallback = None;
        let rest = strings(&["image/png", "text/plain"]);
        assert_eq!(read(URI_LIST, url, &rest, &mut fallback), Read::Next);
        assert_eq!(fallback, Some(link.clone()));
        assert_eq!(
            read("image/png", PNG, &rest[1..], &mut fallback),
            Read::Image("png")
        );
        // A type offered and sent empty, or not what it says: the next.
        assert_eq!(read("image/png", b"", &[], &mut fallback), Read::Next);
        assert_eq!(read("image/jpeg", PNG, &[], &mut fallback), Read::Next);
        // No image to come: the link is taken at once.
        let mut none = None;
        let text = strings(&["text/plain"]);
        assert_eq!(read(URI_LIST, url, &text, &mut none), Read::Take(link));
        assert_eq!(none, None);
        // Local files are always taken.
        let files = read(URI_LIST, b"file:///tmp/a.png\r\n", &rest, &mut none);
        assert_eq!(files, Read::Take(Dropped::Files(vec!["/tmp/a.png".into()])));
    }

    #[test]
    fn kept_images_never_replace_a_file() {
        let dir = std::env::temp_dir().join(format!("wl-note-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = save_image(&dir, "2026-11-20-1041", "png", b"one").unwrap();
        let b = save_image(&dir, "2026-11-20-1041", "png", b"two").unwrap();
        assert_eq!(a.file_name().unwrap(), "2026-11-20-1041.png");
        assert_eq!(b.file_name().unwrap(), "2026-11-20-1041-2.png");
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_files_first_then_utf8_text() {
        let mimes = |m: &[&str]| m.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Thunderbird dragging text: its UTF-8 type, not the escaped one.
        let offered = mimes(&["text/html", "text/plain", "text/plain;charset=utf-8"]);
        assert_eq!(readable(&offered), ["text/plain;charset=utf-8"]);
        let offered = mimes(&["DELETE", "text/plain", "text/uri-list"]);
        assert_eq!(readable(&offered), ["text/uri-list", "text/plain"]);
        assert!(readable(&mimes(&["DELETE", "application/x-moz-internal-item-list"])).is_empty());
    }

    #[test]
    fn parses_what_a_drop_brings() {
        let files = Dropped::parse(URI_LIST, b"file:///tmp/a%20b.eml\r\n");
        assert_eq!(files, Some(Dropped::Files(vec!["/tmp/a b.eml".into()])));
        let url = Dropped::parse(URI_LIST, b"https://example.com/x\r\n");
        assert_eq!(url, Some(Dropped::Text("<https://example.com/x>".into())));
        // Several URLs that aren't files: the text type is tried next.
        assert_eq!(
            Dropped::parse(URI_LIST, b"https://a.se\r\nhttps://b.se\r\n"),
            None
        );
        let text = Dropped::parse("text/plain;charset=utf-8", " fråga Anna\n".as_bytes());
        assert_eq!(text, Some(Dropped::Text(" fråga Anna\n".into())));
        assert_eq!(Dropped::parse("text/plain", b" \n"), None);
        assert_eq!(Dropped::parse("text/plain", b"\xff\xfe"), None);
    }

    #[test]
    fn dropped_text_is_its_own_block_or_goes_in_place() {
        // The non-breaking space Thunderbird puts first goes with the trim.
        let text = Dropped::Text("\u{a0}pratade om SP1?\u{a0}\r\nVad tycker ni?\n".into());
        let expected = "pratade om SP1?\nVad tycker ni?\n";
        assert_eq!(text.append("", "\n"), expected);
        assert_eq!(text.append("a", "\n"), format!("\n\n{expected}"));
        assert_eq!(text.append("a\n", "\n"), format!("\n{expected}"));
        assert_eq!(text.append("a\n\n", "\n"), expected);
        // After a file list, a paragraph of its own, not an item.
        let after = text.append("- [a](file:///tmp/a)\n", "\r\n");
        assert_eq!(after, "\r\npratade om SP1?\r\nVad tycker ni?\r\n");
        // In an editor: right at the offset, as typed.
        assert_eq!(text.insert("abc", 1), "pratade om SP1?\nVad tycker ni?");
    }
}
