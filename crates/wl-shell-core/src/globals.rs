//! Protocol globals the shell needs, and user data for objects whose events
//! we don't care about.

use smithay_client_toolkit::{
    compositor::CompositorState, dispatch2::Dispatch2, error::GlobalError,
    globals::ProvidesBoundGlobal, shell::wlr_layer::LayerShell, subcompositor::SubcompositorState,
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::GlobalList,
    protocol::{wl_buffer::WlBuffer, wl_shm::WlShm},
};
use wayland_protocols::{
    ext::{
        image_capture_source::v1::client::{
            ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
            ext_image_capture_source_v1::ExtImageCaptureSourceV1,
            ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
        },
        image_copy_capture::v1::client::ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1,
    },
    wp::{
        alpha_modifier::v1::client::{
            wp_alpha_modifier_surface_v1::WpAlphaModifierSurfaceV1,
            wp_alpha_modifier_v1::WpAlphaModifierV1,
        },
        keyboard_shortcuts_inhibit::zv1::client::{
            zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
            zwp_keyboard_shortcuts_inhibitor_v1::ZwpKeyboardShortcutsInhibitorV1,
        },
        linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1,
        viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
    },
};

use wl_shell_log::warn;

use crate::dmabuf::Dmabuf;
use crate::shell::{Program, Shell};

/// User data for objects whose events are ignored (or that have none).
pub struct Ignore;

macro_rules! ignore_events {
    ($($iface:ty),* $(,)?) => {$(
        impl<D> Dispatch2<$iface, D> for Ignore {
            fn event(
                &self,
                _: &mut D,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &Connection,
                _: &QueueHandle<D>,
            ) {
            }
        }
    )*};
}

ignore_events!(
    WlShm,
    WlBuffer,
    WpViewporter,
    WpViewport,
    WpSinglePixelBufferManagerV1,
    WpAlphaModifierV1,
    WpAlphaModifierSurfaceV1,
    ExtForeignToplevelImageCaptureSourceManagerV1,
    ExtOutputImageCaptureSourceManagerV1,
    ExtImageCaptureSourceV1,
    ExtImageCopyCaptureManagerV1,
    ZwpKeyboardShortcutsInhibitManagerV1,
    // active/inactive: nothing to do either way; without it, the
    // compositor's own bindings win, as before.
    ZwpKeyboardShortcutsInhibitorV1,
    // Only create_immed is used, which has no events but `failed`, and that
    // is a protocol error instead.
    ZwpLinuxBufferParamsV1,
);

/// wl_shm without SCTK's format tracking, which would require the state to
/// always own one.
pub struct Shm(WlShm);

impl ProvidesBoundGlobal<WlShm, 1> for Shm {
    fn bound_global(&self) -> Result<WlShm, GlobalError> {
        Ok(self.0.clone())
    }
}

pub struct Globals {
    pub compositor: CompositorState,
    pub subcompositor: SubcompositorState,
    pub shm: Shm,
    pub viewporter: WpViewporter,
    /// Optional; falls back to 1x1 shm buffers.
    pub single_pixel: Option<WpSinglePixelBufferManagerV1>,
    /// Optional; without it, surfaces can't fade.
    pub alpha_modifier: Option<WpAlphaModifierV1>,
    pub layer_shell: LayerShell,
    pub source_manager: ExtForeignToplevelImageCaptureSourceManagerV1,
    /// Optional; without it, outputs can't be captured.
    pub output_source_manager: Option<ExtOutputImageCaptureSourceManagerV1>,
    pub copy_manager: ExtImageCopyCaptureManagerV1,
    /// Optional; captures fall back to shm.
    pub dmabuf: Option<Dmabuf>,
    /// Optional; without it, the compositor's bindings keep firing while a
    /// surface has the keyboard.
    pub shortcuts_inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
}

impl Globals {
    /// Bind everything the shell needs, reporting every missing global.
    pub fn bind<P: Program>(globals: &GlobalList, qh: &QueueHandle<Shell<P>>) -> Option<Self> {
        fn require<T>(name: &str, global: Option<T>) -> Option<T> {
            if global.is_none() {
                warn!("missing global: {name}");
            }
            global
        }
        let compositor = require("wl_compositor", CompositorState::bind(globals, qh).ok());
        let subcompositor = require(
            "wl_subcompositor",
            compositor.as_ref().and_then(|c| {
                SubcompositorState::bind(c.wl_compositor().clone(), globals, qh).ok()
            }),
        );
        let shm: Option<WlShm> = require("wl_shm", globals.bind(qh, 1..=1, Ignore).ok());
        let viewporter: Option<WpViewporter> =
            require("wp_viewporter", globals.bind(qh, 1..=1, Ignore).ok());
        let layer_shell = require("zwlr_layer_shell_v1", LayerShell::bind(globals, qh).ok());
        let source_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1> = require(
            "ext_foreign_toplevel_image_capture_source_manager_v1",
            globals.bind(qh, 1..=1, Ignore).ok(),
        );
        let copy_manager: Option<ExtImageCopyCaptureManagerV1> = require(
            "ext_image_copy_capture_manager_v1",
            globals.bind(qh, 1..=1, Ignore).ok(),
        );

        let new_enough = compositor
            .as_ref()
            .is_none_or(|c| c.wl_compositor().version() >= 4);
        if !new_enough {
            warn!("wl_compositor version 4 or newer required");
        }
        Some(Self {
            compositor: compositor.filter(|_| new_enough)?,
            subcompositor: subcompositor?,
            shm: Shm(shm?),
            viewporter: viewporter?,
            single_pixel: globals.bind(qh, 1..=1, Ignore).ok(),
            alpha_modifier: globals.bind(qh, 1..=1, Ignore).ok(),
            layer_shell: layer_shell?,
            source_manager: source_manager?,
            output_source_manager: globals.bind(qh, 1..=1, Ignore).ok(),
            copy_manager: copy_manager?,
            dmabuf: Dmabuf::bind(globals, qh),
            shortcuts_inhibit: globals.bind(qh, 1..=1, Ignore).ok(),
        })
    }
}
