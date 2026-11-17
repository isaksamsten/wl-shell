//! The activate surface: what keyboard activation (activate.rs) holds the
//! keyboard with. A 1×1 layer surface (layer `overlay`, exclusive
//! keyboard, no input region, a transparent pixel), created without an
//! output so the compositor puts it on the focused one; its
//! `wl_surface.enter` says which dock acts. It lives while the mode is on.

use smithay_client_toolkit::compositor::Region;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{KeyboardInteractivity, Layer, LayerSurface};
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1;

use wl_shell_core::{globals::Ignore, Core};
use wl_shell_ui::cache;

use crate::app::WlDock;

pub struct ActivateView {
    layer: LayerSurface,
    transparent: WlBuffer,
    configured: bool,
    /// One per seat; destroyed before the surface.
    inhibitors: Vec<ZwpKeyboardShortcutsInhibitorV1>,
}

impl ActivateView {
    /// Map the surface; the compositor puts it on the focused output and
    /// gives it the keyboard.
    pub fn new(core: &Core<WlDock>) -> std::io::Result<Self> {
        let g = &core.g;
        let qh = &core.qh;
        let transparent = cache::color_buffer(g, 0, qh)?;
        let no_input = Region::new(&g.compositor).map_err(std::io::Error::other)?;
        let surface = g.compositor.create_surface(qh);
        let layer = (g.layer_shell).create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("wl-dock-activate"),
            None,
        );
        layer.set_size(1, 1);
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        // Copied: the region can go.
        (layer.wl_surface()).set_input_region(Some(no_input.wl_region()));
        layer.commit();
        let inhibitors = g
            .shortcuts_inhibit
            .as_ref()
            .map_or_else(Vec::new, |manager| {
                core.seat_state
                    .seats()
                    .map(|seat| manager.inhibit_shortcuts(layer.wl_surface(), &seat, qh, Ignore))
                    .collect()
            });
        Ok(Self {
            layer,
            transparent,
            configured: false,
            inhibitors,
        })
    }

    pub fn surface(&self) -> &WlSurface {
        self.layer.wl_surface()
    }

    pub fn is(&self, layer: &LayerSurface) -> bool {
        self.layer.wl_surface() == layer.wl_surface()
    }

    /// The compositor configured it: show the pixel, which maps it.
    pub fn configure(&mut self) {
        if std::mem::replace(&mut self.configured, true) {
            return;
        }
        let surface = self.layer.wl_surface();
        surface.attach(Some(&self.transparent), 0, 0);
        surface.damage_buffer(0, 0, 1, 1);
        surface.commit();
    }
}

impl Drop for ActivateView {
    fn drop(&mut self) {
        for inhibitor in self.inhibitors.drain(..) {
            inhibitor.destroy();
        }
        self.transparent.destroy();
    }
}
