//! Moving and sizing a terminal against its origin: `place_beside` and
//! `resize`.
//!
//! `resize` reads the terminal's tab with `pane.layout`, and its split
//! tree from the split ids (`splits.rs`). The lowest split holding the
//! terminal and `ORIGIN` gets the ratio that gives the terminal's part
//! `SIZE` cells of content: each pane across it along the split has a
//! frame of a cell on each side, as herdr's default borders draw a tab
//! with a split, and a Neovim pane is on the alternate screen, where
//! herdr keeps no scrollbar column. One `layout.set_split_ratio`, which
//! lays the tab out again, resizing every pty in it.

use std::time::Duration;

use serde_json::json;

use super::client::{Herdr, UNREADABLE, text};
use super::panes::{Beside, Side, input, put_beside, ratio, tab_of};
use super::splits::{Node, tree};
use crate::providers::builtin::Given;

/// What `place_beside` and `resize` have, of their 2 s.
const MOVE_TIME: Duration = Duration::from_millis(1500);

/// The cells of a pane's frame along a split, both sides together, in a
/// tab with a split.
const FRAME: u16 = 2;

/// Moves the terminal beside the origin, or says why it can't.
pub(super) fn place_beside(given: &Given) -> Result<(), String> {
    let terminal = input(given, "terminal")?;
    let origin = input(given, "origin")?;
    let side = Side::of(given)?;
    let ratio = ratio(given)?;
    let size = given
        .var("size")
        .filter(|s| !s.is_empty())
        .map(|_| size(given))
        .transpose()?;
    if terminal == origin {
        return Err(format!("herdr pane {terminal} is {origin}"));
    }
    let herdr = Herdr::of(given, MOVE_TIME)?;
    let snapshot = herdr.call("session.snapshot", json!({}))?;
    let snapshot = &snapshot["snapshot"];
    let to = tab_of(snapshot, &origin)?;
    let from = tab_of(snapshot, &terminal)?;
    for layout in [to, from] {
        if layout["zoomed"] == true {
            return Err(format!("herdr tab {} is zoomed", text(layout, "tab_id")?));
        }
    }
    let tab = text(to, "tab_id")?;
    let workspace = text(to, "workspace_id")?;
    let its_workspace = text(from, "workspace_id")?;
    if its_workspace != workspace {
        return Err(format!(
            "herdr pane {terminal} is in workspace {its_workspace}, not {workspace} with \
             {origin}: moved there, it would get another id"
        ));
    }
    let focused = snapshot["focused_pane_id"].as_str();
    let beside = Beside {
        capability: "place_beside",
        origin: &origin,
        tab,
        side,
        ratio,
        focused,
    };
    // within one tab the room it leaves goes to its neighbour, which may
    // be the origin: sized after
    let size_it = |pane: &str| match size {
        Some(size) => size_against(&herdr, pane, &origin, side.across(), size),
        None => Ok(()),
    };
    if text(from, "tab_id")? != tab {
        let pane = put_beside(&herdr, &terminal, &beside)?;
        return size_it(&pane);
    }
    // herdr moves no pane within its tab: out to a tab of its own first
    let out = herdr.call(
        "pane.move",
        json!({
            "pane_id": terminal,
            "focus": false,
            "destination": { "type": "new_tab", "workspace_id": workspace },
        }),
    )?;
    let out = &out["move_result"];
    if out["changed"] != true {
        return Err(match out["reason"].as_str() {
            Some(reason) => format!("herdr didn't move pane {terminal} out of tab {tab}: {reason}"),
            None => UNREADABLE.into(),
        });
    }
    let pane = text(&out["pane"], "pane_id")?;
    let own = text(&out["pane"], "tab_id")?;
    // no way back: left in its tab, the next placing moves it
    let pane = put_beside(&herdr, pane, &beside)
        .map_err(|why| format!("{why}: pane {pane} is left in tab {own}"))?;
    size_it(&pane)
}

/// Sizes the terminal, or says why it can't.
pub(super) fn resize(given: &Given) -> Result<(), String> {
    let terminal = input(given, "terminal")?;
    let origin = input(given, "origin")?;
    let side = Side::of(given)?;
    let size = size(given)?;
    let herdr = Herdr::of(given, MOVE_TIME)?;
    size_against(&herdr, &terminal, &origin, side.across(), size)
}

/// Gives `terminal` `size` cells of content in the lowest split holding
/// it and `origin`, side by side if `across`, else stacked; or says why
/// it can't.
fn size_against(
    herdr: &Herdr,
    terminal: &str,
    origin: &str,
    across: bool,
    size: u16,
) -> Result<(), String> {
    let layout = herdr.call("pane.layout", json!({ "pane_id": terminal }))?;
    let layout = &layout["layout"];
    let tab = text(layout, "tab_id")?;
    let root = tree(layout)?;
    let (path, ratio) = plan(&root, terminal, origin, across, size)?;
    herdr.call(
        "layout.set_split_ratio",
        json!({ "tab_id": tab, "path": path, "ratio": ratio }),
    )?;
    Ok(())
}

/// The path of the lowest split of `root` holding `terminal` and `origin`,
/// which must be split side by side if `across`, else stacked, and the
/// ratio that gives `terminal`'s part of it `size` cells of content; or
/// why there's none.
fn plan(
    root: &Node,
    terminal: &str,
    origin: &str,
    across: bool,
    size: u16,
) -> Result<(Vec<bool>, f64), String> {
    let Some(to_origin) = root.path_to(origin) else {
        return Err(format!(
            "herdr panes {terminal} and {origin} aren't in one tab"
        ));
    };
    let to_terminal = root.path_to(terminal).ok_or(UNREADABLE)?;
    let depth = to_terminal
        .iter()
        .zip(&to_origin)
        .take_while(|(a, b)| a == b)
        .count();
    let Some(Node::Split(split)) = root.at(&to_terminal[..depth]) else {
        return Err(format!("herdr pane {terminal} is {origin}"));
    };
    if split.across != across {
        let way = if across { "side by side" } else { "stacked" };
        return Err(format!("herdr panes {terminal} and {origin} aren't {way}"));
    }
    let terminal_second = to_terminal[depth];
    let part = if terminal_second {
        &split.second
    } else {
        &split.first
    };
    let total = split.cells;
    if total < 2 {
        return Err(format!(
            "herdr panes {terminal} and {origin} have no room to share"
        ));
    }
    let wanted = size.saturating_add(FRAME.saturating_mul(part.panes_along(across)));
    let wanted = wanted.clamp(1, total - 1);
    let first = if terminal_second {
        total - wanted
    } else {
        wanted
    };
    Ok((split.path.clone(), f64::from(first) / f64::from(total)))
}

/// `SIZE`, cells, at least 1.
fn size(given: &Given) -> Result<u16, String> {
    let size = given.var("size").unwrap_or_default();
    std::str::from_utf8(&size)
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|&n| n > 0)
        .ok_or_else(|| format!("SIZE {:?} isn't cells", String::from_utf8_lossy(&size)))
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Fake, Reply, given, recorded};
    use super::*;
    use serde_json::Value;

    /// `resize` of `terminal` beside `origin`, `direction`, `size`, on
    /// herdr 0.9.5's tab `(P1 / (P4 | P5)) | (P2 / P3)`, 74x29: its
    /// `set_split_ratio`'s params, or why it failed, and what it asked.
    fn resized(
        terminal: &str,
        origin: &str,
        direction: &str,
        size: &str,
    ) -> (Result<Value, String>, Vec<String>) {
        let fake = Fake::serve(vec![
            ("pane.layout", Reply::Line(recorded("resize-layout"))),
            (
                "layout.set_split_ratio",
                Reply::Line(recorded("resize-set-ratio")),
            ),
        ]);
        let vars = [
            ("TERMINAL", terminal),
            ("ORIGIN", origin),
            ("DIRECTION", direction),
            ("SIZE", size),
            ("RATIO", "0.5"),
        ];
        let done = resize(&given(&fake, &vars));
        let asked = fake.asked();
        let set = asked
            .iter()
            .find(|r| r["method"] == "layout.set_split_ratio");
        // the ratio to a billionth: JSON's round trip may change its last bit
        let done = done.map(|()| {
            let mut params = set.unwrap()["params"].clone();
            params["ratio"] = (params["ratio"].as_f64().unwrap() * 1e9).round().into();
            params
        });
        (done, fake.methods())
    }

    /// The `set_split_ratio` params for `path` at `cells` of `total`.
    fn set(path: &[bool], cells: u16, total: u16) -> Result<Value, String> {
        Ok(json!({
            "tab_id": "w1:t1",
            "path": path,
            "ratio": (f64::from(cells) / f64::from(total) * 1e9).round(),
        }))
    }

    #[test]
    fn the_lowest_split_holding_both_gets_the_terminal_its_cells_and_its_frame() {
        // P5, right of P4 in their 44 cells: 20 and a frame of 2
        let (done, asked) = resized("w1:p5", "w1:p4", "right", "20");
        assert_eq!(done, set(&[false, true], 22, 44));
        assert_eq!(asked, ["pane.layout", "layout.set_split_ratio"]);
        // P4, left of P5: the first part
        assert_eq!(
            resized("w1:p4", "w1:p5", "left", "20").0,
            set(&[false, true], 22, 44)
        );
        // P3, below P2 in 29 rows
        assert_eq!(
            resized("w1:p3", "w1:p2", "down", "10").0,
            set(&[true], 17, 29)
        );
    }

    #[test]
    fn across_a_part_of_several_panes_each_has_its_frame() {
        // P2 / P3, right of the rest: one pane across, 28 and 2
        assert_eq!(resized("w1:p2", "w1:p1", "right", "28").0, set(&[], 44, 74));
        // P1 / (P4 | P5), left of P2: two panes across, 40 and 4
        assert_eq!(resized("w1:p1", "w1:p2", "left", "40").0, set(&[], 44, 74));
        // stacked, P1 over P4 | P5 is one pane down each
        assert_eq!(
            resized("w1:p4", "w1:p1", "down", "10").0,
            set(&[false], 17, 29)
        );
    }

    #[test]
    fn a_size_past_the_room_is_held_inside_it() {
        assert_eq!(
            resized("w1:p5", "w1:p4", "right", "500").0,
            set(&[false, true], 1, 44)
        );
        assert_eq!(
            resized("w1:p4", "w1:p5", "right", "500").0,
            set(&[false, true], 43, 44)
        );
    }

    #[test]
    fn panes_not_split_that_way_or_not_both_there_resize_nothing() {
        for (terminal, origin, direction, why) in [
            (
                "w1:p3",
                "w1:p2",
                "right",
                "herdr panes w1:p3 and w1:p2 aren't side by side",
            ),
            (
                "w1:p5",
                "w1:p4",
                "up",
                "herdr panes w1:p5 and w1:p4 aren't stacked",
            ),
            (
                "w1:p5",
                "w1:p9",
                "left",
                "herdr panes w1:p5 and w1:p9 aren't in one tab",
            ),
            ("w1:p5", "w1:p5", "left", "herdr pane w1:p5 is w1:p5"),
        ] {
            let (done, asked) = resized(terminal, origin, direction, "10");
            assert_eq!(done, Err(why.into()));
            assert_eq!(asked, ["pane.layout"], "{why}");
        }
    }

    #[test]
    fn a_split_id_that_cant_be_read_resizes_nothing() {
        let mut layout: Value = serde_json::from_str(&recorded("resize-layout")).unwrap();
        layout["result"]["layout"]["splits"][1]["id"] = "split_1_first".into();
        let fake = Fake::serve(vec![("pane.layout", Reply::Line(layout.to_string()))]);
        let vars = [
            ("TERMINAL", "w1:p5"),
            ("ORIGIN", "w1:p4"),
            ("DIRECTION", "right"),
            ("SIZE", "20"),
        ];
        assert_eq!(
            resize(&given(&fake, &vars)),
            Err("herdr's split id \"split_1_first\" can't be read".into())
        );
        assert_eq!(fake.methods(), ["pane.layout"]);
    }

    /// herdr 0.9.5 as it answered `place_beside` of w1:p3 left of w1:p1,
    /// both in tab w1:t1, w1:p3 having the focus: out to tab w1:t2 and
    /// back, then swapped; `snapshot` its session first.
    fn placing(snapshot: Reply, back: &str) -> Fake {
        Fake::serve(vec![
            ("pane.layout", laid_out("place-swap", "swap")),
            ("session.snapshot", snapshot),
            ("pane.move", Reply::Line(recorded("place-move-out"))),
            ("pane.move", Reply::Line(recorded(back))),
            ("pane.swap", Reply::Line(recorded("place-swap"))),
            ("pane.focus", Reply::Line(recorded("open-focus"))),
            (
                "layout.set_split_ratio",
                Reply::Line(recorded("place-set-ratio")),
            ),
        ])
    }

    /// `pane.layout`'s reply, as herdr gives it, with the layout the
    /// recorded reply `name` gives under `key`.
    fn laid_out(name: &str, key: &str) -> Reply {
        let reply: Value = serde_json::from_str(&recorded(name)).unwrap();
        let mut layout = reply["result"][key].clone();
        if key == "move_result" {
            layout = layout["target_layout"].clone();
        } else {
            layout = layout["layout"].clone();
        }
        let reply = json!({ "id": "x", "result": { "type": "pane_layout", "layout": layout } });
        Reply::Line(reply.to_string())
    }

    /// The recorded snapshot of `placing`, the server's focus on `focus`.
    fn focused_on(focus: &str) -> Reply {
        let mut reply: Value = serde_json::from_str(&recorded("place-snapshot")).unwrap();
        reply["result"]["snapshot"]["focused_pane_id"] = focus.into();
        Reply::Line(reply.to_string())
    }

    /// `place_beside` of `terminal` beside `origin`, on side `direction`,
    /// its `RATIO` 0.3.
    fn place(fake: &Fake, terminal: &str, origin: &str, direction: &str) -> Result<(), String> {
        let vars = [
            ("TERMINAL", terminal),
            ("ORIGIN", origin),
            ("DIRECTION", direction),
            ("SIZE", "12"),
            ("RATIO", "0.3"),
        ];
        place_beside(&given(fake, &vars))
    }

    fn close_to(a: &Value, b: f64) -> bool {
        a.as_f64().is_some_and(|a| (a - b).abs() < 1e-6)
    }

    #[test]
    fn within_its_tab_the_terminal_goes_out_to_a_tab_of_its_own_and_back_beside() {
        let fake = placing(Reply::Line(recorded("place-snapshot")), "place-move-back");
        assert_eq!(place(&fake, "w1:p3", "w1:p1", "left"), Ok(()));
        assert_eq!(
            fake.methods(),
            [
                "session.snapshot",
                "pane.move",
                "pane.move",
                "pane.swap",
                "layout.set_split_ratio",
                "layout.set_split_ratio",
                "pane.layout",
                "layout.set_split_ratio"
            ]
        );
        let asked = fake.asked();
        // out, to a new tab of its workspace, the server's focus left
        assert_eq!(
            asked[1]["params"],
            json!({
                "pane_id": "w1:p3",
                "focus": false,
                "destination": { "type": "new_tab", "workspace_id": "w1" },
            })
        );
        // back, right of the origin, its RATIO the first pane's share
        // after the swap; focused, as it had the focus
        let back = &asked[2]["params"];
        assert_eq!(
            (&back["pane_id"], &back["focus"]),
            (&json!("w1:p3"), &json!(true))
        );
        let to = &back["destination"];
        assert_eq!(
            (
                &to["type"],
                &to["tab_id"],
                &to["target_pane_id"],
                &to["split"]
            ),
            (
                &json!("tab"),
                &json!("w1:t1"),
                &json!("w1:p1"),
                &json!("right")
            )
        );
        assert!(close_to(&to["ratio"], 0.3), "{to}");
        // the swap's source, which it focuses, is the pane that had the
        // focus
        assert_eq!(
            asked[3]["params"],
            json!({ "source_pane_id": "w1:p3", "target_pane_id": "w1:p1" })
        );
        // the root split, at the ratio the swap left it
        let set = &asked[4]["params"];
        assert_eq!(
            (&set["tab_id"], &set["path"]),
            (&json!("w1:t1"), &json!([]))
        );
        assert!(close_to(&set["ratio"], 0.6), "{set}");
        // then its SIZE, 12 columns and its frame, of the 44 it shares
        // with the origin
        assert_eq!(asked[6]["params"], json!({ "pane_id": "w1:p3" }));
        let set = &asked[7]["params"];
        assert_eq!(
            (&set["tab_id"], &set["path"]),
            (&json!("w1:t1"), &json!([false, false]))
        );
        assert!(close_to(&set["ratio"], 14.0 / 44.0), "{set}");
    }

    #[test]
    fn the_focus_stays_where_it_was() {
        // on another pane: the move leaves it, the swap's source is the
        // origin, and the focus is put back
        let fake = placing(focused_on("w1:p2"), "place-move-back");
        assert_eq!(place(&fake, "w1:p3", "w1:p1", "left"), Ok(()));
        let asked = fake.asked();
        assert_eq!(asked[2]["params"]["focus"], false);
        assert_eq!(
            asked[3]["params"],
            json!({ "source_pane_id": "w1:p1", "target_pane_id": "w1:p3" })
        );
        assert_eq!(asked[4]["method"], "pane.focus");
        assert_eq!(asked[4]["params"], json!({ "pane_id": "w1:p2" }));
        // on the origin: the swap leaves it there
        let fake = placing(focused_on("w1:p1"), "place-move-back");
        assert_eq!(place(&fake, "w1:p3", "w1:p1", "left"), Ok(()));
        assert!(!fake.methods().contains(&"pane.focus".to_string()));
    }

    #[test]
    fn right_or_down_needs_no_swap_and_the_origin_keeps_the_rest() {
        let fake = placing(Reply::Line(recorded("place-snapshot")), "place-move-back");
        assert_eq!(place(&fake, "w1:p3", "w1:p1", "right"), Ok(()));
        assert_eq!(
            fake.methods(),
            [
                "session.snapshot",
                "pane.move",
                "pane.move",
                "layout.set_split_ratio",
                "layout.set_split_ratio",
                "pane.layout",
                "layout.set_split_ratio"
            ]
        );
        let to = &fake.asked()[2]["params"]["destination"];
        assert_eq!(to["split"], "right");
        assert!(close_to(&to["ratio"], 0.7), "{to}");
    }

    #[test]
    fn from_another_tab_it_takes_one_move() {
        // w1:p6, alone in tab w1:t3, below w1:p2 in w1:t1
        let fake = Fake::serve(vec![
            (
                "session.snapshot",
                Reply::Line(recorded("place-snapshot-across")),
            ),
            ("pane.move", Reply::Line(recorded("place-move-across"))),
            (
                "layout.set_split_ratio",
                Reply::Line(recorded("place-set-ratio-across")),
            ),
            ("pane.layout", laid_out("place-move-across", "move_result")),
        ]);
        assert_eq!(place(&fake, "w1:p6", "w1:p2", "down"), Ok(()));
        assert_eq!(
            fake.methods(),
            [
                "session.snapshot",
                "pane.move",
                "layout.set_split_ratio",
                "layout.set_split_ratio",
                "pane.layout",
                "layout.set_split_ratio"
            ]
        );
        // sized: 12 rows and its frame, of the 29 below and beside w1:p2
        let set = &fake.asked()[5]["params"];
        assert_eq!(set["path"], json!([true]));
        assert!(close_to(&set["ratio"], 15.0 / 29.0), "{set}");
        let moved = &fake.asked()[1]["params"];
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
                &json!("w1:t1"),
                &json!("w1:p2"),
                &json!("down")
            )
        );
    }

    #[test]
    fn a_zoomed_tab_either_side_is_refused_before_anything_moves() {
        // w2:t1 is zoomed, w2:p1 and w2:p3 in it; w1:t1 isn't
        for (terminal, origin) in [("w2:p3", "w2:p1"), ("w2:p3", "w1:p1"), ("w1:p1", "w2:p3")] {
            let fake = Fake::serve(vec![(
                "session.snapshot",
                Reply::Line(recorded("session-snapshot-zoomed")),
            )]);
            assert_eq!(
                place(&fake, terminal, origin, "left"),
                Err("herdr tab w2:t1 is zoomed".into()),
                "{terminal} beside {origin}"
            );
            assert_eq!(fake.methods(), ["session.snapshot"]);
        }
    }

    #[test]
    fn a_terminal_in_another_workspace_isnt_moved() {
        // a move across workspaces gives the pane a new id
        let mut snapshot: Value =
            serde_json::from_str(&recorded("session-snapshot-zoomed")).unwrap();
        snapshot["result"]["snapshot"]["layouts"][1]["zoomed"] = false.into();
        let fake = Fake::serve(vec![(
            "session.snapshot",
            Reply::Line(snapshot.to_string()),
        )]);
        assert_eq!(
            place(&fake, "w2:p3", "w1:p1", "left"),
            Err("herdr pane w2:p3 is in workspace w2, not w1 with w1:p1: \
                 moved there, it would get another id"
                .into())
        );
        assert_eq!(fake.methods(), ["session.snapshot"]);
    }

    #[test]
    fn a_second_move_that_fails_leaves_the_terminal_in_its_tab_of_its_own() {
        // the origin's tab zoomed between the two moves: no going back
        let fake = placing(Reply::Line(recorded("place-snapshot")), "pane-move-zoomed");
        assert_eq!(
            place(&fake, "w1:p3", "w1:p1", "left"),
            Err("herdr tab w1:t1 is zoomed: pane w1:p3 is left in tab w1:t2".into())
        );
        assert_eq!(
            fake.methods(),
            ["session.snapshot", "pane.move", "pane.move"]
        );
        // a first move that fails moved nothing
        let fake = Fake::serve(vec![
            ("session.snapshot", Reply::Line(recorded("place-snapshot"))),
            ("pane.move", Reply::Line(recorded("pane-not-found"))),
        ]);
        assert_eq!(
            place(&fake, "w1:p3", "w1:p1", "left"),
            Err("herdr: pane not found".into())
        );
        assert_eq!(fake.methods(), ["session.snapshot", "pane.move"]);
    }

    #[test]
    fn place_beside_with_bad_inputs_asks_herdr_nothing() {
        let fake = Fake::serve(vec![]);
        for (vars, why) in [
            (
                vec![("ORIGIN", "w1:p1"), ("DIRECTION", "left")],
                "TERMINAL isn't set",
            ),
            (
                vec![
                    ("TERMINAL", "w1:p3"),
                    ("ORIGIN", "w1:p1"),
                    ("DIRECTION", "in"),
                ],
                "DIRECTION \"in\" isn't left, right, up or down",
            ),
            (
                vec![
                    ("TERMINAL", "w1:p3"),
                    ("ORIGIN", "w1:p1"),
                    ("DIRECTION", "left"),
                    ("RATIO", "1"),
                ],
                "RATIO \"1\" isn't between 0 and 1",
            ),
            (
                vec![
                    ("TERMINAL", "w1:p3"),
                    ("ORIGIN", "w1:p3"),
                    ("DIRECTION", "left"),
                ],
                "herdr pane w1:p3 is w1:p3",
            ),
        ] {
            assert_eq!(place_beside(&given(&fake, &vars)), Err(why.into()));
        }
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn bad_inputs_ask_herdr_nothing() {
        let fake = Fake::serve(vec![]);
        let base = [
            ("TERMINAL", "w1:p5"),
            ("ORIGIN", "w1:p4"),
            ("DIRECTION", "right"),
        ];
        for (more, why) in [
            (vec![], "SIZE \"\" isn't cells"),
            (vec![("SIZE", "0")], "SIZE \"0\" isn't cells"),
            (vec![("SIZE", "1.5")], "SIZE \"1.5\" isn't cells"),
            (
                vec![("SIZE", "9"), ("DIRECTION", "over")],
                "DIRECTION \"over\" isn't left, right, up or down",
            ),
        ] {
            let mut vars: Vec<(&str, &str)> = base.to_vec();
            for (k, v) in more {
                vars.retain(|(n, _)| *n != k);
                vars.push((k, v));
            }
            assert_eq!(resize(&given(&fake, &vars)), Err(why.into()));
        }
        let no_origin = [("TERMINAL", "w1:p5"), ("DIRECTION", "right"), ("SIZE", "9")];
        assert_eq!(
            resize(&given(&fake, &no_origin)),
            Err("ORIGIN isn't set".into())
        );
        assert!(fake.asked().is_empty());
    }
}
