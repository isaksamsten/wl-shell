//! Playing widgets' animated canvases (wl-shell-render's `ui::anim`) on
//! subsurfaces, in popups (widgetview.rs) and over dock tiles (view.rs).
//!
//! A tree painted `apart` lists its animated canvases as live parts.
//! Each gets a frame sheet, painted once by the worker for the part's
//! rect and scale; after that a frame costs an attach and a commit, not
//! a paint, and no Lua. A new tree keeps a part's sheet while the part
//! would paint the same (`ui::anim::sheet_key`), so a redraw that only
//! changes a label paints nothing again; the `Player` keeps its phase.
//!
//! `Sheets` is that bookkeeping, generic over the buffer type so it can
//! be tested without a compositor. `Pacer` paces the timer: one per
//! surface, set for the soonest layer due, and set again only once the
//! compositor has shown the last frame (its frame callback). A surface
//! that gets no frame callbacks (hidden by autohide, or on an idle
//! output) gets no ticks until they come back.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wl_shell_render::ui::{self, LivePart, Player};

/// The most frames a second: on dock tiles, and in popups.
pub const TILE_FPS: f64 = 30.0;
pub const POPUP_FPS: f64 = 60.0;
/// The most animated canvases a tile plays.
pub const MAX_PER_TILE: usize = 4;

/// Seconds since the epoch: the animations' clock, and `live.at`'s
/// (`dock.now()`).
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// A sheet's images as buffers.
struct Painted<B> {
    buffers: Vec<B>,
    step: usize,
}

/// An animated canvas and its sheet.
pub struct SheetLayer<B> {
    pub part: LivePart,
    /// What its sheet is painted for.
    key: u64,
    /// The sheet's paint in flight.
    wanted: Option<u64>,
    sheet: Option<Painted<B>>,
}

/// The animated canvases of one tree.
pub struct Sheets<B> {
    pub layers: Vec<SheetLayer<B>>,
}

impl<B> Default for Sheets<B> {
    fn default() -> Self {
        Sheets { layers: Vec::new() }
    }
}

impl<B> Sheets<B> {
    /// The tree painted with `base` has these animated `parts` now. The
    /// sheets to paint (by layer, with their requests: `asked` once
    /// sent), and the buffers no layer shows anymore. A part that paints
    /// differently now shows its old sheet until its new one is in.
    #[allow(clippy::type_complexity)]
    pub fn sync(
        &mut self,
        parts: Vec<LivePart>,
        base: &ui::Request,
    ) -> (Vec<(usize, ui::Request)>, Vec<B>) {
        let mut old: Vec<SheetLayer<B>> = std::mem::take(&mut self.layers);
        let mut paint = Vec::new();
        for part in parts {
            let request = base.part(&part);
            let key = ui::anim::sheet_key(&request);
            let layer = match old.iter().position(|l| l.key == key) {
                Some(i) => SheetLayer {
                    part,
                    ..old.remove(i)
                },
                None => {
                    paint.push((self.layers.len(), request));
                    // The same node drawn anew: its old frames meanwhile.
                    let named = (part.node.key.as_ref()).and_then(|k| {
                        old.iter()
                            .position(|l| l.part.node.key.as_ref() == Some(k))
                    });
                    let sheet = named.and_then(|i| old.remove(i).sheet);
                    SheetLayer {
                        part,
                        key,
                        wanted: None,
                        sheet,
                    }
                }
            };
            self.layers.push(layer);
        }
        let stale = (old.into_iter())
            .flat_map(|l| l.sheet.map(|s| s.buffers).unwrap_or_default())
            .collect();
        (paint, stale)
    }

    /// Layer `i`'s sheet was asked for with `tag`.
    pub fn asked(&mut self, i: usize, tag: u64) {
        if let Some(l) = self.layers.get_mut(i) {
            l.wanted = Some(tag);
        }
    }

    /// Whether the sheet `tag` is one of these layers'.
    pub fn wants(&self, tag: u64) -> bool {
        self.layers.iter().any(|l| l.wanted == Some(tag))
    }

    /// The sheet `tag` arrived as `buffers` (every `step`th frame): the
    /// buffers it replaces.
    pub fn arrived(&mut self, tag: u64, buffers: Vec<B>, step: usize) -> Vec<B> {
        let Some(l) = self.layers.iter_mut().find(|l| l.wanted == Some(tag)) else {
            return buffers;
        };
        l.wanted = None;
        let new = Painted {
            buffers,
            step: step.max(1),
        };
        (l.sheet.replace(new)).map_or(Vec::new(), |s| s.buffers)
    }

    /// What layer `i` shows at `now` (its old sheet until the new one is
    /// in); None until it has one.
    pub fn shown(&self, i: usize, player: &Player, now: f64) -> Option<&B> {
        let l = self.layers.get(i)?;
        let s = l.sheet.as_ref()?;
        let frame = player.frame(&l.part.node, now);
        let k = (frame / s.step).min(s.buffers.len().checked_sub(1)?);
        s.buffers.get(k)
    }

    /// How soon a layer shows another frame, at most `fps` a second.
    pub fn due(&self, player: &Player, now: f64, fps: f64) -> Option<f64> {
        (self.layers.iter())
            .filter(|l| l.sheet.is_some())
            .filter_map(|l| player.due(&l.part.node, now, 1.0 / fps))
            .reduce(f64::min)
    }

    /// The nodes the layers play, for `Player::sync`.
    pub fn nodes(&self) -> impl Iterator<Item = &ui::Node> {
        self.layers.iter().map(|l| &l.part.node)
    }

    /// Every buffer, for destroying.
    pub fn drain(&mut self) -> Vec<B> {
        (self.layers.drain(..))
            .flat_map(|l| l.sheet.map(|s| s.buffers).unwrap_or_default())
            .collect()
    }
}

/// One timer per surface, set only once the last frame it drew is on
/// screen.
#[derive(Debug, Default)]
pub struct Pacer {
    /// A timer is set.
    timer: bool,
    /// A frame was drawn, and the compositor hasn't said it showed it.
    waiting: bool,
}

impl Pacer {
    /// The timer to set for something due in `due` seconds, if none is
    /// set and the compositor is showing what was drawn.
    pub fn arm(&mut self, due: Option<f64>) -> Option<Duration> {
        if self.timer || self.waiting {
            return None;
        }
        let due = due?;
        self.timer = true;
        Some(Duration::from_secs_f64(due.max(0.0)))
    }

    /// The timer fired.
    pub fn fired(&mut self) {
        self.timer = false;
    }

    /// A frame was drawn and committed with a frame callback.
    pub fn drew(&mut self) {
        self.waiting = true;
    }

    pub fn waiting(&self) -> bool {
        self.waiting
    }

    /// The frame callback came.
    pub fn shown(&mut self) {
        self.waiting = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wl_shell_render::ui::{Drive, Frames, Kind, Node, Rect};

    /// A spinner, with `mark` in its drawing (another `mark`, other
    /// frames).
    fn spinner(mark: f32) -> LivePart {
        let frames = (0..24)
            .map(|i| vec![ui::Cmd::Rotate(i as f32 + mark)])
            .collect();
        let mut node = Node::new(Kind::Canvas {
            viewbox: (100.0, 100.0),
            commands: Vec::new(),
            anim: Some(Box::new(Frames {
                frames,
                drive: Drive::Loop {
                    period: 1.2,
                    mirror: false,
                },
            })),
        });
        node.key = Some("spin".into());
        LivePart {
            index: 3,
            rect: Rect {
                x: 10.0,
                y: 10.0,
                w: 24.0,
                h: 24.0,
            },
            alpha: 1.0,
            node,
        }
    }

    fn base() -> ui::Request {
        ui::Request::new(
            Arc::new(Node::new(Kind::Col)),
            ui::Bounds::Exact(48.0, 48.0),
            2,
            Arc::new(ui::Theme::default()),
        )
    }

    /// A spinner turns between trees with no Lua call and no tree
    /// paint: its sheet is painted once, then each tick only picks
    /// another of its buffers. A tree that changes only the label keeps
    /// the sheet and the phase; new frames are painted again.
    #[test]
    fn spinners_turn_without_paints() {
        let mut sheets: Sheets<u32> = Sheets::default();
        let mut player = Player::default();
        let mut paints = 0;
        // A new tree: the buffers that went (from `sync` and `arrived`).
        let mut tree = |sheets: &mut Sheets<u32>, player: &mut Player, mark: f32, now: f64| {
            let (paint, mut gone) = sheets.sync(vec![spinner(mark)], &base());
            player.sync(sheets.nodes(), now);
            for (i, req) in paint {
                paints += 1;
                assert_eq!(req.bounds, ui::Bounds::Exact(24.0, 24.0));
                let tag = 100 + paints;
                sheets.asked(i, tag);
                assert!(sheets.wants(tag));
                // 24 frames, one buffer each (u32s standing in).
                let buffers = (0..24).map(|b| b + 1000 * paints as u32).collect();
                gone.extend(sheets.arrived(tag, buffers, 1));
            }
            gone
        };
        assert!(tree(&mut sheets, &mut player, 0.0, 0.0).is_empty());
        // Two seconds of ticks, as the pacer would set them.
        let due = sheets.due(&player, 0.0, TILE_FPS).unwrap();
        assert!((due - 0.05).abs() < 1e-6, "24 frames in 1.2 s: {due}");
        let seen: Vec<u32> = (0..40)
            .map(|k| *sheets.shown(0, &player, k as f64 * due).unwrap())
            .collect();
        assert_eq!(seen[..3], [1000, 1001, 1002]);
        assert_eq!(seen[24], 1000, "round again");
        // A redraw for a new label: no paint, the same phase.
        let at = *sheets.shown(0, &player, 0.6).unwrap();
        assert!(tree(&mut sheets, &mut player, 0.0, 0.55).is_empty());
        assert_eq!(*sheets.shown(0, &player, 0.6).unwrap(), at);
        // Other frames: painted again; the old ones go once the new are in.
        let gone = tree(&mut sheets, &mut player, 1.0, 0.7);
        assert_eq!(gone.len(), 24);
        assert_eq!(*sheets.shown(0, &player, 0.7).unwrap(), 2000);
        // Gone: its buffers go.
        let (paint, stale) = sheets.sync(Vec::new(), &base());
        assert!(paint.is_empty() && stale.len() == 24);
        // In all: one paint per new drawing, none per frame.
        assert_eq!(paints, 2);
    }

    #[test]
    fn without_frame_callbacks_no_ticks() {
        let mut p = Pacer::default();
        assert_eq!(p.arm(None), None);
        assert_eq!(p.arm(Some(0.05)), Some(Duration::from_millis(50)));
        // One timer at a time.
        assert_eq!(p.arm(Some(0.01)), None);
        p.fired();
        p.drew();
        // The frame isn't shown (a hidden dock): nothing is set.
        assert_eq!(p.arm(Some(0.05)), None);
        assert_eq!(p.arm(Some(0.05)), None);
        p.shown();
        assert!(p.arm(Some(0.05)).is_some());
    }
}

