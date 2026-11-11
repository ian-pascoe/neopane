//! Rex's pane provider over a `UnixStream` pair: the asks as the JSON Rex
//! reads, and Rex's answers as outcomes.

use std::io::Write;
use std::os::unix::net::UnixStream;

use serde_json::{Value, json};

use super::*;
use crate::attach::PaneId;
use crate::daemon::provider::Direction;
use crate::rex::session::tests::{sent, server_frame, session};

/// Rex's pane provider on a socket, and Rex's end of it.
fn rex() -> (RexProvider, UnixStream) {
    let (s, theirs) = session();
    (RexProvider::new(s), theirs)
}

fn pane() -> PaneId {
    PaneId::parse("3-abcdefghij").unwrap()
}

fn attach() -> Vec<String> {
    vec!["exe".into(), "attach".into()]
}

fn open_beside() -> Ask {
    Ask::OpenBeside {
        pane: pane(),
        attach: attach(),
        title: String::new(),
        origin: TerminalId::from("block:o"),
        dir: Direction::Left,
        size: 40,
        ratio: 0.5,
        cols: 40,
        rows: 24,
    }
}

/// Rex answers request `id` with `payload`, and the provider reads it.
fn reply(p: &mut RexProvider, theirs: &mut UnixStream, id: u64, payload: Value) {
    let text = json!({ "type": "response", "id": id.to_string(), "seq": 0, "payload": payload });
    theirs.write_all(&server_frame(&text.to_string())).unwrap();
    p.on_ready(0);
}

/// Rex refuses request `id` with `code`.
fn refuse(p: &mut RexProvider, theirs: &mut UnixStream, id: u64, code: &str) {
    let text = json!({ "type": "response", "id": id.to_string(), "seq": 0,
                       "error": { "code": code, "message": "no", "status": 400 } });
    theirs.write_all(&server_frame(&text.to_string())).unwrap();
    p.on_ready(0);
}

fn events(p: &mut RexProvider) -> Vec<ProviderEvent> {
    std::iter::from_fn(|| p.next()).collect()
}

fn answered(id: u64, outcome: Outcome) -> ProviderEvent {
    ProviderEvent::Answered {
        id: AskId(id),
        outcome,
    }
}

/// The method and payload of each request Rex read, once `p` is dropped.
fn requests(p: RexProvider, theirs: UnixStream) -> Vec<(String, Value)> {
    drop(p);
    sent(theirs)
        .iter()
        .map(|t| {
            let v: Value = serde_json::from_str(t).unwrap();
            (
                v["method"].as_str().unwrap().to_string(),
                v["payload"].clone(),
            )
        })
        .collect()
}

#[test]
fn the_opens_become_rex_requests_with_no_focus_key() {
    let (mut p, theirs) = rex();
    p.ask(AskId(1), open_beside());
    // asked with the origin's Rex window, 200 x 50
    p.ask(
        AskId(2),
        Ask::OpenTabPage {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            origin: TerminalId::from("block:o"),
            cols: 200,
            rows: 50,
        },
    );
    p.ask(
        AskId(3),
        Ask::OpenFloat {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            origin: TerminalId::from("block:o"),
            x: 11,
            y: 6,
            cols: 37,
            rows: 18,
            origin_cols: 100,
            origin_rows: 40,
            area_bounds: None,
        },
    );
    let reqs = requests(p, theirs);
    let block = |size: Value| {
        let mut b = json!({
            "flavor": "com.superlogical.terminal.shell",
            "label": NAME,
            "options": { "shell": "none", "command": ["exe", "attach"] },
        });
        if !size.is_null() {
            b["options"]["size"] = size;
        }
        b
    };
    assert_eq!(
        vec![
            (
                "session.new_split".to_string(),
                json!({
                    "session_id": "session:s",
                    "anchor_id": "block:o",
                    "direction": "horizontal",
                    "side": "before",
                    "layout": { "block": block(json!({ "columns": 40, "rows": 24 })) },
                })
            ),
            (
                "session.new_layer".to_string(),
                json!({
                    "session_id": "session:s",
                    "bounds": [0.05, 0.05, 0.9, 0.9],
                    // 0.9 of it, less the frame
                    "layout": { "block": block(json!({ "columns": 177, "rows": 43 })) },
                })
            ),
            (
                "session.new_layer".to_string(),
                json!({
                    "session_id": "session:s",
                    // the group with a frame of 3 x 2 cells around it, as
                    // fractions of 100 x 40
                    "bounds": [0.1, 0.125, 0.4, 0.5],
                    "layout": { "block": block(Value::Null) },
                })
            ),
        ],
        reqs
    );
    assert!(reqs.iter().all(|(_, v)| !v.to_string().contains("focus")));
}

#[test]
fn a_floats_layer_is_at_its_area_bounds_frame_included() {
    let (mut p, theirs) = rex();
    // a picker 10 cells into a pane that is the right half of a 200 x 50 window
    p.ask(
        AskId(1),
        open_float_at(
            "block:o",
            [10, 4],
            [60, 20],
            [100, 50],
            Some([110.0 / 200.0, 4.0 / 50.0, 60.0 / 200.0, 20.0 / 50.0]),
        ),
    );
    let reqs = requests(p, theirs);
    let bounds: Vec<f64> = reqs[0].1["bounds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    // one cell left of and above the group, 3 x 2 cells bigger
    let want = [109.0 / 200.0, 3.0 / 50.0, 63.0 / 200.0, 22.0 / 50.0];
    for (w, b) in want.iter().zip(&bounds) {
        assert!((w - b).abs() < 1e-9, "{want:?} != {bounds:?}");
    }
}

#[test]
fn an_open_is_done_only_once_its_block_is_disowned() {
    let (mut p, mut theirs) = rex();
    p.ask(AskId(7), open_beside());
    reply(&mut p, &mut theirs, 1, json!({ "block_ids": ["block:n"] }));
    assert_eq!(Vec::<ProviderEvent>::new(), events(&mut p));

    reply(&mut p, &mut theirs, 2, json!({}));
    assert_eq!(
        vec![answered(
            7,
            Outcome::Done(Answer::Terminal(TerminalId::from("block:n")))
        )],
        events(&mut p)
    );
    let reqs = requests(p, theirs);
    assert_eq!("session.disown_block", reqs[1].0);
    assert_eq!(json!(["block:n"]), reqs[1].1["block_ids"]);
}

#[test]
fn close_is_always_block_close() {
    let (mut p, mut theirs) = rex();
    p.ask(
        AskId(4),
        Ask::Close {
            terminal: TerminalId::from("block:f"),
        },
    );
    reply(&mut p, &mut theirs, 1, json!({}));
    assert_eq!(
        vec![answered(4, Outcome::Done(Answer::Nothing))],
        events(&mut p)
    );
    assert_eq!(
        vec![(
            "block.close".to_string(),
            json!({ "session_id": "session:s", "block_id": "block:f" })
        )],
        requests(p, theirs)
    );
}

#[test]
fn a_layer_made_with_no_block_is_closed_and_its_open_fails() {
    let (mut p, mut theirs) = rex();
    p.ask(
        AskId(5),
        Ask::OpenTabPage {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            origin: TerminalId::from("block:o"),
            cols: 72,
            rows: 21,
        },
    );
    reply(&mut p, &mut theirs, 1, json!({ "layer_id": "layer:l" }));
    assert_eq!(Vec::<ProviderEvent>::new(), events(&mut p));
    reply(&mut p, &mut theirs, 2, json!({}));
    assert_eq!(vec![answered(5, no_block())], events(&mut p));
    let reqs = requests(p, theirs);
    assert_eq!("session.close_layer", reqs[1].0);
    assert_eq!(json!("layer:l"), reqs[1].1["layer_id"]);
}

#[test]
fn method_not_found_is_unsupported_and_any_other_error_fails() {
    let (mut p, mut theirs) = rex();
    p.ask(AskId(1), open_beside());
    p.ask(AskId(2), open_beside());
    refuse(&mut p, &mut theirs, 1, "method_not_found");
    refuse(&mut p, &mut theirs, 2, "invalid_anchor");
    assert_eq!(
        vec![
            answered(1, Outcome::Unsupported),
            answered(2, Outcome::Failed("invalid_anchor: no".into())),
        ],
        events(&mut p)
    );
}

#[test]
fn place_beside_and_resize_are_move_block_after_and_resize_pane_with_the_anchors_share() {
    let (mut p, mut theirs) = rex();
    assert!(p.capabilities().has(Capability::PlaceBeside));
    assert!(p.capabilities().has(Capability::Resize));
    // the placed terminal's share is 0.25: the anchor's, which Rex takes, is 0.75
    p.ask(
        AskId(1),
        Ask::PlaceBeside {
            terminal: TerminalId::from("block:p"),
            origin: TerminalId::from("block:a"),
            dir: Direction::Right,
            size: 20,
            ratio: 0.25,
        },
    );
    p.ask(
        AskId(2),
        Ask::Resize {
            terminal: TerminalId::from("block:p"),
            origin: TerminalId::from("block:a"),
            dir: Direction::Down,
            size: 10,
            ratio: 0.5,
        },
    );
    reply(&mut p, &mut theirs, 1, json!({}));
    refuse(&mut p, &mut theirs, 2, "invalid_anchor");
    assert_eq!(
        vec![
            answered(1, Outcome::Done(Answer::Nothing)),
            answered(2, Outcome::Failed("invalid_anchor: no".into())),
        ],
        events(&mut p)
    );
    assert_eq!(
        vec![
            (
                "session.move_block".to_string(),
                json!({
                    "session_id": "session:s",
                    "block_id": "block:p",
                    "anchor_block_id": "block:a",
                    "direction": "horizontal",
                    "side": "after",
                    "ratio": 0.75,
                })
            ),
            (
                // `{origin}` before the split, `{terminal}` after it
                "session.resize_pane".to_string(),
                json!({
                    "session_id": "session:s",
                    "before_block_id": "block:a",
                    "after_block_id": "block:p",
                    "direction": "vertical",
                    "ratio": 0.5,
                })
            ),
        ],
        requests(p, theirs)
    );
}

#[test]
fn what_rex_has_no_request_for_is_unsupported() {
    let (mut p, _theirs) = rex();
    p.ask(
        AskId(9),
        Ask::Focus {
            terminal: TerminalId::from("block:o"),
        },
    );
    assert_eq!(vec![answered(9, Outcome::Unsupported)], events(&mut p));
}

fn window_id(n: u32) -> String {
    format!("window:00000000-0000-0000-0000-{n:0>12}")
}

/// A Rex block id, as Rex's views name blocks.
fn block_id(n: u32) -> String {
    format!("block:00000000-0000-0000-0000-{n:0>12}")
}

/// Rex's view changes to `windows`, at revision `rev`, its view fetch
/// being request `id`.
fn view_changes(p: &mut RexProvider, theirs: &mut UnixStream, id: u64, rev: i64, windows: Value) {
    let changed = json!({ "type": "event", "seq": 0, "payload": {
        "type": "session_view_changed", "payload": { "revision": rev } } });
    theirs
        .write_all(&server_frame(&changed.to_string()))
        .unwrap();
    p.on_ready(0);
    let view = json!({ "revision": rev, "active_window_id": window_id(2),
        "windows": windows });
    reply(p, theirs, id, view);
}

#[test]
fn each_view_is_told_as_a_live_layout_and_report_layout_answers_the_last() {
    let (mut p, mut theirs) = rex();
    assert!(p.capabilities().has(Capability::ReportLayout));
    assert!(p.capabilities().has(Capability::Events));
    // no view yet: nothing to report
    p.ask(AskId(1), Ask::ReportLayout);
    assert_eq!(
        vec![answered(1, Outcome::Failed("no view from Rex yet".into()))],
        events(&mut p)
    );

    let (a, b) = (block_id(1), block_id(2));
    let windows = json!([
        { "window_id": window_id(1), "focused_block_id": a, "layers": [
            { "kind": "tiled", "layout": { "block_id": a, "rect": [0, 0, 1, 1] } } ] },
        { "window_id": window_id(2), "focused_block_id": b, "layers": [
            { "kind": "tiled", "layout": { "block_id": b, "rect": [0, 0, 1, 1] } } ] },
    ]);
    view_changes(&mut p, &mut theirs, 1, 1, windows);
    let told = events(&mut p);
    let [ProviderEvent::Layout(l)] = &told[..] else {
        panic!("{told:?}");
    };
    assert_eq!(
        vec![window_id(1), window_id(2)],
        l.areas.iter().map(|a| a.id.clone()).collect::<Vec<_>>()
    );
    // the active window's focused block
    assert_eq!(Some(TerminalId::from(b.as_str())), l.current);

    p.ask(AskId(2), Ask::ReportLayout);
    assert_eq!(
        vec![answered(2, Outcome::Done(Answer::Layout(l.clone())))],
        events(&mut p)
    );
}

fn open_float(origin: &str) -> Ask {
    open_float_at(origin, [11, 6], [37, 18], [100, 40], None)
}

/// An `open_float` from `origin` of a group `size` cells at `at` from its
/// top left, the origin `origin_size`.
fn open_float_at(
    origin: &str,
    at: [i32; 2],
    size: [u32; 2],
    origin_size: [u32; 2],
    area_bounds: Option<[f64; 4]>,
) -> Ask {
    Ask::OpenFloat {
        pane: pane(),
        attach: attach(),
        title: String::new(),
        origin: TerminalId::from(origin),
        x: at[0],
        y: at[1],
        cols: size[0],
        rows: size[1],
        origin_cols: origin_size[0],
        origin_rows: origin_size[1],
        area_bounds,
    }
}

#[test]
fn a_float_groups_block_is_framed_and_a_layer_opens_in_its_origins_window() {
    let (mut p, mut theirs) = rex();
    p.ask(AskId(1), open_float("block:a"));
    reply(&mut p, &mut theirs, 1, json!({ "block_ids": ["block:f"] }));
    reply(&mut p, &mut theirs, 2, json!({}));
    events(&mut p);
    let bounds = [0.1, 0.125, 0.4, 0.5];
    let windows = json!([
        { "window_id": window_id(1), "layers": [
            { "kind": "tiled", "layout": { "block_id": "block:a", "rect": [0, 0, 1, 1] } } ] },
        { "window_id": window_id(2), "layers": [
            { "kind": "tiled", "layout": { "block_id": "block:b", "rect": [0, 0, 1, 1] } },
            { "kind": "floating", "bounds": bounds, "blocks": [ { "block_id": "block:f" } ],
              "layout": { "block_id": "block:f", "rect": [0, 0, 1, 1] } } ] },
    ]);
    view_changes(&mut p, &mut theirs, 3, 1, windows);
    let told = events(&mut p);
    let [ProviderEvent::Layout(l)] = &told[..] else {
        panic!("{told:?}");
    };
    let f = &l.areas[1].terminals[1];
    assert_eq!(
        ("block:f", bounds, true, layout::FRAME_INSET),
        (f.terminal.as_str(), f.rect, f.floating, f.inset)
    );

    // into the window of the origin: the first, not the active one
    p.ask(AskId(2), open_float("block:a"));
    p.ask(
        AskId(3),
        Ask::OpenTabPage {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            origin: TerminalId::from("block:b"),
            cols: 72,
            rows: 21,
        },
    );
    // an origin the last view didn't have: Rex's active window, not the last
    p.ask(AskId(4), open_float("block:gone"));
    let reqs = requests(p, theirs);
    let layers: Vec<&Value> = reqs
        .iter()
        .filter(|(m, _)| m == "session.new_layer")
        .map(|(_, v)| &v["window_id"])
        .collect();
    assert_eq!(
        vec![
            &Value::Null,
            &json!(window_id(1)),
            &json!(window_id(2)),
            &Value::Null
        ],
        layers
    );
}

/// Checks `p` told `Gone` once, and nothing after.
fn assert_gone(p: &mut RexProvider) {
    let told = events(p);
    assert_eq!(1, told.len(), "{told:?}");
    assert!(matches!(told[0], ProviderEvent::Gone { .. }));
    // nothing more, even when asked
    p.ask(AskId(99), open_beside());
    assert_eq!(None, p.next());
}

#[test]
fn a_malformed_message_from_rex_is_gone() {
    let (mut p, mut theirs) = rex();
    p.ask(AskId(1), open_beside());
    // JSON, but without a `type`
    let body = br#"{"payload":{}}"#;
    let mut frame = vec![0x81, body.len() as u8];
    frame.extend_from_slice(body);
    theirs.write_all(&frame).unwrap();
    p.on_ready(0);
    assert_gone(&mut p);
}

#[test]
fn rex_hanging_up_is_gone() {
    let (mut p, theirs) = rex();
    p.ask(AskId(1), open_beside());
    theirs.shutdown(std::net::Shutdown::Both).unwrap();
    p.on_ready(0);
    assert_gone(&mut p);
}

#[test]
fn a_failed_write_is_gone() {
    let (mut p, theirs) = rex();
    drop(theirs); // writes to a socket nobody holds fail
    p.ask(AskId(1), open_beside());
    assert_gone(&mut p);
}
