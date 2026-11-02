//! One of Neovim's grids: its cells, dirty rows, cursor and pending scrolls.
//! `Grids` routes redraw events here by grid id.

use crate::nvim::msgpack::Decoder;
use crate::nvim::rpc;

/// Rows changed since the renderer last cleared this. Zig's
/// `std.DynamicBitSetUnmanaged`, with the operations `Grid` and its users need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirtyRows {
    words: Vec<u64>,
    len: usize,
}

impl DirtyRows {
    /// Number of rows tracked.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Tracks `new_len` rows; rows added are set to `fill`.
    pub fn resize(&mut self, new_len: usize, fill: bool) {
        let old = self.len;
        self.words.resize(new_len.div_ceil(64), 0);
        self.len = new_len;
        if new_len > old {
            for i in old..new_len {
                self.set_value(i, fill);
            }
        } else {
            // keep the bits past the end clear
            self.mask_tail();
        }
    }

    fn mask_tail(&mut self) {
        let rem = self.len % 64;
        if rem != 0
            && let Some(last) = self.words.last_mut()
        {
            *last &= (1u64 << rem) - 1;
        }
    }

    pub fn is_set(&self, i: usize) -> bool {
        debug_assert!(i < self.len);
        self.words[i / 64] & (1u64 << (i % 64)) != 0
    }

    pub fn set(&mut self, i: usize) {
        debug_assert!(i < self.len);
        self.words[i / 64] |= 1u64 << (i % 64);
    }

    pub fn unset(&mut self, i: usize) {
        debug_assert!(i < self.len);
        self.words[i / 64] &= !(1u64 << (i % 64));
    }

    pub fn set_value(&mut self, i: usize, value: bool) {
        if value {
            self.set(i);
        } else {
            self.unset(i);
        }
    }

    pub fn set_all(&mut self) {
        self.words.fill(!0);
        self.mask_tail();
    }

    pub fn unset_all(&mut self) {
        self.words.fill(0);
    }

    /// How many rows are set.
    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The set rows, in ascending order.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.len).filter(|&i| self.is_set(i))
    }
}

#[derive(Debug, Clone)]
pub struct Grid {
    pub width: u32,
    pub height: u32,
    /// Row-major, `width * height` cells.
    pub cells: Vec<Cell>,
    /// Rows changed since the renderer last cleared this.
    pub dirty: DirtyRows,
    pub cursor_row: u32,
    pub cursor_col: u32,
    /// Full-width scrolls since the last frame, for the terminal to repeat.
    pub scrolls: [Scroll; MAX_SCROLLS],
    pub scrolls_len: u8,
}

impl Default for Grid {
    fn default() -> Self {
        Grid {
            width: 0,
            height: 0,
            cells: Vec::new(),
            dirty: DirtyRows::default(),
            cursor_row: 0,
            cursor_col: 0,
            scrolls: [Scroll::default(); MAX_SCROLLS],
            scrolls_len: 0,
        }
    }
}

const MAX_SCROLLS: usize = 8;

/// Rows `top..bot` moved up by `rows`, down if negative.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Scroll {
    pub top: u32,
    pub bot: u32,
    pub rows: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    /// One grapheme cluster. Empty for the right half of a wide char.
    pub text: [u8; Cell::MAX_TEXT],
    pub len: u8,
    pub hl: u32,
}

impl Default for Cell {
    fn default() -> Self {
        let mut text = [0; Cell::MAX_TEXT];
        text[0] = b' ';
        Cell {
            text,
            len: 1,
            hl: 0,
        }
    }
}

impl Cell {
    /// Longer clusters are stored as U+FFFD.
    pub const MAX_TEXT: usize = 31;

    pub fn str(&self) -> &[u8] {
        &self.text[0..self.len as usize]
    }

    pub fn init(text: impl AsRef<[u8]>, hl: u32) -> Cell {
        let mut c = Cell {
            hl,
            ..Cell::default()
        };
        let text = text.as_ref();
        let t: &[u8] = if text.len() <= Cell::MAX_TEXT {
            text
        } else {
            "\u{fffd}".as_bytes()
        };
        c.text[0..t.len()].copy_from_slice(t);
        c.len = t.len() as u8;
        c
    }
}

impl Grid {
    /// Scrolls to repeat before drawing the dirty rows.
    pub fn pending_scrolls(&self) -> &[Scroll] {
        &self.scrolls[0..self.scrolls_len as usize]
    }

    pub fn row(&self, r: u32) -> &[Cell] {
        let start = r as usize * self.width as usize;
        &self.cells[start..start + self.width as usize]
    }

    /// New size, keeping the cells that still fit; the rest are blank. Every
    /// row is dirty. Neovim only redraws a resized window's rows that changed.
    pub fn resize(&mut self, w: u32, h: u32) {
        let mut cells = vec![Cell::default(); w as usize * h as usize];
        let keep = w.min(self.width) as usize;
        for r in 0..h.min(self.height) as usize {
            let dst = r * w as usize;
            let src = r * self.width as usize;
            cells[dst..dst + keep].copy_from_slice(&self.cells[src..src + keep]);
        }
        self.cells = cells;
        self.width = w;
        self.height = h;
        self.dirty.resize(h as usize, true);
        self.dirty.set_all();
        self.scrolls_len = 0;
        self.cursor_row = self.cursor_row.min(h.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(w.saturating_sub(1));
    }

    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
        self.dirty.set_all();
        self.scrolls_len = 0;
    }

    /// `grid_line`'s cells: `[text, hl_id?, repeat?]`, from column `col`.
    pub fn line(&mut self, d: &mut Decoder<'_>, r: u32, col: u32) -> rpc::Result<()> {
        if r >= self.height {
            return Err(rpc::Error::BadMessage);
        }
        let width = self.width;
        let start = r as usize * width as usize;
        let dst = &mut self.cells[start..start + width as usize];
        let mut c = col;
        // an absent hl_id means the previous cell's
        let mut hl: u32 = 0;
        let count = rpc::expect_array(d)?;
        for _ in 0..count {
            let len = rpc::expect_array(d)?;
            if len == 0 {
                return Err(rpc::Error::BadMessage);
            }
            let text = rpc::expect_str(d)?;
            if len >= 2 {
                hl = expect_int(d)?;
            }
            let repeat: u32 = if len >= 3 { expect_int(d)? } else { 1 };
            for _ in 3..len.max(3) {
                rpc::skip(d)?;
            }
            let cell = Cell::init(text, hl);
            for _ in 0..repeat {
                if c >= width {
                    return Err(rpc::Error::BadMessage);
                }
                dst[c as usize] = cell;
                c += 1;
            }
        }
        self.dirty.set(r as usize);
        Ok(())
    }

    /// Moves rows `top..bot`, columns `left..right`, up by `rows` (down if
    /// negative). The rows left behind are refilled by grid_line events.
    pub fn scroll(
        &mut self,
        top: u32,
        bot: u32,
        left: u32,
        right: u32,
        rows: i64,
    ) -> rpc::Result<()> {
        if top > bot || bot > self.height || left > right || right > self.width {
            return Err(rpc::Error::BadMessage);
        }
        if rows == 0 {
            return Ok(());
        }
        let n: u32 = rows.unsigned_abs().min((bot - top) as u64) as u32;
        let w = right - left;
        if rows > 0 {
            // up: copy top to bottom, so no source row is overwritten first
            let mut r = top;
            while r + n < bot {
                self.copy_row(r + n, r, left, w);
                r += 1;
            }
        } else {
            // down: copy bottom to top, for the same reason
            let mut r = bot;
            while r > top + n {
                r -= 1;
                self.copy_row(r - n, r, left, w);
            }
        }

        // a split window's scroll can't be repeated, so redraw its rows
        let full = left == 0 && right == self.width;
        if !full || n == bot - top || self.scrolls_len as usize == MAX_SCROLLS {
            for r in top..bot {
                self.dirty.set(r as usize);
            }
            return Ok(());
        }
        self.scrolls[self.scrolls_len as usize] = Scroll {
            top,
            bot,
            rows: if rows > 0 { n as i32 } else { -(n as i32) },
        };
        self.scrolls_len += 1;
        // dirty bits move with their rows; only the rows left behind are new
        if rows > 0 {
            for r in top..bot - n {
                let v = self.dirty.is_set((r + n) as usize);
                self.dirty.set_value(r as usize, v);
            }
            for r in bot - n..bot {
                self.dirty.set(r as usize);
            }
        } else {
            let mut r = bot;
            while r > top + n {
                r -= 1;
                let v = self.dirty.is_set((r - n) as usize);
                self.dirty.set_value(r as usize, v);
            }
            for i in top..top + n {
                self.dirty.set(i as usize);
            }
        }
        Ok(())
    }

    fn copy_row(&mut self, from: u32, to: u32, left: u32, w: u32) {
        let width = self.width as usize;
        let src = from as usize * width + left as usize;
        let dst = to as usize * width + left as usize;
        self.cells.copy_within(src..src + w as usize, dst);
    }

    /// Row `r` as text, wide-char halves dropped.
    pub fn row_text(&self, r: u32) -> String {
        let mut out = Vec::new();
        for c in self.row(r) {
            out.extend_from_slice(c.str());
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

fn expect_int(d: &mut Decoder<'_>) -> rpc::Result<u32> {
    u32::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_keeps_what_still_fits() {
        let mut g = Grid::default();
        g.resize(3, 2);
        for (i, c) in g.cells.iter_mut().enumerate() {
            *c = Cell::init([b'a' + i as u8], 0);
        }
        g.dirty.unset_all();
        g.resize(2, 3);
        for (r, want) in ["ab", "de", "  "].into_iter().enumerate() {
            assert_eq!(want, g.row_text(r as u32));
        }
        assert_eq!(3, g.dirty.count());
    }

    #[test]
    fn scroll_up_and_down() {
        let mut g = Grid::default();
        g.resize(2, 5);
        for r in 0..5 {
            g.cells[r * 2] = Cell::init([b'a' + r as u8], 0);
        }

        // rows 1..4 up by 1: rows 1..3 get rows 2..4; row 3 is left for grid_line
        g.scroll(1, 4, 0, 2, 1).unwrap();
        for (r, want) in ["a ", "c ", "d ", "d ", "e "].into_iter().enumerate() {
            assert_eq!(want, g.row_text(r as u32));
        }

        // rows 0..5 down by 2: rows 2..5 get rows 0..3; rows 0..2 are left
        g.scroll(0, 5, 0, 2, -2).unwrap();
        for (r, want) in ["a ", "c ", "a ", "c ", "d "].into_iter().enumerate() {
            assert_eq!(want, g.row_text(r as u32));
        }
    }

    #[test]
    fn full_width_scrolls_are_kept_dirty_rows_move_with_them() {
        let mut g = Grid::default();
        g.resize(2, 5);
        g.dirty.unset_all();
        g.dirty.set(3);

        g.scroll(0, 5, 0, 2, 2).unwrap();
        assert_eq!(
            &[Scroll {
                top: 0,
                bot: 5,
                rows: 2
            }][..],
            g.pending_scrolls()
        );
        // row 3 moved to 1; rows 3 and 4 were left behind
        for (r, want) in [false, true, false, true, true].into_iter().enumerate() {
            assert_eq!(want, g.dirty.is_set(r));
        }

        // not full width: no scroll kept, the region is redrawn
        g.dirty.unset_all();
        g.scroll(1, 3, 0, 1, -1).unwrap();
        assert_eq!(1, g.pending_scrolls().len());
        for (r, want) in [false, true, true, false, false].into_iter().enumerate() {
            assert_eq!(want, g.dirty.is_set(r));
        }
    }
}
