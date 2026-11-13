//! Background image: the viewport crop that scales it to cover the
//! overlay, and the blurred, darkened copy shown outside the card.

use wl_shell_render::Image;

/// The source rect (x, y, w, h in buffer pixels) of an `iw`x`ih` image that,
/// scaled to `w`x`h`, covers it without distortion: centered, cropping the
/// overflowing axis.
pub fn cover_source(iw: u32, ih: u32, w: u32, h: u32) -> (f64, f64, f64, f64) {
    let (iw, ih, w, h) = (iw as f64, ih as f64, w as f64, h as f64);
    let scale = (w / iw).max(h / ih);
    let (sw, sh) = ((w / scale).min(iw), (h / scale).min(ih));
    // wl_fixed has 1/256 precision; keep the rect inside the buffer after
    // rounding, or the compositor raises a protocol error.
    let floor = |v: f64| (v * 256.0).floor() / 256.0;
    let min = 1.0 / 256.0;
    let (sw, sh) = (floor(sw).max(min), floor(sh).max(min));
    (floor((iw - sw) / 2.0), floor((ih - sh) / 2.0), sw, sh)
}

/// Most the blurred background is scaled down by.
const MAX_DOWNSCALE: u32 = 8;

/// How much the blurred background is scaled down for a `blur` radius: by
/// an integer factor that keeps a few pixels per blur sigma, at most
/// `MAX_DOWNSCALE`.
fn blur_factor(blur: u32) -> u32 {
    (blur / 3).clamp(1, MAX_DOWNSCALE)
}

/// Borrowed premultiplied ARGB8888 pixels with a stride of `width * 4`.
#[derive(Clone, Copy)]
pub struct Pixels<'a> {
    pub width: u32,
    pub height: u32,
    pub data: &'a [u8],
}

/// The part of `img` that covers a `w`x`h` output, scaled down, blurred by
/// `blur` logical pixels (sigma = blur / 2), with the 0xRRGGBBAA `tint`
/// drawn over it, and opaque. Shown scaled up to the output by a viewport.
pub fn blurred(img: Pixels, w: u32, h: u32, blur: u32, tint: u32) -> Image {
    let f = blur_factor(blur);
    let (sw, sh) = (w.div_ceil(f).max(1), h.div_ceil(f).max(1));
    let crop = cover_source(img.width, img.height, w, h);
    let mut px = downsample(img, crop, sw, sh);
    let sigma = blur as f64 / 2.0 / f as f64;
    for radius in box_radii(sigma) {
        box_blur(&mut px, sw as usize, sh as usize, radius, true);
        box_blur(&mut px, sw as usize, sh as usize, radius, false);
    }
    // Premultiplied tint over the image, made opaque: an image with alpha
    // is shown over black.
    let [tr, tg, tb, ta] = tint.to_be_bytes().map(|c| c as f32 / 255.0);
    let tint = [tb * ta, tg * ta, tr * ta];
    let mut data = Vec::with_capacity(px.len());
    for p in px.as_chunks::<4>().0 {
        for c in 0..3 {
            let v = tint[c] * 255.0 + (1.0 - ta) * p[c];
            data.push(v.round().clamp(0.0, 255.0) as u8);
        }
        data.push(255);
    }
    Image {
        width: sw,
        height: sh,
        data,
    }
}

/// Average the source rect `(x, y, w, h)` of `img` into `dw`x`dh` pixels
/// (B, G, R, A as floats, premultiplied).
fn downsample(img: Pixels, (x, y, w, h): (f64, f64, f64, f64), dw: u32, dh: u32) -> Vec<f32> {
    let edges = |origin: f64, size: f64, n: u32, max: u32| -> Vec<usize> {
        (0..=n)
            .map(|i| (origin + size * i as f64 / n as f64).round() as usize)
            .map(|v| v.min(max as usize))
            .collect()
    };
    let xs = edges(x, w, dw, img.width);
    let ys = edges(y, h, dh, img.height);
    let stride = img.width as usize * 4;
    let mut out = Vec::with_capacity(dw as usize * dh as usize * 4);
    for j in 0..dh as usize {
        // At least one source row and column, even when scaling up.
        let (y0, y1) = (ys[j].min(img.height as usize - 1), ys[j + 1]);
        let y1 = y1.max(y0 + 1);
        for i in 0..dw as usize {
            let (x0, x1) = (xs[i].min(img.width as usize - 1), xs[i + 1]);
            let x1 = x1.max(x0 + 1);
            let mut sum = [0u64; 4];
            for row in y0..y1 {
                let line = &img.data[row * stride + x0 * 4..row * stride + x1 * 4];
                for p in line.as_chunks::<4>().0 {
                    for c in 0..4 {
                        sum[c] += p[c] as u64;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as f32;
            out.extend(sum.map(|s| s as f32 / n));
        }
    }
    out
}

/// Radii of three box blurs that approximate a Gaussian blur of `sigma`.
fn box_radii(sigma: f64) -> Vec<usize> {
    if sigma < 0.5 {
        return Vec::new();
    }
    let n = 3.0;
    let ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut lower = ideal.floor() as i64;
    if lower % 2 == 0 {
        lower -= 1;
    }
    let lower = lower.max(1) as f64;
    let upper = lower + 2.0;
    let m = ((12.0 * sigma * sigma - n * lower * lower - 4.0 * n * lower - 3.0 * n)
        / (-4.0 * lower - 4.0))
        .round();
    (0..3)
        .map(|i| if (i as f64) < m { lower } else { upper })
        .map(|size| ((size - 1.0) / 2.0) as usize)
        .filter(|&r| r > 0)
        .collect()
}

/// One box blur pass over rows (`horizontal`) or columns, clamping at the
/// edges.
fn box_blur(px: &mut [f32], w: usize, h: usize, radius: usize, horizontal: bool) {
    let (lines, len, step, line_step) = if horizontal {
        (h, w, 4, w * 4)
    } else {
        (w, h, w * 4, 4)
    };
    let r = radius as isize;
    let norm = 1.0 / (2 * radius + 1) as f32;
    let mut line = vec![0f32; len * 4];
    for l in 0..lines {
        let base = l * line_step;
        for k in 0..len {
            line[k * 4..k * 4 + 4].copy_from_slice(&px[base + k * step..base + k * step + 4]);
        }
        let at = |k: isize| (k.clamp(0, len as isize - 1) as usize) * 4;
        let mut sum = [0f32; 4];
        for k in -r..=r {
            for c in 0..4 {
                sum[c] += line[at(k) + c];
            }
        }
        for k in 0..len as isize {
            let out = base + k as usize * step;
            for c in 0..4 {
                px[out + c] = sum[c] * norm;
                sum[c] += line[at(k + r + 1) + c] - line[at(k - r) + c];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All of `img`'s pixels.
    fn pixels(img: &Image) -> Pixels<'_> {
        Pixels {
            width: img.width,
            height: img.height,
            data: &img.data,
        }
    }

    #[test]
    fn cover() {
        // Same aspect: everything.
        assert_eq!(
            cover_source(1920, 1080, 3840, 2160),
            (0.0, 0.0, 1920.0, 1080.0)
        );
        // Wider image: crop the sides.
        assert_eq!(
            cover_source(2000, 1000, 1000, 1000),
            (500.0, 0.0, 1000.0, 1000.0)
        );
        // Taller image: crop top and bottom.
        assert_eq!(
            cover_source(1000, 2000, 1000, 1000),
            (0.0, 500.0, 1000.0, 1000.0)
        );
        // Never outside the buffer.
        for (iw, ih, w, h) in [(1366, 768, 1920, 1200), (3, 7, 2560, 1440), (1, 1, 5, 3)] {
            let (x, y, sw, sh) = cover_source(iw, ih, w, h);
            assert!(x >= 0.0 && y >= 0.0 && sw > 0.0 && sh > 0.0);
            assert!(x + sw <= iw as f64 && y + sh <= ih as f64);
        }
    }

    fn image(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Image {
        let mut data = Vec::new();
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&f(x, y));
            }
        }
        Image {
            width,
            height,
            data,
        }
    }

    #[test]
    fn blurred_size_and_opaque() {
        let img = image(64, 32, |x, _| [x as u8 * 4, 0, 0, 255]);
        for (blur, w, h, sw, sh) in [(0, 64, 32, 64, 32), (24, 64, 32, 8, 4), (100, 70, 30, 9, 4)] {
            let out = blurred(pixels(&img), w, h, blur, 0x00000000);
            assert_eq!((out.width, out.height), (sw, sh), "blur {blur}");
            assert_eq!(out.data.len(), (sw * sh * 4) as usize);
            assert!(out.data.chunks(4).all(|p| p[3] == 255));
        }
    }

    #[test]
    fn uniform_stays_uniform() {
        let img = image(50, 40, |_, _| [10, 20, 30, 255]);
        let out = blurred(pixels(&img), 100, 80, 40, 0x00000000);
        for p in out.data.chunks(4) {
            assert_eq!(p, [10, 20, 30, 255]);
        }
    }

    #[test]
    fn tint_darkens() {
        let img = image(16, 16, |_, _| [200, 200, 200, 255]);
        let half = blurred(pixels(&img), 16, 16, 6, 0x00000080);
        assert!(half.data.chunks(4).all(|p| p[0].abs_diff(100) <= 1));
        let black = blurred(pixels(&img), 16, 16, 6, 0x000000ff);
        assert!(black.data.chunks(4).all(|p| p == [0, 0, 0, 255]));
    }

    #[test]
    fn blur_spreads_and_keeps_mass() {
        // A bright column in the middle of a dark image.
        let img = image(64, 8, |x, _| {
            if x == 32 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let out = blurred(pixels(&img), 64, 8, 3, 0x00000000);
        let row: Vec<u32> = (0..out.width)
            .map(|x| out.data[(x * 4) as usize] as u32)
            .collect();
        assert!(row[32] < 255 && row[31] > 0 && row[33] > 0, "{row:?}");
        let sum: u32 = row.iter().sum();
        assert!(sum.abs_diff(255) < 8, "{row:?}");
        assert_eq!(row[0], 0);
    }

    #[test]
    fn box_radii_match_sigma() {
        assert!(box_radii(0.0).is_empty());
        for sigma in [1.0, 2.5, 7.0, 20.0] {
            let radii = box_radii(sigma);
            // The variance of a box of width d is (d^2 - 1) / 12.
            let var: f64 = radii
                .iter()
                .map(|&r| ((2 * r + 1).pow(2) - 1) as f64 / 12.0)
                .sum();
            assert!((var.sqrt() - sigma).abs() < 0.6, "{sigma}: {radii:?}");
        }
    }
}
