//! Choosing Rex at a self-attach (#114): never blocking the loop, confirmed
//! or refused by its `find`, the windows Neovim has meanwhile waiting for
//! it; and the reach of later self-attaches.

use std::cell::Cell;

use super::*;
use crate::daemon::pane::Reach;
use crate::rex::client::tests::{answer_hello, answer_upgrade, read_client_frame};

const BLOCK: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

thread_local! {
    /// The device the fake `tty_of_pid` gives every process's terminal, for
    /// the test on this thread.
    static TTY: Cell<u64> = const { Cell::new(0) };
}

fn fake_tty_of_pid(_pid: u32) -> Option<u64> {
    Some(TTY.get())
}

/// A self-attach's hello from a pane of Rex session `session`, with
/// `REX_BLOCK` `block`.
fn rex_hello(session: &str, block: &str) -> Vec<u8> {
    use crate::daemon::attach_conn::{self_hello, test_terminal};
    let mut t = test_terminal(80, 24);
    t.env = [
        ("REX_SERVER", "unix:///fake/rex.sock"),
        ("REX_SESSION", session),
        ("REX_BLOCK", block),
    ]
    .iter()
    .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
    .collect();
    self_hello(t)
}

/// A fake Rex on a socket pair: the daemon's end goes to whoever connects.
struct Rex {
    theirs: UnixStream,
}

impl Rex {
    /// Rex's side of each request the daemon sends next, as JSON.
    fn request(&mut self) -> Value {
        serde_json::from_slice(&read_client_frame(&mut self.theirs)).unwrap()
    }

    /// Rex answers request `id` with `payload`, or refuses it with `code`.
    fn reply(&mut self, d: &mut Daemon, id: &Value, payload: Value) {
        let text = json!({ "type": "response", "id": id, "seq": 0, "payload": payload });
        self.theirs
            .write_all(&server_frame(&text.to_string()))
            .unwrap();
        ready(d);
    }

    fn refuse(&mut self, d: &mut Daemon, id: &Value, code: &str) {
        let text = json!({ "type": "response", "id": id, "seq": 0,
                           "error": { "code": code, "message": "no", "status": 404 } });
        self.theirs
            .write_all(&server_frame(&text.to_string()))
            .unwrap();
        ready(d);
    }

    /// Rex upgrades the connection, welcomes the daemon, and answers its
    /// `session.attach` with an empty view. The next request: the `find`'s.
    fn attach(&mut self, d: &mut Daemon) -> Value {
        answer_upgrade(&mut self.theirs);
        ready(d);
        assert_eq!(NAME, answer_hello(&mut self.theirs)["client"]["name"]);
        ready(d);
        let attach = self.request();
        assert_eq!("session.attach", attach["method"]);
        let view = json!({ "revision": 1, "active_window_id": "window:w", "windows": [] });
        self.reply(
            d,
            &attach["id"],
            json!({ "client_id": "client:c", "view": view }),
        );
        self.request()
    }
}

/// The loop says Rex's socket is ready.
fn ready(d: &mut Daemon) {
    d.on_provider(Token::Provider as usize).unwrap();
}

/// A daemon with no pane provider, ready, connecting any Rex to the fake.
fn fixture_with_rex() -> (Fixture, Rex) {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut ours = Some(ours);
    fx.d.connect = Box::new(move |url| {
        assert_eq!("unix:///fake/rex.sock", url);
        Ok(ours.take().expect("connected once"))
    });
    (fx, Rex { theirs })
}

/// Self-attaches `hello` to `fx`'s daemon, its pane showing window 1000:
/// its id, its helper's end, and its pty.
fn attach_showing_1000(fx: &mut Fixture, hello: &[u8]) -> (u32, UnixStream, Pty) {
    draw_external(&mut fx.d, 2, 1000, 80, 23);
    let (helper, pty) = self_attach_with(fx, "choose", hello);
    let p = fx.d.panes.list.last_mut().unwrap();
    p.neovim_window_handle = Some(1000);
    fx.d.window_ask = None;
    fx.d.nvim.current_win = 1000;
    let id = p.id;
    TTY.set(p.info.as_ref().unwrap().device.unwrap());
    (id, helper, pty)
}

/// Gives Rex's pane provider the fake `tty_of_pid`.
fn fake_ttys(d: &mut Daemon) {
    match &mut d.provider {
        Some(Held::Rex(r)) => r.tty_of_pid = fake_tty_of_pid,
        _ => panic!("Rex wasn't chosen"),
    }
}

fn child() -> Value {
    json!({ "child": { "pid": 1, "user_id": 0, "name": "login" } })
}

#[test]
fn choosing_rex_blocks_no_key_and_its_find_confirms_the_choosing_pane() {
    let (mut fx, mut rex) = fixture_with_rex();
    let (id, _helper, pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    fake_ttys(&mut fx.d);
    assert_eq!(Some(id), fx.d.choosing);
    let _ = sent_to_nvim(&mut fx);

    // Rex hasn't answered at all: a key goes to Neovim at once (a line:
    // the pty isn't raw, as a helper would make it)
    // SAFETY: writing to the pty's master, as typing does.
    assert_eq!(2, unsafe {
        libc::write(pty.master, b"x\r".as_ptr().cast(), 2)
    });
    fx.d.on_input(id).unwrap();
    let sent = sent_to_nvim(&mut fx);
    assert!(sent.contains("nvim_input"), "{sent:?}");

    let find = rex.attach(&mut fx.d);
    assert_eq!(
        json!({ "session_id": "session:s", "block_id": BLOCK }),
        find["payload"]
    );
    assert_eq!(Reach::Finding, fx.d.panes.by_id(id).unwrap().reach);
    rex.reply(&mut fx.d, &find["id"], child());

    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!(Some(TerminalId::from(BLOCK)), p.terminal);
    assert_eq!(Reach::In, p.reach);
    assert_eq!(None, fx.d.choosing);
    assert!(fx.d.provider.is_some());
}

#[test]
fn a_choice_its_find_refuses_drops_rex_with_a_warning() {
    // REX_* leaked into a terminal Rex doesn't run
    let (mut fx, mut rex) = fixture_with_rex();
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    fake_ttys(&mut fx.d);
    TTY.set(u64::MAX);
    let _ = sent_to_nvim(&mut fx);
    let find = rex.attach(&mut fx.d);
    rex.reply(&mut fx.d, &find["id"], child());

    assert!(fx.d.provider.is_none());
    assert_eq!(None, fx.d.choosing);
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    let warned = sent_to_nvim(&mut fx);
    assert!(
        warned.contains("rex: find didn't confirm this terminal: REX_BLOCK names another terminal; no pane provider"),
        "{warned:?}"
    );
}

#[test]
fn a_choice_rex_cant_attach_or_reach_is_none_with_a_warning() {
    // a stale REX_SESSION: Rex refuses the attach
    let (mut fx, mut rex) = fixture_with_rex();
    attach_showing_1000(&mut fx, &rex_hello("session:gone", BLOCK));
    let _ = sent_to_nvim(&mut fx);
    answer_upgrade(&mut rex.theirs);
    ready(&mut fx.d);
    answer_hello(&mut rex.theirs);
    ready(&mut fx.d);
    let attach = rex.request();
    rex.refuse(&mut fx.d, &attach["id"], "session_not_found");
    assert!(fx.d.provider.is_none());
    assert_eq!(None, fx.d.choosing);
    let warned = sent_to_nvim(&mut fx);
    assert!(
        warned.contains("session_not_found") && warned.contains("no pane provider"),
        "{warned:?}"
    );

    // a stale REX_SERVER: nothing listens
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    fx.d.connect = Box::new(|_| Err(std::io::ErrorKind::ConnectionRefused.into()));
    attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    assert!(fx.d.provider.is_none());
    let warned = sent_to_nvim(&mut fx);
    assert!(
        warned.contains("can't reach Rex at unix:///fake/rex.sock")
            && warned.contains("no pane provider"),
        "{warned:?}"
    );
}

#[test]
fn windows_wait_unshown_for_the_choice_then_open_beside_the_choosing_pane() {
    let (mut fx, mut rex) = fixture_with_rex();
    let (_id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    fake_ttys(&mut fx.d);
    // `-O a b`: Neovim's second window, split off the first
    draw_external(&mut fx.d, 3, 1001, 40, 23);
    fx.d.changes.note_split(windows::Split {
        win: 1001,
        side_by_side: true,
        before: false,
        from: 1000,
        tab: 0,
    });
    fx.d.reconcile().unwrap();
    assert!(fx.d.panes.by_win(1001).is_none(), "drawn before the choice");

    let find = rex.attach(&mut fx.d);
    rex.reply(&mut fx.d, &find["id"], child());
    // opened beside the choosing pane's block, as a new window is
    let p = fx.d.panes.by_win(1001).expect("a pane of its own");
    assert!(!p.is_self_attached());
    let split = rex.request();
    assert_eq!("session.new_split", split["method"]);
    assert_eq!(BLOCK, split["payload"]["anchor_id"]);
    assert_eq!("horizontal", split["payload"]["direction"]);
    assert_eq!("after", split["payload"]["side"]);
}

#[test]
fn a_refused_choice_draws_the_waiting_windows_in_the_choosing_pane() {
    let (mut fx, mut rex) = fixture_with_rex();
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    fake_ttys(&mut fx.d);
    TTY.set(u64::MAX);
    draw_external(&mut fx.d, 3, 1001, 40, 23);
    fx.d.changes.note_split(windows::Split {
        win: 1001,
        side_by_side: true,
        before: false,
        from: 1000,
        tab: 0,
    });
    fx.d.reconcile().unwrap();
    let find = rex.attach(&mut fx.d);
    rex.reply(&mut fx.d, &find["id"], child());
    let p = fx.d.panes.by_id(id).unwrap();
    assert!(p.holds(1001), "not drawn in the choosing pane");
}

/// A daemon whose fake pane provider was chosen in Rex session `session:a`
/// and can `find`, its first pane showing window 1000.
fn chosen_in_session_a() -> Fixture {
    let mut fake = FakeProvider::opening();
    fake.caps.insert(Capability::Find);
    fake.context = &["REX_SESSION"];
    let mut fx = with_provider(Some(Held::Fake(fake)));
    fx.d.context = vec![Some(b"session:a".to_vec())];
    fx.d.nvim.ready = true;
    fx
}

#[test]
fn a_self_attach_from_another_context_is_out_of_reach_unfound() {
    let mut fx = chosen_in_session_a();
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:b", BLOCK));
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    assert!(fake(&mut fx.d).asked().is_empty());
    // a window opened from it is drawn in it
    draw_external(&mut fx.d, 3, 1001, 40, 23);
    fx.d.changes.note_split(windows::Split {
        win: 1001,
        side_by_side: true,
        before: false,
        from: 1000,
        tab: 0,
    });
    fx.d.reconcile().unwrap();
    assert!(fx.d.panes.by_id(id).unwrap().holds(1001));
    assert!(fake(&mut fx.d).asked().is_empty());
}

#[test]
fn a_self_attach_from_the_same_context_is_found_and_in_reach() {
    let mut fx = chosen_in_session_a();
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:a", BLOCK));
    let Some(Ask::Find { env, device, .. }) = fake(&mut fx.d).asked().pop() else {
        panic!("no find asked");
    };
    // only `find`'s variables: the fake names none
    assert!(env.is_empty());
    assert!(device.is_some());
    assert_eq!(Reach::Finding, fx.d.panes.by_id(id).unwrap().reach);
    let ask = fake(&mut fx.d).last_id();
    fake(&mut fx.d).opened(ask, BLOCK);
    fx.d.drain_provider().unwrap();
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!(
        (Reach::In, Some(TerminalId::from(BLOCK))),
        (p.reach, p.terminal.clone())
    );
}
