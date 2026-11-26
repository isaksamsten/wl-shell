//! The shelf: all of a note's files, newest first, standing on boards in
//! place of its text (the file bar's shelf button opens it). Pure, unit
//! tested; column.rs draws it.
//!
//! - **Layout.** A caption ("12 files") at the top, then rows: as many
//!   items as fit at `ITEM_MIN` px plus `ITEM_GAP` between them, between
//!   3 and 6, grown to share the width (at most `ITEM_MAX`). A row is room
//!   for the hover's lift, the item's box, its board and a one-line label
//!   under the board: `ROW` px.
//! - **Order.** Newest first, as in the bar: the last link in the text is
//!   the first item.
//! - **Scrolling** is by rows, eased over `SCROLL_TIME`, so the row at the
//!   top may be cut off by the area's top (and one at the bottom by its
//!   bottom, saying there is more). Only rows in view are laid out.

use std::time::{Duration, Instant};

use wl_shell_notes::due::Zone;
use wl_shell_ui::Rect;

/// An item's width, and the room between items.
pub const ITEM_MIN: f64 = 56.0;
pub const ITEM_MAX: f64 = 72.0;
pub const ITEM_GAP: f64 = 12.0;
const MIN_COLUMNS: usize = 3;
const MAX_COLUMNS: usize = 6;
/// A row, top to bottom: room for the lift, the box, the board, a gap and
/// the label.
pub const LIFT: f64 = 3.0;
const LIFT_ROOM: f64 = 4.0;
pub const BOX: f64 = 56.0;
pub const BOARD: f64 = wl_shell_render::paper::BOARD as f64;
const LABEL_GAP: f64 = 2.0;
pub const LABEL: f64 = 16.0;
pub const ROW: f64 = LIFT_ROOM + BOX + BOARD + LABEL_GAP + LABEL;
/// The caption line above the rows.
pub const CAPTION: f64 = 18.0;
/// Scrolling a row (or more) takes this long; an item rises this fast.
pub const SCROLL_TIME: Duration = Duration::from_millis(120);
pub const LIFT_TIME: Duration = Duration::from_millis(100);

/// The shelf in an area, scrolled to a row.
#[derive(Clone, Debug, PartialEq)]
pub struct Shelf {
    /// The items in view, newest first: the file's index (in the text's
    /// order), its box and its label. Items stand on the box's bottom.
    pub items: Vec<(usize, Rect, Rect)>,
    /// One per row in view, the area's width.
    pub boards: Vec<Rect>,
    pub caption: Rect,
    /// Where the rows show: what they are cut to.
    pub rows: Rect,
    pub columns: usize,
    /// How many rows show whole.
    pub visible: usize,
    /// The row at the top when scrolled to the end.
    pub last_row: usize,
}

/// How many items a row `w` wide holds.
pub fn columns(w: f64) -> usize {
    let fit = ((w + ITEM_GAP) / (ITEM_MIN + ITEM_GAP)).floor().max(0.0) as usize;
    fit.clamp(MIN_COLUMNS, MAX_COLUMNS)
}

/// `count` files on shelves in `area`, scrolled so that row `row` (may
/// be between rows, while scrolling) is at the top.
pub fn layout(area: Rect, count: usize, row: f64) -> Shelf {
    let columns = columns(area.w);
    let caption = Rect::new(area.x, area.y, area.w, CAPTION.min(area.h));
    let rows = Rect::new(
        area.x,
        caption.bottom(),
        area.w,
        (area.h - caption.h).max(0.0),
    );
    let total = count.div_ceil(columns);
    let visible = ((rows.h / ROW).floor() as usize).max(1);
    let last_row = total.saturating_sub(visible);
    let row = row.clamp(0.0, last_row as f64);
    let cell = area.w / columns as f64;
    let gaps = ITEM_GAP * (columns - 1) as f64;
    let item_w = ((area.w - gaps) / columns as f64).clamp(1.0, ITEM_MAX);
    let shows = |r: &Rect| r.bottom() > rows.y && r.y < rows.bottom();
    let mut items = Vec::new();
    let mut boards = Vec::new();
    for r in row.floor() as usize..total {
        let top = rows.y + (r as f64 - row) * ROW;
        if top >= rows.bottom() {
            break;
        }
        let board = Rect::new(rows.x, top + LIFT_ROOM + BOX, rows.w, BOARD);
        if shows(&board) {
            boards.push(board);
        }
        for c in 0..columns {
            let order = r * columns + c;
            if order >= count {
                break;
            }
            let x = rows.x + c as f64 * cell;
            let b = Rect::new(x + (cell - item_w) / 2.0, top + LIFT_ROOM, item_w, BOX);
            let label = Rect::new(x + 2.0, board.bottom() + LABEL_GAP, cell - 4.0, LABEL);
            if shows(&b) || shows(&label) {
                items.push((count - 1 - order, b, label));
            }
        }
    }
    Shelf {
        items,
        boards,
        caption,
        rows,
        columns,
        visible,
        last_row,
    }
}

/// The file at `index` (of `count`) is this far into the shelf: 0 is
/// the newest.
pub fn order(index: usize, count: usize) -> usize {
    count.saturating_sub(1).saturating_sub(index)
}

/// The row to scroll to, from `row`, so the item `order` is in a whole
/// row of the `visible` ones.
pub fn follow(order: usize, row: usize, columns: usize, visible: usize) -> usize {
    let at = order / columns.max(1);
    if at < row {
        at
    } else if at >= row + visible.max(1) {
        at + 1 - visible.max(1)
    } else {
        row
    }
}

/// A keyboard move on the shelf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Left,
    Right,
    Up,
    Down,
}

/// Where the keyboard's item goes from `from` (None: none yet, so the
/// newest) by `m`, among `count` in rows of `columns`.
pub fn step(from: Option<usize>, m: Move, columns: usize, count: usize) -> Option<usize> {
    let last = count.checked_sub(1)?;
    let Some(from) = from else {
        return Some(0);
    };
    let from = from.min(last);
    Some(match m {
        Move::Left => from.saturating_sub(1),
        Move::Right => (from + 1).min(last),
        Move::Up => from.checked_sub(columns).unwrap_or(from),
        Move::Down if from + columns <= last => from + columns,
        // To the last row's last item, if the one below is past it.
        Move::Down if from / columns < last / columns => last,
        Move::Down => from,
    })
}

/// The scrolling: the row scrolled to, eased there from where it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    pub row: usize,
    from: f64,
    since: Option<Instant>,
}

impl Default for Scroll {
    fn default() -> Self {
        Self {
            row: 0,
            from: 0.0,
            since: None,
        }
    }
}

impl Scroll {
    /// Where it is at `now`, in rows.
    pub fn at(&self, now: Instant) -> f64 {
        let Some(since) = self.since else {
            return self.row as f64;
        };
        let t = now.saturating_duration_since(since).as_secs_f64() / SCROLL_TIME.as_secs_f64();
        if t >= 1.0 {
            return self.row as f64;
        }
        self.from + (self.row as f64 - self.from) * ease(t)
    }

    pub fn moving(&self, now: Instant) -> bool {
        self.since
            .is_some_and(|s| now.saturating_duration_since(s) < SCROLL_TIME)
    }

    /// Scroll to `row`, from where it is at `now`.
    pub fn to(&mut self, row: usize, now: Instant) {
        if row != self.row {
            self.from = self.at(now);
            self.row = row;
            self.since = Some(now);
        }
    }
}

/// Fast at first, slowing to a stop: `t` from 0 to 1.
pub fn ease(t: f64) -> f64 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// The caption: how many files, and how many of them are missing.
pub fn caption(count: usize, missing: usize) -> String {
    let files = match count {
        1 => "1 file".to_string(),
        n => format!("{n} files"),
    };
    match missing {
        0 => files,
        n => format!("{files} · {n} missing"),
    }
}

/// A file's size as file managers show it: `512 B`, `3.4 kB`, `12 MB`.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1000.0;
    let mut unit = 0;
    while v >= 999.5 && unit + 1 < UNITS.len() {
        v /= 1000.0;
        unit += 1;
    }
    match v < 9.95 {
        true => format!("{v:.1} {}", UNITS[unit]),
        false => format!("{v:.0} {}", UNITS[unit]),
    }
}

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// What the bar says of a file on the shelf: its name, size and the day
/// it was last changed (`report.pdf · 2.1 MB · Tue 24 Nov`), in `zone`.
/// A missing file is its name.
pub fn details(name: &str, bytes: Option<u64>, mtime: Option<i64>, zone: &dyn Zone) -> String {
    let mut out = name.to_string();
    if let Some(b) = bytes {
        out += " · ";
        out += &size(b);
    }
    if let Some(t) = mtime {
        let c = zone.to_civil(t);
        let month = MONTHS[(c.month.clamp(1, 12) - 1) as usize];
        out += &format!(" · {} {} {month}", WEEKDAYS[c.weekday() as usize], c.day);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_shell_notes::due::Fixed;

    fn inside(r: &Rect, area: &Rect) -> bool {
        r.x >= area.x - 1e-9
            && r.y >= area.y - 1e-9
            && r.right() <= area.right() + 1e-9
            && r.bottom() <= area.bottom() + 1e-9
    }

    #[test]
    fn columns_for_sheet_widths() {
        // A 300 px sheet's text area, a wide one, a narrow one.
        assert_eq!(columns(248.0), 3);
        assert_eq!(columns(300.0), 4);
        assert_eq!(columns(500.0), 6);
        assert_eq!(columns(120.0), 3);
        for w in [120.0, 248.0, 300.0, 420.0, 800.0] {
            let s = layout(Rect::new(10.0, 20.0, w, 400.0), 30, 0.0);
            let row: Vec<_> = s.items.iter().take(s.columns).collect();
            for pair in row.windows(2) {
                assert!(pair[0].1.right() <= pair[1].1.x, "{w}: items overlap");
            }
            assert!(row.iter().all(|(_, b, _)| b.w <= ITEM_MAX));
            assert!(
                row.iter()
                    .all(|(_, b, l)| inside(b, &s.rows) && inside(l, &s.rows))
            );
        }
    }

    #[test]
    fn newest_first_and_on_their_boards() {
        let area = Rect::new(12.0, 10.0, 248.0, 180.0);
        let s = layout(area, 7, 0.0);
        let order: Vec<usize> = s.items.iter().map(|(i, _, _)| *i).collect();
        // The third shelf, with the oldest, is below the area.
        assert_eq!(order, [6, 5, 4, 3, 2, 1]);
        assert!(s.caption.y == area.y && s.rows.y == s.caption.bottom());
        // Two shelves and part of a third in a 220 px sheet's text area.
        assert_eq!((s.columns, s.visible), (3, 1));
        assert_eq!(s.boards.len(), 2);
        for (_, b, l) in &s.items {
            let board = s.boards.iter().find(|r| r.y == b.bottom()).unwrap();
            assert_eq!((board.x, board.w), (area.x, area.w));
            assert!(l.y > board.bottom());
        }
    }

    #[test]
    fn every_item_is_reachable_and_nothing_is_outside() {
        for (h, count) in [(180.0, 20), (110.0, 3), (400.0, 25), (110.0, 9), (180.0, 0)] {
            let area = Rect::new(12.0, 10.0, 248.0, h);
            let end = layout(area, count, f64::MAX);
            let mut seen = vec![false; count];
            for row in 0..=end.last_row {
                let s = layout(area, count, row as f64);
                assert_eq!(s.last_row, end.last_row);
                for (i, b, l) in &s.items {
                    // Every item given shows, at least partly.
                    let shows = |r: &Rect| r.bottom() > s.rows.y && r.y < s.rows.bottom();
                    assert!(shows(b) || shows(l));
                    for r in [b, l] {
                        assert!(r.x >= area.x && r.right() <= area.right());
                    }
                    if inside(b, &s.rows) && inside(l, &s.rows) {
                        seen[*i] = true;
                    }
                }
                assert!(
                    s.boards
                        .iter()
                        .all(|r| r.bottom() > s.rows.y && r.y < s.rows.bottom())
                );
            }
            assert!(seen.iter().all(|s| *s), "{h} {count}: {seen:?}");
            // Scrolled to the end, the oldest is there.
            if count > 0 {
                assert!(end.items.iter().any(|(i, _, _)| *i == 0));
            }
        }
    }

    #[test]
    fn scrolling_eases_to_the_end() {
        let area = Rect::new(0.0, 0.0, 248.0, 180.0);
        let last = layout(area, 20, 0.0).last_row;
        assert_eq!(last, 6);
        let now = Instant::now();
        let mut scroll = Scroll::default();
        for _ in 0..10 {
            let next = (scroll.row + 1).min(last);
            scroll.to(next, now);
        }
        assert_eq!(scroll.row, last);
        assert!(!scroll.moving(now + SCROLL_TIME));
        assert_eq!(scroll.at(now + SCROLL_TIME), last as f64);
        // Midway, between where it was and where it goes.
        let mut s = Scroll::default();
        s.to(2, now);
        let mid = s.at(now + SCROLL_TIME / 2);
        assert!(mid > 1.0 && mid < 2.0, "{mid}");
        assert!(s.moving(now + SCROLL_TIME / 2));
        // Between rows, the top one is cut by the area's top.
        let between = layout(area, 20, mid);
        assert!(between.items[0].1.y < between.rows.y);
        // Back up from midway: from where it is.
        s.to(0, now + SCROLL_TIME / 2);
        assert!((s.at(now + SCROLL_TIME / 2) - mid).abs() < 1e-9);
    }

    #[test]
    fn the_keyboard_moves_and_the_shelf_follows() {
        assert_eq!(step(None, Move::Right, 3, 7), Some(0));
        assert_eq!(step(Some(0), Move::Left, 3, 7), Some(0));
        assert_eq!(step(Some(2), Move::Right, 3, 7), Some(3));
        assert_eq!(step(Some(1), Move::Down, 3, 7), Some(4));
        assert_eq!(step(Some(4), Move::Down, 3, 7), Some(6));
        assert_eq!(step(Some(6), Move::Down, 3, 7), Some(6));
        assert_eq!(step(Some(4), Move::Up, 3, 7), Some(1));
        assert_eq!(step(Some(1), Move::Up, 3, 7), Some(1));
        assert_eq!(step(Some(0), Move::Down, 3, 0), None);
        assert_eq!(follow(6, 0, 3, 1), 2);
        assert_eq!(follow(6, 0, 3, 2), 1);
        assert_eq!(follow(0, 2, 3, 1), 0);
        assert_eq!(follow(4, 1, 3, 1), 1);
        assert_eq!(order(6, 7), 0);
        assert_eq!(order(0, 7), 6);
    }

    #[test]
    fn what_the_bar_says() {
        assert_eq!(caption(1, 0), "1 file");
        assert_eq!(caption(12, 2), "12 files · 2 missing");
        assert_eq!(size(512), "512 B");
        assert_eq!(size(3400), "3.4 kB");
        assert_eq!(size(2_100_000), "2.1 MB");
        assert_eq!(size(12_400_000), "12 MB");
        assert_eq!(size(999_999), "1.0 MB");
        // Tue 24 Nov 2026, 13:00 UTC.
        let t = 1_795_525_200;
        assert_eq!(
            details("report.pdf", Some(2_100_000), Some(t), &Fixed(0)),
            "report.pdf · 2.1 MB · Tue 24 Nov"
        );
        assert_eq!(details("gone.pdf", None, None, &Fixed(60)), "gone.pdf");
    }
}
