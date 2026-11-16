//! Drag and drop, and the clipboard, through `wl_data_device`, for
//! programs that set `Program::DATA_DEVICE`: a data device for the seat,
//! the offer of a drag over one of the program's surfaces, the program's
//! own drags, and the selection it sets.
//!
//! Dropping on the program: `Program::dnd_enter` gets the offered mime
//! types, then `dnd_motion`, and `dnd_leave` or `dnd_drop`. The program
//! reads the data whenever it likes (`Core::dnd_receive`, without
//! blocking: a drag's data may be read before the drop to decide whether
//! to take it), says what it takes (`Dnd::accept`), and once it used a
//! drop, finishes it (`Dnd::finish`). `dnd_action` reports the action the
//! compositor picked from the source's and the program's.
//!
//! Dragging from the program: `Core::start_drag` offers data of a few mime
//! types from a surface the pointer pressed on, with an optional icon.
//! The data is written on a thread when asked for, so a drop on the
//! program itself can't block it. `Program::drag_source_done` reports how
//! it ended.
//!
//! Copying: `Core::set_selection` puts data of a few mime types on the
//! clipboard, as of the last input event (a key or button press, or the
//! keyboard entering one of the program's surfaces), which the compositor
//! may check is the focus's. The program keeps the data, and writes it
//! as above, until another client sets the selection.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;

use calloop::PostAction;
use smithay_client_toolkit::data_device_manager::{
    DataDeviceManagerState, WritePipe,
    data_device::{DataDevice, DataDeviceHandler},
    data_offer::{DataOfferHandler, DragOffer},
    data_source::{CopyPasteSource, DataSourceHandler, DragSource},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    protocol::{
        wl_buffer::WlBuffer, wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
        wl_data_source::WlDataSource, wl_seat::WlSeat, wl_surface::WlSurface,
    },
};
use wl_shell_log::warn;

use crate::shell::{Core, Program, Shell};

/// Data read from an offer at most.
const MAX_DATA: usize = 1 << 22;

/// The drag and drop state of the shell.
pub struct Dnd {
    manager: Option<DataDeviceManagerState>,
    device: Option<DataDevice>,
    /// The action the compositor picked for the offer over us.
    action: DndAction,
    /// The offer over us was dropped (it lives until it is finished).
    dropped: bool,
    /// The program's own drag.
    source: Option<Source>,
    /// The selection the program set, until another replaces it.
    selection: Option<Selection>,
}

/// The data the program put on the clipboard.
struct Selection {
    source: CopyPasteSource,
    data: Vec<(String, Vec<u8>)>,
}

/// One of the program's drags.
struct Source {
    source: DragSource,
    icon: Option<WlSurface>,
    data: Vec<(String, Vec<u8>)>,
    action: DndAction,
}

/// The icon a drag shows under the pointer.
pub struct DragIcon<'a> {
    pub buffer: &'a WlBuffer,
    /// The buffer's scale.
    pub scale: i32,
    /// Where the pointer is on the icon, logical pixels.
    pub hotspot: (i32, i32),
}

impl Dnd {
    pub(crate) fn new(manager: Option<DataDeviceManagerState>) -> Self {
        Self {
            manager,
            device: None,
            action: DndAction::empty(),
            dropped: false,
            source: None,
            selection: None,
        }
    }

    /// Whether there is a data device: drags can come in and go out.
    pub fn available(&self) -> bool {
        self.device.is_some()
    }

    /// Get a data device for `seat`, once.
    pub(crate) fn use_seat<P: Program>(&mut self, seat: &WlSeat, qh: &QueueHandle<Shell<P>>) {
        if let (Some(manager), None) = (&self.manager, &self.device) {
            self.device = Some(manager.get_data_device(qh, seat));
        }
    }

    pub(crate) fn release(&mut self) {
        self.device = None;
        self.source = None;
        self.selection = None;
    }

    /// The offer of the drag over one of the program's surfaces (or
    /// dropped there and not finished yet).
    fn offer(&self) -> Option<DragOffer> {
        self.device.as_ref()?.data().drag_offer()
    }

    /// The mime types the drag over the program offers.
    pub fn mime_types(&self) -> Vec<String> {
        (self.offer()).map_or_else(Vec::new, |o| o.with_mime_types(|m| m.to_vec()))
    }

    /// The actions the drag's source supports.
    pub fn source_actions(&self) -> DndAction {
        self.offer()
            .map_or(DndAction::empty(), |o| o.source_actions)
    }

    /// The action the compositor picked: what a drop does.
    pub fn action(&self) -> DndAction {
        self.action
    }

    /// Take the drag as `mime` with one of `actions`, preferring
    /// `preferred`; None refuses it.
    pub fn accept(&self, mime: Option<&str>, actions: DndAction, preferred: DndAction) {
        let Some(offer) = self.offer() else { return };
        if offer.left && !offer.dropped {
            return;
        }
        offer.accept_mime_type(offer.serial, mime.map(str::to_string));
        match mime {
            Some(_) => offer.set_actions(actions, preferred),
            None => offer.set_actions(DndAction::empty(), DndAction::empty()),
        }
    }

    /// The dropped data was used (or not): the source may clean up.
    pub fn finish(&mut self) {
        if let Some(offer) = self.offer()
            && std::mem::take(&mut self.dropped)
        {
            if offer.inner().version() >= 3 && !self.action.is_empty() {
                offer.finish();
            }
            offer.destroy();
        }
    }

    /// Whether the program's own drag is going on.
    pub fn dragging(&self) -> bool {
        self.source.is_some()
    }
}

impl<P: Program> Core<P> {
    /// Read the drag's data as `mime`; `f` gets it once the source wrote
    /// it all (empty if reading failed). Never blocks.
    pub fn dnd_receive(
        &mut self,
        mime: &str,
        f: impl FnOnce(&mut P, &mut Core<P>, Vec<u8>) + 'static,
    ) -> std::io::Result<()> {
        let offer = (self.dnd.offer()).ok_or_else(|| std::io::Error::other("no drag"))?;
        let pipe = offer.receive(mime.to_string())?;
        // The source only writes once it got the request.
        let _ = self.conn.flush();
        let mut data = Vec::new();
        let mut f = Some(f);
        let mut done = move |shell: &mut Shell<P>, data: Vec<u8>| {
            if let Some(f) = f.take() {
                f(&mut shell.program, &mut shell.core, data);
            }
        };
        self.loop_handle
            .insert_source(pipe, move |(), file, shell| {
                let mut chunk = [0u8; 8192];
                let mut file: &File = file;
                match file.read(&mut chunk) {
                    Ok(0) => {
                        done(shell, std::mem::take(&mut data));
                        PostAction::Remove
                    }
                    Ok(n) if data.len() + n > MAX_DATA => {
                        warn!("drag data too large");
                        done(shell, Vec::new());
                        PostAction::Remove
                    }
                    Ok(n) => {
                        data.extend_from_slice(&chunk[..n]);
                        PostAction::Continue
                    }
                    Err(e)
                        if matches!(e.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
                    {
                        PostAction::Continue
                    }
                    Err(e) => {
                        warn!("reading drag data: {e}");
                        done(shell, Vec::new());
                        PostAction::Remove
                    }
                }
            })
            .map(|_| ())
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Start dragging `data` (mime types and their bytes) from `origin`,
    /// which the press with `serial` went down on, for `actions`. Any
    /// drag of the program's still going is cancelled.
    pub fn start_drag(
        &mut self,
        origin: &WlSurface,
        serial: u32,
        data: Vec<(String, Vec<u8>)>,
        actions: DndAction,
        icon: Option<DragIcon>,
    ) -> Result<(), String> {
        let (Some(manager), Some(device)) = (&self.dnd.manager, &self.dnd.device) else {
            return Err("the compositor has no data device manager".into());
        };
        let mimes = data.iter().map(|(m, _)| m.clone());
        let source = manager.create_drag_and_drop_source(&self.qh, mimes, actions);
        let icon = icon.map(|icon| {
            let surface = self.g.compositor.create_surface(&self.qh);
            surface.set_buffer_scale(icon.scale.max(1));
            let (x, y) = (-icon.hotspot.0, -icon.hotspot.1);
            match surface.version() >= 5 {
                true => {
                    surface.attach(Some(icon.buffer), 0, 0);
                    surface.offset(x, y);
                }
                false => surface.attach(Some(icon.buffer), x, y),
            }
            surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            surface.commit();
            surface
        });
        source.start_drag(device, origin, icon.as_ref(), serial);
        if let Some(old) = self.dnd.source.replace(Source {
            source,
            icon,
            data,
            action: DndAction::empty(),
        }) {
            drop_source(old);
        }
        Ok(())
    }

    /// Put `data` (mime types and their bytes) on the clipboard, as of the
    /// last input event. The program's earlier selection goes.
    pub fn set_selection(&mut self, data: Vec<(String, Vec<u8>)>) -> Result<(), String> {
        let (Some(manager), Some(device)) = (&self.dnd.manager, &self.dnd.device) else {
            return Err("the compositor has no data device manager".into());
        };
        let serial = (self.input.serial()).ok_or("no input event to copy with")?;
        let mimes = data.iter().map(|(m, _)| m.clone());
        let source = manager.create_copy_paste_source(&self.qh, mimes);
        source.set_selection(device, serial);
        // Dropping the old CopyPasteSource destroys it.
        self.dnd.selection = Some(Selection { source, data });
        Ok(())
    }
}

fn drop_source(source: Source) {
    if let Some(icon) = source.icon {
        icon.destroy();
    }
    // Dropping the DragSource destroys it.
}

impl<P: Program> Shell<P> {
    /// The program's drag ended; `action` is what the drop did (None:
    /// cancelled).
    fn source_done(&mut self, source: &WlDataSource, dropped: bool) {
        let ours = (self.core.dnd.source.as_ref()).is_some_and(|s| s.source.inner() == source);
        if !ours {
            return source.destroy();
        }
        let Some(s) = self.core.dnd.source.take() else {
            return;
        };
        let action = dropped.then_some(s.action);
        drop_source(s);
        self.program.drag_source_done(&mut self.core, action);
    }
}

impl<P: Program> DataDeviceHandler for Shell<P> {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        x: f64,
        y: f64,
        surface: &WlSurface,
    ) {
        self.core.dnd.action = DndAction::empty();
        self.core.dnd.dropped = false;
        let mimes = self.core.dnd.mime_types();
        self.program
            .dnd_enter(&mut self.core, surface, (x, y), &mimes);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.program.dnd_leave(&mut self.core);
    }

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, x: f64, y: f64) {
        self.program.dnd_motion(&mut self.core, (x, y));
    }

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        self.core.dnd.dropped = true;
        self.program.dnd_drop(&mut self.core);
    }
}

impl<P: Program> DataOfferHandler for Shell<P> {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
        // Read from the offer when needed. The offer is locked while
        // this runs, so nothing here may touch it.
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        action: DndAction,
    ) {
        if action == self.core.dnd.action {
            return;
        }
        self.core.dnd.action = action;
        // The offer is locked while this runs: tell the program once it
        // isn't, so it can answer.
        self.core.loop_handle.insert_idle(move |shell| {
            if shell.core.dnd.action == action {
                shell.program.dnd_action(&mut shell.core, action);
            }
        });
    }
}

impl<P: Program> DataSourceHandler for Shell<P> {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        fd: WritePipe,
    ) {
        let dnd = &self.core.dnd;
        let drag = (dnd.source.as_ref())
            .filter(|s| s.source.inner() == source)
            .map(|s| &s.data);
        let copied = (dnd.selection.as_ref())
            .filter(|s| s.source.inner() == source)
            .map(|s| &s.data);
        let bytes = (drag.or(copied))
            .and_then(|data| data.iter().find(|(m, _)| *m == mime))
            .map(|(_, b)| b.clone());
        let Some(bytes) = bytes else { return };
        // On a thread: the reader may be this very program, which reads
        // from its event loop.
        let fd = OwnedFd::from(fd);
        let spawned = std::thread::Builder::new()
            .name("data-send".into())
            .spawn(move || {
                let mut file = File::from(fd);
                if let Err(e) = file.write_all(&bytes)
                    && e.kind() != ErrorKind::BrokenPipe
                {
                    warn!("sending data: {e}");
                }
            });
        if let Err(e) = spawned {
            warn!("sending data: {e}");
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        // Another client set the selection: ours goes.
        let dnd = &mut self.core.dnd;
        if dnd
            .selection
            .as_ref()
            .is_some_and(|s| s.source.inner() == source)
        {
            dnd.selection = None;
            return;
        }
        self.source_done(source, false);
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        self.source_done(source, true);
    }

    fn action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &WlDataSource,
        action: DndAction,
    ) {
        if let Some(s) = &mut self.core.dnd.source
            && s.source.inner() == source
        {
            s.action = action;
        }
    }
}
