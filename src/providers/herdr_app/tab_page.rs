//! `open_tab_page`: a Herdr app tab is a tab page area, made in `ORIGIN`'s
//! space without showing it or moving focus. `tab.new` with an `argv`
//! runs it through the login shell and keeps that shell after it ends
//! (hc 0.1.21), so the tab is made bare, `tab.new {focus: false, space}`,
//! and the attach split into it with no shell, `pane.split {argv}`; then
//! the tab's first shell is killed, leaving the attach the tab's one pane,
//! which closes, and the tab with it, when the attach ends.
//!
//! The attach's terminal is kept off, so it ends with the core rather than
//! coming back as a fresh shell, and must show in one block of the doc,
//! else it is killed and the open fails. The doc may lag the window's
//! answer, so a block is waited for, a little. What the open made is
//! killed after a failure, with time of its own. A shell kill or keep that
//! fails is only warned of: after the failure's own words, if it fails.

use std::time::Duration;

use serde_json::json;

use super::core::Core;
use super::doc::spaces_of;
use super::{
    BLOCK_TIME, CLEANUP_TIME, Failure, block_of, cleanup_kill, kill, methods, needs, refused,
    window_of,
};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `open_tab_page` has: its 5 s timeout less 1 s for the daemon.
const OPEN_TIME: Duration = Duration::from_millis(4000);

/// Where the attach is split from the new tab's shell: anywhere, as the
/// shell is killed, leaving the attach alone.
const SPLIT_SIDE: &str = "right";

/// What an open has, for each part.
#[derive(Debug, Clone, Copy)]
struct Times {
    /// For all of it.
    open: Duration,
    /// After a failure, for killing what it made.
    cleanup: Duration,
    /// The most a block is waited for in the doc.
    block: Duration,
}

/// A tab page opened: the attach's terminal, and what to warn of.
#[derive(Debug, PartialEq, Eq)]
struct Opened {
    terminal: String,
    warnings: Vec<String>,
}

/// `open_tab_page`: the attach's terminal, or why there's none.
pub(super) fn open_tab_page(given: &Given) -> Result<String, Failure> {
    let times = Times {
        open: OPEN_TIME,
        cleanup: CLEANUP_TIME,
        block: BLOCK_TIME,
    };
    let opened = open_tab_page_within(given, times)?;
    for why in &opened.warnings {
        eprintln!("herdr-app open_tab_page: {why}");
    }
    Ok(opened.terminal)
}

/// `open_tab_page`, having `times`.
fn open_tab_page_within(given: &Given, times: Times) -> Result<Opened, Failure> {
    let origin = given.text("origin")?;
    let argv = given.attach_strings("herdr-app")?;
    let mut core = Core::of(given, times.open)?;
    let window = window_of(given)?;
    needs(&core, methods(Capability::OpenTabPage), || {
        Ok(window.clone())
    })?;
    let due = core.due();
    let space = match spaces_of(&core.doc()?, &origin).as_slice() {
        [] => return Err(format!("{origin} has no Herdr app pane").into()),
        [one] => one.clone(),
        several => {
            return Err(format!("{origin} shows in {} Herdr app panes", several.len()).into());
        }
    };
    let made = window
        .act("tab.new", &json!({ "focus": false, "space": space }), due)
        .map_err(|ran| refused(ran, &core, &window))?;
    let shell = made["terminal"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "herdr-app tab.new's answer names no terminal".to_string())?
        .to_string();
    let mut warnings = Vec::new();
    let undo = |core: &Core, terminal: &str, failure: Failure, warnings: &[String]| {
        let mut notes = Vec::new();
        if let Err(why) = cleanup_kill(core, terminal, times.cleanup) {
            notes.push(format!("{terminal} is left: {why}"));
        }
        notes.extend_from_slice(warnings);
        noted(failure, &notes)
    };
    let shell_pane = match block_of(&mut core, &shell, times.block) {
        Ok(pane) => pane,
        Err(failure) => return Err(undo(&core, &shell, failure, &warnings)),
    };
    let terminal = match window.split(&shell_pane, SPLIT_SIDE, &argv, due) {
        Ok(terminal) => terminal,
        Err(ran) => {
            let failure = refused(ran, &core, &window);
            return Err(undo(&core, &shell, failure, &warnings));
        }
    };
    if let Err(r) = kill(&mut core, &shell) {
        warnings.push(format!(
            "the new tab's shell {shell} is left, so the tab won't close with the attach: {}",
            r.why
        ));
    }
    if let Err(r) = core.keep(&terminal, false) {
        warnings.push(format!(
            "{terminal} will come back as a shell if the hc core restarts: {}",
            r.why
        ));
    }
    match block_of(&mut core, &terminal, times.block) {
        Ok(_) => Ok(Opened { terminal, warnings }),
        Err(failure) => Err(undo(&core, &terminal, failure, &warnings)),
    }
}

/// `failure`, `notes` after its own words.
fn noted(failure: Failure, notes: &[String]) -> Failure {
    let note = |why: String| notes.iter().fold(why, |why, note| format!("{why}; {note}"));
    match failure {
        Failure::Failed(why) => Failure::Failed(note(why)),
        Failure::Unsupported(why) => Failure::Unsupported(note(why)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use serde_json::{Value, json};

    use super::super::fake::{
        Exchange, FakeApp, FakeCore, Run, given, keep_off, kill_of, recorded_one, recorded_run,
    };
    use super::*;

    /// What a test's open has: well over [`OPEN_TIME`] and
    /// [`CLEANUP_TIME`], as the fakes run shell scripts on a loaded
    /// machine, and a block waited for `block`.
    fn times(block: Duration) -> Times {
        Times {
            open: Duration::from_secs(5),
            cleanup: Duration::from_secs(5),
            block,
        }
    }

    /// Long enough for any block the doc will show.
    const SHOWN: Duration = Duration::from_secs(5);

    /// Short: for a block the doc never shows.
    const NEVER: Duration = Duration::from_millis(50);

    const SPACE: &str = "sp22637f42bbefeecf910ee187a961334c";
    const TAB: &str = "tb647b2d25c9734a2636b21f8d1a7c8c1c";
    const SHELL_PANE: &str = "p0aa1";
    const ATTACH_PANE: &str = "p0bb2";
    const ATTACH: &str = "/bin/np attach --pane '12-k3j9x2qpa8'";

    /// The nested tab's doc, with a new tab of `blocks`, terminal and
    /// pane each, side by side; none: no new tab.
    fn doc_with(blocks: &[(&str, &str)]) -> Exchange {
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        if !blocks.is_empty() {
            let children: Vec<Value> = blocks
                .iter()
                .map(|(t, p)| json!({ "content": { "terminal": t }, "id": p, "type": "block" }))
                .collect();
            let tab = json!({
                "id": TAB,
                "name": "",
                "root": { "children": children, "id": TAB, "type": "row" },
            });
            doc.result_mut()["value"]["spaces"][0]["tabs"]
                .as_array_mut()
                .unwrap()
                .push(tab);
        }
        doc
    }

    /// `terminal.kill` of `t`, answering `not_found`.
    fn kill_not_found(t: &str) -> Exchange {
        let mut kill = recorded_one("terminal.kill-not-found", "terminal.kill");
        kill.params = json!({ "terminal": t });
        kill
    }

    /// A core whose doc reads `docs` in turn, the last again after, that
    /// kills `kills` and keeps `keeps`, and answers any other request
    /// `unknown_method`.
    fn core(docs: Vec<Exchange>, kills: &[&str], keeps: &[&str]) -> FakeCore {
        let mut exchanges = vec![recorded_one("hello", "hello")];
        exchanges.extend(docs);
        exchanges.extend(kills.iter().map(|t| kill_of(t)));
        exchanges.extend(keeps.iter().map(|t| keep_off(t)));
        FakeCore::serve(exchanges)
    }

    /// A run of `herdr-app do COMMAND PARAMS` printing `stdout`, exiting
    /// `exit`.
    fn act(command: &str, params: &Value, exit: i32, stdout: Value) -> Run {
        Run {
            args: vec!["do".into(), command.into(), params.to_string()],
            exit,
            stdout: format!("{stdout}\n"),
            stderr: String::new(),
            sleep: None,
        }
    }

    fn tab_new_params() -> Value {
        json!({ "focus": false, "space": SPACE })
    }

    fn tab_new() -> Run {
        act(
            "tab.new",
            &tab_new_params(),
            0,
            json!({ "space": SPACE, "tab": TAB, "terminal": "t4" }),
        )
    }

    fn split_params() -> Value {
        json!({
            "direction": "right",
            "pane": SHELL_PANE,
            "argv": ["/bin/np", "attach", "--pane", "12-k3j9x2qpa8"],
        })
    }

    fn split() -> Run {
        act(
            "pane.split",
            &split_params(),
            0,
            json!({ "pane": ATTACH_PANE, "terminal": "t5" }),
        )
    }

    /// Recorded run `name`, a refusal, as the answer to `do COMMAND
    /// PARAMS`.
    fn refusal(name: &str, command: &str, params: &Value) -> Run {
        let mut run = recorded_run(name);
        run.args = vec!["do".into(), command.into(), params.to_string()];
        run
    }

    /// `run`, its words `from` reading `to`.
    fn reworded(mut run: Run, from: &str, to: &str) -> Run {
        run.stdout = run.stdout.replace(from, to);
        run
    }

    fn given_of(core: &FakeCore, app: &FakeApp) -> Given {
        given(core, app, &[("ORIGIN", "t3"), ("ATTACH", ATTACH)])
    }

    fn opened(warnings: &[&str]) -> Result<Opened, Failure> {
        Ok(Opened {
            terminal: "t5".into(),
            warnings: warnings.iter().map(|w| w.to_string()).collect(),
        })
    }

    /// The terminals `core` was asked to kill, in order.
    fn killed(core: &FakeCore) -> Vec<Value> {
        core.asked()
            .into_iter()
            .filter(|r| r["method"] == "terminal.kill")
            .map(|r| r["params"]["terminal"].clone())
            .collect()
    }

    #[test]
    fn it_splits_the_attach_into_a_bare_new_tab_of_the_origins_space_then_kills_its_shell() {
        let core = core(
            vec![
                doc_with(&[]),
                doc_with(&[("t4", SHELL_PANE)]),
                doc_with(&[("t5", ATTACH_PANE)]),
            ],
            &["t4"],
            &["t5"],
        );
        let app = FakeApp::new(&[tab_new(), split()]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            opened(&[])
        );
        assert_eq!(
            app.asked(),
            [
                format!("do tab.new {}", tab_new_params()),
                format!("do pane.split {}", split_params()),
            ]
        );
        assert_eq!(
            core.methods(),
            [
                "hello",
                "doc.get",
                "doc.get",
                "terminal.kill",
                "terminal.keep",
                "doc.get"
            ]
        );
        let asked = core.asked();
        assert_eq!(asked[3]["params"], json!({ "terminal": "t4" }));
        assert_eq!(
            asked[4]["params"],
            json!({ "terminal": "t5", "keep": false })
        );
    }

    #[test]
    fn a_block_the_doc_doesnt_show_yet_is_waited_for() {
        let core = core(
            vec![
                doc_with(&[]),
                doc_with(&[]),
                doc_with(&[("t4", SHELL_PANE)]),
                doc_with(&[("t4", SHELL_PANE), ("t5", ATTACH_PANE)]),
                doc_with(&[("t5", ATTACH_PANE)]),
            ],
            &["t4"],
            &["t5"],
        );
        let app = FakeApp::new(&[tab_new(), split()]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            opened(&[])
        );
        let docs = core.methods().iter().filter(|m| *m == "doc.get").count();
        assert_eq!(docs, 4);
    }

    #[test]
    fn a_block_is_waited_for_no_longer_than_its_wait() {
        let core = core(vec![doc_with(&[])], &[], &[]);
        let mut c = Core::connect(&core.path(), Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        assert_eq!(
            block_of(&mut c, "t5", NEVER),
            Err(Failure::Failed("t5 has no Herdr app pane".into()))
        );
        assert!(started.elapsed() < NEVER + Duration::from_millis(500));
        let docs = core.methods().iter().filter(|m| *m == "doc.get").count();
        assert!(docs >= 2, "read {docs} times");
    }

    #[test]
    fn an_attach_the_doc_never_shows_is_killed_and_the_open_fails_saying_so_first() {
        // its shell killed, the tab holds the attach alone, never in the
        // doc; its keep refused
        let core = core(
            vec![
                doc_with(&[]),
                doc_with(&[("t4", SHELL_PANE)]),
                doc_with(&[]),
            ],
            &["t4", "t5"],
            &[],
        );
        let app = FakeApp::new(&[tab_new(), split()]);
        let Err(Failure::Failed(why)) = open_tab_page_within(&given_of(&core, &app), times(NEVER))
        else {
            panic!("opened");
        };
        assert!(
            why.starts_with("t5 has no Herdr app pane; t5 will come back as a shell"),
            "{why}"
        );
        assert_eq!(killed(&core), [json!("t4"), json!("t5")]);
        let docs = core.methods().iter().filter(|m| *m == "doc.get").count();
        assert!(docs >= 4, "read {docs} times");
    }

    #[test]
    fn a_split_that_outlasts_its_time_still_has_the_shell_killed() {
        let core = core(
            vec![doc_with(&[]), doc_with(&[("t4", SHELL_PANE)])],
            &["t4"],
            &[],
        );
        let mut slow = split();
        slow.sleep = Some(10.0);
        let app = FakeApp::new(&[tab_new(), slow]);
        let times = Times {
            open: Duration::from_secs(2),
            ..times(SHOWN)
        };
        let Err(Failure::Failed(why)) = open_tab_page_within(&given_of(&core, &app), times) else {
            panic!("opened");
        };
        assert!(why.ends_with("killed, out of time"), "{why}");
        assert_eq!(killed(&core), [json!("t4")]);
        assert_eq!(core.methods().last().unwrap(), "terminal.kill");
    }

    #[test]
    fn an_origin_in_no_pane_or_two_fails_before_anything_is_made() {
        let app = FakeApp::new(&[tab_new(), split()]);
        let core = core(vec![doc_with(&[])], &[], &[]);
        let given = given(&core, &app, &[("ORIGIN", "t9"), ("ATTACH", ATTACH)]);
        assert_eq!(
            open_tab_page_within(&given, times(SHOWN)),
            Err(Failure::Failed("t9 has no Herdr app pane".into()))
        );
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        let text = doc.replies[0].to_string().replace("\"t2\"", "\"t3\"");
        doc.replies[0] = serde_json::from_str(&text).unwrap();
        let core = self::core(vec![doc], &[], &[]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            Err(Failure::Failed("t3 shows in 2 Herdr app panes".into()))
        );
        assert!(app.asked().is_empty());
    }

    #[test]
    fn a_tab_the_app_refuses_fails_the_open_with_its_words() {
        let core = core(vec![doc_with(&[])], &[], &[]);
        let refused = reworded(
            refusal("err-tab.new-unknown-space", "tab.new", &tab_new_params()),
            "spnope",
            SPACE,
        );
        let app = FakeApp::new(&[refused]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            Err(Failure::Failed(format!("herdr-app: no space \"{SPACE}\"")))
        );
        assert_eq!(core.methods(), ["hello", "doc.get"]);
    }

    #[test]
    fn a_split_that_fails_kills_the_new_tabs_shell() {
        let core = core(
            vec![doc_with(&[]), doc_with(&[("t4", SHELL_PANE)])],
            &["t4"],
            &[],
        );
        let refused = refusal("err-unknown-pane", "pane.split", &split_params());
        let app = FakeApp::new(&[tab_new(), refused]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            Err(Failure::Failed("herdr-app: no such pane".into()))
        );
        assert_eq!(killed(&core), [json!("t4")]);
        assert_eq!(core.methods().last().unwrap(), "terminal.kill");
    }

    #[test]
    fn a_shell_already_gone_is_killed_and_one_the_core_wont_kill_or_keep_is_warned_of() {
        let docs = || {
            vec![
                doc_with(&[]),
                doc_with(&[("t4", SHELL_PANE)]),
                doc_with(&[("t4", SHELL_PANE), ("t5", ATTACH_PANE)]),
            ]
        };
        let mut exchanges = vec![recorded_one("hello", "hello"), kill_not_found("t4")];
        exchanges.extend(docs());
        exchanges.push(keep_off("t5"));
        let gone = FakeCore::serve(exchanges);
        let app = FakeApp::new(&[tab_new(), split()]);
        assert_eq!(
            open_tab_page_within(&given_of(&gone, &app), times(SHOWN)),
            opened(&[])
        );

        // a core that kills and keeps nothing
        let core = core(docs(), &[], &[]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            opened(&[
                "the new tab's shell t4 is left, so the tab won't close with the attach: \
                 hc core: unknown method \"terminal.kill\"",
                "t5 will come back as a shell if the hc core restarts: hc core: unknown \
                 method \"terminal.keep\"",
            ])
        );
    }

    #[test]
    fn a_core_lacking_a_method_it_calls_is_unsupported_before_anything_is_made() {
        for method in ["doc.get", "terminal.kill", "terminal.keep"] {
            let mut hello = recorded_one("hello", "hello");
            let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
            methods.retain(|m| m != method);
            let core = FakeCore::serve(vec![hello]);
            let app = FakeApp::new(&[recorded_run("version")]);
            assert_eq!(
                open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
                Err(Failure::Unsupported(format!(
                    "the hc core has no {method} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
            assert_eq!(app.asked(), ["--version"]);
        }
    }

    #[test]
    fn an_app_without_a_command_or_argument_it_runs_is_unsupported_naming_it_and_both_versions() {
        let core = core(vec![doc_with(&[])], &[], &[]);
        let unknown = reworded(
            refusal("err-unknown-command", "tab.new", &tab_new_params()),
            "no.such",
            "tab.new",
        );
        let app = FakeApp::new(&[unknown, recorded_run("version")]);
        assert_eq!(
            open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
            Err(Failure::Unsupported(
                "herdr-app has no tab.new (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );

        // made a tab, but can't split into it: its shell is killed
        let unknowns = [
            reworded(
                refusal("err-unknown-command", "pane.split", &split_params()),
                "no.such",
                "pane.split",
            ),
            reworded(
                refusal("err-unknown-argument", "pane.split", &split_params()),
                "bogus",
                "argv",
            ),
        ];
        let lacks = [" has no pane.split", "'s pane.split takes no argv"];
        for (unknown, lacks) in unknowns.into_iter().zip(lacks) {
            let core = self::core(
                vec![doc_with(&[]), doc_with(&[("t4", SHELL_PANE)])],
                &["t4"],
                &[],
            );
            let app = FakeApp::new(&[tab_new(), unknown, recorded_run("version")]);
            assert_eq!(
                open_tab_page_within(&given_of(&core, &app), times(SHOWN)),
                Err(Failure::Unsupported(format!(
                    "herdr-app{lacks} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
            assert_eq!(killed(&core), [json!("t4")]);
        }
    }

    #[test]
    fn no_herdr_app_is_unsupported_naming_where_it_looked() {
        let core = core(vec![doc_with(&[])], &[], &[]);
        let socket = core.path();
        let up = crate::name::upper();
        let given = Given::from(move |name| match name {
            "HC_TERMINAL_SOCKET" => Some(socket.clone()),
            "PATH" => Some(b"/nowhere".to_vec()),
            "HOME" => Some(b"/no/home".to_vec()),
            n if n == format!("{up}_ORIGIN") => Some(b"t3".to_vec()),
            n if n == format!("{up}_ATTACH") => Some(ATTACH.as_bytes().to_vec()),
            _ => None,
        });
        assert_eq!(
            open_tab_page_within(&given, times(SHOWN)),
            Err(Failure::Unsupported(
                "no herdr-app on PATH, nor at /no/home/.local/bin/herdr-app".into()
            ))
        );
    }

    #[test]
    fn no_origin_or_attach_fails_before_the_core_is_asked() {
        let core = core(vec![doc_with(&[])], &[], &[]);
        let app = FakeApp::new(&[]);
        let cases = [
            (vec![("ATTACH", ATTACH)], "ORIGIN isn't set"),
            (vec![("ORIGIN", "t3")], "ATTACH is empty"),
        ];
        for (vars, why) in cases {
            assert_eq!(
                open_tab_page_within(&given(&core, &app, &vars), times(SHOWN)),
                Err(Failure::Failed(why.into()))
            );
        }
        assert!(core.asked().is_empty());
    }
}
