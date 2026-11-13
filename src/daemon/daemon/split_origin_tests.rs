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
