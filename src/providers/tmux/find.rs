//! `find`: the pane of the choosing attach's tmux server whose tty is the
//! attaching terminal's `TTY`, as tmux tells a client inside it, compared
//! by device (`providers::tty`). It prints its id, `%N`, else exits 1.
//! `TMUX`, `TMUX_PANE` and `TERM_PROGRAM` leak into terminals tmux doesn't
//! run, such as zellij's inside tmux, so only the tty proves a pane. It
//! refuses a tmux older than 3.2, naming its version.

use super::server::{OLDEST, Server, too_old};
use crate::providers::builtin::{FAILED, Given, print_line};
use crate::providers::tty;

pub(super) fn find(given: &Given) -> u8 {
    match found(given) {
        Ok(pane) => print_line(pane.as_bytes()),
        Err(why) => {
            eprintln!("tmux find: {why}");
            FAILED
        }
    }
}

/// The attaching terminal's pane, or why there's none.
fn found(given: &Given) -> Result<String, String> {
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
            (tty::of_path(path) == Some(device)).then(|| String::from_utf8_lossy(id).into_owned())
        })
        .next()
        .ok_or_else(|| format!("no pane of this tmux server has tty {shown}"))
}
