//! Every grid Neovim draws, built from its redraw events. With ext_multigrid
//! each window has its own grid; grid 1 holds what's left (statuslines of
//! split windows, the tabline). Also tracks where windows and the message
//! grid are.

use indexmap::IndexMap;

use super::grid::Grid;
use super::style::{self, Style};
use crate::nvim::msgpack::{Decoder, Token};
use crate::nvim::rpc;

#[derive(Debug, Clone)]
#[doc(hidden)]
pub struct Grids {
    pub grids: IndexMap<u32, Grid>,
    /// Windows by grid id.
    pub wins: IndexMap<u32, Win>,
    pub style: Style,
    /// The grid the cursor is on.
    pub cursor_grid: u32,
    /// The message grid, drawn over grid 1 from `msg_row` down.
    pub msg_grid: Option<u32>,
    pub(crate) msg_row: u32,
    /// The highlight Neovim draws WinSeparator in, as `hl_group_set` says.
    pub separator_hl: u32,
}

impl Default for Grids {
    fn default() -> Self {
        Grids {
            grids: IndexMap::new(),
            wins: IndexMap::new(),
            style: Style::default(),
            cursor_grid: 1,
            msg_grid: None,
            msg_row: 0,
            separator_hl: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[doc(hidden)]
pub struct Win {
    /// Neovim's window handle; -1 for the popup menu.
    pub win: i64,
    pub kind: WinKind,
    /// Where it floats, for `WinKind::Float`.
    pub(crate) float: Float,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum WinKind {
    /// In the tab's split layout, positioned on grid 1.
    Ordinary,
    /// Outside the layout, for the UI to place.
    External,
    /// Floating over another grid, such as the popup menu.
    Float,
    /// An ordinary window on another tab page.
    Hidden,
}

/// A float's position, as `win_float_pos` gives it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Float {
    /// The grid it floats over, and the corner of the float at `row`, `col`.
    pub(crate) anchor_grid: u32,
    pub(crate) anchor: Anchor,
    pub(crate) row: f64,
    pub(crate) col: f64,
    pub(crate) zindex: u32,
    /// Takes the mouse; Neovim sets this for focusable floats.
    pub(crate) focusable: bool,
    /// Neovim's drawing order among equal zindexes.
    pub(crate) compindex: u32,
    /// Where Neovim put it on grid 1, kept inside the screen.
    pub(crate) screen: Option<[i32; 2]>,
}

impl Default for Float {
    fn default() -> Self {
        Float {
            anchor_grid: 1,
            anchor: Anchor::Nw,
            row: 0.0,
            col: 0.0,
            zindex: 50,
            focusable: true,
            compindex: 0,
            screen: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    Nw,
    Ne,
    Sw,
    Se,
}

/// A float placed over a base grid, in that grid's cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct Placed {
    pub grid: u32,
    pub row: i32,
    pub col: i32,
    pub(crate) zindex: u32,
    pub(crate) compindex: u32,
    /// On the screen (grid 1 or the message grid), not over a window.
    pub(crate) on_screen: bool,
    /// On the message grid, which shows at the bottom.
    pub(crate) on_msg: bool,
    /// The editor-relative float it hangs from, itself included; 0 if none.
    pub(crate) top: u32,
}

/// Where a float sits on the grid under it all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) root: u32,
    pub(crate) row: i32,
    pub(crate) col: i32,
    pub(crate) top: u32,
}

/// Most floats drawn over one pane.
pub(crate) const MAX_FLOATS: usize = 16;

impl Grids {
    /// Grid `id`, if it exists. (`get_mut` is the mutable twin.)
    pub fn get(&self, id: u32) -> Option<&Grid> {
        self.grids.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Grid> {
        self.grids.get_mut(&id)
    }

    /// The grid showing window `win`.
    pub fn grid_of(&self, win: i64) -> Option<u32> {
        for (&grid, w) in &self.wins {
            if w.win == win {
                return Some(grid);
            }
        }
        None
    }

    /// The window on `grid`.
    pub(crate) fn win_on(&self, grid: u32) -> Option<Win> {
        self.wins.get(&grid).copied()
    }

    /// The floats over grid `base`, and over the screen too if `screen`
    /// (floats on grid 1 and the message grid), lowest first. Floats hanging
    /// from an editor float in `skip` are left out. At most `MAX_FLOATS`: the
    /// top-most ones.
    pub fn floats_over(&self, base: u32, screen: bool, skip: &[u32]) -> Vec<Placed> {
        let mut out: Vec<Placed> = Vec::new();
        for (&id, w) in &self.wins {
            if w.kind != WinKind::Float {
                continue;
            }
            let Some(p) = self.place(id) else { continue };
            if p.top != 0 && skip.contains(&p.top) {
                continue;
            }
            let on_screen = p.root == 1 || Some(p.root) == self.msg_grid;
            if p.root != base && !(screen && on_screen) {
                continue;
            }
            out.push(Placed {
                grid: id,
                row: p.row,
                col: p.col,
                zindex: w.float.zindex,
                compindex: w.float.compindex,
                on_screen,
                on_msg: Some(p.root) == self.msg_grid,
                top: p.top,
            });
        }
        keep_top_floats(&mut out);
        out
    }

    /// Where float grid `id` sits on the grid under it all: floats can float over floats.
    pub(crate) fn place(&self, id: u32) -> Option<Place> {
        let w = self.wins.get(&id)?;
        if w.kind != WinKind::Float {
            return None;
        }
        let mut row: i32 = 0;
        let mut col: i32 = 0;
        let mut grid = id;
        let mut float = w.float;
        let mut top: u32 = 0;
        for _ in 0..8 {
            let g = self.get(grid)?;
            if float.anchor_grid == 1 {
                top = grid;
                if let Some(pos) = float.screen {
                    return Some(Place {
                        root: 1,
                        row: row + pos[0],
                        col: col + pos[1],
                        top,
                    });
                }
            }
            row += float.row.floor() as i32
                - if matches!(float.anchor, Anchor::Sw | Anchor::Se) {
                    g.height as i32
                } else {
                    0
                };
            col += float.col.floor() as i32
                - if matches!(float.anchor, Anchor::Ne | Anchor::Se) {
                    g.width as i32
                } else {
                    0
                };
            grid = float.anchor_grid;
            let Some(under) = self.wins.get(&grid) else {
                // the message grid sits on grid 1 at msg_row
                if Some(grid) == self.msg_grid {
                    return Some(Place {
                        root: grid,
                        row: row + self.msg_row as i32,
                        col,
                        top,
                    });
                }
                return Some(Place {
                    root: grid,
                    row,
                    col,
                    top,
                });
            };
            if under.kind != WinKind::Float {
                return Some(Place {
                    root: grid,
                    row,
                    col,
                    top,
                });
            }
            float = under.float;
        }
        None
    }

    /// Rows the message grid covers at the bottom of grid 1: the command
    /// line, or more while a long message scrolls up.
    pub fn msg_rows(&self) -> u32 {
        let Some(id) = self.msg_grid else { return 0 };
        let Some(screen) = self.get(1) else { return 0 };
        let Some(msg) = self.get(id) else { return 0 };
        screen.height.saturating_sub(self.msg_row).min(msg.height)
    }

    /// Applies a `redraw` notification's params. True if it ended a frame
    /// with `flush`, so it's time to draw.
    pub fn apply_redraw(&mut self, params: &mut Decoder<'_>) -> rpc::Result<bool> {
        let mut flushed = false;
        // [[name, args, args, ...], ...]
        let events = rpc::expect_array(params)?;
        for _ in 0..events {
            let len = rpc::expect_array(params)?;
            if len == 0 {
                return Err(rpc::Error::BadMessage);
            }
            let name = rpc::expect_str(params)?;
            let event = Event::from_name(name);
            for _ in 1..len {
                let Some(e) = event else {
                    rpc::skip(params)?;
                    continue;
                };
                let n = rpc::expect_array(params)?;
                let used = self.apply(e, params, n)?;
                for _ in used..n {
                    rpc::skip(params)?;
                }
                if e == Event::Flush {
                    flushed = true;
                }
            }
        }
        Ok(flushed)
    }

    /// One event's args, after their array header of `len`. Returns how many
    /// args it read.
    fn apply(&mut self, event: Event, d: &mut Decoder<'_>, len: u32) -> rpc::Result<u32> {
        match event {
            Event::GridResize => {
                if len < 3 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                let w = expect_int::<u32>(d)?;
                let h = expect_int::<u32>(d)?;
                self.grids.entry(id).or_default().resize(w, h);
                Ok(3)
            }
            Event::GridClear => {
                if len < 1 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                if let Some(g) = self.get_mut(id) {
                    g.clear();
                }
                Ok(1)
            }
            Event::GridLine => {
                if len < 4 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                let r = expect_int::<u32>(d)?;
                let col = expect_int::<u32>(d)?;
                if let Some(grid) = self.get_mut(id) {
                    grid.line(d, r, col)?;
                } else {
                    rpc::skip(d)?;
                }
                Ok(4)
            }
            Event::GridScroll => {
                if len < 7 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                let top = expect_int::<u32>(d)?;
                let bot = expect_int::<u32>(d)?;
                let left = expect_int::<u32>(d)?;
                let right = expect_int::<u32>(d)?;
                let rows = expect_int::<i64>(d)?;
                expect_int::<i64>(d)?; // cols, always 0
                if let Some(grid) = self.get_mut(id) {
                    grid.scroll(top, bot, left, right, rows)?;
                }
                Ok(7)
            }
            Event::GridCursorGoto => {
                if len < 3 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                let r = expect_int::<u32>(d)?;
                let c = expect_int::<u32>(d)?;
                self.cursor_grid = id;
                if let Some(g) = self.get_mut(id) {
                    g.cursor_row = r;
                    g.cursor_col = c;
                }
                Ok(3)
            }
            Event::GridDestroy => {
                if len < 1 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                self.grids.swap_remove(&id);
                self.wins.swap_remove(&id);
                Ok(1)
            }
            Event::WinPos | Event::WinExternalPos | Event::WinFloatPos => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                let win = rpc::expect_handle(d)?;
                let kind = match event {
                    Event::WinPos => WinKind::Ordinary,
                    Event::WinExternalPos => WinKind::External,
                    _ => WinKind::Float,
                };
                if event != Event::WinFloatPos {
                    self.wins.insert(
                        id,
                        Win {
                            win,
                            kind,
                            float: Float::default(),
                        },
                    );
                    return Ok(2);
                }
                if len < 8 {
                    return Err(rpc::Error::BadMessage);
                }
                let anchor = match &lower2(rpc::expect_str(d)?) {
                    b"nw" => Anchor::Nw,
                    b"ne" => Anchor::Ne,
                    b"sw" => Anchor::Sw,
                    b"se" => Anchor::Se,
                    _ => Anchor::Nw,
                };
                let mut f = Float {
                    anchor,
                    ..Float::default()
                };
                f.anchor_grid = expect_int(d)?;
                f.row = expect_number(d)?;
                f.col = expect_number(d)?;
                f.focusable = rpc::expect_bool(d)?;
                f.zindex = expect_int(d)?;
                if len < 11 {
                    self.wins.insert(
                        id,
                        Win {
                            win,
                            kind,
                            float: f,
                        },
                    );
                    return Ok(8);
                }
                f.compindex = expect_int(d)?;
                let sr = expect_int::<i32>(d)?;
                let sc = expect_int::<i32>(d)?;
                f.screen = Some([sr, sc]);
                self.wins.insert(
                    id,
                    Win {
                        win,
                        kind,
                        float: f,
                    },
                );
                Ok(11)
            }
            Event::WinHide => {
                if len < 1 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                if let Some(w) = self.wins.get_mut(&id) {
                    w.kind = WinKind::Hidden;
                }
                Ok(1)
            }
            Event::WinClose => {
                if len < 1 {
                    return Err(rpc::Error::BadMessage);
                }
                let id = expect_int::<u32>(d)?;
                self.wins.swap_remove(&id);
                Ok(1)
            }
            Event::MsgSetPos => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                self.msg_grid = Some(expect_int(d)?);
                self.msg_row = expect_int(d)?;
                Ok(2)
            }
            Event::HlAttrDefine => self.style.apply(style::Event::HlAttrDefine, d, len),
            Event::HlGroupSet => {
                if len < 2 {
                    return Err(rpc::Error::BadMessage);
                }
                let name = rpc::expect_str(d)?;
                let separator = name == b"WinSeparator";
                let hl = expect_int::<u32>(d)?;
                if separator {
                    self.separator_hl = hl;
                }
                Ok(2)
            }
            Event::DefaultColorsSet => self.style.apply(style::Event::DefaultColorsSet, d, len),
            Event::ModeInfoSet => self.style.apply(style::Event::ModeInfoSet, d, len),
            Event::ModeChange => self.style.apply(style::Event::ModeChange, d, len),
            Event::BusyStart => self.style.apply(style::Event::BusyStart, d, len),
            Event::BusyStop => self.style.apply(style::Event::BusyStop, d, len),
            Event::Flush => Ok(0),
        }
    }
}

/// Sorts lowest first, as Neovim stacks them, and keeps the top-most
/// `MAX_FLOATS`: with more floats than that, it's the lowest that go.
pub(crate) fn keep_top_floats(placed: &mut Vec<Placed>) {
    placed.sort_by_key(|p| (p.zindex, p.compindex));
    let extra = placed.len().saturating_sub(MAX_FLOATS);
    placed.drain(..extra);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    GridResize,
    GridClear,
    GridLine,
    GridScroll,
    GridCursorGoto,
    GridDestroy,
    WinPos,
    WinExternalPos,
    WinFloatPos,
    WinHide,
    WinClose,
    MsgSetPos,
    HlAttrDefine,
    HlGroupSet,
    DefaultColorsSet,
    ModeInfoSet,
    ModeChange,
    BusyStart,
    BusyStop,
    Flush,
}

impl Event {
    /// The event named by a redraw event's name, as `std.meta.stringToEnum`.
    fn from_name(name: &[u8]) -> Option<Event> {
        Some(match name {
            b"grid_resize" => Event::GridResize,
            b"grid_clear" => Event::GridClear,
            b"grid_line" => Event::GridLine,
            b"grid_scroll" => Event::GridScroll,
            b"grid_cursor_goto" => Event::GridCursorGoto,
            b"grid_destroy" => Event::GridDestroy,
            b"win_pos" => Event::WinPos,
            b"win_external_pos" => Event::WinExternalPos,
            b"win_float_pos" => Event::WinFloatPos,
            b"win_hide" => Event::WinHide,
            b"win_close" => Event::WinClose,
            b"msg_set_pos" => Event::MsgSetPos,
            b"hl_attr_define" => Event::HlAttrDefine,
            b"hl_group_set" => Event::HlGroupSet,
            b"default_colors_set" => Event::DefaultColorsSet,
            b"mode_info_set" => Event::ModeInfoSet,
            b"mode_change" => Event::ModeChange,
            b"busy_start" => Event::BusyStart,
            b"busy_stop" => Event::BusyStop,
            b"flush" => Event::Flush,
            _ => return None,
        })
    }
}

/// "NW" as "nw"; anything else as "??".
fn lower2(s: &[u8]) -> [u8; 2] {
    if s.len() != 2 {
        return *b"??";
    }
    [s[0].to_ascii_lowercase(), s[1].to_ascii_lowercase()]
}

/// A float or an int, as Neovim sends positions.
fn expect_number(d: &mut Decoder<'_>) -> rpc::Result<f64> {
    match rpc::next_token(d)? {
        Token::Float(f) => Ok(f),
        Token::Int(n) => Ok(n as f64),
        Token::Uint(n) => Ok(n as f64),
        _ => Err(rpc::Error::BadMessage),
    }
}

fn expect_int<T: TryFrom<i64>>(d: &mut Decoder<'_>) -> rpc::Result<T> {
    T::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::msgpack::{
        write_array_header, write_bool, write_float, write_int, write_map_header, write_str,
    };
    use crate::nvim::nvim::{UiOptions, spawn_for_test_with};
    use crate::screen::grid::Cell;
    use crate::screen::style::{Attr, CursorMode, Shape, Underline};

    /// Builds redraw params from events written by `f`, then applies them.
    fn apply_test(gs: &mut Grids, f: impl FnOnce(&mut Vec<u8>)) -> bool {
        let mut w = Vec::new();
        f(&mut w);
        let mut d = Decoder::new();
        d.feed(&w);
        gs.apply_redraw(&mut d).unwrap()
    }

    fn ints(w: &mut Vec<u8>, values: &[i64]) {
        write_array_header(w, values.len() as u32);
        for &v in values {
            write_int(w, v);
        }
    }

    /// `[name, [args...]]` with int args.
    fn ev(w: &mut Vec<u8>, name: &str, args: &[i64]) {
        write_array_header(w, 2);
        write_str(w, name);
        ints(w, args);
    }

    /// `[name, [grid, window handle]]`, the handle as Neovim's ext 1.
    fn win_event(w: &mut Vec<u8>, name: &str, grid: i64, win: u16) {
        write_array_header(w, 2);
        write_str(w, name);
        write_array_header(w, 2);
        write_int(w, grid);
        w.extend_from_slice(&[0xc7, 0x03, 0x01, 0xcd, (win >> 8) as u8, win as u8]);
    }

    /// `["grid_line", [grid, row, col, [[text, 0]], false]]`
    fn line_event(w: &mut Vec<u8>, grid: i64, r: i64, col: i64, text: &str) {
        write_array_header(w, 2);
        write_str(w, "grid_line");
        write_array_header(w, 5);
        write_int(w, grid);
        write_int(w, r);
        write_int(w, col);
        write_array_header(w, 1);
        write_array_header(w, 2);
        write_str(w, text);
        write_int(w, 0);
        write_bool(w, false);
    }

    #[test]
    fn the_highlight_set_for_win_separator_is_kept_and_other_groups_ignored() {
        let mut gs = Grids::default();
        let group = |w: &mut Vec<u8>, name: &str, hl: i64| {
            write_array_header(w, 2);
            write_str(w, "hl_group_set");
            write_array_header(w, 2);
            write_str(w, name);
            write_int(w, hl);
        };
        apply_test(&mut gs, |w| {
            write_array_header(w, 3);
            group(w, "StatusLine", 3);
            group(w, "WinSeparator", 7);
            group(w, "VertSplit", 9);
        });
        assert_eq!(7, gs.separator_hl);
    }

    #[test]
    fn events_go_to_their_grids_windows_are_tracked() {
        let mut gs = Grids::default();
        let flushed = apply_test(&mut gs, |w| {
            write_array_header(w, 10);
            ev(w, "grid_resize", &[1, 4, 3]);
            ev(w, "grid_resize", &[2, 3, 2]);
            ev(w, "grid_resize", &[4, 4, 3]);
            line_event(w, 2, 1, 0, "x");
            line_event(w, 4, 0, 1, "m");
            win_event(w, "win_external_pos", 2, 1000);
            win_event(w, "win_pos", 3, 1001);
            ev(w, "msg_set_pos", &[4, 2]);
            ev(w, "grid_cursor_goto", &[2, 1, 2]);
            ev(w, "flush", &[]);
        });
        assert!(flushed);
        assert_eq!("   ", gs.get(2).unwrap().row_text(0));
        assert_eq!("x  ", gs.get(2).unwrap().row_text(1));
        assert_eq!("    ", gs.get(1).unwrap().row_text(1));
        assert_eq!(" m  ", gs.get(4).unwrap().row_text(0));
        assert_eq!(Some(2), gs.grid_of(1000));
        assert_eq!(WinKind::External, gs.win_on(2).unwrap().kind);
        assert_eq!(WinKind::Ordinary, gs.win_on(3).unwrap().kind);
        assert_eq!(2, gs.cursor_grid);
        assert_eq!(1, gs.get(2).unwrap().cursor_row);
        // grid 1 is 3 rows; messages from row 2 down
        assert_eq!(1, gs.msg_rows());

        apply_test(&mut gs, |w| {
            write_array_header(w, 4);
            ev(w, "win_hide", &[3]);
            ev(w, "win_close", &[2]);
            ev(w, "grid_destroy", &[2]);
            // a line for a grid that's gone is skipped
            line_event(w, 2, 0, 0, "y");
        });
        assert!(gs.get(2).is_none());
        assert_eq!(None, gs.grid_of(1000));
        assert_eq!(WinKind::Hidden, gs.win_on(3).unwrap().kind);
    }

    #[test]
    fn resize_line_cursor_and_flush() {
        let mut gs = Grids::default();
        let flushed = apply_test(&mut gs, |w| {
            write_array_header(w, 4);
            // ["grid_resize", [1, 6, 2]]
            ev(w, "grid_resize", &[1, 6, 2]);
            // ["grid_line", [1, 0, 0, [["h", 3], ["e"], ["l", 3, 2], ["o", 4]], false]]
            write_array_header(w, 2);
            write_str(w, "grid_line");
            write_array_header(w, 5);
            write_int(w, 1);
            write_int(w, 0);
            write_int(w, 0);
            write_array_header(w, 4);
            write_array_header(w, 2);
            write_str(w, "h");
            write_int(w, 3);
            write_array_header(w, 1);
            write_str(w, "e");
            write_array_header(w, 3);
            write_str(w, "l");
            write_int(w, 3);
            write_int(w, 2);
            write_array_header(w, 2);
            write_str(w, "o");
            write_int(w, 4);
            write_bool(w, false);
            // ["grid_cursor_goto", [1, 1, 2]]
            ev(w, "grid_cursor_goto", &[1, 1, 2]);
            ev(w, "flush", &[]);
        });
        assert!(flushed);
        let g = gs.get(1).unwrap();
        assert_eq!("hello ", g.row_text(0));
        assert_eq!("      ", g.row_text(1));
        // "e" carries "h"'s hl; the untouched last cell keeps 0
        let hls = [3u32, 3, 3, 3, 4, 0];
        for (c, hl) in g.row(0).iter().zip(hls) {
            assert_eq!(hl, c.hl);
        }
        assert_eq!(1, g.cursor_row);
        assert_eq!(2, g.cursor_col);
        assert!(g.dirty.is_set(0) && g.dirty.is_set(1));
    }

    #[test]
    fn wide_chars_and_long_clusters() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 2);
            ev(w, "grid_resize", &[1, 4, 1]);
            write_array_header(w, 2);
            write_str(w, "grid_line");
            write_array_header(w, 5);
            write_int(w, 1);
            write_int(w, 0);
            write_int(w, 0);
            write_array_header(w, 3);
            write_array_header(w, 2);
            write_str(w, "界");
            write_int(w, 0);
            write_array_header(w, 1);
            write_str(w, "");
            write_array_header(w, 1);
            let long = [b'x'; Cell::MAX_TEXT + 2];
            write_str(w, long);
            write_bool(w, false);
        });
        let g = gs.get(1).unwrap();
        assert_eq!("界".as_bytes(), g.row(0)[0].str());
        assert_eq!(b"", g.row(0)[1].str());
        assert_eq!("\u{fffd}".as_bytes(), g.row(0)[2].str());
    }

    #[test]
    fn hl_attr_define_and_default_colors() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 2);
            // ["default_colors_set", [0xffffff, 0x000000, -1, 0, 0]]
            ev(w, "default_colors_set", &[0xffffff, 0, -1, 0, 0]);
            // ["hl_attr_define", [5, {fg, bold, undercurl, url}, {}, []]]
            write_array_header(w, 2);
            write_str(w, "hl_attr_define");
            write_array_header(w, 4);
            write_int(w, 5);
            write_map_header(w, 4);
            write_str(w, "foreground");
            write_int(w, 0x112233);
            write_str(w, "bold");
            write_bool(w, true);
            write_str(w, "undercurl");
            write_bool(w, true);
            write_str(w, "url");
            write_str(w, "https://neovim.io");
            write_map_header(w, 0);
            write_array_header(w, 0);
        });
        let s = &gs.style;
        assert_eq!(Some(0xffffff), s.default_fg);
        assert_eq!(None, s.default_sp);
        let a: Attr = s.attrs[5];
        assert_eq!(Some(0x112233), a.fg);
        assert_eq!(None, a.bg);
        assert!(a.bold && !a.italic);
        assert_eq!(Underline::Curl, a.underline);
        assert_eq!(6, s.attrs.len());
    }

    #[test]
    fn cursor_modes_and_busy() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 3);
            // ["mode_info_set", [true, [{cursor_shape: "block"}, {cursor_shape: "vertical", blinkon: 500, blinkoff: 500, name: "insert"}]]]
            write_array_header(w, 2);
            write_str(w, "mode_info_set");
            write_array_header(w, 2);
            write_bool(w, true);
            write_array_header(w, 2);
            write_map_header(w, 1);
            write_str(w, "cursor_shape");
            write_str(w, "block");
            write_map_header(w, 4);
            write_str(w, "cursor_shape");
            write_str(w, "vertical");
            write_str(w, "blinkon");
            write_int(w, 500);
            write_str(w, "blinkoff");
            write_int(w, 500);
            write_str(w, "name");
            write_str(w, "insert");
            // ["mode_change", ["insert", 1]]
            write_array_header(w, 2);
            write_str(w, "mode_change");
            write_array_header(w, 2);
            write_str(w, "insert");
            write_int(w, 1);
            ev(w, "busy_start", &[]);
        });
        let s = &mut gs.style;
        assert_eq!(
            Some(CursorMode {
                shape: Shape::Vertical,
                blink: true
            }),
            s.cursor_mode()
        );
        assert!(s.busy);
        s.mode_idx = 0;
        assert_eq!(Some(CursorMode::default()), s.cursor_mode());
        s.cursor_style_enabled = false;
        assert_eq!(None, s.cursor_mode());
    }

    #[test]
    fn unknown_events_are_skipped() {
        let mut gs = Grids::default();
        let flushed = apply_test(&mut gs, |w| {
            // ["option_set", [...], [...]]
            write_array_header(w, 1);
            write_array_header(w, 3);
            write_str(w, "option_set");
            write_array_header(w, 2);
            write_str(w, "emoji");
            write_bool(w, true);
            write_array_header(w, 2);
            write_str(w, "linespace");
            write_int(w, 0);
        });
        assert!(!flushed);
    }

    /// Applies Neovim's redraws until `done` holds after a flush.
    fn pump_until(
        nvim: &mut crate::nvim::nvim::Nvim,
        gs: &mut Grids,
        done: impl Fn(&Grids) -> bool,
    ) {
        for _ in 0..100 {
            let mut fds = [libc::pollfd {
                fd: nvim.read_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: `fds` is one valid pollfd.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 2000) };
            assert!(ready > 0, "TestTimedOut");
            assert!(nvim.read().unwrap(), "NvimExited");
            while let Some(frame) = nvim.next().unwrap() {
                match frame.message().unwrap() {
                    rpc::Message::Notification { method, mut params } if method == b"redraw" => {
                        if gs.apply_redraw(&mut params).unwrap() && done(gs) {
                            return;
                        }
                    }
                    rpc::Message::Response { err: Some(e), .. } => {
                        panic!("neovim: {}", String::from_utf8_lossy(e));
                    }
                    _ => {}
                }
            }
        }
        panic!("TestTimedOut");
    }

    #[test]
    fn neovim_draws_windows_into_their_own_grids() {
        let Some(mut nvim) = spawn_for_test_with(&["nvim", "--clean", "--embed", "-n"]) else {
            return;
        };
        nvim.ui_attach(
            20,
            6,
            UiOptions {
                ext_multigrid: true,
                ..UiOptions::default()
            },
        )
        .unwrap();
        nvim.request("nvim_input", 1);
        write_str(nvim.writer(), "ihello<Esc>");
        nvim.flush().unwrap();

        let mut gs = Grids::default();
        pump_until(&mut nvim, &mut gs, |g| {
            let Some(id) = g.grid_of(1000) else {
                return false;
            };
            g.get(id).unwrap().row_text(0).starts_with("hello")
        });
        let first = gs.grid_of(1000).unwrap();
        assert_ne!(1, first);
        assert_eq!(WinKind::Ordinary, gs.win_on(first).unwrap().kind);
        assert_eq!(first, gs.cursor_grid);
        assert_eq!(4, gs.get(first).unwrap().cursor_col);

        // a split gets another grid; the first window, made external, keeps its own
        nvim.exec_lua(
            "vim.cmd.split()\nvim.api.nvim_win_set_config(1000, { external = true, width = 12, height = 3 })",
            0,
        );
        nvim.flush().unwrap();
        pump_until(&mut nvim, &mut gs, |g| {
            let Some(id) = g.grid_of(1000) else {
                return false;
            };
            g.win_on(id).unwrap().kind == WinKind::External
                && g.get(id).unwrap().width == 12
                && g.grid_of(1001).is_some()
                && g.get(id).unwrap().row_text(0).starts_with("hello")
        });
        let id = gs.grid_of(1000).unwrap();
        assert_eq!("hello       ", gs.get(id).unwrap().row_text(0));
        let second = gs.grid_of(1001).unwrap();
        assert_eq!(WinKind::Ordinary, gs.win_on(second).unwrap().kind);
        assert!(gs.msg_grid.is_some());
    }

    /// `["win_float_pos", [grid, win, anchor, anchor_grid, row, col, true, zindex, compindex, screen_row, screen_col]]`
    #[allow(clippy::too_many_arguments)]
    fn float_event(
        w: &mut Vec<u8>,
        grid: i64,
        anchor: &str,
        anchor_grid: i64,
        row: f64,
        col: f64,
        zindex: i64,
        screen: Option<[i64; 2]>,
    ) {
        write_array_header(w, 2);
        write_str(w, "win_float_pos");
        write_array_header(w, if screen.is_some() { 11 } else { 8 });
        write_int(w, grid);
        w.extend_from_slice(&[0xc7, 0x03, 0x01, 0xcd, 0x03, 0xf0]);
        write_str(w, anchor);
        write_int(w, anchor_grid);
        write_float(w, row);
        write_float(w, col);
        write_bool(w, true);
        write_int(w, zindex);
        if let Some(s) = screen {
            write_int(w, 0);
            write_int(w, s[0]);
            write_int(w, s[1]);
        }
    }

    #[test]
    fn floats_are_placed_over_the_grid_under_them() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 9);
            ev(w, "grid_resize", &[2, 20, 10]);
            win_event(w, "win_external_pos", 2, 1000);
            ev(w, "grid_resize", &[3, 4, 2]);
            ev(w, "grid_resize", &[4, 3, 1]);
            ev(w, "grid_resize", &[5, 5, 2]);
            // its bottom right corner at 6.5, 10 of grid 2
            float_event(w, 3, "SE", 2, 6.5, 10.0, 50, None);
            // over the float, so over grid 2 too; drawn above it
            float_event(w, 4, "NW", 3, 1.0, 1.0, 40, Some([0, 0]));
            // on the screen: where Neovim put it
            float_event(w, 5, "NW", 1, 30.0, 30.0, 50, Some([7, 2]));
            ev(w, "flush", &[]);
        });
        let over = gs.floats_over(2, false, &[]);
        assert_eq!(2, over.len());
        assert_eq!(
            Placed {
                grid: 4,
                row: 5,
                col: 7,
                zindex: 40,
                compindex: 0,
                ..Placed::default()
            },
            over[0]
        );
        assert_eq!(
            Placed {
                grid: 3,
                row: 4,
                col: 6,
                zindex: 50,
                compindex: 0,
                ..Placed::default()
            },
            over[1]
        );
        let all = gs.floats_over(2, true, &[]);
        assert_eq!(3, all.len());
        assert_eq!(
            Placed {
                grid: 5,
                row: 7,
                col: 2,
                zindex: 50,
                compindex: 0,
                on_screen: true,
                top: 5,
                ..Placed::default()
            },
            all[2]
        );
        // left out with the editor float it hangs from
        assert_eq!(2, gs.floats_over(2, true, &[5]).len());
    }

    #[test]
    fn past_the_cap_floats_over_a_pane_keep_the_top_most() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 40);
            for i in 0..20i64 {
                ev(w, "grid_resize", &[3 + i, 4, 1]);
                float_event(w, 3 + i, "NW", 1, i as f64, 0.0, 50 + i, None);
            }
        });
        let fs = gs.floats_over(1, true, &[]);
        assert_eq!(MAX_FLOATS, fs.len());
        assert_eq!(7, fs[0].grid);
        assert_eq!(22, fs.last().unwrap().grid);
        assert!(fs.windows(2).all(|w| w[0].zindex <= w[1].zindex));
    }

    #[test]
    fn floats_over_an_editor_float_hang_from_it() {
        let mut gs = Grids::default();
        apply_test(&mut gs, |w| {
            write_array_header(w, 6);
            ev(w, "grid_resize", &[3, 20, 8]);
            ev(w, "grid_resize", &[4, 10, 1]);
            ev(w, "grid_resize", &[5, 4, 2]);
            // a box on the screen, an input inside it, the pair on a third
            float_event(w, 3, "NW", 1, 2.0, 5.0, 50, Some([2, 5]));
            float_event(w, 4, "NW", 3, 1.0, 1.0, 51, None);
            float_event(w, 5, "NW", 4, 0.0, 2.0, 52, None);
        });
        assert_eq!(
            Place {
                root: 1,
                row: 2,
                col: 5,
                top: 3
            },
            gs.place(3).unwrap()
        );
        assert_eq!(
            Place {
                root: 1,
                row: 3,
                col: 6,
                top: 3
            },
            gs.place(4).unwrap()
        );
        assert_eq!(
            Place {
                root: 1,
                row: 3,
                col: 8,
                top: 3
            },
            gs.place(5).unwrap()
        );
        assert_eq!(0, gs.floats_over(1, true, &[3]).len());
    }
}
