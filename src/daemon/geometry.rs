//! Screen and pane geometry: window and pane sizes, and where the screen's
//! tabline falls over the panes. Pure reads of the grids and panes.
//!
//! `Geometry` holds separate borrows of the daemon's parts, since the daemon
//! can't be borrowed whole while its Neovim session is.

use super::floating_panes::LAYER_FRAME;
use super::pane::Pane;
use super::panes::Panes;
use crate::nvim::windows;
use crate::screen::grids::Grids;

/// A new Rex terminal's size, cols x rows, until the app lays it out.
pub const REX_DEFAULT_SIZE: [u32; 2] = [80, 24];

/// Where a new tab page's floating pane sits, as fractions of the window: x, y, w, h.
pub const TAB_LAYER_BOUNDS: [f64; 4] = [0.05, 0.05, 0.9, 0.9];

/// The grid showing the pane's window, if it has one yet.
pub fn grid_of_pane(grids: &Grids, pane: &Pane) -> Option<u32> {
    let win = pane.neovim_window_handle?;
    grids.grid_of(win)
}

pub struct Geometry<'a> {
    pub grids: &'a Grids,
    pub panes: &'a Panes,
    /// Rows the screen's tabline takes, as Neovim last said.
    pub tabline_rows: u32,
    /// Rows at the bottom of every pane kept for Neovim's messages.
    pub msg_rows: u32,
}

impl Geometry<'_> {
    /// A guess at a new split's terminal size, cols x rows: half the pane it
    /// splits (`from`). The first of a tab page gets its floating pane's size.
    pub fn new_pane_size(
        &self,
        sp: windows::Split,
        from: Option<&Pane>,
        first_of_tab: bool,
    ) -> [u32; 2] {
        if first_of_tab {
            return self.tab_pane_size();
        }
        let Some(p) = from else {
            return REX_DEFAULT_SIZE;
        };
        let Some(s) = &p.terminal_surface else {
            return REX_DEFAULT_SIZE;
        };
        let cols = s.view.cols.max(1);
        let rows = s.view.rows.max(2);
        if sp.side_by_side {
            return [(cols / 2).max(1), rows];
        }
        [cols, (rows / 2).max(2)]
    }

    /// A guess at a new tab page's floating pane's terminal size, cols x rows.
    pub fn tab_pane_size(&self) -> [u32; 2] {
        let Some(screen) = self.grids.get(1) else {
            return REX_DEFAULT_SIZE;
        };
        let w = (f64::from(screen.width) * TAB_LAYER_BOUNDS[2]) as i64;
        let h = (f64::from(screen.height) * TAB_LAYER_BOUNDS[3]) as i64;
        [
            (w - i64::from(LAYER_FRAME[0])).max(1) as u32,
            (h - i64::from(LAYER_FRAME[1])).max(2) as u32,
        ]
    }

    /// Where pane `p`'s top left sits on grid 1: row, col.
    pub fn pane_origin(&self, p: &Pane) -> [i32; 2] {
        let Some(r) = p.rect else {
            return [0, 0];
        };
        let Some(screen) = self.grids.get(1) else {
            return [0, 0];
        };
        [
            (r[1] * f64::from(screen.height)).round() as i32,
            (r[0] * f64::from(screen.width)).round() as i32,
        ]
    }

    /// No terminal yet: the smallest window, as Neovim allows.
    pub fn window_size(&self, p: &Pane) -> [u32; 2] {
        let Some(s) = &p.terminal_surface else {
            return [1, 1];
        };
        let size = s.window_size(self.msg_rows);
        [
            size[0],
            size[1].saturating_sub(self.tabline_rows_of(p)).max(1),
        ]
    }

    /// The tabline's rows over pane `p`: those of a window's pane along the top
    /// of the Rex window, which shows its share of the screen's tabline.
    /// A pane Rex hasn't placed yet counts only while none has been: it would
    /// otherwise take the top left from the panes already there, until the
    /// layout arrives. Without Rex's layout at all, the one pane is the window.
    pub fn tabline_rows_of(&self, p: &Pane) -> u32 {
        if self.tabline_rows == 0 || p.layer.is_some() || p.tab != 0 {
            return 0;
        }
        if p.rect.is_none() && self.panes.list.iter().any(|q| q.rect.is_some()) {
            return 0;
        }
        if self.pane_origin(p)[0] == 0 {
            self.tabline_rows
        } else {
            0
        }
    }

    /// Whether pane `p` draws part of the screen's tabline. It needs its
    /// terminal: its width is what the panes right of it start after, and
    /// without one it draws nothing.
    pub fn shows_tabline(&self, p: &Pane) -> bool {
        p.terminal_surface.is_some() && self.tabline_rows_of(p) > 0
    }

    /// The column of the screen's tabline pane `p` starts at, if it shows any.
    /// The panes along the top that show it lay it out left to right from
    /// column 0, whatever else is in the Rex window, so each tab shows in
    /// exactly one of them. Panes at the same column go in list order.
    /// A pane closing in the middle of `draw` leaves the panes drawn before it
    /// on their old columns for that frame; the next draw sees their start
    /// move and `PaneView` redraws the row.
    pub fn tabline_start(&self, p: &Pane) -> Option<u32> {
        if !self.shows_tabline(p) {
            return None;
        }
        let mine = self.pane_origin(p)[1];
        let mut col: u32 = 0;
        let mut listed_before = true;
        for q in &self.panes.list {
            if q.id == p.id {
                listed_before = false;
                continue;
            }
            if !self.shows_tabline(q) {
                continue;
            }
            let theirs = self.pane_origin(q)[1];
            if theirs < mine || (theirs == mine && listed_before) {
                col += q.terminal_surface.as_ref().unwrap().view.cols;
            }
        }
        Some(col)
    }
}
