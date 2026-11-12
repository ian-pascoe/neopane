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
use lifetime::Lifetime;

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
        fx.d.unsaved_file = Some(claimed.unsaved());
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
    assert_eq!(Lifetime::Unattached, rig.fx.d.lifetime);
    assert_eq!(Some("unsaved 1\n/a\n".into()), rig.unsaved());
    rig.told(&["/a", ""], 0);
    assert_eq!(Some("unsaved 1\n/a\n\n".into()), rig.unsaved());
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
    assert_eq!(Lifetime::Attached, rig.fx.d.lifetime);
    rig.told(&[], FIRST_ASK);
    rig.fx.d.unattach_wait_over().unwrap();
    assert_eq!(None, rig.fx.d.stopped);
}

#[test]
fn an_attach_to_a_daemon_kept_for_unsaved_changes_removes_its_list() {
    let mut rig = Rig::new();
    rig.last_pane_goes();
    rig.told(&["/a"], FIRST_ASK);
    assert!(rig.unsaved().is_some());
    let (_helper, _pty) = rig.self_attach(None);
    assert_eq!(Lifetime::Attached, rig.fx.d.lifetime);
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
    assert_eq!(Lifetime::Attached, rig.fx.d.lifetime);
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
