//! Shapes rendered in software and shown on subsurfaces: a panel with its
//! shadow, rounded rects, rows of dots, and the shadows of cards (window
//! previews). `ShapeKey` says what to render; the cache (cache.rs) keeps
//! the buffers. Placing them is here too:
//!
//! - A panel is shown in three slices, so it can grow along its axis
//!   without stretching its corners: the two ends as rendered, the middle
//!   stretched between them (`panel_slices`, `place_panel`).
//! - A card's shadow is four strips around it (top, bottom, left, right),
//!   so the card itself can be a separate surface; they scale with the
//!   card as it zooms or morphs (`shadow_side`, `place_card_shadow`).

use wayland_client::protocol::wl_buffer::WlBuffer;

use wl_shell_render::{Image, shadow, text};

use crate::geom::{IRect, Rect};
use crate::sub::Sub;

/// A rendered shape: its buffer, and where it goes relative to what it
/// was rendered for (a panel's shadow reaches past the panel).
#[derive(Clone, Debug)]
pub struct Shape {
    pub buffer: WlBuffer,
    /// Logical pixels.
    pub rect: shadow::Rect,
}

/// What a shape was rendered for.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeKey {
    Panel {
        w: i32,
        h: i32,
        radius: i32,
        color: u32,
        shadow: Option<shadow::Settings>,
        scale: i32,
    },
    /// A rounded rect.
    Rect {
        w: i32,
        h: i32,
        radius: i32,
        color: u32,
        scale: i32,
    },
    /// A row of `count` round dots of `size`, `size` apart.
    Dots {
        count: u32,
        size: i32,
        color: u32,
        scale: i32,
    },
    /// One side (top, bottom, left, right) of the shadow of a `w`x`h`
    /// card with square corners; its rect is relative to the card.
    CardShadow {
        w: i32,
        h: i32,
        shadow: shadow::Settings,
        side: u8,
        scale: i32,
    },
}

impl ShapeKey {
    /// The image and where it goes; None if there is nothing to draw.
    pub fn render(&self) -> Option<(Image, shadow::Rect)> {
        let rect = |w, h| shadow::Rect { x: 0, y: 0, w, h };
        match *self {
            ShapeKey::Panel {
                w,
                h,
                radius,
                color,
                shadow,
                scale,
            } => {
                let p = shadow::panel(w, h, radius as f64, color, shadow.as_ref(), scale);
                Some((p.image, p.rect))
            }
            ShapeKey::Rect {
                w,
                h,
                radius,
                color,
                scale,
            } => {
                let img = text::rounded_rect(w * scale, h * scale, (radius * scale) as f64, color);
                Some((img, rect(w, h)))
            }
            ShapeKey::Dots {
                count,
                size,
                color,
                scale,
            } => {
                let count = count.max(1) as i32;
                let w = (2 * count - 1) * size;
                Some((dots(count, size * scale, color), rect(w, size)))
            }
            ShapeKey::CardShadow {
                w,
                h,
                shadow,
                side,
                scale,
            } => {
                let strip = shadow::render(w, h, &shadow, scale)
                    .into_iter()
                    .nth(side as usize)??;
                Some((
                    strip_image(strip.width, strip.height, strip.pixels),
                    strip.rect,
                ))
            }
        }
    }
}

/// A rendered shadow strip's pixels as an image.
pub(crate) fn strip_image(width: u32, height: u32, data: Vec<u8>) -> Image {
    Image {
        width,
        height,
        data,
    }
}

/// `count` round dots of `size` pixels, `size` apart, in one row.
fn dots(count: i32, size: i32, color: u32) -> Image {
    let size = size.max(1);
    let dot = text::rounded_rect(size, size, size as f64 / 2.0, color);
    let w = ((2 * count - 1) * size) as u32;
    let mut data = vec![0; (w * size as u32 * 4) as usize];
    let row = size as usize * 4;
    for k in 0..count as usize {
        for y in 0..size as usize {
            let dst = (y * w as usize + k * 2 * size as usize) * 4;
            data[dst..dst + row].copy_from_slice(&dot.data[y * row..(y + 1) * row]);
        }
    }
    Image {
        width: w,
        height: size as u32,
        data,
    }
}

/// The direction a panel grows in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// The three slices of a panel shape (`shape`: where its image goes
/// relative to the panel it was rendered for, `rendered` wide and tall)
/// shown for a panel now at `panel`, stretched along `axis`: per slice,
/// where it goes (logical pixels) and the part of the buffer it shows
/// (x, y, w, h in buffer pixels, at `scale`).
pub fn panel_slices(
    shape: shadow::Rect,
    panel: Rect,
    rendered: (f64, f64),
    axis: Axis,
    scale: f64,
) -> [(Rect, [f64; 4]); 3] {
    let (rw, rh) = (shape.w as f64, shape.h as f64);
    let (x, y) = (panel.x + shape.x as f64, panel.y + shape.y as f64);
    match axis {
        Axis::Horizontal => {
            let full = Rect::new(x, y, rw + (panel.w - rendered.0), rh);
            let cap = ((rw / 2.0).floor() - 1.0).max(1.0);
            let src = |sx: f64, sw: f64| [sx * scale, 0.0, sw * scale, rh * scale];
            [
                (Rect::new(full.x, y, cap, rh), src(0.0, cap)),
                (
                    Rect::new(full.x + cap, y, full.w - 2.0 * cap, rh),
                    src(cap, rw - 2.0 * cap),
                ),
                (
                    Rect::new(full.right() - cap, y, cap, rh),
                    src(rw - cap, cap),
                ),
            ]
        }
        Axis::Vertical => {
            let full = Rect::new(x, y, rw, rh + (panel.h - rendered.1));
            let cap = ((rh / 2.0).floor() - 1.0).max(1.0);
            let src = |sy: f64, sh: f64| [0.0, sy * scale, rw * scale, sh * scale];
            [
                (Rect::new(x, full.y, rw, cap), src(0.0, cap)),
                (
                    Rect::new(x, full.y + cap, rw, full.h - 2.0 * cap),
                    src(cap, rh - 2.0 * cap),
                ),
                (
                    Rect::new(x, full.bottom() - cap, rw, cap),
                    src(rh - cap, cap),
                ),
            ]
        }
    }
}

/// Show a panel `shape` (rendered for a panel `rendered` in size) on the
/// three `subs` for a panel now at `panel`, transformed by `xf`, or hide
/// them without a shape or if fully transparent. Commits the subs.
#[allow(clippy::too_many_arguments)]
pub fn place_panel(
    subs: &mut [Sub; 3],
    shape: Option<&Shape>,
    panel: Rect,
    rendered: (f64, f64),
    axis: Axis,
    scale: f64,
    xf: &dyn Fn(Rect) -> IRect,
    alpha: f64,
) {
    match shape {
        Some(shape) if alpha > 0.0 => {
            let slices = panel_slices(shape.rect, panel, rendered, axis, scale);
            for (sub, (rect, src)) in subs.iter_mut().zip(slices) {
                sub.show(&shape.buffer);
                sub.crop(Some(src));
                sub.place(xf(rect));
                sub.set_alpha(alpha);
                sub.commit();
            }
        }
        _ => subs.iter_mut().for_each(Sub::clear),
    }
}

/// Where a side of a card's shadow (`side`, relative to the card it was
/// rendered for, `rendered` in size) goes for the card drawn at `card`:
/// it scales with the card.
pub fn shadow_side(side: shadow::Rect, card: Rect, rendered: (f64, f64)) -> Rect {
    let (sx, sy) = (card.w / rendered.0, card.h / rendered.1);
    Rect::new(
        card.x + side.x as f64 * sx,
        card.y + side.y as f64 * sy,
        side.w as f64 * sx,
        side.h as f64 * sy,
    )
}

/// Show the sides of a card's shadow (as `Cache::card_shadow` returns
/// them, for a card `rendered` in size) on the four `subs` around the card
/// drawn at `card`, or hide them. Commits the subs.
pub fn place_card_shadow(
    subs: &mut [Sub; 4],
    sides: Option<&[Option<Shape>; 4]>,
    card: Rect,
    rendered: (f64, f64),
    alpha: f64,
) {
    for (k, sub) in subs.iter_mut().enumerate() {
        match sides.and_then(|s| s[k].as_ref()) {
            Some(sh) => sub.present(
                Some(&sh.buffer),
                shadow_side(sh.rect, card, rendered),
                alpha,
            ),
            None => sub.clear(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_row() {
        let img = dots(3, 4, 0xffffffff);
        assert_eq!((img.width, img.height), (20, 4));
        let alpha = |x: u32, y: u32| img.data[((y * 20 + x) * 4 + 3) as usize];
        // Dot centers are opaque, the spaces between them clear.
        for k in 0..3 {
            assert_eq!(alpha(k * 8 + 2, 2), 255);
        }
        assert_eq!(alpha(5, 2), 0);
        assert_eq!(alpha(13, 2), 0);
    }

    #[test]
    fn shape_sizes() {
        let key = ShapeKey::Dots {
            count: 2,
            size: 5,
            color: 0xffffffff,
            scale: 2,
        };
        let (img, rect) = key.render().unwrap();
        assert_eq!((rect.w, rect.h), (15, 5));
        assert_eq!((img.width, img.height), (30, 10));
        let key = ShapeKey::Rect {
            w: 30,
            h: 20,
            radius: 4,
            color: 0xffffffff,
            scale: 3,
        };
        let (img, rect) = key.render().unwrap();
        assert_eq!((rect.w, rect.h), (30, 20));
        assert_eq!((img.width, img.height), (90, 60));
        let shadow = shadow::Settings {
            color: 0x00000080,
            softness: 10.0,
            spread: 0,
            offset: (0, 4),
        };
        let key = |side| ShapeKey::CardShadow {
            w: 100,
            h: 60,
            shadow,
            side,
            scale: 2,
        };
        let (img, rect) = key(2).render().unwrap();
        assert!(rect.x < 0 && rect.w > 0);
        assert_eq!(
            (img.width, img.height),
            (rect.w as u32 * 2, rect.h as u32 * 2)
        );
        assert!(key(4).render().is_none());
    }

    /// A panel's image reaching 10 px past a 200x60 panel at (100, 50).
    fn panel_shape() -> (shadow::Rect, Rect) {
        let shape = shadow::Rect {
            x: -10,
            y: -10,
            w: 220,
            h: 80,
        };
        (shape, Rect::new(100.0, 50.0, 200.0, 60.0))
    }

    #[test]
    fn panel_slices_at_rest_tile_the_image() {
        let (shape, panel) = panel_shape();
        for (axis, along) in [(Axis::Horizontal, 220.0), (Axis::Vertical, 80.0)] {
            let s = panel_slices(shape, panel, (200.0, 60.0), axis, 2.0);
            // A slice's position and length along the axis.
            let along_axis = |r: &Rect| match axis {
                Axis::Horizontal => (r.x, r.w),
                Axis::Vertical => (r.y, r.h),
            };
            let pos = |r: &Rect| along_axis(r).0;
            let len = |r: &Rect| along_axis(r).1;
            let src = |i: usize| match axis {
                Axis::Horizontal => (s[i].1[0], s[i].1[2]),
                Axis::Vertical => (s[i].1[1], s[i].1[3]),
            };
            // Back to back, covering the image, each showing its own part
            // of the buffer at 2x.
            assert_eq!(pos(&s[0].0), pos(&Rect::new(90.0, 40.0, 0.0, 0.0)));
            for i in 0..2 {
                assert_eq!(pos(&s[i].0) + len(&s[i].0), pos(&s[i + 1].0));
                assert_eq!(src(i).0 + src(i).1, src(i + 1).0);
            }
            assert_eq!((0..3).map(|i| len(&s[i].0)).sum::<f64>(), along);
            for (i, (r, _)) in s.iter().enumerate() {
                assert_eq!(src(i).1, len(r) * 2.0);
            }
        }
    }

    #[test]
    fn panel_slices_stretch_the_middle_only() {
        let (shape, panel) = panel_shape();
        let rest = panel_slices(shape, panel, (200.0, 60.0), Axis::Horizontal, 1.0);
        let wide = Rect::new(50.0, 50.0, 300.0, 60.0);
        let s = panel_slices(shape, wide, (200.0, 60.0), Axis::Horizontal, 1.0);
        assert_eq!(s[0].0.w, rest[0].0.w);
        assert_eq!(s[2].0.w, rest[2].0.w);
        assert_eq!(s[1].0.w, rest[1].0.w + 100.0);
        assert_eq!(s[0].0.x, 40.0);
        assert_eq!(s[2].0.right(), 360.0);
        // The buffer parts don't change.
        for i in 0..3 {
            assert_eq!(s[i].1, rest[i].1);
            assert_eq!((s[i].0.y, s[i].0.h), (40.0, 80.0));
        }
        let tall = Rect::new(100.0, 0.0, 200.0, 160.0);
        let s = panel_slices(shape, tall, (200.0, 60.0), Axis::Vertical, 1.0);
        assert_eq!(s[0].0.y, -10.0);
        assert_eq!(s[2].0.bottom(), 170.0);
        assert!(s.iter().all(|(r, _)| (r.x, r.w) == (90.0, 220.0)));
    }

    #[test]
    fn shadow_sides_scale_with_the_card() {
        let side = shadow::Rect {
            x: -8,
            y: 60,
            w: 116,
            h: 12,
        };
        let card = Rect::new(500.0, 300.0, 100.0, 60.0);
        assert_eq!(
            shadow_side(side, card, (100.0, 60.0)),
            Rect::new(492.0, 360.0, 116.0, 12.0)
        );
        let half = Rect::new(500.0, 300.0, 50.0, 30.0);
        assert_eq!(
            shadow_side(side, half, (100.0, 60.0)),
            Rect::new(496.0, 330.0, 58.0, 6.0)
        );
    }
}
