//! Opening and closing panes (ADR 0017). herdr's `pane.split` runs no
//! command, so `open_beside` makes the terminal where one can run an
//! argv, a new tab, and moves it:
//!
//! 1. A zoomed origin tab is refused: herdr refuses a move into one.
//! 2. `layout.apply` makes a tab (`focus:false`) in the origin's
//!    workspace, its one pane running the attach command, with no shell.
//! 3. `pane.move` puts that pane beside the origin, which empties and
//!    closes its tab. herdr only puts it right of or below the origin,
//!    and its `ratio` is the first pane's share.
//! 4. For left or up, `pane.swap` swaps the two. A swap focuses its
//!    source, the origin: the server's focus is put back if it was
//!    elsewhere.
//! 5. The tab's root split is set to the ratio it has: herdr resizes no
//!    pty on a move, and laying out the tab again resizes every one. It is
//!    set again 100 ms later: a terminal that went to the alternate screen
//!    before a client drew it in its new tab is otherwise left a column
//!    short, herdr keeping its scrollbar's column, until the next layout.
//!
//! Any failure after the tab is made closes its pane. The pane keeps its
//! id through the move, within its workspace: that is what it prints.

use serde_json::{Value, json};

use super::client::{Herdr, UNREADABLE, text};
use crate::providers::builtin::Given;

/// What `open_beside` has, of its 5 s.
const OPEN_TIME: std::time::Duration = std::time::Duration::from_millis(4500);
/// How long after laying the tab out `open_beside` lays it out again.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(100);
/// What `close` has, of its 2 s.
const CLOSE_TIME: std::time::Duration = std::time::Duration::from_millis(1500);

/// The new pane's id, or why there's none.
pub(super) fn open_beside(given: &Given) -> Result<String, String> {
    let origin = input(given, "origin")?;
    let direction = given.var("direction").unwrap_or_default();
    let (split, before) = match direction.as_slice() {
        b"left" => ("right", true),
        b"right" => ("right", false),
        b"up" => ("down", true),
        b"down" => ("down", false),
        other => {
            return Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let ratio = ratio(given)?;
    // herdr's ratio is the first pane's share: the origin's, or after a
    // swap, the new one's
    let first = if before { ratio } else { 1.0 - ratio };
    let first = first.clamp(0.1, 0.9);
    let words = attach_words(given)?;
    let herdr = Herdr::of(given, OPEN_TIME)?;

    let snapshot = herdr.call("session.snapshot", json!({}))?;
    let snapshot = &snapshot["snapshot"];
    let layout = tab_of(snapshot, &origin)?;
    let tab = text(layout, "tab_id")?.to_string();
    if layout["zoomed"] == true {
        return Err(format!("herdr tab {tab} is zoomed"));
    }
    let workspace = text(layout, "workspace_id")?;
    let focused = snapshot["focused_pane_id"].as_str().map(String::from);
    let applied = herdr.call(
        "layout.apply",
        json!({
            "workspace_id": workspace,
            "focus": false,
            "root": { "type": "pane", "command": words },
        }),
    )?;
    let made = text(&applied["layout"]["root"], "pane_id")?.to_string();
    let placed = (|| {
        let moved = herdr.call(
            "pane.move",
            json!({
                "pane_id": made,
                "focus": false,
                "destination": {
                    "type": "tab",
                    "tab_id": tab,
                    "target_pane_id": origin,
                    "split": split,
                    "ratio": first,
                },
            }),
        )?;
        let moved = &moved["move_result"];
        if moved["changed"] != true {
            return Err(match moved["reason"].as_str() {
                Some("zoomed_tab") => format!("herdr tab {tab} is zoomed"),
                Some(reason) => format!("herdr didn't move pane {made} beside {origin}: {reason}"),
                None => UNREADABLE.into(),
            });
        }
        let pane = text(&moved["pane"], "pane_id")?.to_string();
        let mut laid_out = moved["target_layout"].clone();
        if before {
            let swapped = herdr.call(
                "pane.swap",
                json!({ "source_pane_id": origin, "target_pane_id": pane }),
            )?;
            laid_out = swapped["swap"]["layout"].clone();
            if let Some(focused) = focused.filter(|f| *f != origin)
                && let Err(e) = herdr.call("pane.focus", json!({ "pane_id": focused }))
            {
                eprintln!("herdr open_beside: can't focus {focused} again: {}", e.why);
            }
        }
        let root = root_ratio(&laid_out).ok_or(UNREADABLE)?;
        let lay_out = || {
            herdr.call(
                "layout.set_split_ratio",
                json!({ "tab_id": tab, "path": [], "ratio": root }),
            )
        };
        lay_out()?;
        // a terminal that went to the alternate screen before a client
        // drew it in its new tab keeps the scrollbar's column until the
        // tab is laid out again, once drawn
        std::thread::sleep(SETTLE);
        if let Err(e) = lay_out() {
            eprintln!(
                "herdr open_beside: can't lay tab {tab} out again: {}",
                e.why
            );
        }
        Ok(pane)
    })();
    placed.inspect_err(|_| {
        // its tab, or wherever it is
        let _ = herdr.call("pane.close", json!({ "pane_id": made }));
    })
}

/// The layout, in `snapshot`, of the tab holding pane `pane`, or why
/// there's none.
fn tab_of<'a>(snapshot: &'a Value, pane: &str) -> Result<&'a Value, String> {
    let layouts = snapshot["layouts"].as_array().ok_or(UNREADABLE)?;
    layouts
        .iter()
        .find(|l| {
            l["panes"]
                .as_array()
                .is_some_and(|ps| ps.iter().any(|p| p["pane_id"] == pane))
        })
        .ok_or_else(|| format!("herdr has no pane {pane}"))
}

/// The ratio of the root split of a tab's `layout`: the split whose rect
/// is the whole area. None if it has none.
fn root_ratio(layout: &Value) -> Option<f64> {
    let area = &layout["area"];
    layout["splits"]
        .as_array()?
        .iter()
        .find(|s| s["rect"] == *area)?["ratio"]
        .as_f64()
}

/// Kills the pane, or says why it's still there.
pub(super) fn close(given: &Given) -> Result<(), String> {
    let pane = input(given, "terminal")?;
    let herdr = Herdr::of(given, CLOSE_TIME)?;
    match herdr.call("pane.close", json!({ "pane_id": pane })) {
        Ok(_) => Ok(()),
        Err(refused) if refused.code.as_deref() == Some("pane_not_found") => Ok(()),
        Err(refused) => Err(refused.why),
    }
}

/// Input `name`, which must be set, as text.
fn input(given: &Given, name: &str) -> Result<String, String> {
    let shown = name.to_ascii_uppercase();
    let value = given.var(name).filter(|v| !v.is_empty());
    let value = value.ok_or_else(|| format!("{shown} isn't set"))?;
    String::from_utf8(value).map_err(|_| format!("{shown} isn't UTF-8"))
}

/// `RATIO`, the new terminal's share, between 0 and 1; 0.5 if unset.
fn ratio(given: &Given) -> Result<f64, String> {
    let Some(ratio) = given.var("ratio").filter(|r| !r.is_empty()) else {
        return Ok(0.5);
    };
    let shown = String::from_utf8_lossy(&ratio);
    shown
        .parse::<f64>()
        .ok()
        .filter(|r| *r > 0.0 && *r < 1.0)
        .ok_or_else(|| format!("RATIO {shown:?} isn't between 0 and 1"))
}

/// The attach command's words, from `ATTACH`, as UTF-8: herdr's JSON
/// can't carry other bytes.
fn attach_words(given: &Given) -> Result<Vec<String>, String> {
    given
        .attach_words()?
        .into_iter()
        .map(|w| {
            w.into_string()
                .map_err(|_| "the attach command isn't UTF-8, which herdr can't run".into())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Fake, Reply, recorded};
    use super::*;

    /// What a capability is given beside `fake`: its `<NAME>_*` inputs
    /// `vars`, and the choosing attach's `HERDR_SOCKET_PATH`.
    fn given(fake: &Fake, vars: &[(&str, &str)]) -> Given {
        let up = crate::name::upper();
        let mut env: Vec<(String, Vec<u8>)> = vars
            .iter()
            .map(|(k, v)| (format!("{up}_{k}"), v.as_bytes().to_vec()))
            .collect();
        env.push(("HERDR_SOCKET_PATH".into(), fake.path()));
        Given::from(move |name| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    const ATTACH: &str = "/bin/np attach --address '/tmp/a b'";

    /// herdr 0.9.5 as it answered one open beside `w2:p3`.
    fn opening(moved: &str) -> Fake {
        Fake::serve(vec![
            ("session.snapshot", Reply::Line(recorded("open-snapshot"))),
            ("layout.apply", Reply::Line(recorded("open-apply"))),
            ("pane.move", Reply::Line(recorded(moved))),
            ("pane.swap", Reply::Line(recorded("open-swap"))),
            ("pane.focus", Reply::Line(recorded("open-focus"))),
            (
                "layout.set_split_ratio",
                Reply::Line(recorded("open-set-ratio")),
            ),
            ("pane.close", Reply::Line(recorded("pane-close"))),
        ])
    }

    fn open(fake: &Fake, direction: &str, ratio: &str) -> Result<String, String> {
        open_beside(&given(
            fake,
            &[
                ("ORIGIN", "w2:p3"),
                ("DIRECTION", direction),
                ("RATIO", ratio),
                ("ATTACH", ATTACH),
            ],
        ))
    }

    fn close_to(a: &Value, b: f64) -> bool {
        a.as_f64().is_some_and(|a| (a - b).abs() < 1e-9)
    }

    #[test]
    fn right_runs_the_attach_in_a_new_tab_moves_it_beside_and_lays_the_tab_out_again() {
        let fake = opening("open-move");
        assert_eq!(open(&fake, "right", "0.3"), Ok("w2:p4".into()));
        assert_eq!(
            fake.methods(),
            [
                "session.snapshot",
                "layout.apply",
                "pane.move",
                "layout.set_split_ratio",
                "layout.set_split_ratio"
            ]
        );
        let asked = fake.asked();
        assert_eq!(
            asked[1]["params"],
            json!({
                "workspace_id": "w2",
                "focus": false,
                "root": { "type": "pane", "command": ["/bin/np", "attach", "--address", "/tmp/a b"] },
            })
        );
        let moved = &asked[2]["params"];
        assert_eq!(moved["pane_id"], "w2:p4");
        assert_eq!(moved["focus"], false);
        let to = &moved["destination"];
        assert_eq!(
            (
                &to["type"],
                &to["tab_id"],
                &to["target_pane_id"],
                &to["split"]
            ),
            (
                &json!("tab"),
                &json!("w2:t1"),
                &json!("w2:p3"),
                &json!("right")
            )
        );
        // herdr's ratio is the origin's share
        assert!(close_to(&to["ratio"], 0.7), "{to}");
        // the root split, to the ratio the move's reply has it at
        let set = &asked[3]["params"];
        assert_eq!(
            (&set["tab_id"], &set["path"]),
            (&json!("w2:t1"), &json!([]))
        );
        assert!(close_to(&set["ratio"], 0.5), "{set}");
    }

    #[test]
    fn left_swaps_the_two_and_gives_the_focus_back() {
        let fake = opening("open-move");
        assert_eq!(open(&fake, "left", "0.4"), Ok("w2:p4".into()));
        assert_eq!(
            fake.methods(),
            [
                "session.snapshot",
                "layout.apply",
                "pane.move",
                "pane.swap",
                "pane.focus",
                "layout.set_split_ratio",
                "layout.set_split_ratio"
            ]
        );
        let asked = fake.asked();
        // the new pane is the first after the swap: the ratio is its share
        let to = &asked[2]["params"]["destination"];
        assert_eq!(to["split"], "right");
        assert!(close_to(&to["ratio"], 0.4), "{to}");
        assert_eq!(
            asked[3]["params"],
            json!({ "source_pane_id": "w2:p3", "target_pane_id": "w2:p4" })
        );
        // the server's focus was w1:p1, not the origin
        assert_eq!(asked[4]["params"], json!({ "pane_id": "w1:p1" }));
    }

    #[test]
    fn up_splits_down_and_swaps_and_the_ratio_is_kept_within_herdrs_bounds() {
        let fake = opening("open-move");
        assert_eq!(open(&fake, "up", "0.05"), Ok("w2:p4".into()));
        let asked = fake.asked();
        let to = &asked[2]["params"]["destination"];
        assert_eq!(to["split"], "down");
        assert!(close_to(&to["ratio"], 0.1), "{to}");
        assert_eq!(asked[3]["method"], "pane.swap");
        let fake = opening("open-move");
        assert_eq!(open(&fake, "down", "0.95"), Ok("w2:p4".into()));
        let to = &fake.asked()[2]["params"]["destination"];
        assert_eq!(to["split"], "down");
        assert!(close_to(&to["ratio"], 0.1), "{to}");
    }

    #[test]
    fn a_zoomed_origin_is_refused_before_anything_is_made() {
        let fake = Fake::serve(vec![(
            "session.snapshot",
            Reply::Line(recorded("session-snapshot-zoomed")),
        )]);
        let opened = open_beside(&given(
            &fake,
            &[
                ("ORIGIN", "w2:p1"),
                ("DIRECTION", "right"),
                ("ATTACH", ATTACH),
            ],
        ));
        assert_eq!(opened, Err("herdr tab w2:t1 is zoomed".into()));
        assert_eq!(fake.methods(), ["session.snapshot"]);
    }

    #[test]
    fn a_tab_zoomed_before_the_move_closes_the_pane_made() {
        let fake = opening("pane-move-zoomed");
        assert_eq!(
            open(&fake, "right", "0.5"),
            Err("herdr tab w2:t1 is zoomed".into())
        );
        let asked = fake.asked();
        assert_eq!(asked.last().unwrap()["method"], "pane.close");
        assert_eq!(
            asked.last().unwrap()["params"],
            json!({ "pane_id": "w2:p4" })
        );
    }

    #[test]
    fn any_failure_after_the_tab_is_made_closes_its_pane() {
        let fake = Fake::serve(vec![
            ("session.snapshot", Reply::Line(recorded("open-snapshot"))),
            ("layout.apply", Reply::Line(recorded("open-apply"))),
            ("pane.move", Reply::Line(recorded("open-move"))),
            (
                "layout.set_split_ratio",
                Reply::Line(recorded("pane-not-found")),
            ),
            ("pane.close", Reply::Line(recorded("pane-close"))),
        ]);
        assert_eq!(
            open(&fake, "right", "0.5"),
            Err("herdr: pane not found".into())
        );
        assert_eq!(fake.methods().last().unwrap(), "pane.close");
        // nothing made, nothing closed
        let fake = Fake::serve(vec![
            ("session.snapshot", Reply::Line(recorded("open-snapshot"))),
            ("layout.apply", Reply::Line(recorded("pane-not-found"))),
        ]);
        assert_eq!(
            open(&fake, "right", "0.5"),
            Err("herdr: pane not found".into())
        );
        assert_eq!(fake.methods(), ["session.snapshot", "layout.apply"]);
    }

    #[test]
    fn an_attach_command_not_utf8_asks_herdr_nothing() {
        let fake = opening("open-move");
        let up = crate::name::upper();
        let socket = fake.path();
        let given = Given::from(move |name| {
            let var = name.strip_prefix(&format!("{up}_"));
            match (name, var) {
                (_, Some("ORIGIN")) => Some(b"w2:p3".to_vec()),
                (_, Some("DIRECTION")) => Some(b"left".to_vec()),
                (_, Some("ATTACH")) => Some(b"/bin/np attach \xff".to_vec()),
                ("HERDR_SOCKET_PATH", _) => Some(socket.clone()),
                _ => None,
            }
        });
        assert_eq!(
            open_beside(&given),
            Err("the attach command isn't UTF-8, which herdr can't run".into())
        );
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn bad_inputs_ask_herdr_nothing() {
        let fake = opening("open-move");
        for (vars, why) in [
            (vec![("DIRECTION", "left")], "ORIGIN isn't set"),
            (
                vec![("ORIGIN", "w2:p3"), ("DIRECTION", "sideways")],
                "DIRECTION \"sideways\" isn't left, right, up or down",
            ),
            (
                vec![("ORIGIN", "w2:p3"), ("DIRECTION", "left"), ("RATIO", "1.5")],
                "RATIO \"1.5\" isn't between 0 and 1",
            ),
            (
                vec![("ORIGIN", "w2:p3"), ("DIRECTION", "left")],
                "ATTACH is empty",
            ),
        ] {
            assert_eq!(open_beside(&given(&fake, &vars)), Err(why.into()));
        }
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn close_closes_the_pane_and_one_already_gone_is_done() {
        for reply in ["pane-close", "pane-close-gone"] {
            let fake = Fake::serve(vec![("pane.close", Reply::Line(recorded(reply)))]);
            assert_eq!(close(&given(&fake, &[("TERMINAL", "w2:p2")])), Ok(()));
            assert_eq!(fake.asked()[0]["params"], json!({ "pane_id": "w2:p2" }));
        }
        let fake = Fake::serve(vec![(
            "pane.close",
            Reply::Line(r#"{"id":"x","error":{"code":"internal","message":"boom"}}"#.into()),
        )]);
        assert_eq!(
            close(&given(&fake, &[("TERMINAL", "w2:p2")])),
            Err("herdr: boom".into())
        );
    }
}
