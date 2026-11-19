//! Outputs, identified: what a program needs to know to put something on
//! a monitor, and to let a config name that monitor.
//!
//! [`Core::outputs`] wraps SCTK's `OutputInfo` in [`Output`], so no program
//! reads `OutputInfo` itself. An output has an [`Ident`] (connector name,
//! description, make and model: what a config pattern is matched against)
//! and a unique [`Output::key`], the name the program uses for it. The key
//! exists for every output, also for those a compositor gives no name
//! (neither wl_output v4 nor xdg-output v2), so none is left without a dock
//! or a column.
//!
//! [`list`] is the same on a connection of its own, for `<app> outputs`.

use smithay_client_toolkit::{
    delegate_dispatch2, delegate_registry,
    output::{OutputHandler, OutputInfo, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
};
use wayland_client::{
    Connection, Proxy, QueueHandle, globals::registry_queue_init, protocol::wl_output::WlOutput,
};
use wl_shell_log::{info, warn};

use crate::shell::{Core, Error, Program};

/// What identifies an output, for matching it against the config.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ident {
    /// The connector, from wl_output v4 or xdg-output v2.
    pub name: Option<String>,
    /// From wl_output v4 or xdg-output v2; may change at any time.
    pub description: Option<String>,
    /// Empty if the compositor says nothing.
    pub make: String,
    pub model: String,
}

impl Ident {
    /// `"<make> <model>"`, without the parts that are empty; empty if both
    /// are.
    pub fn make_model(&self) -> String {
        let (make, model) = (self.make.trim(), self.model.trim());
        match (make.is_empty(), model.is_empty()) {
            (true, _) => model.to_string(),
            (_, true) => make.to_string(),
            _ => format!("{make} {model}"),
        }
    }
}

impl From<&OutputInfo> for Ident {
    fn from(info: &OutputInfo) -> Self {
        Ident {
            name: info.name.clone().filter(|n| !n.is_empty()),
            description: info.description.clone().filter(|d| !d.is_empty()),
            make: info.make.clone(),
            model: info.model.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Output {
    pub wl: WlOutput,
    pub ident: Ident,
    /// Unique among the current outputs; the name the program uses for it.
    pub key: String,
    pub scale: i32,
    pub logical_size: Option<(i32, i32)>,
}

/// The key of every output in `outputs` (their registry global numbers
/// and idents), in the same order. A key is the connector name; without
/// one `"<make> <model>"`; without those `output-<n>`. If two outputs
/// would get the same key, the later one gets `#2`, `#3`, and so on.
pub fn keys<'a>(outputs: impl IntoIterator<Item = (u32, &'a Ident)>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for (id, ident) in outputs {
        let base = match (&ident.name, ident.make_model()) {
            (Some(name), _) => name.clone(),
            (None, make_model) if !make_model.is_empty() => make_model,
            _ => format!("output-{id}"),
        };
        let mut key = base.clone();
        let mut n = 2;
        while keys.contains(&key) {
            key = format!("{base}#{n}");
            n += 1;
        }
        keys.push(key);
    }
    keys
}

/// Every output of `state` with complete state, with keys.
fn collect(state: &OutputState) -> Vec<Output> {
    let infos: Vec<(WlOutput, OutputInfo)> = (state.outputs())
        .filter_map(|wl| {
            let info = state.info(&wl)?;
            Some((wl, info))
        })
        .collect();
    let idents: Vec<Ident> = infos.iter().map(|(_, info)| Ident::from(info)).collect();
    let keys = keys(infos.iter().map(|(_, info)| info.id).zip(&idents));
    infos
        .into_iter()
        .zip(idents)
        .zip(keys)
        .map(|(((wl, info), ident), key)| Output {
            wl,
            ident,
            key,
            scale: info.scale_factor.max(1),
            logical_size: info.logical_size,
        })
        .collect()
}

impl<P: Program> Core<P> {
    /// The outputs whose state is complete, in the compositor's order.
    pub fn outputs(&self) -> Vec<Output> {
        collect(&self.output_state)
    }

    /// `wl`, if its state is complete.
    pub fn output(&self, wl: &WlOutput) -> Option<Output> {
        self.outputs().into_iter().find(|o| &o.wl == wl)
    }

    /// Log `wl` if it is new or its ident changed, and warn once if the
    /// compositor gives it no name. Called before the program hears of it.
    pub(crate) fn note_output(&mut self, wl: &WlOutput) {
        let Some(output) = self.output(wl) else {
            return;
        };
        let Some(id) = self.output_state.info(wl).map(|info| info.id) else {
            return;
        };
        let Some(first) = self.output_seen.update(id, &output.ident) else {
            return;
        };
        if first && output.ident.name.is_none() {
            warn!(
                "output has no name (wl_output v{}, no xdg-output v2); matching it by make and model",
                wl.version()
            );
        }
        info!("{}", describe(&output));
    }

    /// `wl` is gone.
    pub(crate) fn forget_output(&mut self, wl: &WlOutput) {
        if let Some(info) = self.output_state.info(wl) {
            self.output_seen.forget(info.id);
        }
    }
}

/// `output DP-2: "Dell Inc. DELL U2720Q 8XYZ123 (DP-2)", 2560x1440, scale 2`
pub fn describe(output: &Output) -> String {
    let what = match &output.ident.description {
        Some(description) => description.clone(),
        None => output.ident.make_model(),
    };
    let mut line = format!("output {}: \"{what}\"", output.key);
    if let Some((w, h)) = output.logical_size {
        line.push_str(&format!(", {w}x{h}"));
    }
    line.push_str(&format!(", scale {}", output.scale));
    line
}

/// The idents last logged, by registry global number.
#[derive(Default)]
pub(crate) struct Seen(Vec<(u32, Ident)>);

impl Seen {
    /// Remember `ident` for `id`. None if it is the one remembered already;
    /// else whether the output is new.
    fn update(&mut self, id: u32, ident: &Ident) -> Option<bool> {
        match self.0.iter_mut().find(|(i, _)| *i == id) {
            Some((_, seen)) if seen == ident => None,
            Some((_, seen)) => {
                *seen = ident.clone();
                Some(false)
            }
            None => {
                self.0.push((id, ident.clone()));
                Some(true)
            }
        }
    }

    fn forget(&mut self, id: u32) {
        self.0.retain(|(i, _)| *i != id);
    }
}

// ---------------------------------------------------------------------------
// list

/// The outputs, from a connection of its own: no `Shell`, so it works
/// whether or not a program is running.
pub fn list() -> Result<Vec<Output>, Error> {
    let conn = Connection::connect_to_env()
        .map_err(|e| format!("failed to connect to wayland display: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<Lister>(&conn)?;
    let qh = queue.handle();
    let mut lister = Lister {
        registry: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
    };
    // The first roundtrip delivers the outputs' state, the second what
    // xdg-output adds.
    queue.roundtrip(&mut lister)?;
    queue.roundtrip(&mut lister)?;
    Ok(collect(&lister.output_state))
}

struct Lister {
    registry: RegistryState,
    output_state: OutputState,
}

impl OutputHandler for Lister {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ProvidesRegistryState for Lister {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState];
}

delegate_registry!(Lister);
delegate_dispatch2!(Lister);

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(name: Option<&str>, make: &str, model: &str) -> Ident {
        Ident {
            name: name.map(String::from),
            description: None,
            make: make.into(),
            model: model.into(),
        }
    }

    fn keys_of(idents: &[Ident]) -> Vec<String> {
        keys(idents.iter().enumerate().map(|(i, id)| (i as u32 + 10, id)))
    }

    #[test]
    fn key_is_the_connector_name() {
        let idents = [ident(Some("DP-2"), "Dell Inc.", "DELL U2720Q")];
        assert_eq!(keys_of(&idents), ["DP-2"]);
    }

    #[test]
    fn key_without_a_name_is_make_and_model() {
        let idents = [
            ident(None, "Dell Inc.", "DELL U2720Q"),
            ident(None, "BOE", ""),
            ident(None, "", "0x0BCA"),
        ];
        assert_eq!(keys_of(&idents), ["Dell Inc. DELL U2720Q", "BOE", "0x0BCA"]);
    }

    #[test]
    fn key_without_anything_is_the_global_number() {
        let idents = [ident(None, "", ""), ident(None, " ", "")];
        assert_eq!(keys_of(&idents), ["output-10", "output-11"]);
    }

    #[test]
    fn duplicate_keys_get_a_counter_on_the_later_output() {
        let twin = ident(None, "Dell Inc.", "DELL U2720Q");
        let idents = [
            twin.clone(),
            ident(Some("DP-1"), "", ""),
            twin.clone(),
            twin,
        ];
        assert_eq!(
            keys_of(&idents),
            [
                "Dell Inc. DELL U2720Q",
                "DP-1",
                "Dell Inc. DELL U2720Q#2",
                "Dell Inc. DELL U2720Q#3",
            ]
        );
    }

    #[test]
    fn a_counter_never_collides_with_a_real_key() {
        let idents = [
            ident(Some("X"), "", ""),
            ident(Some("X#2"), "", ""),
            ident(Some("X"), "", ""),
        ];
        assert_eq!(keys_of(&idents), ["X", "X#2", "X#3"]);
    }

    #[test]
    fn make_model_skips_what_is_empty() {
        assert_eq!(ident(None, "A", "B").make_model(), "A B");
        assert_eq!(ident(None, "A", "").make_model(), "A");
        assert_eq!(ident(None, "", "B").make_model(), "B");
        assert_eq!(ident(None, "", "").make_model(), "");
    }

    #[test]
    fn seen_reports_new_changed_and_unchanged() {
        let mut seen = Seen::default();
        let a = ident(Some("DP-1"), "", "");
        let b = ident(Some("DP-2"), "", "");
        assert_eq!(seen.update(1, &a), Some(true));
        assert_eq!(seen.update(1, &a), None);
        assert_eq!(seen.update(1, &b), Some(false));
        seen.forget(1);
        assert_eq!(seen.update(1, &b), Some(true));
    }
}
