//! Window preview cards, as wl-switcher's strip and wl-dock's strip (and
//! its launcher's Windows…) show them: a window's capture on a
//! subsurface, with its shadow in four strips around it (shapes.rs) that
//! scale with the card.
//!
//! - [`Preview`] is a window's capture and its latest frame. A capture
//!   starts static (one frame, so a strip can open without placeholders),
//!   goes live while its card is shown, and pauses again (`set_live`). A
//!   frame handed over but never attached is given back to the capture
//!   when the next one replaces it, since the compositor never releases
//!   it.
//! - [`Card`] is the subsurfaces of one card. `cards` creates a strip's
//!   worth with every shadow below every preview, so a card's shadow never
//!   covers its neighbor.
//! - `aspect` and `stagger` are the layout rules both strips share: how a
//!   window's aspect ratio is sanitized, and how cards further from where
//!   they grow out of start later.

use smithay_client_toolkit::compositor::Region;
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_output::Transform, wl_surface::WlSurface},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::Globals;
use wl_shell_core::{Program, Shell};
use wl_shell_log::warn;

use crate::geom::{self, IRect, Rect};
use crate::shapes::{Shape, shadow_side};
use crate::sub::Sub;

/// The aspect ratio (width / height) of a window whose size isn't known.
pub const DEFAULT_ASPECT: f64 = 16.0 / 10.0;
/// Aspect ratios are clamped to this range, so an odd window can't make a
/// sliver of a card.
const ASPECTS: (f64, f64) = (0.25, 4.0);
/// Morph delay per card of distance, as a fraction of the morph.
pub const STAGGER: f64 = 0.05;

/// The aspect ratio to lay out: `a`, sanitized.
pub fn aspect(a: f64) -> f64 {
    if a.is_finite() && a > 0.0 {
        a.clamp(ASPECTS.0, ASPECTS.1)
    } else {
        DEFAULT_ASPECT
    }
}

/// The morph progress of a card `k` cards away from where the morph
/// starts, at overall progress `morph`: cards further away start later and
/// catch up.
pub fn stagger(morph: f64, k: usize) -> f64 {
    let delay = (k as f64 * STAGGER).min(0.5);
    ((morph - delay) / (1.0 - delay)).clamp(0.0, 1.0)
}

/// A capture frame handed to the program.
pub struct Shown {
    pub buffer: WlBuffer,
    pub width: u32,
    pub height: u32,
    pub transform: Transform,
    /// Attached at least once, so the compositor will release it.
    used: bool,
}

impl Shown {
    /// Its size as shown, in buffer pixels: turned by its transform.
    pub fn size(&self) -> (u32, u32) {
        upright(self.width, self.height, self.transform)
    }
}

/// `w` x `h` turned by `transform`.
fn upright(w: u32, h: u32, transform: Transform) -> (u32, u32) {
    let swaps = matches!(
        transform,
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270
    );
    if swaps { (h, w) } else { (w, h) }
}

/// What a capture event changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Update {
    /// A new frame arrived, or capturing failed.
    pub shown: bool,
    /// The window's aspect ratio changed (or became known).
    pub resized: bool,
}

/// A window's capture and its latest frame.
pub struct Preview {
    /// The window's ext identifier.
    pub window: String,
    pub id: CaptureId,
    pub capture: Capture,
    frame: Option<Shown>,
}

impl Preview {
    /// Capture `toplevel` (the window `window`), one frame or `live`.
    pub fn new<P: Program>(
        window: String,
        toplevel: &ExtForeignToplevelHandleV1,
        g: &Globals,
        live: bool,
        qh: &QueueHandle<Shell<P>>,
    ) -> Self {
        let id = CaptureId::next();
        Self {
            window,
            id,
            capture: Capture::window(id, toplevel, g, live, false, qh),
            frame: None,
        }
    }

    /// The window's aspect ratio, once known.
    pub fn aspect(&self) -> Option<f64> {
        let (w, h) = match (&self.frame, self.capture.size()) {
            (Some(f), _) => f.size(),
            (None, Some((w, h))) => (w, h),
            (None, None) => return None,
        };
        (w > 0 && h > 0).then(|| w as f64 / h as f64)
    }

    /// The latest frame, unless it is from before a reallocation (and gone).
    pub fn frame(&self) -> Option<&Shown> {
        (self.frame.as_ref()).filter(|f| self.capture.owns(&f.buffer))
    }

    /// Run `f` on the capture and keep the frame it reports.
    pub fn update<P: Program>(
        &mut self,
        g: &Globals,
        qh: &QueueHandle<Shell<P>>,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) -> Update {
        let before = self.aspect();
        let shown = match f(&mut self.capture, g) {
            Some(CaptureEvent::Frame(frame)) => {
                // A frame never attached is never released: free it here.
                if let Some(old) = self.frame.take()
                    && !old.used
                {
                    self.capture.discard(&old.buffer, qh);
                }
                self.frame = Some(Shown {
                    buffer: frame.buffer,
                    width: frame.width,
                    height: frame.height,
                    transform: frame.transform,
                    used: false,
                });
                true
            }
            Some(CaptureEvent::Failed(why)) => {
                warn!("capture of '{}' failed: {why}", self.window);
                true
            }
            None => false,
        };
        Update {
            shown,
            resized: self.aspect() != before,
        }
    }

    /// Capture continuously, or pause after the frame in flight.
    pub fn set_live<P: Program>(&mut self, live: bool, g: &Globals, qh: &QueueHandle<Shell<P>>) {
        self.capture.set_live(live, g, qh);
    }
}

/// What a card shows.
pub enum Content<'a> {
    /// A window's latest frame, or `placeholder` (a 1x1 buffer) until
    /// there is one.
    Preview {
        preview: Option<&'a mut Preview>,
        placeholder: &'a WlBuffer,
    },
    /// Part of a buffer `size` pixels large (`src`: x, y, w, h in buffer
    /// pixels), or all of it: an icon, say.
    Image {
        buffer: &'a WlBuffer,
        size: (f64, f64),
        src: Option<[f64; 4]>,
    },
}

/// The subsurfaces of one card: its shadow's sides (top, bottom, left,
/// right) and the preview.
pub struct Card {
    pub shadow: [Sub; 4],
    pub preview: Sub,
}

/// `n` cards on `parent`, above its existing children: every shadow below
/// every preview.
pub fn cards<P: Program>(
    g: &Globals,
    parent: &WlSurface,
    no_input: &Region,
    n: usize,
    qh: &QueueHandle<Shell<P>>,
) -> Vec<Card> {
    let sub = || Sub::new(g, parent, no_input, qh);
    let shadows: Vec<[Sub; 4]> = (0..n).map(|_| [sub(), sub(), sub(), sub()]).collect();
    (shadows.into_iter())
        .map(|shadow| Card {
            shadow,
            preview: sub(),
        })
        .collect()
}

/// Show `buffer` (`size` pixels, showing `src` of it) on `sub` at `rect`,
/// cut to `clip`, or hide it if nothing is left or it is transparent.
/// Returns whether the buffer was newly attached. Doesn't commit.
fn put(
    sub: &mut Sub,
    buffer: &WlBuffer,
    size: (f64, f64),
    src: Option<[f64; 4]>,
    rect: Rect,
    clip: Option<Rect>,
    alpha: f64,
) -> bool {
    let part = match clip {
        Some(c) => geom::clip(rect, size, src, c),
        None => Some((rect, src)),
    };
    match part {
        Some((rect, src)) if alpha > 0.0 => {
            sub.crop(src);
            let attached = sub.show(buffer);
            sub.place(IRect::round(rect));
            sub.set_alpha(alpha);
            attached
        }
        _ => {
            sub.hide();
            false
        }
    }
}

impl Card {
    /// Show `content` at `rect` with `alpha`, and its shadow (`shadow`, as
    /// `Cache::card_shadow` rendered it for a card `rendered` in size at
    /// `scale`) scaled with it; only what is inside `clip`, if given.
    /// Commits the subsurfaces.
    #[allow(clippy::too_many_arguments)]
    pub fn place(
        &mut self,
        content: Option<Content>,
        rect: Rect,
        shadow: Option<&[Option<Shape>; 4]>,
        rendered: (f64, f64),
        scale: f64,
        alpha: f64,
        clip: Option<Rect>,
    ) {
        let sub = &mut self.preview;
        match content {
            Some(Content::Preview {
                preview,
                placeholder,
            }) => match preview.filter(|p| p.frame().is_some()) {
                Some(p) => {
                    let f = p.frame.as_mut().expect("filtered");
                    let (w, h) = f.size();
                    sub.set_transform(f.transform);
                    let size = (w as f64, h as f64);
                    // Attaching it (again) makes it busy until released.
                    if put(sub, &f.buffer, size, None, rect, clip, alpha) {
                        f.used = true;
                        p.capture.attached(&f.buffer);
                    }
                }
                None => {
                    sub.set_transform(Transform::Normal);
                    put(sub, placeholder, (1.0, 1.0), None, rect, clip, alpha);
                }
            },
            Some(Content::Image { buffer, size, src }) => {
                sub.set_transform(Transform::Normal);
                put(sub, buffer, size, src, rect, clip, alpha);
            }
            None => sub.hide(),
        }
        sub.commit();

        for (k, sub) in self.shadow.iter_mut().enumerate() {
            match shadow.and_then(|s| s[k].as_ref()) {
                Some(sh) => {
                    let r = shadow_side(sh.rect, rect, rendered);
                    let size = (sh.rect.w as f64 * scale, sh.rect.h as f64 * scale);
                    put(sub, &sh.buffer, size, None, r, clip, alpha);
                }
                None => sub.hide(),
            }
            sub.commit();
        }
    }

    /// Hide everything and commit.
    pub fn clear(&mut self) {
        self.preview.clear();
        self.shadow.iter_mut().for_each(Sub::clear);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspects_are_sanitized() {
        assert_eq!(aspect(1.5), 1.5);
        assert_eq!(aspect(10.0), 4.0);
        assert_eq!(aspect(0.1), 0.25);
        assert_eq!(aspect(f64::NAN), DEFAULT_ASPECT);
        assert_eq!(aspect(0.0), DEFAULT_ASPECT);
    }

    #[test]
    fn transforms_turn_sizes() {
        assert_eq!(upright(100, 50, Transform::Normal), (100, 50));
        assert_eq!(upright(100, 50, Transform::_90), (50, 100));
        assert_eq!(upright(100, 50, Transform::Flipped180), (100, 50));
    }

    #[test]
    fn staggered_morph() {
        assert_eq!(stagger(0.0, 0), 0.0);
        assert_eq!(stagger(0.5, 0), 0.5);
        assert_eq!(stagger(1.0, 0), 1.0);
        // Later cards start later, all finish together.
        assert_eq!(stagger(0.05, 1), 0.0);
        assert!(stagger(0.5, 2) < stagger(0.5, 1));
        assert_eq!(stagger(1.0, 30), 1.0);
        assert!(stagger(0.9, 30) > 0.0);
    }
}
