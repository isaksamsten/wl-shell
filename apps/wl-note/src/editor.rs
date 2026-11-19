//! The open note's text, edited: markdown source with a caret and a
//! selection, list keys and undo. Pure, unit tested; the column lays it
//! out and draws it (`wl_shell_render::edit`), and maps keys and clicks
//! to these.
//!
//! - **Lists.** Enter continues a list item (a checkbox item with
//!   `- [ ] `, a numbered one with the next number), and Enter on an empty
//!   item ends it (outdents it, if nested). Tab and Shift+Tab indent and
//!   outdent the lines touched; Backspace right after an item's marker
//!   removes the marker.
//! - **Undo** keeps the text before each group of edits: typing a word is
//!   one group, as is deleting a run of characters; moving the caret or
//!   another kind of edit starts a new one.
//! - **Reloading** from disk keeps the caret on the same line and column
//!   if it can.

use std::ops::Range;

/// Undo steps kept.
const UNDO: usize = 200;

/// A list item's line, parsed: byte offsets in the line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The indentation's end.
    pub indent: usize,
    /// The marker (`-`, `*`, `+`, or `3.`, `3)`).
    pub marker: Range<usize>,
    /// A numbered item's number.
    pub number: Option<u64>,
    /// A checkbox's `[`, and whether it is checked.
    pub task: Option<(usize, bool)>,
    /// Where its text starts.
    pub body: usize,
}

/// The list item `line` (without its line break) is, if it is one.
pub fn item(line: &str) -> Option<Item> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (len, number) = match rest.as_bytes().first()? {
        b'-' | b'*' | b'+' => (1, None),
        b'0'..=b'9' if (1..=9).contains(&digits) => match rest.as_bytes().get(digits) {
            Some(b'.' | b')') => (digits + 1, rest[..digits].parse().ok()),
            _ => return None,
        },
        _ => return None,
    };
    let after = &rest[len..];
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    if spaces == 0 {
        return None;
    }
    let marker = indent..indent + len;
    let mut body = marker.end + spaces;
    let mut task = None;
    let tail = &line[body..];
    if let Some(mark) = ["[ ]", "[x]", "[X]"]
        .into_iter()
        .find(|m| tail.starts_with(m))
    {
        let after = &tail[3..];
        if after.is_empty() || after.starts_with([' ', '\t']) {
            task = Some((body, mark != "[ ]"));
            let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
            body += 3 + spaces;
        }
    }
    Some(Item {
        indent,
        marker,
        number,
        task,
        body,
    })
}

/// A state to go back to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snap {
    text: String,
    caret: usize,
    anchor: usize,
}

/// What an edit was, for grouping undo steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Insert,
    Delete,
    Other,
}

/// Text being edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Editor {
    text: String,
    /// Byte indices on char boundaries; the selection is between them.
    caret: usize,
    anchor: usize,
    undo: Vec<Snap>,
    redo: Vec<Snap>,
    /// The kind of the last edit, while its group goes on.
    last: Option<Kind>,
    /// Bumped by every change of the text.
    version: u64,
}

impl Editor {
    /// Edit `text` (line breaks made `\n`), the caret at its start.
    pub fn new(text: &str) -> Self {
        Self {
            text: clean(text, true),
            caret: 0,
            anchor: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last: None,
            version: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    /// The selection, if any.
    pub fn selection(&self) -> Option<Range<usize>> {
        (self.caret != self.anchor)
            .then(|| self.caret.min(self.anchor)..self.caret.max(self.anchor))
    }

    pub fn selected(&self) -> Option<&str> {
        self.selection().map(|r| &self.text[r])
    }

    // -- lines -----------------------------------------------------------------

    /// The line `at` is on, and its column.
    pub fn line_col(&self, at: usize) -> (usize, usize) {
        let at = at.min(self.text.len());
        let start = self.line_start(at);
        (self.text[..start].matches('\n').count(), at - start)
    }

    /// The byte of line `line`, column `col` (both clamped).
    pub fn offset(&self, line: usize, col: usize) -> usize {
        let start = match line {
            0 => 0,
            n => match self.text.match_indices('\n').nth(n - 1) {
                Some((i, _)) => i + 1,
                None => return self.text.len(),
            },
        };
        let end = self.line_end(start);
        snap(&self.text, (start + col).min(end))
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |i| at + i)
    }

    /// The lines from the one `range` starts on to the one it ends on,
    /// as (start, end) byte offsets.
    fn lines_of(&self, range: Range<usize>) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut at = self.line_start(range.start);
        loop {
            let end = self.line_end(at);
            out.push((at, end));
            if end >= range.end || end >= self.text.len() {
                break;
            }
            at = end + 1;
        }
        out
    }

    /// The lines with the caret or the selection on them.
    pub fn active_lines(&self) -> Range<usize> {
        let lo = self.caret.min(self.anchor);
        let hi = self.caret.max(self.anchor);
        self.line_col(lo).0..self.line_col(hi).0 + 1
    }

    // -- moving --------------------------------------------------------------

    /// Put the caret at `at`; `extend` keeps the anchor (a selection).
    pub fn set_caret(&mut self, at: usize, extend: bool) {
        self.caret = snap(&self.text, at.min(self.text.len()));
        if !extend {
            self.anchor = self.caret;
        }
        self.last = None;
    }

    pub fn select(&mut self, range: Range<usize>) {
        self.anchor = snap(&self.text, range.start.min(self.text.len()));
        self.caret = snap(&self.text, range.end.min(self.text.len()));
        self.last = None;
    }

    pub fn select_all(&mut self) {
        self.select(0..self.text.len());
    }

    /// Select the word at `at` (or the blanks there).
    pub fn select_word(&mut self, at: usize) {
        let at = snap(&self.text, at.min(self.text.len()));
        let class = |c: char| (c.is_whitespace(), c.is_alphanumeric() || c == '_');
        let Some(c) = self.text[at..]
            .chars()
            .next()
            .or_else(|| self.text[..at].chars().next_back())
        else {
            return;
        };
        let k = class(c);
        let start = (self.text[..at].char_indices().rev())
            .take_while(|(_, c)| class(*c) == k && *c != '\n')
            .last()
            .map_or(at, |(i, _)| i);
        let end = (self.text[at..].char_indices())
            .take_while(|(_, c)| class(*c) == k && *c != '\n')
            .last()
            .map_or(at, |(i, c)| at + i + c.len_utf8());
        self.select(start..end);
    }

    /// Select the line at `at`, its line break included.
    pub fn select_line(&mut self, at: usize) {
        let start = self.line_start(at.min(self.text.len()));
        let end = self.line_end(start);
        self.select(start..(end + 1).min(self.text.len()));
    }

    fn prev(&self, at: usize) -> usize {
        (self.text[..at].char_indices().next_back()).map_or(0, |(i, _)| i)
    }

    fn next(&self, at: usize) -> usize {
        (self.text[at..].chars().next()).map_or(at, |c| at + c.len_utf8())
    }

    /// The start of the word before `at`.
    fn word_back(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end();
        match trimmed.rfind(char::is_whitespace) {
            Some(i) => i + trimmed[i..].chars().next().map_or(1, char::len_utf8),
            None => 0,
        }
    }

    /// The end of the word after `at`.
    fn word_forward(&self, at: usize) -> usize {
        let after = &self.text[at..];
        let skipped = after.len() - after.trim_start().len();
        let rest = &after[skipped..];
        at + skipped + rest.find(char::is_whitespace).unwrap_or(rest.len())
    }

    pub fn left(&mut self, word: bool, extend: bool) {
        let at = match (self.selection(), word, extend) {
            (Some(r), false, false) => r.start,
            (_, true, _) => self.word_back(self.caret),
            _ => self.prev(self.caret),
        };
        self.set_caret(at, extend);
    }

    pub fn right(&mut self, word: bool, extend: bool) {
        let at = match (self.selection(), word, extend) {
            (Some(r), false, false) => r.end,
            (_, true, _) => self.word_forward(self.caret),
            _ => self.next(self.caret),
        };
        self.set_caret(at, extend);
    }

    /// To the line's start: its text's first (a list item's, after the
    /// marker), then the very start.
    pub fn home(&mut self, extend: bool) {
        let start = self.line_start(self.caret);
        let line = &self.text[start..self.line_end(start)];
        let text = match item(line) {
            Some(it) => it.body,
            None => line.len() - line.trim_start().len(),
        };
        let at = match self.caret == start + text {
            true => start,
            false => start + text,
        };
        self.set_caret(at, extend);
    }

    pub fn end(&mut self, extend: bool) {
        let end = self.line_end(self.caret);
        self.set_caret(end, extend);
    }

    pub fn doc_start(&mut self, extend: bool) {
        self.set_caret(0, extend);
    }

    pub fn doc_end(&mut self, extend: bool) {
        self.set_caret(self.text.len(), extend);
    }

    // -- editing -------------------------------------------------------------

    /// Before an edit of `kind`: keep the state to undo to, unless it
    /// goes on the last edit's group.
    fn begin(&mut self, kind: Kind) {
        if kind == Kind::Other || self.last != Some(kind) {
            self.push_undo();
        }
        self.last = Some(kind);
        self.redo.clear();
    }

    fn push_undo(&mut self) {
        self.undo.push(Snap {
            text: self.text.clone(),
            caret: self.caret,
            anchor: self.anchor,
        });
        if self.undo.len() > UNDO {
            self.undo.remove(0);
        }
    }

    /// Replace `range` with `s`; the caret goes after it.
    fn replace(&mut self, range: Range<usize>, s: &str) {
        self.text.replace_range(range.clone(), s);
        self.caret = range.start + s.len();
        self.anchor = self.caret;
        self.version += 1;
    }

    /// Type `s` over the selection (line breaks made `\n`, other control
    /// characters but tabs left out).
    pub fn insert(&mut self, s: &str) {
        let s = clean(s, true);
        if s.is_empty() && self.selection().is_none() {
            return;
        }
        // A new word starts a new undo step.
        let before = self.text[..self.caret].chars().next_back();
        let breaks =
            s.starts_with(char::is_whitespace) && before.is_some_and(|c| !c.is_whitespace());
        if breaks || self.selection().is_some() || s.contains('\n') {
            self.last = None;
        }
        self.begin(Kind::Insert);
        let range = self.selection().unwrap_or(self.caret..self.caret);
        self.replace(range, &s);
        if s.contains('\n') {
            self.last = None;
        }
    }

    /// Paste `s`: one undo step.
    pub fn paste(&mut self, s: &str) {
        self.last = None;
        self.begin(Kind::Other);
        let range = self.selection().unwrap_or(self.caret..self.caret);
        self.replace(range, &clean(s, true));
        self.last = None;
    }

    /// Enter: a new line, continuing a list item (see the module's
    /// comment).
    pub fn enter(&mut self) {
        if let Some(r) = self.selection() {
            self.begin(Kind::Other);
            self.replace(r, "");
        }
        let start = self.line_start(self.caret);
        let end = self.line_end(start);
        let line = self.text[start..end].to_string();
        let col = self.caret - start;
        let cont = match item(&line) {
            Some(it) if col >= it.body => {
                if line[it.body..].trim().is_empty() && col >= line.trim_end().len() {
                    // An empty item: it ends (or comes out a level).
                    self.last = None;
                    self.begin(Kind::Other);
                    if it.indent > 0 {
                        let (s, e) = (start, start + it.indent);
                        let keep = outdented(&line[..it.indent], unit(&it));
                        self.text.replace_range(s..e, &keep);
                        self.caret = start + line.len() - it.indent + keep.len();
                    } else {
                        self.text.replace_range(start..end, "");
                        self.caret = start;
                    }
                    self.anchor = self.caret;
                    self.version += 1;
                    self.last = None;
                    return;
                }
                let marker = match it.number {
                    Some(n) => {
                        let delim = &line[it.marker.end - 1..it.marker.end];
                        format!("{}{delim}", n + 1)
                    }
                    None => line[it.marker.clone()].to_string(),
                };
                let task = if it.task.is_some() { "[ ] " } else { "" };
                format!("\n{}{marker} {task}", &line[..it.indent])
            }
            _ => {
                let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                format!("\n{}", &line[..indent.min(col)])
            }
        };
        self.last = None;
        self.begin(Kind::Other);
        let at = self.caret;
        self.replace(at..at, &cont);
        self.last = None;
    }

    /// Remove the selection, or the char (or word) before the caret, or
    /// an item's marker right before it.
    pub fn backspace(&mut self, word: bool) {
        if let Some(r) = self.selection() {
            self.last = None;
            self.begin(Kind::Other);
            self.replace(r, "");
            self.last = None;
            return;
        }
        if self.caret == 0 {
            return;
        }
        let start = self.line_start(self.caret);
        let line = &self.text[start..self.line_end(start)];
        if let Some(it) = item(line)
            && self.caret == start + it.body
        {
            self.last = None;
            self.begin(Kind::Other);
            self.replace(start + it.indent..start + it.body, "");
            self.last = None;
            return;
        }
        let from = match word {
            true => self.word_back(self.caret),
            false => self.prev(self.caret),
        };
        self.begin(Kind::Delete);
        self.replace(from..self.caret, "");
    }

    /// Remove the selection, or the char (or word) after the caret.
    pub fn delete(&mut self, word: bool) {
        if let Some(r) = self.selection() {
            self.last = None;
            self.begin(Kind::Other);
            self.replace(r, "");
            self.last = None;
            return;
        }
        let to = match word {
            true => self.word_forward(self.caret),
            false => self.next(self.caret),
        };
        if to == self.caret {
            return;
        }
        self.begin(Kind::Delete);
        let at = self.caret;
        self.replace(at..to, "");
    }

    /// Indent (or with `out`, outdent) the lines touched by the caret or
    /// the selection, by their item's marker's width (two spaces else).
    pub fn indent(&mut self, out: bool) {
        let lo = self.caret.min(self.anchor);
        let hi = self.caret.max(self.anchor);
        let lines = self.lines_of(lo..hi);
        let mut text = self.text.clone();
        let (mut caret, mut anchor) = (self.caret, self.anchor);
        let mut changed = false;
        // From the last, so the offsets before stay.
        for (start, end) in lines.into_iter().rev() {
            let line = &self.text[start..end];
            if out && line.is_empty() {
                continue;
            }
            let u = item(line).map_or(2, |it| unit(&it));
            let ws = line.len() - line.trim_start_matches([' ', '\t']).len();
            let (cut, add) = match out {
                false => (0, " ".repeat(u)),
                true => {
                    let kept = outdented(&line[..ws], u);
                    (ws - kept.len(), String::new())
                }
            };
            if cut == 0 && add.is_empty() {
                continue;
            }
            changed = true;
            text.replace_range(start..start + cut, &add);
            let shift = |at: usize| match at {
                a if a <= start => a,
                a if a < start + cut => start,
                a => a - cut + add.len(),
            };
            (caret, anchor) = (shift(caret), shift(anchor));
        }
        if !changed {
            return;
        }
        self.last = None;
        self.begin(Kind::Other);
        self.text = text;
        (self.caret, self.anchor) = (caret, anchor);
        self.version += 1;
        self.last = None;
    }

    /// Check or uncheck the checkbox on line `line`. Returns whether there
    /// was one.
    pub fn toggle_task(&mut self, line: usize) -> bool {
        let start = self.offset(line, 0);
        let end = self.line_end(start);
        let Some((at, checked)) = item(&self.text[start..end]).and_then(|it| it.task) else {
            return false;
        };
        let (caret, anchor) = (self.caret, self.anchor);
        self.last = None;
        self.begin(Kind::Other);
        let mark = if checked { " " } else { "x" };
        self.text
            .replace_range(start + at + 1..start + at + 2, mark);
        (self.caret, self.anchor) = (caret, anchor);
        self.version += 1;
        self.last = None;
        true
    }

    /// Cut the selection: its text.
    pub fn cut(&mut self) -> Option<String> {
        let r = self.selection()?;
        let text = self.text[r.clone()].to_string();
        self.last = None;
        self.begin(Kind::Other);
        self.replace(r, "");
        self.last = None;
        Some(text)
    }

    /// The text changed on disk: take it, keeping the caret's line and
    /// column if it can, with no undo history.
    pub fn reload(&mut self, text: &str) {
        let (line, col) = self.line_col(self.caret);
        self.text = clean(text, true);
        self.undo.clear();
        self.redo.clear();
        self.version += 1;
        let at = self.offset(line, col);
        self.set_caret(at, false);
    }

    pub fn undo(&mut self) -> bool {
        let Some(snap) = self.undo.pop() else {
            return false;
        };
        self.redo.push(Snap {
            text: std::mem::replace(&mut self.text, snap.text),
            caret: self.caret,
            anchor: self.anchor,
        });
        (self.caret, self.anchor) = (snap.caret, snap.anchor);
        self.version += 1;
        self.last = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(snap) = self.redo.pop() else {
            return false;
        };
        self.undo.push(Snap {
            text: std::mem::replace(&mut self.text, snap.text),
            caret: self.caret,
            anchor: self.anchor,
        });
        (self.caret, self.anchor) = (snap.caret, snap.anchor);
        self.version += 1;
        self.last = None;
        true
    }
}

/// How far an item's line indents and outdents: its marker's width and a
/// space, so a nested item lines up with its parent's text.
fn unit(it: &Item) -> usize {
    it.marker.len() + 1
}

/// Indentation `ws` with `unit` columns (or a tab) taken off its end.
fn outdented(ws: &str, unit: usize) -> String {
    if let Some(rest) = ws.strip_suffix('\t') {
        return rest.to_string();
    }
    let spaces = ws.len() - ws.trim_end_matches(' ').len();
    ws[..ws.len() - spaces.min(unit)].to_string()
}

/// `s` with line breaks made `\n`, and control characters but tabs (and
/// line breaks, if `lines`) left out.
fn clean(s: &str, lines: bool) -> String {
    s.replace("\r\n", "\n")
        .replace('\r', "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\t' || (lines && *c == '\n'))
        .collect()
}

/// `at`, or the char boundary before it.
fn snap(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An editor with `text`, the caret where `|` is.
    fn ed(text: &str) -> Editor {
        let at = text.find('|').unwrap();
        let mut e = Editor::new(&text.replace('|', ""));
        e.set_caret(at, false);
        e
    }

    /// The text with `|` at the caret.
    fn show(e: &Editor) -> String {
        let mut t = e.text().to_string();
        t.insert(e.caret(), '|');
        t
    }

    #[test]
    fn items() {
        let it = item("  - [x] done").unwrap();
        assert_eq!(
            (it.indent, it.marker, it.task, it.body),
            (2, 2..3, Some((4, true)), 8)
        );
        let it = item("12. twelve").unwrap();
        assert_eq!((it.number, it.body), (Some(12), 4));
        assert_eq!(item("- ").unwrap().body, 2);
        assert_eq!(item("- [ ]").unwrap().task, Some((2, false)));
        // Not tasks, not items.
        assert_eq!(item("- [ ]x").unwrap().task, None);
        assert!(item("-x").is_none());
        assert!(item("**bold**").is_none());
        assert!(item("2024 was").is_none());
        assert!(item("").is_none());
    }

    #[test]
    fn list_continuation_and_ending() {
        let mut e = ed("- [ ] milk|");
        e.enter();
        assert_eq!(show(&e), "- [ ] milk\n- [ ] |");
        e.insert("eggs");
        e.enter();
        e.enter();
        // An empty item ends the list.
        assert_eq!(show(&e), "- [ ] milk\n- [ ] eggs\n|");
        let mut e = ed("1. one|");
        e.enter();
        assert_eq!(show(&e), "1. one\n2. |");
        let mut e = ed("  * nested|");
        e.enter();
        e.enter();
        // A nested empty item comes out a level.
        assert_eq!(show(&e), "  * nested\n* |");
        // Splitting an item's text.
        let mut e = ed("- ab|cd");
        e.enter();
        assert_eq!(show(&e), "- ab\n- |cd");
        // Plain lines keep their indentation.
        let mut e = ed("  text|");
        e.enter();
        assert_eq!(show(&e), "  text\n  |");
        // Backspace after a marker removes it.
        let mut e = ed("- [ ] |x");
        e.backspace(false);
        assert_eq!(show(&e), "|x");
    }

    #[test]
    fn indenting() {
        let mut e = ed("- a\n- b|");
        e.indent(false);
        assert_eq!(show(&e), "- a\n  - b|");
        e.indent(true);
        assert_eq!(show(&e), "- a\n- b|");
        e.indent(true);
        assert_eq!(show(&e), "- a\n- b|");
        let mut e = ed("1. a\n|1. b\n1. c");
        e.select(e.offset(1, 0)..e.offset(2, 2));
        e.indent(false);
        assert_eq!(e.text(), "1. a\n   1. b\n   1. c");
        assert_eq!(e.selected(), Some("   1. b\n   1."));
        // Tabs come out as tabs.
        let mut e = ed("\t- a|");
        e.indent(true);
        assert_eq!(show(&e), "- a|");
    }

    #[test]
    fn checkboxes_as_edits() {
        let mut e = ed("- [ ] a\n- [x] b|");
        assert!(e.toggle_task(0));
        assert!(e.toggle_task(1));
        assert_eq!(show(&e), "- [x] a\n- [ ] b|");
        assert!(!Editor::new("text").clone().toggle_task(0));
        assert!(e.undo());
        assert_eq!(e.text(), "- [x] a\n- [x] b");
    }

    #[test]
    fn undo_grouping() {
        let mut e = ed("|");
        for c in "one two".chars() {
            e.insert(&c.to_string());
        }
        e.backspace(false);
        e.backspace(false);
        assert_eq!(e.text(), "one t");
        assert!(e.undo());
        assert_eq!(e.text(), "one two");
        assert!(e.undo());
        assert_eq!(e.text(), "one");
        assert!(e.undo());
        assert_eq!(e.text(), "");
        assert!(!e.undo());
        assert!(e.redo());
        assert!(e.redo());
        assert_eq!(show(&e), "one two|");
        // Moving starts a new group.
        e.insert("x");
        e.left(false, false);
        e.insert("y");
        assert!(e.undo());
        assert_eq!(e.text(), "one twox");
        // An edit drops what could be redone.
        e.insert("z");
        assert!(!e.redo());
    }

    #[test]
    fn moving_and_selecting() {
        let mut e = ed("- item|\nnext line");
        e.home(false);
        assert_eq!(e.caret(), 2);
        e.home(false);
        assert_eq!(e.caret(), 0);
        e.end(true);
        assert_eq!(e.selected(), Some("- item"));
        e.right(false, false);
        assert_eq!(e.caret(), 6);
        e.right(true, false);
        assert_eq!(show(&e), "- item\nnext| line");
        e.select_word(e.caret() + 2);
        assert_eq!(e.selected(), Some("line"));
        e.select_line(0);
        assert_eq!(e.selected(), Some("- item\n"));
        assert_eq!(e.active_lines(), 0..2);
        assert_eq!(e.cut().as_deref(), Some("- item\n"));
        assert_eq!(e.text(), "next line");
        e.doc_end(false);
        e.insert("\r\nend\u{7}");
        assert_eq!(e.text(), "next line\nend");
        assert_eq!(e.line_col(e.caret()), (1, 3));
        assert_eq!(e.offset(1, 99), e.text().len());
        assert_eq!(e.offset(9, 0), e.text().len());
    }

    #[test]
    fn reloading_keeps_the_caret() {
        let mut e = ed("one\ntwo\nth|ree");
        e.insert("x");
        e.reload("ONE\nTWO\nTHREE\nFOUR");
        assert_eq!(e.line_col(e.caret()), (2, 3));
        assert!(!e.undo());
        e.reload("short");
        assert_eq!(e.caret(), 5);
    }

    #[test]
    fn multibyte() {
        let mut e = ed("Grüße|");
        e.backspace(false);
        e.left(false, false);
        e.left(false, false);
        e.delete(false);
        assert_eq!(e.text(), "Grß");
        e.set_caret(3, false);
        assert_eq!(e.caret(), 2);
    }
}
