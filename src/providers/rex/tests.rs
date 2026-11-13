//! Rex's pane provider over a `UnixStream` pair: the asks as the JSON Rex
//! reads, and Rex's answers as outcomes.

use std::io::{Read, Write};
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
fn dropping_it_closes_its_websocket() {
    let (p, mut theirs) = rex();
    drop(p);
    let mut bytes = Vec::new();
    theirs.read_to_end(&mut bytes).unwrap();
    // a close frame, then the socket closed
    assert_eq!(Some(&0x88), bytes.first(), "{bytes:?}");
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
fn closing_a_block_already_gone_is_done_and_any_other_refusal_fails() {
    let (mut p, mut theirs) = rex();
    let close = |t: &str| Ask::Close {
        terminal: TerminalId::from(t),
    };
    p.ask(AskId(4), close("block:gone"));
    p.ask(AskId(5), close("block:f"));
    refuse(&mut p, &mut theirs, 1, "not_found");
    refuse(&mut p, &mut theirs, 2, "forbidden");
    assert_eq!(
        vec![
            answered(4, Outcome::Done(Answer::Nothing)),
            answered(5, Outcome::Failed("forbidden: no".into())),
        ],
        events(&mut p)
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
fn place_beside_left_or_above_fails_unasked() {
    let (mut p, theirs) = rex();
    for (n, dir) in [(1, Direction::Left), (2, Direction::Up)] {
        p.ask(
            AskId(n),
            Ask::PlaceBeside {
                terminal: TerminalId::from("block:p"),
                origin: TerminalId::from("block:a"),
                dir,
                size: 20,
                ratio: 0.25,
            },
        );
    }
    let why = || Outcome::Failed("Rex places a terminal only right of or below another".into());
    assert_eq!(vec![answered(1, why()), answered(2, why())], events(&mut p));
    assert!(requests(p, theirs).is_empty());
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
        Ask::Open {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            cols: 80,
            rows: 24,
        },
    );
    assert_eq!(vec![answered(9, Outcome::Unsupported)], events(&mut p));
}

fn focus(t: &str) -> Ask {
    Ask::Focus {
        terminal: TerminalId::from(t),
    }
}

#[test]
fn focus_is_focus_block_done_once_rex_answers() {
    let (mut p, mut theirs) = rex();
    assert!(p.capabilities().has(Capability::Focus));
    p.ask(AskId(1), focus(&block_id(1)));
    p.ask(AskId(2), focus(&block_id(2)));
    p.ask(AskId(3), focus("pane:3"));
    reply(&mut p, &mut theirs, 1, json!({ "revision": 4 }));
    refuse(&mut p, &mut theirs, 2, "not_found");
    assert_eq!(
        vec![
            answered(3, Outcome::Failed("pane:3 is no Rex block".into())),
            answered(1, Outcome::Done(Answer::Nothing)),
            answered(2, Outcome::Failed("not_found: no".into())),
        ],
        events(&mut p)
    );
    let reqs = requests(p, theirs);
    assert_eq!(
        (
            "session.focus_block".to_string(),
            json!({ "session_id": "session:s", "block_id": block_id(1) })
        ),
        reqs[0]
    );
    // refused, with none other on its way: where focus really is
    assert_eq!("session.view", reqs[2].0);
}

#[test]
fn a_refused_focus_fetches_the_view_only_with_no_other_on_its_way() {
    let (mut p, mut theirs) = rex();
    p.ask(AskId(1), focus(&block_id(1)));
    p.ask(AskId(2), focus(&block_id(2)));
    refuse(&mut p, &mut theirs, 1, "not_found");
    refuse(&mut p, &mut theirs, 2, "not_found");
    events(&mut p);
    let methods: Vec<String> = requests(p, theirs).into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        vec!["session.focus_block", "session.focus_block", "session.view"],
        methods
    );
}

#[test]
fn focus_direction_is_count_requests_without_wrapping_done_once_the_last_is_answered() {
    let (mut p, mut theirs) = rex();
    assert!(p.capabilities().has(Capability::FocusDirection));
    let down = |count| Ask::FocusDirection {
        dir: Direction::Down,
        count,
    };
    p.ask(AskId(1), down(2));
    reply(&mut p, &mut theirs, 1, json!({}));
    assert_eq!(Vec::<ProviderEvent>::new(), events(&mut p));
    reply(&mut p, &mut theirs, 2, json!({}));
    assert_eq!(
        vec![answered(1, Outcome::Done(Answer::Nothing))],
        events(&mut p)
    );
    p.ask(AskId(2), down(1));
    refuse(&mut p, &mut theirs, 3, "method_not_found");
    assert_eq!(vec![answered(2, Outcome::Unsupported)], events(&mut p));
    let want = (
        "session.focus_direction".to_string(),
        json!({ "session_id": "session:s", "direction": "down", "wrap": false }),
    );
    assert_eq!(vec![want.clone(), want.clone(), want], requests(p, theirs));
}

/// Rex's view changes, at `rev`, to its active window focused on `block`.
/// What `p` tells of it.
fn view_focused(
    p: &mut RexProvider,
    theirs: &mut UnixStream,
    id: u64,
    rev: i64,
    block: &str,
) -> Vec<ProviderEvent> {
    let windows = json!([{ "window_id": window_id(2), "focused_block_id": block, "layers": [
        { "kind": "tiled", "layout": { "block_id": block, "rect": [0, 0, 1, 1] } } ] }]);
    view_changes(p, theirs, id, rev, windows);
    events(p)
}

/// The focus `told` tells: the live layout's `current`, and `Focused`.
fn focus_told(told: &[ProviderEvent]) -> (Option<&str>, Option<&str>) {
    let current = told.iter().find_map(|e| match e {
        ProviderEvent::Layout(l) => Some(l.current.as_ref().map(TerminalId::as_str)),
        _ => None,
    });
    let focused = told.iter().find_map(|e| match e {
        ProviderEvent::Focused(t) => Some(t.as_str()),
        _ => None,
    });
    (current.expect("a layout"), focused)
}

#[test]
fn only_the_last_of_two_focus_requests_settles() {
    let (mut p, mut theirs) = rex();
    let (a, b) = (block_id(1), block_id(2));
    p.ask(AskId(1), focus(&a));
    p.ask(AskId(2), focus(&b));
    reply(&mut p, &mut theirs, 1, json!({ "revision": 5 }));
    events(&mut p);
    // the first one's echo, while the second is still out: no focus told
    let told = view_focused(&mut p, &mut theirs, 3, 6, &a);
    assert_eq!((None, None), focus_told(&told));
    // the second answers: the view after it tells its block
    reply(&mut p, &mut theirs, 2, json!({ "revision": 7 }));
    events(&mut p);
    let told = view_focused(&mut p, &mut theirs, 4, 8, &b);
    assert_eq!((Some(b.as_str()), Some(b.as_str())), focus_told(&told));
}

#[test]
fn a_view_older_than_the_revision_our_focus_landed_at_tells_no_focus() {
    let (mut p, mut theirs) = rex();
    let (a, b) = (block_id(1), block_id(2));
    p.ask(AskId(1), focus(&a));
    reply(&mut p, &mut theirs, 1, json!({ "revision": 10 }));
    events(&mut p);
    // from before our change landed
    let told = view_focused(&mut p, &mut theirs, 2, 9, &b);
    assert_eq!((None, None), focus_told(&told));
    // at the revision it landed at, a different block is told: maybe the user's
    let told = view_focused(&mut p, &mut theirs, 3, 10, &b);
    assert_eq!((Some(b.as_str()), Some(b.as_str())), focus_told(&told));
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
    // then its focus
    let [ProviderEvent::Layout(l), ProviderEvent::Focused(f)] = &told[..] else {
        panic!("{told:?}");
    };
    assert_eq!(b, f.as_str());
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

/// Rex's side: the next request read from `theirs`, as JSON.
fn read_request(theirs: &mut UnixStream) -> Value {
    let frame = crate::rex::client::tests::read_client_frame(theirs);
    serde_json::from_slice(&frame).unwrap()
}

/// Whether Rex has been sent nothing more.
fn nothing_more_sent(theirs: &UnixStream) -> bool {
    theirs.set_nonblocking(true).unwrap();
    let mut byte = [0u8; 1];
    let none =
        matches!((&*theirs).read(&mut byte), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock);
    theirs.set_nonblocking(false).unwrap();
    none
}

/// A `find` for block `b` of a terminal whose device the system won't say.
fn find_block(b: &str) -> Ask {
    Ask::Find {
        pane: pane(),
        pid: 7,
        tty: String::new(),
        device: None,
        title: String::new(),
        env: vec![(b"REX_BLOCK".to_vec(), b.as_bytes().to_vec())],
    }
}

// Connecting takes steps as Rex answers: nothing waits for it, and a `find`
// asked meanwhile goes to Rex only once it has attached us to the session.
#[test]
fn a_find_asked_while_connecting_waits_for_the_session_attach() {
    use crate::rex::client::tests::{answer_hello, answer_upgrade};
    let block = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let mut p = RexProvider::connecting(ours, "session:s").unwrap();
    p.ask(AskId(9), find_block(block));
    // Rex is slow: nothing to tell, and nothing waits
    p.on_ready(0);
    assert!(events(&mut p).is_empty());

    answer_upgrade(&mut theirs);
    p.on_ready(0);
    assert_eq!(
        json!({ "kind": "other", "name": NAME }),
        answer_hello(&mut theirs)["client"]
    );
    p.on_ready(0);
    assert!(events(&mut p).is_empty());
    let attach = read_request(&mut theirs);
    assert_eq!(
        ("session.attach", json!({ "session_id": "session:s" })),
        (
            attach["method"].as_str().unwrap(),
            attach["payload"].clone()
        )
    );
    // the find waits for the attach's answer
    assert!(nothing_more_sent(&theirs));

    // its view is the first layout
    let view = json!({ "revision": 3, "active_window_id": "window:w", "windows": [] });
    reply(
        &mut p,
        &mut theirs,
        1,
        json!({ "client_id": "client:c", "view": view }),
    );
    assert!(matches!(&events(&mut p)[..], [ProviderEvent::Layout(_)]));
    let process = read_request(&mut theirs);
    assert_eq!(
        (
            "com.superlogical.terminal.process",
            json!({ "session_id": "session:s", "block_id": block })
        ),
        (
            process["method"].as_str().unwrap(),
            process["payload"].clone()
        )
    );
    reply(
        &mut p,
        &mut theirs,
        2,
        json!({ "child": { "pid": 1, "user_id": 0, "name": "login" } }),
    );
    assert_eq!(
        vec![answered(9, Outcome::Done(Answer::Terminal(block.into())))],
        events(&mut p)
    );
}

// A Rex session that's gone: Rex refuses the attach, and the pane provider
// is gone, saying why.
#[test]
fn a_refused_session_attach_is_gone() {
    use crate::rex::client::tests::{answer_hello, answer_upgrade};
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    let mut p = RexProvider::connecting(ours, "session:gone").unwrap();
    p.ask(
        AskId(9),
        find_block("block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50"),
    );
    answer_upgrade(&mut theirs);
    p.on_ready(0);
    answer_hello(&mut theirs);
    p.on_ready(0);
    read_request(&mut theirs);
    refuse(&mut p, &mut theirs, 1, "session_not_found");
    let told = events(&mut p);
    assert!(
        matches!(&told[..], [ProviderEvent::Gone { reason }] if reason.contains("session_not_found")),
        "{told:?}"
    );
}

/// A leaf of a Rex view's layout: block `n`.
fn leaf(n: u32) -> Value {
    json!({ "rect": [0, 0, 1, 1], "block_id": block_id(n) })
}

/// A split of a Rex view's layout, side by side, `before` left of `after`.
fn split(before: Value, after: Value) -> Value {
    json!({ "rect": [0, 0, 1, 1], "split": {
        "split_id": "split:x", "direction": "horizontal", "ratio": 0.5,
        "before": before, "after": after } })
}

/// Rex's view changes to one window tiled as `layout`.
fn tiled(p: &mut RexProvider, theirs: &mut UnixStream, id: u64, layout: Value) {
    let windows = json!([{ "window_id": window_id(2), "focused_block_id": block_id(1),
        "layers": [{ "layer_id": "layer:x", "kind": "tiled", "bounds": [0, 0, 1, 1],
                     "layout": layout, "blocks": [] }] }]);
    view_changes(p, theirs, id, 1, windows);
    events(p);
}

/// A `resize` of block `terminal`'s share, `ratio`, beside block `origin`'s,
/// side by side.
fn resize_of(terminal: u32, origin: u32, ratio: f64) -> Ask {
    Ask::Resize {
        terminal: TerminalId::from(block_id(terminal).as_str()),
        origin: TerminalId::from(block_id(origin).as_str()),
        dir: Direction::Right,
        size: 20,
        ratio,
    }
}

/// `session.resize_pane` of the border between blocks `before` and `after`,
/// side by side, `before` getting `ratio`.
fn resize_pane(before: u32, after: u32, ratio: f64) -> (String, Value) {
    (
        "session.resize_pane".to_string(),
        json!({ "session_id": "session:s", "before_block_id": block_id(before),
                "after_block_id": block_id(after), "direction": "horizontal", "ratio": ratio }),
    )
}

#[test]
fn a_resize_of_two_blocks_side_by_side_in_rex_resizes_their_split_whichever_is_first() {
    let (mut p, mut theirs) = rex();
    // 1 | 2, as a pane left where it opened has it
    tiled(&mut p, &mut theirs, 1, split(leaf(1), leaf(2)));
    // in Rex's order: 1 has the rest
    p.ask(AskId(2), resize_of(2, 1, 0.25));
    reply(&mut p, &mut theirs, 2, json!({}));
    // the other way round, as Neovim has them after `<C-w>x`: 1 is the
    // terminal, so it gets its share
    p.ask(AskId(3), resize_of(1, 2, 0.25));
    reply(&mut p, &mut theirs, 3, json!({}));
    let done = || Outcome::Done(Answer::Nothing);
    assert_eq!(
        vec![answered(2, done()), answered(3, done())],
        events(&mut p)
    );
    assert_eq!(
        vec![resize_pane(1, 2, 0.75), resize_pane(1, 2, 0.25)],
        requests(p, theirs)[1..]
    );
}

#[test]
fn a_resize_of_blocks_not_side_by_side_in_rex_is_skipped_quietly() {
    let (mut p, mut theirs) = rex();
    // 1 | (2 | 3), as splits from 3 make it, where Neovim has 1, 2, 3
    tiled(
        &mut p,
        &mut theirs,
        1,
        split(leaf(1), split(leaf(2), leaf(3))),
    );
    p.ask(AskId(2), resize_of(3, 1, 0.3));
    p.ask(AskId(3), resize_of(2, 1, 0.5));
    // no view to tell
    p.ask(AskId(4), resize_of(9, 8, 0.5));
    let done = || Outcome::Done(Answer::Nothing);
    assert_eq!(
        vec![
            answered(2, done()),
            answered(3, done()),
            answered(4, done())
        ],
        events(&mut p)
    );
    // the view's fetch, and nothing else
    assert_eq!(1, requests(p, theirs).len());
}

#[test]
fn a_resize_of_a_block_rex_placed_beside_another_resizes_the_split_it_made() {
    let (mut p, mut theirs) = rex();
    // as a tab page area's panes are placed: (3 | 1) | 2
    for (n, terminal) in [(2, 2), (3, 1)] {
        p.ask(
            AskId(n),
            Ask::PlaceBeside {
                terminal: TerminalId::from(block_id(terminal).as_str()),
                origin: TerminalId::from(block_id(3).as_str()),
                dir: Direction::Right,
                size: 20,
                ratio: 0.5,
            },
        );
        reply(&mut p, &mut theirs, n, json!({}));
    }
    tiled(
        &mut p,
        &mut theirs,
        4,
        split(split(leaf(3), leaf(1)), leaf(2)),
    );
    // 2 and 3 aren't side by side, but the split between them is the one
    // `place_beside` made
    p.ask(AskId(5), resize_of(2, 3, 0.25));
    reply(&mut p, &mut theirs, 5, json!({}));
    let asked = requests(p, theirs);
    assert_eq!(resize_pane(3, 2, 0.75), asked[asked.len() - 1]);
}
