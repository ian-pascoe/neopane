//! What `provider check` learns of the Herdr app, for its herdr-app
//! section: the `herdr-app` the attach's `PATH` or `HOME` finds, its
//! version, and whether the window under its `HERDR_APP_HOME` answers
//! `status`; the hc core `HC_TERMINAL_SOCKET` names, its version and
//! generation from `hello`; and what the table's capabilities need that
//! the core lacks, which exits 69. Both have the choice's 3 s, together.

use std::time::Instant;

use super::core::Core;
use super::methods;
use super::window::Window;
use crate::attach::env_value;
use crate::config::{Attach, Capability, TIME};
use crate::providers::command::commands_env;

/// What the Herdr app is, for an attach with `attach`'s environment, whose
/// table runs the built-in for capabilities `uses`.
pub(crate) fn probe(attach: &Attach, uses: &[Capability]) -> HerdrAppReport {
    let due = Instant::now() + TIME;
    let shown = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
    let window = Window::within(commands_env(&attach.env));
    let (path, version, answers) = match &window {
        Ok(w) => (
            Ok(w.path().display().to_string()),
            w.version().ok(),
            w.status(due).map(|_| ()).map_err(String::from),
        ),
        Err(why) => (Err(why.clone()), None, Err(why.clone())),
    };
    let socket = env_value(&attach.env, "HC_TERMINAL_SOCKET")
        .filter(|s| !s.is_empty())
        .map(shown);
    let core = match &socket {
        None => Err("HC_TERMINAL_SOCKET isn't set".to_string()),
        Some(socket) => Core::connect(
            socket.as_bytes(),
            due.saturating_duration_since(Instant::now()),
        ),
    };
    let (core_version, generation, missing, server) = match core {
        Ok(core) => {
            let missing = uses
                .iter()
                .filter_map(|&capability| {
                    let why = core.lacks(methods(capability))?;
                    Some(Missing { capability, why })
                })
                .collect();
            let hello = core.hello();
            (
                Some(hello.version.clone()),
                hello.generation,
                missing,
                Ok(()),
            )
        }
        Err(why) => (None, None, Vec::new(), Err(why)),
    };
    HerdrAppReport {
        path,
        version,
        home: env_value(&attach.env, "HERDR_APP_HOME")
            .filter(|h| !h.is_empty())
            .map(shown),
        window: answers,
        socket,
        core_version,
        generation,
        core: server,
        missing,
    }
}

/// What `provider check` learned of the Herdr app.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HerdrAppReport {
    /// The `herdr-app` found, else why there's none.
    pub path: Result<String, String>,
    /// As `herdr-app --version` gives it, such as `0.1.16`.
    pub version: Option<String>,
    /// `HERDR_APP_HOME`; None if unset or empty, where the app uses
    /// `~/.config/herdr-app`.
    pub home: Option<String>,
    /// Whether the window answers `status`, else why not.
    pub window: Result<(), String>,
    /// The socket `HC_TERMINAL_SOCKET` names; None if unset or empty.
    pub socket: Option<String>,
    /// As the core's `hello` gives it, such as `0.1.21`.
    pub core_version: Option<String>,
    pub generation: Option<u64>,
    /// Whether the core answers `hello`, else why not.
    pub core: Result<(), String>,
    /// Each capability of the table whose needs the core lacks.
    pub missing: Vec<Missing>,
}

/// A capability the core lacks something for: `find` refuses, the others
/// exit 69.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Missing {
    pub capability: Capability,
    /// What it lacks, such as `the hc core has no terminal.kill`.
    pub why: String,
}
