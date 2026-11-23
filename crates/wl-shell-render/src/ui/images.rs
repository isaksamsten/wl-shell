//! `ui.image` sources, decoded on the worker and kept for a while:
//! files (`~/` expands), `file://` URLs, and `http(s)://` URLs. PNG, JPEG
//! and WebP are decoded and scaled down to at most `MAX_SIDE`; SVGs are
//! rasterized.
//!
//! A remote image is fetched once, on a thread of its own, into the disk
//! cache (`$XDG_CACHE_HOME/wl-shell/images`, named by the URL's MD5);
//! until it is there the image is nothing, and when it arrives the
//! fetcher says so (`Fetched`, to whoever painted it, so it is painted
//! again; the next `get` finds the file). A failed fetch is tried again
//! after `RETRY`.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use crate::image::{Image, to_argb8888};

/// Images kept, most recently used last.
const KEEP: usize = 16;
/// The longest side an image is kept at.
const MAX_SIDE: u32 = 1024;
/// The largest remote image fetched.
const MAX_FETCH: u64 = 16 << 20;
/// How long a fetch may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// How long after a failed fetch the URL is tried again.
const RETRY: Duration = Duration::from_secs(300);

/// A remote image was fetched (`ok`), or couldn't be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub src: String,
    pub ok: bool,
}

/// Fetches that failed, and when.
type Failures = Arc<Mutex<HashMap<String, Instant>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Where fetches go, and who hears they are done.
struct Fetcher {
    dir: PathBuf,
    notify: Arc<dyn Fn(Fetched) + Send + Sync>,
    failures: Failures,
    queue: Option<mpsc::Sender<String>>,
}

impl Fetcher {
    /// Fetch `src` (starting the thread the first time).
    fn fetch(&mut self, src: &str) {
        if self.queue.is_none() {
            let (send, recv) = mpsc::channel::<String>();
            let (dir, notify) = (self.dir.clone(), self.notify.clone());
            let failures = self.failures.clone();
            let spawned = std::thread::Builder::new()
                .name("image-fetch".into())
                .spawn(move || {
                    let agent = ureq::AgentBuilder::new()
                        .timeout(FETCH_TIMEOUT)
                        .redirects(5)
                        .build();
                    for src in recv {
                        let ok = download(&agent, &src, &cached(&dir, &src))
                            .inspect_err(|e| wl_shell_log::warn!("image {src}: {e}"))
                            .is_ok();
                        if !ok {
                            lock(&failures).insert(src.clone(), Instant::now());
                        }
                        notify(Fetched { src, ok });
                    }
                });
            match spawned {
                Ok(_) => self.queue = Some(send),
                Err(e) => return wl_shell_log::warn!("image {src}: {e}"),
            }
        }
        if let Some(q) = &self.queue {
            let _ = q.send(src.to_string());
        }
    }
}

#[derive(Default)]
pub struct Images {
    /// None: it couldn't be read.
    kept: Vec<(String, Option<Arc<Image>>)>,
    /// None: remote images aren't fetched.
    fetcher: Option<Fetcher>,
    /// Remote images asked for, and not in the cache yet.
    asked: HashSet<String>,
}

impl Images {
    /// Images that fetch remote ones into `dir`, telling `notify` when
    /// each is done.
    pub fn fetching(dir: PathBuf, notify: impl Fn(Fetched) + Send + Sync + 'static) -> Self {
        Images {
            fetcher: Some(Fetcher {
                dir,
                notify: Arc::new(notify),
                failures: Failures::default(),
                queue: None,
            }),
            ..Default::default()
        }
    }

    /// The default folder remote images are cached in.
    pub fn cache_dir() -> Option<PathBuf> {
        Some(crate::files::cache_dir()?.join("wl-shell/images"))
    }

    pub fn get(&mut self, src: &str) -> Option<Arc<Image>> {
        if let Some(i) = self.kept.iter().position(|(s, _)| s == src) {
            let entry = self.kept.remove(i);
            let image = entry.1.clone();
            self.kept.push(entry);
            return image;
        }
        let image = match remote(src) {
            true => {
                let fetcher = self.fetcher.as_mut()?;
                let file = cached(&fetcher.dir, src);
                if !file.exists() {
                    let failed = lock(&fetcher.failures).get(src).copied();
                    match (self.asked.contains(src), failed) {
                        // On its way.
                        (true, None) => return None,
                        (true, Some(at)) if at.elapsed() < RETRY => return None,
                        _ => {}
                    }
                    lock(&fetcher.failures).remove(src);
                    self.asked.insert(src.to_string());
                    fetcher.fetch(src);
                    return None;
                }
                self.asked.remove(src);
                load_file(&file)
            }
            false => path(src).and_then(|p| load_file(&p)),
        };
        let image = image
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
        // Failures are tried again.
        if let Some(f) = &self.fetcher {
            lock(&f.failures).clear();
        }
        self.asked.clear();
    }
}

/// Whether `src` is an `http(s)://` URL.
pub fn remote(src: &str) -> bool {
    let src = src.trim();
    let lower = |p: &str| src.len() > p.len() && src[..p.len()].eq_ignore_ascii_case(p);
    lower("http://") || lower("https://")
}

/// The file remote image `src` is cached in, in `dir`.
fn cached(dir: &Path, src: &str) -> PathBuf {
    dir.join(crate::files::hex(&crate::files::md5(src.trim().as_bytes())))
}

/// Fetch `src` into `file`.
fn download(agent: &ureq::Agent, src: &str, file: &Path) -> Result<(), String> {
    let response = agent.get(src.trim()).call().map_err(|e| e.to_string())?;
    if response.status() != 200 {
        return Err(format!("HTTP {}", response.status()));
    }
    let mut body = Vec::new();
    (response.into_reader().take(MAX_FETCH + 1))
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > MAX_FETCH {
        return Err("too large".into());
    }
    let dir = file.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Whole or not at all.
    let part = file.with_extension(format!("part{}", std::process::id()));
    let written = std::fs::File::create(&part)
        .and_then(|mut f| f.write_all(&body))
        .and_then(|()| std::fs::rename(&part, file));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(format!("{}: {e}", file.display()));
    }
    Ok(())
}

/// The file `src` names: a path or a `file://` URL.
pub fn path(src: &str) -> Result<PathBuf, String> {
    let src = src.trim();
    if let Some(rest) = src.strip_prefix("file://") {
        return Ok(PathBuf::from(percent_decode(rest)));
    }
    if src.contains("://") {
        return Err("only files, file:// and http(s):// URLs are supported".into());
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

fn load_file(path: &Path) -> Result<Image, String> {
    let svg = (path.extension()).is_some_and(|e| e.eq_ignore_ascii_case("svg"));
    if svg {
        return crate::icons::decode(path, 512);
    }
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let img = image::ImageReader::open(path)
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
    use std::net::TcpListener;

    #[test]
    fn sources() {
        assert_eq!(
            path("file:///a%20b/c.png").unwrap(),
            PathBuf::from("/a b/c.png")
        );
        assert_eq!(path("/x/y.png").unwrap(), PathBuf::from("/x/y.png"));
        assert!(path("ftp://example.com/a.png").is_err());
        assert!(remote("https://example.com/a.png") && remote(" HTTP://x/y"));
        assert!(!remote("/x/https://") && !remote("https://"));
        assert_eq!(percent_decode("%zz%41"), "%zzA");
        let mut images = Images::default();
        assert!(images.get("/nonexistent.png").is_none());
        // Without a fetcher, remote images are nothing.
        assert!(images.get("https://example.com/a.png").is_none());
    }

    /// A PNG of `w` by `h`.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A server answering every request with `body` (status 200), or a
    /// 404 for paths with `missing`; its URL.
    fn serve(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let head = match request.contains("missing") {
                    true => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string(),
                    false => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                        body.len()
                    ),
                };
                let _ = s.write_all(head.as_bytes());
                if !request.contains("missing") {
                    let _ = s.write_all(&body);
                }
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    #[test]
    fn remote_images_are_fetched_once_and_cached() {
        let dir = std::env::temp_dir().join(format!("wl-shell-images-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let base = serve(png(3, 2));
        let (send, recv) = mpsc::channel();
        let send = std::sync::Mutex::new(send);
        let mut images = Images::fetching(dir.clone(), move |f| {
            let _ = send.lock().unwrap().send(f);
        });
        let src = format!("{base}/cover.png");
        // Nothing until it arrives; asking again doesn't fetch again.
        assert!(images.get(&src).is_none());
        assert!(images.get(&src).is_none());
        let done = recv.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            done,
            Fetched {
                src: src.clone(),
                ok: true
            }
        );
        let image = images.get(&src).unwrap();
        assert_eq!((image.width, image.height), (3, 2));
        assert!(recv.try_recv().is_err());
        // From the disk next time.
        let mut fresh = Images::fetching(dir.clone(), |_| panic!("fetched again"));
        assert!(fresh.get(&src).is_some());
        // A failure waits before it is tried again.
        let missing = format!("{base}/missing.png");
        assert!(images.get(&missing).is_none());
        let failed = recv.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!failed.ok);
        assert!(images.get(&missing).is_none());
        assert!(recv.recv_timeout(Duration::from_millis(200)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
