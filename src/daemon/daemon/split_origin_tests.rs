//! A window split off a window made in the same flush, whose terminal is on
//! its way (SPI-1157), with a real Neovim and a fake pane provider that opens
//! beside and has no `place_beside`: nothing moves a terminal opened in the
//! wrong place. The split waits for its origin's terminal and opens beside
//! it, in the first tab page and in a tab page area; it stops waiting once
//! that terminal won't come.

use super::self_attach_tests::Rig;
use super::tab_area_tests::{lua, tabs};
use super::*;
use crate::daemon::provider::fake::FakeProvider;
use crate::daemon::provider::{AskId, Outcome};

/// Opens beside its origin, opens tab page areas, focuses and closes: no
/// `place_beside`. A window of the first tab page opened while another is
/// current gets a terminal only if focus can be given back.
fn beside_only() -> FakeProvider {
    FakeProvider::with(&[
        Capability::OpenBeside,
        Capability::OpenTabPage,
        Capability::Focus,
        Capability::Close,
    ])
}

/// The opens asked, with their ids, oldest first.
fn opens(rig: &Rig) -> Vec<(AskId, Ask)> {
    let Some(p) = rig.d.provider.as_ref() else {
        return Vec::new();
    };
    p.fake_ref()
        .asks
        .iter()
        .filter(|(_, a)| matches!(a, Ask::OpenBeside { .. } | Ask::OpenTabPage { .. }))
        .cloned()
        .collect()
}

/// Pumps until `n` opens were asked.
fn until_opens(rig: &mut Rig, n: usize) {
    rig.pump(|r| opens(r).len() >= n);
}

/// Answers open `i`, oldest first, with `outcome`.
fn answer_open(rig: &mut Rig, i: usize, outcome: Outcome) {
    let id = opens(rig)[i].0;
    rig.d.provider.as_mut().unwrap().fake().answer(id, outcome);
    rig.d.drain_provider().unwrap();
}

/// Answers open `i`, oldest first, with terminal `t`.
fn open_as(rig: &mut Rig, i: usize, t: &str) {
    answer_open(rig, i, Outcome::Done(Answer::Terminal(TerminalId::from(t))));
}

/// Where open `ask` opens beside, in which direction, and at what size,
/// cols x rows.
fn beside(ask: &Ask) -> (&str, Direction, [u32; 2]) {
    match ask {
        Ask::OpenBeside {
            origin,
            dir,
            cols,
            rows,
            ..
        } => (origin.as_str(), *dir, [*cols, *rows]),
        a => panic!("not open_beside: {a:?}"),
    }
}

/// The opens asked so far, then the windows each of Neovim's `cmds` makes,
/// all in one flush, each split off the one before. Starts in the first tab
/// page, whose first terminal is `t0`, or with `in_area` in a new tab page's
/// tab page area, whose first terminal is `t5`. None when Neovim isn't
/// installed.
fn split_in_one_flush(
    tag: &str,
    fake: FakeProvider,
    in_area: bool,
    cmds: &[&str],
) -> Option<(Rig, usize, Vec<i64>)> {
    let (mut rig, _) = tabs(tag, 1, fake)?;
    let mut before = 0;
    if in_area {
        lua(&mut rig, "vim.cmd('tabnew')");
        until_opens(&mut rig, 1);
        open_as(&mut rig, 0, "t5");
        before = 1;
    }
    let code: String = cmds
        .iter()
        .map(|c| format!("vim.cmd('{c}')\ntable.insert(wins, vim.api.nvim_get_current_win())\n"))
        .collect();
    let wins = rig.ints(&format!("local wins = {{}}\n{code}return wins"));
    // the first is asked for; reconcile met the others too
    until_opens(&mut rig, before + 1);
    Some((rig, before, wins))
}

/// `:vsplit | split` in one flush: the split of the split waits, without a
/// pane, for the first split's terminal, and opens beside it, above it as
/// Neovim split it, at half the size the first split was opened at.
fn vsplit_split_opens_beside_its_origin(tag: &str, in_area: bool) {
    let Some((mut rig, before, wins)) =
        split_in_one_flush(tag, beside_only(), in_area, &["vsplit", "split"])
    else {
        return;
    };
    let first_origin = if in_area { "t5" } else { "t0" };
    let opens_now = opens(&rig);
    assert_eq!(before + 1, opens_now.len());
    let (origin, dir, [cols, rows]) = beside(&opens_now[before].1);
    assert_eq!((first_origin, Direction::Left), (origin, dir));
    assert!(rig.d.panes.by_win(wins[1]).is_none(), "the split waits");

    // asked as the answer comes, not at Neovim's next flush
    open_as(&mut rig, before, "t1");
    let opens_now = opens(&rig);
    let (origin, dir, size) = beside(&opens_now[before + 1].1);
    assert_eq!(("t1", Direction::Up), (origin, dir));
    // its origin's terminal hasn't attached: half the size it was opened at
    assert_eq!([cols, (rows / 2).max(2)], size);
    assert!(rig.d.panes.by_win(wins[1]).is_some());
}

#[test]
fn a_split_of_a_split_made_in_one_flush_opens_beside_it_in_the_first_tab_page() {
    vsplit_split_opens_beside_its_origin("so-first", false);
}

#[test]
fn a_split_of_a_split_made_in_one_flush_opens_beside_it_in_a_tab_page_area() {
    vsplit_split_opens_beside_its_origin("so-area", true);
}

/// `:vsplit | split | vsplit` in one flush: each split opens beside the one
/// before it, as its terminal is answered, in the direction Neovim split it.
fn a_chain_of_three_opens_each_beside_its_own_origin(tag: &str, in_area: bool) {
    let Some((mut rig, before, wins)) =
        split_in_one_flush(tag, beside_only(), in_area, &["vsplit", "split", "vsplit"])
    else {
        return;
    };
    let first_origin = if in_area { "t5" } else { "t0" };
    let want = [
        (first_origin, Direction::Left),
        ("t1", Direction::Up),
        ("t2", Direction::Left),
    ];
    for (i, want) in want.into_iter().enumerate() {
        let opens_now = opens(&rig);
        assert_eq!(before + i + 1, opens_now.len(), "split {i}");
        let (origin, dir, _) = beside(&opens_now[before + i].1);
        assert_eq!(want, (origin, dir), "split {i}");
        // the ones after it wait, without a pane
        for &w in &wins[i + 1..] {
            assert!(rig.d.panes.by_win(w).is_none(), "split {i}");
        }
        open_as(&mut rig, before + i, &format!("t{}", i + 1));
    }
    assert!(wins.iter().all(|&w| rig.d.panes.by_win(w).is_some()));
}

#[test]
fn a_chain_of_three_splits_in_one_flush_opens_each_beside_its_own_origin_in_the_first_tab_page() {
    a_chain_of_three_opens_each_beside_its_own_origin("so-chain", false);
}

#[test]
fn a_chain_of_three_splits_in_one_flush_opens_each_beside_its_own_origin_in_a_tab_page_area() {
    a_chain_of_three_opens_each_beside_its_own_origin("so-chain-area", true);
}

/// `:vsplit | split` in one flush, then `end` ends the wait of the split of
/// the split, given the two windows: it gets a pane without waiting for
/// the first split's terminal any more. The rig, the opens asked before,
/// and the two windows. None when Neovim isn't installed.
fn the_wait_ends(
    tag: &str,
    fake: FakeProvider,
    in_area: bool,
    end: impl FnOnce(&mut Rig, &[i64]),
) -> Option<(Rig, usize, Vec<i64>)> {
    let (mut rig, before, wins) = split_in_one_flush(tag, fake, in_area, &["vsplit", "split"])?;
    assert!(rig.d.panes.by_win(wins[1]).is_none(), "the split waits");
    end(&mut rig, &wins);
    rig.pump(|r| r.d.panes.by_win(wins[1]).is_some());
    Some((rig, before, wins))
}

/// The first split's open fails: the split opens beside the terminal the
/// first split is drawn in now, its origin's.
#[test]
fn a_split_stops_waiting_when_its_origins_open_fails() {
    let failed = |rig: &mut Rig, _: &[i64]| answer_open(rig, 0, Outcome::Failed("no room".into()));
    let Some((rig, _, wins)) = the_wait_ends("so-fail", beside_only(), false, failed) else {
        return;
    };
    let opens_now = opens(&rig);
    assert_eq!(2, opens_now.len());
    let (origin, dir, _) = beside(&opens_now[1].1);
    assert_eq!(("t0", Direction::Up), (origin, dir));
    // the first split is drawn in the first window's pane
    let p = rig.d.panes.by_win(wins[0]).unwrap();
    assert_eq!(Some("t0"), p.terminal.as_ref().map(TerminalId::as_str));
}

/// In a tab page area, the first split's pane stays given up: the split
/// opens beside the area's first terminal.
#[test]
fn a_split_stops_waiting_when_its_origins_open_fails_in_a_tab_page_area() {
    let failed = |rig: &mut Rig, _: &[i64]| answer_open(rig, 1, Outcome::Failed("no room".into()));
    let Some((rig, _, _)) = the_wait_ends("so-fail-area", beside_only(), true, failed) else {
        return;
    };
    let opens_now = opens(&rig);
    assert_eq!(3, opens_now.len());
    let (origin, dir, _) = beside(&opens_now[2].1);
    assert_eq!(("t5", Direction::Up), (origin, dir));
}

/// The pane provider refuses `open_beside`: neither split gets a terminal,
/// and both are drawn in the first window's pane.
#[test]
fn a_split_stops_waiting_when_its_origins_open_is_refused() {
    let refused = |rig: &mut Rig, _: &[i64]| answer_open(rig, 0, Outcome::Unsupported);
    let Some((rig, _, wins)) = the_wait_ends("so-refused", beside_only(), false, refused) else {
        return;
    };
    assert_eq!(1, opens(&rig).len());
    let first = rig.d.panes.by_win(wins[0]).unwrap().id;
    assert_eq!(first, rig.d.panes.by_win(wins[1]).unwrap().id);
}

/// The first split's open times out: the split opens beside the terminal
/// the first split is drawn in now.
#[test]
fn a_split_stops_waiting_when_its_origins_open_times_out() {
    let mut fake = beside_only();
    fake.timeout = Some(Duration::from_secs(1));
    let Some((rig, _, _)) = the_wait_ends("so-timeout", fake, false, |_, _| {}) else {
        return;
    };
    let opens_now = opens(&rig);
    assert_eq!(2, opens_now.len());
    assert_eq!("t0", beside(&opens_now[1].1).0);
}

/// The first split closes while its terminal is on its way: the split gets
/// a pane of its own.
#[test]
fn a_split_stops_waiting_when_its_origin_closes() {
    let closed = |rig: &mut Rig, wins: &[i64]| {
        lua(rig, &format!("vim.api.nvim_win_close({}, true)", wins[0]));
    };
    let Some((rig, _, wins)) = the_wait_ends("so-closed", beside_only(), false, closed) else {
        return;
    };
    assert!(rig.d.panes.by_win(wins[0]).is_none());
    assert!(rig.d.panes.by_win(wins[1]).unwrap().split.is_some());
}

/// The pane provider is gone, and dropped, while the first split's terminal
/// is on its way: the split is drawn in its origin's pane.
#[test]
fn a_split_stops_waiting_when_the_pane_provider_is_gone() {
    let gone = |rig: &mut Rig, _: &[i64]| {
        let reason = "lost".to_string();
        rig.d
            .provider
            .as_mut()
            .unwrap()
            .fake()
            .push(ProviderEvent::Gone { reason });
        rig.d.drain_provider().unwrap();
    };
    let Some((rig, _, wins)) = the_wait_ends("so-gone", beside_only(), false, gone) else {
        return;
    };
    assert!(rig.d.provider.is_none());
    let first = rig.d.panes.by_win(wins[0]).unwrap().id;
    assert_eq!(first, rig.d.panes.by_win(wins[1]).unwrap().id);
}
