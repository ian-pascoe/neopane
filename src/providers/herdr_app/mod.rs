//! The built-in Herdr app pane provider's command (ADR 0019): `provider
//! builtin herdr-app CAPABILITY`, which its table runs for each
//! capability. Its reach is an hc core, by the socket the choosing
//! attach's `HC_TERMINAL_SOCKET` names, and the Herdr app window under
//! that attach's `HERDR_APP_HOME`; the app's spaces are no boundary.
//! Terminal ids are hc terminal addresses, such as `t7`; the core's doc is
//! the only map from a terminal to its Herdr app pane.
//!
//! - `core.rs`: the hc core's JSON-RPC socket, for reading: `hello`,
//!   `terminal.get`, `terminal.kill`, `doc.get herdr-app`.
//! - `window.rs`: the `herdr-app` command, for the window: found on `PATH`,
//!   else at `~/.local/bin/herdr-app`.
//! - `doc.rs`: the blocks of the core's doc, by terminal.
//! - `find` (`proof.rs`): `hello` generation 1; the attaching terminal's
//!   `HC_TERMINAL_ID`, live, its process on the attach's tty; one block in
//!   the doc showing it; `herdr-app status` answering. A remote terminal,
//!   `machine/t7`, is refused. `inside.herdr_app` runs the same proof.
//! - `close`: `terminal.kill`, done for a terminal finished or not found.
//! - `check.rs`, no capability: what `provider check`'s herdr-app section
//!   shows: both versions, and whether the core and window answer.
//!
//! Each capability gives the line it prints, or why it failed or is
//! unsupported; `run` prints it, or the failure as one `herdr-app
//! CAPABILITY: why` line on stderr, and gives the exit status: 1, or 69
//! for a core lacking what the capability needs, naming it and both
//! versions.

pub(crate) mod check;
mod core;
mod doc;
#[cfg(test)]
mod fake;
mod proof;
mod window;

pub(crate) use proof::inside;

use std::time::{Duration, Instant};

use self::core::Core;
use self::window::Window;
use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// Why a capability did nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Failure {
    /// It failed: exit 1.
    Failed(String),
    /// The core lacks what it needs: exit 69.
    Unsupported(String),
}

impl From<String> for Failure {
    fn from(why: String) -> Failure {
        Failure::Failed(why)
    }
}

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `herdr-app CAPABILITY: why` line on stderr, and gives
/// its exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    let done = match cap {
        Capability::Find => proof::find(given).map(Some).map_err(Failure::Failed),
        Capability::Close => close(given).map(|()| None),
        _ => return UNSUPPORTED,
    };
    let (why, code) = match done {
        Ok(Some(line)) => return print_line(line.as_bytes()),
        Ok(None) => return DONE,
        Err(Failure::Failed(why)) => (why, FAILED),
        Err(Failure::Unsupported(why)) => (why, UNSUPPORTED),
    };
    eprintln!("herdr-app {}: {why}", cap.name());
    code
}

/// What `close` has, of its 2 s.
const CLOSE_TIME: Duration = Duration::from_millis(1500);

/// `close`: `terminal.kill` of `TERMINAL`; done for one already finished
/// or not found.
fn close(given: &Given) -> Result<(), Failure> {
    close_within(given, CLOSE_TIME)
}

/// `close`, its requests, and a version for a message, having `time`.
fn close_within(given: &Given, time: Duration) -> Result<(), Failure> {
    let terminal = given.var("terminal").unwrap_or_default();
    if terminal.is_empty() {
        return Err(Failure::Failed("TERMINAL isn't set".into()));
    }
    let terminal = String::from_utf8_lossy(&terminal).into_owned();
    let mut core = Core::of(given, time)?;
    needs(&core, methods(Capability::Close), || Window::of(given))?;
    match core.kill(&terminal) {
        Ok(_) => Ok(()),
        Err(refused) if refused.kind.as_deref() == Some("not_found") => Ok(()),
        Err(refused) => Err(Failure::Failed(refused.why)),
    }
}

/// The hc core methods capability `cap` calls, besides `hello`.
pub(super) fn methods(cap: Capability) -> &'static [&'static str] {
    match cap {
        Capability::Find => &["terminal.get", "doc.get"],
        Capability::Close => &["terminal.kill"],
        _ => &[],
    }
}

/// Whether `core` has `methods` in the generation the built-in speaks,
/// else unsupported, naming what it lacks and both versions: the core's,
/// and the app's, from the `herdr-app` `window` finds, within the core's
/// deadline.
fn needs(
    core: &Core,
    methods: &[&str],
    window: impl FnOnce() -> Result<Window, String>,
) -> Result<(), Failure> {
    match core.lacks(methods) {
        None => Ok(()),
        Some(why) => Err(Failure::Unsupported(format!(
            "{why} ({})",
            versions(&core.hello().version, window, core.due())
        ))),
    }
}

/// Both versions, for a message: `hc core 0.1.21, herdr-app 0.1.16`; the
/// app's `unknown` if it can't be told by `due`.
fn versions(core: &str, window: impl FnOnce() -> Result<Window, String>, due: Instant) -> String {
    let app = window()
        .and_then(|w| w.version(due).map_err(String::from))
        .unwrap_or_else(|_| "unknown".into());
    format!("hc core {core}, herdr-app {app}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::fake::{FakeApp, FakeCore, given, recorded_one, recorded_run};
    use super::*;

    /// The time a test's close has: well over [`CLOSE_TIME`], as the fakes
    /// run shell scripts on a loaded machine.
    const TIME: Duration = Duration::from_secs(5);

    #[test]
    fn close_kills_the_terminal() {
        let core = FakeCore::replaying(&["terminal.kill"]);
        let app = FakeApp::new(&[]);
        let given = given(&core, &app, &[("TERMINAL", "t10")]);
        assert_eq!(close_within(&given, TIME), Ok(()));
        assert_eq!(core.methods(), ["hello", "terminal.kill"]);
        assert_eq!(core.asked()[1]["params"], json!({ "terminal": "t10" }));
        // the window isn't asked
        assert!(app.asked().is_empty());
    }

    #[test]
    fn close_of_a_terminal_finished_or_not_found_is_done() {
        // killed again: the core answers its finished record
        let core = FakeCore::replaying(&["terminal.kill"]);
        let app = FakeApp::new(&[]);
        let given = given(&core, &app, &[("TERMINAL", "t10")]);
        assert_eq!(close_within(&given, TIME), Ok(()));
        assert_eq!(close_within(&given, TIME), Ok(()));

        let core = FakeCore::replaying(&["terminal.kill-not-found"]);
        let given = super::fake::given(&core, &app, &[("TERMINAL", "t99")]);
        assert_eq!(close_within(&given, TIME), Ok(()));
    }

    #[test]
    fn close_fails_on_any_other_error_or_no_core() {
        let mut kill = recorded_one("terminal.kill-not-found", "terminal.kill");
        kill.replies[0]["error"]["data"]["kind"] = json!("internal");
        kill.replies[0]["error"]["message"] = json!("it broke");
        let core = FakeCore::serve(vec![recorded_one("hello", "hello"), kill]);
        let app = FakeApp::new(&[]);
        let given = given(&core, &app, &[("TERMINAL", "t99")]);
        assert_eq!(
            close_within(&given, TIME),
            Err(Failure::Failed("hc core: it broke".into()))
        );

        let gone = Given::from(|name| match name {
            "HC_TERMINAL_SOCKET" => Some(b"/nowhere/hc.sock".to_vec()),
            n if n.ends_with("_TERMINAL") => Some(b"t1".to_vec()),
            _ => None,
        });
        let Err(Failure::Failed(why)) = close_within(&gone, TIME) else {
            panic!("closed with no core");
        };
        assert!(why.starts_with("can't reach the hc core at "), "{why}");
    }

    #[test]
    fn close_on_a_core_without_terminal_kill_is_unsupported_naming_both_versions() {
        let mut hello = recorded_one("hello", "hello");
        let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
        methods.retain(|m| m != "terminal.kill");
        let core = FakeCore::serve(vec![hello]);
        let app = FakeApp::new(&[recorded_run("version")]);
        let given = given(&core, &app, &[("TERMINAL", "t10")]);
        assert_eq!(
            close_within(&given, TIME),
            Err(Failure::Unsupported(
                "the hc core has no terminal.kill (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
        assert_eq!(core.methods(), ["hello"]);
    }
}
