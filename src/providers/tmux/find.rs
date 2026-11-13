//! `find`: the pane of the choosing attach's tmux server whose tty is the
//! attaching terminal's `TTY`, as tmux tells a client inside it, compared
//! by device (`providers::tty`). It prints its id, `%N`, else exits 1.
//! `TMUX`, `TMUX_PANE` and `TERM_PROGRAM` leak into terminals tmux doesn't
//! run, such as zellij's inside tmux, so only the tty proves a pane. It
//! refuses a tmux older than 3.2, naming its version. `inside.tmux`
//! (`inside.rs`) runs the same check, so it proves exactly when this
//! accepts.

use super::is_pane_id;
use super::server::{OLDEST, Server, too_old};
use crate::providers::builtin::Given;
use crate::providers::tty;

/// The attaching terminal's pane, or why there's none.
pub(super) fn find(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let tty = given.var("tty").unwrap_or_default();
    pane_of(&server, &tty, |args| server.run(args), Server::version)
}

/// The pane of `server` whose tty is `tty`, or why there's none: `find`'s
/// whole check, running tmux through `run` for the listing and `version`
/// for its version, as `tmux -V` prints it after `tmux `.
pub(super) fn pane_of(
    server: &Server,
    tty: &[u8],
    run: impl FnOnce(&[&str]) -> Result<Vec<u8>, String>,
    version: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    let version = version()?;
    if too_old(&version) {
        let (major, minor) = OLDEST;
        return Err(format!(
            "tmux {version} is too old: the built-in tmux needs {major}.{minor} or newer"
        ));
    }
    let shown = String::from_utf8_lossy(tty).into_owned();
    let device =
        tty::of_path(tty).ok_or_else(|| format!("the attach's tty {shown:?} isn't one"))?;
    let panes = run(&LIST_TTYS)?;
    pane_with(&panes, device).ok_or_else(|| not_a_pane(&shown, server))
}

/// The arguments that list every pane of the server with its tty.
pub(super) const LIST_TTYS: [&str; 4] = ["list-panes", "-a", "-F", "#{pane_id} #{pane_tty}"];

/// The pane in `listing`, as [`LIST_TTYS`] prints it, whose tty is
/// `device`.
pub(super) fn pane_with(listing: &[u8], device: u64) -> Option<String> {
    listing
        .split(|&b| b == b'\n')
        .filter_map(|line| {
            let at = line.iter().position(|&b| b == b' ')?;
            let (id, path) = (&line[..at], &line[at + 1..]);
            let ours = is_pane_id(id) && tty::of_path(path) == Some(device);
            ours.then(|| String::from_utf8_lossy(id).into_owned())
        })
        .next()
}

/// Why terminal `tty` is none of `server`'s panes.
pub(super) fn not_a_pane(tty: &str, server: &Server) -> String {
    format!(
        "this terminal ({tty}) isn't a pane of the tmux server at {}",
        server.socket()
    )
}
