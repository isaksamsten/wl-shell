//! wl-dock as a wl-shell program: the config, the items, one dock per
//! output, and how they react to the shell's events.
//!
//! Docks follow the outputs: one is created for every output with a name
//! that the config gives a dock (`outputs`, `[output.<name>]`), restyled
//! when the config changes, and destroyed with its output. The config file
//! is watched with inotify and the desktop's color scheme followed; a
//! config error keeps the previous config.
//!
//! The worker renders icons and labels for all docks into one cache. Every
//! dock shows every window (`windows = "all"`).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use smithay_client_toolkit::{
    seat::pointer::{PointerEvent, PointerEventKind},
    shell::{
        WaylandSurface,
        wlr_layer::{LayerSurface, LayerSurfaceConfigure},
    },
};
use wayland_client::protocol::{wl_output::WlOutput, wl_surface::WlSurface};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use wl_shell_config::Reloader;
use wl_shell_core::reexports::calloop::channel;
use wl_shell_core::{Core, Program, Shell};
use wl_shell_log::warn;
use wl_shell_render::worker::{Assets, Done};
use wl_shell_ui::Cache;

use crate::config::Config;
use crate::dock::{self, Item};
use crate::view::{DockView, Style};

pub struct WlDock {
    /// The config, reloaded when its file changes.
    reloader: Reloader<Config>,
    /// The config in use.
    config: Config,
    /// The config file was touched since it was last read.
    config_touched: bool,
    /// The worker, started with the dock.
    assets: Option<Rc<Assets>>,
    /// Icons, labels and shapes, shared by the docks.
    cache: Rc<RefCell<Cache>>,
    /// One per output that has a dock.
    docks: Vec<DockView>,
    /// What the docks show, rebuilt when windows change.
    items: Vec<Item>,
    /// The window list changed since `items` was built.
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
    /// Read the config (`explicit`, or the default file).
    pub fn new(explicit: Option<PathBuf>) -> Result<Self, String> {
        let reloader = Reloader::<Config>::load(explicit).map_err(|e| format!("config: {e}"))?;
        Ok(Self {
            config: reloader.config().clone(),
            config_touched: false,
            reloader,
            assets: None,
            cache: Rc::new(RefCell::new(Cache::default())),
            docks: Vec::new(),
            items: Vec::new(),
            dirty: true,
            outputs_dirty: true,
            trim: false,
            reload: false,
        })
    }

    /// Connected, with the window list and the outputs received: start the
    /// worker, watch the config, and create the docks.
    pub fn started(&mut self, core: &mut Core<Self>) -> Result<(), String> {
        self.reloader
            .refresh(&core.loop_handle, &mut |m| warn!("{m}"));
        self.config = self.reloader.config().clone();
        self.start_worker(core)?;
        self.watch_config(core);
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

    /// After every event loop dispatch: follow config and theme changes,
    /// match the docks to the outputs, and rebuild the items once for
    /// however many window events arrived.
    pub fn after_dispatch(&mut self, core: &mut Core<Self>) {
        let touched = std::mem::take(&mut self.config_touched);
        if (touched || self.reloader.scheme_changed())
            && (self.reloader).refresh(&core.loop_handle, &mut |m| warn!("{m}"))
        {
            self.config_changed();
        }
        if std::mem::take(&mut self.outputs_dirty) {
            self.sync_docks(core);
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
    fn config_changed(&mut self) {
        self.config = self.reloader.config().clone();
        let mut cache = self.cache.borrow_mut();
        cache.clear_icons();
        cache.clear_shapes();
        drop(cache);
        if let Some(assets) = &self.assets {
            assets.clear_icons();
        }
        self.outputs_dirty = true;
        self.reload = true;
        self.trim = true;
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
        let wanted: Vec<(WlOutput, i32, Style)> = (core.output_state.outputs())
            .filter_map(|output| {
                let info = core.output_state.info(&output)?;
                let layout = self.config.dock(info.name.as_deref()?)?;
                let style = Style::new(&self.config, layout);
                Some((output, info.scale_factor.max(1), style))
            })
            .collect();
        self.docks.retain(|dock| {
            (wanted.iter())
                .any(|(o, _, style)| *o == dock.output && !dock.style().needs_new_surface(style))
        });
        for (output, scale, style) in wanted {
            match self.docks.iter_mut().find(|d| d.output == output) {
                Some(dock) => {
                    dock.set_style(style, reload);
                    dock.set_scale(scale);
                }
                None => match DockView::new(
                    core,
                    output,
                    scale,
                    style,
                    &self.items,
                    self.cache.clone(),
                    assets.clone(),
                ) {
                    Ok(dock) => self.docks.push(dock),
                    Err(e) => warn!("failed to create a dock: {e}"),
                },
            }
        }
    }

    /// Rebuild the items from the window list, leaving out `closed` (the
    /// list still knows a window while its close is handled).
    fn rebuild(&mut self, core: &Core<Self>, closed: Option<&ExtForeignToplevelHandleV1>) {
        let toplevels: Vec<_> = (core.toplevels().into_iter())
            .filter(|(h, _)| Some(h) != closed)
            .map(|(_, t)| t)
            .collect();
        let items = dock::items(&toplevels);
        if items == self.items {
            return;
        }
        // An app not seen before may come with a new desktop entry.
        let new_app = (items.iter()).any(|i| !self.items.iter().any(|o| o.app_id == i.app_id));
        if new_app && let Some(assets) = &self.assets {
            assets.refresh();
        }
        self.items = items;
        for dock in &mut self.docks {
            dock.set_items(&self.items);
        }
    }

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
                for dock in &self.docks {
                    dock.request_assets(true);
                }
            }
            Done::Pill { .. } => {}
        }
    }
}

impl Program for WlDock {
    fn toplevel_new(&mut self, _: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.dirty = true;
    }

    fn toplevel_update(&mut self, _: &mut Core<Self>, _: &ExtForeignToplevelHandleV1) {
        self.dirty = true;
    }

    fn toplevel_closed(&mut self, core: &mut Core<Self>, handle: &ExtForeignToplevelHandleV1) {
        self.rebuild(core, Some(handle));
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

    fn pointer(&mut self, core: &mut Core<Self>, event: &PointerEvent) {
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
            PointerEventKind::Leave { .. } => dock.pointer_leave(core),
            _ => {}
        }
    }
}
