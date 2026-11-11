//! The float group pane lifecycle: a float group pane, which Rex shows as a
//! Rex floating pane, for each group of floats that took focus. It matches
//! panes to groups, says what to ask the pane provider for (`open_float` and
//! `close`), works out their bounds, and moves the floats to follow a pane the
//! user moved.
//!
//! `FloatingPanes` holds separate borrows of the daemon's parts, since the
//! daemon can't be borrowed whole while its Neovim session is.

use super::asks;
use super::dir::Address;
use super::event_loop::Loop;
use super::pane::{self, Close, Pane};
use super::panes::Panes;
use super::provider::{Ask, TerminalId};
use crate::attach::Transport;
use crate::nvim::windows;
use crate::screen::float_groups::{self, Group};
use crate::screen::grids::{Grids, WinKind};

use super::nvim_session::NvimSession;

/// About how many cells a Rex floating pane's frame takes, across and down.
/// Only a first guess: once made, its floats fit whatever size it is.
pub(crate) const LAYER_FRAME: [i32; 2] = [3, 2];

pub(crate) struct FloatingPanes<'a> {
    /// Our executable and the helpers' socket, for new panes' command.
    pub exe: &'a str,
    pub sock: &'a Address,
    pub transport: Transport,
    pub grids: &'a Grids,
    pub panes: &'a mut Panes,
    pub event_loop: &'a Loop,
    pub nvim: &'a mut NvimSession,
}

impl FloatingPanes<'_> {
    /// Float group pane `id` is at `bounds` in the live layout: if that's
    /// somewhere else, the user moved it, so its floats follow. Whether it was.
    pub(crate) fn follow_moved_layer(&mut self, id: u32, bounds: [f64; 4]) -> bool {
        let Some(l) = self.panes.by_id(id).and_then(|p| p.layer) else {
            return false;
        };
        if same_bounds(self.grids, bounds, l.bounds) {
            return false;
        }
        if let Some(l) = &mut self.panes.by_id_mut(id).unwrap().layer {
            l.bounds = bounds;
        }
        let _ = self.follow_layer(id);
        true
    }

    /// Matches float group panes to groups of floats: new groups get one, and
    /// those whose group closed hang up. A float group pane's size is Rex's:
    /// it's never moved to follow its floats, they follow it. Float group
    /// panes are only made if the pane provider can `open_float`, from
    /// `origin`, the current pane's terminal. What to ask for, it returns.
    pub(crate) fn reconcile_layers(
        &mut self,
        open_float: bool,
        origin: Option<TerminalId>,
    ) -> Vec<Ask> {
        let mut out = Vec::new();
        let groups = if open_float {
            float_groups::find(self.grids, self.nvim.entered_floats())
        } else {
            Vec::new()
        };
        let mut i = self.panes.list.len();
        while i > 0 {
            i -= 1;
            let p = &mut self.panes.list[i];
            let Some(l) = &mut p.layer else {
                continue;
            };
            if let Some(g) = groups.iter().find(|g| g.key == l.group.key) {
                l.group = *g;
                continue;
            }
            // without a terminal its open isn't answered yet: the answer closes it
            if let Some(t) = p.terminal.clone() {
                out.push(asks::close(t));
            }
            if p.closing.is_none() {
                p.closing = Some(Close::WindowClosed);
            }
            self.panes.remove(i, self.event_loop);
        }
        for g in groups {
            if self.panes.by_layer_key(g.key).is_none()
                && let Some(ask) = self.new_layer(g, origin.as_ref())
            {
                out.push(ask);
            }
        }
        out
    }

    /// A float group pane for `g`, and its `open_float`; None while there's no
    /// screen or no `origin` to open it from.
    fn new_layer(&mut self, g: Group, origin: Option<&TerminalId>) -> Option<Ask> {
        let origin = origin?.clone();
        let screen = self.grids.get(1)?;
        let (origin_cols, origin_rows) = (screen.width, screen.height);
        let bounds = layer_bounds(self.grids, &g)?;
        let pane = self.panes.add_for_group(g);
        pane.layer.as_mut().unwrap().bounds = bounds;
        let id = pane.attach_id.expect("a requested pane");
        let attach = pane::attach_words(self.exe, self.sock, self.transport, pane);
        // until #111, cells on the screen, which spans the Rex window
        Some(Ask::OpenFloat {
            pane: id,
            attach,
            title: pane::title(&id),
            origin,
            x: g.col,
            y: g.row,
            cols: g.width,
            rows: g.height,
            origin_cols,
            origin_rows,
            area_bounds: None,
        })
    }

    /// Fits a float group pane's floats to where it is and the size Rex gave it.
    /// Returns the request's id, if it sent one.
    pub(crate) fn follow_layer(&mut self, id: u32) -> Option<u32> {
        let p = self.panes.by_id(id)?;
        let l = p.layer.as_ref()?;
        let s = p.terminal_surface.as_ref()?;
        let screen = self.grids.get(1)?;
        let row =
            (l.bounds[1] * f64::from(screen.height)).round() as i32 + LAYER_FRAME[1].div_euclid(2);
        let col =
            (l.bounds[0] * f64::from(screen.width)).round() as i32 + LAYER_FRAME[0].div_euclid(2);
        let g = l.group;
        let (cols, rows) = (s.view.cols, s.view.rows);
        if row == g.row && col == g.col && cols == g.width && rows == g.height {
            return None;
        }
        let mut wins: Vec<i64> = Vec::with_capacity(16);
        for &top in g.top_grids() {
            if let Some(w) = self.grids.win_on(top) {
                wins.push(w.win);
            }
        }
        if wins.is_empty() {
            return None;
        }
        Some(windows::place_group(
            &mut self.nvim.client,
            &wins,
            row,
            col,
            cols,
            rows,
            [
                i64::from(g.row),
                i64::from(g.col),
                i64::from(g.width),
                i64::from(g.height),
            ],
        ))
    }
}

/// Bounds for a Rex floating pane around `g`, as fractions of the window,
/// leaving room for the frame Rex draws around it.
pub(crate) fn layer_bounds(grids: &Grids, g: &Group) -> Option<[f64; 4]> {
    let screen = grids.get(1)?;
    bounds_around(g.col, g.row, g.width, g.height, screen.width, screen.height)
}

/// Bounds for a Rex floating pane around `cols` x `rows` cells at `x`, `y` on a
/// screen of `screen_cols` x `screen_rows`, as fractions of it, leaving room
/// for the frame Rex draws around it. None on an empty screen.
pub(crate) fn bounds_around(
    x: i32,
    y: i32,
    cols: u32,
    rows: u32,
    screen_cols: u32,
    screen_rows: u32,
) -> Option<[f64; 4]> {
    if screen_cols == 0 || screen_rows == 0 {
        return None;
    }
    let sw = f64::from(screen_cols);
    let sh = f64::from(screen_rows);
    let fw = LAYER_FRAME[0];
    let fh = LAYER_FRAME[1];
    let w = ((i64::from(cols) + i64::from(fw)) as f64 / sw).min(1.0);
    let h = ((i64::from(rows) + i64::from(fh)) as f64 / sh).min(1.0);
    let x = (f64::from(x - fw.div_euclid(2)) / sw).clamp(0.0, 1.0 - w);
    let y = (f64::from(y - fh.div_euclid(2)) / sh).clamp(0.0, 1.0 - h);
    Some([x, y, w, h])
}

/// Within half a cell of each other on grid 1.
pub(crate) fn same_bounds(grids: &Grids, a: [f64; 4], b: [f64; 4]) -> bool {
    let Some(screen) = grids.get(1) else {
        return true;
    };
    let half = [
        0.5 / f64::from(screen.width.max(1)),
        0.5 / f64::from(screen.height.max(1)),
    ];
    for i in 0..4 {
        if (a[i] - b[i]).abs() >= half[i % 2] {
            return false;
        }
    }
    true
}

/// The float group pane showing float `win`, if one does.
pub(crate) fn layer_of_win<'a>(grids: &Grids, panes: &'a Panes, win: i64) -> Option<&'a Pane> {
    let grid = grids.grid_of(win)?;
    let top = grids.place(grid)?.top;
    if top == 0 {
        return None;
    }
    panes
        .list
        .iter()
        .find(|p| p.layer.is_some_and(|l| l.group.has_top(top)))
}

/// A float in group `g` that takes focus: the input of a picker, which
/// stacks above its box.
pub(crate) fn focusable_in(grids: &Grids, g: &Group) -> Option<i64> {
    let mut best: Option<(i64, u32)> = None;
    for (&id, w) in &grids.wins {
        if w.kind != WinKind::Float || !w.float.focusable {
            continue;
        }
        let Some(place) = grids.place(id) else {
            continue;
        };
        let top = place.top;
        if top == 0 || !g.has_top(top) {
            continue;
        }
        if best.is_none_or(|(_, z)| w.float.zindex > z) {
            best = Some((w.win, w.float.zindex));
        }
    }
    best.map(|(win, _)| win)
}
