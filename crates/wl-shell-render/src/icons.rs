//! App icons and names: app_id -> desktop entry -> icon file ->
//! premultiplied pixels, and the entry's localized `Name=`.
//!
//! `freedesktop-desktop-entry` matches the app_id against the desktop
//! entries (StartupWMClass, desktop file ID, Name, Exec, Snap name;
//! case-insensitive). The entry's `Icon=` is an absolute path or a theme icon
//! name; without an entry the app_id itself is tried as an icon name. Names
//! are resolved by `freedesktop-icons` in the configured theme, else the
//! gsettings icon theme, else hicolor (and pixmaps). PNGs are decoded with
//! `image` and scaled down; SVGs are rasterized by `resvg` at the exact size.
//!
//! Runs on the worker thread. Decoded icons are cached; desktop entries are
//! reloaded when an applications directory changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use freedesktop_desktop_entry::{self as fde, DesktopEntry, unicase::Ascii};

use crate::image::Image;

/// What identifies a rendered icon.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub app_id: String,
    /// Buffer pixels: icon size times scale.
    pub size: u32,
    /// The configured theme, if any.
    pub theme: Option<String>,
    /// The buffer scale `size` includes.
    pub scale: i32,
    /// Drawn under the icon, which is then padded by its `pad`.
    pub shadow: Option<crate::shadow::IconShadow>,
}

impl Key {
    /// The key of `app_id`'s icon at `size` logical pixels in `theme` (None:
    /// the desktop's), with `shadow` under it, at `scale`.
    pub fn new(
        app_id: &str,
        size: u32,
        theme: Option<String>,
        shadow: Option<crate::shadow::IconShadow>,
        scale: i32,
    ) -> Self {
        let scale = scale.max(1);
        Self {
            app_id: app_id.to_string(),
            size: size * scale as u32,
            theme,
            scale,
            shadow,
        }
    }

    /// The same icon without its shadow.
    pub fn plain(&self) -> Self {
        Self {
            shadow: None,
            ..self.clone()
        }
    }

    /// Logical pixels the image reaches past the icon on every side.
    pub fn pad(&self) -> i32 {
        self.shadow.map_or(0, |s| s.pad())
    }

    /// The image's side in buffer pixels.
    pub fn image_size(&self) -> u32 {
        self.size + 2 * (self.pad() * self.scale) as u32
    }
}

pub struct Icons {
    entries: Vec<DesktopEntry>,
    /// The applications directories and their mtimes when `entries` loaded.
    dirs: Vec<(PathBuf, Option<SystemTime>)>,
    /// For localized names.
    locales: Vec<String>,
    /// The gsettings icon theme, looked up when the cache is cleared.
    default_theme: Option<Option<String>>,
    /// None: no icon was found (the caller draws a letter tile).
    cache: HashMap<Key, Option<Arc<Image>>>,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Default for Icons {
    fn default() -> Self {
        Self::new()
    }
}

impl Icons {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            dirs: Vec::new(),
            locales: Vec::new(),
            default_theme: None,
            cache: HashMap::new(),
        }
    }

    /// Forget decoded icons (and the default theme), e.g. after the config
    /// changed.
    pub fn clear(&mut self) {
        self.cache.clear();
        self.default_theme = None;
    }

    /// Load the desktop entries, unless already loaded.
    pub fn load_entries(&mut self) {
        if self.dirs.is_empty() {
            self.refresh();
        }
    }

    /// Reload the desktop entries if an applications directory changed (or
    /// they were never loaded). Cached icons of apps without an entry may
    /// now have one, so a reload also clears the cache. Returns whether
    /// previously loaded entries were reloaded.
    pub fn refresh(&mut self) -> bool {
        let dirs: Vec<_> = fde::default_paths()
            .map(|dir| {
                let m = mtime(&dir);
                (dir, m)
            })
            .collect();
        if dirs == self.dirs {
            return false;
        }
        let reloaded = !self.dirs.is_empty();
        self.locales = fde::get_languages_from_env();
        self.entries = fde::Iter::new(dirs.iter().map(|(d, _)| d.clone()))
            .filter_map(|p| DesktopEntry::from_path(p, Some(self.locales.as_slice())).ok())
            .collect();
        self.dirs = dirs;
        self.cache.clear();
        reloaded
    }

    /// The app's display name: its desktop entry's `Name=`, else the app_id.
    pub fn name(&mut self, app_id: &str) -> String {
        self.load_entries();
        fde::find_app_by_id(&self.entries, Ascii::new(app_id))
            .and_then(|e| e.name(&self.locales))
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| app_id.to_string())
    }

    /// What it takes to launch the app: its desktop entry's `Exec=` and
    /// so on. None without an entry.
    pub fn entry(&mut self, app_id: &str) -> Option<AppEntry> {
        self.load_entries();
        if app_id.is_empty() {
            return None;
        }
        let e = fde::find_app_by_id(&self.entries, Ascii::new(app_id))?;
        Some(AppEntry {
            desktop_id: format!("{}.desktop", e.id()),
            name: e
                .name(&self.locales)
                .map(|n| n.trim().to_string())
                .unwrap_or_default(),
            icon: e.icon().filter(|i| !i.is_empty()).map(Into::into),
            exec: e.exec().filter(|x| !x.trim().is_empty()).map(Into::into),
            terminal: e.terminal(),
            path: e.path.clone(),
        })
    }

    /// The icon for `key`, from the cache or looked up and decoded.
    pub fn get(&mut self, key: &Key) -> Option<Arc<Image>> {
        if let Some(cached) = self.cache.get(key) {
            return cached.clone();
        }
        let image = self.load(key).map(Arc::new);
        self.cache.insert(key.clone(), image.clone());
        image
    }

    fn load(&mut self, key: &Key) -> Option<Image> {
        if key.app_id.is_empty() || key.size == 0 {
            return None;
        }
        let icon = fde::find_app_by_id(&self.entries, Ascii::new(key.app_id.as_str()))
            .and_then(|e| e.icon())
            .filter(|i| !i.is_empty())
            .map(str::to_string);
        let candidates: Vec<String> = match icon {
            Some(icon) if icon != key.app_id => vec![icon, key.app_id.clone()],
            _ => vec![key.app_id.clone()],
        };
        let theme = match &key.theme {
            Some(t) => t.clone(),
            None => self
                .default_theme
                .get_or_insert_with(gsettings_theme)
                .clone()
                .unwrap_or_else(|| "hicolor".into()),
        };
        candidates.iter().find_map(|name| {
            let path = resolve(name, key.size, &theme)?;
            decode(&path, key.size)
                .inspect_err(|e| wl_shell_log::warn!("icon {}: {e}", path.display()))
                .ok()
        })
    }
}

/// The launch side of a desktop entry, as plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppEntry {
    /// The desktop file ID, e.g. `org.gnome.Nautilus.desktop`.
    pub desktop_id: String,
    /// The localized `Name=`.
    pub name: String,
    pub icon: Option<String>,
    /// `Exec=` as written (after the file's own escapes).
    pub exec: Option<String>,
    /// `Terminal=true`: run it in a terminal.
    pub terminal: bool,
    /// The `.desktop` file.
    pub path: PathBuf,
}

impl AppEntry {
    /// The command to run `Exec=` as argv, opening `files` (paths or
    /// URIs; may be empty). See `exec_argv`.
    pub fn argv(&self, files: &[String]) -> Result<Vec<String>, String> {
        let exec = self
            .exec
            .as_deref()
            .ok_or("the desktop entry has no Exec")?;
        exec_argv(exec, files, self)
    }
}

/// `exec` (an `Exec=` value) split into argv, with its field codes
/// expanded: `%f %u` give the first of `files` and `%F %U` all of them
/// (an argument that is only the code is dropped without files), `%i`
/// gives `--icon <Icon>`, `%c` the name, `%k` the entry's path, `%%` a
/// percent sign; the deprecated `%d %D %n %N %v %m` are removed.
///
/// Arguments are split on unquoted spaces. In double quotes, a backslash
/// escapes `"`, `` ` ``, `$` and `\`; quoted text keeps its field codes
/// as written, except `%%`.
pub fn exec_argv(exec: &str, files: &[String], entry: &AppEntry) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    for (arg, quoted) in split_exec(exec)? {
        if quoted {
            argv.push(arg.replace("%%", "%"));
            continue;
        }
        match arg.as_str() {
            "%f" | "%u" => argv.extend(files.first().cloned()),
            "%F" | "%U" => argv.extend(files.iter().cloned()),
            "%i" => {
                if let Some(icon) = &entry.icon {
                    argv.extend(["--icon".to_string(), icon.clone()]);
                }
            }
            _ => {
                let expanded = expand_codes(&arg, files, entry);
                // `%d` and friends alone leave nothing.
                if !expanded.is_empty() || !arg.starts_with('%') {
                    argv.push(expanded);
                }
            }
        }
    }
    if argv.is_empty() {
        return Err(format!("Exec has no command: {exec}"));
    }
    Ok(argv)
}

/// The field codes in part of an argument.
fn expand_codes(arg: &str, files: &[String], entry: &AppEntry) -> String {
    let mut out = String::new();
    let mut chars = arg.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some('f' | 'u') => out.push_str(files.first().map_or("", String::as_str)),
            Some('F' | 'U') => out.push_str(&files.join(" ")),
            Some('c') => out.push_str(&entry.name),
            Some('k') => out.push_str(&entry.path.to_string_lossy()),
            Some('i') => out.push_str(entry.icon.as_deref().unwrap_or("")),
            // Deprecated or unknown codes are removed.
            Some(_) | None => {}
        }
    }
    out
}

/// Split an `Exec=` value into arguments, each with whether any of it was
/// quoted.
fn split_exec(exec: &str) -> Result<Vec<(String, bool)>, String> {
    let mut args = Vec::new();
    let mut arg: Option<(String, bool)> = None;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => args.extend(arg.take()),
            '"' => {
                let (s, quoted) = arg.get_or_insert_default();
                *quoted = true;
                loop {
                    match chars.next() {
                        None => return Err(format!("unterminated quote in Exec: {exec}")),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(e @ ('"' | '`' | '$' | '\\')) => s.push(e),
                            Some(e) => {
                                s.push('\\');
                                s.push(e);
                            }
                            None => return Err(format!("unterminated quote in Exec: {exec}")),
                        },
                        Some(c) => s.push(c),
                    }
                }
            }
            // Not allowed unquoted by the spec, but seen: escape the next.
            '\\' => {
                let (s, _) = arg.get_or_insert_default();
                s.extend(chars.next());
            }
            c => arg.get_or_insert_default().0.push(c),
        }
    }
    args.extend(arg);
    Ok(args)
}

/// The GNOME icon theme (a directory name), if gsettings knows one.
fn gsettings_theme() -> Option<String> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let name = String::from_utf8(out.stdout).ok()?;
    let name = name.trim().trim_matches('\'');
    (!name.is_empty()).then(|| name.to_string())
}

/// An absolute path as is, a name through the icon theme.
fn resolve(icon: &str, size: u32, theme: &str) -> Option<PathBuf> {
    let path = Path::new(icon);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    // Theme sizes are logical; asking for the full pixel size at scale 1
    // finds the best match for it.
    let size = size.min(u16::MAX as u32) as u16;
    freedesktop_icons::lookup(icon)
        .with_size(size)
        .with_theme(theme)
        .find()
}

/// Decode an icon file to a `size`x`size` image, fitted and centered.
fn decode(path: &Path, size: u32) -> Result<Image, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let rgba = match ext.as_deref() {
        Some("svg") => render_svg(&data, size)?,
        _ => {
            let img = image::load_from_memory(&data).map_err(|e| e.to_string())?;
            let mut rgba = img.into_rgba8();
            // Scale premultiplied pixels, so transparent ones don't bleed.
            premultiply_rgba(&mut rgba);
            let (w, h) = rgba.dimensions();
            let (fw, fh) = fit(w, h, size);
            if (fw, fh) != (w, h) {
                rgba =
                    image::imageops::resize(&rgba, fw, fh, image::imageops::FilterType::Lanczos3);
            }
            Premultiplied(rgba)
        }
    };
    Ok(center(rgba, size))
}

/// RGBA8 pixels with premultiplied alpha.
struct Premultiplied(image::RgbaImage);

fn premultiply_rgba(img: &mut image::RgbaImage) {
    for p in img.pixels_mut() {
        let a = p.0[3] as u32;
        for c in &mut p.0[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8;
        }
    }
}

/// `w`x`h` scaled down (never up) to fit in `size`, keeping the aspect.
fn fit(w: u32, h: u32, size: u32) -> (u32, u32) {
    if w <= size && h <= size {
        return (w.max(1), h.max(1));
    }
    let s = size as f64 / w.max(h) as f64;
    (
        ((w as f64 * s).round() as u32).clamp(1, size),
        ((h as f64 * s).round() as u32).clamp(1, size),
    )
}

fn render_svg(data: &[u8], size: u32) -> Result<Premultiplied, String> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let svg = tree.size();
    let s = size as f32 / svg.width().max(svg.height());
    let (w, h) = (
        ((svg.width() * s).round() as u32).clamp(1, size),
        ((svg.height() * s).round() as u32).clamp(1, size),
    );
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("bad size")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(s, s),
        &mut pixmap.as_mut(),
    );
    // tiny-skia pixmaps are premultiplied RGBA.
    image::RgbaImage::from_raw(w, h, pixmap.take())
        .map(Premultiplied)
        .ok_or_else(|| "bad pixmap".into())
}

/// A `size`x`size` premultiplied ARGB8888 image with `img` centered on it.
fn center(Premultiplied(img): Premultiplied, size: u32) -> Image {
    let (w, h) = img.dimensions();
    let (ox, oy) = ((size - w.min(size)) / 2, (size - h.min(size)) / 2);
    let mut data = vec![0; (size * size * 4) as usize];
    for (x, y, p) in img.enumerate_pixels() {
        let (x, y) = (x + ox, y + oy);
        if x < size && y < size {
            let [r, g, b, a] = p.0;
            let i = ((y * size + x) * 4) as usize;
            data[i..i + 4].copy_from_slice(&[b, g, r, a]);
        }
    }
    Image {
        width: size,
        height: size,
        data,
    }
}

#[cfg(test)]
mod exec_tests {
    use super::*;

    fn entry() -> AppEntry {
        AppEntry {
            desktop_id: "org.example.App.desktop".into(),
            name: "Example".into(),
            icon: Some("example".into()),
            exec: None,
            terminal: false,
            path: "/usr/share/applications/org.example.App.desktop".into(),
        }
    }

    fn argv(exec: &str, files: &[&str]) -> Result<Vec<String>, String> {
        let files: Vec<String> = files.iter().map(|f| f.to_string()).collect();
        exec_argv(exec, &files, &entry())
    }

    #[test]
    fn field_codes_without_files_are_removed() {
        assert_eq!(argv("firefox %u", &[]).unwrap(), ["firefox"]);
        assert_eq!(
            argv("nautilus --new-window %U", &[]).unwrap(),
            ["nautilus", "--new-window"]
        );
        assert_eq!(argv("app %F -x", &[]).unwrap(), ["app", "-x"]);
        assert_eq!(argv("app %d %D %n %N %v %m", &[]).unwrap(), ["app"]);
    }

    #[test]
    fn field_codes_with_files() {
        assert_eq!(argv("app %f", &["/a b", "/c"]).unwrap(), ["app", "/a b"]);
        assert_eq!(
            argv("app %U", &["/a b", "/c"]).unwrap(),
            ["app", "/a b", "/c"]
        );
    }

    #[test]
    fn entry_codes() {
        assert_eq!(
            argv("app %i --class=%c %k 100%%", &[]).unwrap(),
            [
                "app",
                "--icon",
                "example",
                "--class=Example",
                "/usr/share/applications/org.example.App.desktop",
                "100%",
            ]
        );
        let mut no_icon = entry();
        no_icon.icon = None;
        assert_eq!(exec_argv("app %i", &[], &no_icon).unwrap(), ["app"]);
    }

    #[test]
    fn quoting() {
        assert_eq!(
            argv(r#""/opt/My App/run" --arg "a \"b\" \$c \\d" x"#, &[]).unwrap(),
            ["/opt/My App/run", "--arg", r#"a "b" $c \d"#, "x"]
        );
        // Quoted field codes stay as written; an empty quoted arg stays.
        assert_eq!(
            argv(r#"sh -c "echo %f" """#, &[]).unwrap(),
            ["sh", "-c", "echo %f", ""]
        );
        assert_eq!(argv("a  b\tc", &[]).unwrap(), ["a", "b", "c"]);
        assert!(argv(r#"app "open"#, &[]).is_err());
        assert!(argv("%u", &[]).is_err());
        assert!(argv("   ", &[]).is_err());
    }

    #[test]
    fn entry_argv_needs_exec() {
        let mut e = entry();
        assert!(e.argv(&[]).is_err());
        e.exec = Some("foot -e htop".into());
        assert_eq!(e.argv(&[]).unwrap(), ["foot", "-e", "htop"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fitting() {
        assert_eq!(fit(256, 256, 48), (48, 48));
        assert_eq!(fit(32, 32, 48), (32, 32));
        assert_eq!(fit(200, 100, 48), (48, 24));
        assert_eq!(fit(1, 1000, 48), (1, 48));
    }

    #[test]
    fn centered() {
        let img = image::RgbaImage::from_pixel(2, 4, image::Rgba([10, 20, 30, 255]));
        let out = center(Premultiplied(img), 4);
        assert_eq!((out.width, out.height), (4, 4));
        let px = |x: u32, y: u32| &out.data[((y * 4 + x) * 4) as usize..][..4];
        assert_eq!(px(0, 0), [0, 0, 0, 0]);
        assert_eq!(px(1, 0), [30, 20, 10, 255]);
        assert_eq!(px(2, 3), [30, 20, 10, 255]);
        assert_eq!(px(3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn premultiplied() {
        let mut img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 128, 0, 128]));
        premultiply_rgba(&mut img);
        assert_eq!(img.get_pixel(0, 0).0, [128, 64, 0, 128]);
    }

    #[test]
    fn svg() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8">
            <rect width="16" height="8" fill="#ff0000"/></svg>"##;
        let out = center(render_svg(svg, 32).unwrap(), 32);
        let px = |x: u32, y: u32| &out.data[((y * 32 + x) * 4) as usize..][..4];
        // 32x16, centered vertically.
        assert_eq!(px(16, 16), [0, 0, 255, 255]);
        assert_eq!(px(16, 4), [0, 0, 0, 0]);
    }
}

#[cfg(test)]
mod system_tests {
    use super::*;

    /// Needs installed desktop entries and fonts:
    /// `cargo test system -- --ignored --nocapture`
    #[test]
    #[ignore = "depends on the installed system"]
    fn system_lookup() {
        let font = "sans-serif";
        let mut icons = Icons::new();
        icons.load_entries();
        let mut text = crate::text::Text::new();
        for app_id in [
            "firefox",
            "org.gnome.Nautilus",
            "com.mitchellh.ghostty",
            "nope.nothing",
        ] {
            let key = Key::new(app_id, 48, None, None, 2);
            let start = std::time::Instant::now();
            match icons.get(&key) {
                Some(img) => println!(
                    "{app_id}: {}x{} in {:?}",
                    img.width,
                    img.height,
                    start.elapsed()
                ),
                None => {
                    let img = text.letter(app_id, key.size, font);
                    assert!(img.data.iter().any(|&b| b != 0));
                    println!("{app_id}: letter tile {}x{}", img.width, img.height);
                }
            }
        }
        let style = crate::text::Style {
            font: font.into(),
            size: 14.0,
            color: 0xffffffff,
            background: 0x000000b3,
        };
        let pill = text.pill(
            "A rather long window title — 日本語 🎉 that should be cut",
            &style,
            200,
            2,
        );
        println!(
            "pill {}x{} ({}x{})",
            pill.width, pill.height, pill.image.width, pill.image.height
        );
        assert!(pill.width <= 200);
        assert_eq!(pill.image.width as i32, pill.width * 2);
    }
}
