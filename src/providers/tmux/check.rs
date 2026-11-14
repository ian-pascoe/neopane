//! What `provider check` learns of tmux, for its tmux section: the `tmux`
//! on the attach's `PATH`, its version, the server `TMUX` names and
//! whether it answers, and each capability of the table in use that this
//! tmux is too old for, which exits 69. tmux has the choice's 3 s to
//! answer, both runs together.

use std::os::unix::ffi::OsStrExt;
use std::time::Instant;

use super::events::FIRST_WITH_EVENTS;
use super::float::FIRST_WITH_FLOATS;
use super::server::{OLDEST, Ran, Server, older_than, run_within, version_of};
use crate::attach::env_value;
use crate::config::{Attach, Capability, TIME};
use crate::providers::command::{commands_env, on_path};
use crate::util::seconds;

/// The capabilities that need a tmux newer than [`OLDEST`], and the
/// first tmux that has each.
const GATED: [(Capability, (u32, u32)); 2] = [
    (Capability::OpenFloat, FIRST_WITH_FLOATS),
    (Capability::Events, FIRST_WITH_EVENTS),
];

/// What tmux is, for an attach with `attach`'s environment, whose table
/// runs the built-in for capabilities `uses`.
pub(crate) fn probe(attach: &Attach, uses: &[Capability]) -> TmuxReport {
    let env = commands_env(&attach.env);
    let due = Instant::now() + TIME;
    let run = |args: &[&[u8]]| {
        let left = due.saturating_duration_since(Instant::now());
        let args: Vec<&std::ffi::OsStr> = args
            .iter()
            .map(|a| std::ffi::OsStr::from_bytes(a))
            .collect();
        run_within(&args, &env, left).map_err(|ran| match ran {
            Ran::CantRun(e) => format!("can't run tmux: {e}"),
            Ran::Failed(line) => line,
            Ran::TimedOut => format!("no answer within {} s", seconds(TIME)),
        })
    };
    let path = on_path(env_value(&attach.env, "PATH").unwrap_or_default(), "tmux");
    let version = run(&[b"-V"]).ok().map(|out| version_of(&out));
    let socket =
        Server::named_by(env_value(&attach.env, "TMUX").unwrap_or_default()).map(|s| s.socket());
    let server = match &socket {
        None => Err("TMUX isn't set".to_string()),
        Some(socket) => run(&[b"-S", socket.as_bytes(), b"list-sessions"]).map(|_| ()),
    };
    TmuxReport {
        path,
        missing: version
            .as_deref()
            .map_or_else(Vec::new, |v| missing(v, uses)),
        version,
        socket,
        server,
    }
}

/// What tmux `version` is too old for, of capabilities `uses`: every
/// one, below the floor; else each gated one it lacks.
fn missing(version: &str, uses: &[Capability]) -> Vec<Missing> {
    if older_than(version, OLDEST) {
        return vec![Missing {
            capability: None,
            needs: OLDEST,
        }];
    }
    GATED
        .into_iter()
        .filter(|(cap, first)| uses.contains(cap) && older_than(version, *first))
        .map(|(cap, needs)| Missing {
            capability: Some(cap),
            needs,
        })
        .collect()
}

/// What `provider check` learned of tmux.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TmuxReport {
    /// The `tmux` the attach's `PATH` finds; None if it finds none.
    pub path: Option<String>,
    /// As `tmux -V` gives it after `tmux `, such as `3.4`.
    pub version: Option<String>,
    /// The socket `TMUX` names; None if it's unset.
    pub socket: Option<String>,
    /// Whether the server answers, else why not.
    pub server: Result<(), String>,
    /// What this tmux is too old for.
    pub missing: Vec<Missing>,
}

/// Something this tmux is too old for: a capability, which exits 69, or
/// every one, below the floor, where `find` refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Missing {
    /// None: every capability.
    pub capability: Option<Capability>,
    /// The first tmux that has it, major and minor.
    pub needs: (u32, u32),
}
