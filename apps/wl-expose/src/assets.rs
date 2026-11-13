//! The shared worker (see `wl_shell_render::worker`) and the icons received
//! from it, shared by the app and its overlays. The overlay asks for an
//! icon on every relayout: it gets the image if it arrived, and otherwise
//! the request goes out once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use wl_shell_core::capture::CaptureId;
use wl_shell_core::reexports::calloop::channel;
use wl_shell_render::Image;
use wl_shell_render::icons;
use wl_shell_render::worker::{self, Done, PillRequest};

pub struct Assets {
    worker: worker::Assets,
    /// None: requested, not received yet.
    icons: RefCell<HashMap<icons::Key, Option<Arc<Image>>>>,
}

impl Assets {
    /// Start the worker; insert the returned channel into the event loop and
    /// pass its results to `received`.
    pub fn spawn() -> std::io::Result<(Self, channel::Channel<Done>)> {
        let (worker, results) = worker::Assets::spawn()?;
        let assets = Self {
            worker,
            icons: RefCell::new(HashMap::new()),
        };
        Ok((assets, results))
    }

    /// The icon for `key` if received; otherwise it is requested (once) and
    /// arrives as `Done::Icon`.
    pub fn icon(&self, key: &icons::Key, font: &str) -> Option<Arc<Image>> {
        let mut icons = self.icons.borrow_mut();
        if let Some(entry) = icons.get(key) {
            return entry.clone();
        }
        icons.insert(key.clone(), None);
        self.worker.icon(key, font);
        None
    }

    /// A title pill for the window shown with `capture`; arrives as
    /// `Done::Pill` tagged with the capture's ID.
    pub fn pill(&self, capture: CaptureId, request: PillRequest) {
        self.worker.pill(capture.raw().into(), request);
    }

    /// Check for changed desktop entries.
    pub fn refresh(&self) {
        self.worker.refresh();
    }

    /// Forget all icons, here and in the worker.
    pub fn clear_icons(&self) {
        self.icons.borrow_mut().clear();
        self.worker.clear_icons();
    }

    /// Book-keeping for a result from the worker.
    pub fn received(&self, done: &Done) {
        self.worker.received(done);
        match done {
            Done::Icon { key, image } => {
                // Unless cleared since it was requested.
                if let Some(entry) = self.icons.borrow_mut().get_mut(key) {
                    *entry = Some(image.clone());
                }
            }
            // The worker already dropped its stale icons.
            Done::IconsReloaded => self.icons.borrow_mut().clear(),
            Done::Label { .. } | Done::Pill { .. } => {}
        }
    }
}
