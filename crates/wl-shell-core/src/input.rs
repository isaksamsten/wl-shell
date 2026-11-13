//! Seat state: the keyboard (xkbcommon, with key repeat) and pointer, kept
//! by the shell. Programs get the events through `Program::key`,
//! `Program::modifiers` and `Program::pointer`, and read the state here.

use smithay_client_toolkit::seat::keyboard::Modifiers;
use wayland_client::{
    Proxy,
    protocol::{wl_keyboard::WlKeyboard, wl_pointer::WlPointer, wl_surface::WlSurface},
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;

#[derive(Default)]
pub struct Input {
    pub(crate) keyboard: Option<WlKeyboard>,
    pub(crate) pointer: Option<WlPointer>,
    pub(crate) cursor_shape: Option<WpCursorShapeDeviceV1>,
    /// The surface the pointer is on.
    pub(crate) pointer_focus: Option<WlSurface>,
    /// The last pointer position, in the surface-local coordinates of the
    /// surface it was on.
    pub pointer_pos: (f64, f64),
    /// The keyboard's current modifiers.
    pub modifiers: Modifiers,
}

impl Input {
    /// Whether the pointer is on `surface`.
    pub fn pointer_on(&self, surface: &WlSurface) -> bool {
        self.pointer_focus.as_ref() == Some(surface)
    }

    /// Forget the pointer's surface, e.g. because the program destroyed it:
    /// a new surface gets a fresh enter.
    pub fn reset_pointer(&mut self) {
        self.pointer_focus = None;
    }

    pub(crate) fn release_keyboard(&mut self) {
        if let Some(kb) = self.keyboard.take()
            && kb.version() >= 3
        {
            kb.release();
        }
    }

    pub(crate) fn release_pointer(&mut self) {
        if let Some(device) = self.cursor_shape.take() {
            device.destroy();
        }
        if let Some(pointer) = self.pointer.take()
            && pointer.version() >= 3
        {
            pointer.release();
        }
        self.pointer_focus = None;
    }
}
