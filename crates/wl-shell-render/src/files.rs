//! Files for the dock's folders and drops: `file://` URIs and
//! `text/uri-list`, the freedesktop thumbnail cache, and listing a
//! folder's newest entries with an icon for each.
//!
//! - URIs are escaped the way GLib does (`g_filename_to_uri`): every byte
//!   but the unreserved ones, `/` and `!$&'()*+,;=:@` is percent-encoded.
//!   A uri-list is local only if every URI is a `file:` URI without a
//!   host (or with `localhost`).
//! - Thumbnails are only read, never made: the PNG named after the MD5 of
//!   the file's URI in `$XDG_CACHE_HOME/thumbnails/<size>/`, if it is no
//!   older than the file.
//! - A listing leaves out hidden entries and sorts by modification time,
//!   creation time (`added`) or name. Each entry's icon is its thumbnail,
//!   else theme icons for its type (mime.rs), else a folder's; `icons`
//!   gives the same for any one path.

use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
fn thumbnail_dir() -> Option<PathBuf> {
    Some(cache_dir()?.join("thumbnails"))
}

/// The thumbnail's file name for `path`.
pub fn thumbnail_name(path: &Path) -> String {
    format!("{}.png", hex(&md5(file_uri(path).as_bytes())))
}

/// A cached thumbnail of `path`, last modified at `mtime`, if there is
/// one no older than it.
pub fn thumbnail(path: &Path, mtime: Option<SystemTime>) -> Option<PathBuf> {
    let dir = thumbnail_dir()?;
    let name = thumbnail_name(path);
    ["normal", "large", "x-large", "xx-large"]
        .iter()
        .map(|size| dir.join(size).join(&name))
        .find(|t| {
            let made = std::fs::metadata(t).and_then(|m| m.modified()).ok();
            match (made, mtime) {
                (Some(made), Some(mtime)) => made >= mtime,
                (Some(_), None) => true,
                (None, _) => false,
            }
        })
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
}
