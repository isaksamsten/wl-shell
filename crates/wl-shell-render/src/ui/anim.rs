//! Animation: what an animated node shows when, and frame sheets.
//!
//! A plugin describes motion once and the host plays it (see `Frames`):
//! a canvas with `frames` is drawn at that many values of its parameter
//! `t` when the tree is built, painted once into a frame sheet
//! (`render_sheet`), and the host shows the frame for the current `t`.
//! An indeterminate progress bar sweeps its segment along the track, and
//! a slider's or progress bar's value with a `transition` eases to a new
//! one; those are painted again as they move.
//!
//! `Player` keeps what playing needs between trees, by node key: when a
//! loop started (kept while its frames stay the same, so a tree that only
//! changes a label doesn't restart a spinner), and each easing value's
//! way from the value shown to its new one. It knows nothing of surfaces
//! or timers: the host asks it what a node shows at a time (`t`, `frame`,
//! `values`) and how soon that changes (`due`).
//!
//! Times are seconds on the host's clock (`State::now`, the clock of
//! `Live::at`).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hash::{DefaultHasher, Hasher};

use super::layout::Bounds;
use super::paint::{self, Request, Resources};
use super::tree::{Drive, Frames, Kind, Node};
use crate::image::Image;
use crate::text::Text;

/// The fewest and most frames a canvas may have.
pub const MIN_FRAMES: usize = 2;
pub const MAX_FRAMES: usize = 60;
/// The shortest loop, seconds.
pub const MIN_LOOP: f32 = 0.25;
/// An indeterminate bar's sweep: there and back, seconds.
pub const SWEEP: f64 = 2.0;
/// A sheet's most bytes; over it, every other frame (and so on).
pub const SHEET_CAP: usize = 16 << 20;

/// Feeds what it formats to a hasher.
struct Feed<'a>(&'a mut DefaultHasher);

impl std::fmt::Write for Feed<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// A hash of what `v` formats as (commands hold floats, which aren't
/// `Hash`; their `Debug` is exact).
fn hash_debug(v: &impl std::fmt::Debug) -> u64 {
    let mut h = DefaultHasher::new();
    let _ = write!(Feed(&mut h), "{v:?}");
    h.finish()
}

/// A hash of a canvas's frames: a loop keeps its phase while it stays.
pub fn frames_hash(f: &Frames) -> u64 {
    hash_debug(&f.frames)
}

/// What a frame sheet is painted for: the part's node (its frames), its
/// size, the scale and the theme. Equal keys, equal sheets.
pub fn sheet_key(req: &Request) -> u64 {
    hash_debug(&(&req.tree, req.bounds, req.scale, &req.theme))
}

/// Ease-out (cubic) from 0 to 1.
fn ease_out(p: f64) -> f64 {
    let q = 1.0 - p.clamp(0.0, 1.0);
    1.0 - q * q * q
}

/// 0 to 1 every `period`, and with `mirror` back over another.
fn cycle(elapsed: f64, period: f64, mirror: bool) -> f64 {
    let u = elapsed / period.max(1e-3);
    match mirror {
        true => {
            let c = u.rem_euclid(2.0);
            if c > 1.0 { 2.0 - c } else { c }
        }
        false => u.rem_euclid(1.0),
    }
}

/// A value on its way to a new one.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Ease {
    from: f64,
    to: f64,
    start: f64,
    /// Seconds.
    length: f64,
}

impl Ease {
    fn still(v: f64, now: f64) -> Self {
        Ease {
            from: v,
            to: v,
            start: now,
            length: 0.0,
        }
    }

    fn at(&self, now: f64) -> f64 {
        if self.length <= 0.0 || now >= self.start + self.length {
            return self.to;
        }
        let p = ((now - self.start) / self.length).max(0.0);
        self.from + (self.to - self.from) * ease_out(p)
    }

    fn moving(&self, now: f64) -> bool {
        self.from != self.to && now < self.start + self.length
    }
}

/// What a node's eased value and its transition are, if it eases.
fn easing(kind: &Kind) -> Option<(f64, f32)> {
    match kind {
        Kind::Canvas { anim: Some(a), .. } => match a.drive {
            Drive::Value {
                value, transition, ..
            } => Some((value, transition)),
            Drive::Loop { .. } => None,
        },
        Kind::Progress {
            value,
            transition,
            indeterminate: false,
            ..
        }
        | Kind::Slider {
            value, transition, ..
        } if *transition > 0.0 => Some((*value, *transition)),
        _ => None,
    }
}

/// Plays animated nodes, by key, across the trees that show them.
#[derive(Clone, Debug)]
pub struct Player {
    /// Off (`[widgets] animations = false`): loops stay at their start,
    /// values jump, indeterminate bars stand still.
    enabled: bool,
    /// When each loop (or sweeping bar) started, and its frames' hash.
    loops: HashMap<String, (u64, f64)>,
    eases: HashMap<String, Ease>,
}

impl Default for Player {
    fn default() -> Self {
        Player::new(true)
    }
}

impl Player {
    pub fn new(enabled: bool) -> Self {
        Player {
            enabled,
            loops: HashMap::new(),
            eases: HashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turn animations on or off; off, every value is where it's headed.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            for e in self.eases.values_mut() {
                *e = Ease::still(e.to, e.start);
            }
        }
    }

    /// A new tree shows the animated `nodes` (by key) at `now`. A loop
    /// keeps its phase if its frames are the same; a value eases from the
    /// one shown to its new one (the first time, it shows at once).
    /// What isn't among them is forgotten.
    pub fn sync<'a>(&mut self, nodes: impl IntoIterator<Item = &'a Node>, now: f64) {
        let mut loops = HashMap::new();
        let mut eases = HashMap::new();
        for n in nodes {
            let Some(key) = &n.key else { continue };
            let looping = match &n.kind {
                Kind::Canvas { anim: Some(a), .. } if matches!(a.drive, Drive::Loop { .. }) => {
                    Some(frames_hash(a))
                }
                Kind::Progress {
                    indeterminate: true,
                    ..
                } => Some(0),
                _ => None,
            };
            if let Some(hash) = looping {
                let start = match self.loops.get(key) {
                    Some((h, start)) if *h == hash => *start,
                    _ => now,
                };
                loops.insert(key.clone(), (hash, start));
            }
            if let Some((value, ms)) = easing(&n.kind) {
                let e = match self.eases.get(key) {
                    None => Ease::still(value, now),
                    Some(e) if e.to == value => *e,
                    Some(_) if !self.enabled || ms <= 0.0 => Ease::still(value, now),
                    Some(e) => Ease {
                        from: e.at(now),
                        to: value,
                        start: now,
                        length: ms as f64 / 1000.0,
                    },
                };
                eases.insert(key.clone(), e);
            }
        }
        self.loops = loops;
        self.eases = eases;
    }

    /// The value node `key` shows at `now`, if it eases.
    pub fn eased(&self, key: &str, now: f64) -> Option<f64> {
        self.eases.get(key).map(|e| e.at(now))
    }

    /// A loop's (or sweeping bar's) position at `now`, 0 to 1.
    fn phase(&self, key: Option<&str>, period: f64, mirror: bool, now: f64) -> f64 {
        let start = key.and_then(|k| self.loops.get(k)).map_or(now, |l| l.1);
        match self.enabled {
            true => cycle(now - start, period, mirror),
            false => 0.0,
        }
    }

    /// Canvas `n`'s parameter at `now` (0 to 1; a live value may wrap).
    pub fn t(&self, n: &Node, now: f64) -> f64 {
        let Kind::Canvas { anim: Some(a), .. } = &n.kind else {
            return 0.0;
        };
        let key = n.key.as_deref();
        match a.drive {
            Drive::Loop { period, mirror } => self.phase(key, period as f64, mirror, now),
            Drive::Value { value, live, .. } => {
                let v = key.and_then(|k| self.eased(k, now)).unwrap_or(value);
                match live {
                    Some(l) if self.enabled && l.rate.is_finite() => {
                        v + l.rate * (now - l.at).max(0.0)
                    }
                    _ => v,
                }
            }
        }
    }

    /// The frame canvas `n` shows at `now`.
    pub fn frame(&self, n: &Node, now: f64) -> usize {
        match &n.kind {
            Kind::Canvas { anim: Some(a), .. } => a.index(self.t(n, now)),
            _ => 0,
        }
    }

    /// The values to paint `nodes` with at `now` (`State::values`): the
    /// eased ones, and indeterminate bars' phases.
    pub fn values<'a>(
        &self,
        nodes: impl IntoIterator<Item = &'a Node>,
        now: f64,
    ) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for n in nodes {
            let Some(key) = &n.key else { continue };
            let v = match &n.kind {
                Kind::Progress {
                    indeterminate: true,
                    ..
                } => Some(self.phase(Some(key), SWEEP / 2.0, true, now)),
                Kind::Progress { .. } | Kind::Slider { .. } => self.eased(key, now),
                _ => None,
            };
            if let Some(v) = v {
                out.push((key.clone(), v));
            }
        }
        out
    }

    /// How soon node `n` looks different from how it does at `now`,
    /// seconds, never sooner than `fastest`; None while it stands still.
    pub fn due(&self, n: &Node, now: f64, fastest: f64) -> Option<f64> {
        if !self.enabled {
            return None;
        }
        let key = n.key.as_deref();
        let easing = key
            .and_then(|k| self.eases.get(k))
            .is_some_and(|e| e.moving(now));
        let due = match &n.kind {
            Kind::Canvas { anim: Some(a), .. } => {
                let steps = a.frames.len().max(1) as f64;
                match a.drive {
                    Drive::Loop { period, mirror } => {
                        let per = if mirror { steps - 1.0 } else { steps };
                        Some(period as f64 / per.max(1.0))
                    }
                    Drive::Value { .. } if easing => Some(fastest),
                    Drive::Value {
                        live: Some(l), ..
                    } if l.rate != 0.0 && l.rate.is_finite() => {
                        Some((1.0 / (steps * l.rate.abs())).min(1.0))
                    }
                    Drive::Value { .. } => None,
                }
            }
            Kind::Progress {
                indeterminate: true,
                ..
            } => Some(fastest),
            Kind::Progress { .. } | Kind::Slider { .. } if easing => Some(fastest),
            _ => None,
        };
        due.map(|d| d.max(fastest))
    }
}

/// A canvas's frames, painted: one image per `step`th frame (all of them
/// unless that would be over the cap).
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    pub images: Vec<Image>,
    /// Image `i` is frame `i * step`.
    pub step: usize,
}

impl Sheet {
    /// The image of frame `frame`.
    pub fn image_of(&self, frame: usize) -> usize {
        (frame / self.step.max(1)).min(self.images.len().saturating_sub(1))
    }
}

/// How many frames to skip so `n` frames of `bytes` each fit in `cap`:
/// 1, 2, 4, ...
pub fn sheet_step(n: usize, bytes: usize, cap: usize) -> usize {
    let mut step = 1;
    while step < n && n.div_ceil(step) * bytes > cap {
        step *= 2;
    }
    step
}

/// Paint every frame of the animated canvas `req.tree` (a live part,
/// `Request::part`), within `cap` bytes. Anything else is painted once.
pub fn render_sheet(
    req: &Request,
    text: &mut Text,
    res: &mut dyn Resources,
    cap: usize,
) -> Sheet {
    let n = &*req.tree;
    let Kind::Canvas {
        viewbox,
        anim: Some(anim),
        ..
    } = &n.kind
    else {
        return Sheet {
            images: vec![paint::render(req, text, res).image],
            step: 1,
        };
    };
    let s = req.scale.max(1) as f32;
    let (w, h) = match req.bounds {
        Bounds::Exact(w, h) => (w, h),
        Bounds::Content { max_w, max_h } => (max_w, max_h),
    };
    let bytes = ((w * s).ceil().max(1.0) * (h * s).ceil().max(1.0)) as usize * 4;
    let step = sheet_step(anim.frames.len(), bytes, cap);
    let images = (anim.frames.iter().step_by(step))
        .map(|commands| {
            let mut frame = Node::new(Kind::Canvas {
                viewbox: *viewbox,
                commands: commands.clone(),
                anim: None,
            });
            frame.layout = n.layout.clone();
            frame.look = n.look;
            frame.key = n.key.clone();
            let r = Request {
                tree: std::sync::Arc::new(frame),
                ..req.clone()
            };
            paint::render(&r, text, res).image
        })
        .collect();
    Sheet { images, step }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::canvas::Cmd;
    use crate::ui::tree::Live;

    fn canvas(key: &str, n: usize, drive: Drive, mark: f32) -> Node {
        let frames = (0..n).map(|i| vec![Cmd::Rotate(i as f32 + mark)]).collect();
        let mut node = Node::new(Kind::Canvas {
            viewbox: (100.0, 100.0),
            commands: Vec::new(),
            anim: Some(Box::new(Frames { frames, drive })),
        });
        node.key = Some(key.into());
        node
    }

    fn spinner(mark: f32) -> Node {
        let drive = Drive::Loop {
            period: 1.2,
            mirror: false,
        };
        canvas("spin", 24, drive, mark)
    }

    fn ring(value: f64) -> Node {
        let drive = Drive::Value {
            value,
            transition: 300.0,
            live: None,
        };
        canvas("ring", 61, drive, 0.0)
    }

    fn frames_of(n: &Node) -> &Frames {
        match &n.kind {
            Kind::Canvas { anim: Some(a), .. } => a,
            _ => unreachable!(),
        }
    }

    #[test]
    fn frames_sample_t() {
        // A loop's last frame isn't its first again; a value's is 1.
        let drive = Drive::Loop {
            period: 1.0,
            mirror: false,
        };
        let looped = canvas("a", 4, drive, 0.0);
        let a = frames_of(&looped);
        assert_eq!((a.t(0), a.t(3)), (0.0, 0.75));
        assert_eq!((a.index(0.74), a.index(0.9), a.index(1.1)), (3, 0, 0));
        let value = ring(0.0);
        let r = frames_of(&value);
        assert_eq!((r.t(0), r.t(60)), (0.0, 1.0));
        assert_eq!((r.index(0.5), r.index(1.0), r.index(2.0)), (30, 60, 60));
    }

    #[test]
    fn loops_keep_their_phase_while_their_frames_stay() {
        let mut p = Player::default();
        p.sync([&spinner(0.0)], 100.0);
        // A quarter of the way round at 100.3 s.
        assert_eq!(p.frame(&spinner(0.0), 100.3), 6);
        // A new tree, the same spinner: it goes on.
        p.sync([&spinner(0.0)], 100.5);
        assert_eq!(p.frame(&spinner(0.0), 100.6), 12);
        // Other frames: it starts again.
        p.sync([&spinner(1.0)], 100.6);
        assert_eq!(p.frame(&spinner(1.0), 100.6), 0);
        // Gone and back: from the start.
        p.sync([], 101.0);
        p.sync([&spinner(1.0)], 101.0);
        assert_eq!(p.frame(&spinner(1.0), 101.0), 0);
        // Steps of a frame, but never faster than asked.
        let due = p.due(&spinner(1.0), 101.0, 1.0 / 60.0).unwrap();
        assert!((due - 0.05).abs() < 1e-6, "{due}");
        assert_eq!(p.due(&spinner(1.0), 101.0, 1.0 / 10.0), Some(0.1));
    }

    #[test]
    fn mirrored_loops_go_there_and_back() {
        let drive = Drive::Loop {
            period: 1.0,
            mirror: true,
        };
        let n = canvas("m", 11, drive, 0.0);
        let mut p = Player::default();
        p.sync([&n], 0.0);
        assert_eq!(p.frame(&n, 0.5), 5);
        assert_eq!(p.frame(&n, 1.0), 10);
        assert_eq!(p.frame(&n, 1.5), 5);
        assert_eq!(p.frame(&n, 2.0), 0);
    }

    #[test]
    fn values_ease_and_never_jump() {
        let mut p = Player::default();
        // The first value shows at once.
        p.sync([&ring(0.2)], 10.0);
        assert_eq!(p.t(&ring(0.2), 10.0), 0.2);
        assert_eq!(p.due(&ring(0.2), 10.0, 0.01), None);
        // A new one eases in over 300 ms, fast at first.
        p.sync([&ring(0.8)], 10.0);
        let mid = p.t(&ring(0.8), 10.15);
        assert!(mid > 0.5 + 0.1 && mid < 0.8, "{mid}");
        assert_eq!(p.due(&ring(0.8), 10.15, 0.01), Some(0.01));
        // Another mid-way: from where it is, not from 0.2 or 0.8.
        p.sync([&ring(0.0)], 10.15);
        assert!((p.t(&ring(0.0), 10.15) - mid).abs() < 1e-9);
        assert!(p.t(&ring(0.0), 10.2) < mid);
        assert_eq!(p.t(&ring(0.0), 10.5), 0.0);
        assert_eq!(p.due(&ring(0.0), 10.5, 0.01), None);
    }

    #[test]
    fn off_everything_stands_still() {
        let mut p = Player::new(false);
        p.sync([&spinner(0.0), &ring(0.2)], 10.0);
        p.sync([&spinner(0.0), &ring(0.9)], 10.0);
        assert_eq!(p.frame(&spinner(0.0), 10.5), 0);
        assert_eq!(p.t(&ring(0.9), 10.01), 0.9);
        assert_eq!(p.due(&spinner(0.0), 10.5, 0.01), None);
        let mut bar = Node::new(Kind::Progress {
            value: 0.0,
            max: 1.0,
            live: None,
            indeterminate: true,
            transition: 0.0,
        });
        bar.key = Some("bar".into());
        p.sync([&bar], 0.0);
        assert_eq!(p.values([&bar], 0.7), [("bar".to_string(), 0.0)]);
        assert_eq!(p.due(&bar, 0.7, 0.03), None);
        // On, it sweeps there and back.
        p.set_enabled(true);
        assert_eq!(p.values([&bar], 0.5), [("bar".to_string(), 0.5)]);
        assert_eq!(p.values([&bar], 1.5), [("bar".to_string(), 0.5)]);
        assert_eq!(p.due(&bar, 0.7, 0.03), Some(0.03));
    }

    #[test]
    fn live_values_wrap() {
        let drive = Drive::Value {
            value: 0.5,
            transition: 0.0,
            live: Some(Live {
                rate: 1.0 / 60.0,
                at: 100.0,
            }),
        };
        let n = canvas("sweep", 60, drive, 0.0);
        let mut p = Player::default();
        p.sync([&n], 100.0);
        assert_eq!(p.frame(&n, 100.0), 30);
        assert_eq!(p.frame(&n, 130.0), 0);
        assert_eq!(p.due(&n, 100.0, 0.01), Some(1.0));
    }

    #[test]
    fn sheets_fit_their_cap() {
        assert_eq!(sheet_step(24, 1000, 24_000), 1);
        assert_eq!(sheet_step(24, 1000, 23_999), 2);
        assert_eq!(sheet_step(24, 1000, 5_000), 8);
        // At least one frame, however big.
        assert_eq!(sheet_step(3, 1 << 30, 10), 4);
        let s = Sheet {
            images: vec![
                Image {
                    width: 0,
                    height: 0,
                    data: Vec::new(),
                };
                12
            ],
            step: 2,
        };
        assert_eq!((s.image_of(5), s.image_of(23), s.image_of(99)), (2, 11, 11));
    }
}

