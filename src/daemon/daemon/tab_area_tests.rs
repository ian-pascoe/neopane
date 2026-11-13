//! A tab page gets its tab page area when it first becomes current (ADR
//! 0012, SPI-657), with a real Neovim and a fake pane provider: the tab pages
//! Neovim started with, later windows of a tab page, pairs, and `<C-w>T`.

use super::self_attach_tests::{Rig, remove, three_files};
use super::*;
use crate::daemon::provider::fake::FakeProvider;

/// Neovim started with `args`, `fake` its pane provider, the first pane's
/// terminal `t0`; once it is set up and the first window shows. None when
/// Neovim isn't installed.
fn start(args: &[&str], fake: FakeProvider, files: &[String]) -> Option<Rig> {
    let Some(mut rig) = Rig::start_with(args, Some(Box::new(fake)), Some(TerminalId::from("t0")))
    else {
        remove(files);
        return None;
    };
    rig.files = files.to_vec();
    rig.pump(|r| r.d.nvim.ready);
    rig.win_of(1);
    Some(rig)
}

/// `-p` with the files of test `tag`: `alpha`, `bravo` and `charlie`, or
/// the first `n`.
fn tabs(tag: &str, n: usize, fake: FakeProvider) -> Option<(Rig, Vec<i64>)> {
    let files = three_files(tag);
    let mut args = vec!["-p"];
    args.extend(files[..n].iter().map(String::as_str));
    let mut rig = start(&args, fake, &files)?;
    let wins = rig.wins_on(&files[..n]);
    Some((rig, wins))
}

/// Runs the daemon's loop for `ms`, so whatever is coming comes.
fn settle(rig: &mut Rig, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    rig.pump(|_| Instant::now() >= until);
}

/// Has Neovim run Lua `code`, through the daemon's own channel, as a key
/// the daemon sends it would.
fn lua(rig: &mut Rig, code: &str) {
    let _ = rig.d.nvim.client.exec_lua(code, 0);
    rig.d.nvim.client.flush().unwrap();
}

fn asked(rig: &mut Rig) -> Vec<Ask> {
    rig.d.provider.as_mut().unwrap().fake().asked()
}

fn capabilities(rig: &mut Rig) -> Vec<&'static str> {
    asked(rig).iter().map(|a| a.capability().name()).collect()
}

/// Pumps until Lua `code`, returning a list of ints, returns `want`.
fn until_lua(rig: &mut Rig, code: &str, want: &[i64]) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if rig.ints(code) == want {
            return;
        }
        assert!(Instant::now() < deadline, "{code}: {:?}", rig.ints(code));
        settle(rig, 50);
    }
}

#[test]
fn only_the_current_tab_pages_windows_open_at_start_and_gt_asks_open_tab_page_once() {
    let Some((mut rig, wins)) = tabs("tp-start", 3, FakeProvider::opening()) else {
        return;
    };
    settle(&mut rig, 300);
    assert!(asked(&mut rig).is_empty(), "{:?}", asked(&mut rig));
    assert_eq!(1, rig.d.panes.list.len());

    lua(&mut rig, "vim.cmd('tabnext')");
    rig.pump(|r| !r.d.provider.as_ref().unwrap().fake_ref().asks.is_empty());
    settle(&mut rig, 300);
    assert_eq!(vec!["open_tab_page"], capabilities(&mut rig));
    let Ask::OpenTabPage { origin, .. } = &asked(&mut rig)[0] else {
        unreachable!()
    };
    // from the pane current before
    assert_eq!("t0", origin.as_str());
    let p = rig.d.panes.by_win(wins[1]).expect("a pane for b's window");
    assert_ne!(0, p.tab);

    // back, and there again: once only
    lua(&mut rig, "vim.cmd('tabprevious')");
    settle(&mut rig, 200);
    lua(&mut rig, "vim.cmd('tabnext')");
    settle(&mut rig, 300);
    assert_eq!(vec!["open_tab_page"], capabilities(&mut rig));
}

#[test]
fn a_later_window_of_a_startup_tab_page_waits_for_its_areas_first_terminal() {
    let Some((mut rig, _)) = tabs("tp-later", 2, FakeProvider::opening()) else {
        return;
    };
    lua(&mut rig, "vim.cmd('tabnext')");
    rig.pump(|r| !r.d.provider.as_ref().unwrap().fake_ref().asks.is_empty());
    let open = rig.d.provider.as_mut().unwrap().fake().last_id();
    lua(&mut rig, "vim.cmd('vsplit')");
    settle(&mut rig, 300);
    assert_eq!(vec!["open_tab_page"], capabilities(&mut rig));

    rig.d.provider.as_mut().unwrap().fake().opened(open, "t5");
    rig.d.drain_provider().unwrap();
    rig.pump(|r| r.d.provider.as_ref().unwrap().fake_ref().asks.len() > 1);
    let asks = asked(&mut rig);
    let [_, Ask::OpenBeside { origin, .. }] = &asks[..] else {
        panic!("not opened beside: {asks:?}")
    };
    assert_eq!("t5", origin.as_str());
}

#[test]
fn in_a_pair_tab_pages_are_drawn_in_their_pane() {
    let mut fake =
        FakeProvider::with(&[Capability::Open, Capability::PlaceBeside, Capability::Close]);
    fake.pair = true;
    let Some((mut rig, wins)) = tabs("tp-pair", 2, fake) else {
        return;
    };
    lua(&mut rig, "vim.cmd('tabnext')");
    rig.pump(|r| {
        r.d.panes
            .by_id(1)
            .is_some_and(|p| p.neovim_window_handle == Some(wins[1]))
    });
    // a tab page made later, too
    lua(&mut rig, "vim.cmd('tabnew')");
    rig.pump(|r| {
        r.d.panes
            .by_id(1)
            .and_then(|p| p.neovim_window_handle)
            .is_some_and(|w| !wins.contains(&w))
    });
    settle(&mut rig, 300);
    assert!(asked(&mut rig).is_empty(), "{:?}", asked(&mut rig));
    assert_eq!(1, rig.d.panes.list.len());
    let p = rig.d.panes.by_id(1).unwrap();
    // each tab page its own layout there: the first, b's and the new one's
    let mut shown: Vec<i64> = p.held.iter().map(|h| h.tab).collect();
    shown.push(p.shown_tab);
    shown.sort();
    shown.dedup();
    assert_eq!(3, shown.len(), "{shown:?}");
    assert_eq!(0, shown[0]);
}

/// The tab page count, whether window `win` is still valid, and the
/// current window's buffer.
fn after_move(win: i64) -> String {
    format!(
        "return {{ vim.fn.tabpagenr('$'), vim.api.nvim_win_is_valid({win}) and 1 or 0, \
         vim.api.nvim_get_current_buf() }}"
    )
}

#[test]
fn ctrl_w_capital_t_moves_a_window_to_a_new_tab_page_and_refuses_a_tab_pages_only_window() {
    let files = three_files("tp-move");
    let Some(mut rig) = start(&[&files[0]], FakeProvider::opening(), &files) else {
        return;
    };
    let first = rig.win_of(1);
    let buf = rig.ints("return { vim.api.nvim_get_current_buf() }")[0];

    // its tab page's only window: refused
    lua(&mut rig, "vim.cmd.normal(vim.keycode('<C-w>T'))");
    settle(&mut rig, 200);
    assert_eq!(vec![1, 1, buf], rig.ints(&after_move(first)));
    assert!(asked(&mut rig).is_empty());

    lua(&mut rig, "vim.cmd('vsplit')");
    rig.pump(|r| r.d.panes.list.len() == 2);
    let split = rig.ints("return { vim.api.nvim_get_current_win() }")[0];
    assert_ne!(first, split);
    lua(&mut rig, "vim.cmd.normal(vim.keycode('<C-w>T'))");
    until_lua(&mut rig, &after_move(split), &[2, 0, buf]);
    rig.pump(|r| r.d.provider.as_ref().unwrap().fake_ref().asks.len() > 1);
    settle(&mut rig, 300);
    // a new window, with its own place
    let moved = rig.ints("return { vim.api.nvim_get_current_win() }")[0];
    assert!(moved != first && moved != split);
    assert_eq!(
        vec!["open_beside", "open_tab_page"],
        capabilities(&mut rig)[..2]
    );
    // the old one's pane is gone
    rig.pump(|r| r.d.panes.by_win(split).is_none());

    // the new tab page's only window: refused too
    lua(&mut rig, "vim.cmd.normal(vim.keycode('<C-w>T'))");
    settle(&mut rig, 200);
    assert_eq!(vec![2, 1, buf], rig.ints(&after_move(moved)));
}
