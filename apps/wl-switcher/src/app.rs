//! wl-switcher as a wl-shell program: the daemon's caches between shows, the
//! show in progress, and how it reacts to the shell's events.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use smithay_client_toolkit::{
    seat::{
        keyboard::{KeyEvent, Modifiers},
        pointer::{PointerEvent, PointerEventKind},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::Reloader;
use wl_shell_core::capture::{Capture, CaptureEvent, CaptureId};
use wl_shell_core::globals::Globals;
use wl_shell_core::reexports::calloop::{RegistrationToken, channel};
use wl_shell_core::toplevel;
use wl_shell_core::{Core, Program, Shell};
use wl_shell_daemon::{Daemon, Outcome, Reply};
use wl_shell_log::warn;
use wl_shell_render::icons;
use wl_shell_render::worker::{Assets, Done};

use crate::cache::Cache;
use crate::config::Config;
use crate::input::{Action, Hold, Mods};
use crate::ipc::Request;
use crate::overlay::{self, Hit, Overlay};
use crate::switcher::{Mode, Window};

/// linux/input-event-codes.h
const BTN_LEFT: u32 = 0x110;

/// How long the selection must rest in apps mode before its neighborhood
/// is captured.
const CAPTURE_DEBOUNCE: Duration = Duration::from_millis(50);

pub struct WlSwitcher {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The config in use, set before each show.
    config: Config,
    /// The worker thread, started with the daemon.
    assets: Option<Rc<Assets>>,
    /// Icons, labels and shapes, kept between shows.
    cache: Rc<RefCell<Cache>>,
    /// The switcher, while a show is in progress.
    overlay: Option<Overlay>,
    /// Release-to-pick.
    hold: Hold,
    /// The show-delay or cancel fade timer of the current show.
    timer: Option<RegistrationToken>,
    /// The debounce before captures follow an apps-mode selection.
    capture_timer: Option<RegistrationToken>,
    /// Set once the user picked or cancelled.
    outcome: Option<Outcome>,
}

impl WlSwitcher {
    /// Read the config (`explicit`, or the default file).
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        let reloader = Reloader::<Config>::load(explicit).map_err(|e| format!("config: {e}"))?;
        Ok(Self {
            config: reloader.config().clone(),
            reloader,
            assets: None,
            cache: Rc::new(RefCell::new(Cache::default())),
            overlay: None,
            hold: Hold::default(),
            timer: None,
            capture_timer: None,
            outcome: None,
        })
    }

    /// Make the reloaded config the one in use, and (re)start the focus
    /// stream it names.
    fn use_config(&mut self, core: &mut Core<Self>) {
        self.config = self.reloader.config().clone();
        core.use_focus_stream(self.config.mru_event_stream.as_deref());
    }

    /// The worker, started on first use.
    fn assets(&mut self, core: &Core<Self>) -> Result<Rc<Assets>, String> {
        if let Some(assets) = &self.assets {
            return Ok(assets.clone());
        }
        let (assets, results) =
            Assets::spawn().map_err(|e| format!("failed to start the worker: {e}"))?;
        core.loop_handle
            .insert_source(results, |event, _, shell: &mut Shell<Self>| {
                if let channel::Event::Msg(done) = event {
                    shell.program.worker_done(&shell.core, done);
                }
            })
            .map_err(|e| format!("failed to watch the worker: {e}"))?;
        let assets = Rc::new(assets);
        self.assets = Some(assets.clone());
        Ok(assets)
    }

    /// A worker result: into the cache, then to the overlay.
    fn worker_done(&mut self, core: &Core<Self>, done: Done) {
        if let Some(assets) = &self.assets {
            assets.received(&done);
        }
        match done {
            Done::Icon { key, image } => {
                (self.cache.borrow_mut()).insert_icon(&core.g, key.clone(), &image, &core.qh);
                if let Some(overlay) = &mut self.overlay {
                    overlay.icon_ready(&key);
                }
            }
            Done::Label { key, pill } => {
                (self.cache.borrow_mut()).insert_label(&core.g, key.clone(), &pill, &core.qh);
                if let Some(overlay) = &mut self.overlay {
                    overlay.label_ready(&key);
                }
            }
            Done::IconsReloaded => {
                self.cache.borrow_mut().clear_icons();
                match &mut self.overlay {
                    Some(overlay) => overlay.icons_reloaded(),
                    None => self.prefetch(core),
                }
            }
            Done::Pill { tag, request, pill } => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.pill_ready(tag, request, pill);
                }
            }
        }
    }

    /// The config changed: forget what was rendered with the old one.
    fn config_changed(&mut self) {
        let mut cache = self.cache.borrow_mut();
        cache.clear_icons();
        cache.clear_shapes();
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
    }

    /// Have the worker render the icon and label of every open window's app
    /// at every output scale, so the next show has them on its first frame.
    fn prefetch(&mut self, core: &Core<Self>) {
        let assets = match self.assets(core) {
            Ok(assets) => assets,
            Err(e) => return warn!("{e}"),
        };
        let config = &self.config;
        let cache = self.cache.borrow();
        let mut app_ids: Vec<String> = (core.toplevels().into_iter())
            .map(|(_, t)| t.app_id)
            .collect();
        app_ids.sort_unstable();
        app_ids.dedup();
        for scale in core.scales() {
            for app_id in &app_ids {
                let key = icon_key(config, app_id, scale);
                if cache.icon(&key).is_none() {
                    assets.icon(&key, &config.font);
                }
                if let Some(key) = overlay::label_key(app_id, config, scale)
                    && cache.label(&key).is_none()
                {
                    assets.label(&key);
                }
            }
        }
    }

    /// A window appeared or changed its app: get its icon ready, unless a
    /// show is on (`after_show` catches up) or the daemon is still starting.
    fn toplevel_changed(&mut self, core: &Core<Self>) {
        if self.overlay.is_none() && self.assets.is_some() {
            self.prefetch(core);
        }
    }

    /// The output named `name` (None: the compositor's choice) and the
    /// scale to start with: the output's, or the largest.
    fn output(
        core: &Core<Self>,
        name: Option<&str>,
        warn: &mut dyn FnMut(String),
    ) -> (Option<WlOutput>, i32) {
        let largest = core.scales().last().copied().unwrap_or(1);
        let Some(name) = name else {
            return (None, largest);
        };
        match core.output_named(name) {
            Some((output, scale)) => (Some(output), scale),
            None => {
                warn(format!("no output named '{name}'"));
                (None, largest)
            }
        }
    }

    /// Run `f` after `after`, replacing the show's previous timer.
    fn set_timer(
        &mut self,
        core: &Core<Self>,
        after: Duration,
        f: impl FnOnce(&mut Self, &mut Core<Self>) + 'static,
    ) -> Result<(), String> {
        if let Some(token) = self.timer.take() {
            core.loop_handle.remove(token);
        }
        let token = core.timer(after, |app: &mut Self, core| {
            app.timer = None;
            f(app, core);
        })?;
        self.timer = Some(token);
        Ok(())
    }

    /// The selection may have moved: plan the captures for it once it
    /// rests for a moment, so Tab-cycling doesn't start a burst of
    /// sessions.
    fn schedule_captures(&mut self, core: &Core<Self>) {
        if !self.overlay.as_ref().is_some_and(|o| o.captures_stale()) {
            return;
        }
        if let Some(token) = self.capture_timer.take() {
            core.loop_handle.remove(token);
        }
        let token = core.timer(CAPTURE_DEBOUNCE, |app: &mut Self, _| {
            app.capture_timer = None;
            if let Some(overlay) = &mut app.overlay {
                overlay.update_captures();
            }
        });
        match token {
            Ok(token) => self.capture_timer = Some(token),
            // Then right away.
            Err(_) => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.update_captures();
                }
            }
        }
    }

    /// The first outcome wins.
    fn finish(&mut self, outcome: Outcome) {
        self.outcome.get_or_insert(outcome);
    }

    /// Pick the selection: close at once. Not once a cancel is fading out
    /// (releasing Alt after Alt+Escape).
    fn pick(&mut self) {
        let Some(overlay) = self.overlay.as_ref().filter(|o| !o.closing()) else {
            return;
        };
        let id = overlay.switcher.picked().id.clone();
        self.finish(Outcome::Picked(id));
    }

    /// Close without picking, after a short fade if visible.
    fn cancel(&mut self, core: &Core<Self>) {
        if self.outcome.is_some() {
            return;
        }
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        if !overlay.start_close() {
            return self.finish(Outcome::Cancelled);
        }
        let fade = Duration::from_millis(overlay::CLOSE_MS);
        if (self.set_timer(core, fade, |app, _| app.finish(Outcome::Cancelled))).is_err() {
            self.finish(Outcome::Cancelled);
        }
    }

    /// Whether `surface` is the open switcher's.
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

impl Program for WlSwitcher {
    fn toplevel_new(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.toplevel_changed(core);
    }

    fn toplevel_update(&mut self, core: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.toplevel_changed(core);
    }

    fn toplevel_closed(&mut self, _: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        let Some(overlay) = &mut self.overlay else {
            return;
        };
        let Some(id) = overlay.window_of(handle).map(str::to_string) else {
            return;
        };
        if !overlay.remove(&id) {
            warn!("all windows closed");
            self.finish(Outcome::Cancelled);
        }
    }

    fn layer_closed(&mut self, _: &mut Core<Self>, _: &LayerSurface) {
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
    }

    fn keyboard_enter(&mut self, _: &mut Core<Self>, surface: &WlSurface) {
        if self.ours(surface) {
            self.hold.enter();
        }
    }

    fn keyboard_leave(&mut self, core: &mut Core<Self>, surface: &WlSurface) {
        // Without focus, the release can't be seen.
        if self.ours(surface) && self.hold != Hold::Idle {
            self.hold.leave();
            warn!("lost keyboard focus");
            self.cancel(core);
        }
    }

    fn key(&mut self, core: &mut Core<Self>, event: &KeyEvent, repeat: bool) {
        let shift = core.input.modifiers.shift;
        let Some(action) = Action::from_key(event.keysym, event.raw_code, shift) else {
            return;
        };
        if self.overlay.is_none() || (repeat && !action.repeats()) {
            return;
        }
        match action {
            Action::Cancel => self.cancel(core),
            Action::Pick => self.pick(),
            Action::Step(step) => {
                if let Some(overlay) = &mut self.overlay {
                    overlay.step(step);
                }
                self.schedule_captures(core);
            }
        }
    }

    fn modifiers(&mut self, _: &mut Core<Self>, modifiers: &Modifiers) {
        if (self.hold).modifiers(Mods::from(modifiers), self.config.modifier) {
            self.pick();
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
            // an icon when the switcher appears.
            PointerEventKind::Motion { .. } => {
                match overlay.hit(x, y) {
                    // Not with the strip open: the pointer crossing the bar
                    // beside it shouldn't collapse it.
                    Hit::App(i) if overlay.switcher.mode() == Mode::Apps => overlay.select_app(i),
                    Hit::Window(i) => overlay.select_window(i),
                    _ => {}
                }
                self.schedule_captures(core);
            }
            PointerEventKind::Press { button, .. } => {
                if !overlay.visible() || !inside {
                    return;
                }
                let (px, py) = core.input.pointer_pos;
                match overlay.hit(px, py) {
                    Hit::Outside => self.cancel(core),
                    Hit::App(i) if button == BTN_LEFT => {
                        overlay.select_app(i);
                        self.pick();
                    }
                    Hit::Window(i) if button == BTN_LEFT => {
                        overlay.select_window(i);
                        self.pick();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// To the show's overlay, which owns the captures. Events of an earlier
    /// show's captures find nothing.
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

impl Daemon for WlSwitcher {
    type Request = Request;

    fn started(&mut self, core: &mut Core<Self>) {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.use_config(core);
        // Start the worker now, so icons and labels are ready for the first show.
        self.prefetch(core);
    }

    fn refresh(&mut self, core: &mut Core<Self>, warn: &mut dyn FnMut(String)) {
        if self.reloader.refresh(&core.loop_handle, warn) {
            self.use_config(core);
            self.config_changed();
        }
    }

    /// Open the switcher for `req` and start its show-delay timer.
    fn show(
        &mut self,
        core: &mut Core<Self>,
        req: Request,
        warn: &mut dyn FnMut(String),
    ) -> Result<(), String> {
        let (handles, toplevels): (Vec<_>, Vec<_>) = core.toplevels().into_iter().unzip();
        let shown = toplevel::shown(&toplevels, &req.ids, &core.mru, |id| {
            warn(format!("no window with ID '{id}'"));
        });
        if shown.is_empty() {
            return Err("no windows to show".into());
        }
        let windows: Vec<_> = (shown.iter())
            .map(|&i| {
                let t = &toplevels[i];
                let window = Window {
                    id: t.identifier.clone(),
                    app_id: t.app_id.clone(),
                    title: t.title.clone(),
                };
                (handles[i].clone(), window)
            })
            .collect();
        let (output, scale) = Self::output(core, req.output.as_deref(), warn);
        let assets = self.assets(core)?;
        let mut overlay = Overlay::new(
            core.g.clone(),
            windows,
            req.step,
            output.as_ref(),
            scale,
            &self.config,
            self.cache.clone(),
            assets,
            &core.qh,
        )
        .map_err(|e| format!("failed to create the switcher: {e}"))?;
        overlay.inhibit_shortcuts(core.seat_state.seats());
        self.overlay = Some(overlay);
        self.outcome = None;

        let delay = Duration::from_millis(self.config.show_delay.into());
        self.set_timer(core, delay, |app, _| {
            if let Some(overlay) = &mut app.overlay {
                overlay.reveal();
            }
        })
    }

    fn showing(&self) -> bool {
        self.overlay.is_some()
    }

    fn busy(&mut self, _: &mut Core<Self>) -> Reply {
        Reply::Error("the switcher is already open".into())
    }

    fn client_gone(&mut self, core: &mut Core<Self>) {
        self.cancel(core);
    }

    fn take_outcome(&mut self) -> Option<Outcome> {
        self.outcome.take()
    }

    /// Destroy the switcher, and with it its captures.
    fn end_show(&mut self, core: &mut Core<Self>) {
        self.overlay = None;
        for token in [self.timer.take(), self.capture_timer.take()]
            .into_iter()
            .flatten()
        {
            core.loop_handle.remove(token);
        }
        // The next switcher gets a fresh enter.
        core.input.reset_pointer();
        self.hold.leave();
    }

    /// Buffers no longer attached anywhere can go.
    fn after_show(&mut self, core: &mut Core<Self>) {
        self.cache.borrow_mut().trim();
        // Desktop entries installed since, and windows opened since, are
        // ready next time.
        if let Some(assets) = &self.assets {
            assets.refresh();
        }
        self.prefetch(core);
    }

    /// Follow a theme switch now, not on the next show: that would have to
    /// render everything first.
    fn idle(&mut self, core: &mut Core<Self>) {
        if self.reloader.scheme_changed()
            && self
                .reloader
                .refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.use_config(core);
            self.config_changed();
            self.prefetch(core);
        }
    }
}
