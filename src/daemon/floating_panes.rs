//! The float group pane lifecycle: a float group pane, a floating terminal
//! over its origin pane's layout area, for each group of floats that took
//! focus. It matches panes
//! to groups, says what to ask the pane provider for (`open_float`, with the
//! group's cells relative to its origin pane, and `close`), and moves the
//! floats to follow a pane the user moved: to its rect in its layout area,
//! inside its inset.
//!
//! `FloatingPanes` holds separate borrows of the daemon's parts, since the
//! daemon can't be borrowed whole while its Neovim session is.

use super::asks;
use super::dir::Address;
use super::event_loop::Loop;
use super::float_homes::FloatHomes;
use super::geometry;
use super::pane::{self, Close, Pane};
use super::panes::Panes;
use super::provider::{Ask, TerminalId};
use crate::attach::Transport;
use crate::nvim::windows;
use crate::screen::float_groups::{self, Group};
use crate::screen::grids::{Grids, WinKind};

use super::nvim_session::NvimSession;

pub(crate) struct FloatingPanes<'a> {
    /// Our executable and the helpers' socket, for new panes' command.
    pub exe: &'a std::path::Path,
    pub sock: &'a Address,
    pub transport: Transport,
    pub grids: &'a Grids,
    pub panes: &'a mut Panes,
    pub event_loop: &'a Loop,
    pub nvim: &'a mut NvimSession,
    /// Where the groups with no float group pane are drawn.
    pub homes: &'a mut FloatHomes,
}

impl FloatingPanes<'_> {
    /// Float group pane `id`'s terminal is at `rect` in the live layout,
    /// `inset` of it not its own: the first time, or if that's somewhere
    /// else, the user moved it, so its floats follow. Whether it was.
    pub(crate) fn follow_moved_terminal(
        &mut self,
        id: u32,
        rect: [f64; 4],
        inset: [u16; 4],
    ) -> bool {
        let Some(l) = self.panes.by_id(id).and_then(|p| p.layer) else {
            return false;
        };
        if l.rect.is_some_and(|r| same_bounds(self.grids, rect, r)) {
            return false;
        }
        if let Some(l) = &mut self.panes.by_id_mut(id).unwrap().layer {
            l.rect = Some(rect);
            l.inset = inset;
        }
        let _ = self.follow_layer(id);
        true
    }

    /// Matches float group panes to groups of floats: new groups get one, and
    /// those whose group closed hang up. A float group pane's size is the pane provider's:
    /// it's never moved to follow its floats, they follow it. Float group
    /// panes are only made if the pane provider can `open_float`, from
    /// `origin`, the current pane's terminal, for the top-most `MAX_GROUPS`
    /// groups; a group ranked out loses its float group pane. A group that
    /// met no `open_float` is drawn in a pane for life. While the screen is `shrunk` to the current pane, its
    /// bounds are the screen's. What to ask for, it returns.
    pub(crate) fn reconcile_layers(
        &mut self,
        open_float: bool,
        origin: Option<TerminalId>,
        shrunk: bool,
    ) -> Vec<Ask> {
        let mut out = Vec::new();
        let groups = float_groups::find(self.grids, self.nvim.entered_floats());
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
            // hidden in a tab page Neovim couldn't move them out of: the pane
            // keeps its last frame until that tab page is current again
            if float_groups::hidden(self.grids, &l.group) {
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
        self.homes.forget_closed(self.grids);
        if !open_float {
            // drawn in a pane for life: none to ask for, now or later
            for g in &groups {
                if self.panes.by_layer_key(g.key).is_none() {
                    self.homes.keep_in_pane(g.key);
                }
            }
            return out;
        }
        // the top-most groups that may have one, but for room kept by those
        // hidden in another tab page
        let hidden = self
            .panes
            .list
            .iter()
            .filter(|p| p.layer_hides())
            .filter(|p| float_groups::hidden(self.grids, &p.layer.as_ref().unwrap().group))
            .count();
        let top: Vec<Group> = groups
            .iter()
            .rev()
            .filter(|g| !self.in_pane_for_life(g.key))
            .take(float_groups::MAX_GROUPS.saturating_sub(hidden))
            .copied()
            .collect();
        // a group ranked out by a higher one loses its float group pane, and
        // is drawn in a pane until it ranks among the top-most again
        let mut i = self.panes.list.len();
        while i > 0 {
            i -= 1;
            let p = &mut self.panes.list[i];
            let Some(l) = &p.layer else {
                continue;
            };
            if !p.layer_hides()
                || top.iter().any(|g| g.key == l.group.key)
                || float_groups::hidden(self.grids, &l.group)
            {
                continue;
            }
            if let Some(t) = p.terminal.clone() {
                out.push(asks::close(t));
            }
            p.closing = Some(Close::WindowClosed);
            self.panes.remove(i, self.event_loop);
        }
        for g in top {
            if self.panes.by_layer_key(g.key).is_none()
                && let Some(ask) = self.new_layer(g, origin.as_ref(), shrunk)
            {
                out.push(ask);
            }
        }
        out
    }

    /// Whether group `key` is drawn in a pane for life: its `open_float`
    /// failed, timed out or was unsupported, or there was none to ask.
    fn in_pane_for_life(&self, key: i64) -> bool {
        self.homes.in_pane_for_life(key)
            || self
                .panes
                .by_layer_key(key)
                .is_some_and(|p| p.closing.is_some())
    }

    /// A float group pane for `g`, and its `open_float`; None while there's no
    /// screen or no `origin` to open it from.
    fn new_layer(&mut self, g: Group, origin: Option<&TerminalId>, shrunk: bool) -> Option<Ask> {
        let origin = origin?.clone();
        // shrunk, the screen is the origin pane
        let from = self.panes.by_terminal(origin.as_str()).filter(|_| !shrunk);
        let b = group_bounds(self.grids, from, &g)?;
        let pane = self.panes.add_for_group(g);
        let id = pane.attach_id.expect("a requested pane");
        let attach = pane::attach_words(self.exe, self.sock, self.transport, pane);
        Some(Ask::OpenFloat {
            pane: id,
            attach,
            title: pane::title(&id),
            origin,
            x: b.at[0],
            y: b.at[1],
            cols: g.width,
            rows: g.height,
            origin_cols: b.origin_size[0],
            origin_rows: b.origin_size[1],
            area_bounds: b.area_bounds,
        })
    }

    /// Fits a float group pane's floats to where it is and the size the pane provider gave it.
    /// Returns the request's id, if it sent one.
    pub(crate) fn follow_layer(&mut self, id: u32) -> Option<u32> {
        let p = self.panes.by_id(id)?;
        let l = p.layer.as_ref()?;
        let s = p.terminal_surface.as_ref()?;
        let screen = self.grids.get(1)?;
        let g = l.group;
        // where the live layout has it, else where its floats are
        let [col, row] = match l.rect {
            Some(r) => [
                (r[0] * f64::from(screen.width)).round() as i32 + i32::from(l.inset[0]),
                (r[1] * f64::from(screen.height)).round() as i32 + i32::from(l.inset[1]),
            ],
            None => [g.col, g.row],
        };
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

/// Where a float group is, as `open_float` asks for it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct GroupBounds {
    /// Its top left in cells from the top left of its origin pane's
    /// content, inside its inset: x, y.
    pub at: [i32; 2],
    /// The origin pane's terminal's size, cols x rows.
    pub origin_size: [u32; 2],
    /// The group as fractions of the origin's layout area, if that's a
    /// live one.
    pub area_bounds: Option<[f64; 4]>,
}

/// Where float group `g` is relative to the content of pane `origin`, the
/// one its pane opens from, inside its inset: on grid 1, which spans
/// `origin`'s layout area while it's the current pane. Without the pane,
/// its terminal or a live area, the origin is the screen: its top left the
/// screen's, and its size grid 1's. None with no screen.
pub(crate) fn group_bounds(grids: &Grids, origin: Option<&Pane>, g: &Group) -> Option<GroupBounds> {
    let screen = grids.get(1)?;
    let (sw, sh) = (f64::from(screen.width), f64::from(screen.height));
    if screen.width == 0 || screen.height == 0 {
        return None;
    }
    let rect = origin.and_then(|p| p.area.rect());
    // its content's, inside its inset
    let top_left = origin
        .and_then(|p| geometry::content_corner(&p.area, [screen.width, screen.height]))
        .unwrap_or([0, 0]);
    // the terminal's size only with the rect it's at: cells from the
    // screen's top left are counted in the screen
    let origin_size = rect
        .and(origin.and_then(|p| p.terminal_surface.as_ref()))
        .map_or([screen.width, screen.height], |s| {
            [s.view.cols, s.view.rows]
        });
    let area_bounds = rect.map(|_| {
        [
            f64::from(g.col) / sw,
            f64::from(g.row) / sh,
            f64::from(g.width) / sw,
            f64::from(g.height) / sh,
        ]
    });
    Some(GroupBounds {
        at: [g.col - top_left[0], g.row - top_left[1]],
        origin_size,
        area_bounds,
    })
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
