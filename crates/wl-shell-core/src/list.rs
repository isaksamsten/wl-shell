//! The window list on its own, for a quick `--list`: connects without a
//! program and without binding anything but the toplevel list.

use smithay_client_toolkit::{
    delegate_dispatch2, delegate_registry,
    foreign_toplevel_list::{ForeignToplevelList, ForeignToplevelListHandler},
    registry::{ProvidesRegistryState, RegistryState},
};
use wayland_client::{Connection, QueueHandle, globals::registry_queue_init};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

use crate::shell::{Error, require_toplevel_list};
use crate::toplevel::Toplevel;

struct ListState {
    registry: RegistryState,
    toplevels: ForeignToplevelList,
}

/// Open windows with their state, in announcement order.
pub fn toplevels() -> Result<Vec<Toplevel>, Error> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("failed to connect to wayland display: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<ListState>(&conn)?;
    require_toplevel_list(&globals)?;
    let qh = queue.handle();
    let mut state = ListState {
        registry: RegistryState::new(&globals),
        toplevels: ForeignToplevelList::new(&globals, &qh),
    };
    // The first roundtrip delivers the handles, the second their state.
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    let list = &state.toplevels;
    Ok((list.toplevels().iter())
        .filter_map(|handle| {
            let info = list.info(handle)?;
            Some(Toplevel {
                identifier: info.identifier,
                app_id: info.app_id,
                title: info.title,
            })
        })
        .collect())
}

impl ForeignToplevelListHandler for ListState {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        &mut self.toplevels
    }

    fn new_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }

    fn update_toplevel(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }

    fn toplevel_closed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: ExtForeignToplevelHandleV1,
    ) {
    }
}

impl ProvidesRegistryState for ListState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    smithay_client_toolkit::registry_handlers![];
}

delegate_registry!(ListState);
delegate_dispatch2!(ListState);
