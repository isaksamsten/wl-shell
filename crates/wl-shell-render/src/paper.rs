//! Sheets of paper, as wl-note shows its notes: tucked behind a screen
//! edge, with only a sliver of them sticking out.
//!
//! A sheet is its color with a slight vertical gradient (lighter at the
//! top, as if lit from above) and a grain: a tileable value noise at a few
//! percent of contrast, made once, that makes it read as paper rather than
//! a rectangle. Its corners are barely rounded, and the bottom corner on
//! the side that sticks out from the edge curls: a lighter triangle folded
//! over, with the corner cut away under it, so the shadow is pulled in
//! there too. The shadow (shadow.rs's `icon`, of the sheet's own shape)
//! falls down and away from the edge.
//!
//! A `sliver` is the sheet as tucked: a little darker, since it is seen
//! edge-on, with a 1 px light line along the side that sticks out, where
//! the paper's thickness catches the light.
//!
//! Subsurfaces can't rotate, so a sheet's small tilt is drawn into it.

use std::sync::OnceLock;

use resvg::tiny_skia::{
    Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point,
    Rect as SkRect, SpreadMode, Transform,
};

use crate::image::Image;
use crate::shadow::{self, IconShadow};

/// The screen edge a sheet is tucked behind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    #[default]
    Right,
}

/// What to draw. Sizes are logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Paper {
    pub w: i32,
    pub h: i32,
    /// 0xRRGGBBAA, not premultiplied.
    pub color: u32,
    pub edge: Edge,
    /// Degrees, clockwise.
    pub tilt: f64,
    /// Drawn as tucked: darker, with a light line.
    pub sliver: bool,
    /// The shadow; its x offset is away from the edge (positive moves it
    /// onto the screen).
    pub shadow: Option<IconShadow>,
    pub scale: i32,
}

/// A rendered sheet, and where its image goes relative to the sheet's top
/// left corner (it reaches past it for the tilt and the shadow).
#[derive(Clone, Debug, PartialEq)]
pub struct Rendered {
    pub image: Image,
    pub rect: shadow::Rect,
}

/// The grain's contrast: pixels are lightened or darkened by up to this.
const GRAIN: f32 = 0.035;
/// The grain's tile, in buffer pixels.
const TILE: usize = 128;
/// Corner radius, logical pixels.
const RADIUS: f32 = 1.5;
/// Light from above: the top this much lighter, the bottom darker.
const LIGHT: f64 = 0.06;
/// How much darker a sliver is.
const EDGE_ON: f64 = 0.1;

impl Paper {
    /// The curled corner's size.
    fn curl(&self) -> f32 {
        (self.w.min(self.h) as f32 * 0.18).clamp(5.0, 14.0)
    }

    /// The margins the tilt needs around the sheet, logical pixels.
    fn tilt_pad(&self) -> (i32, i32) {
        let t = self.tilt.to_radians().abs();
        let (w, h) = (self.w as f64, self.h as f64);
        let bw = w * t.cos() + h * t.sin();
        let bh = w * t.sin() + h * t.cos();
        let pad = |b: f64, s: f64| ((b - s) / 2.0).max(0.0).ceil() as i32 + 1;
        (pad(bw, w), pad(bh, h))
    }

    /// The shadow as `shadow::icon` takes it: moved away from the edge.
    fn placed_shadow(&self) -> Option<IconShadow> {
        self.shadow.map(|s| IconShadow {
            offset: match self.edge {
                Edge::Right => (-s.offset.0, s.offset.1),
                Edge::Left => s.offset,
            },
            ..s
        })
    }
}

pub fn render(p: &Paper) -> Rendered {
    let (w, h) = (p.w.max(1), p.h.max(1));
    let scale = p.scale.max(1);
    let (px, py) = p.tilt_pad();
    let (bw, bh) = (((w + 2 * px) * scale) as u32, ((h + 2 * py) * scale) as u32);
    let sheet = draw(p, w, h, (px, py), scale, bw, bh);
    let (image, pad) = match p.placed_shadow() {
        Some(s) => (shadow::icon(&sheet, &s, scale), s.pad()),
        None => (sheet, 0),
    };
    Rendered {
        image,
        rect: shadow::Rect {
            x: -(px + pad),
            y: -(py + pad),
            w: w + 2 * (px + pad),
            h: h + 2 * (py + pad),
        },
    }
}

/// The sheet alone, `bw`x`bh` buffer pixels, its top left corner at
/// `pad` logical pixels.
fn draw(p: &Paper, w: i32, h: i32, pad: (i32, i32), scale: i32, bw: u32, bh: u32) -> Image {
    let blank = || Image {
        width: bw,
        height: bh,
        data: vec![0; (bw * bh * 4) as usize],
    };
    let Some(mut pixmap) = Pixmap::new(bw, bh) else {
        return blank();
    };
    let (w, h, s) = (w as f32, h as f32, scale as f32);
    // Sheet coordinates: logical pixels from its top left corner, drawn
    // as if tucked behind the right edge (sticking out to the left), and
    // mirrored for the left edge.
    let mut xf = Transform::from_scale(s, s)
        .pre_translate(pad.0 as f32 + w / 2.0, pad.1 as f32 + h / 2.0)
        .pre_rotate(p.tilt as f32)
        .pre_translate(-w / 2.0, -h / 2.0);
    if p.edge == Edge::Left {
        xf = xf.pre_translate(w, 0.0).pre_scale(-1.0, 1.0);
    }
    let c = p.curl().min(w / 2.0).min(h / 2.0);
    let r = RADIUS.min(w / 4.0).min(h / 4.0);

    let mut pb = PathBuilder::new();
    pb.move_to(r, 0.0);
    pb.line_to(w - r, 0.0);
    pb.quad_to(w, 0.0, w, r);
    pb.line_to(w, h - r);
    pb.quad_to(w, h, w - r, h);
    pb.line_to(c, h);
    pb.line_to(0.0, h - c);
    pb.line_to(0.0, r);
    pb.quad_to(0.0, 0.0, r, 0.0);
    pb.close();
    let Some(outline) = pb.finish() else {
        return blank();
    };

    let base = match p.sliver {
        true => shade(p.color, -EDGE_ON),
        false => p.color,
    };
    let stops = vec![
        GradientStop::new(0.0, color(shade(base, LIGHT))),
        GradientStop::new(1.0, color(shade(base, -LIGHT * 0.8))),
    ];
    let fill = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(0.0, h),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    );
    let paint = match fill {
        Some(shader) => Paint {
            shader,
            anti_alias: true,
            ..Paint::default()
        },
        None => solid(base),
    };
    pixmap.fill_path(&outline, &paint, FillRule::Winding, xf, None);

    // The curl: the cut corner folded over, its back catching the light
    // along the fold.
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, h - c);
    pb.line_to(c, h);
    pb.line_to(c, h - c);
    pb.close();
    if let Some(flap) = pb.finish() {
        let stops = vec![
            GradientStop::new(0.0, color(shade(base, 0.3))),
            GradientStop::new(1.0, color(shade(base, 0.05))),
        ];
        // From the fold's middle toward the flap's tip.
        let fold = Point::from_xy(c / 2.0, h - c / 2.0);
        let tip = Point::from_xy(c, h - c);
        let paint =
            match LinearGradient::new(fold, tip, stops, SpreadMode::Pad, Transform::identity()) {
                Some(shader) => Paint {
                    shader,
                    anti_alias: true,
                    ..Paint::default()
                },
                None => solid(shade(base, 0.2)),
            };
        pixmap.fill_path(&flap, &paint, FillRule::Winding, xf, None);
    }

    if p.sliver
        && let Some(line) = SkRect::from_xywh(0.0, r, 1.0, (h - c - r).max(0.0))
    {
        pixmap.fill_rect(line, &solid(0xffffff70), xf, None);
    }

    grain(&mut pixmap);
    let mut data = Vec::with_capacity((bw * bh * 4) as usize);
    for px in pixmap.data().as_chunks::<4>().0 {
        // Premultiplied RGBA to premultiplied BGRA.
        data.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    Image {
        width: bw,
        height: bh,
        data,
    }
}

/// Lighten (`amount` > 0, toward white) or darken (toward black) an
/// 0xRRGGBBAA color, keeping its alpha.
fn shade(rgba: u32, amount: f64) -> u32 {
    let [r, g, b, a] = rgba.to_be_bytes();
    let f = |c: u8| {
        let c = c as f64;
        let v = match amount >= 0.0 {
            true => c + (255.0 - c) * amount,
            false => c * (1.0 + amount),
        };
        v.round().clamp(0.0, 255.0) as u8
    };
    u32::from_be_bytes([f(r), f(g), f(b), a])
}

fn color(rgba: u32) -> Color {
    let [r, g, b, a] = rgba.to_be_bytes();
    Color::from_rgba8(r, g, b, a)
}

fn solid(rgba: u32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color(rgba));
    p.anti_alias = true;
    p
}

/// Lighten and darken the drawn pixels by the grain.
fn grain(pixmap: &mut Pixmap) {
    let tile = grain_tile();
    let w = pixmap.width() as usize;
    for (i, px) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let a = px[3];
        if a == 0 {
            continue;
        }
        let (x, y) = (i % w, i / w);
        let f = 1.0 + tile[(y % TILE) * TILE + x % TILE] * GRAIN;
        for c in &mut px[..3] {
            *c = (*c as f32 * f).round().clamp(0.0, a as f32) as u8;
        }
    }
}

/// The grain: -1 to 1 per pixel of a `TILE`-square tile that repeats
/// seamlessly. Coarse value noise (fibers) under fine noise (tooth).
fn grain_tile() -> &'static [f32] {
    static TILE_DATA: OnceLock<Vec<f32>> = OnceLock::new();
    TILE_DATA.get_or_init(|| {
        const CELL: usize = 8;
        let cells = TILE / CELL;
        let lattice = |x: usize, y: usize| unit(hash((x % cells) as u32, (y % cells) as u32, 1));
        let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
        let mut out = Vec::with_capacity(TILE * TILE);
        for y in 0..TILE {
            for x in 0..TILE {
                let (cx, cy) = (x / CELL, y / CELL);
                let tx = smooth((x % CELL) as f32 / CELL as f32);
                let ty = smooth((y % CELL) as f32 / CELL as f32);
                let top = lattice(cx, cy) + (lattice(cx + 1, cy) - lattice(cx, cy)) * tx;
                let bottom =
                    lattice(cx, cy + 1) + (lattice(cx + 1, cy + 1) - lattice(cx, cy + 1)) * tx;
                let coarse = top + (bottom - top) * ty;
                let fine = unit(hash(x as u32, y as u32, 2));
                out.push(0.4 * coarse + 0.6 * fine);
            }
        }
        out
    })
}

/// A hash of a lattice point, for noise.
fn hash(x: u32, y: u32, seed: u32) -> u32 {
    let mut h =
        x.wrapping_mul(0x8da6_b343) ^ y.wrapping_mul(0xd816_3841) ^ seed.wrapping_mul(0xcb1a_b31f);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// A hash as -1 to 1.
fn unit(h: u32) -> f32 {
    h as f32 / u32::MAX as f32 * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paper() -> Paper {
        Paper {
            w: 120,
            h: 60,
            color: 0xfff3a0ff,
            edge: Edge::Right,
            tilt: 0.0,
            sliver: false,
            shadow: None,
            scale: 1,
        }
    }

    /// The BGRA pixel at sheet coordinates (`x`, `y`), logical pixels.
    fn at(r: &Rendered, scale: i32, x: i32, y: i32) -> [u8; 4] {
        let (i, j) = ((x - r.rect.x) * scale, (y - r.rect.y) * scale);
        let k = ((j as u32 * r.image.width + i as u32) * 4) as usize;
        r.image.data[k..k + 4].try_into().unwrap()
    }

    #[test]
    fn sizes_match_the_rect() {
        for (tilt, shadow, scale) in [
            (0.0, None, 1),
            (1.5, None, 2),
            (-1.5, Some((8, (3, 4))), 2),
            (0.0, Some((0, (0, 0))), 3),
        ] {
            let p = Paper {
                tilt,
                shadow: shadow.map(|(softness, offset)| IconShadow {
                    color: 0x00000060,
                    softness,
                    offset,
                }),
                scale,
                ..paper()
            };
            let r = render(&p);
            assert_eq!(r.image.width, (r.rect.w * scale) as u32);
            assert_eq!(r.image.height, (r.rect.h * scale) as u32);
            assert_eq!(
                r.image.data.len(),
                (r.image.width * r.image.height * 4) as usize
            );
            assert!(r.rect.x < 0 && r.rect.y < 0);
            assert_eq!(r.rect.w + 2 * r.rect.x, 120);
            assert_eq!(r.rect.h + 2 * r.rect.y, 60);
        }
        // Tilted sheets need more room.
        let flat = render(&paper()).rect;
        let tilted = render(&Paper {
            tilt: 3.0,
            ..paper()
        })
        .rect;
        assert!(tilted.y < flat.y, "{tilted:?} {flat:?}");
    }

    #[test]
    fn the_color_with_a_grain_and_light_from_above() {
        let r = render(&paper());
        let [b, g, red, a] = at(&r, 1, 60, 30);
        assert_eq!(a, 255);
        let near = |v: u8, want: i32| (v as i32 - want).abs() <= 16;
        assert!(
            near(red, 0xff) && near(g, 0xf3) && near(b, 0xa0),
            "{:?}",
            [red, g, b]
        );
        // Lighter at the top than at the bottom, on average over a row.
        let row = |y: i32| (10..110).map(|x| at(&r, 1, x, y)[0] as u32).sum::<u32>();
        assert!(row(2) > row(57));
        // Grain: not every pixel the same.
        let mid: Vec<[u8; 4]> = (10..110).map(|x| at(&r, 1, x, 30)).collect();
        assert!(mid.iter().any(|p| p != &mid[0]));
    }

    #[test]
    fn the_curl_cuts_the_corner_that_sticks_out() {
        let right = render(&paper());
        // Bottom left is cut, bottom right is paper.
        assert_eq!(at(&right, 1, 1, 59)[3], 0);
        assert_eq!(at(&right, 1, 118, 58)[3], 255);
        let left = render(&Paper {
            edge: Edge::Left,
            ..paper()
        });
        assert_eq!(at(&left, 1, 118, 59)[3], 0);
        assert_eq!(at(&left, 1, 1, 58)[3], 255);
        // The shapes mirror each other.
        for y in 0..60 {
            for x in 0..120 {
                let (a, b) = (at(&right, 1, x, y)[3], at(&left, 1, 119 - x, y)[3]);
                // Antialiasing may differ a little at the curves.
                assert!((a as i32 - b as i32).abs() <= 24, "({x}, {y}): {a} {b}");
            }
        }
    }

    #[test]
    fn slivers_are_darker_with_a_light_line() {
        let sheet = render(&paper());
        let sliver = render(&Paper {
            sliver: true,
            ..paper()
        });
        let sum = |r: &Rendered, x: i32| (20..40).map(|y| at(r, 1, x, y)[1] as u32).sum::<u32>();
        assert!(sum(&sliver, 60) < sum(&sheet, 60));
        // The line along the side that sticks out.
        assert!(sum(&sliver, 0) > sum(&sliver, 3));
    }

    #[test]
    fn the_shadow_falls_away_from_the_edge() {
        let s = IconShadow {
            color: 0x000000ff,
            softness: 6,
            offset: (4, 4),
        };
        let r = render(&Paper {
            shadow: Some(s),
            ..paper()
        });
        // Right edge: the shadow is on the left, toward the screen.
        let (left, right) = (at(&r, 1, -3, 30)[3], at(&r, 1, 122, 30)[3]);
        assert!(left > right, "{left} {right}");
        let r = render(&Paper {
            shadow: Some(s),
            edge: Edge::Left,
            ..paper()
        });
        let (left, right) = (at(&r, 1, -3, 30)[3], at(&r, 1, 122, 30)[3]);
        assert!(right > left, "{left} {right}");
    }

    #[test]
    fn grain_tiles() {
        let t = grain_tile();
        assert_eq!(t.len(), TILE * TILE);
        assert!(t.iter().all(|v| (-1.0..=1.0).contains(v)));
        let mean = t.iter().sum::<f32>() / t.len() as f32;
        assert!(mean.abs() < 0.1, "{mean}");
    }

    #[test]
    fn shades() {
        assert_eq!(shade(0x808080ff, 0.0), 0x808080ff);
        assert_eq!(shade(0x000000ff, 0.5), 0x808080ff);
        assert_eq!(shade(0xffffff40, -0.5), 0x80808040);
    }
}
