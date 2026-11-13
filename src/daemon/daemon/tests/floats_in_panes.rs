//! Float groups with no float group pane, drawn in a window pane, and the
//! screen shrunk to fit one (ADR 0010).

use super::*;

/// Pane 50 (window 1000, terminal t1), current, and pane 51 (window 1001,
/// t2), side by side in one live layout area: 60 x 40 each, on a screen of
/// 120 x 40. `provider` is the pane provider, if any.
fn two_panes(provider: Option<FakeProvider>) -> Fixture {
    let mut fx = with_provider(provider.map(|f| Box::new(f) as Box<dyn PaneProvider>));
    let d = &mut fx.d;
    d.nvim.ready = true;
    d.grids.grids.insert(1, Grid::default());
    d.grids.get_mut(1).unwrap().resize(120, 40);
    d.nvim.editor_size = [120, 40];
    for (id, win, grid, x, t) in [(50, 1000, 2, 0.0, "t1"), (51, 1001, 3, 0.5, "t2")] {
        draw_external(d, grid, win, 60, 39);
        let mut p = Pane::from_neovim_window(id, win);
        p.terminal_surface = Some(Surface::for_test(60, 40));
        p.terminal = Some(TerminalId::from(t));
        p.area = Area::Live {
            id: "w".into(),
            rect: [x, 0.0, 0.5, 1.0],
            floating: false,
        };
        d.panes.add(p);
    }
    d.nvim.current_win = 1000;
    fx
}

/// Sets float `win` on grid `grid` to `w` x `h` at `at` on the screen, as
/// Neovim draws it.
fn put_float(d: &mut Daemon, grid: u32, win: i64, at: [i32; 2], w: u32, h: u32) {
    d.grids.grids.entry(grid).or_default().resize(w, h);
    d.grids.wins.insert(
        grid,
        Win {
            win,
            kind: WinKind::Float,
            float: Float {
                screen: Some(at),
                zindex: 50 + grid,
                ..Float::default()
            },
        },
    );
}

/// Float `win`, as `put_float`, entered and made current.
fn enter_float(d: &mut Daemon, grid: u32, win: i64, at: [i32; 2], w: u32, h: u32) {
    put_float(d, grid, win, at, w, h);
    d.on_entered_float(win).unwrap();
    d.on_current_float(win).unwrap();
}

/// The floats pane `id` drew last, by grid, row and column.
fn drawn(d: &Daemon, id: u32) -> Vec<(u32, i32, i32)> {
    let s = d
        .panes
        .by_id(id)
        .unwrap()
        .terminal_surface
        .as_ref()
        .unwrap();
    s.view
        .drawn_floats()
        .iter()
        .map(|r| (r.grid, r.row, r.col))
        .collect()
}

/// How many float group panes there are, ready or on their way.
fn float_group_panes(d: &Daemon) -> usize {
    d.panes.list.iter().filter(|p| p.layer_hides()).count()
}

/// How many `open_float`s were asked.
fn open_floats(d: &mut Daemon) -> usize {
    fake(d)
        .asked()
        .iter()
        .filter(|a| matches!(a, Ask::OpenFloat { .. }))
        .count()
}

/// The float on grid `grid` closed.
fn close_float(d: &mut Daemon, grid: u32) {
    d.grids.wins.swap_remove(&grid);
    d.grids.grids.swap_remove(&grid);
    d.reconcile_layers();
    d.draw().unwrap();
}

#[test]
fn without_open_float_a_picker_is_drawn_in_the_pane_it_was_last_current_in() {
    let Fixture {
        mut d,
        nvim_end: _nvim,
    } = two_panes(None);
    // over the right pane's part of the screen, but current from the left
    enter_float(&mut d, 10, 2000, [5, 80], 50, 10);
    assert_eq!(Shrink::No, d.nvim.shrink);
    assert_eq!(vec![(10, 5, 10)], drawn(&d, 50));
    assert!(drawn(&d, 51).is_empty());

    // the user goes to the other pane: it stays
    d.on_current(1001).unwrap();
    assert_eq!(vec![(10, 5, 10)], drawn(&d, 50));
    assert!(drawn(&d, 51).is_empty());

    // one of its floats becomes current from there: it moves
    d.on_current_float(2000).unwrap();
    assert!(drawn(&d, 50).is_empty());
    assert_eq!(vec![(10, 5, 10)], drawn(&d, 51));
    assert!(d.panes.list.iter().all(|p| p.layer.is_none()));
}

#[test]
fn an_oversized_group_shrinks_the_screen_to_the_pane_and_grows_it_back() {
    let mut fx = two_panes(None);
    let d = &mut fx.d;
    // 80 columns: wider than the pane
    enter_float(d, 10, 2000, [5, 20], 80, 30);
    assert_eq!(Shrink::Shrunk(2000), d.nvim.shrink);
    assert_eq!([60, 40], d.nvim.editor_size);
    // not drawn until Neovim has redrawn at that size
    assert!(drawn(d, 50).is_empty());
    d.grids.get_mut(1).unwrap().resize(60, 40);
    put_float(d, 10, 2000, [5, 5], 50, 30);
    d.draw().unwrap();
    assert_eq!(vec![(10, 5, 5)], drawn(d, 50));

    // a prompt over it: the shrink holds
    enter_float(d, 11, 2001, [2, 6], 20, 1);
    assert_eq!(Shrink::Shrunk(2000), d.nvim.shrink);
    assert_eq!([60, 40], d.nvim.editor_size);

    // a window current again: the screen grows back, and the picker's
    // floats lay themselves out again
    let _ = sent_to_nvim(&mut fx);
    let d = &mut fx.d;
    d.on_current(1000).unwrap();
    assert_eq!(Shrink::No, d.nvim.shrink);
    assert_eq!([120, 40], d.nvim.editor_size);
    let sent = sent_to_nvim(&mut fx);
    assert_eq!(1, sent.matches(&format!("{NAME}.resized_again")).count());
    assert!(sent.contains("nvim_ui_try_resize"));

    // left open, too big for its pane: moved in and cut there
    let d = &mut fx.d;
    d.grids.get_mut(1).unwrap().resize(120, 40);
    close_float(d, 11);
    put_float(d, 10, 2000, [5, 20], 80, 30);
    d.draw().unwrap();
    assert_eq!(vec![(10, 5, 0)], drawn(d, 50));
}

#[test]
fn a_group_with_a_float_group_pane_shrinks_nothing() {
    let Fixture {
        mut d,
        nvim_end: _nvim,
    } = two_panes(Some(FakeProvider::opening()));
    enter_float(&mut d, 10, 2000, [5, 20], 80, 30);
    assert_eq!(1, open_floats(&mut d));
    assert_eq!(Shrink::No, d.nvim.shrink);
    assert_eq!([120, 40], d.nvim.editor_size);
}

#[test]
fn exit_69_draws_every_group_after_in_a_pane_and_a_failure_only_its_own() {
    for (outcome, asks_again) in [
        (Outcome::Unsupported, false),
        (Outcome::Failed("no room".into()), true),
    ] {
        let Fixture {
            mut d,
            nvim_end: _nvim,
        } = two_panes(Some(FakeProvider::opening()));
        enter_float(&mut d, 10, 2000, [5, 5], 20, 5);
        assert_eq!(1, open_floats(&mut d));
        let f = fake(&mut d);
        f.answer(f.last_id(), outcome.clone());
        d.drain_provider().unwrap();
        d.draw().unwrap();
        assert_eq!(vec![(10, 5, 5)], drawn(&d, 50), "{outcome:?}");
        // it isn't asked for again
        d.reconcile_layers();
        assert_eq!(1, open_floats(&mut d));

        close_float(&mut d, 10);
        enter_float(&mut d, 11, 2001, [5, 5], 20, 5);
        assert_eq!(
            if asks_again { 2 } else { 1 },
            open_floats(&mut d),
            "{outcome:?}"
        );
        if !asks_again {
            assert_eq!(vec![(11, 5, 5)], drawn(&d, 50));
        }
    }
}

#[test]
fn a_fifth_group_is_drawn_in_a_pane_for_life() {
    let Fixture {
        mut d,
        nvim_end: _nvim,
    } = two_panes(Some(FakeProvider::opening()));
    for i in 0..5u32 {
        enter_float(
            &mut d,
            10 + i,
            2000 + i64::from(i),
            [5, 10 * i as i32],
            5,
            3,
        );
    }
    assert_eq!(4, float_group_panes(&d));
    assert_eq!(vec![(14, 5, 40)], drawn(&d, 50));
    // one of the four closes: the fifth stays where it is
    close_float(&mut d, 10);
    assert_eq!(3, float_group_panes(&d));
    assert_eq!(vec![(14, 5, 40)], drawn(&d, 50));
}

#[test]
fn a_group_is_drawn_nowhere_while_its_pane_is_on_its_way_then_in_a_pane_once_late() {
    let Fixture {
        mut d,
        nvim_end: _nvim,
    } = two_panes(Some(FakeProvider::opening()));
    enter_float(&mut d, 10, 2000, [5, 5], 20, 5);
    assert!(drawn(&d, 50).is_empty());
    let f = fake(&mut d);
    f.opened(f.last_id(), "t9");
    d.drain_provider().unwrap();
    d.draw().unwrap();
    assert!(drawn(&d, 50).is_empty());
    assert!(drawn(&d, 51).is_empty());

    // its terminal didn't attach in time
    d.time_out_attaches(Instant::now() + Duration::from_secs(600))
        .unwrap();
    d.draw().unwrap();
    assert_eq!(vec![(10, 5, 5)], drawn(&d, 50));
    assert!(
        fake(&mut d)
            .asked()
            .contains(&asks::close(TerminalId::from("t9")))
    );
}

#[test]
fn a_key_in_a_float_group_pane_whose_floats_arent_current_goes_into_them() {
    let Fixture {
        mut d,
        nvim_end: _nvim,
    } = two_panes(Some(FakeProvider::opening()));
    put_float(&mut d, 10, 2000, [5, 5], 20, 5);
    d.nvim.note_entered(2000);
    d.reconcile_layers();
    let id = d.panes.list.iter().find(|p| p.layer.is_some()).unwrap().id;
    d.panes.by_id_mut(id).unwrap().terminal_surface = Some(Surface::for_test(20, 5));
    assert_eq!(0, d.nvim.current_float);
    d.on_read(id, b"x", Instant::now()).unwrap();
    assert_eq!(2000, d.nvim.current_float);
    assert_eq!(1000, d.nvim.current_win);
}
