//! A tab page without a tab page area, as the first: its window panes stay
//! where they opened, beside their origin pane and the self-attached one
//! they came from, but a window Neovim resizes is resized in the pane
//! provider too, whichever side of its split it is on.

use super::*;

/// A daemon whose fake pane provider can `caps`, with windows 1 and 2 of
/// tab page 1 drawn, each in a pane in no tab page area, in layout area
/// `@0`: window 1's the self-attached pane Neovim started in, window 2's
/// requested. Each has the terminal `block_id(n)`.
fn first_tab_page(caps: &[Capability]) -> Fixture {
    let mut f = FakeProvider::opening();
    for &c in caps {
        f.caps.insert(c);
    }
    let mut fx = with_provider(Some(Box::new(f)));
    for n in [1u32, 2] {
        fx.d.grids.wins.insert(
            n + 10,
            Win {
                win: i64::from(n),
                kind: WinKind::External,
                float: Float::default(),
            },
        );
        let p = fx.d.panes.add_for_window(i64::from(n));
        p.self_attached = n == 1;
        p.terminal = Some(TerminalId::from(block_id(n).as_str()));
        p.area = live("@0", [0.0, 0.0, 0.5, 1.0]);
    }
    fx
}

/// Tab page 1's windows 1 and 2 side by side, `widths` wide.
fn first_row(widths: &[u32]) -> Layout {
    let mut layout = window_row(widths);
    layout.tab = 1;
    layout
}

#[test]
fn resizing_the_left_window_resizes_its_pane_against_the_right_one() {
    let mut fx = first_tab_page(&[Capability::Resize]);
    let d = &mut fx.d;
    // as they opened: nothing to place, nor any place_beside to do it
    d.on_tab_layout(first_row(&[40, 39])).unwrap();
    assert!(fake(d).asks.is_empty());
    // `:vertical resize 30` on window 1, the self-attached pane's: window 2
    // has the rest
    d.on_tab_layout(first_row(&[30, 49])).unwrap();
    assert_eq!(
        ratios_rounded(&[resize(2, 1, 49, 49.0 / 79.0)]),
        ratios_rounded(&fake(d).asked())
    );
}

#[test]
fn its_panes_are_never_moved_even_when_neovim_moves_its_windows() {
    let mut fx = first_tab_page(&[Capability::PlaceBeside, Capability::Resize]);
    let d = &mut fx.d;
    d.on_tab_layout(first_row(&[40, 39])).unwrap();
    // `<C-w>x`: the same windows, the other way round
    let mut swapped = first_row(&[39, 40]);
    swapped.nodes[1].win = 2;
    swapped.nodes[2].win = 1;
    d.on_tab_layout(swapped).unwrap();
    assert!(fake(d).asks.is_empty());
    // sized from there
    let mut wider = swapped;
    wider.nodes[1].w = 49;
    wider.nodes[2].w = 30;
    d.on_tab_layout(wider).unwrap();
    assert_eq!(
        ratios_rounded(&[resize(1, 2, 30, 30.0 / 79.0)]),
        ratios_rounded(&fake(d).asked())
    );
}

#[test]
fn panes_the_live_layout_has_in_two_layout_areas_are_not_resized() {
    let mut fx = first_tab_page(&[Capability::Resize]);
    let d = &mut fx.d;
    // the self-attached pane's terminal moved away by hand
    d.panes.by_win_mut(1).unwrap().area = live("@1", [0.0, 0.0, 1.0, 1.0]);
    d.on_tab_layout(first_row(&[40, 39])).unwrap();
    d.on_tab_layout(first_row(&[30, 49])).unwrap();
    assert!(fake(d).asks.is_empty());
    // nor without a live layout, each pane its own area
    d.panes.by_win_mut(1).unwrap().area = Area::Own;
    d.panes.by_win_mut(2).unwrap().area = Area::Own;
    d.on_tab_layout(first_row(&[20, 59])).unwrap();
    assert!(fake(d).asks.is_empty());
}
