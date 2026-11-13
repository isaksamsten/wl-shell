//! wl-expose: pick a window from an exposé-style overlay and print its ID.

mod app;
mod assets;
mod background;
mod config;
mod ipc;
mod layout;
mod options;
mod overlay;

use std::process::ExitCode;

use wl_shell_core::{Error, toplevel};
use wl_shell_log::warn;

use crate::app::Expose;
use crate::options::Options;

fn main() -> ExitCode {
    wl_shell_log::set_name("wl-expose");
    let opts = match Options::parse_args() {
        Ok(opts) => opts,
        Err(code) => return code,
    };
    let result = if opts.daemon {
        daemon(&opts)
    } else if opts.list {
        list()
    } else {
        return wl_shell_daemon::client(&opts.request());
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            warn!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn daemon(opts: &Options) -> Result<(), Error> {
    let expose = Expose::new(opts.config.clone())?;
    wl_shell_daemon::run(expose)
}

/// `--list`: print the window list; needs no daemon.
fn list() -> Result<(), Error> {
    print!(
        "{}",
        toplevel::format_list(&wl_shell_core::list::toplevels()?)
    );
    Ok(())
}
