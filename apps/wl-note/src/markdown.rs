//! A note's markdown as a document to render (`wl_shell_render::doc`),
//! with where its checkboxes are in the file and what its links point at.
//! Pure, unit tested.
//!
//! Parsed with pulldown-cmark (CommonMark, with task lists and
//! strikethrough). Headings, paragraphs, lists (bulleted, numbered and
//! task lists, nested), block quotes, code blocks and rules become blocks;
//! emphasis, strong, strikethrough, inline code and links become runs. A
//! checked task's text is dimmed and struck through. HTML shows as is.
//!
//! Checkboxes are toggled in the file itself (`toggle`), by the byte range
//! of their `[ ]` or `[x]`, so nothing else in it changes.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use wl_shell_notes::Note;
use wl_shell_render::doc::{Block, Kind, Marker, Run};

/// A note, ready to render.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Content {
    /// The title: the frontmatter's, else the first heading's, else the
    /// file's name.
    pub title: String,
    /// The body (after the frontmatter's error, if it is broken).
    pub blocks: Vec<Block>,
    /// The block of `blocks` that is the title, if it came from there.
    pub heading: Option<usize>,
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
        let own = (note.meta.title.as_deref()).is_some_and(|t| !t.trim().is_empty());
        if !own && note.title().is_some() {
            content.heading =
                (content.blocks.iter()).position(|b| matches!(b.kind, Kind::Heading(_)));
        }
        if let Some(e) = &note.error {
            let run = Run {
                dim: true,
                italic: true,
                ..Run::plain(format!("⚠ {e}"))
            };
            content
                .blocks
                .insert(0, Block::new(Kind::Paragraph, vec![run]));
            content.heading = content.heading.map(|h| h + 1);
        }
        content
    }

    /// The body without the heading that is the title.
    pub fn body(&self) -> Vec<Block> {
        (self.blocks.iter().enumerate())
            .filter(|(i, _)| Some(*i) != self.heading)
            .map(|(_, b)| b.clone())
            .collect()
    }

    /// What a sheet shows: the title as a heading, then the body.
    pub fn sheet(&self) -> Vec<Block> {
        let title = Block::new(Kind::Heading(2), vec![Run::plain(self.title.clone())]);
        std::iter::once(title).chain(self.body()).collect()
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
        let n = Note::parse("+++\ntitle = \"Release\"\n+++\n# Heading\n- [ ] a\n");
        let c = Content::of_note(&n, "file");
        assert_eq!((c.title.as_str(), c.heading), ("Release", None));
        assert_eq!(c.body().len(), 2);
        let sheet = c.sheet();
        assert_eq!(texts(&sheet[0]), "Release");
        assert_eq!(sheet[0].kind, Kind::Heading(2));
        assert_eq!(texts(&sheet[1]), "Heading");
        assert_eq!(sheet[2].marker, Marker::Task(false));
        // The first heading is the title: not shown twice.
        let n = Note::parse("intro\n# Groceries\nmilk\n");
        let c = Content::of_note(&n, "file");
        assert_eq!((c.title.as_str(), c.heading), ("Groceries", Some(1)));
        let body: Vec<String> = c.body().iter().map(texts).collect();
        assert_eq!(body, ["intro", "milk"]);
        assert_eq!(c.sheet().len(), 3);
        // Else the file's name.
        let c = Content::of_note(&Note::parse("milk"), "2026-11-17-2059");
        assert_eq!(c.title, "2026-11-17-2059");
        assert_eq!(texts(&c.sheet()[0]), "2026-11-17-2059");
        // A broken frontmatter says so.
        let c = Content::of_note(&Note::parse("+++\nx =\n+++\n# H\nbody"), "f");
        assert!(texts(&c.blocks[0]).starts_with("⚠ frontmatter"));
        assert_eq!((c.title.as_str(), c.heading), ("H", Some(1)));
    }
}
