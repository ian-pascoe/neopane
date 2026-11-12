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

/// The windows each `<name>.<call>(...)` in `all` was given, in order.
fn lists_sent(all: &[u8], call: &str) -> Vec<Vec<i64>> {
    let call = format!("{NAME}.{call}(...)");
    let call = call.as_bytes();
    let mut sent = Vec::new();
    let mut at = 0;
    while let Some(i) = all[at..].windows(call.len()).position(|w| w == call) {
        at += i + call.len();
        let mut args = msgpack::Decoder::of(&all[at..]);
        assert_eq!(Some(msgpack::Token::Array(1)), args.next().unwrap());
        let Some(msgpack::Token::Array(n)) = args.next().unwrap() else {
            panic!("not a list");
        };
        let wins = (0..n)
            .map(|_| match args.next().unwrap() {
                Some(msgpack::Token::Int(w)) => w,
                t => panic!("not a window: {t:?}"),
            })
            .collect();
        sent.push(wins);
    }
    sent
}

#[test]
fn the_editor_floats_of_float_group_panes_are_told_to_neovim_to_carry_when_they_change() {
    let mut fx = fixture();
    fx.d.nvim.ready = true;
    show_picker(&mut fx.d);
    fx.d.reconcile_layers();
    fx.d.reconcile_layers();
    assert_eq!(vec![vec![1002]], lists_sent(&sent_bytes(&mut fx), "carry"));

    // its float closes, and its pane with it
    fx.d.grids.wins.swap_remove(&3);
    fx.d.reconcile_layers();
    assert_eq!(
        vec![Vec::<i64>::new()],
        lists_sent(&sent_bytes(&mut fx), "carry")
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
        lists_sent(&sent_bytes(&mut fx), "panes_show")
    );

    fx.d.panes.add(Pane::from_neovim_window(2, 1000));
    fx.d.after_batch().unwrap();
    assert_eq!(
        vec![vec![999, 1000, 1001]],
        lists_sent(&sent_bytes(&mut fx), "panes_show")
    );
}
