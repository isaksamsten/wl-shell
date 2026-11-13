//! Diagnostics on stderr, prefixed with the program's name, as in
//! `wl-expose: no window with ID '3'`. The program sets its name once at
//! startup; this is the lowest crate of the stack, so every other one can
//! use it.

use std::sync::OnceLock;

static NAME: OnceLock<&'static str> = OnceLock::new();

/// Set the prefix for every message; only the first call counts.
pub fn set_name(name: &'static str) {
    let _ = NAME.set(name);
}

/// The program's name, as set by `set_name`.
pub fn name() -> &'static str {
    NAME.get().copied().unwrap_or("wl-shell")
}

/// `eprintln!` with the program's name in front.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        eprintln!("{}: {}", $crate::name(), format_args!($($arg)*))
    };
}
