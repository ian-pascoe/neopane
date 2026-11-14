//! `focus` and `focus_direction`. herdr has one focus per server, not per
//! client: moving it moves every client attached, as herdr's own keys do.
//!
//! - `focus`: `pane.focus` on the terminal, which goes to its tab and
//!   workspace.
//! - `focus_direction`: `COUNT` steps of `pane.focus_direction`, the first
//!   from the server's focused pane, as `session.snapshot` has it, and
//!   each after from where the last landed. herdr's own stop at the tab's
//!   edge, answering `no_neighbor`: there it stops, done.

use std::time::Duration;

use serde_json::json;

use super::client::{Herdr, UNREADABLE};
use super::panes::input;
use crate::providers::builtin::Given;

/// What `focus` and `focus_direction` have, of their 2 s.
const FOCUS_TIME: Duration = Duration::from_millis(1500);

/// Focuses the terminal, or says why not.
pub(super) fn focus(given: &Given) -> Result<(), String> {
    let pane = input(given, "terminal")?;
    let herdr = Herdr::of(given, FOCUS_TIME)?;
    herdr.call("pane.focus", json!({ "pane_id": pane }))?;
    Ok(())
}

/// Moves focus `COUNT` panes `DIRECTION`, or says why not.
pub(super) fn focus_direction(given: &Given) -> Result<(), String> {
    let direction = given.var("direction").unwrap_or_default();
    let direction = match direction.as_slice() {
        b"left" => "left",
        b"right" => "right",
        b"up" => "up",
        b"down" => "down",
        other => {
            return Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let count = given.var("count").unwrap_or_default();
    let count = std::str::from_utf8(&count)
        .ok()
        .and_then(|c| c.parse::<u32>().ok())
        .filter(|c| (1..=20).contains(c))
        .ok_or_else(|| format!("COUNT {:?} isn't 1 to 20", String::from_utf8_lossy(&count)))?;
    let herdr = Herdr::of(given, FOCUS_TIME)?;
    let snapshot = herdr.call("session.snapshot", json!({}))?;
    let mut from = snapshot["snapshot"]["focused_pane_id"]
        .as_str()
        .ok_or("herdr has no focused pane")?
        .to_string();
    for _ in 0..count {
        let stepped = herdr.call(
            "pane.focus_direction",
            json!({ "pane_id": from, "direction": direction }),
        )?;
        let stepped = &stepped["focus"];
        if stepped["changed"] != true {
            // at the tab's edge: herdr's own reason is no_neighbor
            break;
        }
        from = stepped["focused_pane_id"]
            .as_str()
            .ok_or(UNREADABLE)?
            .to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::fake::{Fake, Reply, given, recorded};
    use super::*;

    #[test]
    fn focus_focuses_the_pane_by_its_id() {
        let fake = Fake::serve(vec![("pane.focus", Reply::Line(recorded("open-focus")))]);
        assert_eq!(focus(&given(&fake, &[("TERMINAL", "w1:p1")])), Ok(()));
        assert_eq!(fake.methods(), ["pane.focus"]);
        assert_eq!(fake.asked()[0]["params"], json!({ "pane_id": "w1:p1" }));
    }

    #[test]
    fn focus_on_a_pane_herdr_doesnt_have_fails() {
        let fake = Fake::serve(vec![(
            "pane.focus",
            Reply::Line(recorded("pane-not-found")),
        )]);
        assert_eq!(
            focus(&given(&fake, &[("TERMINAL", "w1:p9")])),
            Err("herdr: pane not found".into())
        );
        let fake = Fake::serve(vec![]);
        assert_eq!(focus(&given(&fake, &[])), Err("TERMINAL isn't set".into()));
        assert!(fake.asked().is_empty());
    }

    /// The params of each `pane.focus_direction` `fake` was asked.
    fn steps(fake: &Fake) -> Vec<Value> {
        fake.asked()
            .into_iter()
            .filter(|r| r["method"] == "pane.focus_direction")
            .map(|r| r["params"].clone())
            .collect()
    }

    #[test]
    fn focus_direction_steps_from_the_servers_focus_then_from_where_it_landed() {
        // the server's focus is w1:p2; the first step lands on w1:p6
        let fake = Fake::serve(vec![
            (
                "session.snapshot",
                Reply::Line(recorded("session-snapshot")),
            ),
            (
                "pane.focus_direction",
                Reply::Line(recorded("focus-direction")),
            ),
        ]);
        let given = given(&fake, &[("DIRECTION", "right"), ("COUNT", "2")]);
        assert_eq!(focus_direction(&given), Ok(()));
        assert_eq!(
            steps(&fake),
            [
                json!({ "pane_id": "w1:p2", "direction": "right" }),
                json!({ "pane_id": "w1:p6", "direction": "right" }),
            ]
        );
    }

    #[test]
    fn focus_direction_stops_at_the_edge_and_is_done() {
        let fake = Fake::serve(vec![
            (
                "session.snapshot",
                Reply::Line(recorded("session-snapshot")),
            ),
            (
                "pane.focus_direction",
                Reply::Line(recorded("focus-direction-edge")),
            ),
        ]);
        let given = given(&fake, &[("DIRECTION", "left"), ("COUNT", "5")]);
        assert_eq!(focus_direction(&given), Ok(()));
        assert_eq!(
            steps(&fake),
            [json!({ "pane_id": "w1:p2", "direction": "left" })]
        );
    }

    #[test]
    fn focus_direction_with_bad_inputs_asks_herdr_nothing() {
        let fake = Fake::serve(vec![]);
        for (vars, why) in [
            (
                vec![("DIRECTION", "sideways"), ("COUNT", "1")],
                "DIRECTION \"sideways\" isn't left, right, up or down",
            ),
            (
                vec![("DIRECTION", "up"), ("COUNT", "0")],
                "COUNT \"0\" isn't 1 to 20",
            ),
            (
                vec![("DIRECTION", "up"), ("COUNT", "21")],
                "COUNT \"21\" isn't 1 to 20",
            ),
            (vec![("DIRECTION", "up")], "COUNT \"\" isn't 1 to 20"),
        ] {
            assert_eq!(focus_direction(&given(&fake, &vars)), Err(why.into()));
        }
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn focus_direction_with_no_focused_pane_fails() {
        let empty = r#"{"id":"x","result":{"type":"session_snapshot","snapshot":{"focused_pane_id":null,"layouts":[]}}}"#;
        let fake = Fake::serve(vec![("session.snapshot", Reply::Line(empty.into()))]);
        let given = given(&fake, &[("DIRECTION", "up"), ("COUNT", "1")]);
        assert_eq!(
            focus_direction(&given),
            Err("herdr has no focused pane".into())
        );
        assert_eq!(fake.methods(), ["session.snapshot"]);
    }
}
