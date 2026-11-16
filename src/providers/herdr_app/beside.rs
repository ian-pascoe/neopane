//! `open_beside` (ADR 0020): the Herdr app's `pane.split` runs an argv, so
//! the terminal is made where it goes, beside its origin's pane:
//!
//! 1. The doc names `ORIGIN`'s pane; a terminal with none, or with
//!    several, exits 1, making nothing.
//! 2. `herdr-app do pane.split`, `right` or `down` of that pane, running
//!    the attach command with no shell. The new pane takes the focus,
//!    which stands. A split refused, run out of time, or whose answer
//!    names no terminal exits 1: a pane it made all the same runs an
//!    attach whose pane id the daemon has given up, which is refused and
//!    exits, closing the pane.
//! 3. The core's `terminal.keep {keep: false}` of the new terminal at once,
//!    so the core gives it no shell again once it's lost, as an attach
//!    killed by a signal is: failing, it warns.
//! 4. The doc must show the new terminal in one block, its pane; else the
//!    terminal is killed, on a connection of its own, and it exits 1.
//! 5. For `left` and `up`, `herdr-app do pane.swap` swaps the new pane
//!    with the origin's, its neighbour that side, the focus staying with
//!    the new pane; failing, it warns, leaving it right or below.
//!
//! No sizing yet: the split halves the origin's room. Prints the new
//! terminal's address.

use std::time::{Duration, Instant};

use serde_json::json;

use super::core::Core;
use super::doc::pane_of;
use super::window::{Ran, Window};
use super::{Failure, input, methods, needs, versions};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `open_beside` has, of its 5 s, for opening.
const OPEN_TIME: Duration = Duration::from_millis(4000);
/// What it has after a failure, for killing what it made.
const CLEANUP_TIME: Duration = Duration::from_millis(500);
/// The most it waits for the doc to show a new terminal's block.
const BLOCK_TIME: Duration = Duration::from_millis(1000);
/// How often it reads the doc meanwhile.
const BLOCK_POLL: Duration = Duration::from_millis(50);

/// `open_beside`: the new terminal's address, or why there's none.
pub(super) fn open_beside(given: &Given) -> Result<String, Failure> {
    open_beside_within(given, OPEN_TIME, CLEANUP_TIME)
}

/// `open_beside`, its requests having `time`, and a cleanup after a
/// failure `cleanup` more.
fn open_beside_within(given: &Given, time: Duration, cleanup: Duration) -> Result<String, Failure> {
    let origin = input(given, "origin")?;
    let side = Side::of(given)?;
    let argv = attach_words(given)?;
    let mut core = Core::of(given, time)?;
    let window = Window::of(given).map_err(Failure::Unsupported)?;
    needs(
        &core,
        methods(Capability::OpenBeside),
        || Ok(window.clone()),
    )?;
    let due = core.due();

    let origin_pane = pane_of(&core.doc().map_err(String::from)?, &origin)?;
    let split = json!({ "direction": side.split, "pane": origin_pane, "argv": argv });
    let split = window
        .act("pane.split", &split, due)
        .map_err(|ran| refused(&core, &window, "pane.split", ran))?;
    let terminal = split["terminal"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "herdr-app pane.split's answer names no terminal".to_string())?
        .to_string();
    if let Err(e) = core.keep(&terminal, false) {
        eprintln!(
            "herdr-app open_beside: can't turn the hc core's keep off for {terminal}: {}",
            e.why
        );
    }
    let pane = match block_of(&mut core, &terminal) {
        Ok(pane) => pane,
        Err(why) => {
            kill(&core, &terminal, cleanup);
            return Err(why.into());
        }
    };
    if let Some(toward) = side.swap {
        let swap = json!({ "pane": pane, "direction": toward });
        if let Err(ran) = window.act("pane.swap", &swap, due) {
            eprintln!("herdr-app open_beside: can't swap {terminal}'s pane {toward}: {ran}");
        }
    }
    Ok(terminal)
}

/// The pane of new terminal `terminal`, once the doc shows it, within
/// [`BLOCK_TIME`] and the core's time; or why not.
fn block_of(core: &mut Core, terminal: &str) -> Result<String, String> {
    let until = core.due().min(Instant::now() + BLOCK_TIME);
    loop {
        let found = pane_of(&core.doc()?, terminal);
        if found.is_ok() || Instant::now() + BLOCK_POLL >= until {
            return found;
        }
        std::thread::sleep(BLOCK_POLL);
    }
}

/// Kills terminal `terminal`, on a connection of its own having `time`,
/// saying on stderr if it can't.
fn kill(core: &Core, terminal: &str, time: Duration) {
    let killed = core
        .again(time)
        .and_then(|mut c| c.kill(terminal).map(|_| ()).map_err(|refused| refused.why));
    if let Err(why) = killed {
        eprintln!("herdr-app open_beside: can't kill {terminal}: {why}");
    }
}

/// Why window act `command` failed: unsupported where `herdr-app` lacks
/// it, or an argument of it, naming both versions; else failed.
fn refused(core: &Core, window: &Window, command: &str, ran: Ran) -> Failure {
    let lacks = match &ran {
        Ran::Refused(why) if why.starts_with("unknown command") => {
            Some(format!("herdr-app has no {command}"))
        }
        Ran::Refused(why) if why.contains("unknown argument") => Some(ran.to_string()),
        _ => None,
    };
    match lacks {
        Some(why) => Failure::Unsupported(format!(
            "{why} ({})",
            versions(&core.hello().version, || Ok(window.clone()), core.due())
        )),
        None => Failure::Failed(ran.to_string()),
    }
}

/// Where a new pane goes, from `DIRECTION`: the app splits only right or
/// down, so left and up are a split and a swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Side {
    /// `pane.split`'s `direction`.
    pub split: &'static str,
    /// `pane.swap`'s `direction` after, if any.
    pub swap: Option<&'static str>,
}

impl Side {
    pub(super) fn of(given: &Given) -> Result<Side, String> {
        let direction = given.var("direction").unwrap_or_default();
        let (split, swap) = match direction.as_slice() {
            b"left" => ("right", Some("left")),
            b"right" => ("right", None),
            b"up" => ("down", Some("up")),
            b"down" => ("down", None),
            other => {
                return Err(format!(
                    "DIRECTION {:?} isn't left, right, up or down",
                    String::from_utf8_lossy(other)
                ));
            }
        };
        Ok(Side { split, swap })
    }
}

/// The attach command's words, from `ATTACH`, as UTF-8: JSON can't carry
/// other bytes.
fn attach_words(given: &Given) -> Result<Vec<String>, String> {
    given
        .attach_words()?
        .into_iter()
        .map(|w| {
            w.into_string()
                .map_err(|_| "the attach command isn't UTF-8, which herdr-app can't run".into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::fake::{
        Exchange, FakeApp, FakeCore, Run, given, recorded, recorded_one, recorded_run,
    };
    use super::*;

    /// The time a test's open has: well over [`OPEN_TIME`], as the fakes
    /// run shell scripts on a loaded machine.
    const TIME: Duration = Duration::from_secs(5);

    /// An attach command, as `ATTACH` has it.
    const ATTACH: &str = "/bin/np attach --address '/tmp/a b'";
    /// Its words, as JSON.
    const ARGV: &str = r#"["/bin/np","attach","--address","/tmp/a b"]"#;

    /// The nested tab's panes: `t1`, `t2` and `t3`'s.
    const P1: &str = "pf7e3d563947903ed7a8c93ee0ac58181";
    const P2: &str = "p7b3398d57b6dd529d0f978c04b35c687";
    const P3: &str = "p4c229a9b57964c201d7ab577ccf4a2ea";

    /// Recorded run `name`, answering `args` after `herdr-app do`.
    fn answering(name: &str, command: &str, args: &str) -> Run {
        let mut run = recorded_run(name);
        run.args = vec!["do".into(), command.into(), args.into()];
        run
    }

    /// The split `t3` came of, from `t2`'s pane, down.
    fn split_down() -> Run {
        let args = format!(r#"{{"direction":"down","pane":"{P2}","argv":{ARGV}}}"#);
        answering("do-pane.split-down", "pane.split", &args)
    }

    /// The core's `terminal.keep {keep: false}` of `terminal`, answered.
    fn keep_off(terminal: &str) -> Exchange {
        let mut keep = recorded_one("terminal.keep", "terminal.keep");
        keep.params = json!({ "terminal": terminal, "keep": false });
        keep.result_mut()["address"] = json!(terminal);
        keep
    }

    /// `terminal.kill` of `terminal`, answered.
    fn kill_of(terminal: &str) -> Exchange {
        let mut kill = recorded_one("terminal.kill", "terminal.kill");
        kill.params = json!({ "terminal": terminal });
        kill.result_mut()["address"] = json!(terminal);
        kill
    }

    /// The nested tab's doc, `from` shown as `to`.
    fn doc_with(from: &str, to: &str) -> Exchange {
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        let text = doc.replies[0]
            .to_string()
            .replace(&format!("\"{from}\""), &format!("\"{to}\""));
        doc.replies[0] = serde_json::from_str(&text).unwrap();
        doc
    }

    /// The nested tab's core, keep answered for `t3`.
    fn core() -> FakeCore {
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(keep_off("t3"));
        FakeCore::serve(exchanges)
    }

    fn open(core: &FakeCore, app: &FakeApp, direction: &str) -> Result<String, Failure> {
        let given = given(
            core,
            app,
            &[
                ("ORIGIN", "t2"),
                ("DIRECTION", direction),
                ("ATTACH", ATTACH),
            ],
        );
        open_beside_within(&given, TIME, TIME)
    }

    #[test]
    fn down_splits_the_origins_pane_running_the_attach_and_turns_keep_off() {
        let (core, app) = (core(), FakeApp::new(&[split_down()]));
        assert_eq!(open(&core, &app, "down"), Ok("t3".into()));
        assert_eq!(
            app.asked(),
            [format!(
                r#"do pane.split {{"direction":"down","pane":"{P2}","argv":{ARGV}}}"#
            )]
        );
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get"]
        );
        assert_eq!(
            core.asked()[2]["params"],
            json!({ "terminal": "t3", "keep": false })
        );
    }

    #[test]
    fn up_splits_down_then_swaps_the_new_pane_up() {
        let swap = answering(
            "do-pane.swap",
            "pane.swap",
            &format!(r#"{{"pane":"{P3}","direction":"up"}}"#),
        );
        let (core, app) = (core(), FakeApp::new(&[split_down(), swap]));
        assert_eq!(open(&core, &app, "up"), Ok("t3".into()));
        let asked = app.asked();
        assert_eq!(asked.len(), 2);
        assert!(asked[0].starts_with(r#"do pane.split {"direction":"down""#));
        assert_eq!(
            asked[1],
            format!(r#"do pane.swap {{"pane":"{P3}","direction":"up"}}"#)
        );
    }

    #[test]
    fn right_and_left_split_right_left_swapping() {
        // t1's pane split right made t2's
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(keep_off("t2"));
        let core = FakeCore::serve(exchanges);
        let args = format!(r#"{{"direction":"right","pane":"{P1}","argv":{ARGV}}}"#);
        let split = answering("do-pane.split", "pane.split", &args);
        let swap = answering(
            "do-pane.swap-edge",
            "pane.swap",
            &format!(r#"{{"pane":"{P2}","direction":"left"}}"#),
        );
        let app = FakeApp::new(&[split, swap]);
        let given = |direction| {
            given(
                &core,
                &app,
                &[
                    ("ORIGIN", "t1"),
                    ("DIRECTION", direction),
                    ("ATTACH", ATTACH),
                ],
            )
        };
        assert_eq!(
            open_beside_within(&given("right"), TIME, TIME),
            Ok("t2".into())
        );
        assert_eq!(
            open_beside_within(&given("left"), TIME, TIME),
            Ok("t2".into())
        );
        let asked = app.asked();
        assert_eq!(asked.len(), 3);
        assert_eq!(asked[0], asked[1]);
        assert_eq!(
            asked[2],
            format!(r#"do pane.swap {{"pane":"{P2}","direction":"left"}}"#)
        );
    }

    #[test]
    fn a_split_refused_or_unreadable_fails_making_nothing_more() {
        let mut refused = split_down();
        refused.exit = 1;
        refused.stdout = "{\"error\": \"no such pane\"}\n".into();
        let mut no_terminal = split_down();
        no_terminal.stdout = "{\"pane\": \"p1\"}\n".into();
        for (run, why) in [
            (refused, "herdr-app: no such pane"),
            (
                no_terminal,
                "herdr-app pane.split's answer names no terminal",
            ),
        ] {
            let (core, app) = (core(), FakeApp::new(&[run]));
            assert_eq!(open(&core, &app, "up"), Err(Failure::Failed(why.into())));
            assert_eq!(core.methods(), ["hello", "doc.get"]);
            assert_eq!(app.asked().len(), 1);
        }
    }

    #[test]
    fn a_split_that_runs_out_of_time_fails() {
        let mut slow = split_down();
        slow.sleep = Some(5.0);
        let (core, app) = (core(), FakeApp::new(&[slow]));
        let given = given(
            &core,
            &app,
            &[("ORIGIN", "t2"), ("DIRECTION", "down"), ("ATTACH", ATTACH)],
        );
        let started = Instant::now();
        let opened = open_beside_within(&given, Duration::from_millis(500), TIME);
        let Err(Failure::Failed(why)) = opened else {
            panic!("{opened:?}");
        };
        assert!(why.contains("killed, out of time"), "{why}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_new_terminal_with_no_block_is_killed_and_fails() {
        // the doc never shows t3
        let core = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            doc_with("t3", "t9"),
            keep_off("t3"),
            kill_of("t3"),
        ]);
        let app = FakeApp::new(&[split_down()]);
        assert_eq!(
            open(&core, &app, "up"),
            Err(Failure::Failed("t3 has no Herdr app pane".into()))
        );
        let methods = core.methods();
        assert_eq!(methods.last().unwrap(), "terminal.kill");
        // on a connection of its own, after a hello
        assert_eq!(methods[methods.len() - 2], "hello");
        assert_eq!(
            core.asked().last().unwrap()["params"],
            json!({ "terminal": "t3" })
        );
        // no swap
        assert_eq!(app.asked().len(), 1);
    }

    #[test]
    fn a_block_the_doc_shows_late_is_waited_for() {
        let core = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            // the origin's, then t3's first read: no t3 yet
            doc_with("t3", "t9"),
            doc_with("t3", "t9"),
            recorded_one("nested-doc.get", "doc.get"),
            keep_off("t3"),
        ]);
        let app = FakeApp::new(&[split_down()]);
        assert_eq!(open(&core, &app, "down"), Ok("t3".into()));
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get", "doc.get"]
        );
    }

    #[test]
    fn keep_and_swap_failures_warn_and_open_all_the_same() {
        // the core refuses keep; the app refuses the swap
        let mut keep = recorded_one("terminal.keep-not-found", "terminal.keep");
        keep.params = json!({ "terminal": "t3", "keep": false });
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(keep);
        let core = FakeCore::serve(exchanges);
        let mut swap = answering(
            "do-pane.swap",
            "pane.swap",
            &format!(r#"{{"pane":"{P3}","direction":"up"}}"#),
        );
        swap.exit = 1;
        swap.stdout = "{\"error\": \"no such pane\"}\n".into();
        let app = FakeApp::new(&[split_down(), swap]);
        assert_eq!(open(&core, &app, "up"), Ok("t3".into()));
        assert_eq!(app.asked().len(), 2);
    }

    #[test]
    fn an_origin_with_no_pane_or_several_fails_making_nothing() {
        for (doc, why) in [
            (doc_with("t2", "t9"), "t2 has no Herdr app pane"),
            (doc_with("t3", "t2"), "t2 shows in 2 Herdr app panes"),
        ] {
            let core = FakeCore::serve(vec![recorded_one("hello", "hello"), doc]);
            let app = FakeApp::new(&[split_down()]);
            assert_eq!(open(&core, &app, "down"), Err(Failure::Failed(why.into())));
            assert!(app.asked().is_empty());
        }
    }

    #[test]
    fn a_core_lacking_a_method_it_calls_is_unsupported_naming_it_and_both_versions() {
        for method in ["doc.get", "terminal.keep", "terminal.kill"] {
            let mut hello = recorded_one("hello", "hello");
            let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
            methods.retain(|m| m != method);
            let core = FakeCore::serve(vec![hello]);
            let app = FakeApp::new(&[recorded_run("version")]);
            assert_eq!(
                open(&core, &app, "down"),
                Err(Failure::Unsupported(format!(
                    "the hc core has no {method} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
            assert_eq!(core.methods(), ["hello"]);
            assert_eq!(app.asked(), ["--version"]);
        }
    }

    #[test]
    fn an_app_without_pane_split_or_its_argv_is_unsupported_naming_both_versions() {
        let mut unknown = split_down();
        unknown.exit = 1;
        unknown.stdout = recorded_run("err-unknown-command")
            .stdout
            .replace("no.such", "pane.split");
        let mut no_argv = split_down();
        no_argv.exit = 1;
        no_argv.stdout = recorded_run("err-unknown-argument")
            .stdout
            .replace("bogus", "argv");
        for (run, why) in [
            (unknown, "herdr-app has no pane.split".to_string()),
            (
                no_argv,
                "herdr-app: pane.split: unknown argument(s): argv; valid arguments: direction, \
                 pane, argv, mod, props"
                    .to_string(),
            ),
        ] {
            let core = core();
            let app = FakeApp::new(&[run, recorded_run("version")]);
            assert_eq!(
                open(&core, &app, "down"),
                Err(Failure::Unsupported(format!(
                    "{why} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
        }
    }

    #[test]
    fn no_herdr_app_is_unsupported_naming_both_places() {
        let core = core();
        let app = FakeApp::new(&[]);
        let up = crate::name::upper();
        let socket = core.path();
        let given = Given::from(move |name| match name {
            "HC_TERMINAL_SOCKET" => Some(socket.clone()),
            "PATH" => Some(b"/nowhere".to_vec()),
            "HOME" => Some(b"/no/home".to_vec()),
            n if n == format!("{up}_ORIGIN") => Some(b"t2".to_vec()),
            n if n == format!("{up}_DIRECTION") => Some(b"down".to_vec()),
            n if n == format!("{up}_ATTACH") => Some(ATTACH.as_bytes().to_vec()),
            _ => None,
        });
        assert_eq!(
            open_beside_within(&given, TIME, TIME),
            Err(Failure::Unsupported(
                "no herdr-app on PATH, nor at /no/home/.local/bin/herdr-app".into()
            ))
        );
        assert!(app.asked().is_empty());
    }

    #[test]
    fn bad_inputs_ask_nothing() {
        let (core, app) = (core(), FakeApp::new(&[]));
        for (vars, why) in [
            (vec![("DIRECTION", "left")], "ORIGIN isn't set"),
            (
                vec![("ORIGIN", "t2"), ("DIRECTION", "sideways")],
                "DIRECTION \"sideways\" isn't left, right, up or down",
            ),
            (
                vec![("ORIGIN", "t2"), ("DIRECTION", "left")],
                "ATTACH is empty",
            ),
        ] {
            let opened = open_beside_within(&given(&core, &app, &vars), TIME, TIME);
            assert_eq!(opened, Err(Failure::Failed(why.into())), "{vars:?}");
        }
        let up = crate::name::upper();
        let not_utf8 = Given::from(move |name| match name.strip_prefix(&format!("{up}_")) {
            Some("ORIGIN") => Some(b"t2".to_vec()),
            Some("DIRECTION") => Some(b"left".to_vec()),
            Some("ATTACH") => Some(b"/bin/np attach \xff".to_vec()),
            _ => None,
        });
        assert_eq!(
            open_beside_within(&not_utf8, TIME, TIME),
            Err(Failure::Failed(
                "the attach command isn't UTF-8, which herdr-app can't run".into()
            ))
        );
        assert!(core.asked().is_empty());
        assert!(app.asked().is_empty());
    }
}
