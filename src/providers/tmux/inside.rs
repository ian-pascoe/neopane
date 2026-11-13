//! `inside.tmux`: whether an attach's terminal is directly a pane of the
//! tmux server its `TMUX` names, by the same tty search as `find`, so it
//! proves exactly when `find` would accept (bar a tmux older than 3.2,
//! whose panes get no `TERM_PROGRAM=tmux` for the default to name it by).
//! `TMUX_PANE` isn't needed. tmux has [`INSIDE_TIME`] to answer.

use super::find::{LIST_TTYS, not_a_pane, pane_with};
use super::server::{Ran, Server};
use crate::config::{Attach, INSIDE_TIME, Proof};
use crate::providers::command::commands_env;
use crate::providers::tty;
use crate::util::seconds;

/// Whether `attach`'s terminal is a pane of its tmux server, and why.
pub(crate) fn inside(attach: &Attach) -> Proof {
    match pane_of(attach) {
        Ok(pane) => Proof {
            ok: true,
            why: format!("tmux pane {pane}"),
        },
        Err(why) => Proof { ok: false, why },
    }
}

fn pane_of(attach: &Attach) -> Result<String, String> {
    let tmux = crate::attach::env_value(&attach.env, "TMUX").unwrap_or_default();
    let server = Server::named_by(tmux).ok_or("TMUX isn't set")?;
    let device = tty::of_path(&attach.tty).ok_or("the attach has no tty")?;
    let env = commands_env(&attach.env);
    let listing = server
        .run_within(&LIST_TTYS, &env, INSIDE_TIME)
        .map_err(|ran| match ran {
            Ran::CantRun(e) => format!("can't run tmux: {e}"),
            Ran::Failed(line) => format!("tmux: {line}"),
            Ran::TimedOut => format!("tmux didn't answer in {} s", seconds(INSIDE_TIME)),
        })?;
    pane_with(&listing, device)
        .ok_or_else(|| not_a_pane(&String::from_utf8_lossy(&attach.tty), &server))
}
