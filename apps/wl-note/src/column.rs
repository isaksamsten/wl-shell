//! One output's column of notes: its layer surface and the scene on it.
//!
//! A layer surface anchored to the edge and both sides, as wide as a
//! focused note and its shadow (so focusing never resizes it), with an
//! exclusive zone of 0. The surface itself shows a transparent pixel, and
//! its input region is only the slivers, so clicks anywhere else go
//! through. It never takes the keyboard (yet).
//!
//! Each note is a sync subsurface showing its sheet of paper (papers.rs),
//! positioned by the deck's springs (deck.rs): tucked, `tuck` of it shows;
//! out of sight, its shadow is behind the edge too. The sheet reaches past
//! the surface, so it is cropped to it with the viewport, and never shows
//! on a neighboring output. Frames are only drawn while a spring moves:
//! an idle column costs nothing.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use smithay_client_toolkit::{
    compositor::{FrameCallbackData, Region},
    shell::{
        WaylandSurface,
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerSurface},
    },
};
use wayland_client::{
    QueueHandle,
    protocol::{wl_buffer::WlBuffer, wl_output::WlOutput, wl_surface::WlSurface},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Core, Shell};
use wl_shell_log::warn;
use wl_shell_render::paper::Paper;
use wl_shell_render::shadow::IconShadow;
use wl_shell_ui::geom::clip;
use wl_shell_ui::{IRect, Rect, Shape, Sub, cache, lerp};

use crate::app::WlNote;
use crate::config::{Config, Edge, NoteLayer};
use crate::deck::{Card, Deck, Item, Metrics};
use crate::papers::Papers;

/// What the column looks like, from the config.
#[derive(Clone, Debug, PartialEq)]
pub struct Style {
    pub edge: Edge,
    pub layer: NoteLayer,
    pub tuck: f64,
    pub peek: f64,
    pub metrics: Metrics,
    pub focus_width: f64,
    pub response: f64,
    pub shadow: Option<IconShadow>,
}

impl Style {
    pub fn new(config: &Config) -> Self {
        Self {
            edge: config.edge,
            layer: config.layer,
            tuck: config.tuck as f64,
            peek: config.peek as f64,
            metrics: Metrics {
                tab: config.tab_height as f64,
                gap: config.tab_gap as f64,
            },
            focus_width: config.focus_width as f64,
            response: config.response as f64,
            shadow: config.shadow(),
        }
    }

    /// Whether going from `self` to `new` takes a new surface.
    pub fn needs_new_surface(&self, new: &Style) -> bool {
        self.edge != new.edge || self.layer != new.layer
    }

    /// The surface's width: a focused note and its shadow.
    fn width(&self) -> u32 {
        let reach = self.shadow.map_or(0, |s| s.pad());
        (self.focus_width.max(self.peek) as i32 + 2 * reach + 16) as u32
    }
}

/// A card's subsurface, and the sheet it shows.
struct Slot {
    key: String,
    sub: Sub,
    /// What it shows until the sheet it wants arrives (e.g. recolored).
    shown: Option<Shape>,
}

pub struct Column {
    pub output: WlOutput,
    pub name: String,
    layer: LayerSurface,
    viewport: WpViewport,
    transparent: WlBuffer,
    no_input: Region,
    g: Rc<Globals>,
    qh: QueueHandle<Shell<WlNote>>,
    style: Style,
    papers: Rc<RefCell<Papers>>,
    width: u32,
    height: u32,
    scale: i32,
    configured: bool,
    deck: Deck,
    slots: Vec<Slot>,
    /// The input region last set.
    input: Option<Vec<IRect>>,
    frame_pending: bool,
    last_frame: Option<Instant>,
}

impl Column {
    /// An empty column on `output` (named `name`) at `scale`.
    pub fn new(
        core: &Core<WlNote>,
        output: WlOutput,
        name: String,
        scale: i32,
        style: Style,
        papers: Rc<RefCell<Papers>>,
    ) -> std::io::Result<Self> {
        let g = core.g.clone();
        let qh = core.qh.clone();
        let transparent = cache::color_buffer(&g, 0, &qh)?;
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;
        let surface = g.compositor.create_surface(&qh);
        let viewport = g.viewporter.get_viewport(&surface, &qh, Ignore);
        let layer = match style.layer {
            NoteLayer::Top => Layer::Top,
            NoteLayer::Overlay => Layer::Overlay,
        };
        let layer = (g.layer_shell).create_layer_surface(
            &qh,
            surface,
            layer,
            Some("wl-note"),
            Some(&output),
        );
        let side = match style.edge {
            Edge::Left => Anchor::LEFT,
            Edge::Right => Anchor::RIGHT,
        };
        layer.set_anchor(side | Anchor::TOP | Anchor::BOTTOM);
        layer.set_size(style.width(), 0);
        layer.set_exclusive_zone(0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        // Nothing takes input until there are slivers.
        (layer.wl_surface()).set_input_region(Some(no_input.wl_region()));
        // Mapped on configure.
        layer.commit();
        let deck = Deck::new(style.metrics, style.response);
        Ok(Self {
            output,
            name,
            layer,
            viewport,
            transparent,
            no_input,
            g,
            qh,
            style,
            papers,
            width: 0,
            height: 0,
            scale: scale.max(1),
            configured: false,
            deck,
            slots: Vec::new(),
            input: None,
            frame_pending: false,
            last_frame: None,
        })
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn is(&self, layer: &LayerSurface) -> bool {
        self.layer.wl_surface() == layer.wl_surface()
    }

    pub fn style(&self) -> &Style {
        &self.style
    }

    /// A new style (`needs_new_surface` is false for it).
    pub fn set_style(&mut self, style: Style) {
        if style == self.style {
            return;
        }
        self.layer.set_size(style.width(), 0);
        self.deck.set_metrics(style.metrics, style.response);
        self.style = style;
        self.kick();
    }

    pub fn configure(&mut self, width: u32, height: u32) {
        let first = !self.configured;
        self.configured = true;
        (self.width, self.height) = (width, height);
        if width > 0 && height > 0 {
            self.viewport.set_destination(width as i32, height as i32);
        }
        if first {
            let surface = self.layer.wl_surface();
            surface.attach(Some(&self.transparent), 0, 0);
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        }
        self.deck.resize(height as f64);
        self.render();
    }

    pub fn set_scale(&mut self, scale: i32) {
        if scale < 1 || scale == self.scale {
            return;
        }
        self.scale = scale;
        self.kick();
    }

    /// Show `items`, animating the change.
    pub fn set_notes(&mut self, items: Vec<Item>) {
        self.deck.set(items);
        self.kick();
    }

    /// A sheet arrived: show it.
    pub fn paper_ready(&mut self) {
        self.kick();
    }

    /// The buffers on screen, which must not be destroyed.
    pub fn shown_buffers(&self) -> Vec<WlBuffer> {
        (self.slots.iter())
            .filter_map(|s| s.shown.as_ref().map(|s| s.buffer.clone()))
            .collect()
    }

    // -- frames ------------------------------------------------------------

    fn request_frame(&mut self) {
        let surface = self.layer.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        self.frame_pending = true;
    }

    /// Something changed: draw it on the next frame.
    pub fn kick(&mut self) {
        if !self.configured || self.frame_pending {
            return;
        }
        if self.last_frame.is_none() {
            self.last_frame = Some(Instant::now());
        }
        self.request_frame();
        self.layer.commit();
    }

    /// The frame callback: advance the springs and draw.
    pub fn frame_done(&mut self) {
        self.frame_pending = false;
        let now = Instant::now();
        let dt = (self.last_frame).map_or(0.0, |t| now.saturating_duration_since(t).as_secs_f64());
        let moving = self.deck.advance(dt);
        self.last_frame = moving.then_some(now);
        self.render();
    }

    /// Apply the current state to every subsurface and commit, asking for
    /// another frame while anything moves.
    pub fn render(&mut self) {
        if !self.configured || self.width == 0 || self.height == 0 {
            return;
        }
        self.place_all();
        if self.deck.moving() && !self.frame_pending {
            self.request_frame();
        }
        self.layer.commit();
    }

    /// The paper a card is drawn with.
    fn paper(&self, card: &Card) -> Paper {
        Paper {
            w: self.style.peek as i32,
            h: self.style.metrics.tab as i32,
            color: card.look.color,
            edge: self.style.edge.into(),
            tilt: card.look.tilt,
            sliver: true,
            shadow: self.style.shadow,
            scale: self.scale,
        }
    }

    /// Where a card's sheet is: tucked, `tuck` of it shows; out of sight,
    /// not even its shadow (`reach`).
    fn sheet_rect(&self, card: &Card, reach: f64) -> Rect {
        let out = lerp(-reach, self.style.tuck, card.presence.value);
        let w = self.style.peek;
        let x = match self.style.edge {
            Edge::Right => self.width as f64 - out,
            Edge::Left => out - w,
        };
        Rect::new(x, card.y.value, w, self.style.metrics.tab)
    }

    /// Where a card's sliver takes input.
    fn sliver_rect(&self, card: &Card) -> IRect {
        let t = self.style.tuck;
        let x = match self.style.edge {
            Edge::Right => self.width as f64 - t,
            Edge::Left => 0.0,
        };
        IRect::round(Rect::new(x, card.y.value, t, self.style.metrics.tab))
    }

    fn place_all(&mut self) {
        // One slot per card, in the cards' order.
        let cards: Vec<Card> = self.deck.cards().to_vec();
        self.slots.retain(|s| cards.iter().any(|c| c.key == s.key));
        let surface_rect = Rect::new(0.0, 0.0, self.width as f64, self.height as f64);
        let scale = self.scale as f64;
        for card in &cards {
            if !self.slots.iter().any(|s| s.key == card.key) {
                let sub = Sub::new(&self.g, self.layer.wl_surface(), &self.no_input, &self.qh);
                self.slots.push(Slot {
                    key: card.key.clone(),
                    sub,
                    shown: None,
                });
            }
            let want = self.paper(card);
            let ready = self.papers.borrow_mut().get(&want);
            let Some(i) = self.slots.iter().position(|s| s.key == card.key) else {
                continue;
            };
            if let Some(shape) = ready {
                self.slots[i].shown = Some(shape);
            }
            let Some(shape) = self.slots[i].shown.clone() else {
                self.slots[i].sub.clear();
                continue;
            };
            // The sheet's image reaches this far past it, toward the
            // screen too.
            let reach = -shape.rect.x as f64;
            let sheet = self.sheet_rect(card, reach);
            let image = Rect::new(
                sheet.x + shape.rect.x as f64,
                sheet.y + shape.rect.y as f64,
                shape.rect.w as f64,
                shape.rect.h as f64,
            );
            let size = (shape.rect.w as f64 * scale, shape.rect.h as f64 * scale);
            let sub = &mut self.slots[i].sub;
            match clip(image, size, None, surface_rect) {
                Some((rect, src)) if card.presence.value > 0.0 => {
                    sub.show(&shape.buffer);
                    sub.crop(src);
                    sub.place(IRect::round(rect));
                    sub.commit();
                }
                _ => sub.clear(),
            }
        }
        let input: Vec<IRect> = (cards.iter())
            .filter(|c| c.present())
            .map(|c| self.sliver_rect(c))
            .collect();
        if self.input.as_ref() != Some(&input) {
            self.set_input(&input);
            self.input = Some(input);
        }
    }

    fn set_input(&self, rects: &[IRect]) {
        let region = match Region::new(&self.g.compositor) {
            Ok(region) => region,
            Err(e) => return warn!("input region: {e}"),
        };
        for r in rects {
            region.add(r.x, r.y, r.w, r.h);
        }
        // Copied on commit; the region can go right away.
        (self.layer.wl_surface()).set_input_region(Some(region.wl_region()));
    }
}

impl Drop for Column {
    fn drop(&mut self) {
        self.slots.clear();
        self.viewport.destroy();
        self.transparent.destroy();
    }
}
