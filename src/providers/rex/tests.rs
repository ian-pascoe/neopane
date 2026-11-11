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
    p.ask(
        AskId(2),
        Ask::OpenTabPage {
            pane: pane(),
            attach: attach(),
            title: String::new(),
            origin: TerminalId::from("block:o"),
            cols: 72,
            rows: 21,
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
                    "layout": { "block": block(json!({ "columns": 72, "rows": 21 })) },
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
fn what_rex_has_no_request_for_is_unsupported() {
    let (mut p, _theirs) = rex();
    p.ask(AskId(9), Ask::ReportLayout);
    assert_eq!(vec![answered(9, Outcome::Unsupported)], events(&mut p));
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
