//! The drawer: a sheet holding a list of notes rather than a note, such as
//! the hidden ones, those with a due time, or the templates. Pure, unit
//! tested.
//!
//! The program hands over every note the drawer could list (`Entry`),
//! each saying whether it belongs to the context (`here`). The drawer
//! decides what shows: the **scope** (`here` or `all`; not for the
//! templates, which are all there is), the **filter** (typed; titles and
//! bodies, fuzzy, best match first, with `nucleo-matcher`), the
//! **selection** (arrows, Enter) and the **scroll** (a window of
//! `capacity` rows onto the list). Without a filter the rows stay in the
//! order they were handed over (for the hidden notes: the one hidden last
//! first; for the due ones: `sort_due`).
//!
//! `layout` says where the parts are on the drawer's body, which the
//! column draws (drawerview.rs) and finds the pointer on.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use wl_shell_notes::Note;
use wl_shell_ui::Rect;

/// The drawer's key in the deck. Paths can't hold a NUL, so no note is
/// called this.
pub const KEY: &str = "\0drawer";

/// How much of a note's text is searched.
const SEARCHED: usize = 4000;
/// How much of a note's first line is kept.
const EXCERPT: usize = 160;

/// What a drawer lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Hidden,
    /// The notes with a `due`, hidden or not.
    Due,
    /// The files of the `.templates` directory.
    Templates,
}

/// What the button on a hovered row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowAction {
    /// Move the note to the Trash (two clicks).
    Trash,
    /// Clear the note's `due`.
    Clear,
}

impl Kind {
    /// What the margin says (before it is uppercased).
    pub fn label(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Due => "Due",
            Self::Templates => "Templates",
        }
    }

    /// What an empty drawer says, `elsewhere` if some are in the other
    /// scope.
    pub fn empty(self, elsewhere: bool) -> &'static str {
        match (self, elsewhere) {
            (Self::Hidden, false) => "No hidden notes",
            (Self::Hidden, true) => "No hidden notes here (Tab: all)",
            (Self::Due, false) => "No notes with a due time",
            (Self::Due, true) => "No due notes here (Tab: all)",
            (Self::Templates, _) => "No templates in .templates",
        }
    }

    /// What the button on a hovered row does, if there is one.
    pub fn row_action(self) -> Option<RowAction> {
        match self {
            Self::Hidden => Some(RowAction::Trash),
            Self::Due => Some(RowAction::Clear),
            Self::Templates => None,
        }
    }

    /// Whether the notes are for `here` or `all`: a template isn't.
    pub fn scoped(self) -> bool {
        self != Self::Templates
    }
}

/// Which notes the drawer lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Those that would show in the context.
    Here,
    All,
}

impl Scope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Here => "here",
            Self::All => "all",
        }
    }

    pub fn toggled(self) -> Self {
        match self {
            Self::Here => Self::All,
            Self::All => Self::Here,
        }
    }
}

/// A note the drawer can list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The note's key: its path in the notes directory.
    pub key: String,
    pub title: String,
    /// Where it shows: `ALL_APPS`, or its apps.
    pub app: String,
    /// The first line of its text, without markdown's marks.
    pub line: String,
    /// The paper's color, 0xRRGGBBAA.
    pub color: u32,
    /// It belongs to the context.
    pub here: bool,
    pub overdue: bool,
    /// When it is due, in Unix seconds, and as it is said ("Thu 20 Nov
    /// 09:00").
    pub due_at: Option<i64>,
    pub due: String,
    /// Shown dimmed: a note that is hidden, in a drawer of those due.
    pub dim: bool,
    /// What the filter looks in: title, app and text.
    search: String,
}

/// Where a note shown for every app says it shows.
pub const ALL_APPS: &str = "all apps";

impl Entry {
    /// The entry of `note`, at `key`; `name` is its file's name, which
    /// the title falls back to.
    pub fn of(key: String, name: &str, note: &Note, color: u32, here: bool, overdue: bool) -> Self {
        Self::build(key, name, note, color, here, overdue)
    }

    /// The entry of a template: it says where its notes show (all apps
    /// if it names nothing, its placeholders as they are).
    pub fn template(key: String, name: &str, note: &Note, color: u32) -> Self {
        Self::build(key, name, note, color, true, false)
    }

    fn build(key: String, name: &str, note: &Note, color: u32, here: bool, overdue: bool) -> Self {
        let title = note.title_or(name).trim().to_owned();
        let app = if note.global() {
            ALL_APPS.to_owned()
        } else {
            crate::list::shows(note)
        };
        let body: String = note.body().chars().take(SEARCHED).collect();
        let line = first_line(&body, &title);
        // The CLI and config call them global: that finds them too.
        let also = if note.global() { " global" } else { "" };
        let search = format!("{title}\n{app}{also}\n{body}");
        Self {
            key,
            title,
            app,
            line,
            color,
            here,
            overdue,
            due_at: None,
            due: String::new(),
            dim: false,
            search,
        }
    }

    /// Due at `at` (Unix seconds), said `when`.
    pub fn due(mut self, at: i64, when: String) -> Self {
        self.due_at = Some(at);
        self.due = when;
        self
    }

    pub fn dimmed(mut self, dim: bool) -> Self {
        self.dim = dim;
        self
    }

    /// The second line of its row: when it is due, where it shows, and
    /// its first line, whichever there are.
    pub fn subtitle(&self) -> String {
        let parts = [self.due.as_str(), self.app.as_str(), self.line.as_str()];
        let parts: Vec<&str> = parts.into_iter().filter(|p| !p.is_empty()).collect();
        parts.join(" · ")
    }
}

/// Put the entries with a due time in the order of the Due drawer: those
/// overdue first, then the soonest; those due at the same time stay as
/// they were.
pub fn sort_due(entries: &mut [Entry]) {
    entries.sort_by_key(|e| (!e.overdue, e.due_at.unwrap_or(i64::MAX)));
}

/// The first line of `body` worth showing: not blank, and not the heading
/// that is `title`, without the marks of headings, lists, tasks and
/// quotes.
pub fn first_line(body: &str, title: &str) -> String {
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let text = strip_marks(line);
        if text.is_empty() || (line.starts_with('#') && text == title.trim()) {
            continue;
        }
        return text.chars().take(EXCERPT).collect();
    }
    String::new()
}

fn strip_marks(line: &str) -> &str {
    const MARKS: [&str; 10] = [
        "- [ ] ", "- [x] ", "- [X] ", "* [ ] ", "* [x] ", "* [X] ", "- ", "* ", "+ ", "> ",
    ];
    let mut s = line.trim_start_matches('#').trim_start();
    'marks: loop {
        for mark in MARKS {
            if let Some(rest) = s.strip_prefix(mark) {
                s = rest.trim_start();
                continue 'marks;
            }
        }
        // "1. " and "12) ".
        let digits = s.chars().take_while(char::is_ascii_digit).count();
        if digits > 0
            && let Some(rest) = s[digits..]
                .strip_prefix(". ")
                .or_else(|| s[digits..].strip_prefix(") "))
        {
            s = rest.trim_start();
            continue;
        }
        return s.trim();
    }
}

/// The entries in `scope` that `filter` matches, as indices into
/// `entries`: in their order without a filter, else the best match
/// first (a title match counts for more).
fn rank(entries: &[Entry], scope: Scope, filter: &str) -> Vec<usize> {
    let wanted = |e: &Entry| scope == Scope::All || e.here;
    if filter.trim().is_empty() {
        return (0..entries.len())
            .filter(|&i| wanted(&entries[i]))
            .collect();
    }
    let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize)> = Vec::new();
    for (i, e) in entries.iter().enumerate().filter(|(_, e)| wanted(e)) {
        let haystack = Utf32Str::new(&e.search, &mut buf);
        let Some(all) = pattern.score(haystack, &mut matcher) else {
            continue;
        };
        let haystack = Utf32Str::new(&e.title, &mut buf);
        let title = pattern.score(haystack, &mut matcher).unwrap_or(0);
        scored.push((all + 2 * title, i));
    }
    // Stable: ties stay in order.
    scored.sort_by_key(|&(score, _)| std::cmp::Reverse(score));
    scored.into_iter().map(|(_, i)| i).collect()
}

#[derive(Clone, Debug)]
pub struct Drawer {
    kind: Kind,
    scope: Scope,
    entries: Vec<Entry>,
    filter: String,
    /// What shows, as indices into `entries`.
    visible: Vec<usize>,
    /// The selected row, an index into `visible`.
    selected: usize,
    /// The first row in view.
    top: usize,
    /// How many rows fit.
    capacity: usize,
}

impl Drawer {
    /// A drawer of `entries`, on those that belong to the context, or on
    /// all of them if none do.
    pub fn new(kind: Kind, entries: Vec<Entry>) -> Self {
        let scope = match entries.iter().any(|e| e.here) {
            true => Scope::Here,
            false => Scope::All,
        };
        let mut d = Self {
            kind,
            scope,
            entries,
            filter: String::new(),
            visible: Vec::new(),
            selected: 0,
            top: 0,
            capacity: 5,
        };
        d.refresh(None);
        d
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// How many rows show.
    pub fn len(&self) -> usize {
        self.visible.len()
    }

    pub fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// Whether the other scope has rows the filter lets through, so an
    /// empty drawer can say where to look.
    pub fn elsewhere(&self) -> bool {
        self.kind.scoped()
            && self.scope == Scope::Here
            && !rank(&self.entries, Scope::All, &self.filter).is_empty()
    }

    /// Show `entries` instead: the selection stays on its note if it is
    /// still there, else at its place in the list.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        if entries == self.entries {
            return;
        }
        let keep = self.selected().map(|e| e.key.clone());
        self.entries = entries;
        self.refresh(keep);
    }

    /// Filter by `filter`; the best match is selected. Returns whether it
    /// changed.
    pub fn set_filter(&mut self, filter: &str) -> bool {
        if filter == self.filter {
            return false;
        }
        self.filter = filter.to_owned();
        self.selected = 0;
        self.top = 0;
        self.refresh(None);
        true
    }

    pub fn toggle_scope(&mut self) {
        if !self.kind.scoped() {
            return;
        }
        self.scope = self.scope.toggled();
        self.selected = 0;
        self.top = 0;
        self.refresh(None);
    }

    fn refresh(&mut self, keep: Option<String>) {
        self.visible = rank(&self.entries, self.scope, &self.filter);
        if let Some(key) = keep
            && let Some(at) = (self.visible.iter()).position(|&i| self.entries[i].key == key)
        {
            self.selected = at;
        }
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.fit();
    }

    /// Say how many rows fit. Returns whether that changed.
    pub fn set_capacity(&mut self, capacity: usize) -> bool {
        let capacity = capacity.max(1);
        let changed = capacity != self.capacity;
        self.capacity = capacity;
        self.fit();
        changed
    }

    /// Keep the window within the list, and the selection in it.
    fn fit(&mut self) {
        let max_top = self.visible.len().saturating_sub(self.capacity);
        self.top = self.top.min(max_top);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + self.capacity {
            self.top = self.selected + 1 - self.capacity;
        }
    }

    /// The rows in view, as positions in the list.
    pub fn window(&self) -> std::ops::Range<usize> {
        self.top..(self.top + self.capacity).min(self.visible.len())
    }

    /// The `n`-th row in view.
    pub fn row(&self, n: usize) -> Option<&Entry> {
        let at = self.top + n;
        (n < self.capacity)
            .then(|| self.visible.get(at))
            .flatten()
            .map(|&i| &self.entries[i])
    }

    /// Whether the `n`-th row in view is the selected one.
    pub fn is_selected(&self, n: usize) -> bool {
        self.top + n == self.selected && !self.visible.is_empty()
    }

    pub fn selected(&self) -> Option<&Entry> {
        self.visible.get(self.selected).map(|&i| &self.entries[i])
    }

    /// Select the `n`-th row in view.
    pub fn select(&mut self, n: usize) {
        if self.row(n).is_some() {
            self.selected = self.top + n;
        }
    }

    /// Move the selection by `rows`, within the list.
    pub fn step(&mut self, rows: isize) {
        let last = self.visible.len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(rows).min(last);
        self.fit();
    }

    /// Select the first or the last row.
    pub fn jump(&mut self, last: bool) {
        self.selected = if last {
            self.visible.len().saturating_sub(1)
        } else {
            0
        };
        self.fit();
    }

    /// Scroll the window by `rows`, leaving the selection where it is.
    pub fn scroll(&mut self, rows: isize) {
        let max_top = self.visible.len().saturating_sub(self.capacity);
        self.top = self.top.saturating_add_signed(rows).min(max_top);
    }

    /// How many rows are below the window.
    pub fn below(&self) -> usize {
        self.visible.len() - self.window().end
    }

    /// How many rows are above the window.
    pub fn above(&self) -> usize {
        self.top
    }

    /// How many rows a page is.
    pub fn page(&self) -> isize {
        self.capacity.saturating_sub(1).max(1) as isize
    }
}

/// The filter field's height.
pub const FIELD_H: f64 = 24.0;
/// The scope label's width.
pub const SCOPE_W: f64 = 48.0;
/// Between the field and the rows.
const GAP: f64 = 6.0;
/// Between the filter field's edge and its text.
pub const INSET: f64 = 6.0;

/// How wide the filter's text is, on a body `w` wide.
pub fn field_width(w: f64) -> f64 {
    (w - SCOPE_W - GAP - 2.0 * INSET).max(1.0)
}

/// Where the drawer's parts are, from its body's top left corner.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub filter: Rect,
    pub scope: Rect,
    /// The rows that fit, from the top.
    pub rows: Vec<Rect>,
    /// Where "n more" goes, below the last row.
    pub more: Rect,
}

impl Layout {
    pub fn capacity(&self) -> usize {
        self.rows.len()
    }
}

/// Lay a body `w` by `h` out, rows `row_h` tall and a footer `foot_h`
/// tall for `total` rows: the footer's room is only reserved if they
/// don't all fit without it.
pub fn layout(w: f64, h: f64, row_h: f64, foot_h: f64, total: usize) -> Layout {
    let head = FIELD_H + GAP;
    let room = (h - head).max(0.0);
    let row_h = row_h.max(1.0);
    let all = (room / row_h).floor() as usize;
    let capacity = match total <= all {
        true => all,
        false => ((room - foot_h) / row_h).floor().max(0.0) as usize,
    }
    .max(1);
    Layout {
        filter: Rect::new(0.0, 0.0, (w - SCOPE_W - GAP).max(0.0), FIELD_H),
        scope: Rect::new((w - SCOPE_W).max(0.0), 0.0, SCOPE_W.min(w), FIELD_H),
        rows: (0..capacity)
            .map(|i| Rect::new(0.0, head + i as f64 * row_h, w, row_h))
            .collect(),
        more: Rect::new(0.0, (h - foot_h).max(0.0), w, foot_h),
    }
}

/// Where a row's trash icon is, `icon` big, at the row's end.
pub fn trash_rect(row: Rect, icon: f64) -> Rect {
    Rect::new(
        row.right() - icon - 4.0,
        row.y + (row.h - icon) / 2.0,
        icon,
        icon,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, title: &str, here: bool) -> Entry {
        Entry::of(
            key.into(),
            key,
            &Note::parse(format!("# {title}\n\nbody of {title}\n")),
            0xfff2a8ff,
            here,
            false,
        )
    }

    fn with_body(key: &str, title: &str, body: &str, here: bool) -> Entry {
        Entry::of(
            key.into(),
            key,
            &Note::parse(format!("# {title}\n\n{body}\n")),
            0xfff2a8ff,
            here,
            false,
        )
    }

    fn keys(d: &Drawer) -> Vec<&str> {
        d.window()
            .enumerate()
            .map(|(n, _)| d.row(n).unwrap().key.as_str())
            .collect()
    }

    #[test]
    fn it_opens_on_the_context_or_on_everything() {
        let d = Drawer::new(
            Kind::Hidden,
            vec![entry("a", "A", false), entry("b", "B", true)],
        );
        assert_eq!(d.scope(), Scope::Here);
        assert_eq!(keys(&d), ["b"]);
        assert!(d.elsewhere());
        let d = Drawer::new(
            Kind::Hidden,
            vec![entry("a", "A", false), entry("b", "B", false)],
        );
        assert_eq!(d.scope(), Scope::All);
        assert_eq!(keys(&d), ["a", "b"]);
        assert!(!d.elsewhere());
        let d = Drawer::new(Kind::Hidden, Vec::new());
        assert_eq!(d.scope(), Scope::All);
        assert!(d.is_empty() && d.selected().is_none() && d.window().is_empty());
    }

    #[test]
    fn the_scope_toggles() {
        let mut d = Drawer::new(
            Kind::Hidden,
            vec![
                entry("a", "A", false),
                entry("b", "B", true),
                entry("c", "C", false),
            ],
        );
        d.step(0);
        d.toggle_scope();
        assert_eq!(d.scope(), Scope::All);
        assert_eq!(keys(&d), ["a", "b", "c"]);
        assert_eq!(d.selected().unwrap().key, "a");
        d.toggle_scope();
        assert_eq!(keys(&d), ["b"]);
    }

    #[test]
    fn typing_finds_the_note_among_thirty() {
        let titles = [
            "Release checklist",
            "Groceries",
            "Q4 review",
            "Bug triage",
            "Trip to Lisbon",
            "Meeting notes",
            "Reading list",
            "Budget",
        ];
        let mut entries: Vec<Entry> = (0..30)
            .map(|i| {
                let t = format!("{} {i}", titles[i % titles.len()]);
                entry(&format!("n{i}.md"), &t, true)
            })
            .collect();
        entries.push(with_body("x.md", "Errands", "buy stamps and milk", true));
        let mut d = Drawer::new(Kind::Hidden, entries);
        // A few letters: the match is selected, ready for Enter.
        assert!(d.set_filter("lisb"));
        assert!(d.selected().unwrap().title.starts_with("Trip to Lisbon"));
        // Bodies count, too.
        d.set_filter("stamps");
        assert_eq!(keys(&d), ["x.md"]);
        assert_eq!(d.selected().unwrap().key, "x.md");
        // Case doesn't matter; words may be anywhere (title and body).
        d.set_filter("ERRANDS stamps");
        assert_eq!(keys(&d), ["x.md"]);
        d.set_filter("zzzz");
        assert!(d.is_empty() && d.selected().is_none());
        assert!(!d.set_filter("zzzz"));
        d.set_filter("");
        assert_eq!(d.len(), 31);
    }

    #[test]
    fn titles_beat_bodies() {
        let entries = vec![
            with_body("body.md", "Other", "remember the invoice", true),
            entry("title.md", "Invoice", true),
        ];
        let mut d = Drawer::new(Kind::Hidden, entries);
        d.set_filter("invoice");
        assert_eq!(keys(&d), ["title.md", "body.md"]);
    }

    #[test]
    fn the_filter_respects_the_scope() {
        let entries = vec![entry("a", "Alpha", false), entry("b", "Alpine", true)];
        let mut d = Drawer::new(Kind::Hidden, entries);
        d.set_filter("alp");
        assert_eq!(keys(&d), ["b"]);
        assert!(d.elsewhere());
        d.toggle_scope();
        assert_eq!(keys(&d).len(), 2);
        assert!(!d.elsewhere());
    }

    #[test]
    fn the_selection_follows_its_note_and_the_window_follows_it() {
        let entries: Vec<Entry> = (0..12)
            .map(|i| entry(&format!("n{i}"), &format!("N{i}"), true))
            .collect();
        let mut d = Drawer::new(Kind::Hidden, entries.clone());
        d.set_capacity(4);
        assert_eq!(d.window(), 0..4);
        assert_eq!(d.below(), 8);
        d.step(5);
        assert_eq!(d.selected().unwrap().key, "n5");
        assert_eq!(d.window(), 2..6);
        d.step(100);
        assert_eq!(d.selected().unwrap().key, "n11");
        assert_eq!((d.window(), d.below(), d.above()), (8..12, 0, 8));
        d.step(-100);
        assert_eq!(
            (d.selected().unwrap().key.as_str(), d.window()),
            ("n0", 0..4)
        );
        d.jump(true);
        assert_eq!(d.selected().unwrap().key, "n11");
        d.jump(false);
        assert_eq!(d.selected().unwrap().key, "n0");
        // A note leaves (shown again): the selection stays on its note.
        d.step(6);
        let mut fewer = entries.clone();
        fewer.remove(1);
        d.set_entries(fewer.clone());
        assert_eq!(d.selected().unwrap().key, "n6");
        // The selected note itself leaves: the one that took its place.
        fewer.retain(|e| e.key != "n6");
        d.set_entries(fewer);
        assert_eq!(d.selected().unwrap().key, "n7");
        // The last leaves, with it selected: the new last.
        d.jump(true);
        d.set_entries(entries[..3].to_vec());
        assert_eq!(d.selected().unwrap().key, "n2");
        assert_eq!(d.window(), 0..3);
        d.set_entries(Vec::new());
        assert!(d.selected().is_none() && d.window().is_empty());
    }

    #[test]
    fn scrolling_keeps_the_window_in_the_list() {
        let entries: Vec<Entry> = (0..10)
            .map(|i| entry(&format!("n{i}"), &format!("N{i}"), true))
            .collect();
        let mut d = Drawer::new(Kind::Hidden, entries);
        d.set_capacity(4);
        d.scroll(3);
        assert_eq!(d.window(), 3..7);
        // The selection stayed (row 0): not in view, but not lost.
        assert_eq!(d.selected().unwrap().key, "n0");
        d.scroll(100);
        assert_eq!(d.window(), 6..10);
        assert_eq!(d.below(), 0);
        d.scroll(-100);
        assert_eq!(d.window(), 0..4);
        // A taller drawer shows more, a shorter one fewer.
        assert!(d.set_capacity(6));
        assert_eq!(d.window(), 0..6);
        assert!(!d.set_capacity(6));
        d.step(9);
        assert_eq!(d.window(), 4..10);
        d.set_capacity(2);
        assert_eq!(d.window(), 8..10);
        assert_eq!(d.page(), 1);
        d.set_capacity(5);
        assert_eq!(d.page(), 4);
        // Rows are numbered from the window's top.
        assert!(d.is_selected(d.window().len() - 1));
        d.select(0);
        assert_eq!(d.selected().unwrap().key, d.row(0).unwrap().key);
        d.select(99);
        assert!(d.is_selected(0));
    }

    #[test]
    fn rows_fit_the_body() {
        // A sheet's body: 248 by 200, rows 30 tall, a 15 px footer.
        let l = layout(248.0, 200.0, 30.0, 15.0, 3);
        assert_eq!(l.capacity(), 5);
        let l = layout(248.0, 200.0, 30.0, 15.0, 5);
        assert_eq!(l.capacity(), 5);
        // More than fit: the footer takes the room of a part of a row.
        let l = layout(248.0, 200.0, 30.0, 15.0, 6);
        assert_eq!(l.capacity(), 5);
        let l = layout(248.0, 200.0, 36.0, 15.0, 6);
        assert_eq!(l.capacity(), 4);
        let l = layout(248.0, 200.0, 36.0, 15.0, 4);
        assert_eq!(l.capacity(), 4);
        // Never none.
        assert_eq!(layout(248.0, 10.0, 30.0, 15.0, 9).capacity(), 1);
        // Everything inside the body, nothing overlapping.
        for (h, row, total) in [(200.0, 30.0, 40), (160.0, 33.0, 2), (90.0, 30.0, 9)] {
            let l = layout(248.0, h, row, 15.0, total);
            let body = Rect::new(0.0, 0.0, 248.0, h);
            assert_eq!(l.filter.right() + 6.0, l.scope.x);
            assert!(l.scope.right() <= body.right() + 1e-9);
            assert!(l.rows[0].y >= l.filter.bottom());
            for pair in l.rows.windows(2) {
                assert!(pair[0].bottom() <= pair[1].y + 1e-9);
            }
            if l.capacity() > 1 || total > l.capacity() {
                let last = l.rows.last().unwrap();
                if total > l.capacity() {
                    assert!(last.bottom() <= l.more.y + 1e-9, "{h} {row} {total}");
                }
            }
        }
    }

    #[test]
    fn the_trash_icon_sits_at_the_row_end() {
        let row = Rect::new(0.0, 36.0, 248.0, 30.0);
        let t = trash_rect(row, 16.0);
        assert!(t.right() <= row.right() && t.x > row.x);
        assert_eq!(t.y - row.y, row.bottom() - t.bottom());
    }

    #[test]
    fn first_lines() {
        assert_eq!(
            first_line("\n\n  - [ ] bump the version\n- tag", "x"),
            "bump the version"
        );
        assert_eq!(first_line("# Title\nreal text", "Title"), "real text");
        // A heading that isn't the title shows, without its marks.
        assert_eq!(first_line("## Other\ntext", "Title"), "Other");
        assert_eq!(first_line("> quoted", ""), "quoted");
        assert_eq!(first_line("12. twelfth", ""), "twelfth");
        assert_eq!(first_line("1) first", ""), "first");
        assert_eq!(first_line("- [x] done", ""), "done");
        assert_eq!(first_line("", "t"), "");
        assert_eq!(first_line("#\n---", ""), "---");
        assert_eq!(first_line(&"é".repeat(500), "").chars().count(), EXCERPT);
    }

    #[test]
    fn entries_say_where_they_show() {
        let global = Note::parse("# Groceries\nmilk\n");
        let e = Entry::of("g.md".into(), "g", &global, 0, true, false);
        assert_eq!(
            (e.title.as_str(), e.app.as_str(), e.line.as_str()),
            ("Groceries", "all apps", "milk")
        );
        let app = Note::parse("+++\napps = [\"firefox\"]\n+++\nlinks\n");
        let e = Entry::of("f.md".into(), "f", &app, 0, false, true);
        assert_eq!((e.title.as_str(), e.app.as_str()), ("f", "firefox"));
        assert!(e.overdue && !e.here);
        // Found by what the row says, and by what the CLI calls it.
        for filter in ["all apps", "global"] {
            let mut d = Drawer::new(
                Kind::Hidden,
                vec![Entry::of("g.md".into(), "g", &global, 0, true, false), e.clone()],
            );
            d.set_filter(filter);
            assert_eq!(keys(&d), ["g.md"], "{filter}");
        }
    }

    fn due_entry(key: &str, at: i64, overdue: bool, hidden: bool) -> Entry {
        let mut e = entry(key, key, true).due(at, format!("at {at}"));
        e.overdue = overdue;
        e.dimmed(hidden)
    }

    #[test]
    fn the_due_drawer_puts_the_overdue_first_then_the_soonest() {
        let mut entries = vec![
            due_entry("later", 900, false, false),
            due_entry("soon", 300, false, true),
            due_entry("old", 100, true, false),
            due_entry("older", 50, true, true),
            due_entry("same-a", 500, false, false),
            due_entry("same-b", 500, false, false),
        ];
        sort_due(&mut entries);
        let order: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(order, ["older", "old", "soon", "same-a", "same-b", "later"]);
        // Hidden ones are in it, dimmed.
        assert!(entries[0].dim && !entries[1].dim);
        // Overdue wins even over an earlier time that hasn't passed (a
        // clock that was set back).
        let mut odd = vec![
            due_entry("future", 10, false, false),
            due_entry("past", 20, true, false),
        ];
        sort_due(&mut odd);
        assert_eq!(odd[0].key, "past");
        // The same drawer, in a drawer: that is the order shown.
        let d = Drawer::new(Kind::Due, entries);
        assert_eq!(keys(&d)[0], "older");
        assert_eq!(
            d.row(0).unwrap().subtitle(),
            "at 50 · all apps · body of older"
        );
    }

    #[test]
    fn a_row_says_when_where_and_what() {
        let note = Note::parse("+++\napps = [\"firefox\"]\n+++\nlinks\n");
        let e = Entry::of("f.md".into(), "f", &note, 0, true, false);
        assert_eq!(e.subtitle(), "firefox · links");
        let e = e.due(1, "Thu 20 Nov 09:00".into());
        assert_eq!(e.subtitle(), "Thu 20 Nov 09:00 · firefox · links");
        let blank = Note::parse("+++\ntitle = \"T\"\n+++\n");
        let e = Entry::template("t.md".into(), "t", &blank, 0);
        assert_eq!(e.subtitle(), "all apps");
    }

    #[test]
    fn a_template_says_where_its_notes_show() {
        let any = Note::parse("# Meeting {date}\nagenda\n");
        let own = Note::parse("+++\napps = [\"thunderbird\"]\n+++\nreply\n");
        let e = Entry::template("any.md".into(), "any", &any, 0);
        assert_eq!((e.app.as_str(), e.here), ("all apps", true));
        assert_eq!(e.subtitle(), "all apps · agenda");
        let e = Entry::template("own.md".into(), "own", &own, 0);
        assert_eq!((e.app.as_str(), e.here), ("thunderbird", true));
        let focused = Note::parse("+++\napps = [\"{app}\"]\n+++\nx\n");
        let e = Entry::template("f.md".into(), "f", &focused, 0);
        assert_eq!(e.app, "{app}");
    }

    #[test]
    fn the_templates_have_no_scope_and_the_kinds_their_buttons() {
        let mut d = Drawer::new(
            Kind::Templates,
            vec![entry("a", "A", true), entry("b", "B", true)],
        );
        assert_eq!(d.scope(), Scope::Here);
        d.toggle_scope();
        assert_eq!(d.scope(), Scope::Here);
        assert!(!d.elsewhere());
        assert_eq!(d.len(), 2);
        assert!(Kind::Hidden.scoped() && Kind::Due.scoped() && !Kind::Templates.scoped());
        assert_eq!(Kind::Hidden.row_action(), Some(RowAction::Trash));
        assert_eq!(Kind::Due.row_action(), Some(RowAction::Clear));
        assert_eq!(Kind::Templates.row_action(), None);
        for kind in [Kind::Hidden, Kind::Due, Kind::Templates] {
            assert!(!kind.label().is_empty());
            assert!(!kind.empty(false).is_empty() && !kind.empty(true).is_empty());
        }
    }
}
