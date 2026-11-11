//! What one pane's terminal shows: a window's grid from the top left, the
//! floats over it and, in the focused pane, the screen's floats and the
//! message grid over the bottom rows. Remembers what it drew so each frame
//! sends only what changed.
//!
//! Floats are drawn into their window's pane, except groups shown in Rex
//! floating panes, which `frame_floats` draws on their own.

use indexmap::IndexMap;

use super::grid::{Cell, DirtyRows, Grid, Scroll, blank_cut_wide};
use super::grids::{Grids, MAX_FLOATS, Placed};
use super::render;
use super::style::{CursorMode, Style};

#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct PaneView {
    /// The terminal's size.
    pub cols: u32,
    pub rows: u32,
    /// Draw everything on the next frame.
    pub(crate) full: bool,
    /// The grid drawn last, and its size.
    pub(crate) drawn_grid: u32,
    pub(crate) drawn_size: [u32; 2],
    /// Message rows drawn over the bottom last frame.
    pub(crate) drawn_msg: u32,
    /// Tabline rows drawn over the top last frame.
    pub(crate) drawn_tabline: u32,
    /// The tabline column the row shown there started at.
    pub(crate) drawn_tabline_col: u32,
    pub(crate) drawn_cursor: Option<render::Cursor>,
    /// The cursor's shape and blink as last drawn, with the cursor.
    pub(crate) drawn_cursor_mode: Option<CursorMode>,
    /// The floats drawn last frame.
    pub(crate) drawn_floats: [Rect; MAX_FLOATS],
    pub(crate) drawn_floats_len: u8,
    /// The windows `frame_split` drew last, and their separators.
    pub(crate) drawn_split: Vec<Shown>,
    pub(crate) drawn_separators: Vec<Separator>,
}

/// A window's grid shown at a rect of a pane's window area (below its
/// tabline row), its statusline included. A grid taller than its rect shows
/// its first rows and its last, the statusline: a window squeezed to one row
/// shows just that.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct Shown {
    pub grid: u32,
    pub row: u32,
    pub col: u32,
    pub cols: u32,
    pub rows: u32,
}

/// A separator column between windows side by side, in the window area.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct Separator {
    pub row: u32,
    pub col: u32,
    pub rows: u32,
}

/// Several windows laid out in one pane, as Neovim lays out splits.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub struct Split<'a> {
    pub wins: &'a [Shown],
    pub separators: &'a [Separator],
    /// What a separator is drawn with: `'fillchars'`'s `vert` in WinSeparator.
    pub separator: Cell,
}

/// `drawn_grid` after `frame_split`, which draws no one grid.
const SPLIT_GRID: u32 = u32::MAX;

/// How a pane draws the screen's floats and messages.
#[derive(Debug, Clone, Copy, Default)]
#[doc(hidden)]
pub struct Opts<'a> {
    /// Draws the messages and the screen's floats.
    pub focused: bool,
    /// The pane's top left on grid 1, which spans the Rex window: screen
    /// floats move by it, then into the pane.
    pub origin: [i32; 2],
    /// Editor floats shown elsewhere, with every float hanging from them.
    pub skip: &'a [u32],
    /// The screen's tabline over this pane, for a pane along the top of the
    /// Rex window: its row of grid 1 from `tabline_col`, which the panes
    /// along the top lay out from column 0, left to right. The window shows
    /// below it.
    pub tabline: Option<&'a [Cell]>,
    /// The column of the screen's tabline that `tabline` starts at. When
    /// it moves, the row is drawn again even if the tabline didn't change.
    pub tabline_col: u32,
    /// The tabline changed since the last frame.
    pub tabline_dirty: bool,
}

/// A float as drawn, in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub(crate) grid: u32,
    pub(crate) row: i32,
    pub(crate) col: i32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) on_screen: bool,
    pub(crate) on_msg: bool,
    /// The editor float it hangs from, itself included; 0 if none.
    pub(crate) top: u32,
}

impl Rect {
    fn rows(&self, limit: u32) -> (u32, u32) {
        let top = (self.row as i64).clamp(0, limit as i64) as u32;
        let bot = (self.row as i64 + self.height as i64).clamp(0, limit as i64) as u32;
        (top, bot)
    }
}

/// Widest row floats are drawn into.
const MAX_COLS: usize = 1024;
/// Tallest terminal a floating pane draws into.
const MAX_ROWS: usize = 512;

impl PaneView {
    pub fn init(cols: u32, rows: u32) -> PaneView {
        PaneView {
            cols,
            rows,
            full: true,
            drawn_grid: 0,
            drawn_size: [0, 0],
            drawn_msg: 0,
            drawn_tabline: 0,
            drawn_tabline_col: 0,
            drawn_cursor: None,
            drawn_cursor_mode: None,
            drawn_floats: [Rect::default(); MAX_FLOATS],
            drawn_floats_len: 0,
            drawn_split: Vec::new(),
            drawn_separators: Vec::new(),
        }
    }

    /// The terminal changed size; its old contents can't be trusted.
    pub(crate) fn resize(&mut self, cols: u32, rows: u32) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.full = true;
    }

    /// Draws a frame if anything changed: grid `grid_id` (None for none yet)
    /// and, if `focused`, the messages. Clears the grid's dirty rows but not
    /// the message grid's, which the caller clears once every pane is drawn.
    /// True if it wrote anything.
    pub fn frame(
        &mut self,
        grids: &mut Grids,
        grid_id: Option<u32>,
        opts: Opts<'_>,
        w: &mut Vec<u8>,
    ) -> bool {
        let focused = opts.focused;
        grids.style.clear_blends();
        let id = grid_id.unwrap_or(0);
        // (keeps grid ids and looks them up where they're used)
        let gid: Option<u32> = grid_id.filter(|i| grids.grids.contains_key(i));
        let size: [u32; 2] = match gid.and_then(|i| grids.get(i)) {
            Some(gr) => [gr.width, gr.height],
            None => [0, 0],
        };
        if id != self.drawn_grid || size != self.drawn_size {
            self.full = true;
        }
        // rows over the window's grid: grid row i shows at terminal row top + i
        let top: u32 = if opts.tabline.is_some() {
            1.min(self.rows)
        } else {
            0
        };
        if top != self.drawn_tabline {
            self.full = true;
        }

        // the message grid's rows that show, from the bottom
        let msg_total = if focused { grids.msg_rows() } else { 0 };
        let msg_n = msg_total.min(self.rows);
        let msg_id: Option<u32> = if msg_n > 0 {
            grids.msg_grid.filter(|i| grids.grids.contains_key(i))
        } else {
            None
        };
        let msg_top = self.rows - msg_n;
        let grid_rows = size[1].min(msg_top.saturating_sub(top));

        let mut rects = if gid.is_some() {
            let placed = grids.floats_over(id, focused, opts.skip);
            let mut rects = float_rects(grids, &placed);
            self.fit_screen(&mut rects, opts.origin, screen_rows(grids));
            rects
        } else {
            Vec::new()
        };
        // screen floats were placed in the terminal; from here on rects are on the window's grid
        for r in rects.iter_mut() {
            if r.on_screen {
                r.row -= top as i32;
            }
        }
        let floats_moved = !same_rects(&rects, self.drawn_floats());
        let mut floats_changed = floats_moved;
        for r in &rects {
            let f = grids.get(r.grid).unwrap();
            if f.dirty.count() > 0 || f.scrolls_len > 0 {
                floats_changed = true;
            }
        }

        let cur = self.cursor_at(
            grids,
            gid.and_then(|i| grids.get(i)),
            id,
            top,
            grid_rows,
            msg_id.and_then(|i| grids.get(i)),
            msg_total,
            msg_n,
            &rects,
        );
        let grid_changed = floats_changed
            || match gid.and_then(|i| grids.get(i)) {
                Some(gr) => gr.dirty.count() > 0 || gr.scrolls_len > 0,
                None => false,
            };
        let msg_changed = match msg_id.and_then(|i| grids.get(i)) {
            Some(m) => m.dirty.count() > 0 || msg_n != self.drawn_msg,
            None => msg_n != self.drawn_msg,
        };
        let mut tabline_changed = top > 0
            && (self.full || opts.tabline_dirty || opts.tabline_col != self.drawn_tabline_col);
        if !self.full
            && !grid_changed
            && !msg_changed
            && !tabline_changed
            && self.cursor_unchanged(cur, &grids.style)
        {
            return false;
        }

        render::begin(w);
        let mut current: Option<u32> = None;
        if self.full {
            // ED fills with the current background
            render::set_hl(&grids.style, 0, w, &mut current);
            w.extend_from_slice(b"\x1b[2J");
            if let Some(gid) = gid {
                let gr = grids.grids.get_mut(&gid).unwrap();
                gr.dirty.set_all();
                gr.scrolls_len = 0;
            }
        } else if let Some(gid) = gid {
            // rows under floats that moved, changed or went. (Gathered before
            // the grid is borrowed mutably; setting dirty bits commutes with
            // `scrolls`, which only sets them too.)
            let mut marks: Vec<u32> = Vec::new();
            if floats_moved {
                for r in self.drawn_floats() {
                    let (t, b) = r.rows(grid_rows);
                    marks.extend(t..b);
                }
            }
            for r in &rects {
                let f = grids.get(r.grid).unwrap();
                if floats_moved || f.scrolls_len > 0 {
                    let (t, b) = r.rows(grid_rows);
                    marks.extend(t..b);
                    continue;
                }
                for fr in f.dirty.iter() {
                    let tr = r.row as i64 + fr as i64;
                    if tr >= 0 && tr < grid_rows as i64 {
                        marks.push(tr as u32);
                    }
                }
            }
            let gr = grids.grids.get_mut(&gid).unwrap();
            // rows the messages covered last frame show them still, so a scroll
            // over them can't be replayed: it would carry the message up
            let clear_rows =
                grid_rows.min((self.rows - self.drawn_msg.min(self.rows)).saturating_sub(top));
            self.scrolls(gr, top, clear_rows, &rects, w);
            for m in marks {
                gr.dirty.set(m as usize);
            }
            // rows the messages covered last frame and no longer do
            let old_top = self.rows - self.drawn_msg.min(self.rows);
            for r in old_top.min(msg_top)..msg_top {
                if r < top {
                    tabline_changed = true;
                } else if r - top < grid_rows {
                    gr.dirty.set((r - top) as usize);
                } else {
                    render::clear_row(&grids.style, r, w, &mut current);
                }
            }
        } else {
            let old_top = self.rows - self.drawn_msg.min(self.rows);
            for r in old_top.min(msg_top)..msg_top {
                if r < top {
                    tabline_changed = true;
                } else {
                    render::clear_row(&grids.style, r, w, &mut current);
                }
            }
        }
        if tabline_changed && let Some(row) = opts.tabline {
            let cut = &row[..row.len().min(self.cols as usize)];
            render::draw_row(cut, &grids.style, 0, self.cols, w, &mut current);
        }
        if let Some(gid) = gid {
            {
                let gmap = &grids.grids;
                let style = &mut grids.style;
                let gr = &gmap[&gid];
                let mut line = [Cell::default(); MAX_COLS];
                for r in gr.dirty.iter() {
                    if r >= grid_rows as usize {
                        break;
                    }
                    let row = gr.row(r as u32);
                    let shown = compose(
                        gmap,
                        style,
                        row,
                        &rects,
                        r as u32,
                        self.cols.min(MAX_COLS as u32),
                        &mut line,
                    );
                    render::draw_row(shown, style, top + r as u32, self.cols, w, &mut current);
                }
            }
            grids.grids.get_mut(&gid).unwrap().dirty.unset_all();
        }
        if let Some(mid) = msg_id
            && (self.full || msg_changed)
        {
            let m = &grids.grids[&mid];
            for i in 0..msg_n {
                let src = msg_total - msg_n + i;
                render::draw_row(
                    m.row(src),
                    &grids.style,
                    msg_top + i,
                    self.cols,
                    w,
                    &mut current,
                );
            }
        }
        render::end(&grids.style, cur, w);

        self.full = false;
        self.drawn_grid = id;
        self.drawn_size = size;
        self.drawn_msg = msg_n;
        self.drawn_tabline = top;
        self.drawn_tabline_col = opts.tabline_col;
        self.drawn_cursor = cur;
        self.drawn_cursor_mode = grids.style.cursor_mode();
        self.drawn_floats[0..rects.len()].copy_from_slice(&rects);
        self.drawn_floats_len = rects.len() as u8;
        true
    }

    /// Draws a frame if anything changed: the windows of `split` at their
    /// rects, with the separators between and the floats over them, and, as
    /// `frame` does, the tabline and, if focused, the screen's floats and the
    /// messages. Rows are drawn again rather than scrolls repeated, as a
    /// window's scroll moves only its part of a row. True if it wrote anything.
    pub fn frame_split(
        &mut self,
        grids: &mut Grids,
        split: Split<'_>,
        opts: Opts<'_>,
        w: &mut Vec<u8>,
    ) -> bool {
        let focused = opts.focused;
        grids.style.clear_blends();
        let wins: Vec<Shown> = split
            .wins
            .iter()
            .copied()
            .filter(|s| grids.grids.contains_key(&s.grid))
            .collect();
        if self.drawn_grid != SPLIT_GRID
            || self.drawn_split != wins
            || self.drawn_separators != split.separators
        {
            self.full = true;
        }
        let top: u32 = if opts.tabline.is_some() {
            1.min(self.rows)
        } else {
            0
        };
        if top != self.drawn_tabline {
            self.full = true;
        }
        let msg_total = if focused { grids.msg_rows() } else { 0 };
        let msg_n = msg_total.min(self.rows);
        let msg_id: Option<u32> = if msg_n > 0 {
            grids.msg_grid.filter(|i| grids.grids.contains_key(i))
        } else {
            None
        };
        let msg_top = self.rows - msg_n;
        let area_rows = msg_top.saturating_sub(top);

        // the floats over each window, then the screen's, in the window area
        let mut rects: Vec<Rect> = Vec::new();
        for (i, s) in wins.iter().enumerate() {
            let placed = grids.floats_over(s.grid, focused && i == 0, opts.skip);
            for mut r in float_rects(grids, &placed) {
                if !r.on_screen {
                    r.row += s.row as i32;
                    r.col += s.col as i32;
                }
                rects.push(r);
            }
        }
        if wins.is_empty() && focused {
            let placed = grids.floats_over(0, true, opts.skip);
            rects = float_rects(grids, &placed);
        }
        if rects.len() > MAX_FLOATS {
            rects.drain(0..rects.len() - MAX_FLOATS);
        }
        self.fit_screen(&mut rects, opts.origin, screen_rows(grids));
        for r in rects.iter_mut() {
            if r.on_screen {
                r.row -= top as i32;
            }
        }
        let floats_moved = !same_rects(&rects, self.drawn_floats());

        // the window area's rows to draw
        let mut dirty = DirtyRows::default();
        dirty.resize(area_rows as usize, false);
        if self.full {
            dirty.set_all();
        } else {
            for s in &wins {
                let g = grids.get(s.grid).unwrap();
                for i in 0..s.rows {
                    let r = s.row + i;
                    if r >= area_rows {
                        break;
                    }
                    let changed = match shown_row(g, s, i) {
                        Some(gr) => g.scrolls_len > 0 || g.dirty.is_set(gr as usize),
                        None => false,
                    };
                    if changed {
                        dirty.set(r as usize);
                    }
                }
            }
            let mut mark = |r: &Rect, all: bool, g: &Grid| {
                if all {
                    let (t, b) = r.rows(area_rows);
                    for i in t..b {
                        dirty.set(i as usize);
                    }
                    return;
                }
                for fr in g.dirty.iter() {
                    let tr = r.row as i64 + fr as i64;
                    if tr >= 0 && tr < area_rows as i64 {
                        dirty.set(tr as usize);
                    }
                }
            };
            if floats_moved {
                let blank = Grid::default();
                for r in self.drawn_floats() {
                    mark(r, true, &blank);
                }
            }
            for r in &rects {
                let f = grids.get(r.grid).unwrap();
                mark(r, floats_moved || f.scrolls_len > 0, f);
            }
            // rows the messages covered last frame and no longer do
            let old_top = self.rows - self.drawn_msg.min(self.rows);
            for r in old_top.min(msg_top)..msg_top {
                if r >= top {
                    dirty.set((r - top) as usize);
                }
            }
        }
        let msg_changed = match msg_id.and_then(|i| grids.get(i)) {
            Some(m) => m.dirty.count() > 0 || msg_n != self.drawn_msg,
            None => msg_n != self.drawn_msg,
        };
        let old_msg_top = self.rows - self.drawn_msg.min(self.rows);
        let tabline_changed = top > 0
            && (self.full
                || opts.tabline_dirty
                || opts.tabline_col != self.drawn_tabline_col
                || old_msg_top.min(msg_top) < top);
        let cur = self.split_cursor(grids, &wins, &rects, top, area_rows, msg_total, msg_n);
        if !self.full
            && dirty.count() == 0
            && !msg_changed
            && !tabline_changed
            && self.cursor_unchanged(cur, &grids.style)
        {
            return false;
        }

        render::begin(w);
        let mut current: Option<u32> = None;
        if self.full {
            render::set_hl(&grids.style, 0, w, &mut current);
            w.extend_from_slice(b"\x1b[2J");
        }
        if tabline_changed && let Some(row) = opts.tabline {
            let cut = &row[..row.len().min(self.cols as usize)];
            render::draw_row(cut, &grids.style, 0, self.cols, w, &mut current);
        }
        {
            let cols = self.cols.min(MAX_COLS as u32);
            let gmap = &grids.grids;
            let style = &mut grids.style;
            let mut base = [Cell::default(); MAX_COLS];
            let mut line = [Cell::default(); MAX_COLS];
            for r in dirty.iter() {
                let r = r as u32;
                base[..cols as usize].fill(Cell::default());
                for s in &wins {
                    if r < s.row || r >= s.row + s.rows || s.col >= cols {
                        continue;
                    }
                    let g = &gmap[&s.grid];
                    let Some(gr) = shown_row(g, s, r - s.row) else {
                        continue;
                    };
                    let src = g.row(gr);
                    let n = (s.cols.min(cols - s.col) as usize).min(src.len());
                    let to = &mut base[s.col as usize..s.col as usize + n];
                    to.copy_from_slice(&src[..n]);
                    blank_cut_wide(to, src.get(n));
                }
                for sep in split.separators {
                    if r >= sep.row && r < sep.row + sep.rows && sep.col < cols {
                        base[sep.col as usize] = split.separator;
                    }
                }
                let shown = compose(
                    gmap,
                    style,
                    &base[..cols as usize],
                    &rects,
                    r,
                    cols,
                    &mut line,
                );
                render::draw_row(shown, style, top + r, self.cols, w, &mut current);
            }
        }
        if let Some(mid) = msg_id
            && (self.full || msg_changed)
        {
            let m = &grids.grids[&mid];
            for i in 0..msg_n {
                let src = msg_total - msg_n + i;
                render::draw_row(
                    m.row(src),
                    &grids.style,
                    msg_top + i,
                    self.cols,
                    w,
                    &mut current,
                );
            }
        }
        render::end(&grids.style, cur, w);
        for s in &wins {
            let g = grids.grids.get_mut(&s.grid).unwrap();
            g.dirty.unset_all();
            g.scrolls_len = 0;
        }

        self.full = false;
        self.drawn_grid = SPLIT_GRID;
        self.drawn_size = [0, 0];
        self.drawn_msg = msg_n;
        self.drawn_tabline = top;
        self.drawn_tabline_col = opts.tabline_col;
        self.drawn_cursor = cur;
        self.drawn_cursor_mode = grids.style.cursor_mode();
        self.drawn_floats[0..rects.len()].copy_from_slice(&rects);
        self.drawn_floats_len = rects.len() as u8;
        self.drawn_split = wins;
        self.drawn_separators = split.separators.to_vec();
        true
    }

    /// Where the cursor shows among a split's windows, floats and messages.
    #[allow(clippy::too_many_arguments)]
    fn split_cursor(
        &self,
        grids: &Grids,
        wins: &[Shown],
        rects: &[Rect],
        top: u32,
        area_rows: u32,
        msg_total: u32,
        msg_n: u32,
    ) -> Option<render::Cursor> {
        let at = |row: i64, col: i64| {
            (row >= 0 && row < area_rows as i64 && col >= 0 && col < self.cols as i64).then(|| {
                render::Cursor {
                    row: top + row as u32,
                    col: col as u32,
                }
            })
        };
        for r in rects {
            if grids.cursor_grid == r.grid {
                let f = grids.get(r.grid).unwrap();
                return at(
                    r.row as i64 + f.cursor_row as i64,
                    r.col as i64 + f.cursor_col as i64,
                );
            }
        }
        for s in wins {
            if grids.cursor_grid != s.grid {
                continue;
            }
            let g = grids.get(s.grid).unwrap();
            // on a row the window shows, inside its rect
            let shown = (0..s.rows).find(|&i| shown_row(g, s, i) == Some(g.cursor_row))?;
            if g.cursor_col >= s.cols {
                return None;
            }
            return at(i64::from(s.row + shown), i64::from(s.col + g.cursor_col));
        }
        let m = grids.get(grids.msg_grid?)?;
        if Some(grids.cursor_grid) != grids.msg_grid || msg_n == 0 {
            return None;
        }
        let first = msg_total - msg_n;
        if m.cursor_row < first || m.cursor_row >= msg_total || m.cursor_col >= self.cols {
            return None;
        }
        Some(render::Cursor {
            row: self.rows - msg_n + (m.cursor_row - first),
            col: m.cursor_col,
        })
    }

    /// The cursor is where, and looks as, it was drawn. A pane without one
    /// draws no shape.
    fn cursor_unchanged(&self, cur: Option<render::Cursor>, style: &Style) -> bool {
        if cur != self.drawn_cursor {
            return false;
        }
        cur.is_none() || style.cursor_mode() == self.drawn_cursor_mode
    }

    /// Screen floats moved from grid 1 into the pane at `origin`, and kept
    /// inside it where they fit. Floats that go together move by the same
    /// amount, so they stay lined up: a float and those hanging from it, such
    /// as a border or a scrollbar, and floats that touch, such as a command
    /// line and the completion menu under it. Too big to fit, their top left
    /// shows, where the text starts. Message floats keep to the bottom, as the
    /// messages do.
    fn fit_screen(&self, rects: &mut [Rect], origin: [i32; 2], screen_rows: u32) {
        assert!(rects.len() <= MAX_FLOATS);
        for r in rects.iter_mut() {
            if r.on_screen {
                if r.on_msg {
                    let up = screen_rows as i32 - self.rows as i32;
                    r.row = fit_into(r.row - up, r.height, self.rows);
                    r.col = fit_into(r.col, r.width, self.cols);
                    continue;
                }
                r.row -= origin[0];
                r.col -= origin[1];
            }
        }
        // the floats that go together, each named by one of them
        let n = rects.len();
        let mut set = [0usize; MAX_FLOATS];
        for (i, s) in set.iter_mut().enumerate().take(n) {
            *s = i;
        }
        let mut joined = true;
        while joined {
            joined = false;
            for i in 0..n {
                for j in i + 1..n {
                    let (a, b) = (rects[i], rects[j]);
                    if set[i] == set[j] || !movable(a) || !movable(b) || !together(a, b) {
                        continue;
                    }
                    let from = set[j];
                    let to = set[i];
                    for x in set[0..n].iter_mut() {
                        if *x == from {
                            *x = to;
                        }
                    }
                    joined = true;
                }
            }
        }
        for i in 0..n {
            let r = rects[i];
            if !movable(r) || set[i] != i {
                continue;
            }
            // the box around this set
            let mut top: i64 = r.row as i64;
            let mut left: i64 = r.col as i64;
            let mut bottom: i64 = r.row as i64 + r.height as i64;
            let mut right: i64 = r.col as i64 + r.width as i64;
            for (k, m) in rects.iter().enumerate() {
                if set[k] == i {
                    top = top.min(m.row as i64);
                    left = left.min(m.col as i64);
                    bottom = bottom.max(m.row as i64 + m.height as i64);
                    right = right.max(m.col as i64 + m.width as i64);
                }
            }
            let down = fit_start(top, bottom - top, self.rows) - top;
            let across = fit_start(left, right - left, self.cols) - left;
            for (k, m) in rects.iter_mut().enumerate() {
                if set[k] == i {
                    m.row = (m.row as i64 + down) as i32;
                    m.col = (m.col as i64 + across) as i32;
                }
            }
        }
    }

    /// The floats drawn last frame, lowest first.
    pub(crate) fn drawn_floats(&self) -> &[Rect] {
        &self.drawn_floats[0..self.drawn_floats_len as usize]
    }

    /// Draws just floats `placed`, moved up and left by `origin`, over a blank
    /// terminal: a group shown in a Rex floating pane. True if it wrote anything.
    pub(crate) fn frame_floats(
        &mut self,
        grids: &mut Grids,
        placed: &[Placed],
        origin: [i32; 2],
        w: &mut Vec<u8>,
    ) -> bool {
        grids.style.clear_blends();
        let mut rects = float_rects(grids, placed);
        for r in rects.iter_mut() {
            r.row -= origin[0];
            r.col -= origin[1];
        }
        if !same_rects(&rects, self.drawn_floats()) {
            self.full = true;
        }

        let mut dirty = DirtyRows::default();
        dirty.resize(MAX_ROWS, false);
        let rows = self.rows.min(MAX_ROWS as u32);
        for r in &rects {
            let f = grids.get(r.grid).unwrap();
            let (top, bot) = r.rows(rows);
            if f.scrolls_len > 0 {
                for i in top..bot {
                    dirty.set(i as usize);
                }
                continue;
            }
            for fr in f.dirty.iter() {
                let tr = r.row as i64 + fr as i64;
                if tr >= 0 && tr < rows as i64 {
                    dirty.set(tr as usize);
                }
            }
        }
        let cur = float_cursor(grids, &rects, rows, self.cols);
        if !self.full && dirty.count() == 0 && self.cursor_unchanged(cur, &grids.style) {
            return false;
        }

        render::begin(w);
        let mut current: Option<u32> = None;
        if self.full {
            render::set_hl(&grids.style, 0, w, &mut current);
            w.extend_from_slice(b"\x1b[2J");
            dirty.unset_all();
            for r in &rects {
                let (top, bot) = r.rows(rows);
                for i in top..bot {
                    dirty.set(i as usize);
                }
            }
        }
        {
            let gmap = &grids.grids;
            let style = &mut grids.style;
            let mut line = [Cell::default(); MAX_COLS];
            for r in dirty.iter() {
                let shown = compose(
                    gmap,
                    style,
                    &[],
                    &rects,
                    r as u32,
                    self.cols.min(MAX_COLS as u32),
                    &mut line,
                );
                render::draw_row(shown, style, r as u32, self.cols, w, &mut current);
            }
        }
        render::end(&grids.style, cur, w);

        self.full = false;
        self.drawn_grid = 0;
        self.drawn_size = [0, 0];
        self.drawn_cursor = cur;
        self.drawn_cursor_mode = grids.style.cursor_mode();
        self.drawn_floats[0..rects.len()].copy_from_slice(&rects);
        self.drawn_floats_len = rects.len() as u8;
        true
    }

    /// Repeats the grid's scrolls if they stay clear of the messages and
    /// floats; else redraws the rows they moved.
    fn scrolls(&self, g: &mut Grid, top: u32, grid_rows: u32, rects: &[Rect], w: &mut Vec<u8>) {
        let pending: Vec<Scroll> = g.pending_scrolls().to_vec();
        for s in &pending {
            if s.bot > grid_rows || under_float(s, rects, grid_rows) {
                // one can't be repeated, so none are: the terminal must match each in turn
                for p in &pending {
                    for r in p.top..p.bot {
                        g.dirty.set(r as usize);
                    }
                }
                g.scrolls_len = 0;
                return;
            }
        }
        render::replay_scrolls(g, top, w);
    }

    /// Where the cursor shows in this pane, if it does. Grid rows show from
    /// terminal row `top`.
    #[allow(clippy::too_many_arguments)]
    fn cursor_at(
        &self,
        grids: &Grids,
        g: Option<&Grid>,
        id: u32,
        top: u32,
        grid_rows: u32,
        msg: Option<&Grid>,
        msg_total: u32,
        msg_n: u32,
        rects: &[Rect],
    ) -> Option<render::Cursor> {
        for r in rects {
            if grids.cursor_grid == r.grid {
                let f = grids.get(r.grid).unwrap();
                let row = r.row as i64 + f.cursor_row as i64;
                let col = r.col as i64 + f.cursor_col as i64;
                if row < 0 || row >= grid_rows as i64 || col < 0 || col >= self.cols as i64 {
                    return None;
                }
                return Some(render::Cursor {
                    row: (top as i64 + row) as u32,
                    col: col as u32,
                });
            }
        }
        if let Some(gr) = g
            && grids.cursor_grid == id
        {
            if gr.cursor_row < grid_rows && gr.cursor_col < self.cols {
                return Some(render::Cursor {
                    row: top + gr.cursor_row,
                    col: gr.cursor_col,
                });
            }
            return None;
        }
        let m = msg?;
        if Some(grids.cursor_grid) != grids.msg_grid {
            return None;
        }
        let first = msg_total - msg_n;
        if m.cursor_row < first || m.cursor_row >= msg_total || m.cursor_col >= self.cols {
            return None;
        }
        Some(render::Cursor {
            row: self.rows - msg_n + (m.cursor_row - first),
            col: m.cursor_col,
        })
    }
}

/// The row of grid `g` that row `i` of its rect `s` shows: its first
/// rows, and its last (the statusline) in the rect's last row when the grid
/// is taller. None past the grid's end.
fn shown_row(g: &Grid, s: &Shown, i: u32) -> Option<u32> {
    if g.height > s.rows && i + 1 == s.rows {
        return Some(g.height - 1);
    }
    (i < g.height).then_some(i)
}

fn screen_rows(grids: &Grids) -> u32 {
    grids.get(1).map_or(0, |g| g.height)
}

/// A screen float `fit_screen` may move with others.
fn movable(r: Rect) -> bool {
    r.on_screen && !r.on_msg
}

/// Hang from the same editor float, or overlap or touch.
fn together(a: Rect, b: Rect) -> bool {
    if a.top != 0 && a.top == b.top {
        return true;
    }
    a.row as i64 <= b.row as i64 + b.height as i64
        && b.row as i64 <= a.row as i64 + a.height as i64
        && a.col as i64 <= b.col as i64 + b.width as i64
        && b.col as i64 <= a.col as i64 + a.width as i64
}

/// `pos` moved back inside `0..limit` if `len` fits there, else to 0.
fn fit_start(pos: i64, len: i64, limit: u32) -> i64 {
    if len > limit as i64 {
        return 0;
    }
    pos.clamp(0, limit as i64 - len)
}

/// `pos` moved back inside `0..limit` if `len` fits there.
fn fit_into(pos: i32, len: u32, limit: u32) -> i32 {
    if len > limit {
        return pos.min(0).max(limit as i32 - len as i32);
    }
    pos.clamp(0, (limit - len) as i32)
}

/// The cursor, if it's on one of the floats and inside the terminal.
fn float_cursor(grids: &Grids, rects: &[Rect], rows: u32, cols: u32) -> Option<render::Cursor> {
    for r in rects {
        if grids.cursor_grid == r.grid {
            let f = grids.get(r.grid).unwrap();
            let row = r.row as i64 + f.cursor_row as i64;
            let col = r.col as i64 + f.cursor_col as i64;
            if row < 0 || row >= rows as i64 || col < 0 || col >= cols as i64 {
                return None;
            }
            return Some(render::Cursor {
                row: row as u32,
                col: col as u32,
            });
        }
    }
    None
}

fn same_rects(a: &[Rect], b: &[Rect]) -> bool {
    a == b
}

fn under_float(s: &Scroll, rects: &[Rect], grid_rows: u32) -> bool {
    for r in rects {
        let (top, bot) = r.rows(grid_rows);
        if top < s.bot && s.top < bot {
            return true;
        }
    }
    false
}

/// The floats' rects, lowest first, leaving out empty ones.
fn float_rects(grids: &Grids, placed: &[Placed]) -> Vec<Rect> {
    let mut out = Vec::new();
    for p in placed {
        let Some(f) = grids.get(p.grid) else { continue };
        if f.width == 0 || f.height == 0 {
            continue;
        }
        out.push(Rect {
            grid: p.grid,
            row: p.row,
            col: p.col,
            width: f.width,
            height: f.height,
            on_screen: p.on_screen,
            on_msg: p.on_msg,
            top: p.top,
        });
    }
    out
}

/// Base row `row` at terminal row `r` with the floats over it, in `line`.
/// Just `row` if no float covers it. (Takes the grid map and the style apart,
/// not `Grids`: `row` borrows from the map while blending changes the style.)
fn compose<'a>(
    grids: &IndexMap<u32, Grid>,
    style: &mut Style,
    row: &'a [Cell],
    rects: &[Rect],
    r: u32,
    cols: u32,
    line: &'a mut [Cell; MAX_COLS],
) -> &'a [Cell] {
    let mut width: usize = row.len();
    let mut covered = false;
    for f in rects {
        if (r as i64) < f.row as i64 || r as i64 >= f.row as i64 + f.height as i64 {
            continue;
        }
        covered = true;
        width = width.max((f.col as i64 + f.width as i64).clamp(0, cols as i64) as usize);
    }
    if !covered {
        return row;
    }
    width = width.min(cols as usize);
    let base = &row[0..row.len().min(width)];
    line[0..base.len()].copy_from_slice(base);
    line[base.len()..width].fill(Cell::default());
    blank_cut_wide(&mut line[0..base.len()], row.get(base.len()));
    for f in rects {
        if (r as i64) < f.row as i64 || r as i64 >= f.row as i64 + f.height as i64 {
            continue;
        }
        let src = grids[&f.grid].row((r as i64 - f.row as i64) as u32);
        let left = f.col.max(0) as usize;
        let skip = (-f.col).max(0) as usize;
        if left >= width || skip >= src.len() {
            continue;
        }
        let n = (src.len() - skip).min(width - left);
        // a wide char cut by the float's edges leaves a blank
        if left > 0 && line[left].len == 0 {
            line[left - 1] = Cell {
                hl: line[left - 1].hl,
                ..Cell::default()
            };
        }
        if left + n < width && line[left + n].len == 0 {
            line[left + n] = Cell {
                hl: line[left + n].hl,
                ..Cell::default()
            };
        }
        for (k, &cell) in src[skip..skip + n].iter().enumerate() {
            let i = left + k;
            line[i] = blend_cell(style, cell, &line[0..width], i);
        }
        blank_cut_wide(&mut line[left..left + n], src.get(skip + n));
        if line[left].len == 0 {
            line[left] = Cell {
                hl: line[left].hl,
                ..Cell::default()
            };
        }
    }
    &line[0..width]
}

/// Float cell `cell` over `line[i]`. A blended blank lets the text under
/// it show, unless that's half a wide char.
fn blend_cell(style: &mut Style, cell: Cell, line: &[Cell], i: usize) -> Cell {
    if style.attr(cell.hl).blend == 0 {
        return cell;
    }
    let under = line[i];
    let wide = under.len == 0 || (i + 1 < line.len() && line[i + 1].len == 0);
    if cell.len == 1 && cell.text[0] == b' ' && !wide {
        let mut c = under;
        c.hl = style.blend_over(cell.hl, under.hl, true);
        return c;
    }
    let mut c = cell;
    c.hl = style.blend_over(cell.hl, under.hl, false);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains(hay: &[u8], needle: &str) -> bool {
        find(hay, needle.as_bytes()).is_some()
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    fn add_grid(gs: &mut Grids, id: u32, w: u32, h: u32) {
        gs.grids.insert(id, Grid::default());
        gs.grids.get_mut(&id).unwrap().resize(w, h);
    }

    #[test]
    fn screen_floats_move_into_the_pane_and_stay_inside_it() {
        let view = PaneView::init(20, 10);
        let mut rects = [
            // over the pane at 0, 30 of a wider screen
            Rect {
                grid: 3,
                row: 2,
                col: 35,
                width: 4,
                height: 2,
                on_screen: true,
                ..Rect::default()
            },
            // past its right edge: moved back in
            Rect {
                grid: 4,
                row: 0,
                col: 70,
                width: 8,
                height: 1,
                on_screen: true,
                ..Rect::default()
            },
            // over a window: left alone
            Rect {
                grid: 5,
                row: 1,
                col: 1,
                width: 2,
                height: 1,
                ..Rect::default()
            },
            // on the messages: kept to the bottom
            Rect {
                grid: 6,
                row: 25,
                col: 2,
                width: 5,
                height: 3,
                on_screen: true,
                on_msg: true,
                ..Rect::default()
            },
        ];
        view.fit_screen(&mut rects, [0, 30], 30);
        assert_eq!(2, rects[0].row);
        assert_eq!(5, rects[0].col);
        assert_eq!(12, rects[1].col);
        assert_eq!(1, rects[2].col);
        assert_eq!(5, rects[3].row);
        assert_eq!(2, rects[3].col);
    }

    #[test]
    fn a_command_line_and_its_menu_move_into_a_narrow_pane_together() {
        // a pane 40 wide at column 40 of an 80 wide screen
        let view = PaneView::init(40, 20);
        let mut rects = [
            // noice's box, wider than the pane, and its text inside the border
            Rect {
                grid: 3,
                row: 2,
                col: 8,
                width: 64,
                height: 3,
                on_screen: true,
                top: 3,
                ..Rect::default()
            },
            Rect {
                grid: 4,
                row: 3,
                col: 10,
                width: 60,
                height: 1,
                on_screen: true,
                top: 3,
                ..Rect::default()
            },
            // blink's menu over the box's bottom border, and its scrollbar
            Rect {
                grid: 5,
                row: 4,
                col: 8,
                width: 39,
                height: 10,
                on_screen: true,
                top: 5,
                ..Rect::default()
            },
            Rect {
                grid: 6,
                row: 4,
                col: 47,
                width: 1,
                height: 10,
                on_screen: true,
                top: 5,
                ..Rect::default()
            },
            // a notification far off: on its own
            Rect {
                grid: 7,
                row: 18,
                col: 75,
                width: 4,
                height: 1,
                on_screen: true,
                top: 7,
                ..Rect::default()
            },
        ];
        view.fit_screen(&mut rects, [0, 40], 24);
        // too wide: the left edge shows, everything moved alike
        assert_eq!(0, rects[0].col);
        assert_eq!(2, rects[1].col);
        assert_eq!(0, rects[2].col);
        assert_eq!(39, rects[3].col);
        for (r, row) in rects[0..4].iter().zip([2, 3, 4, 4]) {
            assert_eq!(row, r.row);
        }
        // kept inside on its own: 75 - 40, then left to fit
        assert_eq!(35, rects[4].col);
        assert_eq!(18, rects[4].row);
    }

    #[test]
    fn a_scrollbar_stays_on_its_menu_when_the_menu_is_moved_in() {
        let view = PaneView::init(30, 10);
        let mut rects = [
            // past the right edge
            Rect {
                grid: 3,
                row: 1,
                col: 25,
                width: 10,
                height: 4,
                on_screen: true,
                top: 3,
                ..Rect::default()
            },
            Rect {
                grid: 4,
                row: 1,
                col: 35,
                width: 1,
                height: 4,
                on_screen: true,
                top: 3,
                ..Rect::default()
            },
        ];
        view.fit_screen(&mut rects, [0, 0], 10);
        assert_eq!(19, rects[0].col);
        assert_eq!(29, rects[1].col);
    }

    #[test]
    fn frame_floats_draws_a_group_on_its_own_and_the_cursor_in_it() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 3, 6, 2);
        add_grid(&mut gs, 4, 3, 1);
        gs.grids.get_mut(&3).unwrap().cells[0] = Cell::init("a", 0);
        gs.grids.get_mut(&4).unwrap().cells[0] = Cell::init("b", 0);
        gs.cursor_grid = 4;
        gs.grids.get_mut(&4).unwrap().cursor_col = 1;
        let placed = [
            Placed {
                grid: 3,
                row: 10,
                col: 20,
                zindex: 50,
                compindex: 0,
                ..Placed::default()
            },
            Placed {
                grid: 4,
                row: 11,
                col: 21,
                zindex: 51,
                compindex: 1,
                ..Placed::default()
            },
        ];
        let mut view = PaneView::init(8, 4);
        let mut out = Vec::new();
        assert!(view.frame_floats(&mut gs, &placed, [10, 20], &mut out));
        assert!(contains(&out, "a"));
        assert_eq!(
            render::Cursor { row: 1, col: 2 },
            view.drawn_cursor.unwrap()
        );
        assert_eq!(2, view.drawn_floats().len());
        // nothing changed: nothing drawn
        gs.grids.get_mut(&3).unwrap().dirty.unset_all();
        gs.grids.get_mut(&4).unwrap().dirty.unset_all();
        assert!(!view.frame_floats(&mut gs, &placed, [10, 20], &mut out));
    }

    #[test]
    fn a_wide_char_starting_in_the_last_column_is_a_blank_there_and_does_not_wrap() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 3, 4, 1);
        {
            let g = gs.grids.get_mut(&3).unwrap();
            g.cells[0] = Cell::init("a", 0);
            g.cells[1] = Cell::init("b", 0);
            g.cells[2] = Cell::init("\u{3042}", 0);
            g.cells[3] = Cell {
                len: 0,
                ..Cell::default()
            };
        }
        // the float covers columns 7 to 10 of a 10-column pane: the wide
        // char starts at column 9 and its right half would be column 10
        let rects = [Rect {
            grid: 3,
            row: 0,
            col: 7,
            width: 4,
            height: 1,
            ..Rect::default()
        }];
        let base = [Cell::init("w", 0); 10];
        let mut line = [Cell::default(); MAX_COLS];
        let mut style = Style::default();
        let out = compose(&gs.grids, &mut style, &base, &rects, 0, 10, &mut line);
        assert_eq!(10, out.len());
        assert_eq!("a".as_bytes(), out[7].str());
        assert_eq!("b".as_bytes(), out[8].str());
        assert_eq!(" ".as_bytes(), out[9].str());
    }

    #[test]
    fn a_pane_under_the_tabline_draws_it_on_top_and_its_window_below() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 2, 6, 3);
        gs.grids.get_mut(&2).unwrap().cells[0] = Cell::init("w", 0);
        gs.grids.get_mut(&2).unwrap().cursor_row = 1;
        gs.grids.get_mut(&2).unwrap().cursor_col = 2;
        gs.cursor_grid = 2;
        let tabline = [Cell::init("T", 0), Cell::init("1", 0)];
        let mut view = PaneView::init(6, 4);
        let mut out = Vec::new();
        assert!(view.frame(
            &mut gs,
            Some(2),
            Opts {
                tabline: Some(&tabline),
                ..Opts::default()
            },
            &mut out
        ));
        // the tabline on row 1, the window's first row on row 2
        assert!(contains(&out, "\x1b[1;1HT1"));
        assert!(contains(&out, "\x1b[2;1Hw"));
        assert_eq!(
            render::Cursor { row: 2, col: 2 },
            view.drawn_cursor.unwrap()
        );

        // only the tabline changed: only it is drawn
        out.clear();
        assert!(view.frame(
            &mut gs,
            Some(2),
            Opts {
                tabline: Some(&tabline),
                tabline_dirty: true,
                ..Opts::default()
            },
            &mut out
        ));
        assert!(contains(&out, "T1"));
        assert!(!contains(&out, "w"));

        // the tabline went: everything is drawn again, the window from the top
        out.clear();
        assert!(view.frame(&mut gs, Some(2), Opts::default(), &mut out));
        assert!(!contains(&out, "T1"));
        assert!(contains(&out, "\x1b[1;1Hw"));
    }

    #[test]
    fn the_tabline_is_drawn_again_when_the_column_it_starts_at_moves() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 2, 6, 3);
        gs.cursor_grid = 2;
        let first = [Cell::init("A", 0), Cell::init("B", 0)];
        let second = [Cell::init("C", 0), Cell::init("D", 0)];
        let mut view = PaneView::init(6, 4);
        let mut out = Vec::new();
        assert!(view.frame(
            &mut gs,
            Some(2),
            Opts {
                tabline: Some(&first),
                ..Opts::default()
            },
            &mut out
        ));
        // the same column, nothing dirty: nothing to draw
        gs.grids.get_mut(&2).unwrap().dirty.unset_all();
        out.clear();
        assert!(!view.frame(
            &mut gs,
            Some(2),
            Opts {
                tabline: Some(&first),
                ..Opts::default()
            },
            &mut out
        ));
        // panes to its left changed: it shows another part, with nothing else dirty
        assert!(view.frame(
            &mut gs,
            Some(2),
            Opts {
                tabline: Some(&second),
                tabline_col: 2,
                ..Opts::default()
            },
            &mut out
        ));
        assert!(contains(&out, "CD"));
    }

    /// Terminal row `row` (1 based) was drawn from its first column, starting
    /// with `text`, after any colours.
    fn drew_row(written: &[u8], row: u8, text: &str) -> bool {
        let at = format!("\x1b[{row};1H");
        let mut rest = written;
        while let Some(i) = find(rest, at.as_bytes()) {
            rest = &rest[i + at.len()..];
            let mut s = rest;
            while s.starts_with(b"\x1b[") {
                let Some(end) = s[2..].iter().position(|b| !b"0123456789;".contains(b)) else {
                    break;
                };
                if s[2 + end] != b'm' {
                    break;
                }
                s = &s[end + 3..];
            }
            if s.starts_with(text.as_bytes()) {
                return true;
            }
        }
        false
    }

    fn focused() -> Opts<'static> {
        Opts {
            focused: true,
            ..Opts::default()
        }
    }

    #[test]
    fn with_no_row_reserved_for_messages_they_draw_over_the_windows_bottom_rows_and_leave_them_again()
     {
        let mut gs = Grids::default();
        // a screen of 4 rows, 'cmdheight' 0: the window fills the pane
        add_grid(&mut gs, 1, 6, 4);
        add_grid(&mut gs, 2, 6, 4);
        {
            let win = gs.grids.get_mut(&2).unwrap();
            for r in 0..4 {
                win.cells[r * 6] = Cell::init([b'a' + r as u8], 0);
            }
            win.cursor_row = 3;
            win.cursor_col = 1;
        }
        gs.cursor_grid = 2;
        let mut view = PaneView::init(6, 4);
        let mut out = Vec::new();

        // no messages: every row of the window shows, the last too
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 4, "d"));
        assert_eq!(
            render::Cursor { row: 3, col: 1 },
            view.drawn_cursor.unwrap()
        );

        // one message row, typing on it: it covers the window's last row, and the cursor is on it
        add_grid(&mut gs, 5, 6, 1);
        gs.grids.get_mut(&5).unwrap().cells[0] = Cell::init(":", 0);
        gs.grids.get_mut(&5).unwrap().cursor_col = 1;
        gs.msg_grid = Some(5);
        gs.msg_row = 3;
        gs.cursor_grid = 5;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 4, ":"));
        assert!(!drew_row(&out, 4, "d"));
        assert_eq!(
            render::Cursor { row: 3, col: 1 },
            view.drawn_cursor.unwrap()
        );

        // a longer message scrolled up: two rows of the window are covered
        {
            let msg = gs.grids.get_mut(&5).unwrap();
            msg.resize(6, 2);
            msg.cells[0] = Cell::init("x", 0);
            msg.cells[6] = Cell::init("y", 0);
            msg.dirty.set_all();
        }
        gs.msg_row = 2;
        gs.cursor_grid = 2;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 3, "x"));
        assert!(drew_row(&out, 4, "y"));
        // the cursor is on the window, under the messages: hidden
        assert_eq!(None, view.drawn_cursor);

        // a message row shorter: the row it left shows the window again
        {
            let msg = gs.grids.get_mut(&5).unwrap();
            msg.resize(6, 1);
            msg.cells[0] = Cell::init("y", 0);
            msg.dirty.set_all();
        }
        gs.msg_row = 3;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 3, "c"));
        assert!(drew_row(&out, 4, "y"));

        // the messages are gone: the last row is the window's again, and the cursor is back
        gs.msg_grid = None;
        gs.cursor_grid = 2;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 4, "d"));
        assert_eq!(
            render::Cursor { row: 3, col: 1 },
            view.drawn_cursor.unwrap()
        );
    }

    #[test]
    fn a_cursor_that_only_changes_shape_is_drawn_again() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 2, 6, 3);
        gs.cursor_grid = 2;
        gs.style.cursor_style_enabled = true;
        gs.style.cursor_modes.push(CursorMode::default());
        gs.style.cursor_modes.push(CursorMode {
            shape: crate::screen::style::Shape::Vertical,
            ..CursorMode::default()
        });
        let mut view = PaneView::init(6, 3);
        let mut out = Vec::new();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(contains(&out, "\x1b[2 q"));

        // insert mode on an empty line: no cell, and not the cursor's place, changes
        assert!(!view.frame(&mut gs, Some(2), focused(), &mut out));
        gs.style.mode_idx = 1;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(contains(&out, "\x1b[6 q"));

        // a pane with no cursor has no shape to draw
        gs.cursor_grid = 1;
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        gs.style.mode_idx = 0;
        assert!(!view.frame(&mut gs, Some(2), focused(), &mut out));
    }

    #[test]
    fn a_scroll_in_the_frame_the_messages_leave_is_drawn_again_not_replayed_over_the_message() {
        let mut gs = Grids::default();
        add_grid(&mut gs, 1, 6, 4);
        add_grid(&mut gs, 2, 6, 4);
        for r in 0..4 {
            gs.grids.get_mut(&2).unwrap().cells[r * 6] = Cell::init([b'a' + r as u8], 0);
        }
        gs.cursor_grid = 2;
        add_grid(&mut gs, 5, 6, 1);
        gs.grids.get_mut(&5).unwrap().cells[0] = Cell::init("/", 0);
        gs.msg_grid = Some(5);
        gs.msg_row = 3;
        let mut view = PaneView::init(6, 4);
        let mut out = Vec::new();
        // `/` covers the window's last row
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        assert!(drew_row(&out, 4, "/"));

        // `/pattern<CR>` scrolls the window, and the message is gone, in one flush
        let win = gs.grids.get_mut(&2).unwrap();
        win.scroll(0, 4, 0, 6, 1).unwrap();
        win.cells[3 * 6] = Cell::init("e", 0);
        gs.msg_grid = None;
        out.clear();
        assert!(view.frame(&mut gs, Some(2), focused(), &mut out));
        // the terminal's message row would scroll up with the rows: every row is drawn instead
        assert!(!contains(&out, "S"));
        for (row, text) in ["b", "c", "d", "e"].into_iter().enumerate() {
            assert!(drew_row(&out, row as u8 + 1, text));
        }
    }
}
