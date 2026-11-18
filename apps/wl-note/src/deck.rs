//! The notes in a column, how they move when the context changes, and
//! how far out they are. Pure, unit tested.
//!
//! Each note is a card with springs: `y`, its top along the edge;
//! `presence`, from 0 (behind the edge, out of sight) to 1 (in place);
//! `out`, how far it sticks out from the edge when in place (`tuck`
//! tucked, `peek` peeking, a little more when lifted under the pointer);
//! and `focus`, from 0 (in the column) to 1 (focused: grown and fully on
//! screen). When the notes change:
//!
//! 1. The notes that stay are left where they are.
//! 2. The notes that leave slide back behind the edge, staggered.
//! 3. Once they are gone, the column closes up: the notes that stay move
//!    to their new places, and the new ones slide out of the edge at
//!    theirs, staggered too.
//!
//! A note wanted again while it is still leaving comes back from where it
//! is. The column's [`Mode`] (tucked, peeking, or a note focused) and the
//! note under the pointer set where `out` and `focus` head. A `+` sliver
//! after the last note comes out while peeking. Once everything has
//! arrived, `advance` says nothing moves, so no more frames are drawn.

use wl_shell_ui::Spring;

/// The delay between one card starting to move and the next.
pub const STAGGER: f64 = 0.04;
/// Kept free at both ends of the column when it is full.
const MARGIN: f64 = 8.0;
/// Cards overlap like a deck when they don't fit, at least this far apart.
const MIN_STEP: f64 = 6.0;
/// How much farther than `peek` the note under the pointer comes out.
pub const LIFT: f64 = 14.0;

/// What the column is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Only the slivers show.
    #[default]
    Tucked,
    /// The notes are out far enough to read their titles.
    Peeking,
    /// This note is forward, grown; the others are tucked.
    Focused(String),
}

/// How a note looks: what its paper is rendered from.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// 0xRRGGBBAA.
    pub color: u32,
    /// Degrees.
    pub tilt: f64,
}

/// A note to show, by its key (its path in the notes directory).
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub key: String,
    pub look: Look,
}

/// The sizes the column is laid out with, logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// A sliver's length along the edge.
    pub tab: f64,
    /// Between slivers.
    pub gap: f64,
    /// How much of a tucked note shows.
    pub tuck: f64,
    /// How much of a peeking note shows.
    pub peek: f64,
    /// The `+` sliver's length along the edge.
    pub plus: f64,
}

#[derive(Clone, Debug)]
pub struct Card {
    pub key: String,
    pub look: Look,
    /// Its top along the edge.
    pub y: Spring,
    /// 0 out of sight, 1 in place.
    pub presence: Spring,
    /// How far it sticks out when in place.
    pub out: Spring,
    /// 0 in the column, 1 focused.
    pub focus: Spring,
    /// Seconds before `presence` starts toward its target.
    delay: f64,
    /// On its way out.
    pub leaving: bool,
}

impl Card {
    /// Shown, or on its way: it takes input.
    pub fn present(&self) -> bool {
        !self.leaving && self.presence.value > 0.5
    }
}

/// The tops of `n` slivers along an edge `length` long: centered, `gap`
/// apart, or overlapping evenly if they don't fit.
pub fn stack(n: usize, length: f64, m: &Metrics) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let total = n as f64 * m.tab + (n - 1) as f64 * m.gap;
    if total <= length - 2.0 * MARGIN || n == 1 {
        let start = ((length - total) / 2.0).max(0.0);
        return (0..n).map(|i| start + i as f64 * (m.tab + m.gap)).collect();
    }
    let step = ((length - 2.0 * MARGIN - m.tab) / (n - 1) as f64).max(MIN_STEP);
    (0..n).map(|i| MARGIN + i as f64 * step).collect()
}

/// A note's tilt, -`max` to `max` degrees, the same every time for the
/// same key, so the column looks hand-placed but doesn't change.
pub fn tilt(key: &str, max: f64) -> f64 {
    // FNV-1a.
    let mut h: u32 = 0x811c_9dc5;
    for b in key.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    (h as f64 / u32::MAX as f64 * 2.0 - 1.0) * max
}

#[derive(Clone, Debug)]
pub struct Deck {
    cards: Vec<Card>,
    /// The notes to show, in order.
    wanted: Vec<Item>,
    /// The edge's length.
    length: f64,
    metrics: Metrics,
    /// Spring response, ms.
    response: f64,
    /// Leaving cards are on their way out; the column closes up after.
    closing: bool,
    mode: Mode,
    /// The note under the pointer, lifted while peeking.
    hover: Option<String>,
    /// The `+` sliver: 0 out of sight, 1 peeking.
    pub plus: Spring,
}

impl Deck {
    pub fn new(metrics: Metrics, response: f64) -> Self {
        Self {
            cards: Vec::new(),
            wanted: Vec::new(),
            length: 0.0,
            metrics,
            response,
            closing: false,
            mode: Mode::Tucked,
            hover: None,
            plus: Spring::new(0.0, response),
        }
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    pub fn hover(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    /// Tuck, peek or focus a note: the cards head there.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode != self.mode {
            self.mode = mode;
            self.retarget();
        }
    }

    /// The note under the pointer (lifted while peeking).
    pub fn set_hover(&mut self, hover: Option<String>) {
        if hover != self.hover {
            self.hover = hover;
            self.retarget();
        }
    }

    /// Where `out` heads for the card of `key`.
    fn out_target(&self, key: &str) -> f64 {
        let m = &self.metrics;
        match &self.mode {
            Mode::Tucked => m.tuck,
            Mode::Peeking if self.hover.as_deref() == Some(key) => m.peek + LIFT,
            Mode::Peeking => m.peek,
            Mode::Focused(k) if k == key => m.peek,
            Mode::Focused(_) => m.tuck,
        }
    }

    /// Where `focus` heads for the card of `key`.
    fn focus_target(&self, key: &str) -> f64 {
        match &self.mode {
            Mode::Focused(k) if k == key => 1.0,
            _ => 0.0,
        }
    }

    /// Point every spring at where the mode wants it.
    fn retarget(&mut self) {
        let targets: Vec<(f64, f64)> = (self.cards.iter())
            .map(|c| (self.out_target(&c.key), self.focus_target(&c.key)))
            .collect();
        for (c, (out, focus)) in self.cards.iter_mut().zip(targets) {
            c.out.retarget(out);
            c.focus.retarget(if c.leaving { 0.0 } else { focus });
        }
        self.plus.retarget(match self.mode {
            Mode::Peeking => 1.0,
            _ => 0.0,
        });
    }

    /// The `+` sliver's top: right after the last note (centered without
    /// notes), within the column.
    pub fn plus_top(&self) -> f64 {
        let m = &self.metrics;
        let last = self.cards.iter().rev().find(|c| !c.leaving);
        let top = match last {
            Some(c) => c.y.value + m.tab + m.gap,
            None => (self.length - m.plus) / 2.0,
        };
        top.min(self.length - MARGIN - m.plus).max(0.0)
    }

    /// The cards, leaving ones included.
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// Lay the column out along an edge `length` long; the cards move to
    /// their new places (or are put there, the first time).
    pub fn resize(&mut self, length: f64) {
        if length == self.length {
            return;
        }
        let first = self.length == 0.0;
        self.length = length;
        self.relayout(first);
    }

    /// New sizes or springs, e.g. from the config.
    pub fn set_metrics(&mut self, metrics: Metrics, response: f64) {
        if metrics == self.metrics && response == self.response {
            return;
        }
        self.metrics = metrics;
        self.response = response;
        for c in &mut self.cards {
            c.y = respring(c.y, response);
            c.presence = respring(c.presence, response);
            c.out = respring(c.out, response);
            c.focus = respring(c.focus, response);
        }
        self.plus = respring(self.plus, response);
        self.relayout(false);
        self.retarget();
    }

    /// Move the cards that stay to their places among `wanted`.
    fn relayout(&mut self, snap: bool) {
        let tops = stack(self.wanted.len(), self.length, &self.metrics);
        for c in self.cards.iter_mut().filter(|c| !c.leaving) {
            if let Some(i) = self.wanted.iter().position(|w| w.key == c.key) {
                match snap {
                    true => c.y.snap(tops[i]),
                    false => c.y.retarget(tops[i]),
                }
            }
        }
    }

    /// Show `items`, in this order (see the module's comment).
    pub fn set(&mut self, items: Vec<Item>) {
        self.wanted = items;
        let mut rank = 0.0;
        for c in &mut self.cards {
            match self.wanted.iter().find(|i| i.key == c.key) {
                Some(item) => {
                    c.look = item.look.clone();
                    if c.leaving {
                        c.leaving = false;
                        c.delay = 0.0;
                        c.presence.retarget(1.0);
                    }
                }
                None if !c.leaving => {
                    c.leaving = true;
                    c.delay = rank * STAGGER;
                    rank += 1.0;
                    if c.delay == 0.0 {
                        c.presence.retarget(0.0);
                    }
                }
                None => {}
            }
        }
        self.closing = true;
        self.retarget();
        self.try_close_up();
    }

    /// Close up once no card is leaving anymore.
    fn try_close_up(&mut self) {
        let gone = |c: &Card| c.delay <= 0.0 && c.presence.target == 0.0 && c.presence.settled();
        if self.closing && self.cards.iter().all(|c| !c.leaving || gone(c)) {
            self.close_up();
        }
    }

    /// Drop the cards that left, move the others to their places, and
    /// bring in the new ones.
    fn close_up(&mut self) {
        self.closing = false;
        let mut old: Vec<Card> = self.cards.drain(..).filter(|c| !c.leaving).collect();
        let tops = stack(self.wanted.len(), self.length, &self.metrics);
        let mut rank = 0.0;
        for (item, top) in self.wanted.iter().zip(tops) {
            match old.iter().position(|c| c.key == item.key) {
                Some(i) => {
                    let mut c = old.swap_remove(i);
                    c.y.retarget(top);
                    self.cards.push(c);
                }
                None => {
                    let mut focus = Spring::new(0.0, self.response);
                    focus.retarget(self.focus_target(&item.key));
                    let mut c = Card {
                        key: item.key.clone(),
                        look: item.look.clone(),
                        y: Spring::new(top, self.response),
                        presence: Spring::new(0.0, self.response),
                        out: Spring::new(self.out_target(&item.key), self.response),
                        focus,
                        delay: rank * STAGGER,
                        leaving: false,
                    };
                    rank += 1.0;
                    if c.delay == 0.0 {
                        c.presence.retarget(1.0);
                    }
                    self.cards.push(c);
                }
            }
        }
    }

    /// Advance by `dt` seconds. Returns whether anything still moves.
    pub fn advance(&mut self, dt: f64) -> bool {
        for c in &mut self.cards {
            if c.delay > 0.0 {
                c.delay -= dt;
                if c.delay <= 0.0 {
                    c.delay = 0.0;
                    c.presence.retarget(if c.leaving { 0.0 } else { 1.0 });
                }
            }
            c.y.advance(dt);
            c.presence.advance(dt);
            c.out.advance(dt);
            c.focus.advance(dt);
        }
        self.plus.advance(dt);
        self.try_close_up();
        self.moving()
    }

    /// Whether anything moves (or waits to).
    pub fn moving(&self) -> bool {
        let still = |c: &Card| {
            c.delay <= 0.0
                && c.y.settled()
                && c.presence.settled()
                && c.out.settled()
                && c.focus.settled()
        };
        self.closing || !self.plus.settled() || !self.cards.iter().all(still)
    }
}

/// `s` with another response, where it is now.
fn respring(s: Spring, response: f64) -> Spring {
    let mut new = Spring::new(s.value, response);
    new.velocity = s.velocity;
    new.retarget(s.target);
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Metrics = Metrics {
        tab: 60.0,
        gap: 4.0,
        tuck: 10.0,
        peek: 180.0,
        plus: 30.0,
    };

    fn item(key: &str) -> Item {
        Item {
            key: key.into(),
            look: Look {
                color: 0xffff00ff,
                tilt: 0.0,
            },
        }
    }

    fn items(keys: &[&str]) -> Vec<Item> {
        keys.iter().map(|k| item(k)).collect()
    }

    fn keys(d: &Deck) -> Vec<&str> {
        d.cards().iter().map(|c| c.key.as_str()).collect()
    }

    fn card<'a>(d: &'a Deck, key: &str) -> &'a Card {
        d.cards().iter().find(|c| c.key == key).unwrap()
    }

    /// Advance until nothing moves; the frames it took.
    fn settle(d: &mut Deck) -> usize {
        for frame in 1..1000 {
            if !d.advance(1.0 / 60.0) {
                return frame;
            }
        }
        panic!("never settles");
    }

    #[test]
    fn stacks() {
        assert!(stack(0, 500.0, &M).is_empty());
        // Centered.
        assert_eq!(stack(1, 500.0, &M), [220.0]);
        assert_eq!(stack(2, 500.0, &M), [188.0, 252.0]);
        // Overlapping when they don't fit, within the margins.
        let tops = stack(10, 300.0, &M);
        assert_eq!(tops[0], MARGIN);
        assert!((tops[9] + M.tab - (300.0 - MARGIN)).abs() < 1e-9);
        // Never closer than MIN_STEP.
        let tops = stack(100, 300.0, &M);
        assert_eq!(tops[1] - tops[0], MIN_STEP);
    }

    #[test]
    fn tilts() {
        assert_eq!(tilt("a.md", 1.5), tilt("a.md", 1.5));
        assert_ne!(tilt("a.md", 1.5), tilt("b.md", 1.5));
        for k in ["a", "b", "work/c.md", ""] {
            assert!(tilt(k, 1.5).abs() <= 1.5);
        }
        assert_eq!(tilt("a.md", 0.0), 0.0);
    }

    #[test]
    fn notes_slide_out_and_then_rest() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a", "b"]));
        assert_eq!(keys(&d), ["a", "b"]);
        assert_eq!(card(&d, "a").presence.value, 0.0);
        assert!(d.moving());
        settle(&mut d);
        assert!(!d.moving());
        assert_eq!(card(&d, "a").y.value, 188.0);
        assert_eq!(card(&d, "b").presence.value, 1.0);
        // Nothing changing is no animation.
        d.set(items(&["a", "b"]));
        assert!(!d.advance(1.0 / 60.0));
    }

    #[test]
    fn leaving_first_then_closing_up() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a", "b", "c"]));
        settle(&mut d);
        let b_top = card(&d, "b").y.value;
        // b stays, a and c leave, d comes.
        d.set(items(&["b", "d"]));
        assert!(card(&d, "a").leaving && card(&d, "c").leaving);
        // Staggered: c waits for a.
        d.advance(0.01);
        assert!(card(&d, "a").presence.value < 1.0);
        assert_eq!(card(&d, "c").presence.value, 1.0);
        // While they leave, b stays put and d isn't there yet.
        for _ in 0..5 {
            d.advance(1.0 / 60.0);
            assert_eq!(card(&d, "b").y.value, b_top);
            assert!(d.cards().iter().all(|c| c.key != "d"));
        }
        settle(&mut d);
        assert_eq!(keys(&d), ["b", "d"]);
        assert_eq!(card(&d, "b").y.value, stack(2, 500.0, &M)[0]);
        assert_eq!(card(&d, "d").presence.value, 1.0);
    }

    #[test]
    fn coming_back_while_leaving() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a", "b"]));
        settle(&mut d);
        d.set(items(&["b"]));
        for _ in 0..3 {
            d.advance(1.0 / 60.0);
        }
        let partly = card(&d, "a").presence.value;
        assert!(partly > 0.0 && partly < 1.0);
        d.set(items(&["a", "b"]));
        assert!(!card(&d, "a").leaving);
        settle(&mut d);
        assert_eq!(keys(&d), ["a", "b"]);
        assert_eq!(card(&d, "a").presence.value, 1.0);
    }

    #[test]
    fn looks_change_in_place() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a"]));
        settle(&mut d);
        let mut recolored = item("a");
        recolored.look.color = 0xff0000ff;
        d.set(vec![recolored]);
        assert_eq!(card(&d, "a").look.color, 0xff0000ff);
        assert!(!d.advance(1.0 / 60.0));
    }

    #[test]
    fn resizing_moves_the_cards() {
        let mut d = Deck::new(M, 150.0);
        d.set(items(&["a"]));
        // The first size puts them in place.
        d.resize(500.0);
        assert_eq!(card(&d, "a").y.value, 220.0);
        d.resize(300.0);
        assert_eq!(card(&d, "a").y.target, 120.0);
        settle(&mut d);
        assert_eq!(card(&d, "a").y.value, 120.0);
    }

    #[test]
    fn peeking_lifting_and_focusing() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a", "b"]));
        settle(&mut d);
        assert_eq!(card(&d, "a").out.value, M.tuck);
        assert_eq!(d.plus.value, 0.0);
        d.set_mode(Mode::Peeking);
        d.set_hover(Some("b".into()));
        settle(&mut d);
        assert_eq!(card(&d, "a").out.value, M.peek);
        assert_eq!(card(&d, "b").out.value, M.peek + LIFT);
        assert_eq!(d.plus.value, 1.0);
        assert_eq!(d.plus_top(), card(&d, "b").y.value + M.tab + M.gap);
        d.set_mode(Mode::Focused("a".into()));
        settle(&mut d);
        assert_eq!(card(&d, "a").focus.value, 1.0);
        assert_eq!(card(&d, "b").out.value, M.tuck);
        assert_eq!(card(&d, "b").focus.value, 0.0);
        assert_eq!(d.plus.value, 0.0);
        d.set_mode(Mode::Tucked);
        settle(&mut d);
        assert_eq!(card(&d, "a").focus.value, 0.0);
        assert_eq!(card(&d, "a").out.value, M.tuck);
    }

    #[test]
    fn new_notes_come_in_focused() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a"]));
        settle(&mut d);
        // `wl-note new`: focused before its card exists.
        d.set_mode(Mode::Focused("n".into()));
        d.set(items(&["a", "n"]));
        assert_eq!(card(&d, "n").focus.target, 1.0);
        settle(&mut d);
        assert_eq!(card(&d, "n").focus.value, 1.0);
        assert_eq!(card(&d, "n").presence.value, 1.0);
    }
}
