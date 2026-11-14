//! wl-dock: a macOS-style dock for Wayland. See PLAN.md for the design.

mod app;
mod config;
mod dock;
mod layout;
mod options;
mod row;
mod view;
mod zoom;

use std::process::ExitCode;

use wl_shell_core::{Error, Shell};
use wl_shell_log::warn;

use crate::app::WlDock;
use crate::options::Options;

fn main() -> ExitCode {
    wl_shell_log::set_name("wl-dock");
    let opts = match Options::parse_args() {
        Ok(opts) => opts,
        Err(code) => return code,
    };
    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The dock is always shown, so unlike wl-switcher it has no client/show
/// cycle: it runs its own loop until the connection fails.
fn run(opts: &Options) -> Result<(), Error> {
    let dock = WlDock::new(opts.config.clone())?;
    let (mut shell, mut event_loop) = Shell::connect(dock)?;
    shell.program.started(&mut shell.core)?;
    loop {
        event_loop
            .dispatch(None, &mut shell)
            .map_err(|e| format!("wayland connection error: {e}"))?;
        let Shell { core, program } = &mut shell;
        program.after_dispatch(core);
    }
}
