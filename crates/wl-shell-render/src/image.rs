//! Rendered and decoded images: premultiplied ARGB8888 pixels, ready for shm
//! buffers.

use std::path::Path;

/// A decoded or rendered image as premultiplied ARGB8888 (little endian: B,
/// G, R, A), packed rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Image {
    /// Decodes the image file at `path` (PNG, JPEG or WebP).
    pub fn load(path: &Path) -> Result<Self, String> {
        let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
        let rgba = image::ImageReader::open(path)
            .map_err(|e| err(&e))?
            .with_guessed_format()
            .map_err(|e| err(&e))?
            .decode()
            .map_err(|e| err(&e))?
            .into_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 || width > i32::MAX as u32 / 4 || height > i32::MAX as u32 / 4
        {
            return Err(err(&format!("unsupported size {width}x{height}")));
        }
        Ok(Self {
            width,
            height,
            data: to_argb8888(rgba.as_raw()),
        })
    }

    pub fn stride(&self) -> i32 {
        self.width as i32 * 4
    }
}

/// Straight RGBA8 to premultiplied ARGB8888 in memory order.
pub fn to_argb8888(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    for &[r, g, b, a] in rgba.as_chunks::<4>().0 {
        let pre = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out.extend_from_slice(&[pre(b), pre(g), pre(r), a]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply() {
        assert_eq!(to_argb8888(&[255, 128, 0, 255]), [0, 128, 255, 255]);
        assert_eq!(to_argb8888(&[255, 255, 255, 0]), [0, 0, 0, 0]);
        assert_eq!(to_argb8888(&[255, 0, 0, 128]), [0, 0, 128, 128]);
    }
}
