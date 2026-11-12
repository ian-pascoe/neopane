//! Neovim's current tab page following the current pane (SPI-655): what the
//! daemon tells `windows.lua` for it.

use super::*;

/// What the daemon has sent Neovim and Neovim hasn't read.
fn sent_bytes(fx: &mut Fixture) -> Vec<u8> {
    fx.d.nvim.client.flush().unwrap();
    fx.nvim_end.set_nonblocking(true).unwrap();
    let mut sent = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n @ 1..) = fx.nvim_end.read(&mut buf) {
        sent.extend_from_slice(&buf[..n]);
    }
    sent
}

/// The windows each `<name>.<call>(...)` in `all` was given, in order: its
/// one argument, a list of `depth` levels of lists of windows, flattened to
/// one list per call, each inner list closed by a 0.
fn lists_sent(all: &[u8], call: &str, depth: u32) -> Vec<Vec<i64>> {
    fn list(args: &mut msgpack::Decoder<'_>, depth: u32, nested: bool, out: &mut Vec<i64>) {
        let Some(msgpack::Token::Array(n)) = args.next().unwrap() else {
            panic!("not a list");
        };
        for _ in 0..n {
            if depth > 1 {
                list(args, depth - 1, true, out);
                continue;
            }
            match args.next().unwrap() {
                Some(msgpack::Token::Int(w)) => out.push(w),
                t => panic!("not a window: {t:?}"),
            }
        }
        if nested {
            out.push(0);
        }
    }
    let call = format!("{NAME}.{call}(...)");
    let call = call.as_bytes();
    let mut sent = Vec::new();
    let mut at = 0;
    while let Some(i) = all[at..].windows(call.len()).position(|w| w == call) {
        at += i + call.len();
        let mut args = msgpack::Decoder::of(&all[at..]);
        assert_eq!(Some(msgpack::Token::Array(1)), args.next().unwrap());
        let mut wins = Vec::new();
        list(&mut args, depth, false, &mut wins);
        sent.push(wins);
    }
    sent
}

#[test]
fn the_screen_floats_of_float_group_panes_are_told_to_neovim_to_carry_when_they_change() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    show_picker(&mut fx.d);
    fx.d.reconcile_layers();
    fx.d.reconcile_layers();
    // one group, of one float
    assert_eq!(
        vec![vec![1002, 0]],
        lists_sent(&sent_bytes(&mut fx), "carry", 2)
    );

    // its float closes, and its pane with it
    fx.d.grids.wins.swap_remove(&3);
    fx.d.reconcile_layers();
    assert_eq!(
        vec![Vec::<i64>::new()],
        lists_sent(&sent_bytes(&mut fx), "carry", 2)
    );
}

#[test]
fn the_windows_panes_show_are_told_to_neovim_after_a_batch_when_they_change() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    fx.d.panes.add(Pane::from_neovim_window(1, 1001));
    // and a float group pane, whose floats aren't windows a pane shows
    show_picker(&mut fx.d);
    fx.d.reconcile_layers();
    fx.d.after_batch().unwrap();
    fx.d.after_batch().unwrap();
    assert_eq!(
        vec![vec![999, 1001]],
        lists_sent(&sent_bytes(&mut fx), "panes_show", 1)
    );

    fx.d.panes.add(Pane::from_neovim_window(2, 1000));
    fx.d.after_batch().unwrap();
    assert_eq!(
        vec![vec![999, 1000, 1001]],
        lists_sent(&sent_bytes(&mut fx), "panes_show", 1)
    );
}

#[test]
fn a_float_group_panes_floats_hidden_in_another_tab_page_keep_it_and_a_click_goes_to_them() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    show_picker(&mut fx.d);
    fx.d.reconcile_layers();
    let id = picker_pane(&fx.d);
    let f = fake(&mut fx.d);
    f.opened(f.last_id(), "block:5");
    fx.d.drain_provider().unwrap();
    sent_bytes(&mut fx);

    // Neovim couldn't move them into the tab page entered: hidden, they
    // keep their pane
    fx.d.grids.wins.get_mut(&3).unwrap().kind = WinKind::Hidden;
    fx.d.reconcile_layers();
    assert_eq!(
        Some(id),
        fx.d.panes
            .list
            .iter()
            .find(|p| p.layer.is_some())
            .map(|p| p.id)
    );
    // a click in it goes to them, in their tab page
    fx.d.on_provider_focus(&TerminalId::from("block:5"))
        .unwrap();
    let sent = sent_bytes(&mut fx);
    let call = format!("{NAME}.go(...)");
    let at = sent
        .windows(call.len())
        .position(|w| w == call.as_bytes())
        .expect("a switch");
    let mut args = msgpack::Decoder::of(&sent[at + call.len()..]);
    assert_eq!(Some(msgpack::Token::Array(1)), args.next().unwrap());
    assert_eq!(Some(msgpack::Token::Int(1002)), args.next().unwrap());

    // closed, they take it with them
    fx.d.grids.wins.swap_remove(&3);
    fx.d.reconcile_layers();
    assert!(fx.d.panes.list.iter().all(|p| p.layer.is_none()));
}
