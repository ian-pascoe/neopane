//! The built-in tmux pane provider's command (ADR 0016): `provider builtin
//! tmux CAPABILITY`, which its table runs for each capability. It acts on
//! the tmux server the choosing attach runs in, as `TMUX` names it.
//!
//! - Every tmux call passes the socket from `TMUX` (`-S`), and a command
//!   that acts on a pane or window names it with `-t`, so a `TMUX_PANE`
//!   leaked into the choosing attach can't retarget it (`server.rs`).
//! - `find` (`find.rs`): the pane of the server whose tty is the attach's
//!   `TTY`, compared by device; tmux 3.2 at least.
//! - `open_beside` and `close` (`panes.rs`): `split-window -d` beside the
//!   origin, running the attach; `kill-pane`, done if already gone.
//! - `report_layout` (`layout.rs`): one area per window of the server,
//!   from `list-panes -a`.

mod find;
mod layout;
mod panes;
mod server;

use super::builtin::{Given, UNSUPPORTED};
use crate::config::Capability;

/// Runs capability `cap` with `given`: its exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    match cap {
        Capability::Find => find::find(given),
        Capability::OpenBeside => panes::open_beside(given),
        Capability::Close => panes::close(given),
        Capability::ReportLayout => layout::report_layout(given),
        _ => UNSUPPORTED,
    }
}
