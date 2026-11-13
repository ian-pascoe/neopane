//! Control requests (#91 decision 26, #94 decisions 29–32): `status`
//! answered from what the daemon tracks, and `stop`, refused with unsaved
//! changes unless forced.

use super::*;
use crate::attach::{
    ATTACH_PROTOCOL_VERSION, Ending, Hello, Message, OpenFile, Purpose, Refusal, Reply, Wait,
};
use crate::daemon::attach_conn::read_all_frames;

/// Sends `hello` to `fx`'s daemon as a control request: the reply, and
/// whatever came after it.
fn control(fx: &mut Fixture, hello: Hello) -> (Reply, Vec<Message>) {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let at = Address::new(&format!("control-{n}"));
    let helper = at.send(&Message::Hello(hello).encode().unwrap(), None);
    fx.d.accept(&at.server).unwrap();
    let mut said = read_all_frames(&helper);
    let Message::Reply(r) = said.remove(0) else {
        panic!("{said:?}")
    };
    (r, said)
}

fn status() -> Hello {
    Hello::new(Purpose::Status { pid: Some(1) })
}

fn stop(force: bool) -> Hello {
    Hello::new(Purpose::Stop { pid: None, force })
}

/// A daemon named `work`, at `/tmp/x/work/sock`, logging to
/// `/tmp/x/work/log`, whose one requested pane shows window 1000 in
/// terminal block 1. The pane's id.
fn work() -> (Fixture, u32) {
    let mut fx = fixture();
    fx.d.name = "work".into();
    fx.d.sock = crate::daemon::dir::Address::from_arg("/tmp/x/work/sock".into());
    fx.d.log = "/tmp/x/work/log".into();
    fx.d.nvim.ready = true;
    draw_external(&mut fx.d, 2, 1000, 80, 23);
    let pane = pane_in_block(&mut fx.d, 1000, 0, 1);
    (fx, pane)
}

#[test]
fn status_answers_with_one_reply_from_what_the_daemon_tracks() {
    let (mut fx, _) = work();
    let (_helper, _pty) = self_attach_with(&mut fx, "status-self", &self_hello_from(b"", b""));
    fx.d.on_note(windows::Note::Unsaved {
        files: vec![b"/w/a.txt".to_vec(), Vec::new()],
        ask: 0,
    })
    .unwrap();
    fx.d.on_note(windows::Note::Cwd(b"/w".to_vec())).unwrap();
    fx.d.on_note(windows::Note::Terminals(vec![b"/dev/ttys004".to_vec()]))
        .unwrap();
    let (r, after) = control(&mut fx, status());
    assert!(after.is_empty(), "{after:?}");
    assert!(r.ok);
    assert_eq!(None, r.message);
    assert_eq!(ATTACH_PROTOCOL_VERSION, r.v);
    assert_eq!(crate::attach::build(), r.build);
    assert_eq!("work", r.name);
    assert_eq!(std::process::id(), r.pid);
    let s = r.status.expect("versions agree");
    assert_eq!(b"/tmp/x/work/sock".to_vec(), s.address);
    // the self-attached pane: the requested one has no terminal yet
    assert_eq!(1, s.panes);
    assert_eq!(vec![b"/w/a.txt".to_vec(), Vec::new()], s.unsaved);
    // the fixture's
    assert_eq!("fake", s.provider);
    assert_eq!(Some(b"/w".to_vec()), s.dir);
    assert_eq!(Some(false), s.kept);
    let caps = ["open_beside", "open_float", "open_tab_page", "close"];
    assert_eq!(Some(caps.map(String::from).to_vec()), s.capabilities);
    assert_eq!(Some(vec![]), s.unsupported);
    assert_eq!(Some(b"/tmp/x/work/log".to_vec()), s.log);
    assert_eq!(Some(vec![b"/dev/ttys004".to_vec()]), s.terminals);
    assert_eq!(None, fx.d.stopped);
}

#[test]
fn status_says_why_there_is_no_pane_provider() {
    let mut fx = with_provider(None);
    assert_eq!(
        "none",
        control(&mut fx, status()).0.status.unwrap().provider
    );
    assert_eq!(
        Some(vec![]),
        control(&mut fx, status()).0.status.unwrap().capabilities
    );
}

#[test]
fn another_versions_status_gets_only_the_frozen_fields() {
    let (mut fx, _) = work();
    let older = Hello {
        v: ATTACH_PROTOCOL_VERSION - 1,
        build: format!("{NAME} 0.0.9"),
        purpose: Purpose::Status { pid: None },
    };
    let (r, _) = control(&mut fx, older);
    assert!(r.ok);
    assert_eq!("work", r.name);
    assert_eq!(std::process::id(), r.pid);
    assert_eq!(None, r.status);
}

#[test]
fn stop_with_unsaved_changes_is_refused_naming_them_unless_forced() {
    let (mut fx, _) = work();
    fx.d.on_note(windows::Note::Cwd(b"/w".to_vec())).unwrap();
    fx.d.on_note(windows::Note::Unsaved {
        files: vec![b"/w/README.md".to_vec(), b"/w/src/main.rs".to_vec()],
        ask: 0,
    })
    .unwrap();
    let (r, _) = control(&mut fx, stop(false));
    assert!(!r.ok);
    assert_eq!(
        Some(
            "work has unsaved changes: README.md, src/main.rs (save them, or kill --force to \
             keep them only in swap files)"
                .to_string()
        ),
        r.message
    );
    assert_eq!(None, fx.d.stopped);
    assert!(!fx.d.stopping);
    // another version's stop reads `force` too: it's frozen
    let older = Hello {
        v: ATTACH_PROTOCOL_VERSION + 1,
        build: format!("{NAME} 9.0.0"),
        purpose: Purpose::Stop {
            pid: None,
            force: true,
        },
    };
    let (r, _) = control(&mut fx, older);
    assert!(r.ok);
    assert!(fx.d.stopping);
}

#[test]
fn stop_ends_attaches_stopped_and_closes_the_requested_terminals_first() {
    let (mut fx, _) = work();
    let (helper, _pty) = self_attach_with(&mut fx, "stop-self", &self_hello_from(b"", b""));
    let (r, _) = control(&mut fx, stop(false));
    assert!(r.ok);
    // the self-attach ended `stopped`
    let said = read_all_frames(&helper);
    let Some(Message::End(z)) = said.last() else {
        panic!("{said:?}")
    };
    assert_eq!(Ending::Stopped, z.as_ending());
    assert!(fx.d.panes.list.is_empty());
    // the requested terminal is asked to close; it stops once that's done
    let close = Ask::Close {
        terminal: TerminalId::from(block_id(1).as_str()),
    };
    assert_eq!(Some(&close), fake(&mut fx.d).asked().last());
    assert_eq!(None, fx.d.stopped);
    // meanwhile, an attach is refused as the daemon ends
    let at = Address::new("stop-late");
    let late = at.send(&self_hello_from(b"", b""), None);
    fx.d.accept(&at.server).unwrap();
    assert_eq!(Some(Refusal::Stopping), refusal(&late).as_refusal());
    let id = fake(&mut fx.d).last_id();
    fake(&mut fx.d).answer(id, Outcome::Done(Answer::Nothing));
    fx.d.after_batch().unwrap();
    assert_eq!(Some(StopReason::Stopped), fx.d.stopped);
}

#[test]
fn a_redraw_during_the_stop_wait_doesnt_end_it_or_stay_for_unsaved_changes() {
    let (mut fx, _) = work();
    fx.d.on_note(windows::Note::Unsaved {
        files: vec![b"/w/a.txt".to_vec()],
        ask: 0,
    })
    .unwrap();
    control(&mut fx, stop(true));
    assert!(fx.d.stopping);
    // Neovim flushes as its windows' panes go: nothing goes unattached
    fx.d.reconcile().unwrap();
    assert!(fx.d.lifetime.is_attached());
    assert_eq!(None, fx.d.stopped);
    assert!(fx.d.panes.list.is_empty());
    let id = fake(&mut fx.d).last_id();
    fake(&mut fx.d).answer(id, Outcome::Done(Answer::Nothing));
    fx.d.after_batch().unwrap();
    assert_eq!(Some(StopReason::Stopped), fx.d.stopped);
}

#[test]
fn stop_with_nothing_to_close_stops_at_once() {
    let mut fx = fixture();
    let (r, _) = control(&mut fx, stop(false));
    assert!(r.ok);
    assert_eq!(Some(StopReason::Stopped), fx.d.stopped);
}

#[test]
fn a_close_never_answered_is_left_after_the_stop_wait() {
    let (mut fx, _) = work();
    control(&mut fx, stop(true));
    assert_eq!(None, fx.d.stopped);
    let timer = crate::daemon::event_loop::Event {
        token: Token::StopTimer as usize,
        kind: crate::daemon::event_loop::Kind::Timer,
        eof: false,
    };
    fx.d.handle(timer).unwrap();
    assert_eq!(Some(StopReason::Stopped), fx.d.stopped);
}

/// Sends `fx`'s daemon an `open` of `/w/notes.md` from `/w`, at line 3,
/// waiting as `wait` says: the helper's end.
fn send_open(fx: &mut Fixture, tag: &str, wait: Wait) -> (UnixStream, Address) {
    let at = Address::new(tag);
    let hello = Hello::new(Purpose::Open {
        pid: None,
        cwd: b"/w".to_vec(),
        tty: None,
        open: OpenFile {
            file: b"notes.md".to_vec(),
            line: Some(3),
            wait,
        },
    });
    let helper = at.send(&Message::Hello(hello).encode().unwrap(), None);
    fx.d.accept(&at.server).unwrap();
    (helper, at)
}

/// `send_open`, and the request it asked of Neovim.
fn open(fx: &mut Fixture, tag: &str, wait: Wait) -> (UnixStream, Address, u32) {
    let (helper, at) = send_open(fx, tag, wait);
    let req = fx.d.opens.asked.last().expect("asked of Neovim").req;
    (helper, at, req)
}

/// Neovim's answer to `edit`: buffer `buf`, named `name`, `watched`.
fn edited(fx: &mut Fixture, req: u32, buf: i64, name: &[u8], watched: bool) {
    let mut b = Vec::new();
    msgpack::write_array_header(&mut b, 3);
    msgpack::write_int(&mut b, buf);
    msgpack::write_str(&mut b, name);
    msgpack::write_bool(&mut b, watched);
    assert!(fx.d.on_open_answer(req, None, Decoder::of(&b)).unwrap());
}

/// Whether anything was sent to `helper` yet.
fn answered(helper: &UnixStream) -> bool {
    helper.set_nonblocking(true).unwrap();
    let mut buf = [0u8; 1];
    let got = (&*helper).read(&mut buf);
    helper.set_nonblocking(false).unwrap();
    !matches!(got, Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

/// The reply sent to `helper`, which is hung up on after it.
fn reply_of(helper: &UnixStream) -> Reply {
    match &read_all_frames(helper)[..] {
        [Message::Reply(r)] => r.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_open_is_answered_once_neovim_has_its_window() {
    let (mut fx, _) = work();
    let (helper, _at, req) = open(&mut fx, "open-now", Wait::ForWindow);
    assert!(!answered(&helper));
    edited(&mut fx, req, 7, b"/w/notes.md", false);
    let r = reply_of(&helper);
    assert!(r.ok, "{r:?}");
    assert_eq!((None, None), (r.message, r.waited));
    assert!(fx.d.opens.asked.is_empty());
}

#[test]
fn a_waited_open_is_answered_once_its_buffer_is_done() {
    let (mut fx, _) = work();
    let (helper, _at, req) = open(&mut fx, "open-wait", Wait::UntilDone);
    edited(&mut fx, req, 7, b"/w/notes.md", true);
    assert!(!answered(&helper));
    // another buffer's done, and this one's
    fx.d.on_note(windows::Note::Done {
        buf: 8,
        by_pane: false,
    })
    .unwrap();
    assert!(!answered(&helper));
    fx.d.on_note(windows::Note::Done {
        buf: 7,
        by_pane: false,
    })
    .unwrap();
    let r = reply_of(&helper);
    assert!(r.ok, "{r:?}");
    assert_eq!(Some(true), r.waited);
    // done before Neovim answered
    let (helper, _at, req) = open(&mut fx, "open-done", Wait::UntilDone);
    edited(&mut fx, req, 9, b"/w/notes.md", false);
    assert_eq!(Some(true), reply_of(&helper).waited);
}

#[test]
fn a_waited_file_whose_terminal_hung_up_with_unsaved_changes_fails() {
    let (mut fx, _) = work();
    let (unsaved, _a, req) = open(&mut fx, "open-unsaved", Wait::UntilDone);
    edited(&mut fx, req, 7, b"/w/notes.md", true);
    let (saved, _b, req) = open(&mut fx, "open-saved", Wait::UntilDone);
    edited(&mut fx, req, 8, b"/w/other.md", true);
    let (closed, _c, req) = open(&mut fx, "open-closed", Wait::UntilDone);
    edited(&mut fx, req, 9, b"/w/third.md", true);
    fx.d.on_note(windows::Note::Unsaved {
        files: vec![b"/w/notes.md".to_vec(), b"/w/third.md".to_vec()],
        ask: 0,
    })
    .unwrap();
    for (buf, by_pane) in [(7, true), (8, true), (9, false)] {
        fx.d.on_note(windows::Note::Done { buf, by_pane }).unwrap();
    }
    let r = reply_of(&unsaved);
    assert!(!r.ok);
    assert_eq!(Some(true), r.waited);
    assert_eq!(
        Some("notes.md's terminal closed with unsaved changes".to_string()),
        r.message
    );
    // saved, or closed by its own :q, it's done
    assert!(reply_of(&saved).ok);
    assert!(reply_of(&closed).ok);
}

#[test]
fn an_open_neovim_refused_fails_with_its_error() {
    let (mut fx, _) = work();
    let (helper, _at, req) = open(&mut fx, "open-err", Wait::UntilDone);
    let err = b"Error executing lua: x:1: Vim(split):E325: ATTENTION";
    assert!(
        fx.d.on_open_answer(req, Some(err), Decoder::of(&[0xc0]))
            .unwrap()
    );
    let r = reply_of(&helper);
    assert!(!r.ok);
    assert_eq!(None, r.waited);
    assert_eq!(
        Some("notes.md didn't open: E325: ATTENTION".to_string()),
        r.message
    );
}

#[test]
fn stopping_answers_the_opens_left() {
    let (mut fx, _) = work();
    let (waiting, _a, req) = open(&mut fx, "open-stop", Wait::UntilDone);
    edited(&mut fx, req, 7, b"/w/notes.md", true);
    let (asked, _b, _) = open(&mut fx, "open-stop2", Wait::UntilDone);
    control(&mut fx, stop(true));
    for helper in [waiting, asked] {
        let r = reply_of(&helper);
        assert!(!r.ok);
        assert_eq!(Some(true), r.waited);
        assert_eq!(
            Some("work was stopped before notes.md was done".to_string()),
            r.message
        );
    }
}

#[test]
fn an_open_before_neovim_is_ready_fails() {
    let mut fx = fixture();
    fx.d.name = "work".into();
    let (helper, _at) = send_open(&mut fx, "open-early", Wait::UntilDone);
    let r = reply_of(&helper);
    assert!(!r.ok);
    assert_eq!(
        Some("work's Neovim isn't ready to open notes.md".to_string()),
        r.message
    );
}
