//! Full-screen layer surface: a card on an opaque background, with one
//! subsurface per window thumbnail on it.
//!
//! The main surface shows the background color, or the background image
//! blurred and tinted with it. Above it are the card's shadow, the card,
//! which shows the background image (or the card color), and over the card
//! a screenshot of the output. The two start at full size and shrink all the
//! way into place over the blurred background. Everything else is in
//! `content`, a transparent subsurface that hides it all until the
//! thumbnails start spreading out. With wp_alpha_modifier_v1, that starts
//! early in the shrink, as the screenshot fades out and the thumbnails fade
//! in; otherwise the screenshot disappears at once near its end.
//!
//! Colors are single-pixel buffers (or 1x1 shm buffers if the compositor
//! lacks wp_single_pixel_buffer_manager_v1) scaled with viewports;
//! thumbnails attach the capture buffers directly and let the compositor
//! scale them. Shadows are up to four shm strips around each thumbnail,
//! below every thumbnail. App icons (shm, one buffer per app) sit on the
//! thumbnails' bottom edges, above every thumbnail, and the selected
//! window's title pill is above everything. Icons and pills are rendered by
//! the worker thread and attached when they arrive. The selected thumbnail
//! is raised above the others and zoomed (animated on frame callbacks, like
//! the opening), its shadow and icon following it. All subsurfaces have an
//! empty input region, so pointer events arrive on the main surface and are
//! hit-tested here.
//!
//! Closing fades everything out (with wp_alpha_modifier_v1) after giving up
//! the keyboard and input, so the result can be acted on right away.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{FrameCallbackData, Region},
    dispatch2::Dispatch2,
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
    shm::raw::RawPool,
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    protocol::{
        wl_buffer::WlBuffer, wl_output::Transform, wl_output::WlOutput, wl_shm,
        wl_subsurface::WlSubsurface, wl_surface::WlSurface,
    },
};
use wayland_protocols::{
    ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
    wp::{
        alpha_modifier::v1::client::wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
        viewporter::client::wp_viewport::WpViewport,
    },
};

use wl_shell_core::Shell;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId, Frame};
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::toplevel::Toplevel;
use wl_shell_log::warn;
use wl_shell_render::worker::PillRequest;
use wl_shell_render::{Image, icons, shadow, text};

use crate::app::Expose;
use crate::assets::Assets;
use crate::background;
use crate::config::Config;
use crate::ipc::Request;
use crate::layout::{self, Dir};

type Qh = QueueHandle<Shell<Expose>>;

/// Layout size of a window whose capture size is not known yet.
const DEFAULT_SIZE: (f64, f64) = (800.0, 500.0);

/// Space between the icon and the title pill, logical pixels.
const PILL_GAP: i32 = 6;

/// A layout rect in surface-local logical pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x as f64
            && x < (self.x + self.w) as f64
            && y >= self.y as f64
            && y < (self.y + self.h) as f64
    }
}

/// A desync subsurface scaled with a viewport, without input.
struct Sub {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    /// While it is faded in or out.
    alpha: Option<WpAlphaModifierSurfaceV1>,
}

impl Sub {
    fn new(g: &Globals, parent: &WlSurface, no_input: &Region, qh: &Qh) -> Self {
        let (subsurface, surface) = g.subcompositor.create_subsurface(parent.clone(), qh);
        subsurface.set_desync();
        let viewport = g.viewporter.get_viewport(&surface, qh, Ignore);
        surface.set_input_region(Some(no_input.wl_region()));
        Self {
            surface,
            subsurface,
            viewport,
            alpha: None,
        }
    }

    /// Start fading: show it at `a` (0 to 1) from the next commit. False
    /// without wp_alpha_modifier_v1.
    fn start_fade(&mut self, g: &Globals, a: f64, qh: &Qh) -> bool {
        let Some(manager) = &g.alpha_modifier else {
            return false;
        };
        if self.alpha.is_none() {
            self.alpha = Some(manager.get_surface(&self.surface, qh, Ignore));
        }
        self.set_alpha(a);
        true
    }

    /// Show it at `a` (0 to 1) from the next commit, while fading.
    fn set_alpha(&self, a: f64) {
        if let Some(alpha) = &self.alpha {
            alpha.set_multiplier((a.clamp(0.0, 1.0) * u32::MAX as f64).round() as u32);
        }
    }

    /// Stop fading: fully opaque again from the next commit.
    fn end_fade(&mut self) {
        if let Some(alpha) = self.alpha.take() {
            alpha.destroy();
        }
    }

    fn attach(&self, buffer: &WlBuffer) {
        self.surface.attach(Some(buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    }

    fn place(&self, r: IRect) {
        self.subsurface.set_position(r.x, r.y);
        self.viewport.set_destination(r.w, r.h);
    }
}

impl Drop for Sub {
    fn drop(&mut self) {
        // Before the surface, or it is a protocol error.
        self.end_fade();
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// What a thumbnail currently shows.
#[derive(Clone, Copy, PartialEq)]
struct Shown {
    width: u32,
    height: u32,
    transform: Transform,
}

fn transform_swaps(t: Transform) -> bool {
    u32::from(t) & 1 != 0 // 90 and 270, flipped or not
}

/// A window's shadow: top, bottom, left and right strips.
struct Shadow {
    strips: [Sub; 4],
    /// The attached buffers.
    buffers: [Option<WlBuffer>; 4],
    /// Strip rects relative to the thumbnail.
    rects: [Option<shadow::Rect>; 4],
    /// Thumbnail size and scale the buffers were rendered for.
    rendered: Option<(i32, i32, i32)>,
}

impl Shadow {
    fn new(g: &Globals, parent: &WlSurface, no_input: &Region, qh: &Qh) -> Self {
        Self {
            strips: std::array::from_fn(|_| Sub::new(g, parent, no_input, qh)),
            buffers: Default::default(),
            rects: [None; 4],
            rendered: None,
        }
    }

    /// Render the shadow of a `w`x`h` thumbnail, unless it already is at
    /// this scale. Committed by `place`.
    fn render(
        &mut self,
        g: &Globals,
        settings: &shadow::Settings,
        (w, h): (i32, i32),
        scale: i32,
        qh: &Qh,
    ) {
        let key = (w, h, scale);
        if self.rendered != Some(key) {
            self.rendered = Some(key);
            let strips = shadow::render(w, h, settings, scale);
            for (k, strip) in strips.into_iter().enumerate() {
                if let Some(old) = self.buffers[k].take() {
                    retire(&old);
                }
                let buffer = strip.as_ref().and_then(|s| match strip_buffer(g, s, qh) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        warn!("shadow buffer: {e}");
                        None
                    }
                });
                let sub = &self.strips[k];
                match &buffer {
                    Some(b) => sub.attach(b),
                    None => sub.surface.attach(None, 0, 0),
                }
                self.rects[k] = buffer.is_some().then(|| strip.unwrap().rect);
                self.buffers[k] = buffer;
            }
        }
    }

    /// Follow the thumbnail laid out at `base` and shown at `shown`: the
    /// strips are scaled like the thumbnail rather than re-rendered.
    fn place(&self, base: IRect, shown: IRect) {
        let zx = shown.w as f64 / base.w as f64;
        let zy = shown.h as f64 / base.h as f64;
        let edge = |v: i32, z: f64| (v as f64 * z).round() as i32;
        for (sub, rect) in self.strips.iter().zip(self.rects) {
            if let Some(s) = rect {
                let (x0, y0) = (edge(s.x, zx), edge(s.y, zy));
                let (x1, y1) = (edge(s.x + s.w, zx), edge(s.y + s.h, zy));
                sub.place(IRect {
                    x: shown.x + x0,
                    y: shown.y + y0,
                    w: (x1 - x0).max(1),
                    h: (y1 - y0).max(1),
                });
            }
            sub.surface.commit();
        }
    }
}

impl Drop for Shadow {
    fn drop(&mut self) {
        // Like capture buffers, attached ones may be destroyed right away.
        for buffer in self.buffers.iter().flatten() {
            buffer.destroy();
        }
    }
}

/// An shm buffer holding premultiplied ARGB8888 `pixels`. The pool is gone
/// once this returns; the buffer keeps the memory alive.
fn shm_buffer<U>(
    g: &Globals,
    width: u32,
    height: u32,
    pixels: &[u8],
    data: U,
    qh: &Qh,
) -> std::io::Result<WlBuffer>
where
    Shell<Expose>: wayland_client::Dispatch<WlBuffer, U>,
    U: Send + Sync + 'static,
{
    let len = pixels.len();
    if len > i32::MAX as usize || len != width as usize * height as usize * 4 || len == 0 {
        return Err(std::io::Error::other("bad buffer size"));
    }
    let mut pool = RawPool::new(len, &g.shm).map_err(std::io::Error::other)?;
    pool.mmap()[..len].copy_from_slice(pixels);
    Ok(pool.create_buffer(
        0,
        width as i32,
        height as i32,
        width as i32 * 4,
        wl_shm::Format::Argb8888,
        data,
        qh,
    ))
}

fn strip_buffer(g: &Globals, strip: &shadow::Strip, qh: &Qh) -> std::io::Result<WlBuffer> {
    let data = OnceBuffer::default();
    shm_buffer(g, strip.width, strip.height, &strip.pixels, data, qh)
}

/// User data of a buffer that is attached once and replaced later (shadow
/// strips, title pills): it is destroyed once it is both replaced (retired)
/// and released by the compositor, whichever comes last.
#[derive(Default)]
struct OnceBuffer(AtomicU8);

const RELEASED: u8 = 1;
const RETIRED: u8 = 2;

fn retire(buffer: &WlBuffer) {
    if let Some(data) = buffer.data::<OnceBuffer>()
        && data.0.fetch_or(RETIRED, Ordering::Relaxed) & RELEASED != 0
    {
        buffer.destroy();
    }
}

impl<D> Dispatch2<WlBuffer, D> for OnceBuffer {
    fn event(
        &self,
        _: &mut D,
        buffer: &WlBuffer,
        _: wayland_client::protocol::wl_buffer::Event,
        _: &Connection,
        _: &QueueHandle<D>,
    ) {
        // The only event is release. Each buffer is attached exactly once.
        if self.0.fetch_or(RELEASED, Ordering::Relaxed) & RETIRED != 0 {
            buffer.destroy();
        }
    }
}

pub struct Window {
    pub id: CaptureId,
    pub handle: ExtForeignToplevelHandleV1,
    pub identifier: String,
    app_id: String,
    title: String,
    capture: Capture,
    sub: Sub,
    /// None with shadows disabled.
    shadow: Option<Shadow>,
    /// None with icons disabled.
    icon: Option<Icon>,
    /// Where the layout put the thumbnail, unzoomed.
    pub rect: IRect,
    /// The capture frame attached to the thumbnail; None: the placeholder.
    shown: Option<Shown>,
    /// Current zoom factor; 1: unzoomed.
    zoom: f64,
    /// The running zoom animation, if any.
    anim: Option<ZoomAnim>,
}

/// A zoom animation from `from` to `to`, started at `start`.
#[derive(Clone, Copy)]
struct ZoomAnim {
    from: f64,
    to: f64,
    start: Instant,
}

impl ZoomAnim {
    /// The zoom at `now`, eased out (cubic); None once finished.
    fn at(&self, now: Instant, duration: Duration) -> Option<f64> {
        let t = now.saturating_duration_since(self.start).as_secs_f64();
        let t = t / duration.as_secs_f64().max(f64::MIN_POSITIVE);
        (t < 1.0).then(|| {
            let e = 1.0 - (1.0 - t).powi(3);
            self.from + (self.to - self.from) * e
        })
    }
}

/// A window's app icon, centered on the thumbnail's bottom edge.
struct Icon {
    sub: Sub,
    rect: IRect,
    /// The icon attached, if any.
    attached: Option<icons::Key>,
}

impl Window {
    /// Every subsurface of the window that shows something.
    fn subs(&mut self) -> impl Iterator<Item = &mut Sub> {
        std::iter::once(&mut self.sub)
            .chain(self.shadow.iter_mut().flat_map(|s| s.strips.iter_mut()))
            .chain(self.icon.iter_mut().map(|i| &mut i.sub))
    }

    /// Logical size of the window's content, for the layout.
    fn size(&self, scale: i32) -> layout::Item {
        let (w, h, transform) = match (self.shown, self.capture.size()) {
            (Some(s), _) => (s.width, s.height, s.transform),
            (None, Some((w, h))) => (w, h, Transform::Normal),
            (None, None) => {
                return layout::Item {
                    w: DEFAULT_SIZE.0,
                    h: DEFAULT_SIZE.1,
                };
            }
        };
        let (w, h) = if transform_swaps(transform) {
            (h, w)
        } else {
            (w, h)
        };
        // Capture sizes are in buffer pixels; never show windows larger than
        // their real size on a scaled output.
        let scale = scale as f64;
        layout::Item {
            w: w as f64 / scale,
            h: h as f64 / scale,
        }
    }
}

struct Style {
    gap: f64,
    margin: f64,
    /// Zoom factor of the selected thumbnail; 1: none.
    zoom: f64,
    zoom_duration: Duration,
    max_scale: f64,
    shadow: Option<shadow::Settings>,
    /// Logical pixels; 0: no icons.
    icon_size: i32,
    icon_theme: Option<String>,
    icon_shadow: Option<shadow::IconShadow>,
    /// For letter icons.
    font: String,
    /// None: no title pill.
    title: Option<text::Style>,
    /// The opening transition; 0: none.
    open_duration: Duration,
    /// The closing fade-out; 0: none.
    close_duration: Duration,
    /// Card size relative to the output.
    card_scale: f64,
    /// The background color; with a wallpaper, its tint.
    background: u32,
    background_blur: u32,
}

impl Style {
    /// How far the icon reaches below the thumbnail: the lower half.
    fn icon_below(&self) -> i32 {
        self.icon_size - self.icon_size / 2
    }

    /// How far below the thumbnail the title pill starts.
    fn pill_offset(&self) -> i32 {
        self.icon_below() + PILL_GAP
    }

    /// Space the layout keeps free below each thumbnail: the lower half of
    /// the icon, then the title pill.
    fn below(&self) -> f64 {
        match &self.title {
            Some(t) => (self.pill_offset() + text::pill_height(t.size)) as f64,
            None => self.icon_below() as f64,
        }
    }
}

/// The title pill on screen: what it shows and where.
struct ShownPill {
    window: CaptureId,
    request: PillRequest,
    buffer: WlBuffer,
    width: i32,
    height: i32,
}

/// A background image in an shm buffer, shared by every overlay of a daemon,
/// with its blurred backgrounds.
pub struct Wallpaper {
    buffer: WlBuffer,
    width: u32,
    height: u32,
    /// Holds the pixels, which the blurred backgrounds are made from.
    pool: RefCell<RawPool>,
    /// Blurred backgrounds, most recently used first.
    blurred: RefCell<Vec<(BlurKey, WlBuffer)>>,
}

/// What a blurred background was made for: output size, blur and tint.
type BlurKey = (u32, u32, u32, u32);

/// Blurred backgrounds kept per wallpaper (outputs of different sizes).
const MAX_BLURRED: usize = 4;

impl Wallpaper {
    pub fn new(g: &Globals, img: &Image, qh: &Qh) -> std::io::Result<Self> {
        let mut pool = RawPool::new(img.data.len(), &g.shm).map_err(std::io::Error::other)?;
        pool.mmap()[..img.data.len()].copy_from_slice(&img.data);
        let (w, h) = (img.width as i32, img.height as i32);
        let buffer =
            pool.create_buffer(0, w, h, img.stride(), wl_shm::Format::Argb8888, Ignore, qh);
        Ok(Self {
            buffer,
            width: img.width,
            height: img.height,
            pool: RefCell::new(pool),
            blurred: RefCell::new(Vec::new()),
        })
    }

    /// The viewport source that makes the image cover a `w`x`h` rect.
    fn cover(&self, w: u32, h: u32) -> (f64, f64, f64, f64) {
        background::cover_source(self.width, self.height, w, h)
    }

    /// The image blurred by `blur` with `tint` over it, for a `w`x`h`
    /// output, to be scaled up to it; made once and cached.
    fn blurred(
        &self,
        g: &Globals,
        (w, h): (u32, u32),
        blur: u32,
        tint: u32,
        qh: &Qh,
    ) -> std::io::Result<WlBuffer> {
        let key = (w, h, blur, tint);
        let mut cache = self.blurred.borrow_mut();
        if let Some(i) = cache.iter().position(|(k, _)| *k == key) {
            let entry = cache.remove(i);
            let buffer = entry.1.clone();
            cache.insert(0, entry);
            return Ok(buffer);
        }
        let img = {
            let mut pool = self.pool.borrow_mut();
            let len = self.width as usize * self.height as usize * 4;
            let pixels = background::Pixels {
                width: self.width,
                height: self.height,
                data: &pool.mmap()[..len],
            };
            background::blurred(pixels, w, h, blur, tint)
        };
        let buffer = shm_buffer(g, img.width, img.height, &img.data, Ignore, qh)?;
        cache.insert(0, (key, buffer.clone()));
        // Only the newest one can be attached: one overlay at a time.
        if cache.len() > MAX_BLURRED
            && let Some((_, old)) = cache.pop()
        {
            old.destroy();
        }
        Ok(buffer)
    }
}

impl Drop for Wallpaper {
    fn drop(&mut self) {
        self.buffer.destroy();
        for (_, buffer) in self.blurred.get_mut() {
            buffer.destroy();
        }
    }
}

/// The output screenshot the card starts covered by.
struct Screenshot {
    id: CaptureId,
    capture: Capture,
    /// The frame, once captured: its buffer (owned by `capture`) and transform.
    frame: Option<(WlBuffer, Transform)>,
}

/// The opening transition, started at `start`.
#[derive(Clone, Copy)]
struct Opening {
    /// When the first frame was shown, from which it runs; None until then,
    /// as getting the first frame on screen can take long.
    start: Option<Instant>,
    /// The card starts as the screenshot at full output size and shrinks;
    /// otherwise only the thumbnails slide in.
    shrink: bool,
    /// The screenshot fades out as the thumbnails fade in, with
    /// wp_alpha_modifier_v1; otherwise they swap at once.
    crossfade: bool,
}

/// The share of `open-duration` the card takes to shrink into place, as the
/// screenshot over the blurred background; the thumbnails then slide in over
/// the rest of it, on the wallpaper.
const SHRINK_SHARE: f64 = 0.6;
/// Without a crossfade, the share of the shrink still left when the
/// screenshot disappears and the thumbnails start spreading: eased out, the
/// card has nearly arrived by then and finishes under them.
const SPREAD_OVERLAP: f64 = 0.7;
/// With a crossfade, the share of the shrink after which the screenshot
/// starts fading out to the wallpaper and the thumbnails start fading in and
/// spreading; the fade is done when the card arrives.
const FADE_START: f64 = 0.45;
/// How far toward the center of the content the thumbnails start, as a
/// share of their distance to it (1 = all start at the center), and how
/// small they start; they spread outward to their places.
const SPREAD_FROM_CENTER: f64 = 0.4;
const SLIDE_SCALE: f64 = 0.88;

/// Ease out (cubic), `t` clamped to 0..=1.
fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Ease in and out (smoothstep), `t` clamped to 0..=1.
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How far along an animation of `duration` is after `elapsed`, 0 to 1.
fn progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        return 1.0;
    }
    elapsed.as_secs_f64() / duration.as_secs_f64()
}

pub struct Overlay {
    // Field order is drop order: children before their parent surface.
    pub windows: Vec<Window>,
    pub selected: Option<usize>,
    /// The selected window's title; None with titles disabled.
    pill_sub: Option<Sub>,
    /// An empty subsurface right above every thumbnail: the selected one is
    /// raised to just below it.
    stack_top: Sub,
    /// The parent of every window's subsurfaces and the pill: a transparent
    /// pixel that hides them all until it is attached.
    content: Sub,
    /// The card: the wallpaper, or the card color without one.
    card: Sub,
    /// Over the card, the same size: the screenshot during the opening.
    card_shot: Sub,
    /// None with shadows disabled.
    card_shadow: Option<Shadow>,
    /// Until it has faded out; None without one.
    screenshot: Option<Screenshot>,
    viewport: WpViewport,
    /// The main surface's alpha modifier, while closing.
    main_alpha: Option<WpAlphaModifierSurfaceV1>,
    layer: LayerSurface,
    /// Opaque; the main surface's buffer without a wallpaper.
    background: WlBuffer,
    wallpaper: Option<Rc<Wallpaper>>,
    /// The card without a wallpaper.
    card_color: WlBuffer,
    /// Attached to `content` to show it.
    transparent: WlBuffer,
    placeholder: WlBuffer,
    /// Icon buffers, shared by the windows of an app.
    icon_buffers: HashMap<icons::Key, WlBuffer>,
    /// Replaced icon buffers, possibly still attached.
    stale_buffers: Vec<WlBuffer>,
    /// The pill attached to `pill_sub`.
    pill: Option<ShownPill>,
    /// The pill requested from the worker, until it arrives.
    pill_wanted: Option<(CaptureId, PillRequest)>,
    assets: Rc<Assets>,
    g: Rc<Globals>,
    qh: Qh,
    style: Style,

    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    mapped: bool,
    /// The first-frame timeout expired: map even without all frames.
    deadline_passed: bool,
    /// With WL_EXPOSE_TRACE set: when the overlay was created.
    trace: Option<Instant>,
    /// A frame callback for an animation is pending.
    frame_pending: bool,
    /// The card's final rect; the layout is inside it unless the margin is
    /// negative.
    card_rect: IRect,
    /// Where thumbnails and pills may go: the card, grown by a negative
    /// margin, within the output.
    content_rect: IRect,
    /// `content` is attached.
    content_shown: bool,
    /// The running opening transition, if any.
    opening: Option<Opening>,
    /// The card's way from the full output (0) to `card_rect` (1).
    shrink: f64,
    /// The thumbnails' way out from near the center (0) to their place (1).
    slide: f64,
    /// The thumbnails' opacity in the opening crossfade, 0 to 1; the
    /// screenshot's is the rest.
    fade: f64,
    /// When the closing fade-out started, once closing.
    closing: Option<Instant>,
    /// Everything's opacity in the closing fade-out, 1 to 0.
    close_alpha: f64,
}

impl Overlay {
    /// Create the overlay for `toplevels`, in layout order, on `output`
    /// (None: the compositor's choice), and start capturing them and, for
    /// the opening transition, the output. Icons and titles come from
    /// `assets`.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        g: Rc<Globals>,
        toplevels: Vec<(ExtForeignToplevelHandleV1, Toplevel)>,
        output: Option<&WlOutput>,
        req: &Request,
        config: &Config,
        wallpaper: Option<Rc<Wallpaper>>,
        assets: Rc<Assets>,
        qh: &Qh,
    ) -> std::io::Result<Self> {
        let theme = config.theme();
        let background = color_buffer(&g, over_black(theme.background), qh)?;
        let card_color = color_buffer(&g, theme.card_color, qh)?;
        let transparent = color_buffer(&g, 0, qh)?;
        let placeholder = color_buffer(&g, theme.placeholder, qh)?;
        // Copied by set_input_region, so it is destroyed at the end of new().
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;

        let surface = g.compositor.create_surface(qh);
        let viewport = g.viewporter.get_viewport(&surface, qh, Ignore);
        // Output None: the compositor picks the focused output.
        let layer = g.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("wl-expose"),
            output,
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_size(0, 0);
        layer.set_exclusive_zone(-1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);

        // Stacking is creation order: the card's shadow, the card, the
        // screenshot over it, then everything else in `content`.
        let shadow = config.shadow();
        let card_shadow = shadow
            .is_some()
            .then(|| Shadow::new(&g, layer.wl_surface(), &no_input, qh));
        let card = Sub::new(&g, layer.wl_surface(), &no_input, qh);
        let card_shot = Sub::new(&g, layer.wl_surface(), &no_input, qh);
        let content = Sub::new(&g, layer.wl_surface(), &no_input, qh);
        content.place(IRect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        });
        let parent = &content.surface;

        // Every shadow below every thumbnail.
        let shadows: Vec<Option<Shadow>> = toplevels
            .iter()
            .map(|_| {
                shadow
                    .is_some()
                    .then(|| Shadow::new(&g, parent, &no_input, qh))
            })
            .collect();

        let mut windows: Vec<Window> = toplevels
            .into_iter()
            .zip(shadows)
            .map(|((handle, toplevel), shadow)| {
                // Unique across overlays, so late events for a previous
                // overlay's captures never reach this one's.
                let id = CaptureId::next();
                Window {
                    id,
                    capture: Capture::window(id, &handle, &g, req.live, req.cursors, qh),
                    sub: Sub::new(&g, parent, &no_input, qh),
                    shadow,
                    icon: None,
                    handle,
                    identifier: toplevel.identifier,
                    app_id: toplevel.app_id,
                    title: toplevel.title,
                    rect: IRect::default(),
                    shown: None,
                    zoom: 1.0,
                    anim: None,
                }
            })
            .collect();
        let stack_top = Sub::new(&g, parent, &no_input, qh);
        // Above every thumbnail, then the pill above everything.
        if config.icon_size > 0 {
            for win in &mut windows {
                win.icon = Some(Icon {
                    sub: Sub::new(&g, parent, &no_input, qh),
                    rect: IRect::default(),
                    attached: None,
                });
            }
        }
        let title = config.title_style();
        let pill_sub = title.is_some().then(|| Sub::new(&g, parent, &no_input, qh));

        let screenshot = output
            .filter(|_| config.open_duration > 0)
            .and_then(|output| {
                let id = CaptureId::next();
                let capture = Capture::output(id, output, &g, qh)?;
                Some(Screenshot {
                    id,
                    capture,
                    frame: None,
                })
            });

        // Initial commit without a buffer; the map happens once frames are in.
        layer.commit();

        Ok(Self {
            windows,
            selected: None,
            pill_sub,
            stack_top,
            content,
            card,
            card_shot,
            card_shadow,
            screenshot,
            viewport,
            main_alpha: None,
            layer,
            background,
            wallpaper,
            card_color,
            transparent,
            placeholder,
            icon_buffers: HashMap::new(),
            stale_buffers: Vec::new(),
            pill: None,
            pill_wanted: None,
            assets,
            g,
            qh: qh.clone(),
            style: Style {
                gap: config.gap as f64,
                margin: config.margin as f64,
                zoom: config.zoom,
                zoom_duration: Duration::from_millis(config.zoom_duration.into()),
                max_scale: config.max_scale,
                shadow,
                icon_size: config.icon_size as i32,
                icon_theme: theme.icon_theme.clone(),
                icon_shadow: config.icon_shadow(),
                font: config.font.clone(),
                title,
                open_duration: Duration::from_millis(config.open_duration.into()),
                close_duration: Duration::from_millis(config.close_duration.into()),
                card_scale: config.card_scale,
                background: theme.background,
                background_blur: theme.background_blur,
            },
            width: 0,
            height: 0,
            scale: 1,
            configured: false,
            mapped: false,
            deadline_passed: false,
            trace: std::env::var_os("WL_EXPOSE_TRACE").map(|_| Instant::now()),
            frame_pending: false,
            card_rect: IRect::default(),
            content_rect: IRect::default(),
            content_shown: false,
            opening: None,
            shrink: 1.0,
            slide: 1.0,
            fade: 1.0,
            closing: None,
            close_alpha: 1.0,
        })
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn mapped(&self) -> bool {
        self.mapped
    }

    pub fn index_of(&self, id: CaptureId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Subsurface positions and stacking are applied on the parent commit:
    /// `content` for what is in it, the main surface for the card and
    /// `content`. Before mapping, the map commit takes care of that.
    fn commit_parent(&self) {
        if self.mapped {
            self.content.surface.commit();
            self.layer.commit();
        }
    }

    pub fn configure(&mut self, width: u32, height: u32) {
        let changed = !self.configured || (width, height) != (self.width, self.height);
        if let Some(t0) = self.trace
            && !self.configured
        {
            trace(t0, format_args!("configured {width}x{height}"));
        }
        self.configured = true;
        self.width = width;
        self.height = height;
        if width > 0 && height > 0 {
            self.viewport.set_destination(width as i32, height as i32);
        }
        if !self.mapped {
            return;
        }
        if changed {
            self.attach_background();
            self.attach_card();
            self.relayout();
        } else {
            self.layer.commit();
        }
    }

    /// Attach the main surface's buffer: the blurred wallpaper with the
    /// background color over it, or the opaque background color.
    fn attach_background(&self) {
        let blurred = (self.wallpaper.as_ref())
            .filter(|_| self.width > 0 && self.height > 0)
            .and_then(|wp| {
                // In buffer pixels, so an unblurred one stays sharp.
                let s = self.scale.max(1) as u32;
                let size = (self.width * s, self.height * s);
                let (blur, tint) = (self.style.background_blur * s, self.style.background);
                wp.blurred(&self.g, size, blur, tint, &self.qh)
                    .inspect_err(|e| warn!("background buffer: {e}"))
                    .ok()
            });
        let surface = self.layer.wl_surface();
        surface.attach(Some(blurred.as_ref().unwrap_or(&self.background)), 0, 0);
        surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
    }

    /// Attach the card's buffer: the wallpaper, cropped like the output, or
    /// the card color. Committed by `place_card`.
    fn attach_card(&self) {
        let card = &self.card;
        match &self.wallpaper {
            Some(wp) if self.width > 0 && self.height > 0 => {
                // Cropped like the output, which the card's shape is.
                let (x, y, w, h) = wp.cover(self.width, self.height);
                card.viewport.set_source(x, y, w, h);
                card.attach(&wp.buffer);
            }
            _ => {
                card.viewport.set_source(-1.0, -1.0, -1.0, -1.0);
                card.attach(&self.card_color);
            }
        }
    }

    /// Attach the screenshot over the card. Committed by `place_card`.
    fn attach_shot(&self) {
        if let Some((buffer, transform)) = self.screenshot.as_ref().and_then(|s| s.frame.as_ref()) {
            self.card_shot.surface.set_buffer_transform(*transform);
            self.card_shot.attach(buffer);
        }
    }

    /// Every subsurface in `content` that shows something.
    fn content_subs(&mut self) -> impl Iterator<Item = &mut Sub> {
        let windows = self.windows.iter_mut().flat_map(Window::subs);
        windows.chain(self.pill_sub.iter_mut())
    }

    /// Every subsurface that shows something: the card, its screenshot and
    /// shadow, and what is in `content`.
    fn all_subs(&mut self) -> impl Iterator<Item = &mut Sub> {
        let card = [&mut self.card, &mut self.card_shot].into_iter().chain(
            self.card_shadow
                .iter_mut()
                .flat_map(|s| s.strips.iter_mut()),
        );
        let windows = self.windows.iter_mut().flat_map(Window::subs);
        card.chain(windows).chain(self.pill_sub.iter_mut())
    }

    /// Start the crossfade from the screenshot to the thumbnails: false
    /// without wp_alpha_modifier_v1.
    fn start_crossfade(&mut self) -> bool {
        let g = self.g.clone();
        let qh = self.qh.clone();
        if !self.card_shot.start_fade(&g, 1.0, &qh) {
            return false;
        }
        for sub in self.content_subs() {
            sub.start_fade(&g, 0.0, &qh);
        }
        self.fade = 0.0;
        true
    }

    /// Crossfade `t` of the way (0 to 1) from the screenshot to the
    /// wallpaper and the thumbnails, or without a crossfade, swap at 1. The
    /// screenshot is committed by `place_card`, the windows by
    /// `place_window`.
    fn crossfade(&mut self, t: f64) {
        if self.screenshot.is_none() {
            return;
        }
        if t >= 1.0 {
            self.fade = 1.0;
            self.release_shot();
            // While closing, they keep fading.
            if self.closing.is_none() {
                for sub in self.content_subs() {
                    sub.end_fade();
                }
            }
        } else {
            self.fade = t;
        }
        self.apply_alpha();
        if let Some(sub) = &self.pill_sub {
            sub.surface.commit();
        }
    }

    /// Set the opacity of every fading surface from the opening crossfade
    /// and the closing fade-out. Committing is the caller's.
    fn apply_alpha(&mut self) {
        let (fade, c) = (self.fade, self.close_alpha);
        if let Some(alpha) = &self.main_alpha {
            alpha.set_multiplier((c.clamp(0.0, 1.0) * u32::MAX as f64).round() as u32);
        }
        self.card.set_alpha(c);
        self.card_shot.set_alpha((1.0 - fade) * c);
        for sub in self.card_shadow.iter().flat_map(|s| &s.strips) {
            sub.set_alpha(c);
        }
        for sub in self.content_subs() {
            sub.set_alpha(fade * c);
        }
    }

    /// Start fading out, after giving up the keyboard and letting input
    /// through to what is below. False if it can't (not mapped, no
    /// wp_alpha_modifier_v1, or no fade-out configured): the overlay should
    /// go right away then. Otherwise it runs on frame callbacks until
    /// `closed`.
    pub fn start_closing(&mut self) -> bool {
        if self.closing.is_some() {
            return true;
        }
        let Some(manager) = self.g.alpha_modifier.clone() else {
            return false;
        };
        if !self.mapped || self.style.close_duration.is_zero() {
            return false;
        }
        self.closing = Some(Instant::now());
        if let Some(t0) = self.trace {
            trace(t0, format_args!("closing starts"));
        }
        self.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        // Copied by set_input_region, so it can go right away.
        match Region::new(&self.g.compositor) {
            Ok(none) => self.surface().set_input_region(Some(none.wl_region())),
            Err(e) => warn!("input region: {e}"),
        }
        let surface = self.layer.wl_surface();
        self.main_alpha = Some(manager.get_surface(surface, &self.qh, Ignore));
        let g = self.g.clone();
        let qh = self.qh.clone();
        for sub in self.all_subs() {
            sub.start_fade(&g, 1.0, &qh); // corrected by apply_alpha
        }
        self.animate(); // applies the alphas, requests a frame
        self.commit_parent();
        true
    }

    /// Advance the closing fade-out to `now` and commit the subsurfaces;
    /// the main surface is the caller's. Returns whether it is still running.
    fn animate_closing(&mut self, now: Instant) -> bool {
        let Some(start) = self.closing else {
            return false;
        };
        let t = progress(
            now.saturating_duration_since(start),
            self.style.close_duration,
        );
        self.close_alpha = 1.0 - smoothstep(t);
        self.apply_alpha();
        for sub in self.all_subs() {
            sub.surface.commit();
        }
        if t >= 1.0
            && let Some(t0) = self.trace
        {
            trace(t0, format_args!("closing done"));
        }
        t < 1.0
    }

    /// The closing fade-out is done: the overlay can go.
    pub fn closed(&self) -> bool {
        self.closing.is_some() && self.close_alpha <= 0.0
    }

    /// Hide the screenshot and release it.
    fn release_shot(&mut self) {
        self.card_shot.surface.attach(None, 0, 0);
        self.card_shot.end_fade();
        self.card_shot.surface.commit();
        // Attached buffers may be destroyed; the surface keeps its contents
        // until the detach applies.
        self.screenshot = None;
        if let Some(t0) = self.trace {
            trace(
                t0,
                format_args!("screenshot gone, card shows the wallpaper"),
            );
        }
    }

    /// Show the windows, icons and pill.
    fn show_content(&mut self) {
        if !self.content_shown {
            self.content_shown = true;
            self.content.attach(&self.transparent);
            self.content.surface.commit();
        }
    }

    /// The card at its current point of the shrink, and its shadow.
    fn card_shown_rect(&self) -> IRect {
        let f = self.card_rect;
        let t = self.shrink;
        let lerp = |a: i32, b: i32| (a as f64 + (b - a) as f64 * t).round() as i32;
        let (x0, y0) = (lerp(0, f.x), lerp(0, f.y));
        let (x1, y1) = (
            lerp(self.width as i32, f.x + f.w),
            lerp(self.height as i32, f.y + f.h),
        );
        IRect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(1),
            h: (y1 - y0).max(1),
        }
    }

    /// Place the card and its shadow where they are shown, and commit them.
    fn place_card(&self) {
        if self.card_rect.w <= 0 || self.card_rect.h <= 0 {
            return;
        }
        let shown = self.card_shown_rect();
        self.card.place(shown);
        self.card.surface.commit();
        self.card_shot.place(shown);
        self.card_shot.surface.commit();
        if let Some(sh) = &self.card_shadow {
            sh.place(self.card_rect, shown);
        }
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        if self.mapped {
            self.attach_background();
            self.relayout();
        }
    }

    pub fn relayout(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            self.commit_parent();
            return;
        }
        // The output scaled around its center, so the screenshot keeps its
        // aspect ratio.
        let (width, height) = (self.width as f64, self.height as f64);
        let s = self.style.card_scale;
        let (cw, ch) = ((width * s).round().max(1.0), (height * s).round().max(1.0));
        self.card_rect = IRect {
            x: ((width - cw) / 2.0).round() as i32,
            y: ((height - ch) / 2.0).round() as i32,
            w: cw as i32,
            h: ch as i32,
        };
        if let (Some(sh), Some(settings)) = (&mut self.card_shadow, &self.style.shadow) {
            let size = (self.card_rect.w, self.card_rect.h);
            sh.render(&self.g, settings, size, self.scale, &self.qh);
        }
        self.place_card();
        // A negative margin grows the layout area past the card, but never
        // off the output.
        let c = self.card_rect;
        let card = layout::Rect::new(c.x as f64, c.y as f64, c.w as f64, c.h as f64);
        let m = self.style.margin;
        let (x0, y0) = ((card.x + m).max(0.0), (card.y + m).max(0.0));
        let x1 = (card.x + card.w - m).min(width);
        let y1 = (card.y + card.h - m).min(height);
        let mut area = layout::Rect::new(x0, y0, x1 - x0, y1 - y0);
        if area.w <= 0.0 || area.h <= 0.0 {
            area = card;
        }
        self.content_rect = if m < 0.0 {
            IRect {
                x: area.x.round() as i32,
                y: area.y.round() as i32,
                w: area.w.round() as i32,
                h: area.h.round() as i32,
            }
        } else {
            c
        };
        if self.windows.is_empty() {
            self.commit_parent();
            return;
        }

        // Lay out in a canonical order, not the order the windows were passed
        // in (which may be e.g. most recently used), so the same windows always
        // get the same placement. The window order itself (selection, cycling)
        // is left alone.
        let mut order: Vec<usize> = (0..self.windows.len()).collect();
        order.sort_by(|&a, &b| {
            let (a, b) = (&self.windows[a], &self.windows[b]);
            (&a.identifier, &a.app_id, &a.title).cmp(&(&b.identifier, &b.app_id, &b.title))
        });
        let items: Vec<layout::Item> = order
            .iter()
            .map(|&i| self.windows[i].size(self.scale))
            .collect();
        let params = layout::Params {
            spacing: self.style.gap,
            max_scale: self.style.max_scale,
            below: self.style.below(),
        };
        let rects = layout::compute(&items, area, &params);

        for (&i, r) in order.iter().zip(rects) {
            let win = &mut self.windows[i];
            // Round edges rather than sizes so neighbors never overlap.
            let (x0, y0) = (r.x.round() as i32, r.y.round() as i32);
            let (x1, y1) = ((r.x + r.w).round() as i32, (r.y + r.h).round() as i32);
            win.rect = IRect {
                x: x0,
                y: y0,
                w: (x1 - x0).max(1),
                h: (y1 - y0).max(1),
            };
            if win.shown.is_none() {
                win.sub.attach(&self.placeholder);
            }
            if let (Some(sh), Some(settings)) = (&mut win.shadow, &self.style.shadow) {
                let size = (win.rect.w, win.rect.h);
                sh.render(&self.g, settings, size, self.scale, &self.qh);
            }
        }
        for i in 0..self.windows.len() {
            self.place_window(i); // commits the thumbnail, shadow and icon
        }
        self.update_icons(); // commits every icon
        self.update_selection(); // commits the parent
    }

    /// Where `win` ends up: its layout rect zoomed around its center, kept
    /// inside the content rect. Input hit-tests this, also during the opening.
    fn zoomed_rect(&self, win: &Window) -> IRect {
        let r = win.rect;
        if win.zoom == 1.0 {
            return r;
        }
        let w = (r.w as f64 * win.zoom).round().max(1.0) as i32;
        let h = (r.h as f64 * win.zoom).round().max(1.0) as i32;
        let fit = |pos: i32, size: i32, min: i32, max: i32| match max - size {
            room if room >= min => pos.clamp(min, room),
            _ => pos,
        };
        let c = self.content_rect;
        IRect {
            x: fit(r.x - (w - r.w) / 2, w, c.x, c.x + c.w),
            y: fit(r.y - (h - r.h) / 2, h, c.y, c.y + c.h),
            w,
            h,
        }
    }

    /// Where `win` is shown: zoomed, and during the opening, smaller and
    /// pulled toward the center on its way out to its place.
    fn shown_rect(&self, win: &Window) -> IRect {
        let r = self.zoomed_rect(win);
        if self.slide >= 1.0 {
            return r;
        }
        let left = 1.0 - self.slide;
        let s = 1.0 - (1.0 - SLIDE_SCALE) * left;
        let w = (r.w as f64 * s).round().max(1.0) as i32;
        let h = (r.h as f64 * s).round().max(1.0) as i32;
        // Offset of the window's center from the content's center, pulled
        // in by the share still left to go.
        let c = self.content_rect;
        let (cx, cy) = (c.x as f64 + c.w as f64 / 2.0, c.y as f64 + c.h as f64 / 2.0);
        let (rx, ry) = (r.x as f64 + r.w as f64 / 2.0, r.y as f64 + r.h as f64 / 2.0);
        let pull = SPREAD_FROM_CENTER * left;
        let (x, y) = (rx + (cx - rx) * pull, ry + (cy - ry) * pull);
        IRect {
            x: (x - w as f64 / 2.0).round() as i32,
            y: (y - h as f64 / 2.0).round() as i32,
            w,
            h,
        }
    }

    /// The icon of a thumbnail shown at `shown`: centered on its bottom edge.
    fn icon_rect(&self, shown: IRect) -> IRect {
        let s = self.style.icon_size;
        IRect {
            x: shown.x + (shown.w - s) / 2,
            y: shown.y + shown.h - s / 2,
            w: s,
            h: s,
        }
    }

    /// Place window `i`'s thumbnail, shadow and icon where it is shown, and
    /// commit them.
    fn place_window(&mut self, i: usize) {
        let shown = self.shown_rect(&self.windows[i]);
        let icon_rect = self.icon_rect(shown);
        // The icon's image reaches past it by its shadow.
        let pad = self.style.icon_shadow.map_or(0, |s| s.pad());
        let win = &mut self.windows[i];
        win.sub.place(shown);
        win.sub.surface.commit();
        if let Some(sh) = &win.shadow {
            sh.place(win.rect, shown);
        }
        if let Some(icon) = &mut win.icon {
            icon.rect = icon_rect;
            icon.sub.place(IRect {
                x: icon_rect.x - pad,
                y: icon_rect.y - pad,
                w: icon_rect.w + 2 * pad,
                h: icon_rect.h + 2 * pad,
            });
            icon.sub.surface.commit();
        }
    }

    /// The zoom window `i` heads for.
    fn zoom_target(&self, i: usize) -> f64 {
        if self.selected == Some(i) {
            self.style.zoom
        } else {
            1.0
        }
    }

    /// Advance the opening transition to `now`: shrink the card, and on the
    /// way crossfade from the screenshot on it to the wallpaper and the
    /// windows, which spread out while the card settles. Returns whether it
    /// is still running.
    fn animate_opening(&mut self, now: Instant) -> bool {
        let Some(o) = self.opening else {
            return false;
        };
        let elapsed = o
            .start
            .map_or(Duration::ZERO, |s| now.saturating_duration_since(s));
        let duration = self.style.open_duration;
        let shrink_time = if o.shrink {
            duration.mul_f64(SHRINK_SHARE)
        } else {
            Duration::ZERO
        };
        let card = progress(elapsed, shrink_time);
        self.shrink = ease_out(card);
        let spread_start = if o.crossfade {
            shrink_time.mul_f64(FADE_START)
        } else {
            shrink_time.mul_f64(1.0 - SPREAD_OVERLAP)
        };
        let started = elapsed >= spread_start;
        if started {
            // The windows start fading in (or appear) and moving.
            self.show_content();
        }
        let fade = if o.crossfade {
            let t = progress(
                elapsed.saturating_sub(spread_start),
                shrink_time.saturating_sub(spread_start),
            );
            smoothstep(t)
        } else if started {
            1.0
        } else {
            0.0
        };
        self.crossfade(fade);
        let slide_time = duration.saturating_sub(spread_start);
        let slide = progress(elapsed.saturating_sub(spread_start), slide_time);
        self.slide = if started { ease_out(slide) } else { 0.0 };
        self.place_card();
        if let Some(t0) = self.trace {
            let ms = elapsed.as_secs_f64() * 1000.0;
            let r = self.card_shown_rect();
            trace(
                t0,
                format_args!(
                    "opening frame {ms:6.1} ms shrink {:.3} slide {:.3} card {}x{}+{}+{}",
                    self.shrink, self.slide, r.w, r.h, r.x, r.y
                ),
            );
        }
        let done = started && card >= 1.0 && slide >= 1.0;
        if done {
            self.opening = None;
            if let Some(t0) = self.trace {
                trace(t0, format_args!("opening done"));
            }
        }
        !done
    }

    /// Advance the opening and start zoom animations toward the current
    /// targets, advance them to now and place the windows that moved, then
    /// advance the closing fade-out. Until mapped, zooms jump to their
    /// targets. The parent commit is the caller's.
    fn animate(&mut self) {
        let now = Instant::now();
        let opening = self.opening.is_some();
        let mut running = self.animate_opening(now);
        let duration = self.style.zoom_duration;
        let can_animate = self.mapped && !duration.is_zero();
        for i in 0..self.windows.len() {
            let target = self.zoom_target(i);
            let win = &mut self.windows[i];
            let before = win.zoom;
            match win.anim {
                Some(a) if a.to == target => {}
                _ if win.zoom == target => win.anim = None,
                _ if can_animate => {
                    win.anim = Some(ZoomAnim {
                        from: win.zoom,
                        to: target,
                        start: now,
                    });
                }
                _ => {
                    win.anim = None;
                    win.zoom = target;
                }
            }
            if let Some(a) = win.anim {
                match a.at(now, duration) {
                    Some(z) => {
                        win.zoom = z;
                        running = true;
                    }
                    None => {
                        win.zoom = a.to;
                        win.anim = None;
                    }
                }
            }
            if win.zoom != before || opening {
                self.place_window(i);
            }
        }
        running |= self.animate_closing(now);
        if running && self.mapped && !self.frame_pending {
            let surface = self.layer.wl_surface();
            surface.frame(&self.qh, FrameCallbackData(surface.clone()));
            self.frame_pending = true;
        }
    }

    /// The frame callback of the animations: draw their next step. The first
    /// one after mapping starts the opening.
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        if let Some(o) = &mut self.opening
            && o.start.is_none()
        {
            o.start = Some(Instant::now());
            if let Some(t0) = self.trace {
                trace(t0, format_args!("first frame shown, opening starts"));
            }
        }
        self.animate();
        self.place_pill();
        self.commit_parent();
    }

    /// The icon key of `app_id` at the current scale.
    fn icon_key(&self, app_id: &str) -> icons::Key {
        icons::Key::new(
            app_id,
            self.style.icon_size as u32,
            self.style.icon_theme.clone(),
            self.style.icon_shadow,
            self.scale,
        )
    }

    /// The icon buffer for `key`, made from the received image. None if the
    /// image isn't there yet; it is requested then.
    fn icon_buffer(&mut self, key: &icons::Key) -> Option<WlBuffer> {
        if let Some(buffer) = self.icon_buffers.get(key) {
            return Some(buffer.clone());
        }
        let img = self.assets.icon(key, &self.style.font)?;
        match shm_buffer(&self.g, img.width, img.height, &img.data, Ignore, &self.qh) {
            Ok(buffer) => {
                self.icon_buffers.insert(key.clone(), buffer.clone());
                Some(buffer)
            }
            Err(e) => {
                warn!("icon buffer: {e}");
                None
            }
        }
    }

    /// Attach every window's current icon that is available, request the
    /// others, and commit the icons.
    fn update_icons(&mut self) {
        for i in 0..self.windows.len() {
            let Some(icon) = &self.windows[i].icon else {
                continue;
            };
            let key = self.icon_key(&self.windows[i].app_id);
            if icon.attached.as_ref() != Some(&key)
                && let Some(buffer) = self.icon_buffer(&key)
                && let Some(icon) = &mut self.windows[i].icon
            {
                icon.sub.attach(&buffer);
                icon.attached = Some(key);
            }
            if let Some(icon) = &self.windows[i].icon {
                icon.sub.surface.commit();
            }
        }
    }

    /// An icon arrived from the worker.
    pub fn icon_ready(&mut self, key: &icons::Key) {
        let wanted = self
            .windows
            .iter()
            .any(|w| w.icon.is_some() && self.icon_key(&w.app_id) == *key);
        // Before mapping, the map's relayout attaches it.
        if wanted && self.mapped {
            self.update_icons();
        }
    }

    /// The desktop entries changed: get every icon again.
    pub fn icons_reloaded(&mut self) {
        // Attached buffers may be in use; keep them until the overlay goes.
        self.stale_buffers
            .extend(self.icon_buffers.drain().map(|(_, buffer)| buffer));
        for icon in self.windows.iter_mut().filter_map(|w| w.icon.as_mut()) {
            icon.attached = None;
        }
        if self.mapped {
            self.update_icons();
        }
    }

    /// The title or app_id of a shown window changed.
    pub fn update_toplevel(&mut self, handle: &ExtForeignToplevelHandleV1, toplevel: &Toplevel) {
        let Some(i) = self.windows.iter().position(|w| w.handle == *handle) else {
            return;
        };
        let win = &mut self.windows[i];
        let app_changed = win.app_id != toplevel.app_id;
        let title_changed = win.title != toplevel.title;
        win.app_id.clone_from(&toplevel.app_id);
        win.title.clone_from(&toplevel.title);
        if app_changed && self.mapped {
            self.update_icons();
        }
        if title_changed && self.selected == Some(i) {
            self.update_pill();
            self.commit_parent();
        }
    }

    /// Run `f` on a window's (or the screenshot's) capture and apply what
    /// it reports.
    pub fn capture_update(
        &mut self,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        if self.screenshot.as_ref().is_some_and(|s| s.id == id) {
            return self.screenshot_update(f);
        }
        let Some(i) = self.index_of(id) else { return };
        let had_size = self.windows[i].capture.size().is_some();
        let had_frame = self.windows[i].shown.is_some();
        let event = f(&mut self.windows[i].capture, &self.g);
        if let Some(t0) = self.trace {
            let win = &self.windows[i];
            if !had_size && let Some((w, h)) = win.capture.size() {
                trace(t0, format_args!("'{}': size {w}x{h}", win.identifier));
            }
            if !had_frame && matches!(event, Some(CaptureEvent::Frame(_))) {
                let kind = win.capture.buffer_kind().unwrap_or("?");
                trace(
                    t0,
                    format_args!("'{}': first frame ({kind})", win.identifier),
                );
            }
        }
        match event {
            Some(CaptureEvent::Frame(frame)) => self.show_frame(i, frame),
            Some(CaptureEvent::Failed(why)) => {
                let win = &self.windows[i];
                warn!("capture of '{}' failed: {why}", win.identifier);
                // Without configure, the placeholder is attached on the next relayout.
                if win.shown.is_none() && self.configured {
                    win.sub.attach(&self.placeholder);
                    win.sub.surface.commit();
                }
            }
            None => {}
        }
    }

    fn screenshot_update(
        &mut self,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        let Some(shot) = &mut self.screenshot else {
            return;
        };
        match f(&mut shot.capture, &self.g) {
            // Static, so the only one; it is shown when the overlay maps.
            Some(CaptureEvent::Frame(frame)) => {
                if let Some(t0) = self.trace {
                    let kind = shot.capture.buffer_kind().unwrap_or("?");
                    let (w, h) = (frame.width, frame.height);
                    trace(t0, format_args!("screenshot {w}x{h} ({kind})"));
                }
                shot.frame = Some((frame.buffer, frame.transform));
            }
            Some(CaptureEvent::Failed(why)) => {
                warn!("output capture failed: {why}");
                self.screenshot = None; // maybe_map goes on without it
            }
            None => {}
        }
    }
    fn show_frame(&mut self, i: usize, frame: Frame) {
        let scale = self.scale;
        let win = &mut self.windows[i];
        // The layout used the announced capture size until now; only a
        // different size (or a rotation) moves anything.
        let laid_out = win.size(scale);
        win.shown = Some(Shown {
            width: frame.width,
            height: frame.height,
            transform: frame.transform,
        });
        let resized = win.size(scale) != laid_out;
        win.sub.surface.set_buffer_transform(frame.transform);
        win.sub.attach(&frame.buffer);
        if resized && self.mapped {
            self.relayout(); // commits every thumbnail
        } else {
            win.sub.surface.commit();
        }
    }

    /// Raise and zoom the selection, and move the title pill to it.
    pub fn update_selection(&mut self) {
        if let Some(win) = self.selected.and_then(|i| self.windows.get(i)) {
            win.sub.subsurface.place_below(&self.stack_top.surface);
        }
        self.animate();
        self.update_pill();
        self.commit_parent();
    }

    /// The selected window, once laid out.
    fn selected_window(&self) -> Option<&Window> {
        let win = self.windows.get(self.selected?)?;
        (win.rect.w > 0 && win.rect.h > 0).then_some(win)
    }

    /// Show the selected window's title: keep, move or replace the pill, or
    /// request a new one from the worker. The parent commit is the caller's.
    fn update_pill(&mut self) {
        let (Some(sub), Some(style)) = (&self.pill_sub, &self.style.title) else {
            return;
        };
        let wanted = self
            .selected_window()
            .filter(|w| !w.title.trim().is_empty())
            .map(|w| {
                let max_width = w.rect.w.max(text::MIN_PILL_WIDTH).min(self.content_rect.w);
                let request = PillRequest {
                    text: w.title.clone(),
                    max_width,
                    scale: self.scale,
                    style: style.clone(),
                };
                (w.id, request)
            });
        // A pill of another window goes right away; an outdated one of the
        // same window stays until its replacement arrives.
        let keep = matches!((&self.pill, &wanted), (Some(p), Some((id, _))) if p.window == *id);
        if !keep && let Some(old) = self.pill.take() {
            retire(&old.buffer);
            sub.surface.attach(None, 0, 0);
            sub.surface.commit();
        }
        let current = self.pill.as_ref().map(|p| (p.window, &p.request));
        match wanted {
            Some((id, request)) if current != Some((id, &request)) => {
                if self.pill_wanted.as_ref() != Some(&(id, request.clone())) {
                    self.assets.pill(id, request.clone());
                    self.pill_wanted = Some((id, request));
                }
            }
            _ => self.pill_wanted = None,
        }
        self.place_pill();
    }

    /// Put the pill below its window's icon, centered and inside the content
    /// rect.
    fn place_pill(&self) {
        let (Some(sub), Some(pill)) = (&self.pill_sub, &self.pill) else {
            return;
        };
        let Some(win) = self.windows.iter().find(|w| w.id == pill.window) else {
            return;
        };
        let r = self.shown_rect(win);
        let c = self.content_rect;
        let max_x = (c.x + c.w - pill.width).max(c.x);
        sub.place(IRect {
            x: (r.x + (r.w - pill.width) / 2).clamp(c.x, max_x),
            y: r.y + r.h + self.style.pill_offset(),
            w: pill.width,
            h: pill.height,
        });
        sub.surface.commit();
    }

    /// A pill arrived from the worker, tagged with its window's capture ID.
    pub fn pill_ready(&mut self, tag: u64, request: PillRequest, pill: text::Pill) {
        let window = match &self.pill_wanted {
            Some((id, wanted)) if u64::from(id.raw()) == tag && *wanted == request => *id,
            _ => return, // the selection or title changed meanwhile
        };
        self.pill_wanted = None;
        let Some(sub) = &self.pill_sub else { return };
        let img = &pill.image;
        let buffer = match shm_buffer(
            &self.g,
            img.width,
            img.height,
            &img.data,
            OnceBuffer::default(),
            &self.qh,
        ) {
            Ok(buffer) => buffer,
            Err(e) => {
                warn!("title buffer: {e}");
                return;
            }
        };
        sub.attach(&buffer);
        if let Some(old) = self.pill.take() {
            retire(&old.buffer);
        }
        self.pill = Some(ShownPill {
            window,
            request,
            buffer,
            width: pill.width,
            height: pill.height,
        });
        self.place_pill();
        self.commit_parent();
    }

    /// The first-frame timeout expired.
    pub fn deadline_passed(&mut self) {
        self.deadline_passed = true;
        self.maybe_map();
    }

    /// Map once configured, every capture knows its size (frames fill in
    /// as they arrive) and the screenshot is in, or the deadline passed.
    pub fn maybe_map(&mut self) {
        if self.mapped || !self.configured {
            return;
        }
        let shot_ready = self.screenshot.as_ref().is_none_or(|s| s.frame.is_some());
        if !self.deadline_passed && !(shot_ready && self.windows.iter().all(|w| w.capture.sized()))
        {
            return;
        }
        self.mapped = true;
        // Without a screenshot, the card starts as the wallpaper in place.
        let shrink = self.screenshot.as_ref().is_some_and(|s| s.frame.is_some());
        if !shrink {
            self.screenshot = None;
        }
        if let Some(t0) = self.trace {
            let why = if self.deadline_passed {
                "deadline"
            } else {
                "all sized"
            };
            let from = if shrink { "screenshot" } else { "wallpaper" };
            trace(t0, format_args!("mapped ({why}), card from the {from}"));
        }
        self.attach_background();
        let animated = !self.style.open_duration.is_zero();
        // The thumbnails start transparent, to fade in.
        let crossfade = shrink && animated && self.start_crossfade();
        self.opening = animated.then_some(Opening {
            start: None,
            shrink,
            crossfade,
        });
        if shrink {
            // The screen looks unchanged: the screenshot covers the output.
            (self.shrink, self.slide) = (0.0, 0.0);
        } else {
            self.shrink = 1.0;
            self.slide = if animated { 0.0 } else { 1.0 };
            self.show_content();
        }
        self.attach_card();
        self.attach_shot();
        self.relayout(); // commits the parent, mapping it
    }

    /// The window whose thumbnail or icon is at (`x`, `y`). Windows are
    /// where they end up, also while they move in.
    pub fn hit(&self, x: f64, y: f64) -> Option<usize> {
        if !self.mapped {
            return None;
        }
        // Icons are on top, so they win where they cover a thumbnail; the
        // selection is raised and zoomed, so it wins over the others.
        let on_icon =
            |w: &Window| w.icon.is_some() && self.icon_rect(self.zoomed_rect(w)).contains(x, y);
        let on_selected = (self.selected_window())
            .filter(|w| self.zoomed_rect(w).contains(x, y))
            .and(self.selected);
        (self.windows.iter().position(on_icon))
            .or(on_selected)
            .or_else(|| self.windows.iter().position(|w| w.rect.contains(x, y)))
    }

    pub fn select(&mut self, i: usize) {
        if i < self.windows.len() && self.selected != Some(i) {
            self.selected = Some(i);
            self.update_selection();
        }
    }

    /// Move to the nearest window in `dir`.
    pub fn move_selection(&mut self, dir: Dir) {
        let Some(cur) = self.selected else {
            self.select(0);
            return;
        };
        let rects: Vec<layout::Rect> = self
            .windows
            .iter()
            .map(|w| {
                let r = w.rect;
                layout::Rect::new(r.x as f64, r.y as f64, r.w as f64, r.h as f64)
            })
            .collect();
        if let Some(next) = layout::neighbor(&rects, cur, dir) {
            self.select(next);
        }
    }

    /// Cycle in input order.
    pub fn cycle(&mut self, step: isize) {
        let n = self.windows.len();
        if n == 0 {
            return;
        }
        let next = match self.selected {
            Some(cur) => (cur as isize + step).rem_euclid(n as isize) as usize,
            None if step > 0 => 0,
            None => n - 1,
        };
        self.select(next);
    }

    /// Remove a closed window. The selection moves to its successor.
    pub fn remove(&mut self, i: usize) {
        self.windows.remove(i);
        let n = self.windows.len();
        self.selected = match self.selected {
            _ if n == 0 => None,
            Some(s) if s > i => Some(s - 1),
            Some(s) if s == i => Some(i.min(n - 1)),
            other => other,
        };
        if n > 0 {
            self.relayout();
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        // Before the surface, or it is a protocol error.
        if let Some(alpha) = self.main_alpha.take() {
            alpha.destroy();
        }
        self.viewport.destroy();
        self.background.destroy();
        self.placeholder.destroy();
        self.card_color.destroy();
        self.transparent.destroy();
        // Like capture buffers, attached ones may be destroyed right away.
        for buffer in self.icon_buffers.values().chain(&self.stale_buffers) {
            buffer.destroy();
        }
        if let Some(pill) = &self.pill {
            pill.buffer.destroy();
        }
    }
}

fn trace(t0: Instant, what: std::fmt::Arguments) {
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    warn!("trace {ms:7.1} ms  {what}");
}

/// A 1x1 buffer of an 0xRRGGBBAA color, for scaling with a viewport.
fn color_buffer(g: &Globals, rgba: u32, qh: &Qh) -> std::io::Result<WlBuffer> {
    // Both buffer kinds take premultiplied alpha.
    let [r, gr, b, a] = rgba.to_be_bytes().map(|c| c as f64 / 255.0);
    let [r, gr, b] = [r, gr, b].map(|c| c * a);
    if let Some(single_pixel) = &g.single_pixel {
        let c = |v: f64| (v * u32::MAX as f64) as u32;
        return Ok(single_pixel.create_u32_rgba_buffer(c(r), c(gr), c(b), c(a), qh, Ignore));
    }
    let mut pool = RawPool::new(4, &g.shm).map_err(std::io::Error::other)?;
    let c = |v: f64| (v * 255.0).round() as u32;
    let pixel = c(a) << 24 | c(r) << 16 | c(gr) << 8 | c(b);
    pool.mmap()[..4].copy_from_slice(&pixel.to_le_bytes());
    Ok(pool.create_buffer(0, 1, 1, 4, wl_shm::Format::Argb8888, Ignore, qh))
}

/// An 0xRRGGBBAA color drawn over black, opaque.
fn over_black(rgba: u32) -> u32 {
    let [r, g, b, a] = rgba.to_be_bytes();
    let c = |v: u8| ((v as u32 * a as u32 + 127) / 255) as u8;
    u32::from_be_bytes([c(r), c(g), c(b), 0xff])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_background() {
        assert_eq!(over_black(0x000000b3), 0x000000ff);
        assert_eq!(over_black(0xff8000ff), 0xff8000ff);
        assert_eq!(over_black(0xff000080), 0x800000ff);
        assert_eq!(over_black(0xffffff00), 0x000000ff);
    }

    #[test]
    fn easing() {
        assert_eq!(ease_out(-1.0), 0.0);
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.5) > 0.5);
        let ms = Duration::from_millis;
        assert_eq!(progress(ms(125), ms(250)), 0.5);
        assert_eq!(progress(ms(1), Duration::ZERO), 1.0);
    }
}
