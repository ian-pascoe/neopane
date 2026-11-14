//! The built-in herdr pane provider's command (ADR 0016, ADR 0017):
//! `provider builtin herdr CAPABILITY`, which its table runs for each
//! capability. It speaks herdr's socket API (`client.rs`) to the server
//! the choosing attach's `HERDR_SOCKET_PATH` names: a herdr server is the
//! pane provider's reach, its workspaces being no boundary. Terminal ids
//! are herdr pane ids, such as `w1:p4`, which survive a live handoff.
//!
//! - `find` (`proof.rs`): `ping` first, refusing a protocol below 22; then
//!   the pane the attaching terminal's `HERDR_PANE_ID` names, proved by
//!   the controlling terminal of its process. `inside.herdr` runs the same
//!   proof, within 0.5 s.
//! - `open_beside` and `close` (`panes.rs`): a new tab running the attach,
//!   whose pane is moved beside the origin, then the tab laid out again;
//!   `pane.close`, done if already gone.
//! - `report_layout` (`layout.rs`): one area per tab of every workspace,
//!   from `session.snapshot`, with the alternate screen's insets; a zoomed
//!   tab's zoomed pane alone, the panes it hides in `<tab id>~hidden`.
//! - `place_beside` and `resize` (`moves.rs`): a move beside the origin,
//!   two within one tab, out to a tab of its own and back, as herdr moves
//!   no pane within its tab; `layout.set_split_ratio` on the lowest split
//!   holding both, its path read from herdr's split ids (`splits.rs`).
//! - `focus` and `focus_direction` (`focus.rs`): `pane.focus`; steps of
//!   `pane.focus_direction` from the server's focus, stopping at the
//!   tab's edge. herdr's focus is one per server: it moves every client.
//! - `open_tab_page` (`tab_page.rs`): `layout.apply`, a tab of the
//!   origin's workspace running the attach, unfocused: a herdr tab is a
//!   tab page area.
//! - `events` (`events.rs`): one `events.subscribe` held open, focus read
//!   from a fresh snapshot, never an event; it runs until herdr ends the
//!   subscription, then exits 1.
//!
//! Each capability gives the line it prints, or why it failed; `run`
//! prints it, or the failure as one `herdr CAPABILITY: why` line on
//! stderr, and gives the exit status.

mod client;
mod events;
#[cfg(test)]
mod fake;
mod focus;
mod layout;
mod moves;
mod panes;
mod proof;
mod splits;
mod tab_page;

pub(crate) use proof::inside;

use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `herdr CAPABILITY: why` line on stderr, and gives its
/// exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    let done = match cap {
        Capability::Find => proof::find(given).map(Some),
        Capability::OpenBeside => panes::open_beside(given).map(Some),
        Capability::Close => panes::close(given).map(|()| None),
        Capability::ReportLayout => layout::report_layout(given).map(Some),
        Capability::Focus => focus::focus(given).map(|()| None),
        Capability::FocusDirection => focus::focus_direction(given).map(|()| None),
        Capability::OpenTabPage => tab_page::open_tab_page(given).map(Some),
        Capability::PlaceBeside => moves::place_beside(given).map(|()| None),
        Capability::Resize => moves::resize(given).map(|()| None),
        // it runs until herdr ends it: exit 1, so the daemon starts it again
        Capability::Events => Err(events::events(given)),
        _ => return UNSUPPORTED,
    };
    match done {
        Ok(Some(line)) => print_line(line.as_bytes()),
        Ok(None) => DONE,
        Err(why) => {
            eprintln!("herdr {}: {why}", cap.name());
            FAILED
        }
    }
}
