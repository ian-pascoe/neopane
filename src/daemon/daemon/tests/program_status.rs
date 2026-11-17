//! Program status (ADR 0021): which panes' terminals hold which terminal
//! buffers' status records.

use super::*;
use crate::daemon::pane::Held;
use crate::nvim::windows::{BufWin, ProgramStatusEnd};

/// Pane 50 (window 1000), current, and pane 51 (window 1001), each with a
/// terminal, on a screen of 120 x 40. `provider` is the pane provider.
fn two_panes(provider: Option<FakeProvider>) -> Fixture {
    let mut fx = with_provider(provider.map(|f| Box::new(f) as Box<dyn PaneProvider>));
    let d = &mut fx.d;
    d.nvim.ready = true;
    d.grids.grids.insert(1, Grid::default());
    d.grids.get_mut(1).unwrap().resize(120, 40);
    d.nvim.editor_size = [120, 40];
    for (id, win, grid) in [(50, 1000, 2), (51, 1001, 3)] {
        draw_external(d, grid, win, 60, 39);
        let mut p = Pane::from_neovim_window(id, win);
        p.terminal_surface = Some(Surface::for_test(60, 40));
        d.panes.add(p);
    }
    d.nvim.current_win = 1000;
    fx
}

/// The ids of the records pane `id`'s terminal holds, each with its body.
fn held(d: &Daemon, id: u32) -> Vec<(Option<String>, String)> {
    let s = d
        .panes
        .by_id(id)
        .unwrap()
        .terminal_surface
        .as_ref()
        .unwrap();
    s.program_status_held()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn bw(buf: i64, win: i64) -> BufWin {
    BufWin { buf, win }
}

fn root(body: &str) -> (Option<String>, String) {
    (None, body.to_string())
}

fn child(id: &str, body: &str) -> (Option<String>, String) {
    (Some(id.to_string()), body.to_string())
}

#[test]
fn a_pane_holds_the_status_of_the_terminal_buffer_it_shows() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(None);
    d.program_status.report(7, b"state=working:app=pi");
    d.program_status.report(7, b"state=idle:id=w");
    d.relay_program_status();
    // no window yet: kept, shown nowhere
    assert!(held(&d, 50).is_empty());
    d.program_status.set_windows(vec![bw(7, 1000)]);
    d.relay_program_status();
    assert_eq!(
        vec![root("state=working:app=pi"), child("w", "state=idle:id=w")],
        held(&d, 50)
    );
    assert!(held(&d, 51).is_empty());

    // the buffer moves to the other pane's window
    d.program_status.set_windows(vec![bw(7, 1001)]);
    d.relay_program_status();
    assert!(held(&d, 50).is_empty());
    assert_eq!(2, held(&d, 51).len());

    // hidden, then shown again
    d.program_status.set_windows(vec![]);
    d.relay_program_status();
    assert!(held(&d, 51).is_empty());
    d.program_status.set_windows(vec![bw(7, 1001)]);
    d.relay_program_status();
    assert_eq!(2, held(&d, 51).len());

    // its program exits: done stays
    d.program_status.report(7, b"state=done:id=d");
    d.program_status.end(7, ProgramStatusEnd::Exit);
    d.relay_program_status();
    assert_eq!(vec![child("d", "state=done:id=d")], held(&d, 51));
    // the buffer goes
    d.program_status.end(7, ProgramStatusEnd::Gone);
    d.relay_program_status();
    assert!(held(&d, 51).is_empty());
}

#[test]
fn a_prompt_drops_working_blocked_and_idle() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(None);
    d.program_status.report(7, b"state=blocked:kind=auth");
    d.program_status.report(7, b"state=error:id=e");
    d.program_status.set_windows(vec![bw(7, 1000)]);
    d.relay_program_status();
    assert_eq!(2, held(&d, 50).len());
    d.program_status.end(7, ProgramStatusEnd::Prompt);
    d.relay_program_status();
    assert_eq!(vec![child("e", "state=error:id=e")], held(&d, 50));
}

#[test]
fn a_pane_holding_several_buffers_windows_shows_each_under_its_own_segment() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(None);
    // pane 50 holds window 1002 too, in a tab page it isn't showing
    d.panes.by_id_mut(50).unwrap().held.push(Held {
        tab: 2,
        win: 1002,
        layout: None,
        layout_asked: Vec::new(),
    });
    d.program_status.report(7, b"state=working");
    d.program_status.report(7, b"state=idle:id=w");
    d.program_status.report(8, b"state=blocked");
    d.program_status.set_windows(vec![bw(7, 1000)]);
    d.relay_program_status();
    assert_eq!(
        vec![root("state=working"), child("w", "state=idle:id=w")],
        held(&d, 50)
    );

    // shared: the most pressing root, and every record under its buffer
    d.program_status.set_windows(vec![bw(7, 1000), bw(8, 1002)]);
    d.relay_program_status();
    let seg = |b: i64| format!("{NAME}-{b}");
    let w = format!("{}/w", seg(7));
    assert_eq!(
        vec![
            root("state=blocked"),
            child(&seg(7), &format!("state=working:id={}", seg(7))),
            child(&w, &format!("state=idle:id={w}")),
            child(&seg(8), &format!("state=blocked:id={}", seg(8))),
        ],
        held(&d, 50)
    );
    // and one again
    d.program_status.set_windows(vec![bw(8, 1002)]);
    d.relay_program_status();
    assert_eq!(vec![root("state=blocked")], held(&d, 50));
}

#[test]
fn a_floats_status_shows_in_the_pane_it_is_drawn_in() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(Some(FakeProvider::with(&[
        Capability::OpenBeside,
        Capability::Close,
    ])));
    // a float, with no float group pane, entered from pane 51
    d.on_current(1001).unwrap();
    put_float(&mut d, 10, 2000);
    d.on_entered_float(2000).unwrap();
    d.on_current_float(2000).unwrap();
    d.program_status.report(7, b"state=working");
    d.program_status.set_windows(vec![bw(7, 2000)]);
    d.relay_program_status();
    assert!(held(&d, 50).is_empty());
    assert_eq!(vec![root("state=working")], held(&d, 51));

    // in a float group pane of its own
    let g = float_groups::find(&d.grids, d.nvim.entered_floats())[0];
    let mut p = Pane::from_group(52, g);
    p.terminal_surface = Some(Surface::for_test(20, 5));
    d.panes.add(p);
    d.draw().unwrap();
    d.relay_program_status();
    assert!(held(&d, 51).is_empty());
    assert_eq!(vec![root("state=working")], held(&d, 52));
}

/// Float `win` on grid `grid`, 20 x 5 on the screen at row 5, column 70,
/// as Neovim draws it.
fn put_float(d: &mut Daemon, grid: u32, win: i64) {
    d.grids.grids.entry(grid).or_default().resize(20, 5);
    d.grids.wins.insert(
        grid,
        Win {
            win,
            kind: WinKind::Float,
            float: Float {
                screen: Some([5, 70]),
                zindex: 60,
                ..Float::default()
            },
        },
    );
}

// A float opened without entering it, as a `:terminal` float may be, is
// drawn in the current pane: its status shows there.
#[test]
fn a_float_never_entered_shows_its_status_in_the_pane_drawing_it() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(None);
    d.on_current(1001).unwrap();
    put_float(&mut d, 10, 2000);
    d.draw().unwrap();
    d.program_status.report(7, b"state=blocked");
    d.program_status.set_windows(vec![bw(7, 2000)]);
    d.relay_program_status();
    assert!(held(&d, 50).is_empty());
    assert_eq!(vec![root("state=blocked")], held(&d, 51));
    // drawn elsewhere: it follows
    d.on_current(1000).unwrap();
    d.draw().unwrap();
    d.relay_program_status();
    assert_eq!(vec![root("state=blocked")], held(&d, 50));
    assert!(held(&d, 51).is_empty());
}

#[test]
fn a_terminal_that_had_frames_dropped_is_written_again_once_caught_up() {
    let Fixture {
        mut d,
        nvim_end: _n,
    } = two_panes(None);
    d.program_status.report(7, b"state=working");
    d.program_status.set_windows(vec![bw(7, 1000)]);
    d.relay_program_status();
    let s = d
        .panes
        .by_id_mut(50)
        .unwrap()
        .terminal_surface
        .as_deref_mut()
        .unwrap();
    s.program_status_dropped();
    // a second write, unchanged, writes it all again
    assert_eq!(
        Some(surface::Sent::All),
        s.write_program_status(&d.program_status.wanted(&[7]))
            .unwrap()
    );
    assert_eq!(
        None,
        s.write_program_status(&d.program_status.wanted(&[7]))
            .unwrap()
    );
}
