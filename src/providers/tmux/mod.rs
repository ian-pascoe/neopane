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
//!
//! Each capability gives the line it prints, or why it failed; `run`
//! prints it, or the failure as one `tmux CAPABILITY: why` line on stderr,
//! and gives the exit status. Every pane id read or given is `%` and
//! digits (`is_pane_id`).

mod find;
mod layout;
mod panes;
mod server;

use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// What a capability came to: the line it prints on stdout, if any, or
/// why it failed.
type Done = Result<Option<String>, String>;

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `tmux CAPABILITY: why` line on stderr, and gives its
/// exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    let done = match cap {
        Capability::Find => find::find(given).map(Some),
        Capability::OpenBeside => panes::open_beside(given).map(Some),
        Capability::Close => panes::close(given).map(|()| None),
        Capability::ReportLayout => layout::report_layout(given).map(Some),
        _ => return UNSUPPORTED,
    };
    finish(cap, done)
}

fn finish(cap: Capability, done: Done) -> u8 {
    match done {
        Ok(Some(line)) => print_line(line.as_bytes()),
        Ok(None) => DONE,
        Err(why) => {
            eprintln!("tmux {}: {why}", cap.name());
            FAILED
        }
    }
}

/// Whether `id` is a tmux pane id, `%` and digits: anything else, given to
/// tmux's `-t`, could name a session or window, and printed, isn't a pane.
fn is_pane_id(id: &[u8]) -> bool {
    id.strip_prefix(b"%")
        .is_some_and(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit))
}
