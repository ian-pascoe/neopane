//! `events`: herdr's own events, from one `events.subscribe` held open, a
//! line for the daemon as each comes.
//!
//! - **Closes:** `pane.exited` and `pane.closed` print `{"closed": ID}`.
//! - **Focus** is never read from an event: herdr sends them out of causal
//!   order, a `pane.focused` coming before the `pane.created` of its pane,
//!   two in a row naming the panes in either order. After a focus event
//!   (`pane.focused`, `tab.focused`, `workspace.focused`), or a close
//!   (`pane.exited`, `pane.closed`, `tab.closed`), which moves the
//!   server's focus with no focus event, it reads a fresh
//!   `session.snapshot` on a connection of its own, and adds
//!   `"focus": ID` only when the server's focused pane isn't the last it
//!   printed. A focus event that adds nothing prints nothing; a read that
//!   fails, a bare line, the report it has the daemon run telling focus.
//! - **The rest** (a move, a new pane, a layout) print a bare line, so the
//!   layout is read again; so does an event of a kind it doesn't know.
//! - **Its first line** is where focus starts, read once subscribed, if
//!   herdr has a focused pane: the daemon follows moves from there.
//! - **It ends**, exit 1, never 69, when herdr ends the subscription: an
//!   `events_lost` error, herdr closing it, as every connection at a live
//!   handoff, a line that isn't JSON, or no herdr to subscribe to. The
//!   daemon starts it again, and reads the layout. A JSON line that is no
//!   event, as a keepalive might be, is skipped.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::client::Herdr;
use crate::providers::builtin::{DONE, Given, print_line};

/// An event `events` subscribes to. herdr spells each kind twice: dotted
/// in a subscription, `pane.focused`, and with an underscore in the event
/// it sends, `pane_focused`; [`Kind::names`] gives both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    PaneFocused,
    PaneExited,
    PaneClosed,
    PaneMoved,
    PaneCreated,
    LayoutUpdated,
    TabFocused,
    TabClosed,
    WorkspaceFocused,
}

impl Kind {
    /// Every kind subscribed to, in the order subscribed.
    const ALL: [Kind; 9] = [
        Kind::PaneFocused,
        Kind::PaneExited,
        Kind::PaneClosed,
        Kind::PaneMoved,
        Kind::PaneCreated,
        Kind::LayoutUpdated,
        Kind::TabFocused,
        Kind::TabClosed,
        Kind::WorkspaceFocused,
    ];

    /// Its name in a subscription, and in an event herdr sends.
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Kind::PaneFocused => ("pane.focused", "pane_focused"),
            Kind::PaneExited => ("pane.exited", "pane_exited"),
            Kind::PaneClosed => ("pane.closed", "pane_closed"),
            Kind::PaneMoved => ("pane.moved", "pane_moved"),
            Kind::PaneCreated => ("pane.created", "pane_created"),
            Kind::LayoutUpdated => ("layout.updated", "layout_updated"),
            Kind::TabFocused => ("tab.focused", "tab_focused"),
            Kind::TabClosed => ("tab.closed", "tab_closed"),
            Kind::WorkspaceFocused => ("workspace.focused", "workspace_focused"),
        }
    }

    /// The kind of an event herdr sent, by its `event`; None for one it
    /// wasn't asked for.
    fn of_event(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.names().1 == name)
    }

    /// A pane's end: its id is printed as `closed`.
    fn closes_a_pane(self) -> bool {
        matches!(self, Kind::PaneExited | Kind::PaneClosed)
    }

    /// Focus moved, though maybe not where the event says.
    fn focuses(self) -> bool {
        matches!(
            self,
            Kind::PaneFocused | Kind::TabFocused | Kind::WorkspaceFocused
        )
    }

    /// After it, the server's focus is read again: a focus event, or a
    /// close, which moves focus with no focus event.
    fn may_move_focus(self) -> bool {
        self.focuses() || self.closes_a_pane() || self == Kind::TabClosed
    }
}

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
    let subscriptions = Kind::ALL.map(|k| k.names().0);
    let mut events = match herdr.subscribe(&subscriptions) {
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
    // the focus last printed: where it starts, read once subscribed, what
    // moves it after being in an event
    let mut printed = focused().unwrap_or_else(|why| {
        eprintln!("herdr events: can't read where focus starts: {why}");
        None
    });
    if let Some(pane) = &printed
        && !print(&json!({ "focus": pane }).to_string())
    {
        return GONE.into();
    }
    loop {
        let event = match events.next() {
            Ok(e) => e,
            Err(why) => return why,
        };
        let kind = event["event"].as_str().and_then(Kind::of_event);
        let mut line = Map::new();
        if kind.is_some_and(Kind::closes_a_pane)
            && let Some(pane) = event["data"]["pane_id"].as_str()
        {
            line.insert("closed".into(), pane.into());
        }
        if kind.is_some_and(Kind::may_move_focus) {
            match focused() {
                Ok(Some(now)) if printed.as_ref() != Some(&now) => {
                    line.insert("focus".into(), now.as_str().into());
                    printed = Some(now);
                }
                // nothing new: a focus event changed no layout
                Ok(_) if kind.is_some_and(Kind::focuses) => continue,
                Ok(_) => {}
                // the report the line has the daemon run tells focus
                Err(why) => eprintln!("herdr events: can't read the server's focus: {why}"),
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
        assert_eq!(
            kinds,
            [
                "pane.focused",
                "pane.exited",
                "pane.closed",
                "pane.moved",
                "pane.created",
                "layout.updated",
                "tab.focused",
                "tab.closed",
                "workspace.focused",
            ]
        );
    }

    #[test]
    fn each_kinds_event_name_is_its_subscription_name_with_an_underscore() {
        // as herdr 0.9.5's schema spells them: a typo in one would make
        // its events lines that tell nothing
        for kind in Kind::ALL {
            let (subscription, event) = kind.names();
            assert_eq!(subscription.replacen('.', "_", 1), event, "{kind:?}");
            assert_eq!(Kind::of_event(event), Some(kind));
        }
        assert_eq!(Kind::of_event("pane.focused"), None);
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

    /// herdr 0.9.5's events as its own keys zoomed a pane, then unzoomed
    /// it: a `layout.updated` each, and nothing else.
    #[test]
    fn zooming_and_unzooming_each_have_the_layout_read() {
        let fake = subscribed(recorded_stream("events-zoom"), true, &[Some("w1:p3")]);
        let (lines, _) = run(&fake);
        assert_eq!(lines, [r#"{"focus":"w1:p3"}"#, "", ""]);
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
    fn a_focus_herdr_cant_tell_has_the_layout_read_and_the_next_one_printed_if_new() {
        let snapshot_fails = Reply::Line(recorded("pane-not-found"));
        let fake = Fake::serve(vec![
            (
                "events.subscribe",
                Reply::Stream {
                    lines: recorded_stream("events-close-focused")[..5].to_vec(),
                    end: true,
                },
            ),
            ("session.snapshot", focused(Some("w1:p1"))),
            ("session.snapshot", snapshot_fails),
            ("session.snapshot", focused(Some("w1:p5"))),
        ]);
        let (lines, _) = run(&fake);
        // the start; pane.focused unread: a bare line; pane.created and
        // layout.updated; tab.focused read, new, printed
        assert_eq!(
            lines,
            [r#"{"focus":"w1:p1"}"#, "", "", "", r#"{"focus":"w1:p5"}"#]
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

    /// Focus on w1:p1, then on no pane, then on w1:p1 again, read after
    /// three focus events: w1:p1 was the last printed, so it isn't again.
    #[test]
    fn focus_is_printed_only_when_it_isnt_the_last_printed() {
        let focus_event = recorded_stream("events-close-focused")[1].clone();
        let fake = subscribed(
            vec![
                recorded("subscribed"),
                focus_event.clone(),
                focus_event.clone(),
                focus_event,
            ],
            true,
            &[Some("w1:p1"), None, Some("w1:p1"), Some("w1:p5")],
        );
        let (lines, _) = run(&fake);
        // the start, then none to print, w1:p1 again, then w1:p5
        assert_eq!(lines, [r#"{"focus":"w1:p1"}"#, r#"{"focus":"w1:p5"}"#]);
    }

    /// A line that is JSON but no event, as a keepalive might be, is
    /// skipped; one that isn't JSON ends it.
    #[test]
    fn a_json_line_thats_no_event_is_skipped_and_one_thats_not_json_ends_it() {
        let fake = subscribed(
            vec![
                recorded("subscribed"),
                r#"{"keepalive":true}"#.into(),
                r#"{"event":3,"data":{}}"#.into(),
                "[]".into(),
                recorded("event-pane-exited"),
                "nope".into(),
                recorded("event-pane-exited"),
            ],
            true,
            &[None],
        );
        let (lines, why) = run(&fake);
        assert_eq!(lines, [r#"{"closed":"w1:p3"}"#]);
        assert_eq!(why, "herdr's answer can't be read");
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
