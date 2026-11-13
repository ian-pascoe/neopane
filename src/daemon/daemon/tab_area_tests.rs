//! A tab page gets its tab page area when it first becomes current (ADR
//! 0012, SPI-657), with a real Neovim and a fake pane provider: the tab pages
//! Neovim started with, later windows of a tab page, pairs, and `<C-w>T`.
//!
//! Nothing asked is shown by a later act whose ask is waited for: the asks
//! before it are all there are.

use super::self_attach_tests::Rig;
use super::*;
use crate::daemon::provider::fake::FakeProvider;
use crate::daemon::provider::{Answer, Outcome};

/// `-p` with the first `n` files of test `tag`, `alpha`, `bravo` and
/// `charlie`, and `fake` the pane provider; once Neovim is set up and the
/// first window shows. The window on each file. None when Neovim isn't
/// installed.
fn tabs(tag: &str, n: usize, fake: FakeProvider) -> Option<(Rig, Vec<i64>)> {
    let (mut rig, files) = Rig::tabs(tag, n, Some(Box::new(fake)))?;
    rig.pump(|r| r.d.nvim.ready);
    rig.win_of(1);
    let wins = rig.wins_on(&files);
    Some((rig, wins))
}

/// Runs Lua `code` in Neovim and waits until it has run, failing on an
/// error in it.
fn lua(rig: &mut Rig, code: &str) {
    rig.ints(&format!("{code}\nreturn {{}}"));
}

fn asked(rig: &Rig) -> Vec<Ask> {
    rig.d.provider.as_ref().unwrap().fake_ref().asked()
}

/// The capabilities asked, in order.
fn capabilities(rig: &Rig) -> Vec<&'static str> {
    asked(rig).iter().map(|a| a.capability().name()).collect()
}

/// Pumps until `n` asks were asked.
fn until_asked(rig: &mut Rig, n: usize) {
    rig.pump(|r| r.d.provider.as_ref().unwrap().fake_ref().asks.len() >= n);
}

/// Answers the last open asked with terminal `t`.
fn answer_last(rig: &mut Rig, t: &str) {
    let f = rig.d.provider.as_mut().unwrap().fake();
    let (id, _) = f
        .asks
        .iter()
        .rev()
        .find(|(_, a)| !matches!(a, Ask::Close { .. }))
        .expect("an open");
    f.opened(*id, t);
    rig.d.drain_provider().unwrap();
}

/// Pumps until `win` is Neovim's current window, as the daemon was told.
fn until_current(rig: &mut Rig, win: i64) {
    rig.pump(|r| r.d.nvim.current_win == win);
}

/// The origin of open `ask`.
fn origin(ask: &Ask) -> &str {
    match ask {
        Ask::OpenBeside { origin, .. } | Ask::OpenTabPage { origin, .. } => origin.as_str(),
        a => panic!("not an open: {a:?}"),
    }
}

#[test]
fn only_the_current_tab_pages_windows_open_at_start_and_gt_asks_open_tab_page_once() {
    let Some((mut rig, wins)) = tabs("tp-start", 3, FakeProvider::opening()) else {
        return;
    };
    lua(&mut rig, "vim.cmd('tabnext')");
    until_asked(&mut rig, 1);
    // the first ask: nothing was asked at start
    assert_eq!(vec!["open_tab_page"], capabilities(&rig));
    // from the pane current before
    assert_eq!("t0", origin(&asked(&rig)[0]));
    let p = rig.d.panes.by_win(wins[1]).expect("a pane for b's window");
    assert_ne!(0, p.tab);
    answer_last(&mut rig, "t5");

    // back, and there again: asks nothing more, as a split in the first tab
    // page, asked after, shows
    lua(&mut rig, "vim.cmd('tabprevious')");
    until_current(&mut rig, wins[0]);
    lua(&mut rig, "vim.cmd('tabnext')");
    until_current(&mut rig, wins[1]);
    lua(&mut rig, "vim.cmd('tabprevious')");
    until_current(&mut rig, wins[0]);
    lua(&mut rig, "vim.cmd('vsplit')");
    until_asked(&mut rig, 2);
    assert_eq!(vec!["open_tab_page", "open_beside"], capabilities(&rig));
    assert_eq!("t0", origin(&asked(&rig)[1]));
}

#[test]
fn a_later_window_of_a_startup_tab_page_waits_for_its_areas_first_terminal() {
    let Some((mut rig, _)) = tabs("tp-later", 2, FakeProvider::opening()) else {
        return;
    };
    lua(&mut rig, "vim.cmd('tabnext')");
    until_asked(&mut rig, 1);
    let split = rig.ints("vim.cmd('vsplit')\nreturn { vim.api.nvim_get_current_win() }")[0];
    // made external: the daemon has looked at it since, and waits
    rig.pump(|r| {
        r.d.grids
            .wins
            .values()
            .any(|w| w.win == split && w.kind == WinKind::External)
    });
    assert!(rig.d.panes.by_win(split).is_none());
    assert_eq!(vec!["open_tab_page"], capabilities(&rig));

    // the area's first terminal comes: it opens beside that
    answer_last(&mut rig, "t5");
    until_asked(&mut rig, 2);
    let asks = asked(&rig);
    assert!(
        matches!(asks[1], Ask::OpenBeside { .. }),
        "{:?}",
        capabilities(&rig)
    );
    assert_eq!("t5", origin(&asks[1]));
    assert_ne!(0, rig.d.panes.by_win(split).expect("a pane").tab);
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
    assert_eq!(1, rig.d.panes.list.len());
    let p = rig.d.panes.by_id(1).unwrap();
    // each tab page its own layout there: the first, b's and the new one's
    let mut shown: Vec<i64> = p.held.iter().map(|h| h.tab).collect();
    shown.push(p.shown_tab);
    shown.sort();
    shown.dedup();
    assert_eq!(3, shown.len(), "{shown:?}");
    assert_eq!(0, shown[0]);

    // nothing was asked: a split in the first tab page, asked after, is the
    // first ask
    lua(&mut rig, "vim.cmd('tabfirst')\nvim.cmd('vsplit')");
    until_asked(&mut rig, 1);
    assert_eq!(vec!["open"], capabilities(&rig));
}

/// The tab page count, whether window `win` is still valid, the current
/// window's buffer, and how many times the refusal was said.
fn after_move(win: i64) -> String {
    format!(
        "local said = 0\n\
         for _ in vim.api.nvim_exec2('messages', {{ output = true }}).output\
         :gmatch('Already only one window') do said = said + 1 end\n\
         return {{ vim.fn.tabpagenr('$'), vim.api.nvim_win_is_valid({win}) and 1 or 0, \
         vim.api.nvim_get_current_buf(), said }}"
    )
}

/// Types `keys` in Normal mode, mappings and all.
fn normal(rig: &mut Rig, keys: &str) {
    lua(rig, &format!("vim.cmd.normal(vim.keycode('{keys}'))"));
}

#[test]
fn ctrl_w_capital_t_moves_a_window_to_a_new_tab_page_and_refuses_a_tab_pages_only_window() {
    let Some((mut rig, wins)) = tabs("tp-move", 1, FakeProvider::opening()) else {
        return;
    };
    let first = wins[0];
    let buf = rig.ints("return { vim.api.nvim_get_current_buf() }")[0];

    // its tab page's only window: refused, from Visual mode too
    normal(&mut rig, "<C-w>T");
    assert_eq!(vec![1, 1, buf, 1], rig.ints(&after_move(first)));
    normal(&mut rig, "v<C-w>T");
    assert_eq!(vec![1, 1, buf, 2], rig.ints(&after_move(first)));

    // nothing was asked: the split is the first ask
    lua(&mut rig, "vim.cmd('vsplit')");
    until_asked(&mut rig, 1);
    assert_eq!(vec!["open_beside"], capabilities(&rig));
    let split = rig.ints("return { vim.api.nvim_get_current_win() }")[0];
    assert_ne!(first, split);
    // answered, so the next open isn't queued behind it
    answer_last(&mut rig, "t1");

    // moved, from Visual mode: a new window, with its own place
    normal(&mut rig, "v<C-w>T");
    assert_eq!(vec![2, 0, buf, 2], rig.ints(&after_move(split)));
    // its old pane's terminal closes; answered, the next ask comes at once
    until_asked(&mut rig, 2);
    assert_eq!(vec!["open_beside", "close"], capabilities(&rig));
    let f = rig.d.provider.as_mut().unwrap().fake();
    f.answer(f.last_id(), Outcome::Done(Answer::Nothing));
    rig.d.drain_provider().unwrap();
    until_asked(&mut rig, 3);
    assert_eq!(
        vec!["open_beside", "close", "open_tab_page"],
        capabilities(&rig)
    );
    let moved = rig.ints("return { vim.api.nvim_get_current_win() }")[0];
    assert!(moved != first && moved != split);
    // the old one's pane is gone
    rig.pump(|r| r.d.panes.by_win(split).is_none());

    // the new tab page's only window: refused too, and asks nothing, as a
    // split in the first tab page, asked after, shows
    answer_last(&mut rig, "t2");
    normal(&mut rig, "<C-w>T");
    assert_eq!(vec![2, 1, buf, 3], rig.ints(&after_move(moved)));
    lua(&mut rig, "vim.cmd('tabfirst')\nvim.cmd('vsplit')");
    until_asked(&mut rig, 4);
    assert_eq!(
        vec!["open_beside", "close", "open_tab_page", "open_beside"],
        capabilities(&rig)
    );
}
