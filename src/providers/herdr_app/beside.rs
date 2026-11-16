//! `open_beside` (ADR 0020): the Herdr app's `pane.split` runs an argv, so
//! the terminal is made where it goes, beside its origin's pane:
//!
//! 1. The doc names `ORIGIN`'s pane; a terminal with none, or with
//!    several, exits 1, making nothing.
//! 2. `herdr-app do pane.split`, `right` or `down` of that pane, running
//!    the attach command with no shell. The new pane takes the focus,
//!    which stands. A split refused, run out of time, or whose answer
//!    names no terminal exits 1: a pane it made all the same runs an
//!    attach whose pane id the daemon has given up, which is refused, or
//!    hung up if it had attached, and exits, closing the pane.
//! 3. The core's `terminal.keep {keep: false}` of the new terminal at once,
//!    so the core gives it no shell again once it's lost, as an attach
//!    killed by a signal is: failing, it warns.
//! 4. The doc must show the new terminal in one block, its pane, read
//!    again for a while in case the app writes it late; in none, or in
//!    several, the terminal is killed, on a connection of its own, and it
//!    exits 1.
//! 5. For `left` and `up`, `herdr-app do pane.swap` swaps the new pane
//!    with the origin's, its neighbour that side, the focus staying with
//!    the new pane; failing, it warns, leaving it right or below.
//! 6. With `RATIO`, the new pane gets that share of the weight it and the
//!    origin's pane have together (`sizes.rs`), as the doc shows them,
//!    read again after a swap; failing, it warns, leaving the halves the
//!    split made.
//!
//! What it warns of goes on stderr, after the failure's own words if it
//! fails. Prints the new terminal's address.

use std::time::Duration;

use serde_json::json;

use super::core::Core;
use super::doc::pane_of;
use super::{
    BLOCK_TIME, CLEANUP_TIME, Failure, Opened, block_of, cleanup_kill, methods, needs, noted,
    refused, sizes, window_of,
};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `open_beside` has, of its 5 s, for opening.
const OPEN_TIME: Duration = Duration::from_millis(4000);
/// `open_beside`: the new terminal's address, or why there's none.
pub(super) fn open_beside(given: &Given) -> Result<String, Failure> {
    let opened = open_beside_within(given, OPEN_TIME, CLEANUP_TIME, BLOCK_TIME)?;
    for why in &opened.warnings {
        eprintln!("herdr-app open_beside: {why}");
    }
    Ok(opened.terminal)
}

/// `open_beside`, its requests having `time`, waiting at most `block` of
/// it for the new terminal's block, and a cleanup after a failure
/// `cleanup` more.
fn open_beside_within(
    given: &Given,
    time: Duration,
    cleanup: Duration,
    block: Duration,
) -> Result<Opened, Failure> {
    let origin = given.text("origin")?;
    let side = Side::of(given)?;
    let argv = given.attach_strings("herdr-app")?;
    let ratio = given.ratio()?;
    // found first: no herdr-app costs no request of the core
    let window = window_of(given)?;
    let mut core = Core::of(given, time)?;
    needs(
        &core,
        methods(Capability::OpenBeside),
        || Ok(window.clone()),
    )?;
    let due = core.due();

    let origin_pane = pane_of(&core.doc()?, &origin)?;
    let terminal = window
        .split(&origin_pane, side.split, &argv, due)
        .map_err(|ran| refused(ran, &core, &window))?;
    let mut warnings = Vec::new();
    if let Err(e) = core.keep(&terminal, false) {
        warnings.push(format!(
            "can't turn the hc core's keep off for {terminal}: {}",
            e.why
        ));
    }
    let (pane, doc) = match block_of(&mut core, &terminal, block) {
        Ok(found) => found,
        Err(failure) => {
            if let Err(why) = cleanup_kill(&core, &terminal, cleanup) {
                warnings.insert(0, format!("{terminal} is left: {why}"));
            }
            return Err(noted(failure, &warnings));
        }
    };
    let mut doc = Some(doc);
    if let Some(toward) = side.swap {
        let swap = json!({ "pane": pane, "direction": toward });
        match window.act("pane.swap", &swap, due) {
            // the doc has changed
            Ok(_) => doc = None,
            Err(ran) => warnings.push(format!("can't swap {terminal}'s pane {toward}: {ran}")),
        }
    }
    if let Some(ratio) = ratio {
        let sized = match doc {
            Some(doc) => Ok(doc),
            None => core.doc().map_err(String::from),
        }
        .and_then(|doc| sizes::share_by(&window, &doc, &terminal, &origin, ratio, due));
        if let Err(why) = sized {
            warnings.push(format!("can't size {terminal}'s pane: {why}"));
        }
    }
    Ok(Opened { terminal, warnings })
}

/// Where a new pane goes, from `DIRECTION`: the app splits only right or
/// down, so left and up are a split and a swap.
#[derive(Debug, Clone, Copy)]
struct Side {
    /// `pane.split`'s `direction`.
    split: &'static str,
    /// `pane.swap`'s `direction` after, if any.
    swap: Option<&'static str>,
}

impl Side {
    fn of(given: &Given) -> Result<Side, String> {
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

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::{Value, json};

    use super::super::fake::{
        Exchange, FakeApp, FakeCore, Run, given, keep_off, kill_of, recorded, recorded_one,
        recorded_run,
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

    /// An open done, of `terminal`, warning of `warnings`.
    fn opened(terminal: &str, warnings: &[&str]) -> Result<Opened, Failure> {
        Ok(Opened {
            terminal: terminal.into(),
            warnings: warnings.iter().map(|w| w.to_string()).collect(),
        })
    }

    fn open(core: &FakeCore, app: &FakeApp, direction: &str) -> Result<Opened, Failure> {
        let given = given(
            core,
            app,
            &[
                ("ORIGIN", "t2"),
                ("DIRECTION", direction),
                ("ATTACH", ATTACH),
            ],
        );
        open_beside_within(&given, TIME, TIME, BLOCK_TIME)
    }

    #[test]
    fn down_splits_the_origins_pane_running_the_attach_and_turns_keep_off() {
        let (core, app) = (core(), FakeApp::new(&[split_down()]));
        assert_eq!(open(&core, &app, "down"), opened("t3", &[]));
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
        assert_eq!(open(&core, &app, "up"), opened("t3", &[]));
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
            "do-pane.swap",
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
            open_beside_within(&given("right"), TIME, TIME, BLOCK_TIME),
            opened("t2", &[])
        );
        assert_eq!(
            open_beside_within(&given("left"), TIME, TIME, BLOCK_TIME),
            opened("t2", &[])
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
    fn a_split_that_runs_out_of_time_fails_turning_nothing_off_and_killing_nothing() {
        // exec'd, so killing it ends the sleep
        let mut slow = split_down();
        slow.sleep = Some(30.0);
        let (core, app) = (core(), FakeApp::new(&[slow]));
        let given = given(
            &core,
            &app,
            &[("ORIGIN", "t2"), ("DIRECTION", "down"), ("ATTACH", ATTACH)],
        );
        let started = Instant::now();
        let opened = open_beside_within(&given, Duration::from_secs(2), TIME, BLOCK_TIME);
        let Err(Failure::Failed(why)) = opened else {
            panic!("{opened:?}");
        };
        assert!(why.contains("killed, out of time"), "{why}");
        assert!(started.elapsed() < Duration::from_secs(30));
        assert_eq!(core.methods(), ["hello", "doc.get"]);
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
        let given = given(
            &core,
            &app,
            &[("ORIGIN", "t2"), ("DIRECTION", "up"), ("ATTACH", ATTACH)],
        );
        assert_eq!(
            open_beside_within(&given, TIME, TIME, Duration::from_millis(200)),
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
    fn a_new_terminal_in_several_blocks_is_killed_at_once() {
        // t1's block shows t3 too
        let core = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            doc_with("t1", "t3"),
            keep_off("t3"),
            kill_of("t3"),
        ]);
        let app = FakeApp::new(&[split_down()]);
        assert_eq!(
            open(&core, &app, "down"),
            Err(Failure::Failed("t3 shows in 2 Herdr app panes".into()))
        );
        // the origin's read and one of t3's: no waiting
        assert_eq!(
            core.methods(),
            [
                "hello",
                "doc.get",
                "terminal.keep",
                "doc.get",
                "hello",
                "terminal.kill"
            ]
        );
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
        assert_eq!(open(&core, &app, "down"), opened("t3", &[]));
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get", "doc.get"]
        );
    }

    /// Why the core refused `terminal.keep-not-found`, as a warning.
    const KEEP_REFUSED: &str =
        "can't turn the hc core's keep off for t3: hc core: no terminal \"t99\"";

    fn open_by(
        core: &FakeCore,
        app: &FakeApp,
        direction: &str,
        ratio: &str,
    ) -> Result<Opened, Failure> {
        let given = given(
            core,
            app,
            &[
                ("ORIGIN", "t2"),
                ("DIRECTION", direction),
                ("ATTACH", ATTACH),
                ("RATIO", ratio),
            ],
        );
        open_beside_within(&given, TIME, TIME, BLOCK_TIME)
    }

    /// `layout.resize` of `t3`'s pane to `weight`, answered.
    fn resize_p3(weight: &str) -> Run {
        let args = format!(r#"{{"id":"{P3}","weight":{weight}}}"#);
        answering("do-layout.resize", "layout.resize", &args)
    }

    /// The nested tab's core, keep answered for `t3`, the block showing
    /// `terminal` sized `size`.
    fn core_sized(terminal: &str, size: Value) -> FakeCore {
        fn set(node: &mut Value, terminal: &str, size: &Value) {
            if node["content"]["terminal"] == terminal {
                node["size"] = size.clone();
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                children.iter_mut().for_each(|c| set(c, terminal, size));
            }
        }
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(keep_off("t3"));
        let doc = exchanges
            .iter_mut()
            .find(|e| e.method == "doc.get")
            .unwrap();
        for space in doc.result_mut()["value"]["spaces"].as_array_mut().unwrap() {
            for tab in space["tabs"].as_array_mut().unwrap() {
                set(&mut tab["root"], terminal, &size);
            }
        }
        FakeCore::serve(exchanges)
    }

    #[test]
    fn the_new_pane_gets_ratio_of_the_weight_it_shares_with_the_origins() {
        // t2 / t3, neither sized: 0.3 is t2's weight 1 × 0.3 / 0.7
        let (core, app) = (core(), FakeApp::new(&[split_down(), resize_p3("0.428571")]));
        assert_eq!(open_by(&core, &app, "down", "0.3"), opened("t3", &[]));
        assert_eq!(
            app.asked()[1],
            format!(r#"do layout.resize {{"id":"{P3}","weight":0.428571}}"#)
        );
        // the doc that showed its block
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get"]
        );
    }

    #[test]
    fn after_a_swap_the_doc_is_read_again_for_sizing() {
        let swap = answering(
            "do-pane.swap",
            "pane.swap",
            &format!(r#"{{"pane":"{P3}","direction":"up"}}"#),
        );
        let app = FakeApp::new(&[split_down(), swap, resize_p3("0.428571")]);
        let core = core();
        assert_eq!(open_by(&core, &app, "up", "0.3"), opened("t3", &[]));
        let asked = app.asked();
        assert_eq!(asked.len(), 3);
        assert!(asked[1].starts_with("do pane.swap"));
        assert!(asked[2].starts_with("do layout.resize"));
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get", "doc.get"]
        );
    }

    #[test]
    fn a_failed_swap_sizes_by_the_doc_already_read() {
        let mut swap = answering(
            "do-pane.swap",
            "pane.swap",
            &format!(r#"{{"pane":"{P3}","direction":"up"}}"#),
        );
        swap.exit = 1;
        swap.stdout = "{\"error\": \"no such pane\"}\n".into();
        let app = FakeApp::new(&[split_down(), swap, resize_p3("0.428571")]);
        let core = core();
        assert_eq!(
            open_by(&core, &app, "up", "0.3"),
            opened("t3", &["can't swap t3's pane up: herdr-app: no such pane"])
        );
        assert_eq!(app.asked().len(), 3);
        assert_eq!(
            core.methods(),
            ["hello", "doc.get", "terminal.keep", "doc.get"]
        );
    }

    #[test]
    fn half_is_what_the_split_gave_so_nothing_is_resized() {
        let (core, app) = (core(), FakeApp::new(&[split_down()]));
        assert_eq!(open_by(&core, &app, "down", "0.5"), opened("t3", &[]));
        assert_eq!(app.asked().len(), 1);
    }

    #[test]
    fn half_is_set_against_the_origins_own_weight() {
        // t2 at weight 2: half of the pair is t3 at 2 too, not the 1 it has
        let core = core_sized("t2", json!({ "weight": 2.0 }));
        let app = FakeApp::new(&[split_down(), resize_p3("2.0")]);
        assert_eq!(open_by(&core, &app, "down", "0.5"), opened("t3", &[]));
        assert_eq!(
            app.asked()[1],
            format!(r#"do layout.resize {{"id":"{P3}","weight":2.0}}"#)
        );
    }

    #[test]
    fn a_sizing_the_app_refuses_warns_and_opens_all_the_same() {
        let mut refused = resize_p3("0.428571");
        refused.exit = 1;
        refused.stdout = recorded_run("err-unknown-command")
            .stdout
            .replace("no.such", "layout.resize");
        let (core, app) = (core(), FakeApp::new(&[split_down(), refused]));
        let Ok(Opened { terminal, warnings }) = open_by(&core, &app, "down", "0.3") else {
            panic!("not opened");
        };
        assert_eq!(terminal, "t3");
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0]
                .starts_with("can't size t3's pane: herdr-app: unknown command \"layout.resize\""),
            "{warnings:?}"
        );
        assert_eq!(app.asked().len(), 2);
    }

    #[test]
    fn an_origin_sized_in_pixels_warns_and_sets_nothing() {
        let core = core_sized("t2", json!({ "px": 300 }));
        let app = FakeApp::new(&[split_down()]);
        let why = format!("can't size t3's pane: Herdr app node {P2} is sized in pixels");
        assert_eq!(open_by(&core, &app, "down", "0.3"), opened("t3", &[&why]));
        assert_eq!(app.asked().len(), 1);
    }

    #[test]
    fn a_failure_says_what_it_warned_of_after_its_own_words() {
        // the core refuses keep; the doc never shows t3
        let mut keep = recorded_one("terminal.keep-not-found", "terminal.keep");
        keep.params = json!({ "terminal": "t3", "keep": false });
        let core = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            doc_with("t3", "t9"),
            keep,
            kill_of("t3"),
        ]);
        let app = FakeApp::new(&[split_down()]);
        let given = given(
            &core,
            &app,
            &[("ORIGIN", "t2"), ("DIRECTION", "down"), ("ATTACH", ATTACH)],
        );
        assert_eq!(
            open_beside_within(&given, TIME, TIME, Duration::from_millis(200)),
            Err(Failure::Failed(format!(
                "t3 has no Herdr app pane; {KEEP_REFUSED}"
            )))
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
        assert_eq!(
            open(&core, &app, "up"),
            opened(
                "t3",
                &[
                    KEEP_REFUSED,
                    "can't swap t3's pane up: herdr-app: no such pane"
                ]
            )
        );
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
            (no_argv, "herdr-app's pane.split takes no argv".to_string()),
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
            open_beside_within(&given, TIME, TIME, BLOCK_TIME),
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
            (
                vec![
                    ("ORIGIN", "t2"),
                    ("DIRECTION", "left"),
                    ("ATTACH", ATTACH),
                    ("RATIO", "1.5"),
                ],
                "RATIO \"1.5\" isn't between 0 and 1",
            ),
        ] {
            let opened = open_beside_within(&given(&core, &app, &vars), TIME, TIME, BLOCK_TIME);
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
            open_beside_within(&not_utf8, TIME, TIME, BLOCK_TIME),
            Err(Failure::Failed(
                "the attach command isn't UTF-8, which herdr-app can't run".into()
            ))
        );
        assert!(core.asked().is_empty());
        assert!(app.asked().is_empty());
    }
}
