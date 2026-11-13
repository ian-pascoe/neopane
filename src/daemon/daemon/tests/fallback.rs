//! Drawing a window in its origin pane when it gets no terminal of its own
//! (#112), and how a pane ends.

use super::*;
use crate::attach::{Ending, Hello, Message, Purpose, Refusal};
use crate::daemon::pane::SelfAttach;

/// The other ends of a pane's terminal and helper socket, held open so it
/// can be drawn.
pub(super) struct Live {
    pub(super) pty: Pty,
    helper: UnixStream,
}

/// A terminal of `cols` x `rows` handed over on a helper's socket, as an
/// attach brings it, and its other ends.
fn attached_surface(cols: u16, rows: u16) -> (Box<Surface>, Live) {
    let pty = Pty::new(cols, rows);
    let (socket, helper) = UnixStream::pair().unwrap();
    // SAFETY: a copy of the pty's slave, for the surface to own.
    let slave = unsafe { libc::dup(pty.slave) };
    let cells = crate::daemon::surface::cells(cols, rows);
    (Surface::create(socket, slave, cells), Live { pty, helper })
}

/// Pane `id` of window `win`, drawn, its terminal `terminal`, attached and
/// drawn into. Its id, and its terminal's other ends.
fn live_pane(fx: &mut Fixture, id: u32, win: i64, terminal: Option<&str>) -> (u32, Live) {
    draw_external(&mut fx.d, id + 2, win, 80, 23);
    let (surface, live) = attached_surface(80, 24);
    let mut p = Pane::from_neovim_window(id, win);
    p.terminal_surface = Some(surface);
    p.terminal = terminal.map(TerminalId::from);
    p.area = Area::Own;
    // attached by hand
    p.self_attached = true;
    (fx.d.panes.add(p).id, live)
}

/// Window `win` split off below window 1000, the window of pane 50 in
/// terminal t1, drawn and ready, and entered. Pane 50's id, and its ends.
fn split_off_origin(fx: &mut Fixture, win: i64) -> (u32, Live) {
    fx.d.nvim.ready = true;
    let origin = live_pane(fx, 50, 1000, Some("t1"));
    split_from(fx, win, 1000);
    origin
}

/// Window `win`, drawn, split off below `from`, and entered.
fn split_from(fx: &mut Fixture, win: i64, from: i64) {
    draw_external(&mut fx.d, win as u32 - 900, win, 80, 11);
    note_split_from(&mut fx.d, win, from, false, false);
    fx.d.nvim.current_win = win;
}

/// Window `win` is drawn in pane `origin`, below window 1000, and is the
/// window it shows.
fn assert_in_origin(d: &Daemon, origin: u32, win: i64) {
    let p = d.panes.by_win(win).unwrap();
    assert_eq!(origin, p.id);
    assert_eq!(Some(win), p.neovim_window_handle);
    let layout = p.layout.as_ref().unwrap();
    let laid = layout.lay_out([80, 23], win);
    let [above, below] = [1000, win].map(|w| laid.of(w).unwrap());
    assert!(above.row < below.row && above.col == below.col, "{laid:?}");
}

/// The open last asked fails with `outcome`.
fn open_ends(fx: &mut Fixture, outcome: Outcome) {
    let f = fake(&mut fx.d);
    f.answer(f.last_id(), outcome);
    fx.d.drain_provider().unwrap();
}

#[test]
fn a_failed_open_draws_its_window_in_its_origin_pane_and_warns() {
    let mut fx = fixture();
    let (origin, _live) = split_off_origin(&mut fx, 1002);
    fx.d.new_pane(1002).unwrap();
    let asked = fx.d.panes.by_win(1002).unwrap();
    let (asked, pane) = (asked.id, asked.attach_id.unwrap());
    assert_ne!(origin, asked);

    open_ends(&mut fx, Outcome::Failed("refused: no".into()));
    assert_in_origin(&fx.d, origin, 1002);
    assert_eq!(vec![origin], pane_ids(&fx.d));
    assert_eq!(Err(Refusal::PaneExpired), fx.d.panes.ids.check(&pane));
    assert_eq!(None, fx.d.stopped);
    let sent = sent_by(fx);
    assert_eq!(vec!["open_beside"], sent.capabilities());
    let warning = format!("{NAME}: fake: open_beside failed: refused: no");
    assert_eq!(1, sent.nvim.matches(&warning).count());
}

#[test]
fn unsupported_sends_that_window_and_later_ones_straight_to_their_origin_pane() {
    let mut fx = fixture();
    let (origin, _live) = split_off_origin(&mut fx, 1002);
    fx.d.new_pane(1002).unwrap();
    open_ends(&mut fx, Outcome::Unsupported);
    assert_in_origin(&fx.d, origin, 1002);

    // no pane minted, nothing asked
    let next_id = fx.d.panes.next_id;
    split_from(&mut fx, 1003, 1002);
    fx.d.new_pane(1003).unwrap();
    assert_eq!(Some(origin), fx.d.panes.by_win(1003).map(|p| p.id));
    assert_eq!(next_id, fx.d.panes.next_id);
    assert_eq!(vec!["open_beside"], sent_by(fx).capabilities());
}

#[test]
fn a_window_with_no_origin_goes_to_the_current_pane_with_no_pane_provider() {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    let _a = live_pane(&mut fx, 50, 1000, None);
    let (current, _b) = live_pane(&mut fx, 51, 2000, None);
    fx.d.nvim.current_win = 2000;
    draw_external(&mut fx.d, 103, 1003, 80, 11);
    note_split_from(&mut fx.d, 1003, 0, true, false);
    fx.d.new_pane(1003).unwrap();
    let p = fx.d.panes.by_win(1003).unwrap();
    assert_eq!(current, p.id);
    assert!(p.layout.as_ref().unwrap().contains(2000));
    assert_eq!(2, fx.d.panes.list.len());
}

/// A pane waiting for its terminal, `block:n`, opened for window 1002
/// beside pane 50's: the pane's id and pane id, when its wait ends, and pane
/// 50's ends.
fn opened_not_attached(fx: &mut Fixture) -> (u32, crate::attach::PaneId, Instant, Live) {
    let (_, live) = split_off_origin(fx, 1002);
    fx.d.new_pane(1002).unwrap();
    let p = fx.d.panes.by_win(1002).unwrap();
    let (id, pane) = (p.id, p.attach_id.unwrap());
    let f = fake(&mut fx.d);
    f.opened(f.last_id(), "block:n");
    fx.d.drain_provider().unwrap();
    let due = fx.d.panes.by_id(id).unwrap().attach_due.unwrap();
    (id, pane, due, live)
}

#[test]
fn the_attach_timeout_is_the_terminal_providers_five_seconds_from_the_open_at_most_sixty() {
    for (set, want) in [(None, 5), (Some(1), 1), (Some(90), 60)] {
        let mut fx = fixture();
        fake(&mut fx.d).attach_timeout = set.map(Duration::from_secs);
        let before = Instant::now();
        let (_, _, due, _live) = opened_not_attached(&mut fx);
        let want = Duration::from_secs(want);
        assert!(
            due >= before + want && due <= Instant::now() + want,
            "{set:?}"
        );
    }
}

#[test]
fn after_the_attach_timeout_the_window_is_in_its_origin_pane_with_a_warning_and_a_late_attach_is_closed_and_expired()
 {
    use crate::daemon::pane_helper::{self, Exit, Want};
    let mut fx = fixture();
    let (id, pane, due, _live) = opened_not_attached(&mut fx);
    fx.d.time_out_attaches(due - Duration::from_millis(1))
        .unwrap();
    assert!(fx.d.panes.by_id(id).is_some());
    assert_eq!(Ok(()), fx.d.panes.ids.check(&pane));

    fx.d.time_out_attaches(due).unwrap();
    assert!(fx.d.panes.by_id(id).is_none());
    assert_in_origin(&fx.d, 50, 1002);
    assert_eq!(
        Some(&asks::close(TerminalId::from("block:n"))),
        fake(&mut fx.d).asked().last()
    );
    assert_eq!(Err(Refusal::PaneExpired), fx.d.panes.ids.check(&pane));

    // its helper attaching late is refused, and exits 0
    let at = Address::new("late");
    let helper_end = UnixStream::connect(&at.path).unwrap();
    let helper = std::thread::spawn(move || {
        let pty = Pty::new(80, 24);
        let how = pane_helper::Attach::new(Want::Requested(pane), crate::attach::Transport::Fd);
        pane_helper::attach_on(helper_end, pty.slave, &how, None)
    });
    fx.d.accept(&at.server).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Some(&(token, _)) = fx.d.pending.first() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
        fx.d.on_pending(token).unwrap();
    }
    let exit: Exit = helper.join().unwrap();
    assert_eq!((0, None), (exit.status, exit.message));
    let sent = sent_by(fx);
    assert_eq!(
        1,
        sent.nvim
            .matches("terminal block:n didn't attach in time")
            .count()
    );
}

#[test]
fn a_terminal_attaching_in_time_stops_its_wait() {
    let mut fx = fixture();
    let (id, pane, due, _live) = opened_not_attached(&mut fx);
    let (_helper, _pty, _at) = attach_terminal(&mut fx, pane, 80, 11);
    assert_eq!(None, fx.d.panes.by_id(id).unwrap().attach_due);
    fx.d.time_out_attaches(due).unwrap();
    assert_eq!(Some(id), fx.d.panes.by_win(1002).map(|p| p.id));
}

#[test]
fn the_last_window_of_a_requested_pane_closing_closes_its_terminal() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    let _origin = live_pane(&mut fx, 50, 1000, Some("t1"));
    // attached, its window gone
    let p = fx.d.panes.add_for_window(1002);
    p.terminal = Some(TerminalId::from("block:n"));
    p.terminal_surface = Some(Surface::for_test(80, 11));
    fx.d.reconcile().unwrap();
    assert!(fx.d.panes.by_win(1002).is_none());
    assert_eq!(
        vec![asks::close(TerminalId::from("block:n"))],
        sent_by(fx).asks
    );
}

#[test]
fn a_self_attached_panes_last_window_closing_ends_its_attach_and_closes_no_terminal() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    let _origin = live_pane(&mut fx, 50, 1000, Some("t1"));
    let (surface, live) = attached_surface(80, 24);
    let terminal = Some(TerminalId::from("t0"));
    let id =
        fx.d.panes
            .add_self_attached(SelfAttach {
                surface,
                terminal,
                hello: None,
            })
            .id;
    fx.d.panes.by_id_mut(id).unwrap().neovim_window_handle = Some(1001);

    fx.d.reconcile().unwrap();
    assert!(fx.d.panes.by_id(id).is_none());
    let said = crate::daemon::attach_conn::read_all_frames(&live.helper);
    let Some(Message::End(z)) = said.last() else {
        panic!("{said:?}")
    };
    assert_eq!(Ending::Closed, z.as_ending());
    let asks = sent_by(fx).asks;
    assert!(asks.is_empty(), "{asks:?}");
}

#[test]
fn a_terminal_hanging_up_closes_every_window_its_pane_draws() {
    let mut fx = with_provider(None);
    let (origin, _live) = split_off_origin(&mut fx, 1002);
    let _other = live_pane(&mut fx, 51, 2000, None);
    fx.d.new_pane(1002).unwrap();
    assert_in_origin(&fx.d, origin, 1002);
    fx.d.pane_gone(origin).unwrap();
    assert_eq!(None, fx.d.stopped);
    assert!(fx.d.changes.is_closing(1000) && fx.d.changes.is_closing(1002));
    assert_eq!(2, sent_by(fx).name_closes());
}

#[test]
fn a_window_opened_unentered_is_drawn_in_its_origin_pane_unless_focus_can_be_given_back() {
    // whether the provider can focus, and the current pane's terminal id
    for (can_focus, current_id) in [(false, true), (true, false), (true, true)] {
        let mut fx = if can_focus { focusing() } else { fixture() };
        let (origin, _live) = split_off_origin(&mut fx, 1002);
        // without one, another terminal to open beside all the same
        let _other = (!current_id).then(|| {
            fx.d.panes.by_id_mut(origin).unwrap().terminal = None;
            live_pane(&mut fx, 51, 2000, Some("t2"))
        });
        fx.d.nvim.current_win = 1000;
        fx.d.new_pane(1002).unwrap();
        let p = fx.d.panes.by_win(1002).unwrap().id;
        let opened = can_focus && current_id;
        assert_eq!(opened, p != origin, "{can_focus} {current_id}");
    }
}

#[test]
fn two_splits_in_one_flush_each_open_a_terminal_though_the_current_ones_id_is_on_its_way() {
    let mut fx = focusing();
    let (origin, _live) = split_off_origin(&mut fx, 1002);
    // two splits of the first, as `:split | wincmd p | split`: the second
    // is current, and neither has a pane. A split of the first split would
    // wait for its terminal (SPI-1157).
    split_from(&mut fx, 1003, 1000);
    fx.d.reconcile().unwrap();
    let [a, b] = [1002, 1003].map(|w| fx.d.panes.by_win(w).unwrap().id);
    assert!(a != origin && b != origin && a != b);
    // one asked, one waiting its turn
    assert_eq!(2, fx.d.asks.waiting());

    // another, unentered, while the current one's open is on its way
    draw_external(&mut fx.d, 104, 1004, 80, 11);
    note_split_from(&mut fx.d, 1004, 1000, false, false);
    fx.d.new_pane(1004).unwrap();
    assert_ne!(Some(origin), fx.d.panes.by_win(1004).map(|p| p.id));
    assert_eq!(3, fx.d.asks.waiting());
}

/// A hello for `pane` from a build of another attach protocol.
fn hello_of_another_version(pane: crate::attach::PaneId) -> Vec<u8> {
    let mut hello = Hello::new(Purpose::Requested {
        pane,
        terminal: crate::daemon::attach_conn::test_terminal(80, 11),
    });
    hello.v = 2;
    hello.build = format!("{NAME} 9.9.9");
    Message::Hello(hello).encode().unwrap()
}

#[test]
fn after_a_version_refusal_new_windows_are_drawn_in_their_origin_panes_until_the_exe_changes() {
    let dir = std::env::temp_dir().join(format!("{NAME}-exe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("exe");
    std::fs::write(&exe, "new").unwrap();
    let mut fx = fixture();
    fx.d.exe = exe.clone();
    let (origin, _live) = split_off_origin(&mut fx, 1002);
    fx.d.new_pane(1002).unwrap();
    let pane = fx.d.panes.by_win(1002).unwrap().attach_id.unwrap();
    let f = fake(&mut fx.d);
    f.opened(f.last_id(), "block:n");
    fx.d.drain_provider().unwrap();
    let at = Address::new("installed");
    let helper = at.send(&hello_of_another_version(pane), None);
    fx.d.accept(&at.server).unwrap();
    assert_eq!(Some(Refusal::Version), refusal(&helper).as_refusal());
    assert_in_origin(&fx.d, origin, 1002);

    // windows go straight to their origin pane, for as long as it's the same file
    for win in [1003, 1004] {
        split_from(&mut fx, win, 1000);
        fx.d.new_pane(win).unwrap();
        assert_eq!(Some(origin), fx.d.panes.by_win(win).map(|p| p.id));
    }
    // its terminal closed, and nothing opened
    let closed = asks::close(TerminalId::from("block:n"));
    assert_eq!(Some(&closed), fake(&mut fx.d).asked().last());
    assert_eq!(2, fake(&mut fx.d).asks.len());
    let f = fake(&mut fx.d);
    f.answer(f.last_id(), Outcome::Done(Answer::Nothing));
    fx.d.drain_provider().unwrap();

    // installed again
    let new = dir.join(".exe.new");
    std::fs::write(&new, "newer").unwrap();
    std::fs::rename(&new, &exe).unwrap();
    split_from(&mut fx, 1005, 1000);
    fx.d.new_pane(1005).unwrap();
    assert_ne!(Some(origin), fx.d.panes.by_win(1005).map(|p| p.id));
    let _ = std::fs::remove_dir_all(&dir);
    let sent = sent_by(fx);
    assert_eq!(
        vec!["open_beside", "close", "open_beside"],
        sent.capabilities()
    );
    assert_eq!(
        1,
        sent.nvim
            .matches("drawn in their origin panes until")
            .count()
    );
}

/// Runs `d`'s loop, as `run` does, until `done`. Panics after 20 s.
pub(super) fn pump(d: &mut Daemon, done: impl Fn(&Daemon) -> bool) {
    const PUMP_TIMER: usize = usize::MAX;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut events = [Event::default(); 16];
    while !done(d) {
        assert!(Instant::now() < deadline, "timed out");
        d.event_loop.add_timer(100, 50, PUMP_TIMER).unwrap();
        let n = d.event_loop.wait(&mut events).unwrap().len();
        for &e in &events[..n] {
            if e.token != PUMP_TIMER {
                d.handle(e).unwrap();
            }
        }
        assert_eq!(None, d.stopped);
        d.after_batch().unwrap();
    }
}

/// A daemon over a real Neovim started with `flags`, such as `-o3`, and
/// `provider`, its first pane attached in terminal t0 and read: the daemon,
/// the terminal's other ends, and its reader. None without Neovim.
pub(super) fn started_with(
    flags: &[&str],
    provider: Option<Box<dyn PaneProvider>>,
) -> Option<(Daemon, Live, std::thread::JoinHandle<()>)> {
    use crate::nvim::nvim::spawn_for_test_with;
    let argv = [&["nvim", "--clean", "--embed", "-n"][..], flags].concat();
    let nvim = spawn_for_test_with(&argv)?;
    let event_loop = Loop::init().unwrap();
    event_loop
        .add_read(nvim.read_fd(), Token::NvimChannel as usize)
        .unwrap();
    let mut d = Daemon::new(
        event_loop,
        NvimSession::new(nvim),
        provider,
        std::path::PathBuf::new(),
        crate::daemon::dir::Address::from_arg(Default::default()),
    );
    let (surface, live) = attached_surface(120, 40);
    // what the daemon draws, read so it never waits
    // SAFETY: a copy of the pty's master, for the reader to own.
    let master = unsafe { libc::dup(live.pty.master) };
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        // SAFETY: `buf` is valid for its length.
        while unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
        // SAFETY: the reader's copy.
        unsafe { libc::close(master) };
    });
    let terminal = Some(TerminalId::from("t0"));
    d.start(&mut Some(SelfAttach {
        surface,
        terminal,
        hello: None,
    }))
    .unwrap();
    Some((d, live, reader))
}

#[test]
fn with_no_pane_provider_windows_opened_with_o_or_capital_o_are_laid_out_in_the_first_pane_as_neovim_had_them()
 {
    for (flag, across) in [("-o3", false), ("-O3", true)] {
        let Some((mut d, live, reader)) = started_with(&[flag], None) else {
            return;
        };
        pump(&mut d, |d| {
            d.nvim.ready
                && d.panes
                    .list
                    .iter()
                    .map(|p| p.windows().len())
                    .sum::<usize>()
                    == 3
        });
        assert_eq!(1, d.panes.list.len(), "{flag}");
        let p = &d.panes.list[0];
        let laid = p.layout.as_ref().unwrap().lay_out([120, 39], 1002);
        // in Neovim's order: it makes them last first, and enters the first
        let at = [1002, 1001, 1000].map(|w| laid.of(w).unwrap());
        for pair in at.windows(2) {
            let [a, b] = [pair[0], pair[1]];
            if across {
                assert!(a.col < b.col && a.row == b.row, "{flag}: {laid:?}");
            } else {
                assert!(a.row < b.row && a.col == b.col, "{flag}: {laid:?}");
            }
        }
        drop(d);
        drop(live);
        reader.join().unwrap();
    }
}

#[test]
fn with_a_pane_provider_windows_opened_with_o_or_capital_o_open_where_neovims_layout_has_them() {
    for (flag, want) in [("-o3", Direction::Down), ("-O3", Direction::Right)] {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::Focus);
        let Some((mut d, live, reader)) = started_with(&[flag], Some(Box::new(f))) else {
            return;
        };
        let open = |a: &Ask| matches!(a, Ask::OpenBeside { .. });
        pump(&mut d, |d| {
            d.provider
                .as_ref()
                .is_some_and(|p| p.fake_ref().asked().iter().any(open))
        });
        // the first asked, the next waiting their turn, beside the first
        // pane the way Neovim split them (ADR 0009)
        let asked = fake(&mut d).asked();
        let Some(Ask::OpenBeside { origin, dir, .. }) = asked.iter().find(|a| open(a)) else {
            unreachable!()
        };
        assert_eq!(("t0", want), (origin.as_str(), *dir), "{flag}");
        drop(d);
        drop(live);
        reader.join().unwrap();
    }
}

/// A self-attached pane in terminal `terminal`, with no window yet. Its id,
/// and its terminal's other ends.
fn windowless_pane(fx: &mut Fixture, terminal: &str) -> (u32, Live) {
    let (surface, live) = attached_surface(80, 24);
    let terminal = Some(TerminalId::from(terminal));
    let p = fx.d.panes.add_self_attached(SelfAttach {
        surface,
        terminal,
        hello: None,
    });
    p.area = Area::Own;
    (p.id, live)
}

#[test]
fn a_pane_with_no_window_yet_is_no_origin_pane() {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    let _windowless = windowless_pane(&mut fx, "t0");
    let (shown, _live) = live_pane(&mut fx, 51, 2000, None);
    split_from(&mut fx, 1003, 0);
    fx.d.new_pane(1003).unwrap();
    assert_eq!(Some(shown), fx.d.panes.by_win(1003).map(|p| p.id));
}

#[test]
fn a_window_whose_open_failed_with_no_pane_to_join_waits_for_one_unasked() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    let (origin, _live) = windowless_pane(&mut fx, "t0");
    split_from(&mut fx, 1002, 0);
    fx.d.reconcile().unwrap();
    let asked = fx.d.panes.by_win(1002).unwrap().id;
    open_ends(&mut fx, Outcome::Failed("refused: no".into()));
    assert!(fx.d.panes.by_id(asked).is_none());
    assert!(fx.d.panes.by_win(1002).is_none());

    // flushes ask nothing more while no pane can take it
    fx.d.reconcile().unwrap();
    assert_eq!(1, fake(&mut fx.d).asks.len());
    assert!(fx.d.panes.by_win(1002).is_none());

    // a pane that can, once it shows a window
    draw_external(&mut fx.d, 100, 1000, 80, 23);
    fx.d.panes.by_id_mut(origin).unwrap().neovim_window_handle = Some(1000);
    fx.d.reconcile().unwrap();
    assert_eq!(Some(origin), fx.d.panes.by_win(1002).map(|p| p.id));
    assert!(fx.d.unshown_awaiting_origin.is_empty());
    assert_eq!(1, sent_by(fx).asks.len());
}

#[test]
fn giving_up_the_last_pane_goes_unattached_as_its_last_pane_gone() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    draw_external(&mut fx.d, 102, 1002, 80, 23);
    let id = fx.d.panes.add_for_window(1002).id;
    fx.d.give_up(id).unwrap();
    assert!(fx.d.panes.list.is_empty());
    assert!(deciding(&fx.d));
}

#[test]
fn a_pane_taking_the_window_kept_from_a_later_tab_page_lays_its_splits_out_beside_it() {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    // the last pane, showing tab page 7's window, goes: its window is kept
    let (last, _a) = live_pane(&mut fx, 50, 1001, None);
    fx.d.panes.by_id_mut(last).unwrap().shown_tab = 7;
    fx.d.nvim.current_win = 1001;
    fx.d.go_unattached(Some(last)).unwrap();
    let kept = super::lifetime::Kept { win: 1001, tab: 7 };
    assert_eq!(Some(kept), fx.d.lifetime.kept);

    // the next self-attach takes it, and shows its tab page
    let (id, _b) = windowless_pane(&mut fx, "t0");
    fx.d.take_unshown(id, 1001, &[1001]);
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!((7, Some(1001)), (p.shown_tab, p.neovim_window_handle));

    // `:vsplit` there: side by side, not another tab page's layout
    draw_external(&mut fx.d, 102, 1002, 80, 23);
    fx.d.changes.note_split(crate::nvim::windows::Split {
        win: 1002,
        side_by_side: true,
        before: true,
        from: 1001,
        tab: 7,
    });
    fx.d.new_pane(1002).unwrap();
    let p = fx.d.panes.by_id(id).unwrap();
    assert!(p.held.is_empty(), "{:?}", p.held);
    assert_eq!(vec![1002, 1001], p.layout.as_ref().unwrap().windows());
}

/// Window `win`, drawn, split off `from` side by side in tab page `tab`
/// (as split notes name it), with no pane yet.
fn split_in_tab(fx: &mut Fixture, win: i64, from: i64, tab: i64) {
    draw_external(&mut fx.d, win as u32 - 900, win, 80, 23);
    fx.d.changes.note_split(crate::nvim::windows::Split {
        win,
        side_by_side: true,
        before: false,
        from,
        tab,
    });
}

#[test]
fn a_self_attach_opening_a_file_on_a_daemon_kept_on_a_later_tab_page_lays_its_splits_out_beside_it()
{
    use super::self_attach::{AskKind, WindowAsk};
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    draw_external(&mut fx.d, 101, 1001, 80, 23);
    fx.d.lifetime.kept = Some(super::lifetime::Kept { win: 1001, tab: 2 });
    fx.d.nvim.current_win = 1001;
    // its file opens in a new window, in the current tab page
    let (id, _live) = windowless_pane(&mut fx, "t0");
    fx.d.window_ask = Some(WindowAsk {
        pane: id,
        req: 1,
        kind: AskKind::Open,
    });
    fx.d.on_note(crate::nvim::windows::Note::Opened(1002))
        .unwrap();
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!((2, Some(1002)), (p.shown_tab, p.neovim_window_handle));

    split_in_tab(&mut fx, 1003, 1002, 2);
    fx.d.new_pane(1003).unwrap();
    let p = fx.d.panes.by_id(id).unwrap();
    assert!(p.held.is_empty(), "{:?}", p.held);
    assert_eq!(vec![1002, 1003], p.layout.as_ref().unwrap().windows());
}

#[test]
fn a_self_attach_taking_an_unshown_window_shows_the_tab_page_its_split_note_names() {
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    split_in_tab(&mut fx, 1005, 0, 4);
    let (id, _live) = windowless_pane(&mut fx, "t0");
    fx.d.take_unshown(id, 1005, &[1005]);
    let p = fx.d.panes.by_id(id).unwrap();
    assert_eq!((4, Some(1005)), (p.shown_tab, p.neovim_window_handle));
}
