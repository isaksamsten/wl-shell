//! A cache of ready `WlBuffer`s, so drawing a frame renders nothing:
//!
//! - Icons and app name labels, by key (one per app and output scale),
//!   and icons by name (menu rows). A widget's tiles are icons too: a new
//!   tile replaces the app's older ones of its size, and until it is
//!   there, the older one (or the app's icon) stands in. They are rendered by the worker; the
//!   program turns the results into buffers as they arrive.
//! - Shapes (shapes.rs), rendered here on first use for a size, color and
//!   scale. The most recently used few dozen are kept.
//!
//! Replaced buffers are only destroyed by `trim`, which the program calls
//! when none can be attached anymore (between shows, or after a frame that
//! no longer uses them): one replaced mid-frame may still be attached, or
//! about to be with the next parent commit.

use std::collections::HashMap;

use smithay_client_toolkit::shm::raw::RawPool;
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_shm},
};

use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Program, Shell};
use wl_shell_log::warn;
use wl_shell_render::worker::LabelKey;
use wl_shell_render::{Image, icons, shadow, text};

use crate::shapes::{Shape, ShapeKey, strip_image};

/// Shapes kept: a strip of a dozen cards has four shadow strips each.
const MAX_SHAPES: usize = 64;
/// Labels kept per app: a few names, at a few scales.
const MAX_LABELS_PER_APP: usize = 8;

/// A buffer and its logical size.
#[derive(Clone, Debug)]
pub struct Sized {
    pub buffer: WlBuffer,
    pub width: i32,
    pub height: i32,
}

#[derive(Default)]
pub struct Cache {
    icons: HashMap<icons::Key, WlBuffer>,
    /// None: there is no such icon.
    named: HashMap<icons::NamedIcon, Option<WlBuffer>>,
    labels: Vec<(LabelKey, Sized)>,
    /// Most recently used first.
    shapes: Vec<(ShapeKey, Shape)>,
    /// Replaced buffers, destroyed by `trim`.
    stale: Vec<WlBuffer>,
}

impl Cache {
    pub fn icon(&self, key: &icons::Key) -> Option<WlBuffer> {
        self.icons.get(key).cloned()
    }

    /// The icon of `key`; for a tile that isn't there yet, the app's
    /// last tile (of its size if there is one), else the app's icon.
    pub fn icon_or_older(&self, key: &icons::Key) -> Option<WlBuffer> {
        if let Some(b) = self.icons.get(key) {
            return Some(b.clone());
        }
        let tile = key.tile?;
        let same = |k: &icons::Key| {
            k.app_id == key.app_id && k.scale == key.scale && k.shadow == key.shadow
        };
        let older = (self.icons.iter())
            .filter(|(k, _)| same(k) && k.tile.is_some())
            .max_by_key(|(k, _)| {
                let t = k.tile.unwrap_or(tile);
                ((t.w, t.h) == (tile.w, tile.h), t.version)
            });
        match older {
            Some((_, b)) => Some(b.clone()),
            None => self.icons.get(&key.clone().with_tile(None)).cloned(),
        }
    }

    pub fn insert_icon<P: Program>(
        &mut self,
        g: &Globals,
        key: icons::Key,
        img: &Image,
        qh: &QueueHandle<Shell<P>>,
    ) {
        match shm_buffer(g, img, qh) {
            Ok(buffer) => {
                // A tile replaces its app's older ones of its size.
                if let Some(tile) = key.tile {
                    let older: Vec<icons::Key> = (self.icons.keys())
                        .filter(|k| {
                            k.app_id == key.app_id
                                && k.scale == key.scale
                                && k.shadow == key.shadow
                                && k.tile.is_some_and(|t| {
                                    (t.w, t.h) == (tile.w, tile.h) && t.version != tile.version
                                })
                        })
                        .cloned()
                        .collect();
                    for k in older {
                        self.stale.extend(self.icons.remove(&k));
                    }
                }
                if let Some(old) = self.icons.insert(key, buffer) {
                    self.stale.push(old);
                }
            }
            Err(e) => warn!("icon buffer: {e}"),
        }
    }

    /// The icon named by `key`: None if it hasn't arrived, Some(None) if
    /// there is none.
    pub fn named_icon(&self, key: &icons::NamedIcon) -> Option<Option<WlBuffer>> {
        self.named.get(key).cloned()
    }

    pub fn insert_named_icon<P: Program>(
        &mut self,
        g: &Globals,
        key: icons::NamedIcon,
        img: Option<&Image>,
        qh: &QueueHandle<Shell<P>>,
    ) {
        let buffer = img.and_then(|img| {
            shm_buffer(g, img, qh)
                .inspect_err(|e| warn!("icon buffer: {e}"))
                .ok()
        });
        if let Some(Some(old)) = self.named.insert(key, buffer) {
            self.stale.push(old);
        }
    }

    pub fn label(&self, key: &LabelKey) -> Option<&Sized> {
        self.labels.iter().find(|(k, _)| k == key).map(|(_, s)| s)
    }

    /// The widest cached label of these.
    pub fn widest_label<'a>(&self, keys: impl IntoIterator<Item = &'a LabelKey>) -> i32 {
        keys.into_iter()
            .filter_map(|k| self.label(k))
            .map(|s| s.width)
            .max()
            .unwrap_or(0)
    }

    pub fn insert_label<P: Program>(
        &mut self,
        g: &Globals,
        key: LabelKey,
        pill: &text::Pill,
        qh: &QueueHandle<Shell<P>>,
    ) {
        let buffer = match shm_buffer(g, &pill.image, qh) {
            Ok(buffer) => buffer,
            Err(e) => return warn!("label buffer: {e}"),
        };
        let sized = Sized {
            buffer,
            width: pill.width,
            height: pill.height,
        };
        match self.labels.iter_mut().find(|(k, _)| *k == key) {
            Some((_, old)) => self.stale.push(std::mem::replace(old, sized).buffer),
            None => {
                // Labels that change (a widget's clock) push out their
                // app's oldest ones.
                let app = key.app_id.clone();
                self.labels.push((key, sized));
                let of_app =
                    |l: &[(LabelKey, Sized)]| l.iter().filter(|(k, _)| k.app_id == app).count();
                while of_app(&self.labels) > MAX_LABELS_PER_APP {
                    if let Some(i) = self.labels.iter().position(|(k, _)| k.app_id == app) {
                        self.stale.push(self.labels.remove(i).1.buffer);
                    }
                }
            }
        }
    }

    /// The shape for `key`, rendered now if it isn't cached.
    pub fn shape<P: Program>(
        &mut self,
        g: &Globals,
        key: ShapeKey,
        qh: &QueueHandle<Shell<P>>,
    ) -> Option<Shape> {
        if let Some(i) = self.shapes.iter().position(|(k, _)| *k == key) {
            let entry = self.shapes.remove(i);
            let shape = entry.1.clone();
            self.shapes.insert(0, entry);
            return Some(shape);
        }
        let (img, rect) = key.render()?;
        let buffer = shm_buffer(g, &img, qh)
            .inspect_err(|e| warn!("shape buffer: {e}"))
            .ok()?;
        let shape = Shape { buffer, rect };
        self.shapes.insert(0, (key, shape.clone()));
        Some(shape)
    }

    /// The four sides of a `w`x`h` card's shadow (top, bottom, left,
    /// right; None where empty), rendered together if any is missing.
    pub fn card_shadow<P: Program>(
        &mut self,
        g: &Globals,
        (w, h): (i32, i32),
        shadow: shadow::Settings,
        scale: i32,
        qh: &QueueHandle<Shell<P>>,
    ) -> [Option<Shape>; 4] {
        let key = |side: u8| ShapeKey::CardShadow {
            w,
            h,
            shadow,
            side,
            scale,
        };
        let cached = |cache: &Self, side| cache.shapes.iter().any(|(k, _)| *k == key(side));
        if (0..4).all(|side| cached(self, side)) {
            return [0, 1, 2, 3].map(|side| self.shape(g, key(side), qh));
        }
        let strips = shadow::render(w, h, &shadow, scale);
        let mut side = 0;
        strips.map(|strip| {
            let k = key(side);
            side += 1;
            let strip = strip?;
            let img = strip_image(strip.width, strip.height, strip.pixels);
            let buffer = shm_buffer(g, &img, qh)
                .inspect_err(|e| warn!("shadow buffer: {e}"))
                .ok()?;
            let shape = Shape {
                buffer,
                rect: strip.rect,
            };
            match self.shapes.iter_mut().find(|(sk, _)| *sk == k) {
                Some((_, old)) => self
                    .stale
                    .push(std::mem::replace(old, shape.clone()).buffer),
                None => self.shapes.insert(0, (k, shape.clone())),
            }
            Some(shape)
        })
    }

    /// Forget icons and labels, e.g. because the config or the desktop
    /// entries changed.
    pub fn clear_icons(&mut self) {
        self.stale.extend(self.icons.drain().map(|(_, b)| b));
        self.stale.extend(self.named.drain().filter_map(|(_, b)| b));
        self.stale
            .extend(self.labels.drain(..).map(|(_, s)| s.buffer));
    }

    /// Forget shapes, e.g. because the config changed.
    pub fn clear_shapes(&mut self) {
        self.stale
            .extend(self.shapes.drain(..).map(|(_, s)| s.buffer));
    }

    /// When no replaced buffer can be attached anymore: destroy them, and
    /// the least recently used shapes.
    pub fn trim(&mut self) {
        while self.shapes.len() > MAX_SHAPES {
            if let Some((_, s)) = self.shapes.pop() {
                self.stale.push(s.buffer);
            }
        }
        for buffer in self.stale.drain(..) {
            buffer.destroy();
        }
    }
}

/// An shm buffer holding `img`. The pool is gone once this returns; the
/// buffer keeps the memory alive.
pub fn shm_buffer<P: Program>(
    g: &Globals,
    img: &Image,
    qh: &QueueHandle<Shell<P>>,
) -> std::io::Result<WlBuffer> {
    let len = img.data.len();
    if len > i32::MAX as usize || len != img.width as usize * img.height as usize * 4 || len == 0 {
        return Err(std::io::Error::other("bad buffer size"));
    }
    let mut pool = RawPool::new(len, &g.shm).map_err(std::io::Error::other)?;
    pool.mmap()[..len].copy_from_slice(&img.data);
    Ok(pool.create_buffer(
        0,
        img.width as i32,
        img.height as i32,
        img.width as i32 * 4,
        wl_shm::Format::Argb8888,
        Ignore,
        qh,
    ))
}

/// A 1x1 buffer of an 0xRRGGBBAA color, for scaling with a viewport.
pub fn color_buffer<P: Program>(
    g: &Globals,
    rgba: u32,
    qh: &QueueHandle<Shell<P>>,
) -> std::io::Result<WlBuffer> {
    // Both buffer kinds take premultiplied alpha.
    let [r, gr, b, a] = rgba.to_be_bytes().map(|c| c as f64 / 255.0);
    let [r, gr, b] = [r, gr, b].map(|c| c * a);
    if let Some(single_pixel) = &g.single_pixel {
        let c = |v: f64| (v * u32::MAX as f64) as u32;
        return Ok(single_pixel.create_u32_rgba_buffer(c(r), c(gr), c(b), c(a), qh, Ignore));
    }
    let c = |v: f64| (v * 255.0).round() as u8;
    let img = Image {
        width: 1,
        height: 1,
        data: vec![c(b), c(gr), c(r), c(a)],
    };
    shm_buffer(g, &img, qh)
}
