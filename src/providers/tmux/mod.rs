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
//! - `open_float` (`float.rs`): `new-pane -d` over the origin's window,
//!   its content on the float group; tmux 3.7 at least, else exit 69.
//! - `report_layout` (`layout.rs`): one area per window of the server,
//!   from `list-panes -a`.
//! - `events` (`events.rs`): a `wait-for -E` waiter per event, a line
//!   after each burst, the focus with it when it moved; tmux 3.8 at
//!   least, else exit 69. It runs until every waiter is gone.
//! - `provider check`'s tmux section (`check.rs`), no capability: the
//!   `tmux` on `PATH`, its version, the server, and what it's too old for.
//! - `inside.tmux` (`inside.rs`), no capability: `find`'s whole check,
//!   run by the built-in default and the config's `<name>.inside.tmux`.
//!
//! Each capability gives the line it prints, or why it failed; `run`
//! prints it, or the failure as one `tmux CAPABILITY: why` line on stderr,
//! and gives the exit status: 69 where the installed tmux can't. Every
//! pane id read or given is `%` and digits (`is_pane_id`).

pub(crate) mod check;
mod events;
mod find;
mod float;
mod focus;
mod inside;
mod layout;
mod panes;
mod resize;
mod server;
mod tab_page;
mod window_layout;

pub(crate) use inside::inside;

use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// Why a capability didn't do it.
enum Failure {
    /// It failed this time: exit 1.
    Failed(String),
    /// The installed tmux can't: exit 69.
    Unsupported(String),
}

impl From<String> for Failure {
    fn from(why: String) -> Failure {
        Failure::Failed(why)
    }
}

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `tmux CAPABILITY: why` line on stderr, and gives its
/// exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    match cap {
        Capability::Find => finish(cap, find::find(given).map(Some)),
        Capability::OpenBeside => finish(cap, panes::open_beside(given).map(Some)),
        Capability::OpenFloat => finish(cap, float::open_float(given).map(Some)),
        Capability::Close => finish(cap, panes::close(given).map(|()| None)),
        Capability::ReportLayout => finish(cap, layout::report_layout(given).map(Some)),
        Capability::Events => finish(cap, Err::<Option<String>, _>(events::events(given))),
        Capability::Resize => finish(cap, resize::resize(given).map(|()| None)),
        Capability::PlaceBeside => finish(cap, resize::place_beside(given).map(|()| None)),
        Capability::Focus => finish(cap, focus::focus(given).map(|()| None)),
        Capability::FocusDirection => finish(cap, focus::focus_direction(given).map(|()| None)),
        Capability::OpenTabPage => finish(cap, tab_page::open_tab_page(given).map(Some)),
        _ => UNSUPPORTED,
    }
}

/// What capability `cap` came to, `done`: the line it prints on stdout,
/// if any, or why it didn't. Prints it; gives the exit status.
fn finish(cap: Capability, done: Result<Option<String>, impl Into<Failure>>) -> u8 {
    match done.map_err(Into::into) {
        Ok(Some(line)) => print_line(line.as_bytes()),
        Ok(None) => DONE,
        Err(Failure::Failed(why)) => {
            eprintln!("tmux {}: {why}", cap.name());
            FAILED
        }
        Err(Failure::Unsupported(why)) => {
            eprintln!("tmux {}: {why}", cap.name());
            UNSUPPORTED
        }
    }
}

/// Whether `id` is a tmux pane id, `%` and digits: anything else, given to
/// tmux's `-t`, could name a session or window, and printed, isn't a pane.
fn is_pane_id(id: &[u8]) -> bool {
    id.strip_prefix(b"%")
        .is_some_and(|n| !n.is_empty() && n.iter().all(u8::is_ascii_digit))
}
