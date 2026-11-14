//! What `provider check` learns of zellij, for its zellij section: the
//! `zellij` on the attach's `PATH`, its version against the floor the
//! built-in's `find` refuses below ([`OLDEST`]), the session
//! `ZELLIJ_SESSION_NAME` names, the socket directory `ZELLIJ_SOCKET_DIR`
//! names, and whether the session answers `list-panes`, as `find` asks it.
//! zellij has the choice's 3 s to answer, both runs together.

use std::time::Instant;

use super::session::{OLDEST, Ran, Session, is_too_old, version_within};
use crate::attach::env_value;
use crate::config::{Attach, TIME};
use crate::providers::command::{commands_env, on_path};
use crate::util::seconds;

/// What zellij is, for an attach with `attach`'s environment, whose
/// table runs the built-in.
pub(crate) fn probe(attach: &Attach) -> ZellijReport {
    let env = commands_env(&attach.env);
    let due = Instant::now() + TIME;
    let shown = |ran: Ran| match ran {
        Ran::TimedOut(_) => format!("no answer within {} s", seconds(TIME)),
        other => other.to_string(),
    };
    let path = on_path(env_value(&attach.env, "PATH").unwrap_or_default(), "zellij");
    let version = version_within(&env, due).ok();
    let name = env_value(&attach.env, "ZELLIJ_SESSION_NAME").unwrap_or_default();
    let socket_dir = env_value(&attach.env, "ZELLIJ_SOCKET_DIR")
        .filter(|dir| !dir.is_empty())
        .map(|dir| String::from_utf8_lossy(dir).into_owned());
    let server = if name.is_empty() {
        Err("ZELLIJ_SESSION_NAME isn't set".to_string())
    } else {
        Session::within(name, env)
            .run_until(None, &["list-panes", "--json"], due)
            .map(|_| ())
            .map_err(shown)
    };
    ZellijReport {
        path,
        missing: version
            .as_deref()
            .filter(|v| is_too_old(v))
            .map(|_| Missing { needs: OLDEST })
            .into_iter()
            .collect(),
        version,
        session: (!name.is_empty()).then(|| String::from_utf8_lossy(name).into_owned()),
        socket_dir,
        server,
    }
}

/// What `provider check` learned of zellij.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ZellijReport {
    /// The `zellij` the attach's `PATH` finds; None if it finds none.
    pub path: Option<String>,
    /// As `zellij --version` gives it after `zellij `, such as `0.46.0`.
    pub version: Option<String>,
    /// The session `ZELLIJ_SESSION_NAME` names; None if it's unset or
    /// empty.
    pub session: Option<String>,
    /// The directory `ZELLIJ_SOCKET_DIR` names; None if it's unset or
    /// empty, where zellij looks in its own.
    pub socket_dir: Option<String>,
    /// Whether the session answers, else why not.
    pub server: Result<(), String>,
    /// What this zellij is too old for.
    pub missing: Vec<Missing>,
}

/// Something this zellij is too old for: every capability, below the
/// floor, where `find` refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Missing {
    /// The first zellij the built-in works with.
    pub needs: (u32, u32, u32),
}
