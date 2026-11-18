//! The notes in a column: where they are along the edge, how far out,
//! and how that follows the pointer and the context. Pure, unit tested.
//!
//! The column is at one of two levels (`Level`): **markers** (each note a
//! small rounded marker, the pointer away) or **headers** (each note's
//! margin out, its title readable, the pointer at the edge). On top of
//! that, one note may be **revealed** (its whole sheet out, read-only,
//! while the pointer is on it) and one **open** (out until closed). A
//! card's role (`Role`) is the most its note is out for: sheet (revealed
//! or open) > lifted header (under the pointer) > header > marker.
//!
//! Markers and headers share one layout (`slots`): each note has a slot
//! center along the edge, and its marker and its header are centered on
//! it, so a marker grows into its header without jumping. Headers
//! shingle: later ones cover the tails of earlier ones, at a pitch of
//! `SHINGLE` of their length, tighter when crowded. Markers keep apart:
//! with more than fit, their gap shrinks first, down to `MIN_GAP`, then
//! their length, down to `MIN_MARKER`.
//!
//! A sheet (revealed or open) takes its whole height in the column
//! (`places`): it comes down from its header's top, so its title stays
//! where it was, and the notes after it and the `+` button move down to
//! below it, never hidden under it. If that runs off the output, the
//! column moves up as little as needed, then the headers' pitch shrinks.
//! Moving from a header that shows onto the next one, that one's sheet is
//! under the pointer, so browsing down the column never skips a note.
//!
//! Each card has springs: `y`, its center along the edge; `presence`,
//! from 0 (behind the edge, out of sight) to 1 (in place); `out`, how far
//! it is out of the edge; and `len`, its length along the edge. When the
//! notes change:
//!
//! 1. The notes that stay are left where they are.
//! 2. The notes that leave slide back behind the edge, staggered.
//! 3. Once they are gone, the column closes up: the notes that stay move
//!    to their new places, and the new ones slide out of the edge at
//!    theirs, staggered too.
//!
//! A note wanted again while it is still leaving comes back from where it
//! is. The open note keeps its place in the column whatever the context
//! does. Once everything has arrived, `advance` says nothing moves, so no
//! more frames are drawn.
//!
//! The pointer (`point`, from what the column finds under it, `On`): at
//! the edge (in the `band` along all of it) the column goes to header
//! level at once; resting on a margin asks for a reveal after
//! `reveal-delay` (`reveal_due`); once a sheet was revealed, other margins
//! reveal at once, as a menu bar does, until the column goes back to
//! markers (`hide_due`, after the pointer left it). Moving onto the strip
//! between headers turns a revealed sheet back into a header; moving from
//! a margin onto its own sheet keeps it out, as the sheet is one target
//! from margin to bottom.

use wl_shell_ui::Spring;

/// The delay between one card starting to move and the next.
pub const STAGGER: f64 = 0.04;
/// How much farther than `header` the margin under the pointer comes out.
pub const LIFT: f64 = 6.0;
/// Header level holds this far past a lifted header, so a pointer
/// drifting while reading the labels doesn't drop back to markers.
pub const HOLD: f64 = 6.0;
/// Headers shingle at this much of their length.
const SHINGLE: f64 = 0.7;
/// Crowded markers' gaps shrink to this, then their lengths to
/// `MIN_MARKER`.
const MIN_GAP: f64 = 2.0;
const MIN_MARKER: f64 = 12.0;
/// Between the last header and the `+` button.
const PLUS_GAP: f64 = 10.0;

/// How far out the column is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Markers only.
    #[default]
    Marker,
    /// Every note's margin, with its title.
    Header,
}

/// The most a note is out for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Marker,
    Header,
    /// The header under the pointer.
    Lifted,
    /// Revealed or open: the whole sheet.
    Sheet,
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
    pub marker_width: f64,
    pub marker_length: f64,
    pub marker_gap: f64,
    /// The edge band's width: the pointer in it, anywhere along the edge,
    /// brings the headers out (0: only the markers do).
    pub band: f64,
    /// Kept free at both ends of the column.
    pub padding: f64,
    /// How far a header is out.
    pub header: f64,
    pub header_length: f64,
    pub sheet_width: f64,
    pub sheet_height: f64,
    /// The `+` button's size.
    pub plus: f64,
}

/// The layout of `n` notes along an edge.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Slots {
    /// Each note's center along the edge.
    pub centers: Vec<f64>,
    /// The markers' length.
    pub marker: f64,
}

/// Lay out `n` notes along an edge `length` long: headers shingled and
/// centered, within the padding (tighter if they don't fit), markers on
/// the same centers (see the module's comment).
pub fn slots(n: usize, length: f64, m: &Metrics) -> Slots {
    let mut s = Slots {
        centers: Vec::new(),
        marker: m.marker_length,
    };
    if n == 0 {
        return s;
    }
    let avail = (length - 2.0 * m.padding).max(0.0);
    let gaps = (n - 1) as f64;
    let mut pitch = m.header_length * SHINGLE;
    if n > 1 && gaps * pitch + m.header_length > avail {
        pitch = ((avail - m.header_length) / gaps).max(1.0);
    }
    let span = gaps * pitch + m.header_length;
    let start = (length - span) / 2.0;
    s.centers = (0..n)
        .map(|i| start + m.header_length / 2.0 + i as f64 * pitch)
        .collect();
    if n > 1 && pitch - m.marker_length < MIN_GAP.min(m.marker_gap) {
        s.marker = (pitch - MIN_GAP).max(MIN_MARKER).min(m.marker_length);
    }
    s
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
pub struct Card {
    pub key: String,
    pub look: Look,
    /// Its center along the edge.
    pub y: Spring,
    /// Its slot's center.
    slot: f64,
    /// 0 out of sight, 1 in place.
    pub presence: Spring,
    /// How far it is out of the edge when in place.
    pub out: Spring,
    /// Its length along the edge.
    pub len: Spring,
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

/// What the pointer is on, as the column finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum On {
    /// Nothing of the column: it left.
    Outside,
    /// The edge band, between or beside the notes.
    Strip,
    /// A note's marker or header.
    Margin(String),
    /// A revealed or open note's sheet.
    Sheet(String),
    /// The `+` button.
    Plus,
}

#[derive(Clone, Debug)]
pub struct Deck {
    cards: Vec<Card>,
    /// The notes to show, in order.
    wanted: Vec<Item>,
    /// The edge's length.
    length: f64,
    metrics: Metrics,
    slots: Slots,
    /// Spring response, ms.
    response: f64,
    /// Leaving cards are on their way out; the column closes up after.
    closing: bool,
    level: Level,
    /// The margin under the pointer, lifted.
    hover: Option<String>,
    revealed: Option<String>,
    open: Option<String>,
    /// A sheet was revealed since the column came out: margins reveal at
    /// once.
    browsing: bool,
    /// The `+` button: 0 out of sight, 1 out.
    pub plus: Spring,
}

impl Deck {
    pub fn new(metrics: Metrics, response: f64) -> Self {
        Self {
            cards: Vec::new(),
            wanted: Vec::new(),
            length: 0.0,
            metrics,
            slots: Slots::default(),
            response,
            closing: false,
            level: Level::Marker,
            hover: None,
            revealed: None,
            open: None,
            browsing: false,
            plus: Spring::new(0.0, response),
        }
    }

    pub fn level(&self) -> Level {
        self.level
    }

    pub fn hover(&self) -> Option<&str> {
        self.hover.as_deref()
    }

    pub fn revealed(&self) -> Option<&str> {
        self.revealed.as_deref()
    }

    pub fn open(&self) -> Option<&str> {
        self.open.as_deref()
    }

    /// The markers' length now.
    pub fn marker_length(&self) -> f64 {
        self.slots.marker
    }

    /// Whether `key` is out as a sheet: revealed or open.
    pub fn held(&self, key: &str) -> bool {
        self.revealed.as_deref() == Some(key) || self.open.as_deref() == Some(key)
    }

    /// The most `key` is out for.
    pub fn role(&self, key: &str) -> Role {
        match () {
            _ if self.held(key) => Role::Sheet,
            _ if self.level == Level::Marker => Role::Marker,
            _ if self.hover.as_deref() == Some(key) => Role::Lifted,
            _ => Role::Header,
        }
    }

    /// How far out and how long a card in `role` is.
    pub fn size(&self, role: Role) -> (f64, f64) {
        let m = &self.metrics;
        match role {
            Role::Marker => (m.marker_width, self.slots.marker),
            Role::Header => (m.header, m.header_length),
            Role::Lifted => (m.header + LIFT, m.header_length),
            Role::Sheet => (m.sheet_width, m.sheet_height),
        }
    }

    /// The role of a card's springs as they are now: the largest it
    /// still is out for.
    pub fn role_now(&self, card: &Card) -> Role {
        let m = &self.metrics;
        match card.out.value {
            o if o > m.header + LIFT + 0.5 => Role::Sheet,
            o if o > m.header + 0.5 => Role::Lifted,
            o if o > m.marker_width + 0.5 => Role::Header,
            _ => Role::Marker,
        }
    }

    /// A sheet's center `y`, moved onto the output (centered on it if it
    /// is too short).
    fn fit(&self, y: f64) -> f64 {
        let (h, pad) = (self.metrics.sheet_height, self.metrics.padding);
        let (lo, hi) = (pad + h / 2.0, self.length - pad - h / 2.0);
        match lo <= hi {
            true => y.clamp(lo, hi),
            false => self.length / 2.0,
        }
    }

    /// Where the cards that stay center along the edge, in the column's
    /// order: on their slots while none is a sheet; else each sheet comes
    /// down from its header's top and what is after it goes below it (see
    /// the module's comment).
    fn places(&self) -> Vec<f64> {
        let m = &self.metrics;
        let (l, h) = (m.header_length, m.sheet_height);
        let cards: Vec<&Card> = self.cards.iter().filter(|c| !c.leaving).collect();
        let held: Vec<bool> = cards.iter().map(|c| self.held(&c.key)).collect();
        if !held.contains(&true) {
            return cards.iter().map(|c| c.slot).collect();
        }
        let n = cards.len();
        // From each card's top to the next one's: a sheet's height after a
        // sheet, the headers' pitch after a header.
        let pitch: Vec<f64> = (1..n)
            .map(|i| (cards[i].slot - cards[i - 1].slot).max(0.0))
            .collect();
        let sheets = (0..n - 1).filter(|&i| held[i]).count() as f64 * h;
        let headers: f64 = (0..n - 1).filter(|&i| !held[i]).map(|i| pitch[i]).sum();
        // The last card, and the `+` button after it.
        let tail = (if held[n - 1] { h } else { l }) + PLUS_GAP + m.plus;
        let avail = (self.length - 2.0 * m.padding).max(0.0);
        let fixed = sheets + tail;
        let squeeze = match headers > 0.0 && fixed + headers > avail {
            true => ((avail - fixed) / headers).clamp(0.0, 1.0),
            false => 1.0,
        };
        let span = fixed + headers * squeeze;
        let mut top = (cards[0].slot - l / 2.0)
            .min(self.length - m.padding - span)
            .max(m.padding);
        let mut places = Vec::with_capacity(n);
        for i in 0..n {
            places.push(match held[i] {
                true => self.fit(top + h / 2.0),
                false => top + l / 2.0,
            });
            if i + 1 < n {
                top += if held[i] { h } else { pitch[i] * squeeze };
            }
        }
        places
    }

    /// Point every spring at where the levels want it.
    fn retarget(&mut self) {
        let mut places = self.places().into_iter();
        let targets: Vec<(f64, f64, Option<f64>)> = (self.cards.iter())
            .map(|c| {
                let (out, len) = self.size(self.role(&c.key));
                let y = if c.leaving { None } else { places.next() };
                (out, len, y)
            })
            .collect();
        for (c, (out, len, y)) in self.cards.iter_mut().zip(targets) {
            c.out.retarget(out);
            c.len.retarget(len);
            if let Some(y) = y {
                c.y.retarget(y);
            }
        }
        self.plus.retarget(match self.level {
            Level::Header => 1.0,
            Level::Marker => 0.0,
        });
    }

    // -- the pointer -------------------------------------------------------

    /// The pointer is on `on`. Returns the note to reveal once the pointer
    /// rested on it for `reveal-delay` (call `reveal_due` then), if any.
    pub fn point(&mut self, on: On) -> Option<String> {
        let mut wait = None;
        match on {
            On::Outside => self.hover = None,
            On::Strip | On::Plus => {
                self.level = Level::Header;
                self.hover = None;
                self.revealed = None;
            }
            On::Margin(k) => {
                self.level = Level::Header;
                if self.browsing {
                    self.revealed = Some(k.clone());
                } else if self.revealed.as_deref() != Some(&k) {
                    wait = Some(k.clone());
                }
                self.hover = Some(k);
            }
            On::Sheet(k) => {
                self.level = Level::Header;
                self.hover = None;
                // Onto the open sheet: the revealed one goes back.
                if self.revealed.as_deref() != Some(&k) {
                    self.revealed = None;
                }
            }
        }
        self.retarget();
        wait
    }

    /// The pointer rested on `key`'s margin: reveal it, if it still is
    /// there.
    pub fn reveal_due(&mut self, key: &str) {
        if self.level == Level::Header
            && self.hover.as_deref() == Some(key)
            && self.open.as_deref() != Some(key)
        {
            self.revealed = Some(key.to_string());
            self.browsing = true;
            self.retarget();
        }
    }

    /// The pointer left the column a while ago: back to markers. The open
    /// note stays.
    pub fn hide_due(&mut self) {
        self.level = Level::Marker;
        self.hover = None;
        self.revealed = None;
        self.browsing = false;
        self.retarget();
    }

    /// Open `key` (before its card exists, too): it stays out until
    /// closed.
    pub fn open_note(&mut self, key: &str) {
        self.open = Some(key.to_string());
        if self.revealed.as_deref() == Some(key) {
            self.revealed = None;
        }
        if self.hover.as_deref() == Some(key) {
            self.hover = None;
        }
        self.retarget();
    }

    pub fn close(&mut self) {
        if self.open.take().is_some() {
            self.retarget();
        }
    }

    // -- the layout --------------------------------------------------------

    /// The `+` button's center along the edge: past the last note, or
    /// below a sheet reaching farther (centered without notes), within the
    /// column.
    pub fn plus_center(&self) -> f64 {
        let m = &self.metrics;
        let center = match self.extent() {
            Some((_, bottom)) => bottom + PLUS_GAP + m.plus / 2.0,
            None => self.length / 2.0,
        };
        center
            .min(self.length - m.padding - m.plus / 2.0)
            .max(m.plus / 2.0)
    }

    /// What the notes that stay span along the edge where they are going,
    /// as (top, bottom): their headers, or sheets.
    fn extent(&self) -> Option<(f64, f64)> {
        let m = &self.metrics;
        (self.cards.iter().filter(|c| !c.leaving))
            .map(|c| {
                let half = match self.held(&c.key) {
                    true => m.sheet_height / 2.0,
                    false => m.header_length / 2.0,
                };
                (c.y.target - half, c.y.target + half)
            })
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    }

    /// The edge bands, as (top, bottom, width out of the edge): the
    /// pointer in one is at the column. One is `band` wide along the whole
    /// edge; at header level, another is along the notes and over the `+`
    /// button, as wide as a lifted header and a little more (the
    /// hysteresis).
    pub fn bands(&self) -> Vec<(f64, f64, f64)> {
        let m = &self.metrics;
        let mut bands = Vec::new();
        if m.band > 0.0 && self.length > 0.0 {
            bands.push((0.0, self.length, m.band));
        }
        if self.level == Level::Header {
            let plus = self.plus_center();
            let (top, bottom) = self.extent().unwrap_or((plus, plus));
            let (top, bottom) = (
                top.min(plus - m.plus / 2.0),
                bottom.max(plus + m.plus / 2.0),
            );
            bands.push((top, bottom, m.header + LIFT + HOLD));
        }
        bands
    }

    /// The cards, leaving ones included, in the column's order.
    pub fn cards(&self) -> &[Card] {
        &self.cards
    }

    /// The cards bottom first, as they are painted (and hit-tested,
    /// topmost first): the column's order (later on top), then the open
    /// note, then the revealed one.
    pub fn paint_order(&self) -> Vec<&Card> {
        let rank = |c: &Card| match () {
            _ if self.revealed.as_deref() == Some(c.key.as_str()) => 2,
            _ if self.open.as_deref() == Some(c.key.as_str()) => 1,
            _ => 0,
        };
        let mut order: Vec<&Card> = self.cards.iter().collect();
        // Stable: the column's order within each rank.
        order.sort_by_key(|c| rank(c));
        order
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
            for s in [&mut c.y, &mut c.presence, &mut c.out, &mut c.len] {
                *s = respring(*s, response);
            }
        }
        self.plus = respring(self.plus, response);
        self.relayout(false);
    }

    /// Move the cards that stay to their places among `wanted`.
    fn relayout(&mut self, snap: bool) {
        self.slots = slots(self.wanted.len(), self.length, &self.metrics);
        for c in self.cards.iter_mut().filter(|c| !c.leaving) {
            if let Some(i) = self.wanted.iter().position(|w| w.key == c.key) {
                c.slot = self.slots.centers[i];
            }
        }
        self.retarget();
        if snap {
            for c in &mut self.cards {
                c.y.snap(c.y.target);
                c.len.snap(c.len.target);
            }
        }
    }

    /// Show `items`, in this order (see the module's comment). The open
    /// note keeps its place among them; the revealed and hovered ones go
    /// if they aren't among them.
    pub fn set(&mut self, mut items: Vec<Item>) {
        if let Some(open) = &self.open
            && let Some(i) = items.iter().position(|i| i.key == *open)
            && let Some(was) = self.wanted.iter().position(|w| w.key == *open)
        {
            let item = items.remove(i);
            items.insert(was.min(items.len()), item);
        }
        self.wanted = items;
        let has = |k: &Option<String>| {
            k.as_ref()
                .is_none_or(|k| self.wanted.iter().any(|w| w.key == *k))
        };
        if !has(&self.open) {
            self.open = None;
        }
        if !has(&self.revealed) {
            self.revealed = None;
        }
        if !has(&self.hover) {
            self.hover = None;
        }
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
        self.slots = slots(self.wanted.len(), self.length, &self.metrics);
        let mut rank = 0.0;
        let mut new = Vec::new();
        let wanted = self.wanted.clone();
        for (item, slot) in wanted.iter().zip(self.slots.centers.clone()) {
            match old.iter().position(|c| c.key == item.key) {
                Some(i) => {
                    let mut c = old.swap_remove(i);
                    c.slot = slot;
                    self.cards.push(c);
                }
                None => {
                    let role = self.role(&item.key);
                    let (out, len) = self.size(role);
                    let r = self.response;
                    let mut c = Card {
                        key: item.key.clone(),
                        look: item.look.clone(),
                        y: Spring::new(slot, r),
                        slot,
                        presence: Spring::new(0.0, r),
                        out: Spring::new(out, r),
                        len: Spring::new(len, r),
                        delay: rank * STAGGER,
                        leaving: false,
                    };
                    rank += 1.0;
                    if c.delay == 0.0 {
                        c.presence.retarget(1.0);
                    }
                    new.push(item.key.clone());
                    self.cards.push(c);
                }
            }
        }
        self.retarget();
        // The new ones come out of the edge where they go.
        for c in self.cards.iter_mut().filter(|c| new.contains(&c.key)) {
            c.y.snap(c.y.target);
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
            c.len.advance(dt);
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
                && c.len.settled()
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
        marker_width: 6.0,
        marker_length: 28.0,
        marker_gap: 8.0,
        band: 2.0,
        padding: 12.0,
        header: 28.0,
        header_length: 100.0,
        sheet_width: 300.0,
        sheet_height: 220.0,
        plus: 22.0,
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

    fn deck(keys: &[&str]) -> Deck {
        let mut d = Deck::new(M, 150.0);
        d.resize(1000.0);
        d.set(items(keys));
        settle(&mut d);
        d
    }

    fn margin(k: &str) -> On {
        On::Margin(k.into())
    }

    #[test]
    fn slots_shingle_headers_and_share_their_centers() {
        assert!(slots(0, 500.0, &M).centers.is_empty());
        assert_eq!(slots(1, 500.0, &M).centers, [250.0]);
        // Shingled at 70% of a header's length, centered.
        let s = slots(3, 1000.0, &M);
        assert_eq!(s.centers, [430.0, 500.0, 570.0]);
        assert_eq!(s.marker, M.marker_length);
        // Crowded: tighter, within the padding.
        let s = slots(6, 300.0, &M);
        let pitch = s.centers[1] - s.centers[0];
        assert!((pitch - (276.0 - 100.0) / 5.0).abs() < 1e-9);
        assert!((s.centers[0] - 50.0 - M.padding).abs() < 1e-9);
        assert!((s.centers[5] + 50.0 - (300.0 - M.padding)).abs() < 1e-9);
        // Markers keep apart: the gap shrinks first, then the length.
        assert!(pitch - s.marker >= 2.0 - 1e-9);
        let roomy = Metrics {
            header_length: 60.0,
            ..M
        };
        let s = slots(4, 400.0, &roomy);
        let pitch = s.centers[1] - s.centers[0];
        assert_eq!((pitch.round(), s.marker), (42.0, 28.0));
        let s = slots(7, 200.0, &roomy);
        let pitch = s.centers[1] - s.centers[0];
        assert!(pitch < 30.0);
        assert!(
            (s.marker - (pitch - 2.0)).abs() < 1e-9,
            "{pitch} {}",
            s.marker
        );
        let s = slots(40, 200.0, &roomy);
        assert_eq!(s.marker, 12.0);
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
    fn notes_slide_out_as_markers_and_then_rest() {
        let mut d = Deck::new(M, 150.0);
        d.resize(1000.0);
        d.set(items(&["a", "b"]));
        assert_eq!(keys(&d), ["a", "b"]);
        assert_eq!(card(&d, "a").presence.value, 0.0);
        assert!(d.moving());
        settle(&mut d);
        assert!(!d.moving());
        assert_eq!(card(&d, "a").y.value, 465.0);
        assert_eq!(card(&d, "a").out.value, M.marker_width);
        assert_eq!(card(&d, "a").len.value, M.marker_length);
        assert_eq!(card(&d, "b").presence.value, 1.0);
        // Nothing changing is no animation.
        d.set(items(&["a", "b"]));
        assert!(!d.advance(1.0 / 60.0));
    }

    #[test]
    fn levels_set_the_targets() {
        let mut d = deck(&["a", "b", "c"]);
        assert_eq!(d.level(), Level::Marker);
        assert_eq!(d.plus.value, 0.0);
        // At the edge: headers at once, no reveal yet.
        assert_eq!(d.point(On::Strip), None);
        settle(&mut d);
        assert_eq!(d.level(), Level::Header);
        assert_eq!(card(&d, "a").out.value, M.header);
        assert_eq!(card(&d, "a").len.value, M.header_length);
        assert_eq!(d.plus.value, 1.0);
        // On a margin: it lifts, and is revealed after the delay.
        assert_eq!(d.point(margin("b")), Some("b".into()));
        settle(&mut d);
        assert_eq!(d.role("b"), Role::Lifted);
        assert_eq!(card(&d, "b").out.value, M.header + LIFT);
        d.reveal_due("b");
        settle(&mut d);
        assert_eq!(d.role("b"), Role::Sheet);
        assert_eq!(
            (card(&d, "b").out.value, card(&d, "b").len.value),
            (300.0, 220.0)
        );
        // The neighbors stay headers: one sheet moves.
        assert_eq!(d.role("a"), Role::Header);
        assert_eq!(card(&d, "a").out.value, M.header);
        // Away a while: markers again.
        d.point(On::Outside);
        d.hide_due();
        settle(&mut d);
        assert_eq!(d.revealed(), None);
        assert_eq!(card(&d, "b").out.value, M.marker_width);
        assert_eq!(d.plus.value, 0.0);
    }

    #[test]
    fn revealing_waits_for_the_pointer_to_rest() {
        let mut d = deck(&["a", "b"]);
        d.point(margin("a"));
        // Moved on before the delay: nothing is revealed.
        d.point(margin("b"));
        d.reveal_due("a");
        assert_eq!(d.revealed(), None);
        d.reveal_due("b");
        assert_eq!(d.revealed(), Some("b"));
    }

    #[test]
    fn revealed_sheets_swap_without_a_delay() {
        let mut d = deck(&["a", "b", "c"]);
        d.point(margin("a"));
        d.reveal_due("a");
        assert_eq!(d.point(margin("c")), None);
        assert_eq!(d.revealed(), Some("c"));
        // Onto its own sheet: it stays.
        d.point(On::Sheet("c".into()));
        assert_eq!(d.revealed(), Some("c"));
        // Onto the strip: back to a header, but browsing goes on.
        d.point(On::Strip);
        assert_eq!(d.revealed(), None);
        assert_eq!(d.role("c"), Role::Header);
        assert_eq!(d.point(margin("a")), None);
        assert_eq!(d.revealed(), Some("a"));
        // Back at markers, the delay is back.
        d.hide_due();
        assert_eq!(d.point(margin("b")), Some("b".into()));
        assert_eq!(d.revealed(), None);
    }

    #[test]
    fn the_band_runs_along_the_edge_and_holds_header_level() {
        let mut d = deck(&["a", "b"]);
        // At markers: the whole edge, `band` wide.
        assert_eq!(d.bands(), [(0.0, 1000.0, M.band)]);
        // At headers, also the column, wider.
        d.point(On::Strip);
        let bands = d.bands();
        assert_eq!(bands.len(), 2);
        let (top, bottom, width) = bands[1];
        assert_eq!(width, M.header + LIFT + HOLD);
        assert_eq!(top, 415.0);
        assert!(bottom >= d.plus_center() + M.plus / 2.0);
        assert!(Deck::new(M, 150.0).bands().is_empty());
        // Without a band, only the markers take the pointer.
        let mut d = Deck::new(Metrics { band: 0.0, ..M }, 150.0);
        d.resize(1000.0);
        assert!(d.bands().is_empty());
    }

    /// Where `key`'s header or sheet is, along the edge, once settled.
    fn span(d: &Deck, key: &str) -> (f64, f64) {
        let c = card(d, key);
        (c.y.value - c.len.value / 2.0, c.y.value + c.len.value / 2.0)
    }

    #[test]
    fn sheets_push_what_comes_after_them_down() {
        let mut d = deck(&["a", "b", "c", "d"]);
        d.point(On::Strip);
        settle(&mut d);
        let before: Vec<(f64, f64)> = ["a", "b"].iter().map(|k| span(&d, k)).collect();
        d.point(margin("b"));
        d.reveal_due("b");
        settle(&mut d);
        // Above it, nothing moves; it comes down from its header's top.
        assert_eq!(span(&d, "a"), before[0]);
        assert_eq!(span(&d, "b").0, before[1].0);
        assert_eq!(span(&d, "b").1 - span(&d, "b").0, M.sheet_height);
        // The next header starts below it, and keeps its pitch to the one
        // after.
        assert_eq!(span(&d, "c").0, span(&d, "b").1);
        assert_eq!(span(&d, "d").0 - span(&d, "c").0, 70.0);
        // The `+` button too.
        d.point(margin("d"));
        settle(&mut d);
        assert_eq!(d.revealed(), Some("d"));
        assert!(d.plus_center() - M.plus / 2.0 >= span(&d, "d").1);
        // Back to headers: back on their slots.
        d.point(On::Strip);
        settle(&mut d);
        assert_eq!(span(&d, "b"), before[1]);
    }

    #[test]
    fn browsing_down_lands_on_the_next_sheet() {
        let mut d = deck(&["a", "b", "c", "d"]);
        d.point(margin("b"));
        d.reveal_due("b");
        settle(&mut d);
        // What shows of c: down to d's top.
        let (top, bottom) = (span(&d, "c").0, span(&d, "d").0);
        d.point(margin("c"));
        settle(&mut d);
        let (sheet_top, sheet_bottom) = span(&d, "c");
        assert!(sheet_top <= top && bottom <= sheet_bottom);
    }

    #[test]
    fn sheets_near_the_end_move_the_column_up() {
        let mut d = Deck::new(M, 150.0);
        d.resize(500.0);
        d.set(items(&["a", "b", "c"]));
        settle(&mut d);
        d.point(margin("c"));
        d.reveal_due("c");
        settle(&mut d);
        // The sheet and the `+` button fit, and the headers still show.
        assert!(span(&d, "c").1 + PLUS_GAP + M.plus <= 500.0 - M.padding + 1e-9);
        assert!(span(&d, "a").0 >= M.padding - 1e-9);
        assert!(span(&d, "b").0 < span(&d, "c").0);
        assert!(span(&d, "a").0 < span(&d, "b").0);
    }

    #[test]
    fn sheets_fit_on_the_output() {
        let mut d = Deck::new(M, 150.0);
        d.resize(400.0);
        d.set(items(&["a", "b", "c", "d"]));
        settle(&mut d);
        let first = card(&d, "a").y.value;
        d.point(margin("a"));
        d.reveal_due("a");
        settle(&mut d);
        let a = card(&d, "a");
        assert!(a.y.value > first);
        assert_eq!(a.y.value - 110.0, M.padding);
        // Too short for a sheet: centered.
        d.resize(200.0);
        settle(&mut d);
        assert_eq!(card(&d, "a").y.value, 100.0);
    }

    #[test]
    fn the_open_note_stays_through_everything() {
        let mut d = deck(&["a", "b", "c"]);
        d.point(margin("b"));
        d.reveal_due("b");
        d.open_note("b");
        assert_eq!((d.open(), d.revealed()), (Some("b"), None));
        settle(&mut d);
        let slot = card(&d, "b").slot;
        // The pointer leaves: the rest go back to markers, it stays.
        d.point(On::Outside);
        d.hide_due();
        settle(&mut d);
        assert_eq!(d.role("b"), Role::Sheet);
        assert_eq!(d.role("a"), Role::Marker);
        // Another note revealed over it, which paints on top.
        d.point(margin("c"));
        d.reveal_due("c");
        let order: Vec<&str> = d.paint_order().iter().map(|c| c.key.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
        d.point(margin("a"));
        let order: Vec<&str> = d.paint_order().iter().map(|c| c.key.as_str()).collect();
        assert_eq!(order, ["c", "b", "a"]);
        // The context changes; it doesn't belong to the new one, so the
        // program keeps it (appended), and it keeps its slot.
        d.set(items(&["a", "c", "b"]));
        settle(&mut d);
        assert_eq!(keys(&d), ["a", "b", "c"]);
        assert_eq!(card(&d, "b").slot, slot);
        assert_eq!(d.open(), Some("b"));
        // Closed, it is a note like the others.
        d.close();
        settle(&mut d);
        assert_eq!(d.role("b"), Role::Header);
        // A note gone closes.
        d.open_note("c");
        d.set(items(&["a"]));
        assert_eq!(d.open(), None);
    }

    #[test]
    fn new_notes_come_in_open() {
        let mut d = deck(&["a"]);
        // `wl-note new`: open before its card exists.
        d.open_note("n");
        d.set(items(&["a", "n"]));
        settle(&mut d);
        assert_eq!(card(&d, "n").out.value, M.sheet_width);
        assert_eq!(card(&d, "n").presence.value, 1.0);
        assert_eq!(d.role_now(card(&d, "n")), Role::Sheet);
        assert_eq!(d.role_now(card(&d, "a")), Role::Marker);
    }

    #[test]
    fn leaving_first_then_closing_up() {
        let mut d = deck(&["a", "b", "c"]);
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
        assert_eq!(card(&d, "b").y.value, slots(2, 1000.0, &M).centers[0]);
        assert_eq!(card(&d, "d").presence.value, 1.0);
    }

    #[test]
    fn coming_back_while_leaving() {
        let mut d = deck(&["a", "b"]);
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
        let mut d = deck(&["a"]);
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
        assert_eq!(card(&d, "a").y.value, 250.0);
        d.resize(300.0);
        assert_eq!(card(&d, "a").y.target, 150.0);
        settle(&mut d);
        assert_eq!(card(&d, "a").y.value, 150.0);
    }

    #[test]
    fn the_plus_follows_the_last_note() {
        let mut d = deck(&["a", "b"]);
        d.point(On::Strip);
        let b = card(&d, "b").slot;
        assert_eq!(d.plus_center(), b + 50.0 + PLUS_GAP + 11.0);
        assert_eq!(Deck::new(M, 150.0).plus_center(), M.plus / 2.0);
    }
}
