//! Screen and pane geometry: window and pane sizes, layout area sizes, and
//! where the screen's tabline falls over the panes of each layout area. Pure
//! reads of the grids and panes.
//!
//! `Geometry` holds separate borrows of the daemon's parts, since the daemon
//! can't be borrowed whole while its Neovim session is.

use super::floating_panes::LAYER_FRAME;
use super::live_layout::Area;
use super::pane::Pane;
use super::panes::Panes;
use crate::nvim::windows;
use crate::screen::grids::Grids;

/// A new Rex terminal's size, cols x rows, until the app lays it out.
pub(crate) const REX_DEFAULT_SIZE: [u32; 2] = [80, 24];

/// Where a new tab page's floating pane sits, as fractions of the window: x, y, w, h.
pub(crate) const TAB_LAYER_BOUNDS: [f64; 4] = [0.05, 0.05, 0.9, 0.9];

/// The grid showing the pane's window, if it has one yet.
pub(crate) fn grid_of_pane(grids: &Grids, pane: &Pane) -> Option<u32> {
    let win = pane.neovim_window_handle?;
    grids.grid_of(win)
}

pub(crate) struct Geometry<'a> {
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
    pub(crate) fn new_pane_size(
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
    pub(crate) fn tab_pane_size(&self) -> [u32; 2] {
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

    /// Where pane `p`'s top left sits on grid 1, which spans its layout
    /// area while it's current: row, col.
    pub(crate) fn pane_origin(&self, p: &Pane) -> [i32; 2] {
        let Some(r) = p.area.rect() else {
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
    pub(crate) fn window_size(&self, p: &Pane) -> [u32; 2] {
        let Some(s) = &p.terminal_surface else {
            return [1, 1];
        };
        let size = s.window_size(self.msg_rows);
        [
            size[0],
            size[1].saturating_sub(self.tabline_rows_of(p)).max(1),
        ]
    }

    /// The tabline's rows over pane `p`: those of a tabline pane, which
    /// shows its share of the screen's tabline.
    pub(crate) fn tabline_rows_of(&self, p: &Pane) -> u32 {
        if self.tabline_rows > 0 && is_tabline_pane(p) {
            self.tabline_rows
        } else {
            0
        }
    }

    /// The tabline width, the columns of the tabline panes of `current`'s
    /// layout area, while it's narrower than the area, which shows the
    /// tabline past it nowhere. None when it hides nothing, or there are no
    /// tabline panes there. Whether the tabline is on doesn't matter.
    pub(crate) fn tabline_width(&self, current: &Pane) -> Option<u32> {
        let screen = area_size(current)
            .map(|s| s[0])
            .or_else(|| Some(self.grids.get(1)?.width))?;
        let cols: u32 = self
            .panes
            .list
            .iter()
            .filter(|p| is_tabline_pane(p) && same_area(p, current))
            .filter_map(|p| p.terminal_surface.as_ref())
            .map(|s| s.view.cols)
            .sum();
        (cols > 0 && cols < screen).then_some(cols)
    }

    /// Whether pane `p` draws part of the screen's tabline. It needs its
    /// terminal: its width is what the panes right of it start after, and
    /// without one it draws nothing.
    pub(crate) fn shows_tabline(&self, p: &Pane) -> bool {
        p.terminal_surface.is_some() && self.tabline_rows_of(p) > 0
    }

    /// The column of the screen's tabline pane `p` starts at, if it shows any.
    /// The panes along the top of a layout area that show it lay it out left
    /// to right from column 0, whatever else is in the area, so each tab
    /// shows in exactly one of them. Panes at the same column go in list
    /// order. A pane closing in the middle of `draw` leaves the panes drawn
    /// before it on their old columns for that frame; the next draw sees
    /// their start move and `PaneView` redraws the row.
    pub(crate) fn tabline_start(&self, p: &Pane) -> Option<u32> {
        if !self.shows_tabline(p) {
            return None;
        }
        let mine = left(p);
        let mut col: u32 = 0;
        let mut listed_before = true;
        for q in &self.panes.list {
            if q.id == p.id {
                listed_before = false;
                continue;
            }
            if !self.shows_tabline(q) || !same_area(p, q) {
                continue;
            }
            let theirs = left(q);
            if theirs < mine || (theirs == mine && listed_before) {
                col += q.terminal_surface.as_ref().unwrap().view.cols;
            }
        }
        Some(col)
    }
}

/// Whether pane `p` is one of the tabline panes (#92 decision 17): a window
/// pane that is its own layout area, or along the top of a live one and not
/// floating. Never one that's in no area yet. It doesn't depend on which
/// pane is current, so no window gains or loses a row as focus moves.
pub(crate) fn is_tabline_pane(p: &Pane) -> bool {
    if p.layer.is_some() {
        return false;
    }
    match &p.area {
        Area::Own => true,
        Area::Live { rect, floating, .. } => !floating && rect[1].abs() < TOP_EPSILON,
        Area::Unreported => false,
    }
}

/// How far below its area's top a rect still counts as at the top.
const TOP_EPSILON: f64 = 1e-6;

/// Whether panes `a` and `b` share a layout area.
pub(crate) fn same_area(a: &Pane, b: &Pane) -> bool {
    match (&a.area, &b.area) {
        (Area::Live { id: x, .. }, Area::Live { id: y, .. }) => x == y,
        _ => a.id == b.id,
    }
}

/// Where pane `p`'s left edge is in its area, as a fraction.
fn left(p: &Pane) -> f64 {
    p.area.rect().map_or(0.0, |r| r[0])
}

/// The size of pane `p`'s layout area, in its cells: its cells over its
/// share of the area (ADR 0008). Neovim's screen is this, for the current
/// pane. None while it's in no area, or has no terminal.
pub(crate) fn area_size(p: &Pane) -> Option<[u32; 2]> {
    let s = p.terminal_surface.as_ref()?;
    let (cols, rows) = (s.view.cols, s.view.rows);
    match &p.area {
        Area::Unreported => None,
        Area::Own => Some([cols, rows]),
        Area::Live { rect: r, .. } if r[2] <= 0.0 || r[3] <= 0.0 => Some([cols, rows]),
        Area::Live { rect: r, .. } => Some([
            cols.max((f64::from(cols) / r[2]).round() as u32),
            rows.max((f64::from(rows) / r[3]).round() as u32),
        ]),
    }
}
