//! The runtime: [`Shell`] owns the Wayland connection and the state every
//! program needs (globals, outputs, seats, the toplevel list and its focus
//! order), implements all the protocol handlers, and passes what matters on
//! to the [`Program`].
//!
//! A program is the rest: its surfaces, what it shows and how it reacts to
//! input. It never implements Wayland handlers itself. Each hook gets the
//! [`Core`], the shell's half of the state, for creating surfaces and
//! captures, timers, and so on. Event loop callbacks get the whole
//! `Shell`, so they can call into both halves.

use std::rc::Rc;
use std::time::Duration;

use calloop::{
    EventLoop, LoopHandle, RegistrationToken,
    timer::{TimeoutAction, Timer},
};
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::CompositorHandler,
    delegate_dispatch2, delegate_registry,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            PointerEvent, PointerEventKind, PointerHandler, cursor_shape::CursorShapeManager,
        },
    },
    shell::wlr_layer::{LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{
        wl_keyboard::WlKeyboard,
        wl_output::{self, WlOutput},
        wl_pointer::WlPointer,
        wl_seat::WlSeat,
        wl_surface::WlSurface,
    },
};
use wayland_protocols::{
    ext::foreign_toplevel_list::v1::client::{
        ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1,
        ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1,
    },
    wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape,
};
use wl_shell_log::warn;

use crate::capture::{Capture, CaptureEvent, CaptureId};
use crate::focus::FocusStream;
use crate::globals::Globals;
use crate::input::Input;
use crate::mru::Mru;
use crate::toplevel::Toplevel;

pub type Error = Box<dyn std::error::Error>;

/// What a program does with the shell's events. Every hook has a default
/// that ignores the event.
#[allow(unused_variables)]
pub trait Program: Sized + 'static {
    /// A window appeared. Its state (`Core::toplevel`) arrives with it.
    fn toplevel_new(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {}

    /// A window's title or app ID changed.
    fn toplevel_update(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {}

    /// A window closed. It is gone from the focus order already, but
    /// `Core::toplevel` still knows it.
    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {}

    /// The compositor closed one of the program's layer surfaces.
    fn layer_closed(&mut self, core: &mut Core<Self>, layer: &LayerSurface) {}

    fn layer_configure(
        &mut self,
        core: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
    }

    fn scale_factor_changed(&mut self, core: &mut Core<Self>, surface: &WlSurface, factor: i32) {}

    /// A frame callback of `surface` fired.
    fn frame(&mut self, core: &mut Core<Self>, surface: &WlSurface) {}

    /// `surface` got keyboard focus.
    fn keyboard_enter(&mut self, core: &mut Core<Self>, surface: &WlSurface) {}

    /// `surface` lost keyboard focus.
    fn keyboard_leave(&mut self, core: &mut Core<Self>, surface: &WlSurface) {}

    /// A key press; `repeat` for key repeats (client- or compositor-side).
    /// The current modifiers are in `core.input.modifiers`.
    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {}

    /// The modifiers changed; `core.input.modifiers` has them already.
    fn modifiers(&mut self, core: &mut Core<Self>, modifiers: &Modifiers) {}

    /// A pointer event. `core.input` tracks the pointer's surface and
    /// position already, and the cursor is set on enter.
    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {}

    /// An event for capture `id`: if the program has that capture, call
    /// `f` with it and handle what it returns. The program owns its
    /// captures; events for captures it dropped find nothing.
    fn capture_update(
        &mut self,
        core: &mut Core<Self>,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
    }
}

/// The shell's half of the state.
pub struct Core<P> {
    pub conn: Connection,
    pub qh: QueueHandle<Shell<P>>,
    pub loop_handle: LoopHandle<'static, Shell<P>>,
    pub g: Rc<Globals>,
    registry: RegistryState,
    pub seat_state: SeatState,
    pub output_state: OutputState,
    pub toplevel_list: ForeignToplevelList,
    cursor_shape: Option<CursorShapeManager>,
    pub input: Input,
    /// Focus order, fed by `focus_stream`.
    pub mru: Mru,
    /// The `mru-event-stream` command, while configured.
    pub focus_stream: FocusStream,
}

/// The whole state: what the event loop dispatches to.
pub struct Shell<P> {
    pub core: Core<P>,
    pub program: P,
}

impl<P: Program> Shell<P> {
    /// Connect, bind the globals, and receive the window list. Insert the
    /// connection into the returned event loop, which dispatches to the
    /// returned shell.
    pub fn connect(program: P) -> Result<(Self, EventLoop<'static, Self>), Error> {
        let conn = Connection::connect_to_env()
            .map_err(|e| format!("failed to connect to wayland display: {e}"))?;
        let (globals, mut queue) = registry_queue_init::<Self>(&conn)?;
        let qh = queue.handle();
        require_toplevel_list(&globals)?;
        // Globals::bind reports each missing global.
        let g = Rc::new(Globals::bind(&globals, &qh).ok_or("required globals missing")?);
        let event_loop: EventLoop<Self> = EventLoop::try_new()?;
        let core = Core {
            conn: conn.clone(),
            qh: qh.clone(),
            loop_handle: event_loop.handle(),
            g,
            registry: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            toplevel_list: ForeignToplevelList::new(&globals, &qh),
            cursor_shape: CursorShapeManager::bind(&globals, &qh).ok(),
            input: Input::default(),
            mru: Mru::default(),
            focus_stream: FocusStream::default(),
        };
        let mut shell = Shell { core, program };
        // The first roundtrip delivers the handles, the second their state.
        queue.roundtrip(&mut shell)?;
        queue.roundtrip(&mut shell)?;
        WaylandSource::new(conn, queue).insert(event_loop.handle())?;
        Ok((shell, event_loop))
    }
}

/// Fail unless the compositor has the ext toplevel list.
pub(crate) fn require_toplevel_list(
    globals: &wayland_client::globals::GlobalList,
) -> Result<(), Error> {
    let name = ExtForeignToplevelListV1::interface().name;
    let found = (globals.contents()).with_list(|list| list.iter().any(|g| g.interface == name));
    if !found {
        return Err(format!("missing global: {name}").into());
    }
    Ok(())
}

impl<P: Program> Core<P> {
    /// The state of the window `handle`, if it has any yet.
    pub fn toplevel(&self, handle: &ExtForeignToplevelHandleV1) -> Option<Toplevel> {
        let info = self.toplevel_list.info(handle)?;
        Some(Toplevel {
            identifier: info.identifier,
            app_id: info.app_id,
            title: info.title,
        })
    }

    /// Open windows with their state, in announcement order.
    pub fn toplevels(&self) -> Vec<(ExtForeignToplevelHandleV1, Toplevel)> {
        (self.toplevel_list.toplevels().iter())
            .filter_map(|handle| Some((handle.clone(), self.toplevel(handle)?)))
            .collect()
    }

    /// The scale of every output, deduplicated and sorted; `[1]` without
    /// outputs.
    pub fn scales(&self) -> Vec<i32> {
        let mut scales: Vec<i32> = (self.output_state.outputs())
            .filter_map(|o| self.output_state.info(&o))
            .map(|info| info.scale_factor.max(1))
            .collect();
        scales.sort_unstable();
        scales.dedup();
        if scales.is_empty() {
            scales.push(1);
        }
        scales
    }

    /// The output named `name` and its scale.
    pub fn output_named(&self, name: &str) -> Option<(WlOutput, i32)> {
        (self.output_state.outputs()).find_map(|o| {
            let info = self.output_state.info(&o)?;
            (info.name.as_deref() == Some(name)).then(|| (o, info.scale_factor.max(1)))
        })
    }

    /// Call `f` once after `after`. Remove the returned token to cancel.
    pub fn timer(
        &self,
        after: Duration,
        f: impl FnOnce(&mut P, &mut Core<P>) + 'static,
    ) -> Result<RegistrationToken, String> {
        let mut f = Some(f);
        self.loop_handle
            .insert_source(Timer::from_duration(after), move |_, _, shell| {
                if let Some(f) = f.take() {
                    f(&mut shell.program, &mut shell.core);
                }
                TimeoutAction::Drop
            })
            .map_err(|e| format!("failed to start a timer: {e}"))
    }

    /// Run the `mru-event-stream` command `command`: start it, restart it
    /// if it changed, or stop it if None.
    pub fn use_focus_stream(&mut self, command: Option<&str>) {
        let handle = self.loop_handle.clone();
        self.focus_stream.configure(command, &handle);
    }
}

// ---------------------------------------------------------------------------
// handlers

impl<P: Program> ForeignToplevelListHandler for Shell<P> {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        &mut self.core.toplevel_list
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: ExtForeignToplevelHandleV1,
    ) {
        self.program.toplevel_new(&mut self.core, &handle);
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: ExtForeignToplevelHandleV1,
    ) {
        self.program.toplevel_update(&mut self.core, &handle);
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        handle: ExtForeignToplevelHandleV1,
    ) {
        if let Some(info) = self.core.toplevel_list.info(&handle) {
            self.core.mru.forget(&info.identifier);
        }
        self.program.toplevel_closed(&mut self.core, &handle);
    }
}

impl<P: Program> LayerShellHandler for Shell<P> {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        self.program.layer_closed(&mut self.core, layer);
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        self.program
            .layer_configure(&mut self.core, layer, configure);
    }
}

impl<P: Program> CompositorHandler for Shell<P> {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: i32,
    ) {
        self.program
            .scale_factor_changed(&mut self.core, surface, factor);
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surface: &WlSurface, _: u32) {
        self.program.frame(&mut self.core, surface);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl<P: Program> OutputHandler for Shell<P> {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.core.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl<P: Program> SeatHandler for Shell<P> {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.core.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: WlSeat,
        cap: Capability,
    ) {
        let core = &mut self.core;
        match cap {
            Capability::Keyboard if core.input.keyboard.is_none() => {
                let repeat = Box::new(|shell: &mut Self, _: &WlKeyboard, event: KeyEvent| {
                    shell.program.key(&mut shell.core, &event, true);
                });
                let loop_handle = core.loop_handle.clone();
                match (core.seat_state).get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    loop_handle,
                    repeat,
                ) {
                    Ok(kb) => core.input.keyboard = Some(kb),
                    Err(e) => warn!("keyboard unavailable: {e}"),
                }
            }
            Capability::Pointer if core.input.pointer.is_none() => {
                match core.seat_state.get_pointer(qh, &seat) {
                    Ok(pointer) => {
                        core.input.cursor_shape =
                            (core.cursor_shape.as_ref()).map(|m| m.get_shape_device(&pointer, qh));
                        core.input.pointer = Some(pointer);
                    }
                    Err(e) => warn!("pointer unavailable: {e}"),
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: WlSeat,
        cap: Capability,
    ) {
        match cap {
            Capability::Keyboard => self.core.input.release_keyboard(),
            Capability::Pointer => self.core.input.release_pointer(),
            _ => {}
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl<P: Program> KeyboardHandler for Shell<P> {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.program.keyboard_enter(&mut self.core, surface);
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        surface: &WlSurface,
        _: u32,
    ) {
        self.program.keyboard_leave(&mut self.core, surface);
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.program.key(&mut self.core, &event, false);
    }

    /// Compositor-side repeat (wl_keyboard v10).
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.program.key(&mut self.core, &event, true);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.core.input.modifiers = modifiers;
        self.program.modifiers(&mut self.core, &modifiers);
    }
}

impl<P: Program> PointerHandler for Shell<P> {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let input = &mut self.core.input;
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    input.pointer_focus = Some(event.surface.clone());
                    input.pointer_pos = event.position;
                    if let Some(device) = &input.cursor_shape {
                        device.set_shape(serial, Shape::Default);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    if input.pointer_focus.as_ref() == Some(&event.surface) {
                        input.pointer_focus = None;
                    }
                }
                PointerEventKind::Motion { .. } => input.pointer_pos = event.position,
                _ => {}
            }
            self.program.pointer(&mut self.core, event);
        }
    }
}

impl<P: Program> ProvidesRegistryState for Shell<P> {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.core.registry
    }

    registry_handlers![OutputState, SeatState];
}

delegate_registry!(@<P: Program> Shell<P>);
delegate_dispatch2!(@<P: Program> Shell<P>);
