//! The close button shown on the selected thumbnail: a round button with a
//! cross, drawn with signed distances, over a drop shadow that follows its
//! shape. Pure, unit tested.

use wl_shell_render::{Image, shadow};

/// What the button looks like, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub radius: u32,
    /// 0xRRGGBBAA: the circle, the circle under the pointer, and the cross.
    pub background: u32,
    pub hover_background: u32,
    pub foreground: u32,
    pub shadow: Option<shadow::IconShadow>,
}

impl Look {
    /// The button's diameter.
    pub fn size(&self) -> i32 {
        2 * self.radius as i32
    }

    /// How far the image reaches past the button on every side, for the
    /// shadow.
    pub fn pad(&self) -> i32 {
        self.shadow.map_or(0, |s| s.pad())
    }
}

/// The button at `scale`, `hovered` or not: (size + 2 pad) * scale pixels
/// square, premultiplied ARGB8888.
pub fn render(look: &Look, scale: i32, hovered: bool) -> Image {
    let scale = scale.max(1);
    let button = render_button(look, scale, hovered);
    match &look.shadow {
        Some(s) => shadow::icon(&button, s, scale),
        None => button,
    }
}

/// The button alone, size * scale pixels square.
fn render_button(look: &Look, scale: i32, hovered: bool) -> Image {
    let s = scale as f64;
    let side = (look.size() * scale).max(1) as u32;
    let c = side as f64 / 2.0;
    let r = look.radius as f64 * s;
    // The cross: two diagonals through the center.
    let arm = r * 0.36;
    let half_width = (r * 0.11).max(0.9 * s);

    let bg = premultiplied(if hovered {
        look.hover_background
    } else {
        look.background
    });
    let fg = premultiplied(look.foreground);
    let mut data = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            // Pixel centers.
            let (px, py) = (x as f64 + 0.5 - c, y as f64 + 0.5 - c);
            let circle = coverage(r - px.hypot(py));
            let line = segment(px, py, arm).min(segment(px, -py, arm));
            // The cross stays inside the circle.
            let cross = coverage(half_width - line).min(circle);
            let out = over(scaled(fg, cross), scaled(bg, circle));
            let [r, g, b, a] = out.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            data.extend_from_slice(&[b, g, r, a]);
        }
    }
    Image {
        width: side,
        height: side,
        data,
    }
}

/// An 0xRRGGBBAA color as premultiplied RGBA, 0 to 1.
fn premultiplied(rgba: u32) -> [f64; 4] {
    let [r, g, b, a] = rgba.to_be_bytes().map(|v| v as f64 / 255.0);
    [r * a, g * a, b * a, a]
}

fn scaled(c: [f64; 4], k: f64) -> [f64; 4] {
    c.map(|v| v * k)
}

/// Premultiplied `src` over `dst`.
fn over(src: [f64; 4], dst: [f64; 4]) -> [f64; 4] {
    let k = 1.0 - src[3];
    [0, 1, 2, 3].map(|i| src[i] + dst[i] * k)
}

/// How much of a pixel is inside, from its signed distance to the edge
/// (positive inside).
fn coverage(inside: f64) -> f64 {
    (inside + 0.5).clamp(0.0, 1.0)
}

/// Distance from (`x`, `y`) to the segment from (-`arm`, -`arm`) to
/// (`arm`, `arm`).
fn segment(x: f64, y: f64, arm: f64) -> f64 {
    // Along the diagonal, clamped to the segment, then the distance to it.
    let t = ((x + y) / 2.0).clamp(-arm, arm);
    (x - t).hypot(y - t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.data[i..i + 4].try_into().unwrap()
    }

    const SHADOW: shadow::IconShadow = shadow::IconShadow {
        color: 0x00000080,
        softness: 6,
        offset: (0, 2),
    };

    fn look(radius: u32, shadow: Option<shadow::IconShadow>) -> Look {
        Look {
            radius,
            background: 0x202020ff,
            hover_background: 0x606060ff,
            foreground: 0xffffffff,
            shadow,
        }
    }

    #[test]
    fn button() {
        for scale in [1, 2] {
            let look = look(13, None);
            let img = render(&look, scale, false);
            let side = (26 * scale) as u32;
            assert_eq!((img.width, img.height), (side, side));
            assert_eq!(img.data.len(), (side * side * 4) as usize);
            let c = side / 2;
            // The cross at the center, the background beside it.
            assert_eq!(pixel(&img, c, c), [255, 255, 255, 255]);
            assert_eq!(pixel(&img, c, c - 9 * scale as u32), [32, 32, 32, 255]);
            // Nothing in the corners.
            assert_eq!(pixel(&img, 0, 0), [0, 0, 0, 0]);
            // Premultiplied: no channel above alpha.
            assert!(
                img.data
                    .chunks(4)
                    .all(|p| p[..3].iter().all(|&v| v <= p[3]))
            );
        }
    }

    #[test]
    fn hovered() {
        let img = render(&look(13, None), 1, true);
        // Lighter, the cross unchanged.
        assert_eq!(pixel(&img, 13, 4), [96, 96, 96, 255]);
        assert_eq!(pixel(&img, 13, 13), [255, 255, 255, 255]);
    }

    #[test]
    fn radius() {
        let small = render(&look(8, None), 1, false);
        let large = render(&look(20, None), 2, false);
        assert_eq!(small.width, 16);
        assert_eq!(large.width, 80);
        assert_eq!(pixel(&large, 40, 40), [255, 255, 255, 255]);
    }

    #[test]
    fn shadow() {
        let look = look(13, Some(SHADOW));
        let pad = look.pad();
        assert!(pad > 0);
        let img = render(&look, 2, false);
        let side = ((26 + 2 * pad) * 2) as u32;
        assert_eq!((img.width, img.height), (side, side));
        let c = side / 2;
        // The button is still centered, its shadow just below it.
        assert_eq!(pixel(&img, c, c), [255, 255, 255, 255]);
        let below = c + 13 * 2 + 2;
        assert!(pixel(&img, c, below)[3] > 0);
        assert_eq!(pixel(&img, 0, 0)[3], 0);
    }
}
