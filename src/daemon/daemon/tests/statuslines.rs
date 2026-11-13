//! The statuslines neopane draws for Neovim at `'laststatus'` 0 (SPI-969):
//! asked after each redraw and when windows.lua says they changed, one ask at
//! a time, and only while a window has one.

use super::choosing::nvim_flushes;
use super::*;
use crate::screen::grid::Cell;
use crate::screen::style::STATUSLINE_BASE;

/// The windows each `<name>.statuslines(...)` in `all` asked for.
fn statuslines_asked(all: &[u8]) -> Vec<Vec<i64>> {
    let call = format!("{NAME}.statuslines(...)");
    let call = call.as_bytes();
    let mut asked = Vec::new();
    let mut at = 0;
    while let Some(i) = all[at..].windows(call.len()).position(|w| w == call) {
        at += i + call.len();
        let mut args = msgpack::Decoder::of(&all[at..]);
        assert_eq!(Some(msgpack::Token::Array(1)), args.next().unwrap());
        let Some(msgpack::Token::Array(n)) = args.next().unwrap() else {
            panic!("no windows");
        };
        let wins = (0..n)
            .map(|_| match args.next().unwrap() {
                Some(msgpack::Token::Int(w)) => w,
                t => panic!("not a window: {t:?}"),
            })
            .collect();
        asked.push(wins);
    }
    asked
}

/// With Rex closed and `'laststatus'` `laststatus`: a pane showing window
/// 1001 (grid 3) above 1000 (grid 2), the one above current.
fn two_stacked(fx: &mut Fixture, laststatus: u32) {
    fx.d.provider = None;
    fx.d.nvim.ready = true;
    fx.d.on_note(windows::Note::Options(windows::SplitOptions {
        statuslines: laststatus != 0,
        ..windows::SplitOptions::default()
    }))
    .unwrap();
    let mut p = Pane::from_neovim_window(1, 1000);
    p.terminal_surface = Some(Surface::for_test(80, 21));
    fx.d.panes.add(p);
    fx.d.nvim.current_win = 1000;
    draw_external(&mut fx.d, 2, 1000, 80, 20);
    note_split_from(&mut fx.d, 1001, 1000, false, true);
    fx.d.on_note(windows::Note::Current(1001)).unwrap();
    draw_external(&mut fx.d, 3, 1001, 80, 9);
    fx.d.reconcile().unwrap();
}

/// Neovim's answer: one highlight, red, and window 1001's statusline `text`.
fn answer(text: &str) -> Vec<u8> {
    let mut a = Vec::new();
    msgpack::write_array_header(&mut a, 2);
    msgpack::write_array_header(&mut a, 1);
    msgpack::write_map_header(&mut a, 1);
    msgpack::write_str(&mut a, "foreground");
    msgpack::write_int(&mut a, 0xff0000);
    msgpack::write_array_header(&mut a, 1);
    msgpack::write_array_header(&mut a, 2);
    msgpack::write_int(&mut a, 1001);
    msgpack::write_array_header(&mut a, text.len() as u32);
    for c in text.chars() {
        msgpack::write_array_header(&mut a, 2);
        msgpack::write_str(&mut a, c.to_string());
        msgpack::write_int(&mut a, 0);
    }
    a
}

fn cells(text: &str) -> Vec<Cell> {
    text.chars()
        .map(|c| Cell::init(c.to_string(), STATUSLINE_BASE))
        .collect()
}

#[test]
fn after_a_redraw_neovim_is_asked_for_the_statuslines_neopane_draws_one_ask_at_a_time() {
    let mut fx = fixture();
    two_stacked(&mut fx, 0);
    nvim_flushes(&mut fx);
    let id = fx.d.statuslines_ask.in_flight().unwrap();
    // redrawn again before the answer: asked again once it comes
    nvim_flushes(&mut fx);
    assert_eq!(Some(id), fx.d.statuslines_ask.in_flight());

    fx.d.on_response(id, None, msgpack::Decoder::of(&answer("x")))
        .unwrap();
    assert_eq!(cells("x"), fx.d.grids.statuslines[&3]);
    assert_eq!(Some(0xff0000), fx.d.grids.style.attr(STATUSLINE_BASE).fg);
    assert!(
        fx.d.statuslines_ask
            .in_flight()
            .is_some_and(|again| again != id)
    );
    let sent = sent_by(fx);
    assert_eq!(
        vec![vec![1001], vec![1001]],
        statuslines_asked(&sent.nvim_bytes)
    );
}

#[test]
fn a_change_neovim_draws_nothing_for_asks_too() {
    let mut fx = fixture();
    two_stacked(&mut fx, 0);
    fx.d.on_note(windows::Note::Statuslines).unwrap();
    assert!(fx.d.statuslines_ask.in_flight().is_some());
    assert_eq!(vec![vec![1001]], statuslines_asked(&sent_by(fx).nvim_bytes));
}

#[test]
fn with_no_statusline_neopane_draws_nothing_is_asked() {
    // Neovim draws the statuslines
    let mut fx = fixture();
    two_stacked(&mut fx, 2);
    nvim_flushes(&mut fx);
    fx.d.on_note(windows::Note::Statuslines).unwrap();
    assert_eq!(None, fx.d.statuslines_ask.in_flight());
    assert!(statuslines_asked(&sent_by(fx).nvim_bytes).is_empty());

    // a window alone in its pane has none
    let mut fx = fixture();
    fx.d.provider = None;
    fx.d.nvim.ready = true;
    fx.d.on_note(windows::Note::Options(windows::SplitOptions {
        statuslines: false,
        ..windows::SplitOptions::default()
    }))
    .unwrap();
    let mut p = Pane::from_neovim_window(1, 1000);
    p.terminal_surface = Some(Surface::for_test(80, 21));
    fx.d.panes.add(p);
    fx.d.nvim.current_win = 1000;
    draw_external(&mut fx.d, 2, 1000, 80, 20);
    fx.d.reconcile().unwrap();
    nvim_flushes(&mut fx);
    assert_eq!(None, fx.d.statuslines_ask.in_flight());
    assert!(statuslines_asked(&sent_by(fx).nvim_bytes).is_empty());
}

#[test]
fn a_statusline_no_longer_drawn_is_forgotten() {
    let mut fx = fixture();
    two_stacked(&mut fx, 0);
    nvim_flushes(&mut fx);
    let id = fx.d.statuslines_ask.in_flight().unwrap();
    fx.d.on_response(id, None, msgpack::Decoder::of(&answer("x")))
        .unwrap();
    assert!(fx.d.grids.statuslines.contains_key(&3));
    // 'laststatus' 2: Neovim draws it
    fx.d.on_note(windows::Note::Options(windows::SplitOptions::default()))
        .unwrap();
    nvim_flushes(&mut fx);
    assert!(fx.d.grids.statuslines.is_empty());
}
