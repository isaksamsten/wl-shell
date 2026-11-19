//! A note's markdown as a document to render (`wl_shell_render::doc`),
//! with where its checkboxes are in the file and what its links point at;
//! and as source lines to edit (`wl_shell_render::edit`). Pure, unit
//! tested.
//!
//! Parsed with pulldown-cmark (CommonMark, with task lists and
//! strikethrough). Headings, paragraphs, lists (bulleted, numbered and
//! task lists, nested), block quotes, code blocks and rules become blocks;
//! emphasis, strong, strikethrough, inline code and links become runs. A
//! checked task's text is dimmed and struck through. HTML shows as is.
//!
//! Checkboxes are toggled in the file itself (`toggle`), by the byte range
//! of their `[ ]` or `[x]`, so nothing else in it changes.
//!
//! **Source lines** (`source_lines`) are the editor's: the markdown as
//! typed, styled, its syntax (`**`, `#`, a link's `](…)`, list markers)
//! hidden except on the active lines (the caret's and the selection's),
//! where it shows faint. List markers and checkboxes are drawn in a
//! marker column, with the text hanging beside them.

use std::ops::Range;

use std::path::Path;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use wl_shell_notes::Note;
use wl_shell_render::doc::{Block, Kind, Marker, Run};
use wl_shell_render::edit::{Line, LineMarker, Span};

use crate::editor;

/// A note, ready to render.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Content {
    /// The title: the heading the body opens with, else the file's name.
    pub title: String,
    /// The body (after the frontmatter's error, if it is broken), its
    /// heading included.
    pub blocks: Vec<Block>,
    /// The byte range in the file of each task's `[ ]` or `[x]`, in order:
    /// `Target::Task(n)` is `tasks[n]`.
    pub tasks: Vec<Range<usize>>,
    /// Each link's destination: `Target::Link(n)` is `links[n]`.
    pub links: Vec<String>,
}

impl Content {
    /// A note's content: its title (`name` if it has none), the
    /// frontmatter's error if it is broken, and its body.
    pub fn of_note(note: &Note, name: &str) -> Self {
        let body = note.body();
        let offset = note.text().len() - body.len();
        let mut content = parse(body, offset);
        content.title = note.title_or(name).to_string();
        if let Some(e) = &note.error {
            let run = Run {
                dim: true,
                italic: true,
                ..Run::plain(format!("⚠ {e}"))
            };
            content
                .blocks
                .insert(0, Block::new(Kind::Paragraph, vec![run]));
        }
        content
    }
}

/// The blocks of `body`, which starts at byte `offset` of its file (for
/// the tasks' ranges).
pub fn parse(body: &str, offset: usize) -> Content {
    let mut b = Builder {
        offset,
        ..Default::default()
    };
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        b.event(event, range);
    }
    b.close();
    Content {
        blocks: b.blocks,
        tasks: b.tasks,
        links: b.links,
        ..Default::default()
    }
}

/// Toggle the checkbox whose `[ ]` or `[x]` is at `range` of `text`: the
/// new text, or None if it isn't one there (the file changed).
pub fn toggle(text: &str, range: Range<usize>) -> Option<String> {
    let at = range.start + text.get(range.clone())?.find('[')?;
    let bytes = text.as_bytes();
    if bytes.get(at + 2) != Some(&b']') {
        return None;
    }
    let new = match bytes.get(at + 1)? {
        b' ' => "x",
        b'x' | b'X' => " ",
        _ => return None,
    };
    Some(format!("{}{new}{}", &text[..at + 1], &text[at + 2..]))
}

/// Where a note's editable text starts: after its frontmatter.
pub fn editable(note: &Note) -> usize {
    note.text().len() - note.body().len()
}

/// An ATX heading's level, and its text.
fn heading_level(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let hashes = t.bytes().take_while(|b| *b == b'#').count();
    let rest = &t[hashes..];
    ((1..=6).contains(&hashes) && (rest.is_empty() || rest.starts_with([' ', '\t'])))
        .then(|| (hashes as u8, rest.trim().trim_end_matches('#').trim_end()))
}

/// A style over a range of the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    Bold,
    Italic,
    Strike,
    Code,
    Link(u32),
}

/// `text` (an editor's, with `\n` line breaks) as styled source lines,
/// the lines in `active` showing their syntax; and its links'
/// destinations (`Span::link` indexes them).
pub fn source_lines(text: &str, active: Range<usize>) -> (Vec<Line>, Vec<String>) {
    let mut styles: Vec<(Range<usize>, Style)> = Vec::new();
    let mut marks: Vec<Range<usize>> = Vec::new();
    let mut blocks: Vec<Range<usize>> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    // Open links: their range, and where their text starts and ends.
    let mut open: Vec<(Range<usize>, usize, usize)> = Vec::new();
    let delims = |r: &Range<usize>, chars: &[u8], most: usize| {
        let s = &text.as_bytes()[r.clone()];
        let n = s.iter().take_while(|b| chars.contains(b)).count().min(most);
        let m = s
            .iter()
            .rev()
            .take_while(|b| chars.contains(b))
            .count()
            .min(most);
        (n == m && 2 * n <= s.len()).then_some(n)
    };
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    for (event, r) in Parser::new_ext(text, options).into_offset_iter() {
        if !matches!(
            event,
            Event::Start(Tag::Link { .. } | Tag::Image { .. })
                | Event::End(TagEnd::Link | TagEnd::Image)
        ) && let Some((_, lo, hi)) = open.last_mut()
        {
            *lo = (*lo).min(r.start);
            *hi = (*hi).max(r.end);
        }
        let mut style = |style: Style, chars: &[u8], most: usize| {
            if let Some(n) = delims(&r, chars, most) {
                marks.push(r.start..r.start + n);
                marks.push(r.end - n..r.end);
            }
            styles.push((r.clone(), style));
        };
        match event {
            Event::Start(Tag::Strong) => style(Style::Bold, b"*_", 2),
            Event::Start(Tag::Emphasis) => style(Style::Italic, b"*_", 1),
            Event::Start(Tag::Strikethrough) => style(Style::Strike, b"~", 2),
            Event::Code(_) => style(Style::Code, b"`", usize::MAX),
            Event::Start(Tag::CodeBlock(_)) => blocks.push(r),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                links.push(dest_url.to_string());
                styles.push((r.clone(), Style::Link(links.len() as u32 - 1)));
                open.push((r.clone(), r.end, r.start));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                if let Some((r, lo, hi)) = open.pop() {
                    match lo < hi {
                        true => {
                            marks.push(r.start..lo);
                            marks.push(hi..r.end);
                        }
                        false => marks.push(r.clone()),
                    }
                    // The enclosing link's text includes this one.
                    if let Some((_, plo, phi)) = open.last_mut() {
                        *plo = (*plo).min(r.start);
                        *phi = (*phi).max(r.end);
                    }
                }
            }
            _ => {}
        }
    }
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, line) in text.split('\n').enumerate() {
        let range = start..start + line.len();
        start = range.end + 1;
        let code = blocks
            .iter()
            .any(|b| range.start >= b.start && range.start < b.end);
        lines.push(source_line(
            line,
            range,
            active.contains(&i),
            code,
            &styles,
            &marks,
        ));
    }
    (lines, links)
}

/// One source line: `line`, at `range` of the text.
fn source_line(
    line: &str,
    range: Range<usize>,
    active: bool,
    code: bool,
    styles: &[(Range<usize>, Style)],
    marks: &[Range<usize>],
) -> Line {
    let mut out = Line {
        text: line.to_string(),
        active,
        code,
        ..Default::default()
    };
    let ws = line.len() - line.trim_start_matches([' ', '\t']).len();
    let cols: usize = line[..ws]
        .chars()
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    if code {
        let fence = ["```", "~~~"]
            .iter()
            .any(|f| line.trim_start().starts_with(f));
        if !line.is_empty() {
            out.spans.push(Span {
                faint: fence,
                code: true,
                ..Span::plain(0..line.len())
            });
        }
        return out;
    }
    out.depth = (cols / 2).min(12) as u8;
    let mut body = ws;
    let mut checked = false;
    if let Some(it) = editor::item(line) {
        body = it.body;
        out.marker = match it.task {
            Some((_, c)) => {
                checked = c;
                LineMarker::Task(c)
            }
            None => LineMarker::Bullet,
        };
        if active || it.number.is_some() {
            let end = match active {
                true => it.body,
                false => it.marker.end,
            };
            out.lead.push(Span {
                faint: it.number.is_none(),
                ..Span::plain(it.indent..end)
            });
        }
    } else if let Some((level, _)) = heading_level(line) {
        out.heading = level;
        let t = &line[ws..];
        let hashes = t.bytes().take_while(|b| *b == b'#').count();
        let after = &t[hashes..];
        let syntax = ws + hashes + (after.len() - after.trim_start().len());
        if active {
            out.spans.push(Span {
                faint: true,
                ..Span::plain(ws..syntax)
            });
        }
        body = syntax;
    } else if line[ws..].starts_with('>') {
        let t = &line[ws..];
        let n = t.len() - t.trim_start_matches(['>', ' ']).len();
        out.spans.push(Span {
            faint: true,
            ..Span::plain(ws..ws + n)
        });
        body = ws + n;
    }
    // The text, cut where styles and syntax start and end.
    let abs = |i: usize| range.start + i;
    let mut cuts: Vec<usize> = vec![body, line.len()];
    for r in styles.iter().map(|(r, _)| r).chain(marks) {
        for p in [r.start, r.end] {
            if p > abs(body) && p < range.end {
                cuts.push(p - range.start);
            }
        }
    }
    cuts.sort_unstable();
    cuts.dedup();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let inside = |r: &Range<usize>| r.start <= abs(a) && abs(b) <= r.end;
        let syntax = marks.iter().any(inside);
        if syntax && !active {
            continue;
        }
        let mut sp = Span {
            faint: syntax,
            dim: checked,
            strike: checked,
            ..Span::plain(a..b)
        };
        for (r, style) in styles.iter().filter(|(r, _)| inside(r)) {
            let _ = r;
            match style {
                Style::Bold => sp.bold = true,
                Style::Italic => sp.italic = true,
                Style::Strike => sp.strike = true,
                Style::Code => sp.code = true,
                Style::Link(n) => sp.link = Some(*n),
            }
        }
        match out.spans.last_mut() {
            Some(last) if last.range.end == a && same(last, &sp) => last.range.end = b,
            _ => out.spans.push(sp),
        }
    }
    out
}

/// Whether two spans look the same.
fn same(a: &Span, b: &Span) -> bool {
    (a.bold, a.italic, a.code, a.strike, a.dim, a.faint, a.link)
        == (b.bold, b.italic, b.code, b.strike, b.dim, b.faint, b.link)
}

/// Remove every occurrence of a local file link (the bar groups them by
/// path). Use source ranges so unrelated Markdown and TOML stay intact.
pub fn remove_file(note: &Note, path: &Path) -> Option<String> {
    let body = note.body();
    let options = Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut items = Vec::new();
    let mut cuts = Vec::new();
    for (event, range) in Parser::new_ext(body, options).into_offset_iter() {
        match event {
            Event::Start(Tag::Item) => items.push(range),
            Event::End(TagEnd::Item) => {
                items.pop();
            }
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. })
                if wl_shell_render::files::uri_path(dest_url.trim()).as_deref() == Some(path) =>
            {
                let item = items.last().and_then(|item| {
                    let prefix = body.get(item.start..range.start)?.trim();
                    let marker = prefix
                        .strip_suffix("[ ]")
                        .or_else(|| prefix.strip_suffix("[x]"))
                        .or_else(|| prefix.strip_suffix("[X]"))
                        .unwrap_or(prefix)
                        .trim_end();
                    let numbered = marker
                        .strip_suffix(['.', ')'])
                        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
                    if !(numbered || matches!(marker, "-" | "+" | "*"))
                        || !body.get(range.end..item.end)?.trim().is_empty()
                    {
                        return None;
                    }
                    let line = body[..item.start].rfind('\n').map_or(0, |i| i + 1);
                    let start = if body[line..item.start].trim().is_empty() {
                        line
                    } else {
                        item.start
                    };
                    let end = body[range.end..item.end]
                        .find('\n')
                        .map_or(item.end, |i| range.end + i + 1);
                    Some(start..end)
                });
                cuts.push(item.unwrap_or(range));
            }
            _ => {}
        }
    }
    if cuts.is_empty() {
        return None;
    }
    cuts.sort_by_key(|r| (r.start, r.end));
    let mut text = String::with_capacity(note.text().len());
    let mut at = 0;
    let offset = note.text().len() - body.len();
    text.push_str(&note.text()[..offset]);
    for r in cuts {
        if r.start >= at {
            text.push_str(&body[at..r.start]);
        }
        at = at.max(r.end);
    }
    text.push_str(&body[at..]);
    Some(text)
}

#[test]
fn removing_a_file_preserves_other_source() {
    let note = Note::parse(
        "+++\r\n# keep\r\ncolor = 'pink'\r\n+++\r\n\r\n- [x] [gone](file:///a%20b)\r\n- Keep [gone][ref] and [other](file:///c)\r\n  - [gone](file:///a%20b)\r\n\r\n[ref]: file:///a%20b\r\n",
    );
    let new = remove_file(&note, Path::new("/a b")).unwrap();
    assert_eq!(
        new,
        "+++\r\n# keep\r\ncolor = 'pink'\r\n+++\r\n\r\n- Keep  and [other](file:///c)\r\n\r\n[ref]: file:///a%20b\r\n"
    );
    assert!(remove_file(&note, Path::new("/missing")).is_none());
}
#[derive(Default)]
struct Builder {
    offset: usize,
    blocks: Vec<Block>,
    tasks: Vec<Range<usize>>,
    links: Vec<String>,
    /// The block whose runs are being collected.
    open: Option<Block>,
    bold: u32,
    italic: u32,
    strike: u32,
    link: Option<u32>,
    /// The open lists: the next item's number (None: bulleted).
    lists: Vec<Option<u64>>,
    quotes: u8,
    /// The marker of the item just started, for its first block.
    marker: Option<Marker>,
    /// A list just started: its first block gets a gap.
    list_start: bool,
    /// The lists' depth of a checked task, while in it.
    checked: Option<usize>,
}

impl Builder {
    fn event(&mut self, event: Event, range: Range<usize>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t, false),
            Event::Code(t) => self.text(&t, true),
            Event::Html(t) | Event::InlineHtml(t) => self.text(t.trim_end_matches('\n'), false),
            Event::InlineMath(t) | Event::DisplayMath(t) => self.text(&t, true),
            Event::FootnoteReference(t) => self.text(&format!("[^{t}]"), false),
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Rule => {
                self.close();
                let mut rule = self.block(Kind::Rule, true);
                rule.marker = Marker::None;
                self.blocks.push(rule);
            }
            Event::TaskListMarker(checked) => {
                self.tasks
                    .push(self.offset + range.start..self.offset + range.end);
                self.marker = Some(Marker::Task(checked));
                if checked {
                    self.checked = Some(self.lists.len());
                }
            }
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => self.open(Kind::Paragraph, true),
            Tag::Heading { level, .. } => self.open(Kind::Heading(level as u8), true),
            Tag::CodeBlock(_) => self.open(Kind::Code, true),
            Tag::HtmlBlock => self.open(Kind::Paragraph, true),
            Tag::BlockQuote(_) => {
                self.close();
                self.quotes += 1;
            }
            Tag::List(start) => {
                self.close();
                self.lists.push(start);
                // Nested lists follow their item closely.
                self.list_start = self.lists.len() == 1;
            }
            Tag::Item => {
                self.close();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        Marker::Number(*n - 1)
                    }
                    _ => Marker::Bullet,
                };
                self.marker = Some(marker);
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                self.links.push(dest_url.to_string());
                self.link = Some(self.links.len() as u32 - 1);
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock | TagEnd::HtmlBlock => {
                self.close()
            }
            TagEnd::BlockQuote(_) => {
                self.close();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::List(_) => {
                self.close();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.close();
                self.marker = None;
                if self.checked == Some(self.lists.len()) {
                    self.checked = None;
                }
            }
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link | TagEnd::Image => self.link = None,
            _ => {}
        }
    }

    /// A new block of `kind`, placed for where the parser is.
    fn block(&mut self, kind: Kind, gap: bool) -> Block {
        let lists = self.lists.len() as u8;
        let (depth, marker) = match self.marker.take() {
            Some(m) => (lists.saturating_sub(1) + self.quotes, m),
            // A list item's later paragraphs line up with its text.
            None => (lists + self.quotes, Marker::None),
        };
        let gap = gap || std::mem::take(&mut self.list_start);
        Block {
            depth,
            quote: self.quotes > 0,
            marker,
            gap,
            ..Block::new(kind, Vec::new())
        }
    }

    fn open(&mut self, kind: Kind, gap: bool) {
        self.close();
        self.open = Some(self.block(kind, gap));
    }

    fn close(&mut self) {
        if let Some(mut b) = self.open.take() {
            if let Some(last) = b.runs.last_mut() {
                let trimmed = last.text.trim_end_matches(['\n', ' ']).len();
                last.text.truncate(trimmed);
            }
            b.runs.retain(|r| !r.text.is_empty());
            self.blocks.push(b);
        }
    }

    fn text(&mut self, text: &str, code: bool) {
        if self.open.is_none() {
            // Text right in a tight list item.
            let b = self.block(Kind::Paragraph, false);
            self.open = Some(b);
        }
        let dim = self.checked.is_some_and(|d| d == self.lists.len());
        let run = Run {
            text: text.to_string(),
            bold: self.bold > 0,
            italic: self.italic > 0,
            code,
            strike: self.strike > 0 || dim,
            dim,
            link: self.link,
        };
        let Some(b) = &mut self.open else { return };
        match b.runs.last_mut() {
            Some(last) if same_style(last, &run) => last.text.push_str(&run.text),
            _ => b.runs.push(run),
        }
    }
}

fn same_style(a: &Run, b: &Run) -> bool {
    (a.bold, a.italic, a.code, a.strike, a.dim, a.link)
        == (b.bold, b.italic, b.code, b.strike, b.dim, b.link)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(b: &Block) -> String {
        b.runs.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn paragraphs_and_headings() {
        let c = parse("# Title\n\nSome *soft*\nwrapped **text**.\n\n---\n", 0);
        assert_eq!(c.blocks.len(), 3);
        assert_eq!(c.blocks[0].kind, Kind::Heading(1));
        assert_eq!(texts(&c.blocks[0]), "Title");
        assert_eq!(texts(&c.blocks[1]), "Some soft wrapped text.");
        let runs = &c.blocks[1].runs;
        assert!(runs.iter().any(|r| r.italic && r.text == "soft"));
        assert!(runs.iter().any(|r| r.bold && r.text == "text"));
        assert_eq!(c.blocks[2].kind, Kind::Rule);
    }

    #[test]
    fn lists() {
        let c = parse("intro\n\n- a\n- b\n  - c\n\n3. x\n4. y\n", 0);
        let summary: Vec<_> = (c.blocks.iter())
            .map(|b| (texts(b), b.depth, b.marker.clone(), b.gap))
            .collect();
        assert_eq!(
            summary,
            [
                ("intro".into(), 0, Marker::None, true),
                ("a".into(), 0, Marker::Bullet, true),
                ("b".into(), 0, Marker::Bullet, false),
                ("c".into(), 1, Marker::Bullet, false),
                ("x".into(), 0, Marker::Number(3), true),
                ("y".into(), 0, Marker::Number(4), false),
            ]
        );
    }

    #[test]
    fn tasks_and_toggling() {
        let front = "+++\ntitle = \"t\"\n+++\n";
        let body = "- [ ] bump\n- [x] tag *it*\n  - [ ] sub\n";
        let text = format!("{front}{body}");
        let c = parse(body, front.len());
        assert_eq!(c.tasks.len(), 3);
        assert_eq!(&text[c.tasks[0].clone()], "[ ]");
        assert_eq!(&text[c.tasks[1].clone()], "[x]");
        assert_eq!(c.blocks[0].marker, Marker::Task(false));
        assert_eq!(c.blocks[1].marker, Marker::Task(true));
        // A checked task is dimmed and struck through, but not its subtasks.
        assert!(c.blocks[1].runs.iter().all(|r| r.dim && r.strike));
        assert_eq!(c.blocks[2].marker, Marker::Task(false));
        assert!(c.blocks[2].runs.iter().all(|r| !r.dim));
        let once = toggle(&text, c.tasks[0].clone()).unwrap();
        assert_eq!(
            once,
            format!("{front}- [x] bump\n- [x] tag *it*\n  - [ ] sub\n")
        );
        let twice = toggle(&once, c.tasks[1].clone()).unwrap();
        assert!(twice.contains("- [ ] tag *it*"));
        // Not a checkbox there (anymore).
        assert_eq!(toggle(&text, 0..3), None);
        assert_eq!(toggle(&text, 1000..1003), None);
    }

    #[test]
    fn links_code_and_quotes() {
        let body = "See [the docs](https://example.com) and `code`.\n\n> quoted\n\n```rust\nfn main() {}\n```\n";
        let c = parse(body, 0);
        assert_eq!(c.links, ["https://example.com"]);
        let runs = &c.blocks[0].runs;
        assert!(
            runs.iter()
                .any(|r| r.link == Some(0) && r.text == "the docs")
        );
        assert!(runs.iter().any(|r| r.code && r.text == "code"));
        assert!(c.blocks[1].quote);
        assert_eq!(c.blocks[1].depth, 1);
        assert_eq!(c.blocks[2].kind, Kind::Code);
        assert_eq!(texts(&c.blocks[2]), "fn main() {}");
    }

    #[test]
    fn titles() {
        // The heading the body opens with is the title, and stays in it.
        let n = Note::parse("+++\ntitle = \"Old\"\n+++\n\n# Heading\n- [ ] a\n");
        let c = Content::of_note(&n, "file");
        assert_eq!(c.title, "Heading");
        assert_eq!(c.blocks.len(), 2);
        assert_eq!(texts(&c.blocks[0]), "Heading");
        assert_eq!(c.blocks[0].kind, Kind::Heading(1));
        assert_eq!(c.blocks[1].marker, Marker::Task(false));
        // A heading further down isn't; the file's name is.
        let c = Content::of_note(&Note::parse("intro\n# Groceries\n"), "2026-11-17-2059");
        assert_eq!(c.title, "2026-11-17-2059");
        assert_eq!(c.blocks.len(), 2);
        // A broken frontmatter says so.
        let c = Content::of_note(&Note::parse("+++\nx =\n+++\n# H\nbody"), "f");
        assert!(texts(&c.blocks[0]).starts_with("⚠ frontmatter"));
        assert_eq!(c.title, "H");
    }

    #[test]
    fn live_preview_keeps_source_on_the_active_line() {
        let source = "# Heading\n- [x] **done**\n[a *link*](https://example.org)";
        let visible = |line: &Line| -> String {
            line.lead
                .iter()
                .chain(&line.spans)
                .map(|s| &line.text[s.range.clone()])
                .collect()
        };
        let (lines, links) = source_lines(source, 1..2);
        assert_eq!(visible(&lines[0]), "Heading");
        assert_eq!(visible(&lines[1]), "- [x] **done**");
        assert_eq!(visible(&lines[2]), "a link");
        assert_eq!(links, ["https://example.org"]);
        let (lines, _) = source_lines(source, 2..3);
        assert_eq!(visible(&lines[1]), "done");
        assert_eq!(lines[1].marker, LineMarker::Task(true));
        assert_eq!(visible(&lines[2]), "[a *link*](https://example.org)");
    }
}
