//! Files for the dock's folders and drops: `file://` URIs and
//! `text/uri-list`, the freedesktop thumbnail cache, and listing a
//! folder's newest entries with an icon for each.
//!
//! - URIs are escaped the way GLib does (`g_filename_to_uri`): every byte
//!   but the unreserved ones, `/` and `!$&'()*+,;=:@` is percent-encoded.
//!   A uri-list is local only if every URI is a `file:` URI without a
//!   host (or with `localhost`).
//! - Thumbnails are read from the PNG named after the MD5 of the file's
//!   URI in `$XDG_CACHE_HOME/thumbnails/<size>/`, if it is still the
//!   file's: its `Thumb::MTime` is the file's modification time (without
//!   one, the thumbnail is no older than the file).
//! - Previews (`preview`, wl-note's shelf) are a cached thumbnail, else,
//!   for an image this crate can decode, the image itself, of which a
//!   `large` thumbnail is written to the cache as the spec has it (with
//!   `Thumb::URI` and `Thumb::MTime`), so it is made once and file
//!   managers get it too.
//! - A listing leaves out hidden entries and sorts by modification time,
//!   creation time (`added`) or name. Each entry's icon is its thumbnail,
//!   else theme icons for its type (mime.rs), else a folder's; `icons`
//!   gives the same for any one path.

use std::io::{self, BufReader, BufWriter, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::icons::{Premultiplied, premultiply_rgba, render_svg};
use crate::image::Image;
use crate::mime::MimeDb;

/// Bytes kept as they are in a URI path.
fn plain(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~/!$&'()*+,;=:@".contains(&b)
}

/// The `file://` URI of the absolute `path`.
pub fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        match plain(b) {
            true => out.push(b as char),
            false => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `text/uri-list` for `paths`.
pub fn uri_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| file_uri(p) + "\r\n").collect()
}

/// `s` with its `%XX` escapes decoded; None if one is broken.
fn percent_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The local path of a `file:` URI; None for anything else.
pub fn uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file:")?;
    let path = match rest.strip_prefix("//") {
        Some(authority) => {
            let slash = authority.find('/')?;
            let host = &authority[..slash];
            if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            &authority[slash..]
        }
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    // A query or fragment has no place in a file URI.
    let path = path.split(['?', '#']).next()?;
    let bytes = percent_decode(path)?;
    if bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// The local paths of a `text/uri-list`; None if it holds anything else
/// (or nothing).
pub fn parse_uri_list(text: &str) -> Option<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = uri_path(line)?;
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// The MD5 digest of `data` (RFC 1321), for thumbnail names.
pub fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: [u32; 64] =
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    let mut h: [u32; 4] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476];
    for chunk in msg.chunks(64) {
        let m: [u32; 16] = std::array::from_fn(|i| {
            u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap())
        });
        let [mut a, mut b, mut c, mut d] = h;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0; 16];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `$XDG_CACHE_HOME` (else `~/.cache`).
pub(crate) fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".cache")))
}

/// `$XDG_CACHE_HOME/thumbnails`.
pub fn thumbnail_dir() -> Option<PathBuf> {
    Some(cache_dir()?.join("thumbnails"))
}

/// The thumbnail's file name for `path`.
pub fn thumbnail_name(path: &Path) -> String {
    format!("{}.png", hex(&md5(file_uri(path).as_bytes())))
}

/// Thumbnail sizes, smallest first (icons), and best for a preview first.
const ICON_SIZES: [&str; 4] = ["normal", "large", "x-large", "xx-large"];
const PREVIEW_SIZES: [&str; 4] = ["large", "x-large", "xx-large", "normal"];

/// A cached thumbnail of `path`, last modified at `mtime`, if there is
/// one that is still the file's.
pub fn thumbnail(path: &Path, mtime: Option<SystemTime>) -> Option<PathBuf> {
    thumbnail_in(&thumbnail_dir()?, path, mtime, &ICON_SIZES)
}

/// The same in the thumbnail directory `dir`, of the first of `sizes`
/// that has one.
fn thumbnail_in(
    dir: &Path,
    path: &Path,
    mtime: Option<SystemTime>,
    sizes: &[&str],
) -> Option<PathBuf> {
    let name = thumbnail_name(path);
    (sizes.iter())
        .map(|size| dir.join(size).join(&name))
        .find(|t| fresh(t, mtime))
}

/// Whether the thumbnail `thumb` is of the file as it was last modified
/// at `mtime`: its `Thumb::MTime` says so, or without one, it is no
/// older than the file.
fn fresh(thumb: &Path, mtime: Option<SystemTime>) -> bool {
    let Ok(made) = std::fs::metadata(thumb).and_then(|m| m.modified()) else {
        return false;
    };
    let Some(mtime) = mtime else {
        return true;
    };
    match thumb_mtime(thumb) {
        Some(secs) => unix_secs(mtime) == Some(secs),
        None => made >= mtime,
    }
}

fn unix_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// A thumbnail's text chunks (those before its pixels).
fn thumb_text(thumb: &Path) -> Option<Vec<(String, String)>> {
    let file = std::fs::File::open(thumb).ok()?;
    let reader = png::Decoder::new(BufReader::new(file)).read_info().ok()?;
    let text = &reader.info().uncompressed_latin1_text;
    Some(text.iter().map(|t| (t.keyword.clone(), t.text.clone())).collect())
}

/// A thumbnail's `Thumb::MTime`, if it has one.
pub fn thumb_mtime(thumb: &Path) -> Option<u64> {
    let text = thumb_text(thumb)?;
    let (_, secs) = text.iter().find(|(k, _)| k == "Thumb::MTime")?;
    secs.trim().parse().ok()
}

// -- previews ------------------------------------------------------------

/// Files larger than this aren't read for a preview.
pub const PREVIEW_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// The `large` thumbnails' size.
const LARGE: u32 = 256;

/// Whether a preview can be decoded from `path` itself, by its name:
/// Some(true) for an SVG.
fn decodable(path: &Path) -> Option<bool> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "jpe" | "webp" => Some(false),
        "svg" => Some(true),
        _ => None,
    }
}

/// A preview of `path` fitted in `w`x`h` buffer pixels: a cached
/// thumbnail (`large` first), else the image itself if it is one, which
/// leaves a `large` thumbnail in the cache. None for anything else.
pub fn preview(path: &Path, w: u32, h: u32) -> Option<Image> {
    preview_in(thumbnail_dir().as_deref(), path, w, h, PREVIEW_MAX_BYTES)
}

/// The same with the thumbnail directory `cache` (None: none), reading
/// no image larger than `max_bytes`.
pub fn preview_in(
    cache: Option<&Path>,
    path: &Path,
    w: u32,
    h: u32,
    max_bytes: u64,
) -> Option<Image> {
    let meta = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let mtime = meta.modified().ok();
    let cached = cache.and_then(|dir| thumbnail_in(dir, path, mtime, &PREVIEW_SIZES));
    if let Some(thumb) = cached
        && let Ok(img) = decode(&std::fs::read(&thumb).unwrap_or_default())
    {
        // A thumbnail stands in for a larger image: it may grow.
        return Some(fitted(img, w, h, true));
    }
    let svg = decodable(path)?;
    if meta.len() > max_bytes {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let img = match svg {
        true => unpremultiply(render_svg(&data, LARGE).ok()?),
        false => decode(&data).ok()?,
    };
    // Not a thumbnail of a thumbnail.
    if let Some(dir) = cache.filter(|dir| !path.starts_with(dir)) {
        let thumb = shrunk(&img, LARGE);
        if let Err(e) = write_thumbnail(dir, path, mtime, meta.len(), &thumb) {
            wl_shell_log::warn!("thumbnail of {}: {e}", path.display());
        }
    }
    Some(fitted(img, w, h, false))
}

/// An image file's pixels, straight RGBA, turned as its EXIF says.
fn decode(data: &[u8]) -> Result<image::RgbaImage, String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(io::Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().ok();
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img.into_rgba8())
}

fn unpremultiply(Premultiplied(mut img): Premultiplied) -> image::RgbaImage {
    for p in img.pixels_mut() {
        let a = p.0[3] as u32;
        for c in &mut p.0[..3] {
            if let Some(v) = (*c as u32 * 255 + a / 2).checked_div(a) {
                *c = v.min(255) as u8;
            }
        }
    }
    img
}

/// `w`x`h` scaled to fit in `bw`x`bh`, keeping its aspect; only down
/// unless `grow`.
pub fn fit_box(w: u32, h: u32, bw: u32, bh: u32, grow: bool) -> (u32, u32) {
    let s = (bw as f64 / w.max(1) as f64).min(bh as f64 / h.max(1) as f64);
    let s = if grow { s } else { s.min(1.0) };
    (
        ((w as f64 * s).round() as u32).clamp(1, bw.max(1)),
        ((h as f64 * s).round() as u32).clamp(1, bh.max(1)),
    )
}

/// `img` no larger than `size` either way.
fn shrunk(img: &image::RgbaImage, size: u32) -> image::RgbaImage {
    let (w, h) = img.dimensions();
    match fit_box(w, h, size, size, false) {
        (fw, fh) if (fw, fh) == (w, h) => img.clone(),
        (fw, fh) => image::imageops::thumbnail(img, fw, fh),
    }
}

/// Straight RGBA `img` fitted in `w`x`h` (see `fit_box`), as an image to
/// show.
fn fitted(mut img: image::RgbaImage, w: u32, h: u32, grow: bool) -> Image {
    // Scaled premultiplied, so transparent pixels don't bleed.
    premultiply_rgba(&mut img);
    let (iw, ih) = img.dimensions();
    let (fw, fh) = fit_box(iw, ih, w, h, grow);
    let img = match (fw, fh) {
        _ if (fw, fh) == (iw, ih) => img,
        _ if fw < iw => image::imageops::thumbnail(&img, fw, fh),
        _ => image::imageops::resize(&img, fw, fh, image::imageops::FilterType::CatmullRom),
    };
    let mut data = img.into_raw();
    for px in data.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    Image {
        width: fw,
        height: fh,
        data,
    }
}

/// `img` (premultiplied) as a print: over white, in a white border
/// `border` pixels wide.
pub fn print(img: &Image, border: u32) -> Image {
    let (w, h) = (img.width + 2 * border, img.height + 2 * border);
    let mut data = vec![255; (w * h * 4) as usize];
    for y in 0..img.height {
        for x in 0..img.width {
            let i = ((y * img.width + x) * 4) as usize;
            let o = (((y + border) * w + x + border) * 4) as usize;
            let a = img.data[i + 3] as u32;
            for c in 0..3 {
                data[o + c] = (img.data[i + c] as u32 + (255 - a)).min(255) as u8;
            }
        }
    }
    Image {
        width: w,
        height: h,
        data,
    }
}

/// Write `thumb` as the `large` thumbnail of `path` (last modified at
/// `mtime`, `size` bytes) in the thumbnail directory `dir`, as the
/// thumbnail spec has it: by way of a temporary file, readable only by
/// the user, with `Thumb::URI` and `Thumb::MTime`.
fn write_thumbnail(
    dir: &Path,
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    thumb: &image::RgbaImage,
) -> io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let large = dir.join("large");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&large)?;
    let name = thumbnail_name(path);
    let target = large.join(&name);
    let tmp = large.join(format!(".{name}.{}.tmp", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    let written = encode_thumbnail(BufWriter::new(file), path, mtime, size, thumb)
        .and_then(|()| std::fs::rename(&tmp, &target));
    match written {
        Ok(()) => Ok(target),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn encode_thumbnail(
    out: impl Write,
    path: &Path,
    mtime: Option<SystemTime>,
    size: u64,
    thumb: &image::RgbaImage,
) -> io::Result<()> {
    let mut enc = png::Encoder::new(out, thumb.width(), thumb.height());
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut text = vec![
        ("Thumb::URI", file_uri(path)),
        ("Thumb::Size", size.to_string()),
        ("Software", "wl-shell".to_string()),
    ];
    if let Some(secs) = mtime.and_then(unix_secs) {
        text.push(("Thumb::MTime", secs.to_string()));
    }
    for (key, value) in text {
        enc.add_text_chunk(key.into(), value)
            .map_err(io::Error::other)?;
    }
    let mut writer = enc.write_header().map_err(io::Error::other)?;
    writer
        .write_image_data(thumb.as_raw())
        .map_err(io::Error::other)?;
    writer.finish().map_err(io::Error::other)
}

/// How a folder's entries are sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Sort {
    /// Most recently modified first.
    #[default]
    Modified,
    /// Most recently created first.
    Added,
    /// By name, ignoring case.
    Name,
}

/// What to list.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FolderRequest {
    pub dir: PathBuf,
    pub sort: Sort,
    /// Entries at most.
    pub count: usize,
}

/// One entry of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    /// The file name, as shown.
    pub name: String,
    pub dir: bool,
    /// Its thumbnail's path, else theme icon names; best first.
    pub icons: Vec<String>,
    modified: Option<SystemTime>,
    added: Option<SystemTime>,
}

/// The icon names of a folder.
pub const FOLDER_ICONS: [&str; 2] = ["folder", "inode-directory"];

/// Sort `entries` by `sort` and keep `count`. Folders and files mix;
/// ties go by name.
fn sort_entries(entries: &mut Vec<FileEntry>, sort: Sort, count: usize) {
    let name = |e: &FileEntry| e.name.to_lowercase();
    entries.sort_by(|a, b| {
        let by_time = |ta: Option<SystemTime>, tb: Option<SystemTime>| tb.cmp(&ta);
        match sort {
            Sort::Modified => by_time(a.modified, b.modified),
            Sort::Added => by_time(a.added.or(a.modified), b.added.or(b.modified)),
            Sort::Name => std::cmp::Ordering::Equal,
        }
        .then_with(|| name(a).cmp(&name(b)))
        .then_with(|| a.name.cmp(&b.name))
    });
    entries.truncate(count);
}

/// The `request.count` first entries of `request.dir`, hidden ones left
/// out.
pub fn list(request: &FolderRequest, mime: &MimeDb) -> Result<Vec<FileEntry>, String> {
    let read = std::fs::read_dir(&request.dir).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        if name.as_bytes().starts_with(b".") {
            continue;
        }
        let path = entry.path();
        // Through symlinks; a broken one is a file.
        let meta = std::fs::metadata(&path).or_else(|_| entry.metadata());
        let Ok(meta) = meta else { continue };
        entries.push(FileEntry {
            name: name.to_string_lossy().into_owned(),
            dir: meta.is_dir(),
            icons: Vec::new(),
            modified: meta.modified().ok(),
            added: meta.created().ok(),
            path,
        });
    }
    sort_entries(&mut entries, request.sort, request.count);
    for e in &mut entries {
        e.icons = entry_icons(&e.path, &e.name, e.dir, e.modified, mime);
    }
    Ok(entries)
}

/// The icons of a file named `name` at `path` (a folder if `dir`), last
/// modified at `mtime`: its thumbnail's path, else theme icon names for
/// its type; best first.
fn entry_icons(
    path: &Path,
    name: &str,
    dir: bool,
    mtime: Option<SystemTime>,
    mime: &MimeDb,
) -> Vec<String> {
    if dir {
        return FOLDER_ICONS.map(String::from).to_vec();
    }
    let thumb = thumbnail(path, mtime).map(|t| t.to_string_lossy().into_owned());
    thumb
        .into_iter()
        .chain(mime.icons(mime.type_of(name)))
        .collect()
}

/// The icons of `path` (a folder if `dir`), as a listing gives them.
pub fn icons(path: &Path, dir: bool, mime: &MimeDb) -> Vec<String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mtime = match dir {
        true => None,
        false => std::fs::metadata(path).and_then(|m| m.modified()).ok(),
    };
    entry_icons(path, &name, dir, mtime, mime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn uris() {
        assert_eq!(
            file_uri(Path::new("/a/b c/é%.txt")),
            "file:///a/b%20c/%C3%A9%25.txt"
        );
        assert_eq!(
            file_uri(Path::new("/x/(1)+a=b@c:d")),
            "file:///x/(1)+a=b@c:d"
        );
        for p in ["/a/b c/é%.txt", "/x/#?", "/"] {
            assert_eq!(uri_path(&file_uri(Path::new(p))).unwrap(), Path::new(p));
        }
        assert_eq!(
            uri_path("file://localhost/a%20b").unwrap(),
            Path::new("/a b")
        );
        assert_eq!(uri_path("file:/a").unwrap(), Path::new("/a"));
        assert_eq!(uri_path("file://host/a"), None);
        assert_eq!(uri_path("trash:///a"), None);
        assert_eq!(uri_path("file:///a%2"), None);
        assert_eq!(uri_path("file:///a%00"), None);
    }

    #[test]
    fn uri_lists() {
        let list = "# from somewhere\r\nfile:///home/u/a.txt\r\nfile:///home/u/My%20Dir\r\n\r\n";
        assert_eq!(
            parse_uri_list(list).unwrap(),
            [
                PathBuf::from("/home/u/a.txt"),
                PathBuf::from("/home/u/My Dir")
            ]
        );
        // Anything but local files refuses the whole drag.
        assert_eq!(parse_uri_list("file:///a\nhttps://x.org/b\n"), None);
        assert_eq!(parse_uri_list("\n# nothing\n"), None);
        let paths = [PathBuf::from("/a b"), PathBuf::from("/c")];
        assert_eq!(parse_uri_list(&uri_list(&paths)).unwrap(), paths);
    }

    #[test]
    fn md5_vectors() {
        let h = |s: &str| hex(&md5(s.as_bytes()));
        assert_eq!(h(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(h("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            h("The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
        let long = "1234567890".repeat(8);
        assert_eq!(h(&long), "57edf4a22be3c955ac49da2e2107b67a");
        // The spec's example.
        assert_eq!(
            thumbnail_name(Path::new("/home/jens/photos/me.png")),
            "c6ee772d9e49320e97ec29a7eb5b1697.png"
        );
    }

    fn entry(name: &str, modified: u64, added: Option<u64>) -> FileEntry {
        let t = |s| Some(SystemTime::UNIX_EPOCH + Duration::from_secs(s));
        FileEntry {
            path: PathBuf::from(name),
            name: name.into(),
            dir: false,
            icons: Vec::new(),
            modified: t(modified),
            added: added.and_then(t),
        }
    }

    #[test]
    fn sorting() {
        let all = vec![
            entry("b", 30, Some(5)),
            entry("A", 10, Some(20)),
            entry("c", 20, None),
            entry("d", 30, Some(1)),
        ];
        let names = |sort, count| {
            let mut e = all.clone();
            sort_entries(&mut e, sort, count);
            e.iter().map(|e| e.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(names(Sort::Modified, 9), ["b", "d", "c", "A"]);
        // Without a creation time, the modification time stands in.
        assert_eq!(names(Sort::Added, 9), ["A", "c", "b", "d"]);
        assert_eq!(names(Sort::Name, 9), ["A", "b", "c", "d"]);
        assert_eq!(names(Sort::Modified, 2), ["b", "d"]);
    }

    #[test]
    fn listing() {
        let dir = std::env::temp_dir().join(format!("wl-shell-files-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.png"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        let mut db = MimeDb::default();
        db.add_globs2("50:image/png:*.png\n");
        let request = FolderRequest {
            dir: dir.clone(),
            sort: Sort::Name,
            count: 10,
        };
        let got = list(&request, &db).unwrap();
        let names: Vec<_> = got.iter().map(|e| (e.name.as_str(), e.dir)).collect();
        assert_eq!(names, [("a.png", false), ("sub", true)]);
        assert!(got[0].icons.contains(&"image-png".to_string()));
        assert_eq!(got[1].icons, FOLDER_ICONS);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(list(&request, &db).is_err());
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wl-shell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> image::RgbaImage {
        image::RgbaImage::from_pixel(w, h, image::Rgba(rgba))
    }

    #[test]
    fn previews_of_images_go_into_the_cache() {
        let dir = temp("preview");
        let cache = dir.join("thumbnails");
        let file = dir.join("shot 1.png");
        solid(400, 200, [0, 0, 255, 255]).save(&file).unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        // Fitted in the box, keeping its aspect; blue is B first.
        let img = preview_in(Some(&cache), &file, 100, 100, PREVIEW_MAX_BYTES).unwrap();
        assert_eq!((img.width, img.height), (100, 50));
        assert_eq!(&img.data[..4], [255, 0, 0, 255]);
        // The cache file: the URI's MD5, large, with the spec's keys.
        let thumb = cache.join("large").join(thumbnail_name(&file));
        assert_eq!(
            thumb.file_name().unwrap().to_str().unwrap(),
            format!("{}.png", hex(&md5(file_uri(&file).as_bytes())))
        );
        let text = thumb_text(&thumb).unwrap();
        let get = |k: &str| text.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        assert_eq!(get("Thumb::URI"), Some(file_uri(&file)));
        assert_eq!(thumb_mtime(&thumb), unix_secs(mtime));
        let made = image::open(&thumb).unwrap();
        assert_eq!((made.width(), made.height()), (256, 128));
        // A small image isn't grown, and its thumbnail is its size.
        let small = dir.join("small.png");
        solid(20, 10, [0, 255, 0, 128]).save(&small).unwrap();
        let img = preview_in(Some(&cache), &small, 100, 100, PREVIEW_MAX_BYTES).unwrap();
        assert_eq!((img.width, img.height), (20, 10));
        let made = image::open(cache.join("large").join(thumbnail_name(&small))).unwrap();
        assert_eq!((made.width(), made.height()), (20, 10));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn previews_leave_large_files_and_other_types() {
        let dir = temp("preview-limit");
        let cache = dir.join("thumbnails");
        let file = dir.join("big.png");
        solid(40, 40, [255, 0, 0, 255]).save(&file).unwrap();
        let size = std::fs::metadata(&file).unwrap().len();
        assert!(preview_in(Some(&cache), &file, 50, 50, size - 1).is_none());
        assert!(!cache.join("large").join(thumbnail_name(&file)).exists());
        assert!(preview_in(Some(&cache), &file, 50, 50, size).is_some());
        // Not an image: nothing without a thumbnail.
        let text = dir.join("notes.txt");
        std::fs::write(&text, "hello").unwrap();
        assert!(preview_in(Some(&cache), &text, 50, 50, PREVIEW_MAX_BYTES).is_none());
        assert!(preview_in(Some(&cache), &dir.join("gone.png"), 50, 50, size).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stale_thumbnails_are_not_used() {
        let dir = temp("preview-stale");
        let cache = dir.join("thumbnails");
        let file = dir.join("report.pdf");
        std::fs::write(&file, "%PDF-1.4").unwrap();
        let mtime = std::fs::metadata(&file).unwrap().modified().unwrap();
        let secs = unix_secs(mtime).unwrap();
        let large = cache.join("large");
        std::fs::create_dir_all(&large).unwrap();
        let red = solid(64, 32, [255, 0, 0, 255]);
        let write = |mtime: Option<SystemTime>| {
            let out = std::fs::File::create(large.join(thumbnail_name(&file))).unwrap();
            encode_thumbnail(out, &file, mtime, 8, &red).unwrap();
        };
        // A thumbnail made of another version of the file.
        write(Some(mtime - std::time::Duration::from_secs(100)));
        assert!(preview_in(Some(&cache), &file, 64, 64, PREVIEW_MAX_BYTES).is_none());
        assert_eq!(thumbnail_in(&cache, &file, Some(mtime), &PREVIEW_SIZES), None);
        // Of this one: used, and it may grow to the box.
        write(Some(mtime));
        assert_eq!(thumb_mtime(&large.join(thumbnail_name(&file))), Some(secs));
        let img = preview_in(Some(&cache), &file, 128, 128, PREVIEW_MAX_BYTES).unwrap();
        assert_eq!((img.width, img.height), (128, 64));
        assert_eq!(&img.data[..4], [0, 0, 255, 255]);
        // Without `Thumb::MTime`, one older than the file is stale.
        write(None);
        let thumb = large.join(thumbnail_name(&file));
        assert!(thumbnail_in(&cache, &file, Some(mtime), &PREVIEW_SIZES).is_some());
        std::fs::File::options()
            .write(true)
            .open(&thumb)
            .unwrap()
            .set_modified(mtime - std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(thumbnail_in(&cache, &file, Some(mtime), &PREVIEW_SIZES), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn prints_have_a_white_border() {
        let img = Image {
            width: 1,
            height: 1,
            data: vec![0, 0, 128, 128],
        };
        let p = print(&img, 2);
        assert_eq!((p.width, p.height), (5, 5));
        assert_eq!(&p.data[..4], [255, 255, 255, 255]);
        // Half transparent red over white.
        let mid = ((2 * 5 + 2) * 4) as usize;
        assert_eq!(&p.data[mid..mid + 4], [127, 127, 255, 255]);
        assert_eq!(fit_box(400, 200, 100, 100, false), (100, 50));
        assert_eq!(fit_box(20, 10, 100, 100, false), (20, 10));
        assert_eq!(fit_box(20, 10, 100, 100, true), (100, 50));
    }
}
