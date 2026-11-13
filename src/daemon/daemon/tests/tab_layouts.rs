//! A pane layout per tab page (ADR 0012): with no pane provider, a tab page
//! is drawn whole in the pane it opened in, and `gt` swaps the pane between
//! tab pages as plain Neovim swaps its screen. Typed into the pane's terminal
//! of a daemon over a real Neovim.

use super::fallback::{Live, pump, started_with};
use super::*;

/// A daemon over Neovim with no pane provider, its one pane attached.
struct Lone {
    d: Daemon,
    live: Live,
    reader: std::thread::JoinHandle<()>,
}

impl Lone {
    /// None without Neovim.
    fn new() -> Option<Lone> {
        let (mut d, live, reader) = started_with("-n", None)?;
        pump(&mut d, |d| {
            d.nvim.ready
                && d.nvim.current_win != 0
                && d.panes.list.len() == 1
                && d.panes.list[0].neovim_window_handle == Some(d.nvim.current_win)
        });
        // keys come as typed, not a line at a time
        crate::term::raw::as_the_helper_does(live.pty.slave);
        Some(Lone { d, live, reader })
    }

    /// Types `bytes` into the pane's terminal.
    fn keys(&self, bytes: &[u8]) {
        // SAFETY: `bytes` is valid for its length.
        let n = unsafe { libc::write(self.live.pty.master, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(bytes.len() as isize, n);
    }

    /// The one pane.
    fn pane(&self) -> &Pane {
        assert_eq!(1, self.d.panes.list.len());
        &self.d.panes.list[0]
    }

    /// Runs the daemon until `done` says so of it and its one pane.
    fn until(&mut self, done: impl Fn(&Daemon, &Pane) -> bool) {
        pump(&mut self.d, |d| {
            d.panes.list.len() == 1 && done(d, &d.panes.list[0])
        });
    }

    /// `:vsplit` in the first tab page, then `:tabnew`. The windows of the
    /// first, left (current there) and right, its layout, and the second's
    /// window and tab page.
    fn two_tab_pages(&mut self) -> TwoTabPages {
        let right = self.d.nvim.current_win;
        self.keys(b":vsplit\r");
        self.until(|d, p| p.shown_windows().len() == 2 && d.nvim.current_win != right);
        let left = self.d.nvim.current_win;
        let layout = self.pane().layout.clone().unwrap();
        assert_eq!(vec![left, right], layout.windows());

        self.keys(b":tabnew\r");
        self.until(|d, p| p.shown_tab != 0 && p.neovim_window_handle == Some(d.nvim.current_win));
        let p = self.pane();
        TwoTabPages {
            left,
            right,
            layout,
            new: p.neovim_window_handle.unwrap(),
            tab: p.shown_tab,
        }
    }

    fn end(self) {
        drop(self.d);
        drop(self.live);
        self.reader.join().unwrap();
    }
}

struct TwoTabPages {
    left: i64,
    right: i64,
    layout: PaneLayout,
    new: i64,
    tab: i64,
}

/// Pane `p` holds, for tab page `tab`, its window last current `win`, and
/// `layout` if it has several.
fn assert_held(p: &Pane, tab: i64, win: i64, layout: Option<&PaneLayout>) {
    assert_eq!(1, p.held.len(), "{:?}", p.held);
    let h = &p.held[0];
    assert_eq!((tab, win, layout), (h.tab, h.win, h.layout.as_ref()));
}

#[test]
fn tabnew_then_gt_and_capital_gt_swap_a_lone_pane_between_two_tab_pages_layouts() {
    let Some(mut t) = Lone::new() else {
        return;
    };
    let two = t.two_tab_pages();
    // the new tab page whole in the pane, the first held
    let p = t.pane();
    assert_eq!(vec![two.new], p.shown_windows());
    assert!(p.layout.is_none());
    assert_held(p, 0, two.left, Some(&two.layout));

    t.keys(b"gt");
    t.until(|d, p| p.shown_tab == 0 && d.nvim.current_win == two.left);
    let p = t.pane();
    assert_eq!(Some(&two.layout), p.layout.as_ref());
    assert_eq!(Some(two.left), p.neovim_window_handle);
    assert_held(p, two.tab, two.new, None);
    let mut shown = vec![two.left, two.right];
    shown.sort_unstable();
    assert_eq!(shown, t.d.nvim.shown);

    t.keys(b"gT");
    t.until(|d, p| p.shown_tab == two.tab && d.nvim.current_win == two.new);
    let p = t.pane();
    assert_eq!(vec![two.new], p.shown_windows());
    assert_held(p, 0, two.left, Some(&two.layout));
    assert_eq!(vec![two.new], t.d.nvim.shown);
    t.end();
}

#[test]
fn a_window_opened_unentered_into_the_tab_page_a_pane_isnt_showing_waits_there_held() {
    let Some(mut t) = Lone::new() else {
        return;
    };
    let two = t.two_tab_pages();
    // a plugin splits a window of the first tab page, not entering it
    t.keys(
        format!(
            ":lua vim.api.nvim_win_call({}, function() vim.cmd('split') end)\r",
            two.left
        )
        .as_bytes(),
    );
    t.until(|_, p| {
        p.held
            .first()
            .and_then(|h| h.layout.as_ref())
            .is_some_and(|l| l.windows().len() == 3)
    });
    let p = t.pane();
    assert_eq!(
        (two.tab, Some(two.new)),
        (p.shown_tab, p.neovim_window_handle)
    );
    assert_eq!(vec![two.new], p.shown_windows());
    assert_eq!(two.new, t.d.nvim.current_win);
    let held = p.held[0].layout.as_ref().unwrap().windows();
    let split = *held
        .iter()
        .find(|w| ![two.left, two.right].contains(w))
        .unwrap();

    // `gt` shows it, at the window last current in the pane there
    t.keys(b"gt");
    t.until(|d, p| p.shown_tab == 0 && d.nvim.current_win == two.left);
    let p = t.pane();
    assert_eq!(held, p.shown_windows());
    assert!(p.shown_windows().contains(&split));
    t.end();
}

#[test]
fn tabclose_returns_the_pane_to_the_tab_page_left() {
    let Some(mut t) = Lone::new() else {
        return;
    };
    let two = t.two_tab_pages();
    t.keys(b":tabclose\r");
    // the first tab page's windows, reopened as they followed into the
    // closing one, back in the pane
    t.until(|d, p| {
        p.shown_tab == 0
            && p.held.is_empty()
            && p.shown_windows().len() == 2
            && p.shows(d.nvim.current_win)
            && d.grids.grid_of(d.nvim.current_win).is_some()
    });
    let p = t.pane();
    assert!(!p.holds(two.new));
    assert!(p.layout.as_ref().unwrap().is_split());
    assert_eq!(None, t.d.stopped);
    t.end();
}
