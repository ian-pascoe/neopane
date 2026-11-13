//! Choosing a pane provider at a self-attach (#114): never blocking the
//! loop, confirmed or refused by its `find`, the windows Neovim has
//! meanwhile waiting for it; the reach of later self-attaches; and dropping
//! it and choosing again (#115). Rex's own side of this is its provider's
//! tests: here, the fake is chosen.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use super::*;
use crate::daemon::pane::Reach;
use crate::daemon::provider::AskId;
use crate::providers::choose::Chosen;

const BLOCK: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

/// A self-attach's hello from a pane of Rex session `session`, with
/// `REX_BLOCK` `block`.
fn rex_hello(session: &str, block: &str) -> Vec<u8> {
    use crate::daemon::attach_conn::{self_hello, test_terminal};
    let mut t = test_terminal(80, 24);
    t.env = env_of(session, block);
    self_hello(t)
}

/// A requested pane's hello from a pane of Rex session `session`, with
/// `REX_BLOCK` `block`.
fn requested_rex_hello(pane: crate::attach::PaneId, session: &str, block: &str) -> Vec<u8> {
    use crate::daemon::attach_conn::{requested_hello, test_terminal};
    let mut t = test_terminal(80, 24);
    t.env = env_of(session, block);
    requested_hello(pane, t)
}

fn env_of(session: &str, block: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    [
        ("REX_SERVER", "unix:///fake/rex.sock"),
        ("REX_SESSION", session),
        ("REX_BLOCK", block),
    ]
    .iter()
    .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
    .collect()
}

/// A fake as the built-in default chooses Rex: it can open, close and
/// `find`, with Rex's context variables.
fn finding() -> FakeProvider {
    let mut f = FakeProvider::opening();
    f.caps.insert(Capability::Find);
    f.caps.insert(Capability::Focus);
    f.context = &["REX_SERVER", "REX_SESSION"];
    f
}

/// What each self-attach that chooses gets, in turn; then none.
type Choices = Rc<RefCell<VecDeque<Chosen>>>;

/// A daemon with no pane provider, Neovim set up, whose self-attaches
/// choose each of `fakes` in turn.
fn fixture_choosing(fakes: Vec<FakeProvider>) -> (Fixture, Choices) {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    let choices: Choices = Rc::new(RefCell::new(
        fakes
            .into_iter()
            .map(|f| Chosen::Provider {
                held: Box::new(f),
                spent: Duration::ZERO,
            })
            .collect(),
    ));
    let next = choices.clone();
    fx.d.test_choose = Some(Box::new(move |_| {
        next.borrow_mut().pop_front().unwrap_or(Chosen::None {
            why: "none left".into(),
            warn: false,
        })
    }));
    (fx, choices)
}

/// A daemon whose self-attaches choose none, for `why`, with a warning.
fn fixture_unreachable(why: &'static str) -> Fixture {
    let mut fx = with_provider(None);
    fx.d.test_choose = Some(Box::new(move |_| Chosen::None {
        why: why.into(),
        warn: true,
    }));
    fx
}

/// Self-attaches `hello` to `fx`'s daemon, its pane still without a
/// window: its id, its helper's end, and its pty.
fn attach_later(fx: &mut Fixture, hello: &[u8]) -> (u32, UnixStream, Pty) {
    let (helper, pty) = self_attach_with(fx, "later", hello);
    fx.d.window_ask = None;
    (fx.d.panes.list.last().unwrap().id, helper, pty)
}

fn choosing_id(d: &Daemon) -> Option<u32> {
    d.choosing.as_ref().map(|c| c.id)
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
    (id, helper, pty)
}

/// The id of the last `find` asked, and its pane id.
fn last_find(d: &mut Daemon) -> (AskId, crate::attach::PaneId) {
    let (id, ask) = last_ask(d);
    let Ask::Find { pane, .. } = ask else {
        panic!("not a find: {ask:?}");
    };
    (id, pane)
}

/// The id of the last `open_beside` asked, and its ask.
fn last_open_beside(d: &mut Daemon) -> (AskId, Ask) {
    let (id, ask) = last_ask(d);
    assert!(
        matches!(ask, Ask::OpenBeside { .. }),
        "not an open_beside: {ask:?}"
    );
    (id, ask)
}

/// The last ask, and its id.
fn last_ask(d: &mut Daemon) -> (AskId, Ask) {
    fake(d).asks.last().expect("an ask").clone()
}

/// The pane provider finds the terminal of the last `find`: `block`.
fn found(d: &mut Daemon, block: &str) {
    let (id, _) = last_find(d);
    fake(d).opened(id, block);
    d.drain_provider().unwrap();
}

/// The pane provider refuses the last `find`, for `why`.
fn not_found(d: &mut Daemon, why: &str) {
    let (id, _) = last_find(d);
    fake(d).answer(id, Outcome::Failed(why.into()));
    d.drain_provider().unwrap();
}

fn finds_asked(d: &mut Daemon) -> usize {
    fake(d)
        .asked()
        .iter()
        .filter(|a| matches!(a, Ask::Find { .. }))
        .count()
}

/// Neovim's second window, split off the first to its right, as `-O a b`
/// makes it.
fn split_1001_off_1000(fx: &mut Fixture) {
    draw_external(&mut fx.d, 3, 1001, 40, 23);
    fx.d.changes.note_split(windows::Split {
        win: 1001,
        side_by_side: true,
        before: false,
        from: 1000,
        tab: 0,
    });
    fx.d.reconcile().unwrap();
}

#[test]
fn choosing_blocks_no_key_and_its_find_confirms_the_choosing_pane() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let (id, _helper, pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    assert_eq!(Some(id), choosing_id(&fx.d));
    let _ = sent_to_nvim(&mut fx);

    // nothing answered at all: a key goes to Neovim at once (a line: the
    // pty isn't raw, as a helper would make it)
    // SAFETY: writing to the pty's master, as typing does.
    assert_eq!(2, unsafe {
        libc::write(pty.master, b"x\r".as_ptr().cast(), 2)
    });
    fx.d.on_input(id).unwrap();
    let sent = sent_to_nvim(&mut fx);
    assert!(sent.contains("nvim_input"), "{sent:?}");

    assert_eq!(Reach::Finding, fx.d.panes.by_id(id).unwrap().reach);
    found(&mut fx.d, BLOCK);
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!(Some(TerminalId::from(BLOCK)), p.terminal);
    assert_eq!(Reach::In, p.reach);
    assert_eq!(None, choosing_id(&fx.d));
    assert!(fx.d.provider.is_some());
}

#[test]
fn a_choice_its_find_refuses_drops_the_pane_provider_with_a_warning() {
    // REX_* leaked into a terminal Rex doesn't run
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let _ = sent_to_nvim(&mut fx);
    // status says so
    assert_eq!("(choosing)", fx.d.provider_shown());
    not_found(&mut fx.d, "REX_BLOCK names another terminal");
    assert_eq!("none (find refused)", fx.d.provider_shown());

    assert!(fx.d.provider.is_none());
    assert_eq!(None, choosing_id(&fx.d));
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    let warned = sent_to_nvim(&mut fx);
    assert_eq!(
        1,
        warned
            .matches("fake: find didn't confirm this terminal (REX_BLOCK names another terminal); no pane provider")
            .count(),
        "{warned:?}"
    );
}

#[test]
fn a_choice_gone_before_its_find_or_none_reached_is_no_pane_provider_with_a_warning() {
    // a stale REX_SESSION: Rex refuses the attach, and is gone
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:gone", BLOCK));
    let _ = sent_to_nvim(&mut fx);
    fake(&mut fx.d).push(ProviderEvent::Gone {
        reason: "Rex refused session.attach: session_not_found".into(),
    });
    fx.d.drain_provider().unwrap();
    assert!(fx.d.provider.is_none());
    assert_eq!(None, choosing_id(&fx.d));
    let warned = sent_to_nvim(&mut fx);
    assert_eq!(
        1,
        warned
            .matches("fake: Rex refused session.attach: session_not_found; no pane provider")
            .count(),
        "{warned:?}"
    );

    // a stale REX_SERVER: nothing listens
    let mut fx = fixture_unreachable("rex: can't reach Rex: connection refused");
    fx.d.nvim.ready = true;
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    assert!(fx.d.provider.is_none());
    let warned = sent_to_nvim(&mut fx);
    assert!(
        warned.contains("rex: can't reach Rex: connection refused; no pane provider"),
        "{warned:?}"
    );
}

#[test]
fn windows_wait_unshown_for_the_choice_then_open_beside_the_choosing_pane() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    split_1001_off_1000(&mut fx);
    assert!(fx.d.panes.by_win(1001).is_none(), "drawn before the choice");

    found(&mut fx.d, BLOCK);
    // opened beside the choosing pane's terminal, as a new window is
    let p = fx.d.panes.by_win(1001).expect("a pane of its own");
    assert!(!p.is_self_attached());
    let (_, Ask::OpenBeside { origin, dir, .. }) = last_open_beside(&mut fx.d) else {
        unreachable!()
    };
    assert_eq!((TerminalId::from(BLOCK), Direction::Right), (origin, dir));
}

#[test]
fn a_refused_choice_draws_the_waiting_windows_in_the_choosing_pane() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    split_1001_off_1000(&mut fx);
    not_found(&mut fx.d, "REX_BLOCK names another terminal");
    let p = fx.d.panes.by_id(id).unwrap();
    assert!(p.holds(1001), "not drawn in the choosing pane");
}

/// A daemon whose fake pane provider was chosen in Rex session `session:a`
/// and can `find`, a pane in its reach.
fn chosen_in_session_a() -> Fixture {
    let mut fake = FakeProvider::opening();
    fake.caps.insert(Capability::Find);
    fake.context = &["REX_SESSION"];
    let mut fx = with_provider(Some(Box::new(fake)));
    fx.d.context = vec![Some(b"session:a".to_vec())];
    fx.d.nvim.ready = true;
    // the choosing pane's, in reach, showing a window of its own
    fx.d.panes.add_for_window(900);
    fx
}

#[test]
fn a_self_attach_from_another_context_is_out_of_reach_unfound() {
    let mut fx = chosen_in_session_a();
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:b", BLOCK));
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    assert!(fake(&mut fx.d).asked().is_empty());
    // a window opened from it is drawn in it
    split_1001_off_1000(&mut fx);
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
    found(&mut fx.d, BLOCK);
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!(
        (Reach::In, Some(TerminalId::from(BLOCK))),
        (p.reach, p.terminal.clone())
    );
}

/// `rex_hello` with the hint `hint`, as `--provider` gives it.
fn rex_hello_hinting(session: &str, hint: &str) -> Vec<u8> {
    use crate::attach::{Hello, Message, Purpose};
    use crate::daemon::attach_conn::test_terminal;
    let mut terminal = test_terminal(80, 24);
    terminal.env = env_of(session, BLOCK);
    let purpose = Purpose::SelfAttach {
        terminal,
        provider: Some(hint.into()),
        file: None,
        line: None,
    };
    Message::Hello(Hello::new(purpose)).encode().unwrap()
}

#[test]
fn a_hint_from_a_self_attach_that_doesnt_choose_is_logged_as_ignored() {
    let mut fx = chosen_in_session_a();
    fx.d.name = "work".into();
    let hello = rex_hello_hinting("session:a", "tmux");
    let (_, lines) = crate::log::capturing::capture(|| attach_showing_1000(&mut fx, &hello));
    let ignored = format!("{NAME} daemon: hint tmux ignored: work keeps fake");
    assert!(lines.contains(&ignored), "{lines:?}");
}

#[test]
fn a_hint_from_a_self_attach_that_chooses_isnt_logged_as_ignored() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    fx.d.name = "work".into();
    let hello = rex_hello_hinting("session:a", "tmux");
    let (_, lines) = crate::log::capturing::capture(|| attach_showing_1000(&mut fx, &hello));
    assert!(lines.iter().any(|l| l.contains("chose fake")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("ignored")), "{lines:?}");
}

/// Neovim's redraw batch ends: a `flush`, as it sends one.
fn nvim_flushes(fx: &mut Fixture) {
    let mut buf = Vec::new();
    msgpack::write_array_header(&mut buf, 3);
    msgpack::write_int(&mut buf, 2);
    msgpack::write_str(&mut buf, "redraw");
    msgpack::write_array_header(&mut buf, 1);
    msgpack::write_array_header(&mut buf, 2);
    msgpack::write_str(&mut buf, "flush");
    msgpack::write_array_header(&mut buf, 0);
    fx.nvim_end.set_nonblocking(false).unwrap();
    fx.nvim_end.write_all(&buf).unwrap();
    fx.d.on_nvim().unwrap();
}

#[test]
fn a_warning_before_neovim_is_set_up_waits_for_it_to_draw() {
    // at a hit-enter prompt it would hide the first window until a key
    let mut fx = fixture_unreachable("rex: can't reach Rex: connection refused");
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    assert!(!sent_to_nvim(&mut fx).contains("can't reach Rex"));
    // set up, not drawn yet
    fx.d.nvim.ready = true;
    assert!(!sent_to_nvim(&mut fx).contains("can't reach Rex"));
    nvim_flushes(&mut fx);
    assert!(sent_to_nvim(&mut fx).contains("can't reach Rex"));
    // once
    nvim_flushes(&mut fx);
    assert!(!sent_to_nvim(&mut fx).contains("can't reach Rex"));
}

#[test]
fn a_self_attach_during_a_choice_waits_for_it_then_is_found() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let (first, _h1, _p1) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let (second, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    assert_eq!(Some(first), choosing_id(&fx.d));
    assert_eq!(Reach::Finding, fx.d.panes.by_id(second).unwrap().reach);
    // the waiting attach isn't asked about before the choice is confirmed
    assert_eq!(1, finds_asked(&mut fx.d));
    found(&mut fx.d, BLOCK);
    assert_eq!(None, choosing_id(&fx.d));
    // then its own find
    assert_eq!(2, finds_asked(&mut fx.d));
    let attach = fx.d.panes.by_id(second).unwrap().attach_id.unwrap();
    assert_eq!(attach, last_find(&mut fx.d).1);
    assert_eq!(Reach::Finding, fx.d.panes.by_id(second).unwrap().reach);
}

#[test]
fn a_self_attach_during_a_refused_choice_chooses_again() {
    let (mut fx, choices) = fixture_choosing(vec![finding(), finding()]);
    let (first, _h1, _p1) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let (second, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    not_found(&mut fx.d, "REX_BLOCK names another terminal");
    assert_eq!(Reach::Out, fx.d.panes.by_id(first).unwrap().reach);
    // the next waiting attach chooses, a pane provider of its own
    assert_eq!(Some(second), choosing_id(&fx.d));
    assert!(choices.borrow().is_empty());
    assert!(fx.d.provider.is_some());
    let attach = fx.d.panes.by_id(second).unwrap().attach_id.unwrap();
    assert_eq!(attach, last_find(&mut fx.d).1);
}

#[test]
fn a_choice_unanswered_is_refused_three_seconds_from_its_start() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    // the choice starts between these
    let before = Instant::now();
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let after = Instant::now();
    let _ = sent_to_nvim(&mut fx);
    fx.d.time_out_choice(before + Duration::from_millis(2900))
        .unwrap();
    assert!(fx.d.provider.is_some());
    fx.d.time_out_choice(after + Duration::from_secs(4))
        .unwrap();
    assert!(fx.d.provider.is_none());
    assert_eq!(None, choosing_id(&fx.d));
    let warned = sent_to_nvim(&mut fx);
    assert!(
        warned.contains("fake: no answer within 3 s; no pane provider"),
        "{warned:?}"
    );
}

#[test]
fn connecting_counts_against_the_choice_and_a_find_queued_behind_a_layout_act_doesnt_extend_it() {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    fx.d.test_choose = Some(Box::new(|_| Chosen::None {
        why: "not yet".into(),
        warn: false,
    }));
    let (id, _helper, _pty) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    // a layout act running: the find waits behind it
    fx.d.provider = Some(Box::new(finding()));
    fx.d.act(Ask::Close {
        terminal: TerminalId::from("block:old"),
    });
    let held = fx.d.provider.take().unwrap();
    // connecting took 2.5 s of the 3
    let now = Instant::now();
    fx.d.take_choice(
        id,
        Chosen::Provider {
            held,
            spent: Duration::from_millis(2500),
        },
    )
    .unwrap();
    assert_eq!(0, finds_asked(&mut fx.d));
    fx.d.time_out_choice(now + Duration::from_millis(600))
        .unwrap();
    assert!(fx.d.provider.is_none());
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
}

#[test]
fn a_waiting_self_attach_that_hangs_up_is_never_asked_about() {
    let (mut fx, _) = fixture_choosing(vec![finding()]);
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let (second, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    fx.d.pane_gone(second).unwrap();
    found(&mut fx.d, BLOCK);
    assert_eq!(None, choosing_id(&fx.d));
    assert!(fx.d.waiting_choice.is_empty());
    assert_eq!(1, finds_asked(&mut fx.d));
}

#[test]
fn a_pane_provider_gone_mid_choice_lets_the_waiting_self_attach_choose() {
    let (mut fx, _) = fixture_choosing(vec![finding(), finding()]);
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let (second, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    let _ = sent_to_nvim(&mut fx);
    fake(&mut fx.d).push(ProviderEvent::Gone {
        reason: "Rex hung up".into(),
    });
    fx.d.drain_provider().unwrap();
    assert_eq!(Some(second), choosing_id(&fx.d));
    assert!(sent_to_nvim(&mut fx).contains("no pane provider"));
    let attach = fx.d.panes.by_id(second).unwrap().attach_id.unwrap();
    assert_eq!(attach, last_find(&mut fx.d).1);
}

#[test]
fn self_attaches_waiting_for_a_refused_choice_take_their_turns_in_order() {
    let (mut fx, _) = fixture_choosing(vec![finding(), finding()]);
    let _first = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    let (second, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    let (third, _h3, _p3) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    not_found(&mut fx.d, "REX_BLOCK names another terminal");
    // the second chooses; the third waits for that choice
    assert_eq!(Some(second), choosing_id(&fx.d));
    assert_eq!(vec![third], fx.d.waiting_choice);
    assert_eq!(Reach::Finding, fx.d.panes.by_id(third).unwrap().reach);
}

// Dropping the pane provider, and choosing again (#115)

const BLOCK_2: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a52";
const BLOCK_3: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a53";

/// A daemon that chose a fake from a pane showing window 1000, in reach as
/// `BLOCK`, with a requested pane for window 1001 attached as `BLOCK_2`;
/// its next choice is a second fake. The two panes' ids, and what keeps
/// their terminals open.
fn chosen_with_a_requested_pane() -> (Fixture, [u32; 2], Vec<(UnixStream, Pty)>) {
    let (mut fx, first, h1, p1, pane) = chosen_with_a_requested_pane_opened();
    let h2 = attach_requested(&mut fx, pane);
    let requested = fx.d.panes.by_win(1001).unwrap().id;
    assert!(
        fx.d.panes
            .by_id(requested)
            .unwrap()
            .terminal_surface
            .is_some()
    );
    let _ = sent_to_nvim(&mut fx);
    (fx, [first, requested], vec![(h1, p1), h2])
}

/// As `chosen_with_a_requested_pane`, its terminal opened as `BLOCK_2` but
/// not attached yet: the choosing pane's id, what keeps it open, and the
/// requested pane's pane id.
fn chosen_with_a_requested_pane_opened() -> (Fixture, u32, UnixStream, Pty, crate::attach::PaneId) {
    let (mut fx, _) = fixture_choosing(vec![finding(), finding()]);
    let (first, h1, p1) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    found(&mut fx.d, BLOCK);
    split_1001_off_1000(&mut fx);
    let (ask, Ask::OpenBeside { pane, .. }) = last_open_beside(&mut fx.d) else {
        unreachable!()
    };
    fake(&mut fx.d).opened(ask, BLOCK_2);
    fx.d.drain_provider().unwrap();
    (fx, first, h1, p1, pane)
}

/// The terminal of requested pane `pane` attaches, from Rex block `BLOCK_2`.
fn attach_requested(fx: &mut Fixture, pane: crate::attach::PaneId) -> (UnixStream, Pty) {
    let at = Address::new(&format!("rejoin-{}-{}", fx.d.panes.list.len(), pane));
    let pty = Pty::new(40, 23);
    let h = at.send(
        &requested_rex_hello(pane, "session:s", BLOCK_2),
        Some(pty.slave),
    );
    fx.d.accept(&at.server).unwrap();
    (h, pty)
}

#[test]
fn a_requested_pane_attaching_after_a_new_choice_is_checked_and_rejoins() {
    let (mut fx, first, _h1, _p1, pane) = chosen_with_a_requested_pane_opened();
    // gone while the requested pane's terminal is on its way
    fake(&mut fx.d).push(ProviderEvent::Gone {
        reason: "Rex hung up".into(),
    });
    fx.d.drain_provider().unwrap();
    let requested = fx.d.panes.by_win(1001).unwrap().id;
    assert_eq!(Reach::Out, fx.d.panes.by_id(requested).unwrap().reach);
    // chosen again, and confirmed: the choosing pane rejoins
    let (_third, _h3, _p3) = attach_later(&mut fx, &rex_hello("session:s", BLOCK_3));
    found(&mut fx.d, BLOCK_3);
    assert_eq!(None, choosing_id(&fx.d));
    found(&mut fx.d, BLOCK);
    assert_eq!(Reach::In, fx.d.panes.by_id(first).unwrap().reach);
    // then the requested pane's terminal comes: it is checked as well
    let finds = finds_asked(&mut fx.d);
    let _h2 = attach_requested(&mut fx, pane);
    assert_eq!(finds + 1, finds_asked(&mut fx.d));
    assert_eq!(pane, last_find(&mut fx.d).1);
    assert_eq!(Reach::Finding, fx.d.panes.by_id(requested).unwrap().reach);
    found(&mut fx.d, BLOCK_2);
    let p = fx.d.panes.by_id(requested).unwrap();
    assert_eq!(
        (Reach::In, Some(TerminalId::from(BLOCK_2))),
        (p.reach, p.terminal.clone())
    );
}

#[test]
fn a_self_attach_while_the_pane_provider_has_only_closes_left_chooses_anew() {
    // a command runner whose close runs on: its act stays unanswered for
    // its ask timeout, and nothing is in its reach
    let mut fx = commanded_by(&[(Capability::Close, &["sleep", "30"])]);
    fx.d.act(Ask::Close {
        terminal: TerminalId::from("t1"),
    });
    assert!(fx.d.asks.busy());
    fx.d.test_choose = Some(Box::new(|_| Chosen::Provider {
        held: Box::new(finding()),
        spent: Duration::ZERO,
    }));
    let (id, _h, _p) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    // not checked against the command runner's context: a choice of its own
    assert_eq!("fake", fx.d.provider.as_ref().unwrap().name());
    assert_eq!(Some(id), choosing_id(&fx.d));
    assert_eq!(1, finds_asked(&mut fx.d));
}

#[test]
fn a_pane_provider_gone_with_asks_pending_falls_each_back_with_one_warning() {
    let (mut fx, [first, requested], _keep) = chosen_with_a_requested_pane();
    fx.d.provider
        .as_mut()
        .unwrap()
        .fake()
        .caps
        .insert(Capability::Focus);
    // an open, a close waiting behind it, and a focus
    let opened = opening_for(&mut fx.d, 1005);
    fx.d.act(Ask::Close {
        terminal: TerminalId::from(BLOCK_3),
    });
    fx.d.act(Ask::Focus {
        terminal: TerminalId::from(BLOCK),
    });
    assert_eq!(3, fx.d.asks.waiting());
    fake(&mut fx.d).push(ProviderEvent::Gone {
        reason: "Rex hung up".into(),
    });
    fx.d.drain_provider().unwrap();

    assert!(fx.d.provider.is_none());
    assert_eq!(0, fx.d.asks.waiting());
    assert!(fx.d.panes.by_id(opened).is_none(), "its open not given up");
    assert_eq!(None, fx.d.stopped);
    let warned = sent_to_nvim(&mut fx);
    assert_eq!(1, warned.matches("no pane provider").count(), "{warned:?}");
    assert_eq!(
        1,
        warned
            .matches("fake: Rex hung up; no pane provider")
            .count()
    );
    assert!(!warned.contains("failed"), "{warned:?}");
    // its terminal ids went with it, and its panes are out of reach
    for id in [first, requested] {
        let p = fx.d.panes.by_id(id).unwrap();
        assert_eq!((None, Reach::Out), (p.terminal.clone(), p.reach));
    }
}

#[test]
fn a_self_attach_after_a_pane_provider_gone_chooses_again_and_the_attached_panes_rejoin() {
    let (mut fx, [first, requested], _keep) = chosen_with_a_requested_pane();
    // what the first found unsupported isn't the next one's
    fake(&mut fx.d).caps.insert(Capability::Focus);
    fx.d.act(Ask::Focus {
        terminal: TerminalId::from(BLOCK),
    });
    let id = fake(&mut fx.d).last_id();
    fake(&mut fx.d).answer(id, Outcome::Unsupported);
    fx.d.drain_provider().unwrap();
    assert!(fx.d.asks.unsupported(Capability::Focus));
    fake(&mut fx.d).push(ProviderEvent::Gone {
        reason: "Rex hung up".into(),
    });
    fx.d.drain_provider().unwrap();
    assert!(fx.d.provider.is_none());
    assert!(!fx.d.asks.unsupported(Capability::Focus));

    // a new self-attach chooses again
    let (third, _h3, _p3) = attach_later(&mut fx, &rex_hello("session:s", BLOCK_3));
    assert_eq!(Some(third), choosing_id(&fx.d));
    assert_eq!(1, finds_asked(&mut fx.d));
    // the panes attached before wait for the choice, out of reach
    assert_eq!(Reach::Out, fx.d.panes.by_id(first).unwrap().reach);
    found(&mut fx.d, BLOCK_3);
    assert_eq!(None, choosing_id(&fx.d));

    // confirmed: each pane attached before is asked for in turn, and
    // rejoins
    for _ in 0..2 {
        let (_, pane) = last_find(&mut fx.d);
        let id = fx.d.panes.by_attach_id(&pane).unwrap().id;
        assert!([first, requested].contains(&id), "{id}");
        assert_eq!(Reach::Finding, fx.d.panes.by_id(id).unwrap().reach);
        found(&mut fx.d, if id == first { BLOCK } else { BLOCK_2 });
    }
    assert_eq!(3, finds_asked(&mut fx.d));
    let p = fx.d.panes.by_id(first).unwrap();
    assert_eq!(
        (Reach::In, Some(TerminalId::from(BLOCK))),
        (p.reach, p.terminal.clone())
    );
    let p = fx.d.panes.by_id(requested).unwrap();
    assert_eq!(
        (Reach::In, Some(TerminalId::from(BLOCK_2))),
        (p.reach, p.terminal.clone())
    );
}

#[test]
fn the_last_terminal_in_reach_going_drops_the_pane_provider() {
    let (mut fx, [first, requested], _keep) = chosen_with_a_requested_pane();
    // a pane from another Rex session: out of reach
    let (other, _h, _p) = attach_later(&mut fx, &rex_hello("session:b", BLOCK_3));
    assert_eq!(Reach::Out, fx.d.panes.by_id(other).unwrap().reach);
    let _ = sent_to_nvim(&mut fx);

    fx.d.pane_gone(requested).unwrap();
    fx.d.after_batch().unwrap();
    assert!(fx.d.provider.is_some(), "a terminal in reach is left");
    fx.d.pane_gone(first).unwrap();
    fx.d.after_batch().unwrap();
    assert!(fx.d.provider.is_none());
    assert_eq!(None, fx.d.stopped);
    // quietly: nothing failed
    assert!(!sent_to_nvim(&mut fx).contains("no pane provider"));

    // the next self-attach chooses again
    let (next, _h4, _p4) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    assert_eq!(Some(next), choosing_id(&fx.d));
}

#[test]
fn a_pane_provider_is_kept_while_a_terminal_in_its_reach_is_on_its_way() {
    let (mut fx, [first, requested], _keep) = chosen_with_a_requested_pane();
    fx.d.pane_gone(requested).unwrap();
    // a requested pane asked for, its terminal not attached
    let opened = opening_for(&mut fx.d, 1005);
    let id = fake(&mut fx.d).last_id();
    fake(&mut fx.d).opened(id, BLOCK_3);
    fx.d.drain_provider().unwrap();
    fx.d.pane_gone(first).unwrap();
    fx.d.after_batch().unwrap();
    assert!(fx.d.panes.by_id(opened).is_some());
    assert!(fx.d.provider.is_some());
}

#[test]
fn context_variables_compare_as_bytes_unset_matching_unset() {
    use crate::daemon::daemon::choosing::in_context;
    let env = |vars: &[(&str, &[u8])]| -> Vec<(Vec<u8>, Vec<u8>)> {
        vars.iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.to_vec()))
            .collect()
    };
    let vars = ["REX_SERVER", "REX_SESSION"];
    let chose = [Some(b"unix:///s".to_vec()), None];
    assert!(in_context(
        &vars,
        &chose,
        &env(&[("REX_SERVER", b"unix:///s")])
    ));
    // set where the choosing attach had it unset, even empty
    assert!(!in_context(
        &vars,
        &chose,
        &env(&[("REX_SERVER", b"unix:///s"), ("REX_SESSION", b"")])
    ));
    // a byte apart, and unset where it was set
    assert!(!in_context(
        &vars,
        &chose,
        &env(&[("REX_SERVER", b"unix:///S")])
    ));
    assert!(!in_context(&vars, &chose, &env(&[])));
    // raw bytes, not text
    let raw = [Some(b"\xff".to_vec())];
    assert!(in_context(&["X"], &raw, &env(&[("X", b"\xff")])));
    // none: every attach
    assert!(in_context(&[], &[], &env(&[("REX_SESSION", b"other")])));
}

// The pane title (#127)

/// What the daemon wrote to the terminal of pty `master`: until it holds
/// `until`, for at most 5 s; else what came before 50 ms passed quietly.
fn written_to(master: i32, until: Option<&[u8]>) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(want) = until
            && (out.windows(want.len()).any(|w| w == want) || Instant::now() >= deadline)
        {
            return out;
        }
        let mut fds = [libc::pollfd {
            fd: master,
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one valid pollfd.
        let ready =
            unsafe { libc::poll(fds.as_mut_ptr(), 1, if until.is_some() { 100 } else { 50 }) };
        if ready <= 0 {
            if until.is_none() {
                return out;
            }
            continue;
        }
        // SAFETY: reading into a buffer of its size.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
}

/// The frames `helper` was sent so far.
fn frames_sent(helper: &UnixStream) -> Vec<crate::attach::Message> {
    helper
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = (&*helper).read(&mut buf) {
        bytes.extend_from_slice(&buf[..n]);
    }
    let mut out = Vec::new();
    while let Some(m) = crate::daemon::attach_conn::next_message(&mut bytes).unwrap() {
        out.push(m);
    }
    out
}

/// A daemon whose fake pane provider can `find`, `wants_title` or not,
/// chosen already with no context variables. A self-attach to it: its
/// pane, its helper's end, its pty, and what its terminal had been sent
/// when `find` was asked.
fn self_attach_finding(wants_title: bool) -> (Fixture, u32, UnixStream, Pty, Rc<RefCell<Vec<u8>>>) {
    let mut f = FakeProvider::opening();
    f.caps.insert(Capability::Find);
    f.wants_title = wants_title;
    let pty = Pty::new(80, 24);
    let at_find = Rc::new(RefCell::new(Vec::new()));
    let (master, seen) = (pty.master, at_find.clone());
    f.on_ask = Some(Box::new(move |ask| {
        if let Ask::Find { title, .. } = ask {
            // what a title would be, were it written
            let set = crate::term::modes::set_title(title);
            let until = wants_title.then_some(set.as_slice());
            seen.borrow_mut().extend(written_to(master, until));
        }
    }));
    let mut fx = with_provider(Some(Box::new(f)));
    fx.d.nvim.ready = true;
    fx.d.panes.add_for_window(900);
    let at = Address::new(&format!("title-{wants_title}"));
    let helper = at.send(&rex_hello("session:s", BLOCK), Some(pty.slave));
    fx.d.accept(&at.server).unwrap();
    let id = fx.d.panes.list.last().unwrap().id;
    (fx, id, helper, pty, at_find)
}

#[test]
fn the_pane_title_is_written_before_find_runs_and_put_back_by_the_undo_bytes() {
    let (mut fx, id, helper, pty, at_find) = self_attach_finding(true);
    let attach = fx.d.panes.by_id(id).unwrap().attach_id.unwrap();
    let (_, Ask::Find { title, .. }) = last_ask(&mut fx.d) else {
        panic!("no find asked");
    };
    assert_eq!(format!("{NAME}-{}", attach.token()), title);
    // saved, then set, before `find` was asked
    let set = format!("\x1b[22;2t\x1b]2;{title}\x1b\\");
    let seen = String::from_utf8_lossy(&at_find.borrow()).into_owned();
    assert!(seen.contains(&set), "{seen:?}");
    // the undo bytes clear it and put the saved one back, in a new `u`
    use crate::attach::Message;
    let undos: Vec<Vec<u8>> = frames_sent(&helper)
        .into_iter()
        .filter_map(|m| match m {
            Message::Undo(u) => Some(u),
            _ => None,
        })
        .collect();
    let undo = crate::term::modes::undo();
    let with_title = [undo.clone(), b"\x1b]2;\x1b\\\x1b[23;2t".to_vec()].concat();
    assert_eq!(vec![undo, with_title], undos);

    // once per attach: a second `find` writes nothing more
    fx.d.drop_provider().unwrap();
    let mut f = FakeProvider::opening();
    f.caps.insert(Capability::Find);
    f.wants_title = true;
    fx.d.provider = Some(Box::new(f));
    // the window's pane, chosen in, keeps it
    fx.d.panes.by_win_mut(900).unwrap().reach = Reach::In;
    fx.d.choose_or_reach(id).unwrap();
    assert!(matches!(
        fake(&mut fx.d).asked().last(),
        Some(Ask::Find { .. })
    ));
    assert!(!String::from_utf8_lossy(&written_to(pty.master, None)).contains("\x1b]2;"));
    assert!(frames_sent(&helper).is_empty());
}

#[test]
fn a_pane_provider_whose_find_wants_no_title_gets_none_written() {
    // as Rex's built-in `find`
    let (mut fx, _id, helper, pty, at_find) = self_attach_finding(false);
    assert!(matches!(
        fake(&mut fx.d).asked().last(),
        Some(Ask::Find { .. })
    ));
    let mut seen = at_find.borrow().clone();
    seen.extend(written_to(pty.master, None));
    let seen = String::from_utf8_lossy(&seen).into_owned();
    assert!(
        !seen.contains("\x1b]2;") && !seen.contains("\x1b[22;2t"),
        "{seen:?}"
    );
    let undos = frames_sent(&helper)
        .into_iter()
        .filter(|m| matches!(m, crate::attach::Message::Undo(_)))
        .count();
    assert_eq!(1, undos);
}

// A requested terminal's `find`, and pairs (#127)

/// A fake pair: its terminal half opens and closes, its layout half finds
/// and places.
fn pair() -> FakeProvider {
    let mut f = FakeProvider::with(&[
        Capability::Open,
        Capability::Close,
        Capability::Find,
        Capability::PlaceBeside,
        Capability::Focus,
    ]);
    f.pair = true;
    f
}

/// A daemon that chose `fake` from a pane showing window 1000, found as
/// `L0`, then split window 1001 off it, opened by `open` to be placed: the
/// open's id, and what keeps the choosing pane open.
fn split_opened(fake: FakeProvider) -> (Fixture, AskId, crate::attach::PaneId, (UnixStream, Pty)) {
    let (mut fx, _) = fixture_choosing(vec![fake]);
    let (_first, h1, p1) = attach_showing_1000(&mut fx, &rex_hello("session:s", BLOCK));
    found(&mut fx.d, "L0");
    split_1001_off_1000(&mut fx);
    let (ask, Ask::Open { pane, .. }) = last_ask(&mut fx.d) else {
        panic!("not an open: {:?}", last_ask(&mut fx.d));
    };
    (fx, ask, pane, (h1, p1))
}

/// The pane provider is done with the last act asked.
fn placed(d: &mut Daemon) {
    let id = fake(d).last_id();
    fake(d).answer(id, Outcome::Done(Answer::Nothing));
    d.drain_provider().unwrap();
}

#[test]
fn a_pairs_close_takes_the_terminal_halfs_id_and_placement_the_layout_halfs() {
    let (mut fx, open, pane, _keep) = split_opened(pair());
    fake(&mut fx.d).opened(open, "T1");
    fx.d.drain_provider().unwrap();
    // `open`'s id places nothing: nothing is asked of it
    assert!(matches!(last_ask(&mut fx.d).1, Ask::Open { .. }));
    let id = fx.d.panes.by_win(1001).unwrap().id;
    assert_eq!(None, fx.d.panes.by_id(id).unwrap().terminal);

    // the layout half's `find` runs for it as it attaches, and its id places it
    let _h2 = attach_requested(&mut fx, pane);
    assert_eq!(pane, last_find(&mut fx.d).1);
    found(&mut fx.d, "L1");
    let Ask::PlaceBeside {
        terminal, origin, ..
    } = last_ask(&mut fx.d).1
    else {
        panic!("not placed: {:?}", last_ask(&mut fx.d));
    };
    assert_eq!(
        (TerminalId::from("L1"), TerminalId::from("L0")),
        (terminal, origin)
    );
    assert_eq!(Reach::In, fx.d.panes.by_id(id).unwrap().reach);
    placed(&mut fx.d);

    // its window closes: `close` takes the terminal half's id
    fx.d.give_up(id).unwrap();
    assert_eq!(
        Ask::Close {
            terminal: TerminalId::from("T1")
        },
        last_ask(&mut fx.d).1
    );
}

#[test]
fn a_pairs_find_runs_even_when_its_terminal_attached_before_open_answered() {
    let (mut fx, open, pane, _keep) = split_opened(pair());
    let _h2 = attach_requested(&mut fx, pane);
    // asked once the open is answered, layout acts going one at a time
    fake(&mut fx.d).opened(open, "T1");
    fx.d.drain_provider().unwrap();
    assert_eq!(pane, last_find(&mut fx.d).1);
    // once: the choosing pane's, and its
    assert_eq!(2, finds_asked(&mut fx.d));
}

#[test]
fn a_pair_whose_open_printed_no_id_closes_nothing() {
    let (mut fx, open, pane, _keep) = split_opened(pair());
    fake(&mut fx.d).answer(open, Outcome::Done(Answer::Nothing));
    fx.d.drain_provider().unwrap();
    let _h2 = attach_requested(&mut fx, pane);
    found(&mut fx.d, "L1");
    let id = fx.d.panes.by_win(1001).unwrap().id;
    let asked = fake(&mut fx.d).asks.len();
    fx.d.give_up(id).unwrap();
    assert_eq!(asked, fake(&mut fx.d).asks.len());
}

#[test]
fn a_requested_terminal_find_fails_for_is_unplaced_and_still_in_reach() {
    let (mut fx, open, pane, _keep) = split_opened(pair());
    fake(&mut fx.d).opened(open, "T1");
    fx.d.drain_provider().unwrap();
    let _h2 = attach_requested(&mut fx, pane);
    not_found(&mut fx.d, "exit 1");
    let id = fx.d.panes.by_win(1001).unwrap().id;
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!((Reach::In, None), (p.reach, p.terminal.clone()));
    assert_eq!(Area::Own, p.area);
    // never placed, the pane provider kept, with no warning
    assert!(matches!(last_ask(&mut fx.d).1, Ask::Find { .. }));
    assert!(fx.d.provider.is_some());
    assert!(!sent_to_nvim(&mut fx).contains("no pane provider"));
}

#[test]
fn a_whole_tables_requested_terminal_is_found_only_when_its_open_printed_no_id() {
    let mut whole = pair();
    whole.pair = false;
    // an id: no `find`
    let (mut fx, open, pane, _keep) = split_opened(whole);
    fake(&mut fx.d).opened(open, "T1");
    fx.d.drain_provider().unwrap();
    let _h2 = attach_requested(&mut fx, pane);
    assert_eq!(1, finds_asked(&mut fx.d));

    let mut whole = pair();
    whole.pair = false;
    let (mut fx, open, pane, _keep) = split_opened(whole);
    fake(&mut fx.d).answer(open, Outcome::Done(Answer::Nothing));
    fx.d.drain_provider().unwrap();
    let _h2 = attach_requested(&mut fx, pane);
    assert_eq!(pane, last_find(&mut fx.d).1);
    found(&mut fx.d, "T1");
    // its id places it, and closes it
    assert!(matches!(last_ask(&mut fx.d).1, Ask::PlaceBeside { .. }));
    placed(&mut fx.d);
    let id = fx.d.panes.by_win(1001).unwrap().id;
    fx.d.give_up(id).unwrap();
    assert_eq!(
        Ask::Close {
            terminal: TerminalId::from("T1")
        },
        last_ask(&mut fx.d).1
    );
}

// `find` as a command, one test per column of #93 decision 17's table:
// (a) confirming the choice, (b) a later self-attach, (c) a requested
// terminal. Each runs every row: a line, nothing, 69, and a failure.

/// The rows of decision 17's table, as `find` scripts.
const ROWS: [&str; 4] = ["echo L1", "exit 0", "exit 69", "echo no >&2; exit 1"];

/// A command runner whose `find` is `script`, in a pair if `pair`, its
/// context variable `CTX`; its `open` prints `T1`, and it can place and
/// close.
fn runner_finding(script: &Script, pair: bool) -> Box<dyn PaneProvider> {
    use crate::providers::command::{CapCommand, CommandProvider, Spec};
    let command = |cap, argv: &[&str]| CapCommand {
        cap,
        argv: argv.iter().map(|w| w.as_bytes().to_vec()).collect(),
        timeout: Duration::from_secs(3),
        protocol: 1,
    };
    let spec = Spec {
        commands: vec![
            command(Capability::Find, &["sh", script.path()]),
            command(Capability::Open, &["echo", "T1"]),
            command(Capability::PlaceBeside, &["true"]),
            command(Capability::Close, &["true"]),
        ],
        context_vars: vec!["CTX".into()],
        pair,
        ..Spec::named("cmd")
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let env = [
        (b"PATH".to_vec(), path.as_encoded_bytes().to_vec()),
        (b"CTX".to_vec(), b"a".to_vec()),
    ];
    Box::new(CommandProvider::new(spec, &env, b"/").unwrap())
}

/// A self-attach's hello from context `ctx`.
fn ctx_hello(ctx: &str) -> Vec<u8> {
    use crate::daemon::attach_conn::{self_hello, test_terminal};
    let mut t = test_terminal(80, 24);
    t.env = vec![(b"CTX".to_vec(), ctx.as_bytes().to_vec())];
    self_hello(t)
}

/// A daemon holding `p` as chosen in context `a`, watching it on its loop,
/// with a pane of its own in reach.
fn holding(p: Box<dyn PaneProvider>) -> Fixture {
    let mut fx = with_provider(Some(p));
    let fd = fx.d.provider.as_ref().and_then(|p| p.fd()).unwrap();
    fx.d.event_loop
        .add_read(fd, Token::Provider as usize)
        .unwrap();
    fx.d.context = vec![Some(b"a".to_vec())];
    fx.d.nvim.ready = true;
    fx.d.panes.add_for_window(900);
    fx
}

#[test]
fn find_confirming_the_choice_accepts_on_exit_0_or_69_and_refuses_otherwise() {
    for (row, script_text) in ROWS.iter().enumerate() {
        let script = Script::new("find-a");
        script.set(script_text);
        let mut fx = with_provider(None);
        fx.d.nvim.ready = true;
        let mut held = Some(runner_finding(&script, false));
        fx.d.test_choose = Some(Box::new(move |_| Chosen::Provider {
            held: held.take().unwrap(),
            spent: Duration::ZERO,
        }));
        let (id, _h, _p) = attach_showing_1000(&mut fx, &ctx_hello("a"));
        pump_until(&mut fx, |d| d.choosing.is_none());
        let kept = fx.d.provider.is_some();
        let p = fx.d.panes.by_id(id).unwrap();
        let (reach, terminal) = (p.reach, p.terminal.clone());
        match row {
            0 => assert_eq!(
                (true, Reach::In, Some(TerminalId::from("L1"))),
                (kept, reach, terminal)
            ),
            1 => assert_eq!((true, Reach::In, None), (kept, reach, terminal)),
            2 => {
                assert_eq!((true, Reach::In, None), (kept, reach, terminal));
                assert!(fx.d.asks.unsupported(Capability::Find));
            }
            _ => {
                assert_eq!((false, Reach::Out), (kept, reach));
                let warned = sent_to_nvim(&mut fx);
                assert!(
                    warned.contains("cmd: find didn't confirm this terminal (exit 1: no"),
                    "{warned:?}"
                );
            }
        }
    }
}

#[test]
fn find_for_a_later_self_attach_says_its_reach() {
    for (row, script_text) in ROWS.iter().enumerate() {
        let script = Script::new("find-b");
        script.set(script_text);
        let mut fx = holding(runner_finding(&script, false));
        let (id, _h, _p) = attach_later(&mut fx, &ctx_hello("a"));
        pump_until(&mut fx, |d| {
            d.panes.by_id(id).unwrap().reach != Reach::Finding
        });
        let p = fx.d.panes.by_id(id).unwrap();
        let (reach, terminal) = (p.reach, p.terminal.clone());
        match row {
            0 => assert_eq!((Reach::In, Some(TerminalId::from("L1"))), (reach, terminal)),
            1 => assert_eq!((Reach::In, None), (reach, terminal)),
            2 => {
                assert_eq!((Reach::In, None), (reach, terminal));
                // not run again until the next choice: context alone decides
                script.set("echo ran > \"$0.ran\"; echo L2");
                let (next, _h2, _p2) = attach_later(&mut fx, &ctx_hello("a"));
                assert_eq!(Reach::In, fx.d.panes.by_id(next).unwrap().reach);
                let (other, _h3, _p3) = attach_later(&mut fx, &ctx_hello("b"));
                assert_eq!(Reach::Out, fx.d.panes.by_id(other).unwrap().reach);
                assert!(!std::path::Path::new(&format!("{}.ran", script.path())).exists());
            }
            // out of reach: no warning, and the pane provider is kept only
            // while a terminal in its reach is left
            _ => {
                assert_eq!((Reach::Out, None), (reach, terminal));
                assert!(!sent_to_nvim(&mut fx).contains("find"));
            }
        }
    }
}

#[test]
fn a_context_variable_mismatch_skips_find() {
    let script = Script::new("find-ctx");
    script.set("echo ran > \"$0.ran\"; echo L1");
    let mut fx = holding(runner_finding(&script, false));
    let (id, _h, _p) = attach_later(&mut fx, &ctx_hello("b"));
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    assert!(!fx.d.asks.busy());
    // unset where the choosing attach had it set
    let (unset, _h2, _p2) = attach_later(&mut fx, &rex_hello("session:s", BLOCK));
    assert_eq!(Reach::Out, fx.d.panes.by_id(unset).unwrap().reach);
    assert!(!fx.d.asks.busy());
    assert!(!std::path::Path::new(&format!("{}.ran", script.path())).exists());
}

#[test]
fn find_for_a_requested_terminal_gives_its_id_or_none_and_never_puts_it_out_of_reach() {
    for (row, script_text) in ROWS.iter().enumerate() {
        let script = Script::new("find-c");
        script.set(script_text);
        // a pair: `find` runs for it as it attaches
        let mut fx = holding(runner_finding(&script, true));
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new(&format!("find-c-{row}"));
        let pty = Pty::new(80, 23);
        let _h = at.send(&hello_for(pane, 80, 23), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        assert!(fx.d.asks.asked(Capability::Find));
        pump_until(&mut fx, |d| !d.asks.busy());
        let p = fx.d.panes.by_id(id).unwrap();
        let terminal = (row == 0).then(|| TerminalId::from("L1"));
        assert_eq!((Reach::In, terminal), (p.reach, p.terminal.clone()));
        assert!(fx.d.provider.is_some());
        assert!(!sent_to_nvim(&mut fx).contains("no pane provider"));
    }
}
