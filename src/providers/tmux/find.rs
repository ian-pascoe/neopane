//! `find`: the pane of the choosing attach's tmux server whose tty is the
//! attaching terminal's `TTY`, as tmux tells a client inside it, compared
//! by device (`providers::tty`). It prints its id, `%N`, else exits 1.
//! `TMUX`, `TMUX_PANE` and `TERM_PROGRAM` leak into terminals tmux doesn't
//! run, such as zellij's inside tmux, so only the tty proves a pane. It
//! refuses a tmux older than 3.2, naming its version.

use super::is_pane_id;
use super::server::{OLDEST, Server, too_old};
use crate::providers::builtin::Given;
use crate::providers::tty;

/// The attaching terminal's pane, or why there's none.
pub(super) fn find(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let version = Server::version()?;
    if too_old(&version) {
        let (major, minor) = OLDEST;
        return Err(format!(
            "tmux {version} is too old: the built-in tmux needs {major}.{minor} or newer"
        ));
    }
    let tty = given.var("tty").unwrap_or_default();
    let shown = String::from_utf8_lossy(&tty).into_owned();
    let device =
        tty::of_path(&tty).ok_or_else(|| format!("the attach's tty {shown:?} isn't one"))?;
    let panes = server.run(&["list-panes", "-a", "-F", "#{pane_id} #{pane_tty}"])?;
    panes
        .split(|&b| b == b'\n')
        .filter_map(|line| {
            let at = line.iter().position(|&b| b == b' ')?;
            let (id, path) = (&line[..at], &line[at + 1..]);
            let ours = is_pane_id(id) && tty::of_path(path) == Some(device);
            ours.then(|| String::from_utf8_lossy(id).into_owned())
        })
        .next()
        .ok_or_else(|| format!("no pane of this tmux server has tty {shown}"))
}
