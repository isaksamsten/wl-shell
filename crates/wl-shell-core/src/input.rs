//! Seat state: the keyboard (xkbcommon, with key repeat) and pointer, kept
//! by the shell. Programs get the events through `Program::key`,
//! `Program::modifiers` and `Program::pointer`, and read the state here.

use smithay_client_toolkit::seat::keyboard::Modifiers;
use wayland_client::{
    Proxy,
    protocol::{
        wl_keyboard::WlKeyboard, wl_pointer::WlPointer, wl_seat::WlSeat, wl_surface::WlSurface,
    },
};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};

#[derive(Default)]
pub struct Input {
    /// The seat of the pointer (or keyboard), for popup grabs.
    pub(crate) seat: Option<WlSeat>,
    pub(crate) keyboard: Option<WlKeyboard>,
    pub(crate) pointer: Option<WlPointer>,
    pub(crate) cursor_shape: Option<WpCursorShapeDeviceV1>,
    /// The surface the pointer is on.
    pub(crate) pointer_focus: Option<WlSurface>,
    /// The last pointer position, in the surface-local coordinates of the
    /// surface it was on.
    pub pointer_pos: (f64, f64),
    /// The surface with keyboard focus, if it is one of the program's.
    pub(crate) keyboard_focus: Option<WlSurface>,
    /// The keyboard's current modifiers. Only current while one of the
    /// program's surfaces has keyboard focus (`keyboard_focus`).
    pub modifiers: Modifiers,
    /// The serial of the last keyboard enter, key press or button press:
    /// what setting the selection is checked against.
    pub(crate) serial: Option<u32>,
}

impl Input {
    /// The seat input comes from, once it has a pointer or keyboard.
    pub fn seat(&self) -> Option<&WlSeat> {
        self.seat.as_ref()
    }

    /// Whether the pointer is on `surface`.
    pub fn pointer_on(&self, surface: &WlSurface) -> bool {
        self.pointer_focus.as_ref() == Some(surface)
    }

    /// The surface with keyboard focus, if it is one of the program's.
    pub fn keyboard_focus(&self) -> Option<&WlSurface> {
        self.keyboard_focus.as_ref()
    }

    /// The serial of the last keyboard enter, key press or button press.
    pub fn serial(&self) -> Option<u32> {
        self.serial
    }

    /// Show the cursor `shape` on the program's surface the pointer
    /// entered with `serial` (the shell shows the default one on enter).
    /// Nothing without wp_cursor_shape_manager_v1.
    pub fn set_cursor(&self, serial: u32, shape: Shape) {
        if let Some(device) = &self.cursor_shape {
            device.set_shape(serial, shape);
        }
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
        self.keyboard_focus = None;
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
