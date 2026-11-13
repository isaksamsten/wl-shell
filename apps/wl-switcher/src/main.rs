//! wl-switcher: an Alt+Tab app switcher for Wayland that prints the picked
//! window's ID. See PLAN.md for the design.

mod anim;
mod app;
mod bar;
mod cache;
mod config;
mod input;
mod ipc;
mod options;
mod overlay;
mod strip;
mod switcher;

use std::process::ExitCode;

use wl_shell_core::Error;
use wl_shell_log::warn;

use crate::app::WlSwitcher;
use crate::options::Options;

// Exit codes: 0 picked (the ID is on stdout), 1 cancelled or an error.

fn main() -> ExitCode {
    wl_shell_log::set_name("wl-switcher");
    let opts = match Options::parse_args() {
        Ok(opts) => opts,
        Err(code) => return code,
    };
    if !opts.daemon {
        return wl_shell_daemon::client(&opts.request());
    }
    match daemon(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn daemon(opts: &Options) -> Result<(), Error> {
    let switcher = WlSwitcher::new(opts.config.clone())?;
    wl_shell_daemon::run(switcher)
}
