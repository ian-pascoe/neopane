//! `focus` and `focus_direction`, as `docs/capabilities.md`'s Herdr app
//! section gives them: `herdr-app do pane.focus` of the terminal's pane,
//! as the core's doc maps it, and `COUNT` times `herdr-app do pane.go`.
//! Each says `hello` to the core first, which `focus_direction` asks
//! nothing more; no `herdr-app` is unsupported, as is a core or app
//! lacking what they need ([`refused`](super::refused)).

use std::time::Duration;

use serde_json::json;

use super::core::Core;
use super::doc::pane_of;
use super::{Failure, act, methods, needs, window_of};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `focus` and `focus_direction` have, of their 2 s.
const FOCUS_TIME: Duration = Duration::from_millis(1500);

/// Focuses the terminal, or says why not.
pub(super) fn focus(given: &Given) -> Result<(), Failure> {
    focus_within(given, FOCUS_TIME)
}

/// `focus`, having `time`.
fn focus_within(given: &Given, time: Duration) -> Result<(), Failure> {
    let terminal = given.text("terminal")?;
    let window = window_of(given)?;
    let mut core = Core::of(given, time)?;
    needs(&core, methods(Capability::Focus), || Ok(window.clone()))?;
    let pane = pane_of(&core.doc().map_err(String::from)?, &terminal)?;
    act(&window, &core, "pane.focus", &json!({ "pane": pane }))?;
    Ok(())
}

/// Moves focus `COUNT` panes `DIRECTION`, or says why not.
pub(super) fn focus_direction(given: &Given) -> Result<(), Failure> {
    focus_direction_within(given, FOCUS_TIME)
}

/// `focus_direction`, having `time`.
fn focus_direction_within(given: &Given, time: Duration) -> Result<(), Failure> {
    let direction = given.var("direction").unwrap_or_default();
    let direction = match direction.as_slice() {
        b"left" => "left",
        b"right" => "right",
        b"up" => "up",
        b"down" => "down",
        other => {
            return Err(Failure::Failed(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            )));
        }
    };
    let count = given.var("count").unwrap_or_default();
    let count = std::str::from_utf8(&count)
        .ok()
        .and_then(|c| c.parse::<u32>().ok())
        .filter(|c| (1..=20).contains(c))
        .ok_or_else(|| format!("COUNT {:?} isn't 1 to 20", String::from_utf8_lossy(&count)))?;
    let window = window_of(given)?;
    let core = Core::of(given, time)?;
    needs(&core, methods(Capability::FocusDirection), || {
        Ok(window.clone())
    })?;
    let args = json!({ "direction": direction });
    for _ in 0..count {
        act(&window, &core, "pane.go", &args)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::Failure;
    use super::super::fake::{
        FakeApp, FakeCore, Run, given, recorded_do, recorded_one, recorded_run,
    };
    use super::*;

    /// Well over the real times, as the fakes run shell scripts.
    const TIME: Duration = Duration::from_secs(5);

    /// `herdr-app do COMMAND` refused as an app without it refuses it.
    fn unknown(command: &str, params: &str) -> Run {
        let mut run = recorded_run("err-unknown-command");
        run.args = vec!["do".into(), command.into(), params.into()];
        run.stdout = run.stdout.replace("no.such", command);
        run
    }

    #[test]
    fn focus_focuses_the_terminals_pane() {
        let core = FakeCore::replaying(&["nested-doc.get"]);
        let app = FakeApp::new(&[recorded_do("do-pane.focus")]);
        let given = given(&core, &app, &[("TERMINAL", "t2")]);
        assert_eq!(focus_within(&given, TIME), Ok(()));
        assert_eq!(core.methods(), ["hello", "doc.get"]);
        assert_eq!(
            app.asked(),
            [r#"do pane.focus {"pane":"p7b3398d57b6dd529d0f978c04b35c687"}"#]
        );
    }

    #[test]
    fn focus_of_a_terminal_in_no_pane_or_two_fails_asking_no_window() {
        let core = FakeCore::replaying(&["nested-doc.get"]);
        let app = FakeApp::new(&[]);
        let nowhere = given(&core, &app, &[("TERMINAL", "t9")]);
        assert_eq!(
            focus_within(&nowhere, TIME),
            Err(Failure::Failed("t9 has no Herdr app pane".into()))
        );
        // t2's block showing t3 too
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        let twice = doc.replies[0].to_string().replace("\"t2\"", "\"t3\"");
        doc.replies[0] = serde_json::from_str(&twice).unwrap();
        let core = FakeCore::serve(vec![recorded_one("hello", "hello"), doc]);
        let twice = given(&core, &app, &[("TERMINAL", "t3")]);
        assert_eq!(
            focus_within(&twice, TIME),
            Err(Failure::Failed("t3 shows in 2 Herdr app panes".into()))
        );
        let unset = given(&core, &app, &[("TERMINAL", "")]);
        assert_eq!(
            focus_within(&unset, TIME),
            Err(Failure::Failed("TERMINAL isn't set".into()))
        );
        assert!(app.asked().is_empty());
    }

    #[test]
    fn focus_on_a_core_without_doc_get_is_unsupported_naming_both_versions() {
        let mut hello = recorded_one("hello", "hello");
        let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
        methods.retain(|m| m != "doc.get");
        let core = FakeCore::serve(vec![hello]);
        let app = FakeApp::new(&[recorded_run("version")]);
        let given = given(&core, &app, &[("TERMINAL", "t2")]);
        assert_eq!(
            focus_within(&given, TIME),
            Err(Failure::Unsupported(
                "the hc core has no doc.get (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
        assert_eq!(core.methods(), ["hello"]);
    }

    #[test]
    fn no_herdr_app_is_unsupported_asking_the_core_nothing() {
        let core = FakeCore::replaying(&["hello"]);
        let none = Given::from({
            let socket = core.path();
            let up = crate::name::upper();
            move |name: &str| match name {
                "HC_TERMINAL_SOCKET" => Some(socket.clone()),
                "PATH" | "HOME" => Some(b"/nowhere".to_vec()),
                n if n == format!("{up}_TERMINAL") => Some(b"t2".to_vec()),
                n if n == format!("{up}_DIRECTION") => Some(b"up".to_vec()),
                n if n == format!("{up}_COUNT") => Some(b"1".to_vec()),
                _ => None,
            }
        });
        let why = "no herdr-app on PATH, nor at /nowhere/.local/bin/herdr-app";
        assert_eq!(
            focus_within(&none, TIME),
            Err(Failure::Unsupported(why.into()))
        );
        assert_eq!(
            focus_direction_within(&none, TIME),
            Err(Failure::Unsupported(why.into()))
        );
        assert!(core.methods().is_empty());
    }

    #[test]
    fn focus_refused_by_the_app_fails_with_its_words() {
        let core = FakeCore::replaying(&["nested-doc.get"]);
        let mut gone = recorded_do("err-unknown-pane");
        gone.args[2] = r#"{"pane":"p7b3398d57b6dd529d0f978c04b35c687"}"#.into();
        let app = FakeApp::new(&[gone]);
        let given = given(&core, &app, &[("TERMINAL", "t2")]);
        assert_eq!(
            focus_within(&given, TIME),
            Err(Failure::Failed("herdr-app: no such pane".into()))
        );
    }

    #[test]
    fn focus_on_an_app_without_pane_focus_is_unsupported_naming_both_versions() {
        let core = FakeCore::replaying(&["nested-doc.get"]);
        let app = FakeApp::new(&[
            unknown(
                "pane.focus",
                r#"{"pane":"p7b3398d57b6dd529d0f978c04b35c687"}"#,
            ),
            recorded_run("version"),
        ]);
        let given = given(&core, &app, &[("TERMINAL", "t2")]);
        assert_eq!(
            focus_within(&given, TIME),
            Err(Failure::Unsupported(
                "herdr-app has no pane.focus (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
    }

    #[test]
    fn focus_direction_goes_count_times_from_the_windows_focus() {
        let core = FakeCore::replaying(&["hello"]);
        let app = FakeApp::new(&[recorded_do("do-pane.go")]);
        let given = given(&core, &app, &[("DIRECTION", "right"), ("COUNT", "3")]);
        assert_eq!(focus_direction_within(&given, TIME), Ok(()));
        assert_eq!(app.asked(), [r#"do pane.go {"direction":"right"}"#; 3]);
        // the core is asked only hello
        assert_eq!(core.methods(), ["hello"]);
    }

    #[test]
    fn focus_direction_at_the_edge_is_done() {
        let core = FakeCore::replaying(&["hello"]);
        let app = FakeApp::new(&[recorded_do("do-pane.go-edge")]);
        let given = given(&core, &app, &[("DIRECTION", "left"), ("COUNT", "1")]);
        assert_eq!(focus_direction_within(&given, TIME), Ok(()));
    }

    #[test]
    fn focus_direction_needs_a_direction_and_a_count_of_1_to_20() {
        let core = FakeCore::replaying(&["hello"]);
        let app = FakeApp::new(&[]);
        for (direction, count, why) in [
            (
                "diagonal",
                "1",
                r#"DIRECTION "diagonal" isn't left, right, up or down"#,
            ),
            ("up", "0", r#"COUNT "0" isn't 1 to 20"#),
            ("up", "21", r#"COUNT "21" isn't 1 to 20"#),
            ("up", "", r#"COUNT "" isn't 1 to 20"#),
        ] {
            let given = given(&core, &app, &[("DIRECTION", direction), ("COUNT", count)]);
            assert_eq!(
                focus_direction_within(&given, TIME),
                Err(Failure::Failed(why.into()))
            );
        }
        assert!(app.asked().is_empty());
    }

    #[test]
    fn focus_direction_stops_at_a_failure() {
        let core = FakeCore::replaying(&["hello"]);
        let mut no_window = recorded_run("err-no-window-do");
        no_window.args = vec![
            "do".into(),
            "pane.go".into(),
            r#"{"direction":"down"}"#.into(),
        ];
        let app = FakeApp::new(&[no_window]);
        let given = given(&core, &app, &[("DIRECTION", "down"), ("COUNT", "4")]);
        let Err(Failure::Failed(why)) = focus_direction_within(&given, TIME) else {
            panic!("moved with no window");
        };
        assert!(why.starts_with("herdr-app: no Herdr window at "), "{why}");
        assert_eq!(app.asked().len(), 1);
    }

    #[test]
    fn focus_direction_on_a_core_of_another_generation_is_unsupported_moving_nothing() {
        let mut hello = recorded_one("hello", "hello");
        hello.result_mut()["generation"] = serde_json::json!(2);
        let core = FakeCore::serve(vec![hello]);
        let app = FakeApp::new(&[recorded_run("version"), recorded_do("do-pane.go")]);
        let given = given(&core, &app, &[("DIRECTION", "right"), ("COUNT", "1")]);
        assert_eq!(
            focus_direction_within(&given, TIME),
            Err(Failure::Unsupported(
                "the hc core speaks generation 2; the built-in herdr-app speaks 1 \
                 (hc core 0.1.21, herdr-app 0.1.16)"
                    .into()
            ))
        );
        assert_eq!(app.asked(), ["--version"]);
    }

    #[test]
    fn focus_direction_on_an_app_without_pane_go_is_unsupported_naming_both_versions() {
        let core = FakeCore::replaying(&["hello"]);
        let app = FakeApp::new(&[
            unknown("pane.go", r#"{"direction":"up"}"#),
            recorded_run("version"),
        ]);
        let given = given(&core, &app, &[("DIRECTION", "up"), ("COUNT", "2")]);
        assert_eq!(
            focus_direction_within(&given, TIME),
            Err(Failure::Unsupported(
                "herdr-app has no pane.go (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
        assert_eq!(app.asked().len(), 2);
    }
}
