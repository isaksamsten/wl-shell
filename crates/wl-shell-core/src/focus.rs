//! The `mru-event-stream`: a line stream (stream.rs) printing the ID of
//! each window that gains focus, one per line. Each line moves that window
//! to the front of `Core::mru`; the order learned so far stays when the
//! stream restarts.

use crate::mru;
use crate::shell::{Program, Shell};
use crate::stream::{Hooks, LineStream};

/// The stream, not running until configured.
pub(crate) fn stream<P: Program>() -> LineStream<P> {
    LineStream::new(
        "mru-event-stream",
        Hooks {
            locate: locate::<P>,
            line: line::<P>,
            changed: |_| {},
        },
    )
}

fn locate<P: Program>(shell: &mut Shell<P>) -> &mut LineStream<P> {
    &mut shell.core.focus_stream
}

fn line<P: Program>(shell: &mut Shell<P>, line: &str) {
    if let Some(id) = mru::parse_line(line) {
        shell.core.mru.focus(id);
        shell.program.focus_changed(&mut shell.core);
    }
}
