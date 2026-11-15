//! The strip's layout: an app's windows as cards, each with its title
//! under it, in a grid next to the dock's icon. Pure, unit tested;
//! stripview.rs shows it in a popup.
//!
//! Rows and columns are relative to the dock, not the screen. A row runs
//! along the dock's edge: the first one is next to the dock, centered on
//! the icon, and later rows stack away from it. Above a bottom dock rows
//! are horizontal; beside a side dock they are vertical. Columns are the
//! cards per row.
//!
//! Cards keep the window order. They fill a row up to `max_columns`, then
//! the next row out, and the last, partial row is centered. With more
//! cards than `max_columns` × `max_rows`, the rows cap wins: each row
//! holds ⌈n/rows⌉ cards. 0 means no limit.
//!
//! All cards share one height: `card_height`, or less so the grid fits in
//! `max_along` along the dock and `max_across` away from it, but not below
//! `MIN_CARD`; past that, the strip scrolls (along the dock if it is too
//! long there, else away from it). A card's width follows its window's
//! aspect ratio, capped at `max_card_width` (a capped card gets shorter
//! instead and is centered in its slot). Every card's box is at least
//! `min_slot` wide, so its title has room.
//!
//! The layout works in dock coordinates, `u` along the dock and `v` away
//! from it, and maps them to the strip's surface when asked: (0, 0) is the
//! viewport's top left corner. Each card's box has room for the zoom, so a
//! zoomed card (and its title) never overlaps its neighbors.

use smithay_client_toolkit::seat::keyboard::Keysym;
use wl_shell_core::mru::Mru;
use wl_shell_ui::Rect;
use wl_shell_ui::card::aspect;

use crate::config::Position;

/// Cards never get shorter than this (unless configured smaller).
pub const MIN_CARD: f64 = 72.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Params {
    /// Each window's aspect ratio (width / height), in strip order.
    pub aspects: Vec<f64>,
    /// The dock's edge.
    pub position: Position,
    /// Cards per row at most; 0: no limit.
    pub max_columns: usize,
    /// Rows at most; 0: no limit.
    pub max_rows: usize,
    /// Wanted card height.
    pub card_height: f64,
    /// Widest a card gets.
    pub max_card_width: f64,
    /// Longest the viewport gets along the dock.
    pub max_along: f64,
    /// Longest the viewport gets away from the dock.
    pub max_across: f64,
    /// Narrowest a card's box is (room for its title).
    pub min_slot: f64,
    pub gap: f64,
    /// Room under each card for its title (its gap included); 0: none.
    pub title: f64,
    /// Zoom factor of the highlighted card.
    pub zoom: f64,
}

/// The direction the strip scrolls in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Along the dock.
    Along,
    /// Away from the dock.
    Across,
}

/// An arrow key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Right,
    Up,
    Down,
}

/// What a key does on the open strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// Move the highlight.
    Move(Arrow),
    /// Activate the highlighted card's window.
    Activate,
    /// Activate card n (from 0).
    Pick(usize),
    Close,
}

impl Key {
    pub fn from_sym(sym: Keysym) -> Option<Self> {
        Some(match sym {
            Keysym::Escape => Key::Close,
            Keysym::Return | Keysym::KP_Enter | Keysym::space => Key::Activate,
            Keysym::Left | Keysym::h => Key::Move(Arrow::Left),
            Keysym::Right | Keysym::l => Key::Move(Arrow::Right),
            Keysym::Up | Keysym::k => Key::Move(Arrow::Up),
            Keysym::Down | Keysym::j => Key::Move(Arrow::Down),
            _ => {
                let n = (sym.raw().checked_sub(Keysym::_1.raw())).filter(|&n| n < 9)?;
                Key::Pick(n as usize)
            }
        })
    }

    /// Whether holding the key repeats it.
    pub fn repeats(self) -> bool {
        matches!(self, Key::Move(_))
    }
}

/// The card highlighted when a strip of the windows `ids` opens: the most
/// recently focused one that isn't `focused`, so Enter switches to the
/// app's previous window. Without focus history, the first one that isn't
/// focused.
pub fn first_highlight(ids: &[&str], mru: &Mru, focused: Option<&str>) -> usize {
    let mut order: Vec<&str> = ids.to_vec();
    mru.sort(&mut order, |id| id);
    let pick = (order.iter())
        .find(|&&id| Some(id) != focused)
        .or(order.first());
    pick.and_then(|id| ids.iter().position(|i| i == id))
        .unwrap_or(0)
}

/// The cards per row and the rows for `n` cards with the caps (0: no
/// limit): rows fill up to `max_columns`, and if that makes too many rows,
/// the rows cap wins.
pub fn grid(n: usize, max_columns: usize, max_rows: usize) -> (usize, usize) {
    if n == 0 {
        return (0, 0);
    }
    let columns = match max_columns {
        0 => n,
        c => c.min(n),
    };
    let rows = n.div_ceil(columns);
    if max_rows > 0 && rows > max_rows {
        let columns = n.div_ceil(max_rows);
        return (columns, n.div_ceil(columns));
    }
    (columns, rows)
}

/// A rect in dock coordinates: `u` along the dock, `v` away from it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    u: f64,
    v: f64,
    along: f64,
    across: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    position: Position,
    /// The cards at rest, in surface coordinates relative to their box's
    /// corner (so they can be placed with the box).
    cards: Vec<Rect>,
    /// Each card's box (the card, its zoom and title), in dock coordinates.
    boxes: Vec<Span>,
    /// The part of the grid that takes the pointer for each card: its box,
    /// as thick as its row, and half the gaps around it.
    slots: Vec<Span>,
    /// The content's size, along and across.
    content: (f64, f64),
    /// The viewport's size, along and across.
    view: (f64, f64),
    /// The cards' shared height.
    height: f64,
    zoom: f64,
    pub columns: usize,
    pub rows: usize,
}

impl Layout {
    pub fn new(p: &Params) -> Self {
        let n = p.aspects.len();
        let aspects: Vec<f64> = p.aspects.iter().map(|&a| aspect(a)).collect();
        let zoom = p.zoom.max(1.0);
        let min = MIN_CARD.min(p.card_height);
        let (columns, rows) = grid(n, p.max_columns, p.max_rows);
        let side = p.position != Position::Bottom;
        // The card (its size) and its box, along and across, for cards of
        // height `h`.
        let sized = |h: f64| -> Vec<((f64, f64), (f64, f64))> {
            (aspects.iter())
                .map(|&a| {
                    let w = (h * a).min(p.max_card_width.max(1.0));
                    let (bw, bh) = ((w * zoom).max(p.min_slot), h * zoom + p.title);
                    let b = if side { (bh, bw) } else { (bw, bh) };
                    ((w, w / a), b)
                })
                .collect()
        };
        // Each row's length along and thickness across.
        let rows_of = |boxes: &[(f64, f64)]| -> Vec<(f64, f64)> {
            (boxes.chunks(columns.max(1)))
                .map(|row| {
                    let along: f64 = row.iter().map(|b| b.0).sum();
                    let along = along + (row.len() - 1) as f64 * p.gap;
                    (along, row.iter().map(|b| b.1).fold(0.0, f64::max))
                })
                .collect()
        };
        let extent = |h: f64| -> (f64, f64) {
            let boxes: Vec<(f64, f64)> = sized(h).into_iter().map(|s| s.1).collect();
            let rows = rows_of(&boxes);
            let along = rows.iter().map(|r| r.0).fold(0.0, f64::max);
            let across: f64 = rows.iter().map(|r| r.1).sum();
            (along, across + rows.len().saturating_sub(1) as f64 * p.gap)
        };
        let fits = |h: f64| {
            let (along, across) = extent(h);
            along <= p.max_along && across <= p.max_across
        };
        // The tallest cards that fit, by bisection: the grid grows with the
        // height.
        let mut h = p.card_height.max(1.0);
        if !fits(h) && h > min {
            let (mut lo, mut hi) = (min, h);
            for _ in 0..40 {
                let mid = (lo + hi) / 2.0;
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            h = lo;
        }

        let sized = sized(h);
        let boxes_size: Vec<(f64, f64)> = sized.iter().map(|s| s.1).collect();
        let row_sizes = rows_of(&boxes_size);
        let content_along = row_sizes.iter().map(|r| r.0).fold(0.0, f64::max);
        let content_across = row_sizes.iter().map(|r| r.1).sum::<f64>()
            + row_sizes.len().saturating_sub(1) as f64 * p.gap;
        let mut boxes = Vec::with_capacity(n);
        let mut slots = Vec::with_capacity(n);
        let mut cards = Vec::with_capacity(n);
        let half = p.gap / 2.0;
        let mut v = 0.0;
        for (r, row) in boxes_size.chunks(columns.max(1)).enumerate() {
            let (length, thickness) = row_sizes[r];
            // Each row centered along the content.
            let mut u = (content_along - length) / 2.0;
            for (k, &(along, across)) in row.iter().enumerate() {
                let i = r * columns + k;
                boxes.push(Span {
                    u,
                    v: v + (thickness - across) / 2.0,
                    along,
                    across,
                });
                slots.push(Span {
                    u: u - half,
                    v: v - half,
                    along: along + p.gap,
                    across: thickness + p.gap,
                });
                // The card in its box: centered across the box's width,
                // with room above for the zoom and below for the title.
                let ((w, ch), _) = sized[i];
                let bw = if side { across } else { along };
                let top = (zoom - 1.0) * h / 2.0 + (h - ch) / 2.0;
                cards.push(Rect::new((bw - w) / 2.0, top, w, ch));
                u += along + p.gap;
            }
            v += thickness + p.gap;
        }
        let view = (
            content_along.min(p.max_along.max(0.0)),
            content_across.min(p.max_across.max(0.0)),
        );
        Self {
            position: p.position,
            cards,
            boxes,
            slots,
            content: (content_along, content_across),
            view,
            height: h,
            zoom,
            columns,
            rows,
        }
    }

    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// The viewport's size on the surface.
    pub fn size(&self) -> (f64, f64) {
        match self.position {
            Position::Bottom => self.view,
            Position::Left | Position::Right => (self.view.1, self.view.0),
        }
    }

    /// The cards' shared height.
    #[cfg(test)]
    pub fn height(&self) -> f64 {
        self.height
    }

    /// The direction the strip scrolls in, if it does: along the dock if
    /// it is too long there, else away from it.
    pub fn axis(&self) -> Axis {
        if self.content.0 > self.view.0 + 0.5 {
            Axis::Along
        } else {
            Axis::Across
        }
    }

    /// How far the content scrolls.
    pub fn max_scroll(&self) -> f64 {
        match self.axis() {
            Axis::Along => (self.content.0 - self.view.0).max(0.0),
            Axis::Across => (self.content.1 - self.view.1).max(0.0),
        }
    }

    pub fn scrolls(&self) -> bool {
        self.max_scroll() > 0.5
    }

    /// `s` on the surface, scrolled by `scroll`.
    fn rect(&self, s: Span, scroll: f64) -> Rect {
        let (mut u, mut v) = (s.u, s.v);
        match self.axis() {
            Axis::Along => u -= scroll,
            Axis::Across => v -= scroll,
        }
        let across = self.view.1;
        match self.position {
            Position::Bottom => Rect::new(u, across - v - s.across, s.along, s.across),
            Position::Left => Rect::new(v, u, s.across, s.along),
            Position::Right => Rect::new(across - v - s.across, u, s.across, s.along),
        }
    }

    /// Card `i`'s box (the card, its zoom and title) on the surface.
    #[cfg(test)]
    fn card_box(&self, i: usize, scroll: f64) -> Rect {
        self.rect(self.boxes[i], scroll)
    }

    /// Card `i` on the surface, at `scroll`.
    pub fn card(&self, i: usize, scroll: f64) -> Rect {
        let b = self.rect(self.boxes[i], scroll);
        let c = self.cards[i];
        Rect::new(b.x + c.x, b.y + c.y, c.w, c.h)
    }

    /// Card `i` zoomed by `z` (0: at rest, 1: fully zoomed) around its
    /// center.
    pub fn zoomed(&self, i: usize, scroll: f64, z: f64) -> Rect {
        let s = 1.0 + (self.zoom - 1.0) * z.clamp(0.0, 1.0);
        self.card(i, scroll).scaled_center(s)
    }

    /// The card whose slot is at (`x`, `y`) on the surface, at `scroll`.
    pub fn hit(&self, x: f64, y: f64, scroll: f64) -> Option<usize> {
        let (w, h) = self.size();
        if !(0.0..w).contains(&x) || !(0.0..h).contains(&y) {
            return None;
        }
        (self.slots.iter()).position(|s| self.rect(*s, scroll).contains(x, y))
    }

    /// The card next to `i` in the direction of `arrow` on the screen: of
    /// the cards within 45° of that direction, the nearest, with sideways
    /// distance counting double so the same row or column wins.
    pub fn neighbor(&self, i: usize, arrow: Arrow) -> Option<usize> {
        let (x, y) = self.card(i, 0.0).center();
        // How far card `j` is that way, and sideways.
        let dir = |j: usize| {
            let (cx, cy) = self.card(j, 0.0).center();
            let (dx, dy) = (cx - x, cy - y);
            match arrow {
                Arrow::Left => (-dx, dy.abs()),
                Arrow::Right => (dx, dy.abs()),
                Arrow::Up => (-dy, dx.abs()),
                Arrow::Down => (dy, dx.abs()),
            }
        };
        (0..self.len())
            .filter(|&j| j != i)
            .map(|j| (j, dir(j)))
            .filter(|&(_, (ahead, aside))| ahead > 0.5 && aside <= ahead)
            .map(|(j, (ahead, aside))| (j, ahead + 2.0 * aside))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(j, _)| j)
    }

    /// How a scroll of `dx`, `dy` on the screen (positive: the view moves
    /// right or down) changes the scroll position. A vertical wheel moves
    /// a strip that scrolls sideways too.
    pub fn scroll_delta(&self, dx: f64, dy: f64) -> f64 {
        // The screen direction scrolling moves the view in.
        let (sx, sy) = match (self.axis(), self.position) {
            (Axis::Along, Position::Bottom) => (1.0, 0.0),
            (Axis::Along, _) => (0.0, 1.0),
            (Axis::Across, Position::Bottom) => (0.0, -1.0),
            (Axis::Across, Position::Left) => (1.0, 0.0),
            (Axis::Across, Position::Right) => (-1.0, 0.0),
        };
        match sx != 0.0 {
            true => dy + sx * dx,
            false => sy * dy + dx,
        }
    }

    /// The scroll position nearest `scroll` that shows all of card `i`'s
    /// slot.
    pub fn reveal(&self, i: usize, scroll: f64) -> f64 {
        let Some(s) = self.slots.get(i) else {
            return scroll;
        };
        let (start, len, view) = match self.axis() {
            Axis::Along => (s.u, s.along, self.view.0),
            Axis::Across => (s.v, s.across, self.view.1),
        };
        let max = self.max_scroll();
        let scroll = if start < scroll {
            start
        } else if start + len > scroll + view {
            start + len - view
        } else {
            scroll
        };
        scroll.clamp(0.0, max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(position: Position, aspects: &[f64]) -> Params {
        Params {
            aspects: aspects.to_vec(),
            position,
            max_columns: 3,
            max_rows: 3,
            card_height: 200.0,
            max_card_width: 500.0,
            max_along: 2000.0,
            max_across: 2000.0,
            min_slot: 120.0,
            gap: 12.0,
            title: 30.0,
            zoom: 1.0,
        }
    }

    const POSITIONS: [Position; 3] = [Position::Bottom, Position::Left, Position::Right];

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn fill_order_and_rows() {
        assert_eq!(grid(0, 3, 3), (0, 0));
        assert_eq!(grid(1, 3, 3), (1, 1));
        assert_eq!(grid(3, 3, 3), (3, 1));
        assert_eq!(grid(4, 3, 3), (3, 2));
        assert_eq!(grid(9, 3, 3), (3, 3));
        // A single line along the dock, and one away from it.
        assert_eq!(grid(5, 3, 1), (5, 1));
        assert_eq!(grid(5, 1, 0), (1, 5));
    }

    #[test]
    fn the_rows_cap_wins() {
        // 10 cards in at most 3 rows: 4 per row, the columns cap gives way.
        assert_eq!(grid(10, 3, 3), (4, 3));
        assert_eq!(grid(7, 2, 3), (3, 3));
        // 13 in 3 rows of 5: only 3 rows used.
        assert_eq!(grid(13, 3, 3), (5, 3));
    }

    #[test]
    fn zero_means_no_limit() {
        assert_eq!(grid(8, 0, 3), (8, 1));
        assert_eq!(grid(8, 3, 0), (3, 3));
        assert_eq!(grid(8, 0, 0), (8, 1));
    }

    #[test]
    fn cards_keep_the_window_order() {
        // Bottom: left to right, then up.
        let l = Layout::new(&params(Position::Bottom, &[1.0; 5]));
        let c: Vec<Rect> = (0..5).map(|i| l.card(i, 0.0)).collect();
        assert!(c[0].x < c[1].x && c[1].x < c[2].x);
        assert!(close(c[0].y, c[2].y));
        assert!(c[3].y < c[0].y && close(c[3].y, c[4].y));
        // Left: top to bottom, then right.
        let l = Layout::new(&params(Position::Left, &[1.0; 5]));
        let c: Vec<Rect> = (0..5).map(|i| l.card(i, 0.0)).collect();
        assert!(c[0].y < c[1].y && close(c[0].x, c[1].x));
        assert!(c[3].x > c[0].x);
        // Right: then left.
        let l = Layout::new(&params(Position::Right, &[1.0; 5]));
        assert!(l.card(3, 0.0).x < l.card(0, 0.0).x);
    }

    #[test]
    fn the_first_row_is_next_to_the_dock_and_centered() {
        for position in POSITIONS {
            let l = Layout::new(&params(position, &[1.5, 1.0, 0.8, 2.0, 1.2]));
            let (w, h) = l.size();
            let first: Vec<Rect> = (0..3).map(|i| l.card_box(i, 0.0)).collect();
            // Touching the dock's side.
            let touching = match position {
                Position::Bottom => first.iter().any(|b| close(b.bottom(), h)),
                Position::Left => first.iter().any(|b| close(b.x, 0.0)),
                Position::Right => first.iter().any(|b| close(b.right(), w)),
            };
            assert!(touching, "{position:?}");
            // Centered along it.
            let (lo, hi) = match position {
                Position::Bottom => (first[0].x, first[2].right()),
                _ => (first[0].y, first[2].bottom()),
            };
            let mid = match position {
                Position::Bottom => w / 2.0,
                _ => h / 2.0,
            };
            assert!(close((lo + hi) / 2.0, mid), "{position:?}");
        }
    }

    #[test]
    fn the_last_row_is_centered() {
        for position in POSITIONS {
            let l = Layout::new(&params(position, &[1.0; 4]));
            assert_eq!((l.columns, l.rows), (3, 2));
            // The last row is card 3 alone, centered on the middle card.
            let (a, b) = (l.card(1, 0.0).center(), l.card(3, 0.0).center());
            match position {
                Position::Bottom => assert!(close(a.0, b.0) && b.1 < a.1),
                _ => assert!(close(a.1, b.1)),
            }
        }
    }

    #[test]
    fn titles_get_room() {
        let l = Layout::new(&params(Position::Bottom, &[1.5, 0.5]));
        // Full height, widths by aspect; the narrow card's box is wide
        // enough for its title.
        assert_eq!(l.size(), (300.0 + 12.0 + 120.0, 230.0));
        assert_eq!(l.card(0, 0.0), Rect::new(0.0, 0.0, 300.0, 200.0));
        assert_eq!(l.card(1, 0.0), Rect::new(312.0 + 10.0, 0.0, 100.0, 200.0));
        // Beside a side dock, the title is under the card too.
        let l = Layout::new(&params(Position::Left, &[1.5, 0.5]));
        assert_eq!(l.size(), (300.0, 2.0 * 230.0 + 12.0));
        assert_eq!(l.card(1, 0.0), Rect::new(100.0, 242.0, 100.0, 200.0));
    }

    #[test]
    fn capped_cards_get_shorter() {
        let l = Layout::new(&params(Position::Bottom, &[4.0]));
        let c = l.card(0, 0.0);
        assert_eq!((c.w, c.h), (500.0, 125.0));
        // Centered in its slot.
        assert_eq!(c.y, 37.5);
    }

    #[test]
    fn a_big_grid_shrinks_then_scrolls() {
        let p = Params {
            max_across: 400.0,
            ..params(Position::Bottom, &[1.0; 4])
        };
        let l = Layout::new(&p);
        assert!(!l.scrolls());
        assert!(l.size().1 <= 400.0 + 1e-6);
        assert!((MIN_CARD..200.0).contains(&l.height()));

        // Too many rows: away from the dock.
        let p = Params {
            max_across: 400.0,
            max_columns: 2,
            max_rows: 0,
            ..params(Position::Bottom, &[1.0; 10])
        };
        let l = Layout::new(&p);
        assert!(l.scrolls());
        assert_eq!(l.axis(), Axis::Across);
        assert_eq!(l.height(), MIN_CARD);
        assert_eq!(l.size().1, 400.0);

        // Rows capped: along the dock.
        let p = Params {
            max_along: 600.0,
            max_columns: 2,
            max_rows: 2,
            ..params(Position::Bottom, &[1.0; 20])
        };
        let l = Layout::new(&p);
        assert!(l.scrolls());
        assert_eq!(l.axis(), Axis::Along);
        assert_eq!(l.size().0, 600.0);
    }

    #[test]
    fn scrolling_in_the_right_direction() {
        // Along a bottom dock: the cards move left as the view moves right.
        let p = Params {
            max_along: 600.0,
            max_rows: 1,
            ..params(Position::Bottom, &[1.0; 10])
        };
        let l = Layout::new(&p);
        assert_eq!(l.axis(), Axis::Along);
        assert_eq!(l.card(5, 10.0).x, l.card(5, 0.0).x - 10.0);
        assert!(l.scroll_delta(0.0, 1.0) > 0.0 && l.scroll_delta(1.0, 0.0) > 0.0);
        let s = l.max_scroll();
        assert!(l.card(9, s).right() <= 600.0 + 1e-6);

        // Away from a bottom dock: the far rows are above, and come down
        // into view; the wheel up shows them.
        let p = Params {
            max_across: 400.0,
            max_columns: 2,
            max_rows: 0,
            ..params(Position::Bottom, &[1.0; 10])
        };
        let l = Layout::new(&p);
        assert!(l.card(9, 0.0).bottom() < 0.0);
        assert!(l.card(9, 50.0).y > l.card(9, 0.0).y);
        assert!(l.scroll_delta(0.0, -1.0) > 0.0);
        let s = l.max_scroll();
        assert!(l.card(9, s).y >= -1e-6);

        // Away from a right dock: the far rows are to the left.
        let p = Params {
            max_across: 400.0,
            max_columns: 2,
            max_rows: 0,
            ..params(Position::Right, &[1.0; 10])
        };
        let l = Layout::new(&p);
        assert_eq!(l.axis(), Axis::Across);
        assert!(l.card(9, 50.0).x > l.card(9, 0.0).x);
        assert!(l.scroll_delta(-1.0, 0.0) > 0.0 && l.scroll_delta(0.0, 1.0) > 0.0);
    }

    #[test]
    fn zoomed_cards_never_overlap() {
        for position in POSITIONS {
            let p = Params {
                zoom: 1.1,
                ..params(position, &[1.6, 1.0, 0.8, 2.0, 0.5, 1.3, 3.0])
            };
            let l = Layout::new(&p);
            let (w, h) = l.size();
            for i in 0..l.len() {
                // The zoomed card and its title.
                let z = l.zoomed(i, 0.0, 1.0);
                let a = Rect::new(z.x, z.y, z.w, z.h + p.title);
                assert!(a.x >= -1e-9 && a.right() <= w + 1e-9, "{position:?} {i}");
                assert!(a.y >= -1e-9 && a.bottom() <= h + 1e-9, "{position:?} {i}");
                for j in 0..l.len() {
                    let b = l.card_box(j, 0.0);
                    let apart = a.right() <= b.x + 1e-9
                        || b.right() <= a.x + 1e-9
                        || a.bottom() <= b.y + 1e-9
                        || b.bottom() <= a.y + 1e-9;
                    assert!(i == j || apart, "{position:?} {i} {j}");
                }
            }
            let rest = l.card(1, 0.0);
            let still = l.zoomed(1, 0.0, 0.0);
            assert!(close(still.x, rest.x) && close(still.y, rest.y) && close(still.w, rest.w));
        }
    }

    #[test]
    fn hits_follow_the_grid() {
        for position in POSITIONS {
            let l = Layout::new(&params(position, &[1.0; 5]));
            for i in 0..5 {
                let (cx, cy) = l.card(i, 0.0).center();
                assert_eq!(l.hit(cx, cy, 0.0), Some(i), "{position:?}");
            }
            assert_eq!(l.hit(-1.0, 10.0, 0.0), None);
            assert_eq!(l.hit(10.0, l.size().1 + 1.0, 0.0), None);
        }
        // The title and the gap belong to the card.
        let l = Layout::new(&params(Position::Bottom, &[1.0, 1.0]));
        assert_eq!(l.hit(100.0, 215.0, 0.0), Some(0));
        assert_eq!(l.hit(205.0, 100.0, 0.0), Some(0));
        assert_eq!(l.hit(207.0, 100.0, 0.0), Some(1));
    }

    #[test]
    fn hits_follow_the_scroll() {
        let p = Params {
            max_along: 600.0,
            max_rows: 1,
            ..params(Position::Bottom, &[1.0; 10])
        };
        let l = Layout::new(&p);
        let s = l.max_scroll();
        let (cx, cy) = l.card(9, s).center();
        assert_eq!(l.hit(cx, cy, s), Some(9));
        assert_ne!(l.hit(cx, cy, 0.0), Some(9));
        // Revealing a card scrolls just enough.
        let r = l.reveal(9, 0.0);
        assert!(close(r, s));
        assert_eq!(l.reveal(0, s), 0.0);
        assert_eq!(l.reveal(0, 0.0), 0.0);
    }

    #[test]
    fn arrows_move_on_the_screen() {
        // Bottom: 0 1 2 in the first row, 3 4 above.
        let l = Layout::new(&params(Position::Bottom, &[1.0; 5]));
        assert_eq!(l.neighbor(0, Arrow::Right), Some(1));
        assert_eq!(l.neighbor(1, Arrow::Left), Some(0));
        assert_eq!(l.neighbor(2, Arrow::Right), None);
        assert_eq!(l.neighbor(0, Arrow::Up), Some(3));
        assert!(matches!(l.neighbor(4, Arrow::Down), Some(1 | 2)));
        assert_eq!(l.neighbor(0, Arrow::Down), None);
        // Left: rows are columns, the second one to the right.
        let l = Layout::new(&params(Position::Left, &[1.0; 5]));
        assert_eq!(l.neighbor(0, Arrow::Down), Some(1));
        assert_eq!(l.neighbor(0, Arrow::Right), Some(3));
        assert!(matches!(l.neighbor(3, Arrow::Left), Some(0 | 1)));
    }

    #[test]
    fn odd_aspects_are_sanitized() {
        let l = Layout::new(&params(Position::Bottom, &[f64::NAN, 0.0, 100.0]));
        for i in 0..3 {
            let c = l.card(i, 0.0);
            assert!(c.w > 0.0 && c.h > 0.0 && c.w.is_finite());
        }
    }

    #[test]
    fn empty() {
        let l = Layout::new(&params(Position::Bottom, &[]));
        assert_eq!(l.len(), 0);
        assert_eq!(l.size(), (0.0, 0.0));
        assert!(!l.scrolls());
    }

    #[test]
    fn keys() {
        assert_eq!(Key::from_sym(Keysym::Escape), Some(Key::Close));
        assert_eq!(Key::from_sym(Keysym::Return), Some(Key::Activate));
        assert_eq!(Key::from_sym(Keysym::Up), Some(Key::Move(Arrow::Up)));
        assert_eq!(Key::from_sym(Keysym::l), Some(Key::Move(Arrow::Right)));
        assert_eq!(Key::from_sym(Keysym::_1), Some(Key::Pick(0)));
        assert_eq!(Key::from_sym(Keysym::_9), Some(Key::Pick(8)));
        assert_eq!(Key::from_sym(Keysym::_0), None);
        assert_eq!(Key::from_sym(Keysym::a), None);
        assert!(Key::Move(Arrow::Left).repeats() && !Key::Activate.repeats());
    }

    #[test]
    fn the_highlight_starts_on_the_previous_window() {
        let ids = ["1", "2", "3"];
        // No focus history: the first window.
        assert_eq!(first_highlight(&ids, &Mru::default(), None), 0);
        let mut mru = Mru::default();
        for id in ["2", "9", "3"] {
            mru.focus(id);
        }
        // 3 is focused: the one before it.
        assert_eq!(first_highlight(&ids, &mru, Some("3")), 1);
        // Another app is focused: this app's most recent window.
        assert_eq!(first_highlight(&ids, &mru, Some("9")), 2);
        // Only the focused one was ever focused: the next never focused.
        let mut mru = Mru::default();
        mru.focus("1");
        assert_eq!(first_highlight(&ids, &mru, Some("1")), 1);
        assert_eq!(first_highlight(&["1"], &mru, Some("1")), 0);
        assert_eq!(first_highlight(&[], &mru, None), 0);
    }
}
