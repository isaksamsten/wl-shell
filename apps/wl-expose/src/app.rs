//! wl-expose as a wl-shell program: the daemon's state between shows, the
//! show in progress, and how it reacts to the shell's events.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use wl_shell_config::{Reloader, reload::mtime};
use wl_shell_core::reexports::calloop::{RegistrationToken, channel};
use wl_shell_core::reexports::smithay_client_toolkit::{
    seat::{
        keyboard::{KeyEvent, Keysym},
        pointer::{PointerEvent, PointerEventKind},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerSurface, LayerSurfaceConfigure},
    },
};
use wl_shell_core::reexports::wayland_client::protocol::{
    wl_output::WlOutput, wl_surface::WlSurface,
};
use wl_shell_core::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::Globals;
use wl_shell_core::toplevel;
use wl_shell_core::{Core, Program, Shell};
use wl_shell_daemon::{Daemon, Outcome, Reply};
use wl_shell_log::warn;
use wl_shell_render::worker::Done;
use wl_shell_render::{Image, icons};

use crate::assets::Assets;
use crate::config::Config;
use crate::ipc::Request;
use crate::layout::Dir;
use crate::overlay::{Overlay, Wallpaper};

/// linux/input-event-codes.h
const BTN_LEFT: u32 = 0x110;

pub struct Expose {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The background image the config names, decoded into an shm buffer.
    wallpaper: Option<Rc<Wallpaper>>,
    /// The image path and mtime `wallpaper` was loaded from (or failed to).
    wallpaper_source: Option<(PathBuf, Option<SystemTime>)>,
    /// The worker thread for icons and titles, started with the daemon.
    assets: Option<Rc<Assets>>,
    /// The current overlay, while a show is in progress.
    overlay: Option<Overlay>,
    /// The previous overlay while it fades out, without input.
    closing: Option<Overlay>,
    /// The first-frame timer of the current show, until it fires.
    deadline: Option<RegistrationToken>,
    /// Set once the user picked or cancelled.
    outcome: Option<Outcome>,
}

impl Expose {
    /// Read the config (`explicit`, or the default file).
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        Ok(Self {
            reloader: Reloader::load(explicit).map_err(|e| format!("config: {e}"))?,
            wallpaper: None,
            wallpaper_source: None,
            assets: None,
            overlay: None,
            closing: None,
            deadline: None,
            outcome: None,
        })
    }

    fn config(&self) -> &Config {
        self.reloader.config()
    }

    /// Reload what changed: the config (with the desktop's scheme) and the
    /// background image. Returns whether the config changed.
    fn reload(&mut self, core: &mut Core<Self>, warn: &mut dyn FnMut(String)) -> bool {
        let changed = self.reloader.refresh(&core.loop_handle, warn);
        let source = self.config().theme().background_image.clone().map(|path| {
            let mtime = mtime(&path);
            (path, mtime)
        });
        if source != self.wallpaper_source {
            // Free the old image before decoding the new one.
            self.wallpaper = None;
            self.wallpaper_source = source;
            if let Some((path, _)) = &self.wallpaper_source {
                match load_wallpaper(path, &core.g, core) {
                    Ok(wallpaper) => self.wallpaper = Some(wallpaper),
                    Err(e) => warn(e),
                }
            }
        }
        changed
    }

    /// The config changed: forget icons, (re)start the focus stream.
    fn config_changed(&mut self, core: &mut Core<Self>) {
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
        core.use_focus_stream(self.config().mru_event_stream.as_deref());
    }

    /// The worker, started on first use; its results go to the overlay.
    fn assets(&mut self, core: &Core<Self>) -> Result<Rc<Assets>, String> {
        if let Some(assets) = &self.assets {
            // Entries may have changed since the last show.
            assets.refresh();
            return Ok(assets.clone());
        }
        let (assets, results) =
            Assets::spawn().map_err(|e| format!("failed to start the worker: {e}"))?;
        core.loop_handle
            .insert_source(results, |event, _, shell: &mut Shell<Self>| {
                if let channel::Event::Msg(done) = event {
                    shell.program.worker_done(done);
                }
            })
            .map_err(|e| format!("failed to watch the worker: {e}"))?;
        let assets = Rc::new(assets);
        self.assets = Some(assets.clone());
        Ok(assets)
    }

    fn worker_done(&mut self, done: Done) {
        if let Some(assets) = &self.assets {
            assets.received(&done);
        }
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        match done {
            Done::Icon { key, .. } => overlay.icon_ready(&key),
            Done::IconsReloaded => overlay.icons_reloaded(),
            Done::Pill { tag, request, pill } => overlay.pill_ready(tag, request, pill),
            Done::Label { .. } => {}
        }
    }

    /// Have the worker load the icons of every open window at every output
    /// scale, so the daemon's next show has them on its first frame.
    fn prefetch_icons(&mut self, core: &Core<Self>) {
        let config = self.config().clone();
        if config.icon_size == 0 {
            return;
        }
        let assets = match self.assets(core) {
            Ok(assets) => assets,
            Err(e) => return warn!("{e}"),
        };
        let scales = core.scales();
        for (_, toplevel) in core.toplevels() {
            for &scale in &scales {
                let key = icon_key(&config, &toplevel.app_id, scale);
                assets.icon(&key, &config.font);
            }
        }
    }

    /// The output named `name`, or without a name, the only one. None (the
    /// compositor picks one and there is no screenshot) otherwise.
    fn output(
        core: &Core<Self>,
        name: Option<&str>,
        warn: &mut dyn FnMut(String),
    ) -> Option<WlOutput> {
        let Some(name) = name else {
            let mut outputs = core.output_state.outputs();
            let first = outputs.next();
            return first.filter(|_| outputs.next().is_none());
        };
        let found = core.output_named(name).map(|(output, _)| output);
        if found.is_none() {
            warn(format!("no output named '{name}'"));
        }
        found
    }

    /// The first outcome wins.
    fn finish(&mut self, outcome: Outcome) {
        self.outcome.get_or_insert(outcome);
    }

    fn pick(&mut self, i: usize) {
        let Some(win) = self.overlay.as_ref().and_then(|o| o.windows.get(i)) else {
            return;
        };
        self.finish(Outcome::Picked(win.identifier.clone()));
    }

    /// Whether `surface` is the open overlay's.
    fn ours(&self, surface: &WlSurface) -> bool {
        self.overlay
            .as_ref()
            .is_some_and(|o| o.surface() == surface)
    }
}

/// The key of `app_id`'s icon with `config`'s size, theme and shadow at
/// `scale`; the overlay asks for the same.
fn icon_key(config: &Config, app_id: &str, scale: i32) -> icons::Key {
    let theme = config.theme().icon_theme.clone();
    icons::Key::new(app_id, config.icon_size, theme, config.icon_shadow(), scale)
}

/// Decode `path` into an shm buffer.
fn load_wallpaper(path: &Path, g: &Globals, core: &Core<Expose>) -> Result<Rc<Wallpaper>, String> {
    let img = Image::load(path)?;
    let wallpaper =
        Wallpaper::new(g, &img, &core.qh).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Rc::new(wallpaper))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Action {
    Cancel,
    PickSelected,
    /// Pick the n-th window in input order.
    Pick(usize),
    Move(Dir),
    Cycle(isize),
}

impl Action {
    fn from_key(sym: Keysym, shift: bool) -> Option<Self> {
        Some(match sym {
            Keysym::Escape => Action::Cancel,
            Keysym::Return | Keysym::KP_Enter => Action::PickSelected,
            Keysym::Left | Keysym::h => Action::Move(Dir::Left),
            Keysym::Right | Keysym::l => Action::Move(Dir::Right),
            Keysym::Up | Keysym::k => Action::Move(Dir::Up),
            Keysym::Down | Keysym::j => Action::Move(Dir::Down),
            Keysym::Tab => Action::Cycle(if shift { -1 } else { 1 }),
            Keysym::ISO_Left_Tab => Action::Cycle(-1),
            _ => {
                let n = sym.raw().checked_sub(Keysym::_1.raw()).filter(|&n| n < 9)?;
                Action::Pick(n as usize)
            }
        })
    }

    /// Navigation repeats while the key is held; picking doesn't.
    fn repeats(self) -> bool {
        matches!(self, Action::Move(_) | Action::Cycle(_))
    }
}

impl Program for Expose {
    /// Titles (and app_ids) update live.
    fn toplevel_update(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        if let Some(overlay) = &mut self.overlay
            && let Some(toplevel) = core.toplevel(handle)
        {
            overlay.update_toplevel(handle, &toplevel);
        }
    }

    fn toplevel_closed(&mut self, _: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        let Some(i) = overlay.windows.iter().position(|w| w.handle == *handle) else {
            return;
        };
        overlay.remove(i);
        if overlay.windows.is_empty() {
            warn!("all windows closed");
            self.finish(Outcome::Cancelled);
        }
    }

    fn layer_closed(&mut self, _: &mut Core<Self>, layer: &LayerSurface) {
        if let Some(closing) = &self.closing
            && closing.surface() == layer.wl_surface()
        {
            self.closing = None;
            return;
        }
        self.finish(Outcome::Cancelled);
    }

    fn layer_configure(
        &mut self,
        _: &mut Core<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
    ) {
        if let Some(overlay) = &mut self.overlay
            && overlay.surface() == layer.wl_surface()
        {
            let (width, height) = configure.new_size;
            overlay.configure(width, height);
        }
    }

    fn scale_factor_changed(&mut self, _: &mut Core<Self>, surface: &WlSurface, factor: i32) {
        if let Some(overlay) = &mut self.overlay
            && overlay.surface() == surface
        {
            overlay.set_scale(factor);
        }
    }

    fn frame(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if let Some(overlay) = &mut self.overlay
            && overlay.surface() == surface
        {
            overlay.frame_done();
        }
        if let Some(closing) = &mut self.closing
            && closing.surface() == surface
        {
            closing.frame_done();
            if closing.closed() {
                self.closing = None;
            }
        }
    }

    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let shift = core.input.modifiers.shift;
        let Some(action) = Action::from_key(event.keysym, shift) else {
            return;
        };
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        if repeat && !action.repeats() {
            return;
        }
        match action {
            Action::Cancel => self.finish(Outcome::Cancelled),
            Action::PickSelected => {
                if let Some(i) = overlay.selected {
                    self.pick(i);
                }
            }
            Action::Pick(i) => self.pick(i),
            Action::Move(dir) => overlay.move_selection(dir),
            Action::Cycle(step) => overlay.cycle(step),
        }
    }

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
        if !self.ours(&event.surface) {
            return;
        }
        let inside = core.input.pointer_on(&event.surface);
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        let (x, y) = event.position;
        match event.kind {
            // Don't select on enter: the pointer may just happen to rest on
            // a thumbnail when the overlay appears.
            PointerEventKind::Motion { .. } => {
                if let Some(i) = overlay.hit(x, y) {
                    overlay.select(i);
                }
            }
            PointerEventKind::Press { button, .. } => {
                if !overlay.mapped() || !inside {
                    return;
                }
                let (px, py) = core.input.pointer_pos;
                match overlay.hit(px, py) {
                    None => self.finish(Outcome::Cancelled), // any click on the background
                    Some(i) if button == BTN_LEFT => self.pick(i),
                    Some(_) => {}
                }
            }
            _ => {}
        }
    }

    fn capture_update(
        &mut self,
        _: &mut Core<Self>,
        id: CaptureId,
        f: impl FnOnce(&mut Capture, &Globals) -> Option<CaptureEvent>,
    ) {
        if let Some(overlay) = &mut self.overlay {
            overlay.capture_update(id, f);
        }
    }
}

impl Daemon for Expose {
    type Request = Request;

    fn started(&mut self, core: &mut Core<Self>) {
        self.reload(core, &mut |m| warn!("{m}"));
        core.use_focus_stream(self.config().mru_event_stream.as_deref());
        // Start the worker now, so fonts and icons are ready for the first show.
        self.prefetch_icons(core);
    }

    fn refresh(&mut self, core: &mut Core<Self>, warn: &mut dyn FnMut(String)) {
        if self.reload(core, warn) {
            // Icon size, theme or font may differ.
            self.config_changed(core);
        }
    }

    /// Open the overlay for `req` and start its first-frame timer.
    fn show(
        &mut self,
        core: &mut Core<Self>,
        req: Request,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), String> {
        // It would be in the screenshot.
        self.closing = None;
        let (handles, toplevels): (Vec<_>, Vec<_>) = core.toplevels().into_iter().unzip();
        let shown = toplevel::shown(&toplevels, &req.ids, &core.mru, |id| {
            warn(format!("no window with ID '{id}'"));
        });
        if shown.is_empty() {
            return Err("no windows to show".into());
        }
        let windows: Vec<_> = (shown.iter())
            .map(|&i| (handles[i].clone(), toplevels[i].clone()))
            .collect();
        let output = Self::output(core, req.output.as_deref(), warn);
        let assets = self.assets(core)?;
        let config = self.config().clone();
        let mut overlay = Overlay::new(
            core.g.clone(),
            windows,
            output.as_ref(),
            &req,
            &config,
            self.wallpaper.clone(),
            assets,
            &core.qh,
        )
        .map_err(|e| format!("failed to create the overlay: {e}"))?;
        // The first window shown (the focused one, with focus history)
        // starts selected, unless --select says otherwise.
        let mut selected = 0;
        if let Some(id) = &req.select {
            match overlay.windows.iter().position(|w| w.identifier == *id) {
                Some(i) => selected = i,
                None => warn(format!("--select: no shown window with ID '{id}'")),
            }
        }
        overlay.selected = Some(selected);
        self.overlay = Some(overlay);
        self.outcome = None;

        let timeout = Duration::from_millis(req.timeout_ms.into());
        let token = core.timer(timeout, |expose: &mut Self, _| {
            expose.deadline = None;
            if let Some(overlay) = &mut expose.overlay {
                overlay.deadline_passed();
            }
        })?;
        self.deadline = Some(token);
        Ok(())
    }

    fn showing(&self) -> bool {
        self.overlay.is_some()
    }

    /// Toggle: close the open overlay instead of queueing.
    fn busy(&mut self, _: &mut Core<Self>) -> Reply {
        self.finish(Outcome::Cancelled);
        Reply::Cancelled
    }

    fn client_gone(&mut self, _: &mut Core<Self>) {
        self.finish(Outcome::Cancelled);
    }

    fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }

    /// The overlay fades out, without keyboard focus or input, or goes
    /// right away.
    fn end_show(&mut self, core: &mut Core<Self>) {
        if let Some(mut overlay) = self.overlay.take()
            && overlay.start_closing()
        {
            self.closing = Some(overlay);
        }
        if let Some(token) = self.deadline.take() {
            core.loop_handle.remove(token);
        }
        core.input.reset_pointer();
    }

    /// Windows opened since are ready next time.
    fn after_show(&mut self, core: &mut Core<Self>) {
        self.prefetch_icons(core);
    }

    /// Map once ready.
    fn after_dispatch(&mut self, _: &mut Core<Self>) {
        if let Some(overlay) = &mut self.overlay {
            overlay.maybe_map();
        }
    }

    /// Follow a theme switch now, not on the next show: that would have to
    /// decode its background image first.
    fn idle(&mut self, core: &mut Core<Self>) {
        if self.reloader.scheme_changed() && self.reload(core, &mut |m| warn!("{m}")) {
            self.config_changed(core);
            self.prefetch_icons(core);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_actions() {
        assert_eq!(
            Action::from_key(Keysym::Escape, false),
            Some(Action::Cancel)
        );
        assert_eq!(
            Action::from_key(Keysym::l, false),
            Some(Action::Move(Dir::Right))
        );
        assert_eq!(Action::from_key(Keysym::Tab, true), Some(Action::Cycle(-1)));
        assert_eq!(Action::from_key(Keysym::_1, false), Some(Action::Pick(0)));
        assert_eq!(Action::from_key(Keysym::_9, false), Some(Action::Pick(8)));
        assert_eq!(Action::from_key(Keysym::_0, false), None);
        assert_eq!(Action::from_key(Keysym::a, false), None);
        assert!(!Action::Pick(0).repeats() && Action::Cycle(1).repeats());
    }
}
