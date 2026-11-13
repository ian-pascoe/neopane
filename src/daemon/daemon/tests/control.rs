//! Control requests (#91 decision 26, #94 decisions 29–32): `status`
//! answered from what the daemon tracks, and `stop`, refused with unsaved
//! changes unless forced.

use super::*;
use crate::attach::{ATTACH_PROTOCOL_VERSION, Ending, Hello, Message, Purpose, Refusal, Reply};
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

#[test]
fn open_isnt_taken_yet() {
    let mut fx = fixture();
    let at = Address::new("open");
    let hello = Hello::new(Purpose::Open {
        pid: None,
        cwd: b"/".to_vec(),
        file: b"/f".to_vec(),
        line: None,
        wait: false,
    });
    let helper = at.send(&Message::Hello(hello).encode().unwrap(), None);
    fx.d.accept(&at.server).unwrap();
    let z = refusal(&helper);
    assert_eq!(Some(Refusal::Hello), z.as_refusal());
    assert_eq!("this daemon takes no open hello yet", z.message);
}
