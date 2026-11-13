//! `open_tab_page`: a tmux window is a tab page area. `new-window -d`
//! makes one after `ORIGIN`'s window, named by its id, running the attach
//! command, and leaves the session's current window alone: the daemon
//! focuses the new pane when Neovim goes to its tab page. A window zoomed
//! stays zoomed.

use std::ffi::OsString;

use super::panes::{attach_words, pane_id, printed_pane};
use super::server::Server;
use crate::providers::builtin::Given;

/// The new window's pane, or why there's none.
pub(super) fn open_tab_page(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let origin = pane_id(given, "origin")?;
    let words = attach_words(given)?;
    // new-window takes a window, not a pane
    let window = server.run(&["display-message", "-p", "-t", &origin, "#{window_id}"])?;
    let window = String::from_utf8_lossy(&window).trim().to_string();
    if !window.starts_with('@') {
        return Err(format!("{origin}'s window is {window:?}"));
    }
    let mut args: Vec<OsString> = [
        "new-window",
        "-d",
        "-a",
        "-t",
        &window,
        "-P",
        "-F",
        "#{pane_id}",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.extend(words);
    printed_pane(&server.run(&args)?, "new-window")
}
