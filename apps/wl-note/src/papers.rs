//! Rendered paper, ready to show: the worker draws a sheet once per size,
//! color, tilt and scale, and it is kept as an shm buffer here. The most
//! recently used few dozen are kept.
//!
//! Replaced buffers are only destroyed by `trim`, and only once nothing
//! shows them anymore.

use std::rc::Rc;

use wayland_client::{QueueHandle, protocol::wl_buffer::WlBuffer};
use wl_shell_core::globals::Globals;
use wl_shell_core::{Program, Shell};
use wl_shell_log::warn;
use wl_shell_render::paper::{Paper, Rendered};
use wl_shell_render::worker::Assets;
use wl_shell_ui::Shape;
use wl_shell_ui::cache::shm_buffer;

/// Sheets kept: a few per note shown.
const KEEP: usize = 64;

pub struct Papers {
    assets: Rc<Assets>,
    /// Most recently used first.
    ready: Vec<(Paper, Shape)>,
    /// Asked of the worker.
    pending: Vec<Paper>,
    /// Buffers no longer kept, destroyed by `trim`.
    stale: Vec<WlBuffer>,
}

impl Papers {
    pub fn new(assets: Rc<Assets>) -> Self {
        Self {
            assets,
            ready: Vec::new(),
            pending: Vec::new(),
            stale: Vec::new(),
        }
    }

    /// The sheet for `paper`, or None if it is still being drawn (it is
    /// asked for).
    pub fn get(&mut self, paper: &Paper) -> Option<Shape> {
        if let Some(i) = self.ready.iter().position(|(p, _)| p == paper) {
            let entry = self.ready.remove(i);
            let shape = entry.1.clone();
            self.ready.insert(0, entry);
            return Some(shape);
        }
        if !self.pending.contains(paper) {
            self.pending.push(paper.clone());
            self.assets.paper(paper);
        }
        None
    }

    /// A sheet from the worker. Returns whether it was still wanted.
    pub fn insert<P: Program>(
        &mut self,
        g: &Globals,
        qh: &QueueHandle<Shell<P>>,
        paper: Paper,
        rendered: &Rendered,
    ) -> bool {
        let Some(i) = self.pending.iter().position(|p| *p == paper) else {
            return false;
        };
        self.pending.remove(i);
        let buffer = match shm_buffer(g, &rendered.image, qh) {
            Ok(buffer) => buffer,
            Err(e) => {
                warn!("paper buffer: {e}");
                return false;
            }
        };
        let shape = Shape {
            buffer,
            rect: rendered.rect,
        };
        self.ready.insert(0, (paper, shape));
        true
    }

    /// Forget every sheet, e.g. because the config changed.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.stale
            .extend(self.ready.drain(..).map(|(_, s)| s.buffer));
    }

    /// Destroy the buffers no longer kept, but those in `shown`.
    pub fn trim(&mut self, shown: &[WlBuffer]) {
        while self.ready.len() > KEEP {
            if let Some((_, s)) = self.ready.pop() {
                self.stale.push(s.buffer);
            }
        }
        self.stale.retain(|b| {
            let keep = shown.contains(b);
            if !keep {
                b.destroy();
            }
            keep
        });
    }
}
