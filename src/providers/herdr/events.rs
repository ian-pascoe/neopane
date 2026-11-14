//! `events`: herdr's own events, from one `events.subscribe` held open, a
//! line for the daemon as each comes.
//!
//! - `pane.exited` and `pane.closed` print `{"closed": ID}`; every other
//!   event but a focus event a bare line, so the layout is read again: a
//!   move, a new pane, a layout or a tab closed.
//! - Focus is never read from an event: herdr sends them out of causal
//!   order, a `pane.focused` coming before the `pane.created` of its
//!   pane, two in a row naming the panes in either order. On a focus event
//!   (`pane.focused`, `tab.focused`, `workspace.focused`), and on a close,
//!   which moves the server's focus with no focus event, it reads a fresh
//!   `session.snapshot` on a connection of its own, and adds `"focus": ID`
//!   only when the server's focused pane changed from the last it printed.
//!   A focus event that moved nothing prints nothing.
//! - Its first line is where focus starts, read once subscribed, if
//!   herdr has a focused pane: the daemon follows moves from there.
//! - It ends, exit 1, never 69, when herdr ends the subscription: an
//!   `events_lost` error, herdr closing it, as every connection at a live
//!   handoff, or no herdr to subscribe to. The daemon starts it again,
//!   and reads the layout.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::client::Herdr;
use crate::providers::builtin::{DONE, Given, print_line};

/// The events subscribed to.
pub(super) const SUBSCRIPTIONS: [&str; 9] = [
    "pane.focused",
    "pane.exited",
    "pane.closed",
    "pane.moved",
    "pane.created",
    "layout.updated",
    "tab.focused",
    "tab.closed",
    "workspace.focused",
];

/// What subscribing has, and each read of the server's focus.
const ASK_TIME: Duration = Duration::from_millis(900);

/// Runs `events` on the server `HERDR_SOCKET_PATH` names, until herdr
/// ends it or the daemon stops reading: why it ended.
pub(super) fn events(given: &Given) -> String {
    watch(given, |line| print_line(line.as_bytes()) == DONE)
}

/// Runs `events` with `given`, printing each line with `print`, which says
/// false once the reader is gone: why it ended.
fn watch(given: &Given, mut print: impl FnMut(&str) -> bool) -> String {
    const GONE: &str = "the reader is gone";
    let herdr = match Herdr::of(given, ASK_TIME) {
        Ok(h) => h,
        Err(why) => return why,
    };
    let mut events = match herdr.subscribe(&SUBSCRIPTIONS) {
        Ok(e) => e,
        Err(why) => return why,
    };
    // on a connection of its own, the subscription's being held
    let focused = || -> Result<Option<String>, String> {
        let snapshot = herdr.again(ASK_TIME).call("session.snapshot", json!({}))?;
        Ok(snapshot["snapshot"]["focused_pane_id"]
            .as_str()
            .map(String::from))
    };
    // where focus starts, read once subscribed: what moves it after is in
    // an event
    let mut focus = focused().unwrap_or_else(|why| {
        eprintln!("herdr events: can't read where focus starts: {why}");
        None
    });
    if let Some(pane) = &focus
        && !print(&json!({ "focus": pane }).to_string())
    {
        return GONE.into();
    }
    loop {
        let event = match events.next() {
            Ok(e) => e,
            Err(why) => return why,
        };
        let kind = event["event"].as_str().unwrap_or_default();
        let mut line = Map::new();
        let closes = matches!(kind, "pane_exited" | "pane_closed");
        if closes && let Some(pane) = event["data"]["pane_id"].as_str() {
            line.insert("closed".into(), pane.into());
        }
        let focuses = matches!(kind, "pane_focused" | "tab_focused" | "workspace_focused");
        // a close moves the server's focus with no focus event
        if focuses || closes || kind == "tab_closed" {
            match focused() {
                Ok(now) => {
                    if let Some(pane) = now.as_ref().filter(|n| focus.as_ref() != Some(n)) {
                        line.insert("focus".into(), pane.as_str().into());
                    } else if focuses {
                        // focus didn't move, nor did the layout
                        focus = now;
                        continue;
                    }
                    focus = now;
                }
                Err(why) => {
                    // the report the line asks for tells it; the next read
                    // is printed, whatever it is
                    eprintln!("herdr events: can't read the server's focus: {why}");
                    focus = None;
                }
            }
        }
        let line = if line.is_empty() {
            String::new()
        } else {
            Value::Object(line).to_string()
        };
        if !print(&line) {
            return GONE.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Fake, Reply, given, recorded, recorded_stream};
    use super::*;
    use crate::config::Capability;
    use crate::providers::builtin::FAILED;

    /// herdr 0.9.5's snapshot, its server's focus `focus`.
    fn focused(focus: Option<&str>) -> Reply {
        let mut reply: Value = serde_json::from_str(&recorded("session-snapshot")).unwrap();
        reply["result"]["snapshot"]["focused_pane_id"] = focus.into();
        Reply::Line(reply.to_string())
    }

    /// A fake whose subscription sends `stream`, then ends if `end`, and
    /// whose snapshots' focus is each of `focus` in turn, the last kept.
    fn subscribed(stream: Vec<String>, end: bool, focus: &[Option<&str>]) -> Fake {
        let mut replies = vec![("events.subscribe", Reply::Stream { lines: stream, end })];
        replies.extend(focus.iter().map(|f| ("session.snapshot", focused(*f))));
        Fake::serve(replies)
    }

    /// What `events` printed beside `fake`, and why it ended.
    fn run(fake: &Fake) -> (Vec<String>, String) {
        let mut lines = Vec::new();
        let why = watch(&given(fake, &[]), |l| {
            lines.push(l.to_string());
            true
        });
        (lines, why)
    }

    #[test]
    fn it_subscribes_to_focus_closes_moves_and_layouts() {
        let fake = subscribed(vec![recorded("subscribed")], true, &[None]);
        run(&fake);
        let asked = &fake.asked()[0];
        assert_eq!(asked["method"], "events.subscribe");
        let kinds: Vec<&str> = asked["params"]["subscriptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["type"].as_str().unwrap())
            .collect();
        assert_eq!(kinds, SUBSCRIPTIONS);
    }

    /// herdr 0.9.5's events, as it sent them for a pane made in a tab of
    /// its own and moved beside w1:p1, then w1:p1 focused, then w1:p6 to
    /// its right: both `pane.focused` first, the move before the pane it
    /// moved was created, the tab closed after the layouts.
    #[test]
    fn focus_is_the_servers_as_a_snapshot_reads_it_after_each_focus_event_printed_once() {
        // focus starts on w1:p1; by the first focus event it is on w1:p6,
        // whatever the event says
        let fake = subscribed(
            recorded_stream("events-open-and-focus"),
            true,
            &[Some("w1:p1"), Some("w1:p6")],
        );
        let (lines, why) = run(&fake);
        assert_eq!(
            lines,
            [
                r#"{"focus":"w1:p1"}"#,
                // pane.focused w1:p1 and w1:p6, read as w1:p6, once
                r#"{"focus":"w1:p6"}"#,
                // pane.moved, pane.created, two layout.updated
                "",
                "",
                "",
                "",
                // two tab.focused, nothing new; tab.closed, a line; two
                // workspace.focused, nothing new
                "",
            ]
        );
        assert_eq!(why, "herdr ended the subscription");
        let snapshots = fake
            .methods()
            .iter()
            .filter(|m| *m == "session.snapshot")
            .count();
        // the start, six focus events and the tab closed
        assert_eq!(snapshots, 1 + 6 + 1);
    }

    /// herdr 0.9.5's events as a split focused w1:p5, which was then
    /// closed: focus went back to w1:p1 with no focus event.
    #[test]
    fn a_close_says_closed_and_where_focus_went() {
        let fake = subscribed(
            recorded_stream("events-close-focused"),
            true,
            &[
                Some("w1:p1"),
                Some("w1:p5"),
                Some("w1:p5"),
                Some("w1:p5"),
                Some("w1:p1"),
            ],
        );
        let (lines, _) = run(&fake);
        assert_eq!(
            lines,
            [
                r#"{"focus":"w1:p1"}"#,
                r#"{"focus":"w1:p5"}"#,
                // pane.created, layout.updated
                "",
                "",
                r#"{"closed":"w1:p5","focus":"w1:p1"}"#,
                // layout.updated
                "",
            ]
        );
    }

    #[test]
    fn an_exited_pane_is_closed() {
        let fake = subscribed(
            vec![recorded("subscribed"), recorded("event-pane-exited")],
            true,
            &[Some("w1:p1")],
        );
        let (lines, _) = run(&fake);
        assert_eq!(lines, [r#"{"focus":"w1:p1"}"#, r#"{"closed":"w1:p3"}"#]);
    }

    #[test]
    fn a_focus_herdr_cant_tell_has_the_layout_read_and_the_next_one_printed() {
        let snapshot_fails = Reply::Line(recorded("pane-not-found"));
        let fake = Fake::serve(vec![
            (
                "events.subscribe",
                Reply::Stream {
                    lines: recorded_stream("events-close-focused")[..5].to_vec(),
                    end: true,
                },
            ),
            ("session.snapshot", focused(Some("w1:p5"))),
            ("session.snapshot", snapshot_fails),
            ("session.snapshot", focused(Some("w1:p5"))),
        ]);
        let (lines, _) = run(&fake);
        // the start; pane.focused unread: a bare line; pane.created and
        // layout.updated; tab.focused read again, the same, printed
        assert_eq!(
            lines,
            [r#"{"focus":"w1:p5"}"#, "", "", "", r#"{"focus":"w1:p5"}"#]
        );

        // no focused pane at the start: no first line
        let fake = subscribed(
            recorded_stream("events-close-focused")[..2].to_vec(),
            true,
            &[None, Some("w1:p5")],
        );
        let (lines, _) = run(&fake);
        assert_eq!(lines, [r#"{"focus":"w1:p5"}"#]);
    }

    #[test]
    fn events_lost_ends_it_with_herdrs_message() {
        let fake = subscribed(
            vec![recorded("subscribed"), recorded("events-lost")],
            false,
            &[None],
        );
        let (_, why) = run(&fake);
        assert_eq!(
            why,
            "herdr: event subscription fell behind retained history; \
             resubscribe and resync with session.snapshot"
        );
    }

    #[test]
    fn no_herdr_or_a_refused_subscription_ends_it_and_exits_1_never_69() {
        let fake = Fake::serve(vec![]);
        let (lines, why) = run(&fake);
        assert!(lines.is_empty());
        assert_eq!(why, "herdr: unknown method");
        let gone = crate::providers::builtin::Given::from(|name| {
            (name == "HERDR_SOCKET_PATH").then(|| b"/nowhere/h.sock".to_vec())
        });
        let why = watch(&gone, |_| true);
        assert!(
            why.starts_with("can't reach herdr at /nowhere/h.sock: "),
            "{why}"
        );
        assert_eq!(super::super::run(Capability::Events, &gone), FAILED);
    }

    #[test]
    fn it_ends_once_the_reader_is_gone() {
        let fake = subscribed(vec![recorded("subscribed")], false, &[Some("w1:p1")]);
        let why = watch(&given(&fake, &[]), |_| false);
        assert_eq!(why, "the reader is gone");
    }
}
