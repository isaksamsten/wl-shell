//! `ui.image` sources, decoded on the worker and kept for a while:
//! files (`~/` expands) and `file://` URLs. PNG, JPEG and WebP are
//! decoded and scaled down to at most `MAX_SIDE`; SVGs are rasterized.

use std::path::PathBuf;
use std::sync::Arc;

use crate::image::{Image, to_argb8888};

/// Images kept, most recently used last.
const KEEP: usize = 16;
/// The longest side an image is kept at.
const MAX_SIDE: u32 = 1024;

#[derive(Default)]
pub struct Images {
    /// None: it couldn't be read.
    kept: Vec<(String, Option<Arc<Image>>)>,
}

impl Images {
    pub fn get(&mut self, src: &str) -> Option<Arc<Image>> {
        if let Some(i) = self.kept.iter().position(|(s, _)| s == src) {
            let entry = self.kept.remove(i);
            let image = entry.1.clone();
            self.kept.push(entry);
            return image;
        }
        let image = load(src)
            .inspect_err(|e| wl_shell_log::warn!("image {src}: {e}"))
            .ok()
            .map(Arc::new);
        if self.kept.len() >= KEEP {
            self.kept.remove(0);
        }
        self.kept.push((src.to_string(), image.clone()));
        image
    }

    pub fn clear(&mut self) {
        self.kept.clear();
    }
}

/// The file `src` names: a path or a `file://` URL.
pub fn path(src: &str) -> Result<PathBuf, String> {
    let src = src.trim();
    if let Some(rest) = src.strip_prefix("file://") {
        return Ok(PathBuf::from(percent_decode(rest)));
    }
    if src.contains("://") {
        return Err("only files and file:// URLs are supported".into());
    }
    Ok(PathBuf::from(crate::icons::expand_home(src)))
}

/// `%XX` escapes decoded (others left as they are).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn load(src: &str) -> Result<Image, String> {
    let path = path(src)?;
    let svg = (path.extension()).is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if svg {
        return crate::icons::decode(&path, 512);
    }
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let img = image::ImageReader::open(&path)
        .map_err(|e| err(&e))?
        .with_guessed_format()
        .map_err(|e| err(&e))?
        .decode()
        .map_err(|e| err(&e))?;
    let img = match img.width().max(img.height()) > MAX_SIDE {
        true => img.thumbnail(MAX_SIDE, MAX_SIDE),
        false => img,
    };
    let rgba = img.into_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return Err(err(&"empty"));
    }
    Ok(Image {
        width,
        height,
        data: to_argb8888(rgba.as_raw()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources() {
        assert_eq!(
            path("file:///a%20b/c.png").unwrap(),
            PathBuf::from("/a b/c.png")
        );
        assert_eq!(path("/x/y.png").unwrap(), PathBuf::from("/x/y.png"));
        assert!(path("https://example.com/a.png").is_err());
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        let mut images = Images::default();
        assert!(images.get("/nonexistent.png").is_none());
    }
}
