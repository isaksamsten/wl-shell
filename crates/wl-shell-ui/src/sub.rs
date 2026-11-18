//! The element every scene is made of: a sync subsurface scaled with a
//! viewport, without input, faded with wp_alpha_modifier_v1 if the
//! compositor has it.
//!
//! Being sync, changes to it apply with the parent's next commit, so a
//! frame moves and re-alphas every element and then commits the parent
//! once, atomically.

use smithay_client_toolkit::compositor::Region;
use wayland_client::{
    QueueHandle,
    protocol::{
        wl_buffer::WlBuffer, wl_output::Transform, wl_subsurface::WlSubsurface,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::{
    alpha_modifier::v1::client::wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
    viewporter::client::wp_viewport::WpViewport,
};

use wl_shell_core::globals::{Globals, Ignore};
use wl_shell_core::{Program, Shell};

use crate::geom::{IRect, Rect};

pub struct Sub {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    alpha: Option<WpAlphaModifierSurfaceV1>,
    attached: Option<WlBuffer>,
    /// The multiplier last set.
    shown_alpha: f64,
    cropped: bool,
    transform: Transform,
}

impl Sub {
    /// A new subsurface of `parent`, above its existing children.
    pub fn new<P: Program>(
        g: &Globals,
        parent: &WlSurface,
        no_input: &Region,
        qh: &QueueHandle<Shell<P>>,
    ) -> Self {
        // Sync, the default: changes apply with the parent's commit.
        let (subsurface, surface) = g.subcompositor.create_subsurface(parent.clone(), qh);
        let viewport = g.viewporter.get_viewport(&surface, qh, Ignore);
        let alpha = (g.alpha_modifier.as_ref()).map(|m| m.get_surface(&surface, qh, Ignore));
        surface.set_input_region(Some(no_input.wl_region()));
        Self {
            surface,
            subsurface,
            viewport,
            alpha,
            attached: None,
            shown_alpha: 1.0,
            cropped: false,
            transform: Transform::Normal,
        }
    }

    pub fn surface(&self) -> &WlSurface {
        &self.surface
    }

    /// Stack it right below `sibling` (a sibling or the parent), from the
    /// parent's next commit on.
    pub fn place_below(&self, sibling: &WlSurface) {
        self.subsurface.place_below(sibling);
    }

    /// Stack it right above `sibling` (a sibling or the parent), from the
    /// parent's next commit on.
    pub fn place_above(&self, sibling: &WlSurface) {
        self.subsurface.place_above(sibling);
    }

    /// Attach `buffer` unless it is attached already; true if it wasn't.
    pub fn show(&mut self, buffer: &WlBuffer) -> bool {
        if self.attached.as_ref() == Some(buffer) {
            return false;
        }
        self.surface.attach(Some(buffer), 0, 0);
        self.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
        self.attached = Some(buffer.clone());
        true
    }

    pub fn hide(&mut self) {
        if self.attached.take().is_some() {
            self.surface.attach(None, 0, 0);
        }
    }

    pub fn place(&self, r: IRect) {
        self.subsurface.set_position(r.x, r.y);
        self.viewport.set_destination(r.w, r.h);
    }

    /// Show only `src` (x, y, w, h in buffer pixels) of the buffer, or all
    /// of it.
    pub fn crop(&mut self, src: Option<[f64; 4]>) {
        match src {
            Some([x, y, w, h]) => {
                self.viewport.set_source(x, y, w, h);
                self.cropped = true;
            }
            None if self.cropped => {
                self.viewport.set_source(-1.0, -1.0, -1.0, -1.0);
                self.cropped = false;
            }
            None => {}
        }
    }

    pub fn set_transform(&mut self, transform: Transform) {
        if transform != self.transform {
            self.surface.set_buffer_transform(transform);
            self.transform = transform;
        }
    }

    pub fn set_alpha(&mut self, a: f64) {
        let a = a.clamp(0.0, 1.0);
        if let Some(alpha) = &self.alpha
            && a != self.shown_alpha
        {
            alpha.set_multiplier((a * u32::MAX as f64).round() as u32);
            self.shown_alpha = a;
        }
    }

    /// Show `buffer` at `r` with `alpha`, or nothing if that is invisible
    /// (or there is no buffer). Commits.
    pub fn present(&mut self, buffer: Option<&WlBuffer>, r: Rect, alpha: f64) {
        match buffer {
            Some(buffer) if alpha > 0.0 => {
                self.show(buffer);
                self.place(IRect::round(r));
                self.set_alpha(alpha);
            }
            _ => self.hide(),
        }
        self.commit();
    }

    /// Hide and commit.
    pub fn clear(&mut self) {
        self.hide();
        self.commit();
    }

    pub fn commit(&self) {
        self.surface.commit();
    }
}

impl Drop for Sub {
    fn drop(&mut self) {
        // Before the surface, or it is a protocol error.
        if let Some(alpha) = self.alpha.take() {
            alpha.destroy();
        }
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}
