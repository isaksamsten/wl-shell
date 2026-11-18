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
//! Markers and headers have separate layouts (`slots`). At rest, the
//! marker length and gap set the pitch. When hovered, the cards grow into
//! shingled headers, at a pitch of `SHINGLE` of the header length. When
//! crowded, headers shingle more tightly; markers shrink their gap first
//! (to `MIN_GAP` or the configured gap if smaller), then their length (to
//! `MIN_MARKER` or the configured length if smaller).
//!
//! The column shows global, app, then window scratch notes, `group-gap`
//! apart (at header level, scaled by their pitch). The groups are
//! centered together; empty groups take no room. Crowding counts all
//! groups together. Each group
//! has a backdrop (`Backdrop`) behind its markers, on springs along its
//! extent, fading in with its first note and out with its last, and out
//! at header level.
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
//! 2. The notes that leave slide back behind the edge, staggered; a
//!    hidden one (`crumple`) shrinks and fades into it instead.
//! 3. Once they are gone, the column closes up: the notes that stay move
//!    to their new places, and the new ones slide out of the edge at
//!    theirs, staggered too.
//!
//! A note wanted again while it is still leaving comes back from where it
//! is. (The program keeps the open note in its place in the column
//! whatever the context does.) Once everything has arrived, `advance`
//! says nothing moves, so no more frames are drawn.
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
//!
//! Dragging a margin at header level (`drag_start`, `drag_to`,
//! `drag_end`) moves it with the pointer, lifted and on top, while the
//! others move apart to leave a gap where it would land (reorder.rs). The
//! gap opens in the note's own group, or in the other one if it can go
//! there (`Item::cross`: the program pins or unpins it then). The drop
//! says where; the program writes the new order and shows it.

use std::ops::Range;

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

/// The column's two groups, in their order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Group {
    /// Shown everywhere.
    #[default]
    Global,
    /// Shown for the context's app or matching its title.
    Window,
    /// Scratch notes for this particular toplevel.
    Scratch,
}

impl Group {
    pub const ALL: [Self; 3] = [Self::Global, Self::Window, Self::Scratch];

    fn destinations(self) -> &'static [Self] {
        match self {
            Self::Global => &[Self::Window],
            Self::Window => &[Self::Global],
            Self::Scratch => &[Self::Global, Self::Window],
        }
    }
}
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
    pub group: Group,
    /// It can be dragged into the other group (pinned or unpinned).
    pub cross: bool,
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
    /// Between the groups at marker level (scaled at header level).
    pub group_gap: f64,
    /// The groups' backdrops stay at header level.
    pub backdrop_headers: bool,
}

/// The layouts of the notes along an edge.
#[derive(Clone, Debug, Default, PartialEq)]
struct Slots {
    markers: Vec<f64>,
    headers: Vec<f64>,
    marker_length: f64,
    ranges: Vec<Range<usize>>,
    /// The extra room between the groups, at markers and at headers.
    between: [f64; 2],
}

impl Slots {
    fn range(&self, group: Group) -> Range<usize> {
        self.ranges.get(group as usize).cloned().unwrap_or(0..0)
    }
}

/// Lay out `groups` notes in group order, with gaps between nonempty groups:
/// each level independently, centered as a whole (see the module's
/// comment). Headers keep clear of the `+` button in the bottom corner:
/// they move up as little as needed, then shingle more tightly.
fn slots(groups: impl AsRef<[usize]>, length: f64, m: &Metrics) -> Slots {
    let groups = groups.as_ref();
    let n = groups.iter().sum::<usize>();
    let breaks = groups.iter().filter(|&&g| g > 0).count().saturating_sub(1) as f64;
    let group_gap = if breaks > 0.0 { m.group_gap } else { 0.0 };
    // At headers, the gap is as much more as their pitch is.
    let per_pitch = group_gap / (m.marker_length + m.marker_gap).max(1.0);
    let mut marker_length = m.marker_length;
    let mut marker_gap = m.marker_gap;
    let avail = (length - 2.0 * m.padding).max(0.0);
    let gaps = n.saturating_sub(1) as f64;
    if n > 1 {
        let room = (avail - breaks * group_gap).max(0.0);
        marker_gap =
            ((room - n as f64 * marker_length) / gaps).clamp(MIN_GAP.min(marker_gap), marker_gap);
        marker_length = ((room - gaps * marker_gap) / n as f64)
            .clamp(MIN_MARKER.min(marker_length), marker_length);
    }
    // A layout of cards `len` long at `pitch`, `fixed` plus `rel` pitches
    // between the groups; the room between them.
    let centers = |len: f64, mut pitch: f64, fixed: f64, rel: f64, reserve: f64| {
        let avail = (avail - reserve).max(0.0);
        let span = |pitch: f64| gaps * pitch + breaks * (fixed + rel * pitch) + len;
        if n > 1 && span(pitch) > avail {
            pitch = ((avail - len - breaks * fixed) / (gaps + breaks * rel)).max(1.0);
        }
        let between = fixed + rel * pitch;
        let first = (length / 2.0 - (span(pitch) - len) / 2.0)
            .min(length - m.padding - reserve - span(pitch) + len / 2.0)
            .max(m.padding + len / 2.0);
        let at = groups
            .iter()
            .filter(|&&g| g > 0)
            .enumerate()
            .flat_map(|(group, &count)| (0..count).map(move |_| group as f64 * between));
        (
            at.enumerate()
                .map(|(i, extra)| first + i as f64 * pitch + extra)
                .collect(),
            between,
        )
    };
    let (markers, marker_between) = centers(
        marker_length,
        marker_length + marker_gap,
        group_gap,
        0.0,
        0.0,
    );
    let (headers, header_between) = centers(
        m.header_length,
        m.header_length * SHINGLE,
        0.0,
        per_pitch,
        PLUS_GAP + m.plus,
    );
    Slots {
        markers,
        headers,
        marker_length,
        ranges: groups
            .iter()
            .scan(0, |start, count| {
                let range = *start..*start + count;
                *start = range.end;
                Some(range)
            })
            .collect(),
        between: [marker_between, header_between],
    }
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
    /// Its slot centers at marker and header levels, respectively.
    marker_slot: f64,
    header_slot: f64,
    /// The group its slots are in.
    pub group: Group,
    /// It can be dragged into the other group.
    cross: bool,
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
    /// Hidden: it crumples into the edge as it leaves, rather than
    /// sliding behind it.
    pub crumple: bool,
}

impl Card {
    /// Shown, or on its way: it takes input.
    pub fn present(&self) -> bool {
        !self.leaving && self.presence.value > 0.5
    }

    /// Crumpling into the edge.
    pub fn crumpling(&self) -> bool {
        self.leaving && self.crumple
    }
}

/// The springs of a group's backdrop: its extent along the edge (its
/// cards', without padding) and how much it shows.
#[derive(Clone, Copy, Debug)]
pub struct Backdrop {
    pub top: Spring,
    pub bottom: Spring,
    /// 0 out of sight, 1 shown.
    pub alpha: Spring,
}

impl Backdrop {
    fn new(response: f64) -> Self {
        Self {
            top: Spring::new(0.0, response),
            bottom: Spring::new(0.0, response),
            alpha: Spring::new(0.0, response),
        }
    }

    fn springs(&mut self) -> [&mut Spring; 3] {
        [&mut self.top, &mut self.bottom, &mut self.alpha]
    }

    fn settled(&self) -> bool {
        self.top.settled() && self.bottom.settled() && self.alpha.settled()
    }
}

/// A margin being dragged along the column.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    key: String,
    /// Where its center is along the edge.
    y: f64,
    /// The group it was in, and the one it would land in.
    from: Group,
    group: Group,
    /// The slot it would land in, of the column as it would be then.
    gap: usize,
    /// That column's layout.
    slots: Slots,
}

/// Where a dragged margin was dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Landing {
    pub key: String,
    /// The group it lands in.
    pub group: Group,
    /// The slot it lands in, among that group's notes (it included).
    pub gap: usize,
    /// It came from the other group.
    pub moved: bool,
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
    drag: Option<Drag>,
    /// The `+` button: 0 out of sight, 1 out.
    pub plus: Spring,
    /// The groups' backdrops, by `Group`.
    backdrops: [Backdrop; Group::ALL.len()],
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
            drag: None,
            plus: Spring::new(0.0, response),
            backdrops: [Backdrop::new(response); Group::ALL.len()],
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
        self.slots.marker_length
    }

    /// Whether `key` is out as a sheet: revealed or open.
    pub fn held(&self, key: &str) -> bool {
        self.revealed.as_deref() == Some(key) || self.open.as_deref() == Some(key)
    }

    /// The note whose margin is being dragged.
    pub fn dragging(&self) -> Option<&str> {
        self.drag.as_ref().map(|d| d.key.as_str())
    }

    /// The groups' backdrops, by `Group` (`Global` first).
    pub fn backdrops(&self) -> &[Backdrop; Group::ALL.len()] {
        &self.backdrops
    }

    /// Whether a drop on the edge strip is within the scratch group's extent.
    pub fn over_scratch(&self, y: f64) -> bool {
        let mut extents = self
            .cards
            .iter()
            .filter(|c| !c.leaving && c.group == Group::Scratch)
            .map(|c| (c.y.value - c.len.value / 2.0, c.y.value + c.len.value / 2.0));
        extents.next().is_some_and(|first| {
            let (top, bottom) = extents.fold(first, |a, b| (a.0.min(b.0), a.1.max(b.1)));
            (top..=bottom).contains(&y)
        })
    }

    /// Preserve a card's springs when keeping its scratch file under a new key.
    pub fn rename(&mut self, from: &str, to: &str) {
        for key in self
            .cards
            .iter_mut()
            .map(|c| &mut c.key)
            .chain(self.wanted.iter_mut().map(|w| &mut w.key))
            .chain(self.open.iter_mut())
            .chain(self.revealed.iter_mut())
            .chain(self.hover.iter_mut())
        {
            if key == from {
                to.clone_into(key);
            }
        }
    }

    /// The group `card` is laid out in: a dragged one's is where it would
    /// land.
    fn group_of(&self, card: &Card) -> Group {
        match &self.drag {
            Some(d) if d.key == card.key => d.group,
            _ => card.group,
        }
    }

    /// How many notes each group has, of those to show.
    fn counts(&self) -> [usize; Group::ALL.len()] {
        let mut counts = [0; Group::ALL.len()];
        for w in &self.wanted {
            counts[w.group as usize] += 1;
        }
        counts
    }

    /// The most `key` is out for.
    pub fn role(&self, key: &str) -> Role {
        match () {
            _ if self.held(key) => Role::Sheet,
            _ if self.dragging() == Some(key) => Role::Lifted,
            _ if self.level == Level::Marker => Role::Marker,
            _ if self.hover.as_deref() == Some(key) => Role::Lifted,
            _ => Role::Header,
        }
    }

    /// How far out and how long a card in `role` is.
    pub fn size(&self, role: Role) -> (f64, f64) {
        let m = &self.metrics;
        match role {
            Role::Marker => (m.marker_width, self.marker_length()),
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
    /// the module's comment). A dragged margin is where the pointer has
    /// it, and the others leave its gap free.
    fn places(&self) -> Vec<f64> {
        let m = &self.metrics;
        let l = match self.level {
            Level::Marker => self.marker_length(),
            Level::Header => m.header_length,
        };
        let h = m.sheet_height;
        let live: Vec<&Card> = self.cards.iter().filter(|c| !c.leaving).collect();
        let n = live.len();
        if n == 0 {
            return Vec::new();
        }
        // The order laid out, as indices into `live`, and each position's
        // slot: the column's, or with a drag, the dragged one in its gap.
        let level = match self.level {
            Level::Marker => 0,
            Level::Header => 1,
        };
        let mut order: Vec<usize> = (0..n).collect();
        let mut slot: Vec<f64> = live
            .iter()
            .map(|c| match self.level {
                Level::Marker => c.marker_slot,
                Level::Header => c.header_slot,
            })
            .collect();
        let mut between = self.slots.between[level];
        let drag = (self.drag.as_ref())
            .filter(|d| self.slots.headers.len() == n && d.slots.headers.len() == n)
            .and_then(|d| Some((live.iter().position(|c| c.key == d.key)?, d)));
        if let Some((i, d)) = drag {
            order.remove(i);
            order.insert(d.gap.min(n - 1), i);
            slot = d.slots.headers.clone();
            between = d.slots.between[1];
        }
        let cards: Vec<&Card> = order.iter().map(|&i| live[i]).collect();
        let held: Vec<bool> = cards.iter().map(|c| self.held(&c.key)).collect();
        // Whether the second group starts after each position.
        let parts: Vec<bool> = (cards.windows(2))
            .map(|w| self.group_of(w[0]) != self.group_of(w[1]))
            .collect();
        let mut places = Vec::with_capacity(n);
        if !held.contains(&true) {
            places = slot.clone();
        } else {
            // From each card's top to the next one's: a sheet's height
            // (and the groups' gap) after a sheet, the current level's
            // pitch otherwise.
            let after_sheet = |i: usize| h + if parts[i] { between } else { 0.0 };
            let pitch: Vec<f64> = (1..n).map(|i| (slot[i] - slot[i - 1]).max(0.0)).collect();
            let sheets: f64 = (0..n - 1).filter(|&i| held[i]).map(after_sheet).sum();
            let neighbors: f64 = (0..n - 1).filter(|&i| !held[i]).map(|i| pitch[i]).sum();
            let plus = if self.level == Level::Header {
                PLUS_GAP + m.plus
            } else {
                0.0
            };
            let tail = (if held[n - 1] { h } else { l }) + plus;
            let avail = (self.length - 2.0 * m.padding).max(0.0);
            let fixed = sheets + tail;
            let squeeze = match neighbors > 0.0 && fixed + neighbors > avail {
                true => ((avail - fixed) / neighbors).clamp(0.0, 1.0),
                false => 1.0,
            };
            let span = fixed + neighbors * squeeze;
            let mut top = (slot[0] - l / 2.0)
                .min(self.length - m.padding - span)
                .max(m.padding);
            for i in 0..n {
                places.push(match held[i] {
                    true => self.fit(top + h / 2.0),
                    false => top + l / 2.0,
                });
                if i + 1 < n {
                    top += if held[i] {
                        after_sheet(i)
                    } else {
                        pitch[i] * squeeze
                    };
                }
            }
        }
        // Back in the column's order.
        let mut out = vec![0.0; n];
        for (p, &i) in order.iter().enumerate() {
            out[i] = places[p];
        }
        if let Some((i, d)) = drag {
            out[i] = d.y;
        }
        out
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
            // A crumpling card keeps its size: it shrinks as a whole.
            if !c.crumpling() {
                c.out.retarget(out);
                c.len.retarget(len);
            }
            if let Some(y) = y {
                c.y.retarget(y);
            }
        }
        self.plus.retarget(match self.level {
            Level::Header => 1.0,
            Level::Marker => 0.0,
        });
        self.retarget_backdrops();
    }

    /// Point the backdrops at their groups' extents. One shows while its
    /// group has cards (or is between some leaving and others coming),
    /// at marker level; one coming out of sight starts at its extent.
    fn retarget_backdrops(&mut self) {
        let level = self.level == Level::Marker || self.metrics.backdrop_headers;
        for g in Group::ALL {
            let extent = (self.cards.iter())
                .filter(|c| !c.leaving && self.group_of(c) == g)
                .map(|c| {
                    let half = c.len.target / 2.0;
                    (c.y.target - half, c.y.target + half)
                })
                .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)));
            let coming = self.wanted.iter().any(|w| w.group == g);
            let b = &mut self.backdrops[g as usize];
            let shown = extent.is_some() || (coming && b.alpha.target > 0.0);
            if let Some((top, bottom)) = extent {
                match b.alpha.value <= 0.01 && b.alpha.target == 0.0 {
                    true => {
                        b.top.snap(top);
                        b.bottom.snap(bottom);
                    }
                    false => {
                        b.top.retarget(top);
                        b.bottom.retarget(bottom);
                    }
                }
            }
            b.alpha.retarget(if shown && level { 1.0 } else { 0.0 });
        }
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

    /// Hide `key`: as it leaves (once the program stops showing it), it
    /// crumples into the edge, at the size it is now.
    pub fn crumple(&mut self, key: &str) {
        if let Some(c) = self.cards.iter_mut().find(|c| c.key == key) {
            c.out.snap(c.out.value);
            c.len.snap(c.len.value);
            c.crumple = true;
        }
        if self.revealed.as_deref() == Some(key) {
            self.revealed = None;
        }
        if self.hover.as_deref() == Some(key) {
            self.hover = None;
        }
    }

    // -- reordering --------------------------------------------------------

    /// Start dragging `key`'s margin, its center at `y`. Only a note at
    /// header level moves (a revealed one folds back into its header; the
    /// open one stays), and not while the column changes. Returns whether
    /// the drag started.
    pub fn drag_start(&mut self, key: &str, y: f64) -> bool {
        let live = self.cards.iter().filter(|c| !c.leaving).count();
        let ok = self.level == Level::Header
            && !self.closing
            && self.open.as_deref() != Some(key)
            && live == self.slots.headers.len()
            && self.cards.iter().any(|c| c.key == key && c.present());
        if !ok {
            return false;
        }
        let Some(from) = self.cards.iter().find(|c| c.key == key).map(|c| c.group) else {
            return false;
        };
        if from == Group::Scratch && !self.cards.iter().any(|c| c.key == key && c.cross) {
            return false;
        }
        self.drag = Some(Drag {
            key: key.to_string(),
            y,
            from,
            group: from,
            gap: 0,
            slots: self.slots.clone(),
        });
        self.hover = Some(key.to_string());
        // Folded back, and no sheet pops out under the drop at once.
        self.revealed = None;
        self.browsing = false;
        self.drag_to(y);
        true
    }

    /// The dragged margin's center is at `y` now (kept within the column,
    /// and along its group unless it can go to the other one): the gap
    /// follows it, to the nearest slot it can land in.
    pub fn drag_to(&mut self, y: f64) {
        let Some(d) = &self.drag else {
            return;
        };
        let (key, from) = (d.key.clone(), d.from);
        let cross = self.cards.iter().any(|c| c.key == key && c.cross);
        let m = self.metrics;
        let half = m.header_length / 2.0;
        let bottom = self.length - m.padding - PLUS_GAP - m.plus;
        let (lo, hi) = (m.padding + half, bottom - half);
        let mut y = if lo <= hi {
            y.clamp(lo, hi)
        } else {
            self.length / 2.0
        };
        let own = self.slots.range(from);
        if !cross && !own.is_empty() {
            let reach = half * SHINGLE;
            let (lo, hi) = (
                lo.max(self.slots.headers[own.start] - reach),
                hi.min(self.slots.headers[own.end - 1] + reach),
            );
            if lo <= hi {
                y = y.clamp(lo, hi);
            }
        }
        // The nearest slot of its own group, or of the other one in the
        // column as it would be with it there.
        let nearest = |s: &Slots, g: Group| {
            let r = s.range(g);
            let i = r.start + crate::reorder::gap(&s.headers[r.clone()], y);
            (!r.is_empty()).then(|| ((s.headers[i] - y).abs(), i))
        };
        let same = if from == Group::Scratch {
            let i = self
                .wanted
                .iter()
                .position(|w| w.key == key)
                .unwrap_or(own.start);
            // No reordering within the scratch group.
            Some((f64::INFINITY, i))
        } else {
            nearest(&self.slots, from)
        };
        let mut best = same.map(|(dist, i)| (dist, i, from, None));
        // Scratch keeps its original slot unless dragged into the app or
        // global region. Split the space above scratch at the midpoint
        // between the two groups (or reserve room for an empty group).
        let scratch_target = if from == Group::Scratch {
            let above_scratch = |g: Group| {
                let r = self.slots.range(g);
                let next = self.slots.headers[r.end];
                if r.is_empty() {
                    next - half * SHINGLE
                } else {
                    (self.slots.headers[r.end - 1] + next) / 2.0
                }
            };
            let app_end = above_scratch(Group::Window);
            let global_end = if self.slots.range(Group::Window).is_empty() {
                let global = self.slots.range(Group::Global);
                if global.is_empty() {
                    app_end - half * SHINGLE
                } else {
                    (self.slots.headers[global.end - 1] + app_end) / 2.0
                }
            } else {
                above_scratch(Group::Global)
            };
            if y <= global_end {
                Some(Group::Global)
            } else if y <= app_end {
                Some(Group::Window)
            } else {
                None
            }
        } else {
            None
        };
        for &to in from
            .destinations()
            .iter()
            .filter(|&&to| cross && (from != Group::Scratch || scratch_target == Some(to)))
        {
            let mut counts = self.counts();
            counts[from as usize] -= 1;
            counts[to as usize] += 1;
            let s = slots(counts, self.length, &m);
            if let Some((dist, i)) = nearest(&s, to)
                && best.as_ref().is_none_or(|b| dist < b.0)
            {
                best = Some((dist, i, to, Some(s)));
            }
        }
        let Some((_, gap, group, s)) = best else {
            return;
        };
        let s = s.unwrap_or_else(|| self.slots.clone());
        let Some(d) = &mut self.drag else {
            return;
        };
        (d.y, d.gap, d.group, d.slots) = (y, gap, group, s);
        self.retarget();
        // It follows the pointer directly.
        if let Some(c) = self.cards.iter_mut().find(|c| c.key == key) {
            c.y.snap(y);
        }
    }

    /// Drop the dragged margin: the note, the slot it lands in, and the
    /// group if it changes. It goes back to its place until the program
    /// shows the new order.
    pub fn drag_end(&mut self) -> Option<Landing> {
        let d = self.drag.take()?;
        self.retarget();
        let start = d.slots.range(d.group).start;
        Some(Landing {
            key: d.key,
            group: d.group,
            gap: d.gap - start,
            moved: d.group != d.from,
        })
    }

    /// Give up a drag: the margin goes back to its place.
    pub fn drag_cancel(&mut self) {
        if self.drag.take().is_some() {
            self.retarget();
        }
    }

    // -- the layout --------------------------------------------------------

    /// The `+` button's center along the edge: in the screen's bottom
    /// corner, `padding` from the end, whatever the notes do, so it is
    /// always in the same spot. The notes keep clear of it (`places`).
    pub fn plus_center(&self) -> f64 {
        let m = &self.metrics;
        (self.length - m.padding - m.plus / 2.0).max(m.plus / 2.0)
    }

    /// What the notes that stay span along the edge where they are going,
    /// as (top, bottom), using their target sizes.
    fn extent(&self) -> Option<(f64, f64)> {
        self.cards
            .iter()
            .filter(|c| !c.leaving)
            .map(|c| {
                let half = c.len.target / 2.0;
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
    /// note, then the revealed one, then a dragged margin.
    pub fn paint_order(&self) -> Vec<&Card> {
        let rank = |c: &Card| match () {
            _ if self.dragging() == Some(c.key.as_str()) => 3,
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
        for b in &mut self.backdrops {
            for s in b.springs() {
                *s = respring(*s, response);
            }
        }
        self.relayout(false);
    }

    /// Move the cards that stay to their places among `wanted`.
    fn relayout(&mut self, snap: bool) {
        self.slots = slots(self.counts(), self.length, &self.metrics);
        for c in self.cards.iter_mut().filter(|c| !c.leaving) {
            if let Some(i) = self.wanted.iter().position(|w| w.key == c.key) {
                c.marker_slot = self.slots.markers[i];
                c.header_slot = self.slots.headers[i];
                c.group = self.wanted[i].group;
            }
        }
        self.retarget();
        if snap {
            for c in &mut self.cards {
                c.y.snap(c.y.target);
                c.len.snap(c.len.target);
            }
            for b in &mut self.backdrops {
                b.top.snap(b.top.target);
                b.bottom.snap(b.bottom.target);
            }
        }
    }

    /// Show `items`, each group's in this order, the global ones first
    /// (see the module's comment). The revealed, hovered, dragged and open
    /// ones go if they aren't among them (the program keeps the open note
    /// among them, where it was).
    pub fn set(&mut self, mut items: Vec<Item>) {
        // Stable: each group keeps its order.
        items.sort_by_key(|i| i.group);
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
        if (self.drag.as_ref()).is_some_and(|d| !self.wanted.iter().any(|w| w.key == d.key)) {
            self.drag = None;
        }
        let mut rank = 0.0;
        for c in &mut self.cards {
            match self.wanted.iter().find(|i| i.key == c.key) {
                Some(item) => {
                    c.look = item.look.clone();
                    c.cross = item.cross;
                    if c.leaving {
                        c.leaving = false;
                        c.crumple = false;
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
        // The column changed under a drag: it ends.
        let live = self.cards.iter().filter(|c| !c.leaving).count();
        if self.drag.is_some() && (self.closing || live != self.slots.headers.len()) {
            self.drag_cancel();
        }
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
        self.slots = slots(self.counts(), self.length, &self.metrics);
        let mut rank = 0.0;
        let mut new = Vec::new();
        let wanted = self.wanted.clone();
        for (i, item) in wanted.iter().enumerate() {
            let marker_slot = self.slots.markers[i];
            let header_slot = self.slots.headers[i];
            match old.iter().position(|c| c.key == item.key) {
                Some(i) => {
                    let mut c = old.swap_remove(i);
                    c.marker_slot = marker_slot;
                    c.header_slot = header_slot;
                    c.group = item.group;
                    c.cross = item.cross;
                    self.cards.push(c);
                }
                None => {
                    let role = self.role(&item.key);
                    let (out, len) = self.size(role);
                    let r = self.response;
                    let mut c = Card {
                        key: item.key.clone(),
                        look: item.look.clone(),
                        y: Spring::new(0.0, r),
                        marker_slot,
                        header_slot,
                        group: item.group,
                        cross: item.cross,
                        presence: Spring::new(0.0, r),
                        out: Spring::new(out, r),
                        len: Spring::new(len, r),
                        delay: rank * STAGGER,
                        leaving: false,
                        crumple: false,
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
        for b in &mut self.backdrops {
            for s in b.springs() {
                s.advance(dt);
            }
        }
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
        self.closing
            || !self.plus.settled()
            || !self.backdrops.iter().all(Backdrop::settled)
            || !self.cards.iter().all(still)
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
        group_gap: 8.0,
        backdrop_headers: false,
    };

    fn item(key: &str) -> Item {
        Item {
            key: key.into(),
            look: Look {
                color: 0xffff00ff,
                tilt: 0.0,
            },
            group: Group::Global,
            cross: false,
        }
    }

    /// A note for the window.
    fn win(key: &str) -> Item {
        Item {
            group: Group::Window,
            ..item(key)
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
    fn headers_shingle_independently_of_markers() {
        assert!(slots([0, 0], 500.0, &M).headers.is_empty());
        assert_eq!(slots([1, 0], 500.0, &M).headers, [250.0]);
        assert_eq!(slots([3, 0], 1000.0, &M).headers, [430.0, 500.0, 570.0]);
        // Crowded: they shingle tighter, clear of the `+` button.
        let reserve = PLUS_GAP + M.plus;
        let s = slots([6, 0], 300.0, &M);
        let pitch = s.headers[1] - s.headers[0];
        assert!((pitch - (276.0 - reserve - 100.0) / 5.0).abs() < 1e-9);
        assert!((s.headers[0] - 50.0 - M.padding).abs() < 1e-9);
        assert!((s.headers[5] + 50.0 - (300.0 - M.padding - reserve)).abs() < 1e-9);
        // Reaching the button: they move up as little as needed first.
        let s = slots([3, 0], 310.0, &M);
        assert_eq!(s.headers[1] - s.headers[0], 70.0);
        assert!((s.headers[2] + 50.0 - (310.0 - M.padding - reserve)).abs() < 1e-9);
    }

    #[test]
    fn markers_shrink_their_gaps_then_their_length() {
        assert!(slots([0, 0], 500.0, &M).markers.is_empty());
        assert_eq!(slots([1, 0], 500.0, &M).markers, [250.0]);
        let s = slots([3, 0], 1000.0, &M);
        assert_eq!(s.markers, [464.0, 500.0, 536.0]);
        assert_eq!(s.marker_length, M.marker_length);
        let s = slots([3, 0], 116.0, &M); // Room for 28px markers and 4px gaps.
        assert_eq!(s.markers, [26.0, 58.0, 90.0]);
        assert_eq!(s.marker_length, M.marker_length);
        let s = slots([3, 0], 100.0, &M); // Shrink the markers to keep 2px gaps.
        assert_eq!(s.markers, [24.0, 50.0, 76.0]);
        assert_eq!(s.marker_length, 24.0);
        assert_eq!(slots([40, 0], 200.0, &M).marker_length, MIN_MARKER);
        let tight = Metrics {
            marker_gap: 0.0,
            marker_length: 8.0,
            ..M
        };
        let s = slots([3, 0], 100.0, &tight);
        assert_eq!(s.markers, [42.0, 50.0, 58.0]);
        assert_eq!(s.marker_length, 8.0);
    }

    #[test]
    fn spacing_settings_only_affect_their_own_level() {
        let default = slots([3, 0], 1000.0, &M);
        let tighter = slots(
            [3, 0],
            1000.0,
            &Metrics {
                marker_gap: 4.0,
                ..M
            },
        );
        assert_eq!(tighter.markers, [468.0, 500.0, 532.0]);
        assert_eq!(tighter.headers, default.headers);

        let shorter = slots(
            [3, 0],
            1000.0,
            &Metrics {
                header_length: 80.0,
                ..M
            },
        );
        assert_eq!(shorter.markers, default.markers);
        assert_eq!(shorter.headers, [444.0, 500.0, 556.0]);
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
        assert_eq!(card(&d, "a").y.value, 482.0);
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
        assert_eq!(card(&d, "a").y.value, 464.0);
        // At the edge: headers at once, no reveal yet.
        assert_eq!(d.point(On::Strip), None);
        assert_eq!(card(&d, "a").y.value, 464.0);
        assert_eq!(card(&d, "a").y.target, 430.0);
        settle(&mut d);
        assert_eq!(d.level(), Level::Header);
        assert_eq!(card(&d, "a").out.value, M.header);
        assert_eq!(card(&d, "a").len.value, M.header_length);
        assert_eq!(card(&d, "a").y.value, 430.0);
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
        assert_eq!(card(&d, "a").y.value, 464.0);
        assert_eq!(card(&d, "a").len.value, M.marker_length);
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
        let plus = d.plus_center();
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
        // The `+` button stays put, in the bottom corner.
        assert_eq!(d.plus_center(), plus);
        assert!(plus - M.plus / 2.0 >= span(&d, "d").1);
        d.point(margin("d"));
        settle(&mut d);
        assert_eq!(d.revealed(), Some("d"));
        assert_eq!(d.plus_center(), plus);
        assert!(plus - M.plus / 2.0 >= span(&d, "d").1);
        // Onto the button: the sheet folds, and the button still stays.
        d.point(On::Plus);
        settle(&mut d);
        assert_eq!(d.revealed(), None);
        assert_eq!(d.plus_center(), plus);
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
        d.point(On::Strip);
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
        let slot = card(&d, "b").header_slot;
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
        // program keeps it where it was, and it keeps its slot.
        d.set(items(&["a", "b", "c"]));
        settle(&mut d);
        assert_eq!(keys(&d), ["a", "b", "c"]);
        assert_eq!(card(&d, "b").header_slot, slot);
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
    fn dragging_a_margin_opens_a_gap() {
        let mut d = deck(&["a", "b", "c", "d"]);
        // Only at header level.
        assert!(!d.drag_start("a", 0.0));
        d.point(On::Strip);
        settle(&mut d);
        let slots: Vec<f64> = ["a", "b", "c", "d"]
            .iter()
            .map(|k| card(&d, k).y.value)
            .collect();
        assert!(d.drag_start("a", slots[0]));
        assert_eq!(d.dragging(), Some("a"));
        assert_eq!(d.role("a"), Role::Lifted);
        // Down between c and d: b and c move up a slot, d stays.
        d.drag_to(slots[2] + 5.0);
        settle(&mut d);
        assert_eq!(card(&d, "a").y.value, slots[2] + 5.0);
        assert_eq!(card(&d, "b").y.value, slots[0]);
        assert_eq!(card(&d, "c").y.value, slots[1]);
        assert_eq!(card(&d, "d").y.value, slots[3]);
        // On top while dragged.
        assert_eq!(d.paint_order().last().unwrap().key, "a");
        // Kept within the column.
        d.drag_to(-500.0);
        assert!(card(&d, "a").y.value >= M.padding + M.header_length / 2.0);
        d.drag_to(slots[2]);
        let landing = Landing {
            key: "a".into(),
            group: Group::Global,
            gap: 2,
            moved: false,
        };
        assert_eq!(d.drag_end(), Some(landing));
        assert_eq!(d.dragging(), None);
        // The program shows the new order: everyone to their slot.
        d.set(items(&["b", "c", "a", "d"]));
        settle(&mut d);
        let now: Vec<f64> = ["b", "c", "a", "d"]
            .iter()
            .map(|k| card(&d, k).y.value)
            .collect();
        assert_eq!(now, slots);
        // Leaving the column, or it changing, ends a drag.
        assert!(d.drag_start("d", slots[3]));
        d.set(items(&["b", "c", "a"]));
        assert_eq!(d.dragging(), None);
    }

    #[test]
    fn a_revealed_note_folds_back_to_be_dragged() {
        let mut d = deck(&["a", "b", "c"]);
        d.point(margin("b"));
        d.reveal_due("b");
        settle(&mut d);
        assert_eq!(d.role("b"), Role::Sheet);
        let y = card(&d, "c").y.value;
        assert!(d.drag_start("b", y));
        assert_eq!((d.revealed(), d.role("b")), (None, Role::Lifted));
        settle(&mut d);
        assert_eq!(d.drag_end().map(|l| l.gap), Some(2));
        // Not instantly revealed again under the drop.
        assert_eq!(d.point(margin("b")), Some("b".into()));
        // The open note doesn't move.
        d.open_note("a");
        assert!(!d.drag_start("a", y));
    }

    #[test]
    fn hidden_notes_crumple() {
        let mut d = deck(&["a", "b"]);
        d.point(On::Strip);
        settle(&mut d);
        let out = card(&d, "a").out.value;
        d.crumple("a");
        d.set(items(&["b"]));
        assert!(card(&d, "a").crumpling());
        d.advance(1.0 / 60.0);
        // It keeps its size; it goes as a whole.
        assert_eq!(card(&d, "a").out.target, out);
        settle(&mut d);
        assert_eq!(keys(&d), ["b"]);
        // A note shown again doesn't crumple.
        let mut d = deck(&["a", "b"]);
        d.crumple("a");
        d.set(items(&["b"]));
        d.set(items(&["a", "b"]));
        assert!(!card(&d, "a").crumple);
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
        assert_eq!(card(&d, "b").y.value, slots([2, 0], 1000.0, &M).markers[0]);
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
    fn the_plus_stays_in_the_bottom_corner() {
        let mut d = deck(&["a", "b"]);
        let corner = 1000.0 - M.padding - M.plus / 2.0;
        assert_eq!(d.plus_center(), corner);
        d.point(On::Strip);
        assert_eq!(d.plus_center(), corner);
        d.set(items(&[]));
        assert_eq!(d.plus_center(), corner);
        assert_eq!(Deck::new(M, 150.0).plus_center(), M.plus / 2.0);
    }

    // -- groups ------------------------------------------------------------

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// Two global notes (g1, g2) and two for the window (w1, w2), each
    /// `cross` as given.
    fn grouped(cross: bool) -> Vec<Item> {
        let mut v = vec![item("g1"), item("g2"), win("w1"), win("w2")];
        for i in &mut v {
            i.cross = cross;
        }
        v
    }

    fn grouped_deck(cross: bool) -> Deck {
        let mut d = Deck::new(M, 150.0);
        d.resize(1000.0);
        d.set(grouped(cross));
        settle(&mut d);
        d
    }

    #[test]
    fn groups_are_apart_and_centered_together() {
        let s = slots([2, 2], 1000.0, &M);
        assert_eq!(s.range(Group::Window), 2..4);
        // 28px markers 8px apart, 8px more between the groups, and the
        // whole centered.
        assert_eq!(s.markers, [442.0, 478.0, 522.0, 558.0]);
        assert_eq!(s.markers[0] + s.markers[3], 1000.0);
        // At headers, as much more as their pitch is to the markers'.
        let between = 70.0 * 8.0 / 36.0;
        assert!(close(s.between[1], between));
        assert!(close(s.headers[0], 395.0 - between / 2.0));
        assert!(close(s.headers[1], 465.0 - between / 2.0));
        assert!(close(s.headers[2], 535.0 + between / 2.0));
        assert!(close(s.headers[3], 605.0 + between / 2.0));
    }

    #[test]
    fn an_empty_group_takes_no_room() {
        for n in [1, 3] {
            let alone = slots([n, 0], 1000.0, &M);
            let window = slots([0, n], 1000.0, &M);
            assert_eq!(
                (&alone.markers, &alone.headers),
                (&window.markers, &window.headers)
            );
            assert_eq!(alone.between, [0.0, 0.0]);
        }
        assert!(slots([0, 0], 1000.0, &M).markers.is_empty());
    }

    #[test]
    fn arbitrary_group_counts_skip_empty_groups() {
        let s = slots([1, 0, 1, 1, 0, 1], 1000.0, &M);
        assert_eq!(s.markers, [434.0, 478.0, 522.0, 566.0]);
        assert_eq!(s.ranges, [0..1, 1..1, 1..2, 2..3, 3..3, 3..4]);
        assert!(
            s.headers
                .windows(2)
                .all(|pair| close(pair[1] - pair[0], 70.0 * (1.0 + 8.0 / 36.0)))
        );
    }

    #[test]
    fn scratch_can_land_in_app_or_global_and_reuses_its_card() {
        let mut d = grouped_deck(false);
        let scratch = |key| Item {
            group: Group::Scratch,
            cross: true,
            ..item(key)
        };
        let mut g = item("g1");
        g.cross = true;
        d.set(vec![g.clone(), win("app"), scratch("s1"), scratch("s2")]);
        settle(&mut d);
        assert_eq!(keys(&d), ["g1", "app", "s1", "s2"]);
        assert_eq!(d.backdrops()[2].alpha.value, 1.0);
        d.point(On::Strip);
        settle(&mut d);
        let y = |d: &Deck, key| card(d, key).y.value;
        assert!(d.drag_start("s2", y(&d, "s2")));
        d.drag_to(y(&d, "s1"));
        let stay = d.drag_end().unwrap();
        assert_eq!((stay.group, stay.gap), (Group::Scratch, 1));
        assert!(d.drag_start("s2", y(&d, "s2")));
        d.drag_to(y(&d, "app"));
        let app = d.drag_end().unwrap();
        assert_eq!((app.group, app.moved), (Group::Window, true));
        assert!(d.drag_start("s2", y(&d, "s2")));
        d.drag_to(y(&d, "g1"));
        assert_eq!(d.drag_end().unwrap().group, Group::Global);
        assert!(d.drag_start("g1", y(&d, "g1")));
        d.drag_to(y(&d, "s2"));
        assert_ne!(d.drag_end().unwrap().group, Group::Scratch);
        let before = y(&d, "s2");
        d.rename("s2", "kept");
        d.set(vec![g, item("kept"), win("app"), scratch("s1")]);
        assert_eq!(y(&d, "kept"), before);
        settle(&mut d);
        assert_eq!(keys(&d), ["g1", "kept", "app", "s1"]);
    }

    #[test]
    fn crowding_counts_both_groups() {
        let s = slots([3, 3], 200.0, &M);
        let half = s.marker_length / 2.0;
        assert!(s.marker_length < M.marker_length);
        assert!(s.markers[0] - half >= M.padding - 1e-9);
        assert!(s.markers[5] + half <= 200.0 - M.padding + 1e-9);
        // The groups stay apart.
        let pitch = s.markers[1] - s.markers[0];
        assert!(close(s.markers[3] - s.markers[2], pitch + M.group_gap));
        // Headers shingle tighter, the gap with them.
        let s = slots([3, 3], 400.0, &M);
        let pitch = s.headers[1] - s.headers[0];
        assert!(pitch < 70.0);
        assert!(close(
            s.headers[3] - s.headers[2],
            pitch * (1.0 + 8.0 / 36.0)
        ));
        assert!(s.headers[0] - 50.0 >= M.padding - 1e-9);
        assert!(s.headers[5] + 50.0 <= 400.0 - M.padding - PLUS_GAP - M.plus + 1e-9);
    }

    #[test]
    fn the_column_stays_centered_through_context_switches() {
        let mut d = grouped_deck(false);
        for window in [
            vec![win("w3")],
            vec![],
            vec![win("w1"), win("w2"), win("w3")],
        ] {
            let mut v = vec![item("g1"), item("g2")];
            v.extend(window);
            d.set(v);
            settle(&mut d);
            let (first, last) = (&d.cards()[0], d.cards().last().unwrap());
            assert!(close(first.y.value + last.y.value, 1000.0));
        }
        // Given in any order, the global ones come first.
        d.set(vec![win("w1"), item("g1"), win("w2"), item("g2")]);
        settle(&mut d);
        assert_eq!(keys(&d), ["g1", "g2", "w1", "w2"]);
    }

    #[test]
    fn backdrops_follow_their_groups() {
        let mut d = grouped_deck(false);
        let [g, w, scratch] = *d.backdrops();
        assert_eq!(scratch.alpha.value, 0.0);
        assert_eq!(
            (g.top.value, g.bottom.value, g.alpha.value),
            (428.0, 492.0, 1.0)
        );
        assert_eq!(
            (w.top.value, w.bottom.value, w.alpha.value),
            (508.0, 572.0, 1.0)
        );
        // At header level they fade out.
        d.point(On::Strip);
        settle(&mut d);
        assert!(d.backdrops().iter().all(|b| b.alpha.value == 0.0));
        d.hide_due();
        settle(&mut d);
        assert!(d.backdrops()[..2].iter().all(|b| b.alpha.value == 1.0));
        // Notes coming stretch it.
        d.set(vec![
            item("g1"),
            item("g2"),
            win("w1"),
            win("w2"),
            win("w3"),
        ]);
        settle(&mut d);
        assert_eq!(d.backdrops()[1].bottom.value, 590.0);
        // Swapping all of a group's notes keeps it.
        d.set(vec![item("g1"), item("g2"), win("w4")]);
        for _ in 0..60 {
            d.advance(1.0 / 60.0);
            assert_eq!(d.backdrops()[1].alpha.target, 1.0);
        }
        settle(&mut d);
        // Its last note gone, it fades out.
        d.set(vec![item("g1"), item("g2")]);
        settle(&mut d);
        assert_eq!(d.backdrops()[1].alpha.value, 0.0);
        assert_eq!(d.backdrops()[0].alpha.value, 1.0);
        // Coming back, it fades in where its notes are.
        d.set(vec![item("g1"), item("g2"), win("w1")]);
        let w = d.backdrops()[1];
        assert_eq!((w.top.value, w.bottom.value), (526.0, 554.0));
        // Kept at header level if asked.
        let mut d = Deck::new(
            Metrics {
                backdrop_headers: true,
                ..M
            },
            150.0,
        );
        d.resize(1000.0);
        d.set(grouped(false));
        d.point(On::Strip);
        settle(&mut d);
        assert!(d.backdrops()[..2].iter().all(|b| b.alpha.value == 1.0));
    }

    #[test]
    fn pinning_moves_a_note_to_the_other_group() {
        let mut d = grouped_deck(false);
        d.set(vec![item("g1"), win("g2"), win("w1"), win("w2")]);
        settle(&mut d);
        assert_eq!(card(&d, "g2").group, Group::Window);
        let s = slots([1, 3], 1000.0, &M);
        assert_eq!(card(&d, "g2").y.value, s.markers[1]);
        assert_eq!(card(&d, "g1").y.value, s.markers[0]);
    }

    #[test]
    fn dragging_stays_in_its_group_unless_it_can_cross() {
        let mut d = grouped_deck(false);
        d.point(On::Strip);
        settle(&mut d);
        let w2 = card(&d, "w2").y.value;
        assert!(d.drag_start("g1", card(&d, "g1").y.value));
        d.drag_to(w2);
        // Kept along its group: at its end.
        assert!(card(&d, "g1").y.value <= card(&d, "g2").y.value + 35.0 + 1e-9);
        let l = d.drag_end().unwrap();
        assert_eq!((l.group, l.gap, l.moved), (Group::Global, 1, false));
        assert!(d.drag_start("w2", w2));
        d.drag_to(0.0);
        assert_eq!(
            d.drag_end().map(|l| (l.group, l.gap, l.moved)),
            Some((Group::Window, 0, false))
        );
    }

    #[test]
    fn dragging_into_the_other_group() {
        let mut d = grouped_deck(true);
        d.point(On::Strip);
        settle(&mut d);
        let (g2, w2) = (card(&d, "g2").y.value, card(&d, "w2").y.value);
        assert!(d.drag_start("g1", card(&d, "g1").y.value));
        // Past w2: the column as it would be with g1 last.
        d.drag_to(w2 + 30.0);
        settle(&mut d);
        let s = slots([1, 3], 1000.0, &M);
        assert_eq!(card(&d, "g2").y.value, s.headers[0]);
        assert_eq!(card(&d, "w1").y.value, s.headers[1]);
        assert_eq!(card(&d, "w2").y.value, s.headers[2]);
        // Back over its own group: it stays there.
        d.drag_to(g2);
        assert_eq!(
            d.drag_end().map(|l| (l.group, l.gap, l.moved)),
            Some((Group::Global, 1, false))
        );
        assert!(d.drag_start("g1", card(&d, "g1").y.value));
        d.drag_to(w2 + 30.0);
        let landing = Landing {
            key: "g1".into(),
            group: Group::Window,
            gap: 2,
            moved: true,
        };
        assert_eq!(d.drag_end(), Some(landing));
        // And up from the window's group into the global one.
        settle(&mut d);
        assert!(d.drag_start("w1", card(&d, "w1").y.value));
        d.drag_to(0.0);
        assert_eq!(
            d.drag_end().map(|l| (l.group, l.gap, l.moved)),
            Some((Group::Global, 0, true))
        );
    }
}
