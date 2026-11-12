//! The last pane going (ADR 0003): the daemon goes unattached, keeping one
//! window, and ends unless Neovim has unsaved changes; with them it stays,
//! listing them, until an attach comes or they're gone.

use super::*;
use crate::attach::{Hello, Message, Purpose};
use crate::daemon::attach_conn::{self_hello, test_terminal};
use crate::daemon::dir::testing::Scratch;
use crate::daemon::dir::{self, Claimed};
use crate::daemon::event_loop::Kind;
use crate::screen::float_groups::Group;
use lifetime::Stage;

/// A ready daemon whose one pane shows window 1000, current, listing its
/// unsaved changes in a directory of its own.
struct Rig {
    fx: Fixture,
    pane: u32,
    unsaved: std::path::PathBuf,
    _claimed: Claimed,
    _base: Scratch,
}

impl Rig {
    fn new() -> Rig {
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        draw_external(&mut fx.d, 2, 1000, 80, 23);
        let pane = pane_in_block(&mut fx.d, 1000, 0, 1);
        fx.d.nvim.current_win = 1000;
        let base = Scratch::new();
        let claimed = dir::claim(&base.base(), "d").unwrap();
        fx.d.lifetime.unsaved_file = Some(claimed.unsaved());
        Rig {
            fx,
            pane,
            unsaved: base.0.join("d").join("unsaved"),
            _claimed: claimed,
            _base: base,
        }
    }

    /// Its pane goes.
    fn last_pane_goes(&mut self) {
        self.fx.d.pane_gone(self.pane).unwrap();
        assert!(deciding(&self.fx.d));
    }

    /// Neovim tells its unsaved changes, `files`, as answer `ask`.
    fn told(&mut self, files: &[&str], ask: u32) {
        let files = files.iter().map(|f| f.as_bytes().to_vec()).collect();
        self.fx
            .d
            .on_note(windows::Note::Unsaved { files, ask })
            .unwrap();
    }

    /// The `unsaved` file, if there is one.
    fn unsaved(&self) -> Option<String> {
        std::fs::read_to_string(&self.unsaved).ok()
    }

    /// A self-attach, accepted: its helper's end and its pty.
    fn self_attach(&mut self, file: Option<&str>) -> (UnixStream, Pty) {
        let purpose = Purpose::SelfAttach {
            terminal: test_terminal(80, 24),
            provider: None,
            file: file.map(|f| f.as_bytes().to_vec()),
            line: None,
        };
        let hello = match file {
            Some(_) => Message::Hello(Hello::new(purpose)).encode().unwrap(),
            None => self_hello(test_terminal(80, 24)),
        };
        self_attach_with(&mut self.fx, "unattached", &hello)
    }
}

/// The first answer the daemon asks of Neovim, as it goes unattached.
const FIRST_ASK: u32 = 1;

#[test]
fn going_unattached_keeps_the_current_window_unshown_sends_focuslost_and_asks_neovim() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    assert!(rig.fx.d.panes.list.is_empty());
    let sent = sent_to_nvim(&mut rig.fx);
    assert!(sent.contains("nvim_ui_set_focus"), "{sent}");
    assert!(sent.contains("unsaved_at_safe_state"), "{sent}");
    // the window is kept
    assert!(!sent.contains(".close("), "{sent}");
    // and gets no pane from a flush
    rig.fx.d.reconcile().unwrap();
    assert!(rig.fx.d.panes.list.is_empty());
    assert!(sent_by(rig.fx).asks.is_empty());
}

#[test]
fn with_nothing_unsaved_neovims_answer_ends_the_daemon() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    // a note it didn't ask for decides nothing
    rig.told(&[], 0);
    assert!(deciding(&rig.fx.d));
    rig.told(&[], FIRST_ASK);
    assert_eq!(Some(StopReason::PaneGone), rig.fx.d.stopped);
    assert_eq!(None, rig.unsaved());
}

#[test]
fn with_unsaved_changes_the_daemon_stays_listing_them_until_none_are_left() {
    let mut rig = Rig::new();
    rig.told(&["/a"], 0);
    // attached, nothing is listed
    assert_eq!(None, rig.unsaved());
    rig.last_pane_goes();
    rig.told(&["/a"], FIRST_ASK);
    assert_eq!(None, rig.fx.d.stopped);
    assert_eq!(Stage::StayingForUnsaved, rig.fx.d.lifetime.stage);
    assert_eq!(Some("unsaved 1\n/a\n".into()), rig.unsaved());
    // an unnamed buffer isn't listed: no file has its changes
    rig.told(&["/a", "", "/b"], 0);
    assert_eq!(Some("unsaved 1\n/a\n/b\n".into()), rig.unsaved());
    rig.told(&[], 0);
    assert_eq!(Some(StopReason::PaneGone), rig.fx.d.stopped);
}

#[test]
fn without_an_answer_in_time_what_neovim_last_said_decides() {
    for (files, stays) in [(&["/a"][..], true), (&[][..], false)] {
        let mut rig = Rig::new();
        rig.told(files, 0);
        rig.last_pane_goes();
        rig.fx.d.unattach_wait_over().unwrap();
        assert_eq!(stays, rig.fx.d.stopped.is_none(), "{files:?}");
        assert_eq!(stays, rig.unsaved().is_some(), "{files:?}");
        // its answer, late, decides nothing more
        rig.told(files, FIRST_ASK);
        assert_eq!(stays, rig.fx.d.stopped.is_none(), "{files:?}");
    }
}

#[test]
fn an_attach_while_deciding_keeps_the_daemon() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    let (_helper, _pty) = rig.self_attach(None);
    assert_eq!(Stage::Attached, rig.fx.d.lifetime.stage);
    rig.told(&[], FIRST_ASK);
    rig.fx.d.unattach_wait_over().unwrap();
    assert_eq!(None, rig.fx.d.stopped);
}

#[test]
fn an_attach_to_an_unattached_daemon_removes_its_list() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    rig.told(&["/a"], FIRST_ASK);
    assert!(rig.unsaved().is_some());
    let (_helper, _pty) = rig.self_attach(None);
    assert_eq!(Stage::Attached, rig.fx.d.lifetime.stage);
    assert_eq!(None, rig.unsaved());
    // attached, nothing unsaved ends nothing
    rig.told(&[], 0);
    assert_eq!(None, rig.fx.d.stopped);
}

#[test]
fn a_signal_while_deciding_stops_the_daemon() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    rig.fx
        .d
        .handle(Event {
            token: Token::Signal as usize,
            kind: Kind::default(),
            eof: false,
        })
        .unwrap();
    assert_eq!(Some(StopReason::Signal), rig.fx.d.stopped);
}

#[test]
fn a_requested_pane_on_its_way_counts_until_its_request_fails() {
    let mut rig = Rig::new();
    draw_external(&mut rig.fx.d, 3, 1001, 80, 23);
    let asked = rig.fx.d.panes.add_for_window(1001).id;
    rig.fx.d.pane_gone(rig.pane).unwrap();
    assert_eq!(Stage::Attached, rig.fx.d.lifetime.stage);
    rig.fx.d.give_up(asked).unwrap();
    assert!(deciding(&rig.fx.d));
}

#[test]
fn a_terminal_on_its_way_for_a_float_group_is_refused_once_unattached() {
    let mut rig = Rig::new();
    let id = rig.fx.d.panes.add_for_group(Group::default()).attach_id;
    rig.last_pane_goes();
    assert_eq!(
        Err(crate::attach::Refusal::PaneExpired),
        rig.fx.d.panes.ids.check(&id.unwrap())
    );
}

#[test]
fn a_bare_attach_takes_the_kept_window_first() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    let (_helper, _pty) = rig.self_attach(None);
    // a float current, and another window unshown before it
    list_answer(&mut rig.fx, 2000, &[999, 1000]);
    let id = rig.fx.d.panes.list.last().unwrap().id;
    assert_eq!(Some(id), rig.fx.d.panes.by_win(1000).map(|p| p.id));
}

#[test]
fn a_file_attach_closes_the_kept_window_once_its_file_shows() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    let _ = sent_to_nvim(&mut rig.fx);
    let (_helper, _pty) = rig.self_attach(Some("/f"));
    let ask = rig.fx.d.window_ask.expect("a new window asked");
    rig.fx.d.on_window_opened(1005).unwrap();
    assert!(!sent_to_nvim(&mut rig.fx).contains(".close("));
    let mut buf = Vec::new();
    msgpack::write_int(&mut buf, 1005);
    rig.fx
        .d
        .on_window_answer(ask.req, None, Decoder::of(&buf))
        .unwrap();
    assert!(rig.fx.d.changes.is_closing(1000));
    assert!(sent_to_nvim(&mut rig.fx).contains(".close("));
}

/// The answer to the new window the last self-attach asked for: `win`, or
/// an error.
fn opened(rig: &mut Rig, win: Option<i64>) {
    let ask = rig.fx.d.window_ask.expect("a new window asked");
    let mut buf = Vec::new();
    match win {
        Some(w) => {
            rig.fx.d.on_window_opened(w).unwrap();
            msgpack::write_int(&mut buf, w);
            rig.fx
                .d
                .on_window_answer(ask.req, None, Decoder::of(&buf))
                .unwrap();
        }
        None => {
            msgpack::write_nil(&mut buf);
            rig.fx
                .d
                .on_window_answer(ask.req, Some(b"E1: no"), Decoder::of(&buf))
                .unwrap();
        }
    }
}

#[test]
fn a_failed_file_attach_leaves_the_kept_window_for_the_next() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    let first = rig.self_attach(Some("/f"));
    opened(&mut rig, None);
    // unattached again, still keeping it
    assert!(deciding(&rig.fx.d));
    assert!(!rig.fx.d.changes.is_closing(1000));
    let _ = sent_to_nvim(&mut rig.fx);
    drop(first);
    let (_helper, _pty) = rig.self_attach(Some("/f"));
    opened(&mut rig, Some(1006));
    assert!(rig.fx.d.changes.is_closing(1000));
    assert!(sent_to_nvim(&mut rig.fx).contains(".close("));
}

/// An open of a tab page's terminal beside block 1, for pane `pane`.
fn open_tab_page(pane: crate::attach::PaneId) -> Ask {
    Ask::OpenTabPage {
        pane,
        attach: Vec::new(),
        title: String::new(),
        origin: TerminalId::from(block_id(1).as_str()),
        cols: 80,
        rows: 24,
    }
}

#[test]
fn opens_not_yet_asked_are_dropped_as_the_daemon_goes_unattached() {
    let mut rig = Rig::new();
    let ids = [rig.fx.d.panes.ids.mint(), rig.fx.d.panes.ids.mint()];
    for id in ids {
        rig.fx.d.act(open_tab_page(id));
    }
    // one at a time: the second waits
    assert_eq!(2, rig.fx.d.asks.waiting());
    rig.last_pane_goes();
    // the one asked may still be answered, and closed then
    assert_eq!(1, rig.fx.d.asks.waiting());
    let asked = fake(&mut rig.fx.d).asked();
    assert_eq!(vec![open_tab_page(ids[0])], asked);
}

#[test]
fn an_unattached_daemon_drops_its_pane_provider_once_its_closes_are_answered() {
    let mut rig = Rig::new();
    // a float group pane's terminal: going unattached closes it
    let group = rig.fx.d.panes.add_for_group(Group::default());
    group.terminal = Some(TerminalId::from("block:f"));
    rig.last_pane_goes();
    rig.told(&["/a"], FIRST_ASK);
    assert_eq!(Stage::StayingForUnsaved, rig.fx.d.lifetime.stage);
    let (close, ask) = fake(&mut rig.fx.d).asks.last().cloned().expect("a close");
    assert_eq!(asks::close(TerminalId::from("block:f")), ask);
    // no terminal in its reach, but its close unanswered: kept
    rig.fx.d.after_batch().unwrap();
    assert!(rig.fx.d.provider.is_some());
    fake(&mut rig.fx.d).answer(close, Outcome::Done(Answer::Nothing));
    rig.fx.d.drain_provider().unwrap();
    rig.fx.d.after_batch().unwrap();
    assert!(rig.fx.d.provider.is_none());
    assert_eq!(None, rig.fx.d.stopped);

    // the next attach chooses a pane provider of its own, and takes the
    // kept window
    rig.fx.d.test_choose = Some(Box::new(|_| crate::providers::choose::Chosen::Provider {
        held: Box::new(FakeProvider::opening()),
        spent: Duration::ZERO,
    }));
    let (_helper, _pty) = rig.self_attach(None);
    assert!(fake(&mut rig.fx.d).asks.is_empty(), "the old one");
    list_answer(&mut rig.fx, 2000, &[999, 1000]);
    let id = rig.fx.d.panes.list.last().unwrap().id;
    assert_eq!(Some(id), rig.fx.d.panes.by_win(1000).map(|p| p.id));
    assert_eq!(pane::Reach::In, rig.fx.d.panes.by_id(id).unwrap().reach);
}
