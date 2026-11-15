//! wl-dock as a wl-shell program: the config, the pins, the items, one
//! dock per output, and how they react to the shell's events.
//!
//! Docks follow the outputs: one is created for every output with a name
//! that the config gives a dock (`outputs`, `[output.<name>]`), restyled
//! when the config changes, and destroyed with its output. The config file
//! and the pins file are watched with inotify and the desktop's color
//! scheme followed; a config error keeps the previous config.
//!
//! Each dock gets its own items (dock.rs): the pins, then the running
//! apps, with the windows on its output if it has `windows = "output"`
//! and the `output-event-stream` runs. Clicking activates a window with
//! `activate-command` or launches the app from its desktop entry (looked
//! up by the worker), and a launched app's icon bounces until one of its
//! windows appears.
//!
//! The worker renders icons and labels for all docks into one cache.
//!
//! Clicking an app with several windows opens its strip (view.rs,
//! stripview.rs): a grabbing popup of the dock with a card per window,
//! which takes the keyboard. The dock owns the strip's captures; clicking
//! a card (or Enter) activates its window. Ctrl+click activates the
//! app's most recent window instead: the dock surface takes keyboard
//! focus on clicks, so the modifiers are known by the button's release.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use smithay_client_toolkit::{
    seat::{
        keyboard::KeyEvent,
        pointer::{BTN_LEFT, BTN_MIDDLE, PointerEvent, PointerEventKind},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerSurface, LayerSurfaceConfigure},
        xdg::popup::{Popup, PopupConfigure},
    },
};
use wayland_client::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::Reloader;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::Globals;
use wl_shell_core::reexports::calloop::{RegistrationToken, channel};
use wl_shell_core::spawn::{self, Output};
use wl_shell_core::stream::{Hooks, LineStream};
use wl_shell_core::toplevel::Toplevel;
use wl_shell_core::{Core, Program, Shell};
use wl_shell_log::warn;
use wl_shell_render::icons::AppEntry;
use wl_shell_render::worker::{Assets, Done};
use wl_shell_ui::Cache;

use crate::config::{Config, Windows};
use crate::dock::{self, Click, Item, Order, Placement};
use crate::launch::{self, Command};
use crate::pins::{self, Pin};
use crate::strip::Key;
use crate::view::{DockView, Style};

/// An app launched from the dock, until one of its windows appears.
struct Launch {
    app_id: String,
    /// The app's windows when it was launched: a new one ends the launch.
    before: Vec<String>,
    /// Ends it after `launch-timeout`.
    timeout: Option<RegistrationToken>,
}

/// A button that went down on an app's icon: it is clicked if the button
/// comes up on it too.
#[derive(Clone, Debug, PartialEq)]
struct Pressed {
    output: WlOutput,
    app_id: String,
    button: u32,
    /// The press's serial, for the strip's grab.
    serial: u32,
}

pub struct WlDock {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The config in use.
    config: Config,
    /// The config file was touched since it was last read.
    config_touched: bool,
    /// The pins file; None without a state directory.
    pins_path: Option<PathBuf>,
    pins: Vec<Pin>,
    /// The pins file was touched since it was last read.
    pins_touched: bool,
    /// The worker, started with the dock.
    assets: Option<Rc<Assets>>,
    /// Icons, labels and shapes, shared by the docks.
    cache: Rc<RefCell<Cache>>,
    /// One per output that has a dock.
    docks: Vec<DockView>,
    /// The running apps, in the order their first window appeared.
    order: Order,
    /// Every app ID seen, to notice new apps (which may come with a new
    /// desktop entry).
    seen: Vec<String>,
    /// Which output each window is on, from the output stream.
    placement: Placement,
    output_stream: LineStream<Self>,
    /// Desktop entries from the worker, by app ID; None: it has none.
    entries: HashMap<String, Option<AppEntry>>,
    /// Apps to launch once their desktop entry arrives.
    pending_launches: Vec<String>,
    launching: Vec<Launch>,
    /// The app a button went down on.
    pressed: Option<Pressed>,
    /// Warned that `activate-command` is missing.
    warned_activate: bool,
    /// The items may have changed since they were last built.
    dirty: bool,
    /// The outputs changed since the docks were matched to them.
    outputs_dirty: bool,
    /// Buffers in the cache were replaced: once no dock shows them, they
    /// can go.
    trim: bool,
    /// The cache was cleared: docks get their shapes again.
    reload: bool,
}

impl WlDock {
    /// Read the config (`explicit`, or the default file) and the pins.
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        let reloader = Reloader::<Config>::load(explicit).map_err(|e| format!("config: {e}"))?;
        let pins_path = pins::path();
        let pins = match &pins_path {
            Some(path) => pins::read(path).unwrap_or_else(|e| {
                warn!("{}: {e}", path.display());
                Vec::new()
            }),
            None => Vec::new(),
        };
        Ok(Self {
            config: reloader.config().clone(),
            config_touched: false,
            reloader,
            pins_path,
            pins,
            pins_touched: false,
            assets: None,
            cache: Rc::new(RefCell::new(Cache::default())),
            docks: Vec::new(),
            order: Order::default(),
            seen: Vec::new(),
            placement: Placement::default(),
            output_stream: LineStream::new(
                "output-event-stream",
                Hooks {
                    locate: |shell: &mut Shell<Self>| &mut shell.program.output_stream,
                    line: |shell, line| shell.program.output_line(line),
                    changed: |shell| shell.program.dirty = true,
                },
            ),
            entries: HashMap::new(),
            pending_launches: Vec::new(),
            launching: Vec::new(),
            pressed: None,
            warned_activate: false,
            dirty: true,
            outputs_dirty: true,
            trim: false,
            reload: false,
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker and the streams, watch the files, and create the docks.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.start_worker(core)?;
        self.watch_config(core);
        self.watch_pins(core);
        self.use_streams(core);
        self.prefetch_entries();
        self.after_dispatch(core);
        Ok(())
    }

    fn start_worker(&mut self, core: &Core<Self>) -> Result<(), String> {
        let (assets, results) =
            Assets::spawn().map_err(|e| format!("failed to start the worker: {e}"))?;
        core.loop_handle
            .insert_source(results, |event, _, shell: &mut Shell<Self>| {
                if let channel::Event::Msg(done) = event {
                    shell.program.worker_done(&shell.core, done);
                }
            })
            .map_err(|e| format!("failed to watch the worker: {e}"))?;
        self.assets = Some(Rc::new(assets));
        Ok(())
    }

    /// Reload the config when its file changes. Without its directory,
    /// there is nothing to watch (and nothing to reload).
    fn watch_config(&mut self, core: &Core<Self>) {
        let Some(path) = self.reloader.path() else {
            return;
        };
        let watched =
            wl_shell_config::watch_file(&core.loop_handle, &path, |shell: &mut Shell<Self>| {
                shell.program.config_touched = true;
            });
        if let Err(e) = watched
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!("not watching {}: {e}", path.display());
        }
    }

    /// Reload the pins when their file changes. Its directory is created,
    /// so pins added later by `wl-dock pin` are seen.
    fn watch_pins(&mut self, core: &Core<Self>) {
        let Some(path) = self.pins_path.clone() else {
            return warn!("no state directory: pins are not kept");
        };
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            return warn!("{}: {e}", dir.display());
        }
        let watched =
            wl_shell_config::watch_file(&core.loop_handle, &path, |shell: &mut Shell<Self>| {
                shell.program.pins_touched = true;
            });
        if let Err(e) = watched {
            warn!("not watching {}: {e}", path.display());
        }
    }

    /// Run the streams the config names (restarting changed ones).
    fn use_streams(&mut self, core: &mut Core<Self>) {
        core.use_focus_stream(self.config.mru_event_stream.as_deref());
        let handle = core.loop_handle.clone();
        (self.output_stream).configure(self.config.output_event_stream.as_deref(), &handle);
    }

    /// After every event loop dispatch: follow config, pins and theme
    /// changes, match the docks to the outputs, and rebuild the items
    /// once for however many events arrived.
    pub fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let touched = std::mem::take(&mut self.config_touched);
        if (touched || self.reloader.scheme_changed())
            && (self.reloader).refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.config_changed(core);
        }
        if std::mem::take(&mut self.pins_touched) {
            self.reload_pins();
        }
        if std::mem::take(&mut self.outputs_dirty) {
            self.sync_docks(core);
            self.dirty = true;
        }
        if std::mem::take(&mut self.dirty) {
            self.rebuild(core, None);
        }
        if std::mem::take(&mut self.trim) {
            // Draw now, so no dock shows a replaced buffer anymore.
            for dock in &mut self.docks {
                dock.render();
            }
            self.cache.borrow_mut().trim();
        }
    }

    /// The config (or the scheme in use) changed: forget what was rendered
    /// with the old one and restyle the docks.
    fn config_changed(&mut self, core: &mut Core<Self>) {
        self.config = self.reloader.config().clone();
        let mut cache = self.cache.borrow_mut();
        cache.clear_icons();
        cache.clear_shapes();
        drop(cache);
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
        self.use_streams(core);
        self.outputs_dirty = true;
        self.reload = true;
        self.trim = true;
    }

    fn reload_pins(&mut self) {
        let Some(path) = &self.pins_path else { return };
        match pins::read(path) {
            Ok(pins) if pins != self.pins => {
                self.pins = pins;
                self.dirty = true;
                self.prefetch_entries();
            }
            Ok(_) => {}
            Err(e) => warn!("{}: {e}", path.display()),
        }
    }

    /// Have the worker look up the pinned apps' desktop entries, so a
    /// click launches them at once.
    fn prefetch_entries(&self) {
        let Some(assets) = &self.assets else { return };
        for app_id in pins::apps(&self.pins) {
            if !self.entries.contains_key(&app_id) {
                assets.entry(&app_id);
            }
        }
    }

    /// The dock on `output`.
    pub fn dock_mut(&mut self, output: &WlOutput) -> Option<&mut DockView> {
        self.docks.iter_mut().find(|d| d.output == *output)
    }

    fn dock_on(&mut self, surface: &WlSurface) -> Option<&mut DockView> {
        self.docks.iter_mut().find(|d| d.surface() == surface)
    }

    /// Create, restyle and destroy docks to match the outputs and the
    /// config.
    fn sync_docks(&mut self, core: &Core<Self>) {
        let Some(assets) = self.assets.clone() else {
            return; // not started yet
        };
        let reload = std::mem::take(&mut self.reload);
        let wanted: Vec<(WlOutput, String, i32, Style)> = (core.output_state.outputs())
            .filter_map(|output| {
                let info = core.output_state.info(&output)?;
                let name = info.name?;
                let layout = self.config.dock(&name)?;
                let style = Style::new(&self.config, layout);
                Some((output, name, info.scale_factor.max(1), style))
            })
            .collect();
        self.docks.retain(|dock| {
            (wanted.iter()).any(|(o, name, _, style)| {
                *o == dock.output && *name == dock.name && !dock.style().needs_new_surface(style)
            })
        });
        for (output, name, scale, style) in wanted {
            match self.docks.iter_mut().find(|d| d.output == output) {
                Some(dock) => {
                    dock.set_output_size(Self::output_size(core, &output));
                    dock.set_style(style, reload);
                    dock.set_scale(scale);
                }
                None => match DockView::new(
                    core,
                    output,
                    name,
                    scale,
                    style,
                    self.cache.clone(),
                    assets.clone(),
                ) {
                    Ok(mut dock) => {
                        dock.set_output_size(Self::output_size(core, &dock.output.clone()));
                        let apps: Vec<String> =
                            self.launching.iter().map(|l| l.app_id.clone()).collect();
                        dock.set_launching(&apps);
                        self.docks.push(dock);
                    }
                    Err(e) => warn!("failed to create a dock: {e}"),
                },
            }
        }
    }

    /// `output`'s logical size, if known.
    fn output_size(core: &Core<Self>, output: &WlOutput) -> Option<(i32, i32)> {
        core.output_state.info(output)?.logical_size
    }

    /// The open windows, leaving out `closed` (the list still knows a
    /// window while its close is handled).
    fn toplevels(core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) -> Vec<Toplevel> {
        (core.toplevels().into_iter())
            .filter(|(h, _)| Some(h) != closed)
            .map(|(_, t)| t)
            .collect()
    }

    /// Rebuild every dock's items from the window list and the pins.
    fn rebuild(&mut self, core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) {
        let toplevels = Self::toplevels(core, closed);
        self.order.update(&toplevels);
        // An app not seen before may come with a new desktop entry.
        let mut new_app = false;
        for t in &toplevels {
            if !self.seen.contains(&t.app_id) {
                self.seen.push(t.app_id.clone());
                new_app = true;
            }
        }
        if new_app && let Some(assets) = &self.assets {
            assets.refresh();
        }
        self.end_launches(core, &toplevels);

        let pins = pins::apps(&self.pins);
        let focused = (self.config.active_indicator && core.focus_stream.running())
            .then(|| core.mru.first())
            .flatten();
        let streaming = self.output_stream.running();
        for dock in &mut self.docks {
            let by_output = streaming && dock.style().layout.windows == Windows::Output;
            let items = dock::items(&toplevels, &pins, &self.order, focused, |id| {
                self.placement.place(id, &dock.name, by_output)
            });
            dock.set_items(core, &items);
        }
    }

    /// The items of the dock on `output`'s hovered app, if any.
    fn hovered_item(&self, output: &WlOutput) -> Option<Item> {
        let dock = self.docks.iter().find(|d| d.output == *output)?;
        dock.hovered_item().cloned()
    }

    // -- the output stream ---------------------------------------------------

    fn output_line(&mut self, line: &str) {
        if let Some((output, id)) = dock::parse_output_line(line)
            && self.placement.report(output, id)
        {
            self.dirty = true;
        }
    }

    /// A new window: unless the stream has placed it already, keep it off
    /// the docks that show their output's windows for `output-grace`.
    fn hold_new_window(&mut self, core: &Core<Self>, id: &str) {
        let by_output = (self.docks.iter()).any(|d| d.style().layout.windows == Windows::Output);
        if !by_output || !self.output_stream.running() || self.placement.placed(id) {
            return;
        }
        self.placement.hold(id);
        let grace = Duration::from_millis(self.config.output_grace.into());
        let held = id.to_string();
        let timer = core.timer(grace, move |app: &mut Self, _| {
            if app.placement.release(&held) {
                app.dirty = true;
            }
        });
        if let Err(e) = timer {
            warn!("{e}");
            self.placement.release(id);
        }
    }

    // -- clicks ----------------------------------------------------------------

    fn button(
        &mut self,
        core: &Core<Self>,
        output: &WlOutput,
        button: u32,
        serial: u32,
        down: bool,
    ) {
        let Some(item) = self.hovered_item(output) else {
            self.pressed = None;
            // A click on the dock between icons is a click outside the
            // strip.
            if !down {
                self.close_strips(core);
            }
            return;
        };
        if down {
            self.pressed = Some(Pressed {
                output: output.clone(),
                app_id: item.app_id,
                button,
                serial,
            });
            return;
        }
        // Clicks act on release, on the icon the button went down on.
        let Some(pressed) = self.pressed.take() else {
            return;
        };
        if pressed.output != *output || pressed.app_id != item.app_id || pressed.button != button {
            return;
        }
        match button {
            BTN_LEFT => self.click(core, output, &item, pressed.serial),
            // A new instance (or window), as the app does it.
            BTN_MIDDLE => {
                self.close_strips(core);
                self.launch(core, &item.app_id);
                self.release_keyboard(output);
            }
            _ => {}
        }
    }

    /// A left click on `item` on the dock on `output`, pressed with
    /// `serial`: close its strip if open, else open it if it gets one,
    /// else activate or launch. Ctrl activates the most recent window.
    fn click(&mut self, core: &Core<Self>, output: &WlOutput, item: &Item, serial: u32) {
        // Modifiers are only current while the dock has keyboard focus,
        // which the click gave it.
        let ctrl = core.input.keyboard_focus().is_some() && core.input.modifiers.ctrl;
        let Some(dock) = self.docks.iter().find(|d| d.output == *output) else {
            return;
        };
        let toggle = dock.strip_app() == Some(item.app_id.as_str());
        let strip = !ctrl && !toggle && dock.can_strip(item);
        // Any click closes the strip that is open; clicking its icon only
        // that.
        if toggle {
            self.close_strips(core);
            return;
        }
        if strip {
            // Only one grabbing popup at a time: the others go at once.
            // The clicked dock replaces its own, keeping its keyboard.
            for dock in self.docks.iter_mut().filter(|d| d.output != *output) {
                dock.drop_strip(core);
            }
            let grab = core.input.seat().map(|seat| (seat, serial));
            if let Some(dock) = self.dock_mut(output)
                && dock.open_strip(core, &item.app_id, grab)
            {
                return;
            }
        }
        self.close_strips(core);
        match item.click(&core.mru) {
            Click::Activate(id) => self.activate(core, &id),
            Click::Launch => self.launch(core, &item.app_id),
        }
        self.release_keyboard(output);
    }

    /// Close every dock's strip.
    fn close_strips(&mut self, core: &Core<Self>) {
        for dock in &mut self.docks {
            dock.close_strip(core);
        }
    }

    /// The dock on `output` handled a click without opening a popup: it
    /// gives the keyboard focus back.
    fn release_keyboard(&mut self, output: &WlOutput) {
        if let Some(dock) = self.dock_mut(output) {
            dock.release_keyboard();
        }
    }

    fn activate(&mut self, core: &Core<Self>, id: &str) {
        let Some(template) = self.config.activate_command.as_deref() else {
            if !std::mem::replace(&mut self.warned_activate, true) {
                warn!("set activate-command to focus windows from the dock");
            }
            return;
        };
        let command = launch::activate(template, id);
        let output = Output::Prefixed("activate-command".into());
        if let Err(e) = spawn::shell(&core.loop_handle, &command, output) {
            warn!("activate-command: {e}");
        }
    }

    /// Launch `app_id`, once its desktop entry is known.
    fn launch(&mut self, core: &Core<Self>, app_id: &str) {
        let Some(entry) = self.entries.get(app_id) else {
            if !self.pending_launches.iter().any(|a| a == app_id) {
                self.pending_launches.push(app_id.to_string());
            }
            if let Some(assets) = &self.assets {
                assets.entry(app_id);
            }
            return;
        };
        let command = launch::launch(
            app_id,
            entry.as_ref(),
            self.config.launch_command.as_deref(),
            &self.config.terminal,
        );
        let result = match &command {
            Ok(Command::Argv(argv)) => spawn::argv(&core.loop_handle, argv, Output::Inherit),
            Ok(Command::Shell(line)) => spawn::shell(&core.loop_handle, line, Output::Inherit),
            Err(e) => return warn!("cannot launch {app_id}: {e}"),
        };
        if let Err(e) = result {
            return warn!("cannot launch {app_id}: {e}");
        }
        self.start_bounce(core, app_id);
    }

    /// Bounce `app_id`'s icon until a new window of it appears, or for
    /// `launch-timeout`.
    fn start_bounce(&mut self, core: &Core<Self>, app_id: &str) {
        let timeout = Duration::from_millis(self.config.launch_timeout.into());
        if timeout.is_zero() {
            return;
        }
        if let Some(old) = self.launching.iter().position(|l| l.app_id == app_id) {
            let old = self.launching.remove(old);
            if let Some(token) = old.timeout {
                core.loop_handle.remove(token);
            }
        }
        let before = (Self::toplevels(core, None).into_iter())
            .filter(|t| t.app_id == app_id)
            .map(|t| t.identifier)
            .collect();
        let id = app_id.to_string();
        let token = core.timer(timeout, move |app: &mut Self, _| {
            app.launching.retain(|l| l.app_id != id);
            app.launching_changed();
        });
        self.launching.push(Launch {
            app_id: app_id.to_string(),
            before,
            timeout: token.ok(),
        });
        self.launching_changed();
    }

    /// End the launches whose app got a new window.
    fn end_launches(&mut self, core: &Core<Self>, toplevels: &[Toplevel]) {
        let before = self.launching.len();
        self.launching.retain_mut(|l| {
            let done = (toplevels.iter())
                .any(|t| t.app_id == l.app_id && !l.before.contains(&t.identifier));
            if done && let Some(token) = l.timeout.take() {
                core.loop_handle.remove(token);
            }
            !done
        });
        if self.launching.len() != before {
            self.launching_changed();
        }
    }

    fn launching_changed(&mut self) {
        let apps: Vec<String> = self.launching.iter().map(|l| l.app_id.clone()).collect();
        for dock in &mut self.docks {
            dock.set_launching(&apps);
        }
    }

    // -- the worker ------------------------------------------------------------

    /// A worker result: into the cache, then to the docks.
    fn worker_done(&mut self, core: &Core<Self>, done: Done) {
        let Some(assets) = &self.assets else { return };
        assets.received(&done);
        match done {
            Done::Icon { key, image } => {
                (self.cache.borrow_mut()).insert_icon(&core.g, key.clone(), &image, &core.qh);
                self.trim = true;
                for dock in &mut self.docks {
                    dock.icon_ready(&key);
                }
            }
            Done::Label { key, pill } => {
                (self.cache.borrow_mut()).insert_label(&core.g, key.clone(), &pill, &core.qh);
                self.trim = true;
                for dock in &mut self.docks {
                    dock.label_ready(&key);
                }
            }
            // Re-rendered icons replace the old ones as they arrive.
            Done::IconsReloaded => {
                self.entries.clear();
                self.prefetch_entries();
                for dock in &self.docks {
                    dock.request_assets(true);
                }
            }
            Done::Entry { app_id, entry } => {
                self.entries.insert(app_id.clone(), entry);
                if let Some(i) = self.pending_launches.iter().position(|a| *a == app_id) {
                    self.pending_launches.remove(i);
                    self.launch(core, &app_id);
                }
            }
            Done::Pill { tag, request, pill } => {
                for dock in &mut self.docks {
                    if dock.pill_ready(tag, &request, &pill) {
                        break;
                    }
                }
            }
        }
    }
}

impl Program for WlDock {
    fn toplevel_new(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(t) = core.toplevel(handle) {
            self.hold_new_window(core, &t.identifier);
        }
        self.dirty = true;
    }

    fn toplevel_update(&mut self, _: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.dirty = true;
    }

    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(t) = core.toplevel(handle) {
            self.placement.forget(&t.identifier);
        }
        self.rebuild(core, Some(handle));
    }

    fn focus_changed(&mut self, _: &mut Core<Self>) {
        if self.config.active_indicator {
            self.dirty = true;
        }
    }

    fn output_new(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_update(&mut self, _: &mut Core<Self>, _: &WlOutput) {
        self.outputs_dirty = true;
    }

    fn output_removed(&mut self, _: &mut Core<Self>, output: &WlOutput) {
        self.docks.retain(|d| d.output != *output);
    }

    /// The compositor closed a dock, e.g. because its output is going
    /// away. It comes back if the outputs or the config change.
    fn layer_closed(&mut self, _: &mut Core<Self>, layer: &LayerSurface) {
        self.docks.retain(|d| d.surface() != layer.wl_surface());
    }

    fn layer_configure(
        &mut self,
        _: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
        if let Some(dock) = self.dock_on(layer.wl_surface()) {
            let (width, height) = configure.new_size;
            dock.configure(width, height);
        }
    }

    fn scale_factor_changed(&mut self, _: &mut Core<Self>, surface: &WlSurface, factor: i32) {
        if let Some(dock) = self.dock_on(surface) {
            dock.set_scale(factor);
        }
    }

    fn frame(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if let Some(dock) = self.dock_on(surface) {
            dock.frame_done();
        }
    }

    fn popup_configure(&mut self, _: &mut Core<Self>, popup: &Popup, configure: PopupConfigure) {
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_configure(&configure);
        }
    }

    /// The compositor dismissed a strip: a click on another client.
    fn popup_done(&mut self, core: &mut Core<Self>, popup: &Popup) {
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_popup(popup)) {
            dock.popup_done(core);
        }
    }

    /// Keys reach the open strip, which has the keyboard while it is open.
    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let Some(key) = Key::from_sym(event.keysym) else {
            return;
        };
        if repeat && !key.repeats() {
            return;
        }
        let Some(focus) = core.input.keyboard_focus().cloned() else {
            return;
        };
        let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_strip_surface(&focus)) else {
            return;
        };
        if let Some(id) = dock.strip_key(core, key) {
            self.activate(core, &id);
        }
    }

    fn capture_update(
        &mut self,
        _: &mut Core<Self>,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        // Events of captures a dock dropped find nothing.
        if let Some(dock) = self.docks.iter_mut().find(|d| d.has_capture(id)) {
            dock.capture_update(id, f);
        }
    }

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
        if let Some(dock) = (self.docks.iter_mut()).find(|d| d.has_strip_surface(&event.surface)) {
            if let Some(id) = dock.strip_pointer(core, event) {
                self.activate(core, &id);
            }
            return;
        }
        let Some(dock) = self
            .docks
            .iter_mut()
            .find(|d| *d.surface() == event.surface)
        else {
            return;
        };
        let (x, y) = event.position;
        match event.kind {
            PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => {
                dock.pointer_motion(core, x, y);
            }
            PointerEventKind::Leave { .. } => {
                dock.pointer_leave(core);
                self.pressed = None;
            }
            PointerEventKind::Press { button, serial, .. } => {
                let output = dock.output.clone();
                self.button(core, &output, button, serial, true);
            }
            PointerEventKind::Release { button, serial, .. } => {
                let output = dock.output.clone();
                self.button(core, &output, button, serial, false);
            }
            _ => {}
        }
    }
}
