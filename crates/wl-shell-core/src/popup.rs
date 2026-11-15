//! xdg popups of layer surfaces (`zwlr_layer_surface_v1.get_popup`): the
//! compositor places them next to a rect of their parent with a
//! positioner, and flips or slides them to keep them on the output.
//!
//! A program creates a positioner (`positioner`), fills it in, and opens
//! the popup with `layer_popup`. Its configure and dismissal arrive as
//! `Program::popup_configure` and `Program::popup_done`. Dropping the
//! returned `Popup` (its last clone) destroys it; the program destroys its
//! subsurfaces first.
//!
//! A popup opened with a grab takes the pointer and keyboard: clicks on
//! other clients' surfaces dismiss it (`popup_done`), clicks on the
//! program's own surfaces still reach them, and keys go to the popup. The
//! grab needs the serial of the input event that opened it (a button
//! press, say). A grabbing popup has to be the topmost one: close the
//! program's other grabbing popups first.

use smithay_client_toolkit::shell::{
    wlr_layer::LayerSurface,
    xdg::{XdgPositioner, popup::Popup},
};
use wayland_client::{QueueHandle, protocol::wl_seat::WlSeat};

use crate::globals::Globals;
use crate::shell::{Program, Shell};

pub use smithay_client_toolkit::shell::xdg::popup::{ConfigureKind, PopupConfigure};
pub use wayland_protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};

/// A new positioner; None without xdg_wm_base.
pub fn positioner(g: &Globals) -> Option<XdgPositioner> {
    XdgPositioner::new(g.xdg_wm_base.as_ref()?).ok()
}

/// Open a popup of `parent`, placed by `positioner`, and send its initial
/// commit. With `grab` (a seat and the serial of the event that opens
/// it), the popup takes the pointer and keyboard. Attach nothing before
/// its first configure.
pub fn layer_popup<P: Program>(
    g: &Globals,
    parent: &LayerSurface,
    positioner: &XdgPositioner,
    grab: Option<(&WlSeat, u32)>,
    qh: &QueueHandle<Shell<P>>,
) -> Result<Popup, String> {
    let wm_base = g.xdg_wm_base.as_ref().ok_or("xdg_wm_base missing")?;
    let surface = g.compositor.create_surface(qh);
    let popup = Popup::from_surface(None, positioner, qh, surface, wm_base)
        .map_err(|e| format!("failed to create a popup: {e}"))?;
    parent.get_popup(popup.xdg_popup());
    if let Some((seat, serial)) = grab {
        popup.xdg_popup().grab(seat, serial);
    }
    popup.wl_surface().commit();
    Ok(popup)
}
