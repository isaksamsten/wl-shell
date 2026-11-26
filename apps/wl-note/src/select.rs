//! Screenshots: with the column out of the way, every output is captured
//! once (frozen), and an overlay on each shows its frame to select an
//! area on. The selection is cropped from the frame and handed to the
//! program, which saves it with the notes and links it onto a note
//! (app.rs: hiding the column, saving, linking).
//!
//! - **Selecting** (`Selector`, pure, tested): a drag draws a rectangle
//!   (Shift: a square), clamped to the output it started on. Once
//!   released it can be adjusted: its eight handles resize it, a drag
//!   inside moves it, the arrow keys nudge it (Shift: by ten), and a drag
//!   outside starts a new one, on any output (the old one goes). A click
//!   without a drag, before there is a selection, takes the whole output.
//!   A double click inside it, Enter or the toolbar's ✓ saves; Ctrl+C
//!   copies it too; Escape (or ×) cancels.
//! - **The overlay** (`Overlay`): a layer surface over everything on each
//!   output, showing the frozen frame (the capture's shm buffer, with its
//!   transform, scaled to the output's logical size by a viewport, so
//!   fractional scales work). Outside the selection it is dimmed by four
//!   stretched single-pixel buffers; the selection has a border, handles,
//!   a pill with its size in pixels and the toolbar under it (above it at
//!   the bottom of the screen). Drawing only moves subsurfaces.
//! - **Pixels** (`upright_rect`, `buffer_rect`, `crop`): the selection,
//!   in logical coordinates, is scaled to the frame's upright size, and
//!   the frame's transform undone, so a rotated or fractionally scaled
//!   output gives what was on screen.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{BTN_LEFT, BTN_RIGHT, PointerEvent, PointerEventKind};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerSurface,
};
use wayland_client::QueueHandle;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_output::{Transform, WlOutput},
    wl_shm,
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId, Frame, Pixels};
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::Image;
use wl_shell_render::text;
use wl_shell_ui::{Rect, Sub, cache};

use crate::app::WlNote;
use crate::column::Fonts;

type Qh = QueueHandle<Shell<WlNote>>;

/// How near a handle the pointer grabs it.
const GRAB: f64 = 8.0;
/// How far the pointer moves with the button down before it is a drag.
const DRAG: f64 = 3.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Between the selection and what is under it.
const GAP: f64 = 8.0;
/// A handle's size.
const KNOB: i32 = 10;
/// The dim outside the selection.
const DIM: u32 = 0x00000066;
const PILL_BG: u32 = 0x202020e6;
const OK_BG: u32 = 0x3584e4ff;
const HINT: &str = "Drag to select · Click for the whole screen · Esc to cancel";

// -- selecting (pure) ---------------------------------------------------------

/// A handle on the selection: its corners and the middles of its sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

impl Handle {
    pub const ALL: [Self; 8] = [
        Self::NW,
        Self::N,
        Self::NE,
        Self::E,
        Self::SE,
        Self::S,
        Self::SW,
        Self::W,
    ];

    /// Where it is on `r`.
    pub fn at(self, r: Rect) -> (f64, f64) {
        let (x0, xm, x1) = (r.x, r.x + r.w / 2.0, r.right());
        let (y0, ym, y1) = (r.y, r.y + r.h / 2.0, r.bottom());
        match self {
            Self::NW => (x0, y0),
            Self::N => (xm, y0),
            Self::NE => (x1, y0),
            Self::E => (x1, ym),
            Self::SE => (x1, y1),
            Self::S => (xm, y1),
            Self::SW => (x0, y1),
            Self::W => (x0, ym),
        }
    }

    fn cursor(self) -> Shape {
        match self {
            Self::NW => Shape::NwResize,
            Self::N => Shape::NResize,
            Self::NE => Shape::NeResize,
            Self::E => Shape::EResize,
            Self::SE => Shape::SeResize,
            Self::S => Shape::SResize,
            Self::SW => Shape::SwResize,
            Self::W => Shape::WResize,
        }
    }
}

/// The handle of `sel` at (`x`, `y`), the nearest if several are.
pub fn handle_at(sel: Rect, x: f64, y: f64) -> Option<Handle> {
    (Handle::ALL.into_iter())
        .map(|h| {
            let (hx, hy) = h.at(sel);
            (h, (x - hx).hypot(y - hy))
        })
        .filter(|(_, d)| *d <= GRAB)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(h, _)| h)
}

fn clamp_point((x, y): (f64, f64), b: Rect) -> (f64, f64) {
    (x.clamp(b.x, b.right()), y.clamp(b.y, b.bottom()))
}

/// The rectangle from `anchor` to `to`, inside `bounds`. With `square`,
/// the largest square toward `to` that the pointer reaches and that fits.
pub fn drawn(anchor: (f64, f64), to: (f64, f64), square: bool, bounds: Rect) -> Rect {
    let (ax, ay) = clamp_point(anchor, bounds);
    if !square {
        let (tx, ty) = clamp_point(to, bounds);
        return Rect::new(ax.min(tx), ay.min(ty), (tx - ax).abs(), (ty - ay).abs());
    }
    let (dx, dy) = (to.0 - ax, to.1 - ay);
    let room_x = if dx < 0.0 {
        ax - bounds.x
    } else {
        bounds.right() - ax
    };
    let room_y = if dy < 0.0 {
        ay - bounds.y
    } else {
        bounds.bottom() - ay
    };
    let side = dx.abs().max(dy.abs()).min(room_x).min(room_y).max(0.0);
    let x = if dx < 0.0 { ax - side } else { ax };
    let y = if dy < 0.0 { ay - side } else { ay };
    Rect::new(x, y, side, side)
}

/// `orig` moved by (`dx`, `dy`), kept inside `bounds`.
pub fn moved(orig: Rect, (dx, dy): (f64, f64), bounds: Rect) -> Rect {
    let x = (orig.x + dx).clamp(bounds.x, (bounds.right() - orig.w).max(bounds.x));
    let y = (orig.y + dy).clamp(bounds.y, (bounds.bottom() - orig.h).max(bounds.y));
    Rect::new(x, y, orig.w, orig.h)
}

/// `orig` with `handle` dragged to `to`: a corner draws from the one
/// across, a side moves alone (with `square`, the other side follows).
pub fn resized(orig: Rect, handle: Handle, to: (f64, f64), square: bool, bounds: Rect) -> Rect {
    let (l, t, r, b) = (orig.x, orig.y, orig.right(), orig.bottom());
    match handle {
        Handle::NW => drawn((r, b), to, square, bounds),
        Handle::NE => drawn((l, b), to, square, bounds),
        Handle::SE => drawn((l, t), to, square, bounds),
        Handle::SW => drawn((r, t), to, square, bounds),
        Handle::N | Handle::S => {
            let fixed = if handle == Handle::N { b } else { t };
            let ty = to.1.clamp(bounds.y, bounds.bottom());
            let mut h = (ty - fixed).abs();
            let mut w = orig.w;
            if square {
                h = h.min(bounds.right() - l);
                w = h;
            }
            let y = if ty < fixed { fixed - h } else { fixed };
            Rect::new(l, y, w, h)
        }
        Handle::E | Handle::W => {
            let fixed = if handle == Handle::W { r } else { l };
            let tx = to.0.clamp(bounds.x, bounds.right());
            let mut w = (tx - fixed).abs();
            let mut h = orig.h;
            if square {
                w = w.min(bounds.bottom() - t);
                h = w;
            }
            let x = if tx < fixed { fixed - w } else { fixed };
            Rect::new(x, t, w, h)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    /// Drawing a new selection from `anchor`.
    New {
        anchor: (f64, f64),
        dragged: bool,
    },
    /// Moving the selection, from where it was pressed.
    Move {
        from: (f64, f64),
        orig: Rect,
        dragged: bool,
    },
    Resize {
        handle: Handle,
        orig: Rect,
    },
}

/// What a release asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    Nothing,
    /// The selection changed.
    Changed,
    /// Save the selection.
    Save,
}

/// The selection, and what the pointer does to it.
#[derive(Debug, Default)]
pub struct Selector {
    /// The output (an index) the selection is on, and where on it.
    pub sel: Option<(usize, Rect)>,
    grab: Option<(usize, Grab)>,
    last_click: Option<(Instant, f64, f64)>,
}

impl Selector {
    /// The selection, if it is on `out`.
    pub fn on(&self, out: usize) -> Option<Rect> {
        self.sel.filter(|(o, _)| *o == out).map(|(_, r)| r)
    }

    /// A new selection is being drawn.
    pub fn drawing(&self) -> bool {
        matches!(self.grab, Some((_, Grab::New { dragged: true, .. })))
    }

    /// The button went down at (`x`, `y`) on output `out`.
    pub fn press(&mut self, out: usize, x: f64, y: f64) {
        let grab = match self.on(out) {
            Some(sel) => match handle_at(sel, x, y) {
                Some(handle) => Grab::Resize { handle, orig: sel },
                None if sel.contains(x, y) => Grab::Move {
                    from: (x, y),
                    orig: sel,
                    dragged: false,
                },
                None => Grab::New {
                    anchor: (x, y),
                    dragged: false,
                },
            },
            None => Grab::New {
                anchor: (x, y),
                dragged: false,
            },
        };
        self.grab = Some((out, grab));
    }

    /// The pointer moved to (`x`, `y`), on the output pressed on (whose
    /// area is `bounds`). Returns whether the selection changed.
    pub fn motion(&mut self, x: f64, y: f64, square: bool, bounds: Rect) -> bool {
        let Some((out, grab)) = &mut self.grab else {
            return false;
        };
        let far = |from: (f64, f64)| (x - from.0).hypot(y - from.1) >= DRAG;
        let new = match grab {
            Grab::New { anchor, dragged } => {
                if !*dragged && !far(*anchor) {
                    return false;
                }
                *dragged = true;
                drawn(*anchor, (x, y), square, bounds)
            }
            Grab::Move {
                from,
                orig,
                dragged,
            } => {
                if !*dragged && !far(*from) {
                    return false;
                }
                *dragged = true;
                moved(*orig, (x - from.0, y - from.1), bounds)
            }
            Grab::Resize { handle, orig } => resized(*orig, *handle, (x, y), square, bounds),
        };
        let new = Some((*out, new));
        let changed = self.sel != new;
        self.sel = new;
        changed
    }

    /// The button went up at (`x`, `y`) at `now`; `bounds` is the area of
    /// the output pressed on.
    pub fn release(&mut self, x: f64, y: f64, bounds: Rect, now: Instant) -> Release {
        let Some((out, grab)) = self.grab.take() else {
            return Release::Nothing;
        };
        match grab {
            Grab::New { dragged: true, .. }
            | Grab::Move { dragged: true, .. }
            | Grab::Resize { .. } => {
                self.last_click = None;
                Release::Changed
            }
            // A click before anything is selected: the whole output.
            Grab::New { dragged: false, .. } if self.sel.is_none() => {
                self.sel = Some((out, bounds));
                Release::Save
            }
            Grab::New { dragged: false, .. } => Release::Nothing,
            Grab::Move { dragged: false, .. } => {
                let double = self.last_click.take().is_some_and(|(t, px, py)| {
                    now.saturating_duration_since(t) < DOUBLE_CLICK && (x - px).hypot(y - py) < 5.0
                });
                if double {
                    return Release::Save;
                }
                self.last_click = Some((now, x, y));
                Release::Nothing
            }
        }
    }

    /// Move the selection by (`dx`, `dy`), inside `bounds` (its output's).
    pub fn nudge(&mut self, d: (f64, f64), bounds: Rect) -> bool {
        let Some((out, sel)) = self.sel else {
            return false;
        };
        let new = moved(sel, d, bounds);
        self.sel = Some((out, new));
        new != sel
    }
}

/// Where what goes under the selection (`h` tall) goes: under it, else
/// above it, else inside it at its bottom.
pub fn below(sel: Rect, h: f64, bounds: Rect) -> f64 {
    if sel.bottom() + GAP + h <= bounds.bottom() {
        sel.bottom() + GAP
    } else if sel.y - GAP - h >= bounds.y {
        sel.y - GAP - h
    } else {
        (sel.bottom() - GAP - h).max(bounds.y)
    }
}

// -- pixels (pure) ------------------------------------------------------------

/// A rectangle of whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Area {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// The size a `buffer` shown with `t` has on screen, in its pixels: a
/// quarter turn swaps its sides.
pub fn upright((w, h): (u32, u32), t: Transform) -> (u32, u32) {
    match t {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => (h, w),
        _ => (w, h),
    }
}

/// The selection `sel` on an output `logical` large, in the pixels of a
/// frame `upright` large as it shows: scaled, rounded and kept inside.
pub fn upright_rect(sel: Rect, logical: (f64, f64), upright: (u32, u32)) -> Option<Area> {
    if logical.0 <= 0.0 || logical.1 <= 0.0 {
        return None;
    }
    let (uw, uh) = (upright.0 as f64, upright.1 as f64);
    let (sx, sy) = (uw / logical.0, uh / logical.1);
    let x0 = (sel.x * sx).round().clamp(0.0, uw);
    let x1 = (sel.right() * sx).round().clamp(0.0, uw);
    let y0 = (sel.y * sy).round().clamp(0.0, uh);
    let y1 = (sel.bottom() * sy).round().clamp(0.0, uh);
    (x1 > x0 && y1 > y0).then_some(Area {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    })
}

/// Where the point (`x`, `y`) of a frame as it shows (`w`x`h`, upright)
/// is in its buffer, shown with `t` (the compositor undoes `t`).
fn to_buffer((x, y): (f64, f64), (w, h): (f64, f64), t: Transform) -> (f64, f64) {
    match t {
        Transform::_90 => (y, w - x),
        Transform::_180 => (w - x, h - y),
        Transform::_270 => (h - y, x),
        Transform::Flipped => (w - x, y),
        Transform::Flipped90 => (y, x),
        Transform::Flipped180 => (x, h - y),
        Transform::Flipped270 => (h - y, w - x),
        _ => (x, y),
    }
}

/// The same for the pixel (`x`, `y`).
fn buffer_pixel(x: u32, y: u32, (w, h): (u32, u32), t: Transform) -> (u32, u32) {
    let (bx, by) = to_buffer((x as f64 + 0.5, y as f64 + 0.5), (w as f64, h as f64), t);
    (bx as u32, by as u32)
}

/// The selection `sel` on an output `logical` large, in a `buffer` shown
/// there with `t`: the pixels to copy.
pub fn buffer_rect(
    sel: Rect,
    logical: (f64, f64),
    buffer: (u32, u32),
    t: Transform,
) -> Option<Area> {
    let up = upright(buffer, t);
    let r = upright_rect(sel, logical, up)?;
    let size = (up.0 as f64, up.1 as f64);
    let a = to_buffer((r.x as f64, r.y as f64), size, t);
    let b = to_buffer(((r.x + r.w) as f64, (r.y + r.h) as f64), size, t);
    Some(Area {
        x: a.0.min(b.0) as u32,
        y: a.1.min(b.1) as u32,
        w: (a.0 - b.0).abs() as u32,
        h: (a.1 - b.1).abs() as u32,
    })
}

/// The pixels of `area` (of the frame as it shows) out of `px`, a buffer
/// shown with `t`: upright, as opaque RGB rows.
pub fn crop(px: &Pixels, t: Transform, area: Area) -> Option<Vec<u8>> {
    let up = upright((px.width, px.height), t);
    if area.x.checked_add(area.w)? > up.0 || area.y.checked_add(area.h)? > up.1 {
        return None;
    }
    // In memory: B, G, R, A (little endian ARGB) or R, G, B, A.
    let rgba = matches!(
        px.format,
        wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888
    );
    let stride = px.stride as usize;
    let mut out = Vec::with_capacity(area.w as usize * area.h as usize * 3);
    for v in area.y..area.y + area.h {
        for u in area.x..area.x + area.w {
            let (bx, by) = buffer_pixel(u, v, up, t);
            let i = by as usize * stride + bx as usize * 4;
            let p = px.data.get(i..i + 4)?;
            match rgba {
                true => out.extend_from_slice(&p[..3]),
                false => out.extend_from_slice(&[p[2], p[1], p[0]]),
            }
        }
    }
    Some(out)
}

/// `rgb` rows, `w`x`h`, as a PNG file.
pub fn png(rgb: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgb).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

// -- the screenshot -----------------------------------------------------------

/// Where the screenshot goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Onto this note.
    Note(String),
    /// Onto a new note, for this app (None: global).
    New(Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The column goes out of the way; capturing starts once it is.
    Hiding,
    /// Waiting for every output's frame.
    Capturing,
    /// The overlays are up.
    Selecting,
    /// The selection was taken: it is being saved.
    Saving,
}

/// What an event on the overlays asks of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Nothing,
    Cancel,
    /// Save the selection (and with `copy`, copy it too).
    Save {
        copy: bool,
    },
}

/// A selection taken out of its frame: opaque RGB rows.
pub struct Taken {
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// An output's capture and, once its frame is in, its overlay. The
/// overlay goes before the capture whose buffer it shows.
struct Shot {
    overlay: Option<Overlay>,
    frame: Option<Frame>,
    settled: bool,
    capture: Capture,
    id: CaptureId,
    output: WlOutput,
    scale: i32,
}

/// The overlay on an output.
struct Overlay {
    layer: LayerSurface,
    viewport: WpViewport,
    /// Its logical size, once configured.
    size: Option<(u32, u32)>,
    dims: [Sub; 4],
    lines: [Sub; 4],
    knobs: [Sub; 8],
    size_pill: Sub,
    hint: Sub,
    ok: Sub,
    cancel: Sub,
    ok_rect: Option<Rect>,
    cancel_rect: Option<Rect>,
    /// The pointer's enter serial, while it is on it, and the cursor shown.
    enter: Option<u32>,
    cursor: Option<Shape>,
}

impl Overlay {
    fn bounds(&self) -> Rect {
        let (w, h) = self.size.unwrap_or((0, 0));
        Rect::new(0.0, 0.0, w as f64, h as f64)
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.viewport.destroy();
    }
}

/// A pill drawn: its buffer and logical size.
#[derive(Clone)]
struct Pill {
    buffer: WlBuffer,
    w: f64,
    h: f64,
}

pub struct Screenshot {
    pub target: Target,
    pub phase: Phase,
    /// When the phase started.
    pub since: Instant,
    shots: Vec<Shot>,
    selector: Selector,
    g: std::rc::Rc<Globals>,
    qh: Qh,
    no_input: Region,
    dim: WlBuffer,
    white: WlBuffer,
    knobs: HashMap<i32, WlBuffer>,
    pills: HashMap<(String, u32, i32), Pill>,
    fonts: Fonts,
}

impl Screenshot {
    /// One to take for `target`, starting with the column going away.
    pub fn new(core: &Core<WlNote>, target: Target, fonts: Fonts) -> Result<Self, String> {
        if core.g.output_source_manager.is_none() {
            return Err("the compositor can't capture outputs".into());
        }
        let g = core.g.clone();
        let qh = core.qh.clone();
        let err = |e: std::io::Error| e.to_string();
        let no_input = Region::new(&g.compositor).map_err(|e| e.to_string())?;
        Ok(Self {
            target,
            phase: Phase::Hiding,
            since: Instant::now(),
            shots: Vec::new(),
            selector: Selector::default(),
            dim: cache::color_buffer(&g, DIM, &qh).map_err(err)?,
            white: cache::color_buffer(&g, 0xffffffff, &qh).map_err(err)?,
            g,
            qh,
            no_input,
            knobs: HashMap::new(),
            pills: HashMap::new(),
            fonts,
        })
    }

    fn set_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.since = Instant::now();
    }

    /// Capture every output.
    pub fn capture(&mut self, core: &Core<WlNote>) -> Result<(), String> {
        for o in core.outputs() {
            let id = CaptureId::next();
            if let Some(capture) = Capture::output(id, &o.wl, &self.g, true, &self.qh) {
                self.shots.push(Shot {
                    overlay: None,
                    frame: None,
                    settled: false,
                    capture,
                    id,
                    output: o.wl,
                    scale: o.scale,
                });
            }
        }
        if self.shots.is_empty() {
            return Err("no output to capture".into());
        }
        self.set_phase(Phase::Capturing);
        Ok(())
    }

    pub fn has_capture(&self, id: CaptureId) -> bool {
        self.shots.iter().any(|s| s.id == id)
    }

    /// An event of capture `id`.
    pub fn capture_update(
        &mut self,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        let Some(shot) = self.shots.iter_mut().find(|s| s.id == id) else {
            return;
        };
        match f(&mut shot.capture, &self.g) {
            Some(CaptureEvent::Frame(frame)) => {
                // Static: the only one.
                if shot.frame.is_none() {
                    shot.frame = Some(frame);
                }
                shot.settled = true;
            }
            Some(CaptureEvent::Failed(why)) => {
                warn!("screenshot: capturing an output failed: {why}");
                shot.settled = true;
            }
            None => {}
        }
    }

    /// Every output's frame is in (or won't come).
    pub fn captured(&self) -> bool {
        self.shots.iter().all(|s| s.settled)
    }

    /// Put up the overlays on the outputs captured.
    pub fn show(&mut self) -> Result<(), String> {
        let (g, qh) = (self.g.clone(), self.qh.clone());
        for shot in &mut self.shots {
            if shot.frame.is_none() {
                continue;
            }
            let surface = g.compositor.create_surface(&qh);
            let viewport = g.viewporter.get_viewport(&surface, &qh, Ignore);
            let layer = g.layer_shell.create_layer_surface(
                &qh,
                surface,
                Layer::Overlay,
                Some("wl-note-screenshot"),
                Some(&shot.output),
            );
            layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_size(0, 0);
            layer.set_exclusive_zone(-1);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.commit();
            let parent = layer.wl_surface().clone();
            let sub = || Sub::new(&g, &parent, &self.no_input, &qh);
            shot.overlay = Some(Overlay {
                dims: std::array::from_fn(|_| sub()),
                lines: std::array::from_fn(|_| sub()),
                knobs: std::array::from_fn(|_| sub()),
                size_pill: sub(),
                hint: sub(),
                ok: sub(),
                cancel: sub(),
                layer,
                viewport,
                size: None,
                ok_rect: None,
                cancel_rect: None,
                enter: None,
                cursor: None,
            });
        }
        if self.shots.iter().all(|s| s.overlay.is_none()) {
            return Err("no output was captured".into());
        }
        self.set_phase(Phase::Selecting);
        Ok(())
    }

    /// Whether `layer` is one of the overlays.
    pub fn is(&self, layer: &LayerSurface) -> bool {
        self.overlay_of(layer.wl_surface()).is_some()
    }

    fn overlay_of(
        &self,
        surface: &wayland_client::protocol::wl_surface::WlSurface,
    ) -> Option<usize> {
        (self.shots.iter()).position(|s| {
            s.overlay
                .as_ref()
                .is_some_and(|o| o.layer.wl_surface() == surface)
        })
    }

    /// Whether `surface` is one of the overlays'.
    pub fn owns_surface(&self, surface: &wayland_client::protocol::wl_surface::WlSurface) -> bool {
        self.overlay_of(surface).is_some()
    }

    /// An overlay was configured, `w`x`h`: show its frame.
    pub fn configure(&mut self, layer: &LayerSurface, w: u32, h: u32) {
        let Some(i) = self.overlay_of(layer.wl_surface()) else {
            return;
        };
        let shot = &mut self.shots[i];
        let (Some(o), Some(frame)) = (&mut shot.overlay, &shot.frame) else {
            return;
        };
        if w == 0 || h == 0 {
            return;
        }
        let first = o.size.is_none();
        o.size = Some((w, h));
        o.viewport.set_destination(w as i32, h as i32);
        if first {
            let surface = o.layer.wl_surface();
            surface.set_buffer_transform(frame.transform);
            surface.attach(Some(&frame.buffer), 0, 0);
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            shot.capture.attached(&frame.buffer);
        }
        self.render(i);
    }

    /// Draw every overlay again.
    fn render_all(&mut self) {
        for i in 0..self.shots.len() {
            self.render(i);
        }
    }

    /// A pill saying `text` on `bg`, at `scale`.
    fn pill(&mut self, text: &str, bg: u32, scale: i32) -> Option<Pill> {
        let key = (text.to_owned(), bg, scale);
        if let Some(p) = self.pills.get(&key) {
            return Some(p.clone());
        }
        let drawn = {
            let mut fonts = self.fonts.borrow_mut();
            let style = text::Style {
                font: "sans-serif".into(),
                size: 13.0,
                color: 0xffffffff,
                background: bg,
            };
            fonts.as_mut()?.pill(text, &style, 640, scale)
        };
        let buffer = (cache::shm_buffer(&self.g, &drawn.image, &self.qh))
            .inspect_err(|e| warn!("screenshot pill: {e}"))
            .ok()?;
        let pill = Pill {
            buffer,
            w: drawn.width as f64,
            h: drawn.height as f64,
        };
        // The size pill changes as the selection does.
        if self.pills.len() > 64 {
            for (_, p) in self.pills.drain() {
                p.buffer.destroy();
            }
        }
        self.pills.insert(key, pill.clone());
        Some(pill)
    }

    fn knob(&mut self, scale: i32) -> Option<WlBuffer> {
        if let Some(b) = self.knobs.get(&scale) {
            return Some(b.clone());
        }
        let image = knob_image(scale);
        let buffer = (cache::shm_buffer(&self.g, &image, &self.qh))
            .inspect_err(|e| warn!("screenshot handle: {e}"))
            .ok()?;
        self.knobs.insert(scale, buffer.clone());
        Some(buffer)
    }

    /// Place overlay `i`'s parts for the selection now, and commit it.
    fn render(&mut self, i: usize) {
        let Some(shot) = self.shots.get(i) else {
            return;
        };
        let Some(o) = shot.overlay.as_ref().filter(|o| o.size.is_some()) else {
            return;
        };
        let bounds = o.bounds();
        let scale = shot.scale.max(1);
        let sel = self.selector.on(i);
        let adjusting = sel.is_some() && !self.selector.drawing();
        // How large it is in the frame's pixels.
        let size = sel.and_then(|s| {
            let frame = shot.frame.as_ref()?;
            let up = upright((frame.width, frame.height), frame.transform);
            upright_rect(s, (bounds.w, bounds.h), up)
        });
        let size_pill = size.and_then(|a| self.pill(&format!("{} × {}", a.w, a.h), PILL_BG, scale));
        let hint = (self.selector.sel.is_none())
            .then(|| self.pill(HINT, PILL_BG, scale))
            .flatten();
        let (ok, cancel) = match adjusting {
            true => (
                self.pill("✓ Add to note", OK_BG, scale),
                self.pill("×", PILL_BG, scale),
            ),
            false => (None, None),
        };
        let knob = adjusting.then(|| self.knob(scale)).flatten();
        let (dim, white) = (self.dim.clone(), self.white.clone());
        let Some(o) = self.shots[i].overlay.as_mut() else {
            return;
        };
        let show = |sub: &mut Sub, buffer: &WlBuffer, r: Rect| {
            if r.w > 0.0 && r.h > 0.0 {
                sub.present(Some(buffer), r, 1.0);
            } else {
                sub.clear();
            }
        };
        // The dim, around the selection.
        let dims = match sel {
            Some(s) => [
                Rect::new(0.0, 0.0, bounds.w, s.y),
                Rect::new(0.0, s.bottom(), bounds.w, bounds.h - s.bottom()),
                Rect::new(0.0, s.y, s.x, s.h),
                Rect::new(s.right(), s.y, bounds.w - s.right(), s.h),
            ],
            None => [bounds, Rect::default(), Rect::default(), Rect::default()],
        };
        for (sub, r) in o.dims.iter_mut().zip(dims) {
            show(sub, &dim, r);
        }
        // A border just outside it.
        let lines = match sel {
            Some(s) => [
                Rect::new(s.x - 1.0, s.y - 1.0, s.w + 2.0, 1.0),
                Rect::new(s.x - 1.0, s.bottom(), s.w + 2.0, 1.0),
                Rect::new(s.x - 1.0, s.y, 1.0, s.h),
                Rect::new(s.right(), s.y, 1.0, s.h),
            ],
            None => [Rect::default(); 4],
        };
        for (sub, r) in o.lines.iter_mut().zip(lines) {
            show(sub, &white, r);
        }
        for (sub, h) in o.knobs.iter_mut().zip(Handle::ALL) {
            match (sel, &knob) {
                (Some(s), Some(buffer)) => {
                    let (x, y) = h.at(s);
                    let k = KNOB as f64;
                    show(sub, buffer, Rect::new(x - k / 2.0, y - k / 2.0, k, k));
                }
                _ => sub.clear(),
            }
        }
        // Under it: its size, and the toolbar at its right.
        o.ok_rect = None;
        o.cancel_rect = None;
        if let Some(s) = sel {
            let h = [&size_pill, &ok, &cancel]
                .iter()
                .filter_map(|p| p.as_ref().map(|p| p.h))
                .fold(0.0, f64::max);
            let y = below(s, h, bounds);
            let fit = |x: f64, w: f64| x.clamp(bounds.x, (bounds.right() - w).max(bounds.x));
            match &size_pill {
                Some(p) => show(
                    &mut o.size_pill,
                    &p.buffer,
                    Rect::new(fit(s.x, p.w), y, p.w, p.h),
                ),
                None => o.size_pill.clear(),
            }
            let mut right = s.right();
            if let Some(p) = &cancel {
                let r = Rect::new(fit(right - p.w, p.w), y, p.w, p.h);
                show(&mut o.cancel, &p.buffer, r);
                o.cancel_rect = Some(r);
                right = r.x - 6.0;
            } else {
                o.cancel.clear();
            }
            if let Some(p) = &ok {
                let r = Rect::new(fit(right - p.w, p.w), y, p.w, p.h);
                show(&mut o.ok, &p.buffer, r);
                o.ok_rect = Some(r);
            } else {
                o.ok.clear();
            }
        } else {
            for sub in [&mut o.size_pill, &mut o.ok, &mut o.cancel] {
                sub.clear();
            }
        }
        match &hint {
            Some(p) => {
                let r = Rect::new((bounds.w - p.w) / 2.0, 24.0, p.w, p.h);
                show(&mut o.hint, &p.buffer, r);
            }
            None => o.hint.clear(),
        }
        o.layer.commit();
    }

    /// The cursor for the pointer at (`x`, `y`) on overlay `i`.
    fn cursor_at(&self, i: usize, x: f64, y: f64) -> Shape {
        let Some(o) = self.shots[i].overlay.as_ref() else {
            return Shape::Crosshair;
        };
        if [o.ok_rect, o.cancel_rect]
            .iter()
            .flatten()
            .any(|r| r.contains(x, y))
        {
            return Shape::Pointer;
        }
        match self.selector.on(i) {
            Some(s) => match handle_at(s, x, y) {
                Some(h) => h.cursor(),
                None if s.contains(x, y) => Shape::Move,
                None => Shape::Crosshair,
            },
            None => Shape::Crosshair,
        }
    }

    fn set_cursor(&mut self, core: &Core<WlNote>, i: usize, shape: Shape) {
        let Some(o) = self.shots[i].overlay.as_mut() else {
            return;
        };
        if let Some(serial) = o.enter
            && o.cursor != Some(shape)
        {
            core.input.set_cursor(serial, shape);
            o.cursor = Some(shape);
        }
    }

    /// The area of the output (index) `out`'s overlay.
    fn bounds(&self, out: usize) -> Rect {
        (self.shots.get(out).and_then(|s| s.overlay.as_ref()))
            .map_or(Rect::default(), Overlay::bounds)
    }

    /// A pointer event on one of the overlays.
    pub fn pointer(&mut self, core: &Core<WlNote>, event: &PointerEvent) -> Outcome {
        let Some(i) = self.overlay_of(&event.surface) else {
            return Outcome::Nothing;
        };
        if self.phase != Phase::Selecting {
            return Outcome::Nothing;
        }
        let (x, y) = event.position;
        match event.kind {
            PointerEventKind::Enter { serial } => {
                if let Some(o) = self.shots[i].overlay.as_mut() {
                    o.enter = Some(serial);
                    o.cursor = None;
                }
                let shape = self.cursor_at(i, x, y);
                self.set_cursor(core, i, shape);
            }
            PointerEventKind::Leave { .. } => {
                if let Some(o) = self.shots[i].overlay.as_mut() {
                    o.enter = None;
                }
            }
            PointerEventKind::Motion { .. } => {
                if let Some((out, _)) = self.selector.grab {
                    let square = core.input.modifiers.shift;
                    if self.selector.motion(x, y, square, self.bounds(out)) {
                        self.render_all();
                    }
                } else {
                    let shape = self.cursor_at(i, x, y);
                    self.set_cursor(core, i, shape);
                }
            }
            PointerEventKind::Press { button, .. } if button == BTN_LEFT => {
                let o = self.shots[i].overlay.as_ref();
                if o.and_then(|o| o.ok_rect).is_some_and(|r| r.contains(x, y)) {
                    return Outcome::Save { copy: false };
                }
                if o.and_then(|o| o.cancel_rect)
                    .is_some_and(|r| r.contains(x, y))
                {
                    return Outcome::Cancel;
                }
                self.selector.press(i, x, y);
            }
            PointerEventKind::Press { button, .. } if button == BTN_RIGHT => {
                return Outcome::Cancel;
            }
            PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                let out = self.selector.grab.map_or(i, |(out, _)| out);
                let release = self
                    .selector
                    .release(x, y, self.bounds(out), Instant::now());
                self.render_all();
                let shape = self.cursor_at(i, x, y);
                self.set_cursor(core, i, shape);
                if release == Release::Save {
                    return Outcome::Save { copy: false };
                }
            }
            _ => {}
        }
        Outcome::Nothing
    }

    /// A key, while an overlay has the keyboard.
    pub fn key(&mut self, event: &KeyEvent, mods: &Modifiers) -> Outcome {
        if self.phase != Phase::Selecting {
            return Outcome::Nothing;
        }
        let has = self.selector.sel.is_some();
        let step = if mods.shift { 10.0 } else { 1.0 };
        let d = match event.keysym {
            Keysym::Escape => return Outcome::Cancel,
            Keysym::Return | Keysym::KP_Enter if has => return Outcome::Save { copy: false },
            Keysym::c | Keysym::C if mods.ctrl && has => return Outcome::Save { copy: true },
            Keysym::Left | Keysym::KP_Left => (-step, 0.0),
            Keysym::Right | Keysym::KP_Right => (step, 0.0),
            Keysym::Up | Keysym::KP_Up => (0.0, -step),
            Keysym::Down | Keysym::KP_Down => (0.0, step),
            _ => return Outcome::Nothing,
        };
        if let Some((out, _)) = self.selector.sel
            && self.selector.grab.is_none()
            && self.selector.nudge(d, self.bounds(out))
        {
            self.render_all();
        }
        Outcome::Nothing
    }

    /// Take the selection out of its frame; the overlays stay until the
    /// screenshot is dropped.
    pub fn take(&mut self) -> Option<Taken> {
        let (out, sel) = self.selector.sel?;
        let bounds = self.bounds(out);
        let shot = self.shots.get_mut(out)?;
        let frame = shot.frame.as_ref()?;
        let up = upright((frame.width, frame.height), frame.transform);
        let area = upright_rect(sel, (bounds.w, bounds.h), up)?;
        let (buffer, transform) = (frame.buffer.clone(), frame.transform);
        // What is read of the buffer: inside it, or nothing is.
        let read = buffer_rect(
            sel,
            (bounds.w, bounds.h),
            (frame.width, frame.height),
            transform,
        )?;
        let px = shot.capture.pixels(&buffer)?;
        if read.x + read.w > px.width || read.y + read.h > px.height {
            return None;
        }
        let rgb = crop(&px, transform, area)?;
        self.set_phase(Phase::Saving);
        Some(Taken {
            rgb,
            width: area.w,
            height: area.h,
        })
    }

    /// Output `output` went away: its shot goes (the selection too, if it
    /// was on it). Returns whether any overlay is left.
    pub fn output_removed(&mut self, output: &WlOutput) -> bool {
        if let Some(i) = self.shots.iter().position(|s| s.output == *output) {
            self.shots.remove(i);
            self.selector = Selector::default();
        }
        match self.phase {
            Phase::Selecting | Phase::Saving => self.shots.iter().any(|s| s.overlay.is_some()),
            _ => true,
        }
    }
}

impl Drop for Screenshot {
    fn drop(&mut self) {
        // The overlays first: they show the buffers.
        self.shots.clear();
        for (_, p) in self.pills.drain() {
            p.buffer.destroy();
        }
        for (_, b) in self.knobs.drain() {
            b.destroy();
        }
        self.dim.destroy();
        self.white.destroy();
    }
}

/// A handle: a white disc with a dark ring, `KNOB` wide, at `scale`.
fn knob_image(scale: i32) -> Image {
    let size = (KNOB * scale.max(1)) as u32;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    let half = KNOB as f64 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let c = |i: u32| (i as f64 + 0.5) / scale.max(1) as f64 - half;
            let r = c(x).hypot(c(y));
            let white = (half - 1.5 - r + 0.5).clamp(0.0, 1.0);
            let ring = (half - r + 0.5).clamp(0.0, 1.0);
            // Premultiplied: white over a dark ring.
            let ring_a = ring * 0.6;
            let a = white + ring_a * (1.0 - white);
            let v = (white * 255.0).round() as u8;
            data.extend([v, v, v, (a * 255.0).round() as u8]);
        }
    }
    Image {
        width: size,
        height: size,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 600.0,
    };

    #[test]
    fn drags_draw_clamped_rectangles() {
        // Either way round.
        assert_eq!(
            drawn((100.0, 100.0), (300.0, 200.0), false, B),
            Rect::new(100.0, 100.0, 200.0, 100.0)
        );
        assert_eq!(
            drawn((300.0, 200.0), (100.0, 100.0), false, B),
            Rect::new(100.0, 100.0, 200.0, 100.0)
        );
        // Past the output's edge: to it.
        assert_eq!(
            drawn((900.0, 500.0), (1400.0, -50.0), false, B),
            Rect::new(900.0, 0.0, 100.0, 500.0)
        );
        // Shift: a square, as large as the pointer reaches, toward it.
        assert_eq!(
            drawn((100.0, 100.0), (300.0, 150.0), true, B),
            Rect::new(100.0, 100.0, 200.0, 200.0)
        );
        assert_eq!(
            drawn((100.0, 100.0), (40.0, 80.0), true, B),
            Rect::new(40.0, 40.0, 60.0, 60.0)
        );
        // ...and that fits.
        assert_eq!(
            drawn((900.0, 100.0), (1200.0, 400.0), true, B),
            Rect::new(900.0, 100.0, 100.0, 100.0)
        );
    }

    #[test]
    fn handles_resize_and_moves_stay_inside() {
        let sel = Rect::new(100.0, 100.0, 200.0, 100.0);
        assert_eq!(handle_at(sel, 101.0, 99.0), Some(Handle::NW));
        assert_eq!(handle_at(sel, 200.0, 203.0), Some(Handle::S));
        assert_eq!(handle_at(sel, 300.0, 150.0), Some(Handle::E));
        assert_eq!(handle_at(sel, 200.0, 150.0), None);
        // A corner draws from the one across, past it too.
        assert_eq!(
            resized(sel, Handle::SE, (400.0, 300.0), false, B),
            Rect::new(100.0, 100.0, 300.0, 200.0)
        );
        assert_eq!(
            resized(sel, Handle::SE, (50.0, 150.0), false, B),
            Rect::new(50.0, 100.0, 50.0, 50.0)
        );
        // A side moves alone.
        assert_eq!(
            resized(sel, Handle::N, (0.0, 20.0), false, B),
            Rect::new(100.0, 20.0, 200.0, 180.0)
        );
        assert_eq!(
            resized(sel, Handle::W, (-30.0, 0.0), false, B),
            Rect::new(0.0, 100.0, 300.0, 100.0)
        );
        // With Shift, the other side follows.
        assert_eq!(
            resized(sel, Handle::E, (250.0, 0.0), true, B),
            Rect::new(100.0, 100.0, 150.0, 150.0)
        );
        // Moving keeps it whole and inside.
        assert_eq!(
            moved(sel, (-500.0, 1000.0), B),
            Rect::new(0.0, 500.0, 200.0, 100.0)
        );
    }

    #[test]
    fn selecting_with_the_pointer() {
        let now = Instant::now();
        let mut s = Selector::default();
        // A drag draws one.
        s.press(1, 100.0, 100.0);
        assert!(!s.motion(101.0, 101.0, false, B), "not a drag yet");
        assert!(s.motion(300.0, 200.0, false, B));
        assert!(s.drawing());
        assert_eq!(s.release(300.0, 200.0, B, now), Release::Changed);
        assert_eq!(s.on(1), Some(Rect::new(100.0, 100.0, 200.0, 100.0)));
        assert_eq!(s.on(0), None);
        // Dragging inside moves it.
        s.press(1, 150.0, 150.0);
        s.motion(160.0, 170.0, false, B);
        s.release(160.0, 170.0, B, now);
        assert_eq!(s.on(1), Some(Rect::new(110.0, 120.0, 200.0, 100.0)));
        // A handle resizes it.
        s.press(1, 310.0, 220.0);
        s.motion(410.0, 320.0, false, B);
        s.release(410.0, 320.0, B, now);
        assert_eq!(s.on(1), Some(Rect::new(110.0, 120.0, 300.0, 200.0)));
        // A click outside it leaves it.
        s.press(1, 900.0, 50.0);
        assert_eq!(s.release(900.0, 50.0, B, now), Release::Nothing);
        assert!(s.on(1).is_some());
        // A double click inside saves it.
        s.press(1, 200.0, 200.0);
        assert_eq!(s.release(200.0, 200.0, B, now), Release::Nothing);
        s.press(1, 200.0, 200.0);
        let later = now + Duration::from_millis(200);
        assert_eq!(s.release(200.0, 200.0, B, later), Release::Save);
        // A new one on another output drops it.
        s.press(0, 10.0, 10.0);
        s.motion(50.0, 50.0, true, B);
        s.release(50.0, 50.0, B, now);
        assert_eq!(s.sel, Some((0, Rect::new(10.0, 10.0, 40.0, 40.0))));
        // The arrows nudge it, inside.
        assert!(s.nudge((-100.0, 0.0), B));
        assert_eq!(s.on(0), Some(Rect::new(0.0, 10.0, 40.0, 40.0)));
        assert!(!s.nudge((-1.0, 0.0), B));
    }

    #[test]
    fn a_click_takes_the_whole_output() {
        let mut s = Selector::default();
        s.press(2, 400.0, 300.0);
        s.motion(401.0, 300.0, false, B);
        assert_eq!(s.release(401.0, 300.0, B, Instant::now()), Release::Save);
        assert_eq!(s.sel, Some((2, B)));
    }

    #[test]
    fn the_toolbar_goes_under_or_above() {
        assert_eq!(below(Rect::new(0.0, 100.0, 50.0, 50.0), 20.0, B), 158.0);
        assert_eq!(below(Rect::new(0.0, 500.0, 50.0, 90.0), 20.0, B), 472.0);
        assert_eq!(below(Rect::new(0.0, 0.0, 50.0, 600.0), 20.0, B), 572.0);
    }

    const TRANSFORMS: [Transform; 8] = [
        Transform::Normal,
        Transform::_90,
        Transform::_180,
        Transform::_270,
        Transform::Flipped,
        Transform::Flipped90,
        Transform::Flipped180,
        Transform::Flipped270,
    ];

    /// A buffer shown with `t` that looks like `upright` (`w`x`h` pixels,
    /// each its own value), in `format`.
    fn buffer_of(w: u32, h: u32, t: Transform, rgba: bool) -> (Vec<u8>, (u32, u32)) {
        let (bw, bh) = upright((w, h), t);
        let mut data = vec![0u8; (bw * bh * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let (bx, by) = buffer_pixel(x, y, (w, h), t);
                let i = ((by * bw + bx) * 4) as usize;
                let [r, g, b] = [x as u8, y as u8, 7];
                let px = if rgba { [r, g, b, 0] } else { [b, g, r, 0] };
                data[i..i + 4].copy_from_slice(&px);
            }
        }
        (data, (bw, bh))
    }

    #[test]
    fn transforms_map_the_corners() {
        // A 4x2 picture's top left corner, in the buffer of each transform.
        let corner = |t| buffer_pixel(0, 0, (4, 2), t);
        assert_eq!(corner(Transform::Normal), (0, 0));
        // Turned a quarter counterclockwise: the top left is at the bottom.
        assert_eq!(corner(Transform::_90), (0, 3));
        assert_eq!(corner(Transform::_180), (3, 1));
        assert_eq!(corner(Transform::_270), (1, 0));
        assert_eq!(corner(Transform::Flipped), (3, 0));
        assert_eq!(corner(Transform::Flipped90), (0, 0));
        assert_eq!(corner(Transform::Flipped180), (0, 1));
        assert_eq!(corner(Transform::Flipped270), (1, 3));
        // Every pixel somewhere, once.
        for t in TRANSFORMS {
            let (bw, bh) = upright((4, 2), t);
            let mut seen = std::collections::HashSet::new();
            for y in 0..2 {
                for x in 0..4 {
                    let (bx, by) = buffer_pixel(x, y, (4, 2), t);
                    assert!(bx < bw && by < bh, "{t:?}");
                    assert!(seen.insert((bx, by)), "{t:?}");
                }
            }
        }
    }

    #[test]
    fn the_crop_is_what_was_on_screen() {
        // A 1.5x output, 80x40 logical, so 120x60 as shown.
        let (w, h) = (120, 60);
        let logical = (80.0, 40.0);
        let sel = Rect::new(10.0, 4.0, 20.0, 12.0);
        for t in TRANSFORMS {
            for rgba in [false, true] {
                let (data, (bw, bh)) = buffer_of(w, h, t, rgba);
                let px = Pixels {
                    data: &data,
                    width: bw,
                    height: bh,
                    stride: bw * 4,
                    format: match rgba {
                        true => wl_shm::Format::Xbgr8888,
                        false => wl_shm::Format::Argb8888,
                    },
                };
                let area = upright_rect(sel, logical, upright((bw, bh), t)).unwrap();
                assert_eq!(
                    area,
                    Area {
                        x: 15,
                        y: 6,
                        w: 30,
                        h: 18
                    }
                );
                let rgb = crop(&px, t, area).unwrap();
                assert_eq!(rgb.len(), 30 * 18 * 3);
                for (n, p) in rgb.chunks(3).enumerate() {
                    let (x, y) = (15 + n as u32 % 30, 6 + n as u32 / 30);
                    assert_eq!(p, [x as u8, y as u8, 7], "{t:?} at {x},{y}");
                }
                // The pixels copied are the buffer's rect.
                let r = buffer_rect(sel, logical, (bw, bh), t).unwrap();
                let quarter = upright((1, 2), t) == (2, 1);
                assert_eq!((r.w, r.h), if quarter { (18, 30) } else { (30, 18) });
                assert!(r.x + r.w <= bw && r.y + r.h <= bh, "{t:?}");
                let (bx, by) = buffer_pixel(15, 6, (w, h), t);
                assert!((r.x..r.x + r.w).contains(&bx) && (r.y..r.y + r.h).contains(&by));
            }
        }
    }

    #[test]
    fn selections_outside_are_cut_or_nothing() {
        let r = buffer_rect(
            Rect::new(70.0, 30.0, 50.0, 50.0),
            (80.0, 40.0),
            (60, 120),
            Transform::_90,
        )
        .unwrap();
        // 105..120 by 45..60 shown, in a buffer turned a quarter.
        assert_eq!(
            r,
            Area {
                x: 45,
                y: 0,
                w: 15,
                h: 15
            }
        );
        assert_eq!(
            upright_rect(Rect::new(90.0, 0.0, 5.0, 5.0), (80.0, 40.0), (120, 60)),
            None
        );
    }

    #[test]
    fn png_files() {
        let data = png(&[255, 0, 0, 0, 255, 0], 2, 1).unwrap();
        assert!(data.starts_with(b"\x89PNG\r\n\x1a\n"));
    }
}
