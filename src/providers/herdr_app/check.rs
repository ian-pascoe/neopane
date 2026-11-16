//! What `provider check` learns of the Herdr app, for its herdr-app
//! section: the `herdr-app` the attach's `PATH` or `HOME` finds, its
//! version, and whether the window under its `HERDR_APP_HOME` answers
//! `status`; the hc core `HC_TERMINAL_SOCKET` names, its version and
//! generation from `hello`; and what the table's capabilities need that
//! the core lacks, which exits 69. Both have the choice's 3 s, together.

use std::time::{Duration, Instant};

use super::core::Core;
use super::methods;
use super::window::Window;
use crate::attach::env_value;
use crate::config::{Attach, Capability, TIME};
use crate::providers::command::commands_env;

/// What the Herdr app is, for an attach with `attach`'s environment, whose
/// table runs the built-in for capabilities `uses`.
pub(crate) fn probe(attach: &Attach, uses: &[Capability]) -> HerdrAppReport {
    probe_within(attach, uses, TIME, VERSION_TIME)
}

/// The most of the probe's time `herdr-app --version` has: a hung one
/// leaves the window and core the rest.
const VERSION_TIME: Duration = Duration::from_millis(500);

/// `probe`, all of it having `time`, and `herdr-app --version` at most
/// `version_time` of it.
fn probe_within(
    attach: &Attach,
    uses: &[Capability],
    time: Duration,
    version_time: Duration,
) -> HerdrAppReport {
    let due = Instant::now() + time;
    let shown = |v: &[u8]| String::from_utf8_lossy(v).into_owned();
    let window = Window::within(commands_env(&attach.env));
    let (path, version, answers) = match &window {
        Ok(w) => (
            Ok(w.path().display().to_string()),
            w.version(due.min(Instant::now() + version_time)).ok(),
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fake::{FakeApp, FakeCore, recorded_one, recorded_run};
    use super::*;

    /// Well over the real times, as the fakes run shell scripts.
    const LONG: Duration = Duration::from_secs(5);

    /// An attach whose `PATH` finds `app` and whose `HC_TERMINAL_SOCKET`
    /// names `core`.
    fn attach(core: &FakeCore, app: &FakeApp) -> Attach {
        let env = vec![
            (b"HC_TERMINAL_SOCKET".to_vec(), core.path()),
            (
                b"PATH".to_vec(),
                app.dir().to_string_lossy().into_owned().into_bytes(),
            ),
        ];
        Attach {
            env,
            ..Attach::default()
        }
    }

    #[test]
    fn both_versions_the_generation_and_what_the_core_lacks_are_found() {
        let app = FakeApp::new(&[recorded_run("status"), recorded_run("version")]);
        let core = FakeCore::replaying(&["hello"]);
        let r = probe_within(
            &attach(&core, &app),
            &[Capability::Find, Capability::Close],
            LONG,
            LONG,
        );
        assert_eq!(r.path, Ok(app.program().display().to_string()));
        assert_eq!(r.version.as_deref(), Some("0.1.16"));
        assert_eq!(r.window, Ok(()));
        assert_eq!(r.core_version.as_deref(), Some("0.1.21"));
        assert_eq!(r.generation, Some(1));
        assert_eq!(r.core, Ok(()));
        assert_eq!(r.missing, []);

        let mut hello = recorded_one("hello", "hello");
        let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
        methods.retain(|m| m != &json!("terminal.kill"));
        let core = FakeCore::serve(vec![hello]);
        let r = probe_within(
            &attach(&core, &app),
            &[Capability::Find, Capability::Close],
            LONG,
            LONG,
        );
        assert_eq!(
            r.missing,
            [Missing {
                capability: Capability::Close,
                why: "the hc core has no terminal.kill".into(),
            }]
        );
    }
}
