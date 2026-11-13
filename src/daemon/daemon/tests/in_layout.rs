//! Keys, clicks and `<C-w>hjkl` in a pane laying out several windows (#113),
//! typed into the pane's terminal of a daemon over a real Neovim.

use super::fallback::{Live, pump, started_with};
use super::*;

/// A daemon over Neovim started with `-O3` and `provider`, whose three
/// windows are laid out side by side in its first pane, which `keys` types
/// into. None without Neovim.
struct Three {
    d: Daemon,
    live: Live,
    reader: std::thread::JoinHandle<()>,
    /// The windows, left to right.
    wins: [i64; 3],
}

impl Three {
    fn new(provider: Option<Box<dyn PaneProvider>>) -> Option<Three> {
        let (mut d, live, reader) = started_with(&["-O3"], provider)?;
        pump(&mut d, |d| {
            d.nvim.ready && d.panes.list.len() == 1 && d.panes.list[0].windows().len() == 3
        });
        // keys come as typed, not a line at a time
        crate::term::raw::as_the_helper_does(live.pty.slave);
        let (wins, _) = d.laid_out_view(&d.panes.list[0]);
        let mut wins: Vec<(u32, i64)> = wins
            .iter()
            .map(|w| (w.col, d.grids.win_on(w.grid).unwrap().win))
            .collect();
        wins.sort();
        let wins = [wins[0].1, wins[1].1, wins[2].1];
        Some(Three {
            d,
            live,
            reader,
            wins,
        })
    }

    /// Types `bytes` into the pane's terminal.
    fn keys(&self, bytes: &[u8]) {
        // SAFETY: `bytes` is valid for its length.
        let n = unsafe { libc::write(self.live.pty.master, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(bytes.len() as isize, n);
    }

    /// Clicks the left button at `col`, `row` of the pane's terminal.
    fn click(&self, col: u32, row: u32) {
        let (col, row) = (col + 1, row + 1);
        self.keys(format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m").as_bytes());
    }

    /// Runs the daemon until Neovim's current window is `win`.
    fn until_current(&mut self, win: i64) {
        pump(&mut self.d, |d| d.nvim.current_win == win);
    }

    /// The `focus_direction` asks so far.
    fn directions(&mut self) -> Vec<Ask> {
        match self.d.provider.as_mut() {
            Some(p) => p
                .fake()
                .asked()
                .into_iter()
                .filter(|a| matches!(a, Ask::FocusDirection { .. }))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn end(self) {
        drop(self.d);
        drop(self.live);
        self.reader.join().unwrap();
    }
}

#[test]
fn two_ctrl_w_l_in_a_pane_of_three_crosses_one_window_there_then_asks_to_focus_one_right() {
    let f = FakeProvider::with(&[Capability::FocusDirection]);
    let Some(mut t) = Three::new(Some(Box::new(f))) else {
        return;
    };
    let [left, mid, right] = t.wins;
    t.until_current(left);
    t.keys(b"\x17l");
    t.until_current(mid);
    assert!(t.directions().is_empty());
    t.keys(b"2\x17l");
    pump(&mut t.d, |d| {
        d.provider
            .as_ref()
            .is_some_and(|p| !p.fake_ref().asked().is_empty())
    });
    assert_eq!(right, t.d.nvim.current_win);
    assert_eq!(
        vec![Ask::FocusDirection {
            dir: Direction::Right,
            count: 1
        }],
        t.directions()
    );
    t.end();
}

#[test]
fn without_focus_direction_a_move_stops_at_the_panes_edge() {
    let Some(mut t) = Three::new(Some(Box::new(FakeProvider::with(&[])))) else {
        return;
    };
    let [left, _, right] = t.wins;
    t.until_current(left);
    t.keys(b"5\x17l");
    t.until_current(right);
    t.keys(b"\x17h\x17h");
    t.until_current(left);
    assert!(fake(&mut t.d).asked().is_empty());
    t.end();
}

#[test]
fn a_click_on_the_second_window_makes_it_current_and_one_on_a_separator_does_nothing() {
    let Some(mut t) = Three::new(None) else {
        return;
    };
    let [left, mid, _] = t.wins;
    t.until_current(left);
    let (shown, separators) = t.d.laid_out_view(&t.d.panes.list[0]);
    let second = shown[1];
    assert_eq!(Some(mid), t.d.grids.win_on(second.grid).map(|w| w.win));
    t.click(second.col + 2, 0);
    t.until_current(mid);

    // lines to click among, the cursor on the first
    t.keys(b":call setline(1, repeat(['x'], 30))\r");
    // the separator right of it, level with line 11: had it reached the
    // window, its cursor would be there, and the Y typed after it
    let sep = separators[1];
    assert_eq!(second.col + second.cols, sep.col);
    t.click(sep.col, 10);
    t.keys(b"rY");
    let grid = second.grid;
    pump(&mut t.d, |d| {
        d.grids.get(grid).unwrap().row(0)[0].str() == b"Y"
    });
    assert_eq!(mid, t.d.nvim.current_win);
    t.end();
}
