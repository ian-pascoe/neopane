//! `inside.tmux`: whether an attach's terminal is directly a pane of the
//! tmux server its `TMUX` names. It runs `find`'s whole check, the version
//! included, so it proves exactly when `find` would accept, and says why
//! as `find` would, but for an unset `TMUX` or tty. `TMUX_PANE` isn't
//! needed. tmux has [`INSIDE_TIME`] to answer, both runs together.

use std::time::Instant;

use super::find::pane_of;
use super::server::{Ran, Server, version_of};
use crate::config::{Attach, INSIDE_TIME, Proof};
use crate::providers::command::commands_env;
use crate::util::seconds;

/// Whether `attach`'s terminal is a pane of its tmux server, and why.
pub(crate) fn inside(attach: &Attach) -> Proof {
    match pane(attach) {
        Ok(pane) => Proof {
            ok: true,
            why: format!("tmux pane {pane}"),
        },
        Err(why) => Proof { ok: false, why },
    }
}

fn pane(attach: &Attach) -> Result<String, String> {
    let tmux = crate::attach::env_value(&attach.env, "TMUX").unwrap_or_default();
    let server = Server::named_by(tmux).ok_or("TMUX isn't set")?;
    if attach.tty.is_empty() {
        return Err("the attach has no tty".into());
    }
    let env = commands_env(&attach.env);
    let due = Instant::now() + INSIDE_TIME;
    let run = |args: &[&str]| {
        let left = due.saturating_duration_since(Instant::now());
        server
            .run_within(args, &env, left)
            .map_err(|ran| match ran {
                Ran::CantRun(e) => format!("can't run tmux: {e}"),
                Ran::Failed(line) => format!("tmux: {line}"),
                Ran::TimedOut => format!("tmux didn't answer in {} s", seconds(INSIDE_TIME)),
            })
    };
    pane_of(&server, &attach.tty, run, || {
        run(&["-V"]).map(|out| version_of(&out))
    })
}
