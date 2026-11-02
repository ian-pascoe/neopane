//! Draws a `Grid`'s dirty rows to a terminal as escape sequences.
//! Follows Neovim's own TUI (`tui_flush` in src/nvim/tui/tui.c): trailing
//! blanks are cleared with EL instead of printed, and scrolls are repeated
//! with a scroll region (DECSTBM) instead of redrawing every moved row.

use std::io::Write;

use super::grid::{Cell, Grid};
use super::style::{Attr, CursorMode, Shape, Style, Underline};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct Cursor {
    pub row: u32,
    pub col: u32,
}

/// One frame of `grid` filling the terminal: the dirty rows, then the
/// cursor, inside synchronized output so the terminal shows it all at once.
/// Clears `grid.dirty`.
#[doc(hidden)]
pub fn frame(grid: &mut Grid, style: &Style, w: &mut Vec<u8>) {
    begin(w);
    replay_scrolls(grid, 0, w);
    let mut current: Option<u32> = None;
    for r in grid.dirty.iter() {
        draw_row(
            grid.row(r as u32),
            style,
            r as u32,
            grid.width,
            w,
            &mut current,
        );
    }
    grid.dirty.unset_all();
    end(
        style,
        Some(Cursor {
            row: grid.cursor_row,
            col: grid.cursor_col,
        }),
        w,
    );
}

/// Starts a frame: synchronized output on, cursor hidden while drawing.
pub(crate) fn begin(w: &mut Vec<u8>) {
    w.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
}

/// Repeats the grid's pending scrolls with scroll regions, then forgets them.
/// The grid must sit at the terminal's top left.
pub(crate) fn replay_scrolls(grid: &mut Grid, offset: u32, w: &mut Vec<u8>) {
    let scrolls = grid.pending_scrolls();
    if scrolls.is_empty() {
        return;
    }
    // SU and SD fill with the current background
    w.extend_from_slice(b"\x1b[0m");
    for s in scrolls {
        write!(w, "\x1b[{};{}r", s.top + 1 + offset, s.bot + offset).unwrap();
        if s.rows > 0 {
            write!(w, "\x1b[{}S", s.rows).unwrap();
        } else {
            write!(w, "\x1b[{}T", -s.rows).unwrap();
        }
    }
    w.extend_from_slice(b"\x1b[r");
    grid.scrolls_len = 0;
}

/// Ends a frame: the cursor where it belongs, or left hidden if None.
pub(crate) fn end(style: &Style, cursor: Option<Cursor>, w: &mut Vec<u8>) {
    if let Some(c) = cursor {
        if let Some(m) = style.cursor_mode() {
            write_cursor_style(m, w);
        }
        write!(w, "\x1b[0m\x1b[{};{}H", c.row + 1, c.col + 1).unwrap();
        if !style.busy {
            w.extend_from_slice(b"\x1b[?25h");
        }
    } else {
        w.extend_from_slice(b"\x1b[0m");
    }
    w.extend_from_slice(b"\x1b[?2026l");
}

/// DECSCUSR: 1 block, 3 underline, 5 bar; one more for steady.
fn write_cursor_style(m: CursorMode, w: &mut Vec<u8>) {
    let shape: u8 = match m.shape {
        Shape::Block => 1,
        Shape::Horizontal => 3,
        Shape::Vertical => 5,
    };
    write!(w, "\x1b[{} q", shape + u8::from(!m.blink)).unwrap();
}

/// Draws `row` at terminal row `r`, cut at `cols` columns. `current` is the
/// SGR the terminal has, None if unknown.
pub(crate) fn draw_row(
    row: &[Cell],
    style: &Style,
    r: u32,
    cols: u32,
    w: &mut Vec<u8>,
    current: &mut Option<u32>,
) {
    let cells = &row[0..row.len().min(cols as usize)];
    write!(w, "\x1b[{};1H", r + 1).unwrap();
    if cells.is_empty() {
        return clear_to_end(style, w, current);
    }

    // trailing blanks in the last cell's highlight are cleared, not printed
    let clear_hl = cells[cells.len() - 1].hl;
    let mut clear_col = cells.len();
    if can_clear(style, clear_hl) {
        while clear_col > 0 && is_blank(&cells[clear_col - 1], clear_hl) {
            clear_col -= 1;
        }
    }

    for (c, cell) in cells[0..clear_col].iter().enumerate() {
        // the right half of a wide char: the terminal already moved past it
        if cell.len == 0 {
            continue;
        }
        set_hl(style, cell.hl, w, current);
        // a wide char cut in half at the edge would wrap
        let cut = c + 1 == cells.len() && cells.len() < row.len() && row[c + 1].len == 0;
        w.extend_from_slice(if cut { b" " } else { cell.str() });
    }
    if clear_col < cells.len() {
        // EL fills with the current background
        set_hl(style, clear_hl, w, current);
        w.extend_from_slice(b"\x1b[K");
    } else if (cells.len() as u32) < cols {
        // past a grid narrower than the terminal
        clear_to_end(style, w, current);
    }
}

/// Blanks terminal row `r` in the default colors.
pub(crate) fn clear_row(style: &Style, r: u32, w: &mut Vec<u8>, current: &mut Option<u32>) {
    write!(w, "\x1b[{};1H", r + 1).unwrap();
    clear_to_end(style, w, current);
}

fn clear_to_end(style: &Style, w: &mut Vec<u8>, current: &mut Option<u32>) {
    set_hl(style, 0, w, current);
    w.extend_from_slice(b"\x1b[K");
}

/// EL keeps only the background, so nothing else may show on a blank.
fn can_clear(style: &Style, hl: u32) -> bool {
    let a = style.attr(hl);
    !a.reverse && !a.strikethrough && !a.overline && a.underline == Underline::None
}

fn is_blank(cell: &Cell, hl: u32) -> bool {
    cell.hl == hl && cell.str() == b" "
}

pub(crate) fn set_hl(style: &Style, hl: u32, w: &mut Vec<u8>, current: &mut Option<u32>) {
    if *current == Some(hl) {
        return;
    }
    *current = Some(hl);
    write_sgr(&style.attr(hl), style, w);
}

/// One SGR sequence that resets, then sets everything in `attr`. Colors an
/// `attr` leaves null use the style's defaults; if those are null too, the
/// terminal's own (from ext_termcolors).
pub(crate) fn write_sgr(attr: &Attr, style: &Style, w: &mut Vec<u8>) {
    w.extend_from_slice(b"\x1b[0");
    if attr.bold {
        w.extend_from_slice(b";1");
    }
    if attr.dim {
        w.extend_from_slice(b";2");
    }
    if attr.italic {
        w.extend_from_slice(b";3");
    }
    if attr.blink {
        w.extend_from_slice(b";5");
    }
    if attr.reverse {
        w.extend_from_slice(b";7");
    }
    if attr.conceal {
        w.extend_from_slice(b";8");
    }
    if attr.strikethrough {
        w.extend_from_slice(b";9");
    }
    if attr.overline {
        w.extend_from_slice(b";53");
    }
    w.extend_from_slice(match attr.underline {
        Underline::None => b"",
        Underline::Single => b";4",
        Underline::Double => b";4:2",
        Underline::Curl => b";4:3",
        Underline::Dotted => b";4:4",
        Underline::Dashed => b";4:5",
    });
    write_color(w, 38, attr.fg.or(style.default_fg));
    write_color(w, 48, attr.bg.or(style.default_bg));
    if attr.underline != Underline::None {
        write_color(w, 58, attr.sp.or(style.default_sp));
    }
    w.push(b'm');
}

/// `;38;2;r;g;b` and friends; nothing for None, which the reset covers.
fn write_color(w: &mut Vec<u8>, prefix: u8, color: Option<u32>) {
    let Some(c) = color else { return };
    write!(
        w,
        ";{};2;{};{};{}",
        prefix,
        c >> 16,
        (c >> 8) & 0xff,
        c & 0xff
    )
    .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid and the style it's drawn with.
    struct Fixture {
        g: Grid,
        s: Style,
    }

    impl Fixture {
        fn new(w: u32, h: u32) -> Fixture {
            let mut f = Fixture {
                g: Grid::default(),
                s: Style::default(),
            };
            f.g.resize(w, h);
            f.s.attrs.resize(3, Attr::default());
            f.s.attrs[1] = Attr {
                bold: true,
                fg: Some(0xff0000),
                ..Attr::default()
            };
            f.s.attrs[2] = Attr {
                bg: Some(0x0000ff),
                ..Attr::default()
            };
            f
        }

        fn expect_frame(&mut self, expected: &str) {
            let mut w = Vec::new();
            frame(&mut self.g, &self.s, &mut w);
            assert_eq!(expected, String::from_utf8(w).unwrap());
        }
    }

    const BEGIN_S: &str = "\x1b[?2026h\x1b[?25l";
    const END_S: &str = "\x1b[?25h\x1b[?2026l";

    #[test]
    fn blank_rows_are_just_cleared() {
        let mut f = Fixture::new(4, 2);
        f.expect_frame(&format!(
            "{BEGIN_S}\x1b[1;1H\x1b[0m\x1b[K\x1b[2;1H\x1b[K\x1b[0m\x1b[1;1H{END_S}"
        ));
        assert_eq!(0, f.g.dirty.count());
    }

    #[test]
    fn only_dirty_rows_styles_change_once_cursor_last() {
        let mut f = Fixture::new(5, 3);
        let g = &mut f.g;
        g.dirty.unset_all();
        g.cells[5] = Cell::init("h", 1);
        g.cells[6] = Cell::init("i", 1);
        g.cells[7] = Cell::init("!", 0);
        g.dirty.set(1);
        g.cursor_row = 1;
        g.cursor_col = 3;
        f.expect_frame(&format!(
            "{BEGIN_S}\x1b[2;1H\x1b[0;1;38;2;255;0;0mhi\x1b[0m!\x1b[K\x1b[0m\x1b[2;4H{END_S}"
        ));
    }

    #[test]
    fn colored_trailing_blanks_keep_their_background() {
        let mut f = Fixture::new(4, 1);
        f.g.cells[0] = Cell::init("x", 0);
        for c in &mut f.g.cells[1..] {
            *c = Cell::init(" ", 2);
        }
        f.expect_frame(&format!(
            "{BEGIN_S}\x1b[1;1H\x1b[0mx\x1b[0;48;2;0;0;255m\x1b[K\x1b[0m\x1b[1;1H{END_S}"
        ));
    }

    #[test]
    fn wide_chars_skip_their_right_half() {
        let mut f = Fixture::new(4, 1);
        f.g.cells[0] = Cell::init("界", 0);
        f.g.cells[1] = Cell::init("", 0);
        f.g.cells[2] = Cell::init("a", 0);
        f.expect_frame(&format!(
            "{BEGIN_S}\x1b[1;1H\x1b[0m界a\x1b[K\x1b[0m\x1b[1;1H{END_S}"
        ));
    }

    #[test]
    fn scrolls_go_first_inside_a_scroll_region() {
        let mut f = Fixture::new(2, 4);
        let g = &mut f.g;
        g.dirty.unset_all();
        g.scrolls[0] = super::super::grid::Scroll {
            top: 1,
            bot: 4,
            rows: 2,
        };
        g.scrolls[1] = super::super::grid::Scroll {
            top: 0,
            bot: 4,
            rows: -1,
        };
        g.scrolls_len = 2;
        f.expect_frame(&format!(
            "{BEGIN_S}\x1b[0m\x1b[2;4r\x1b[2S\x1b[1;4r\x1b[1T\x1b[r\x1b[0m\x1b[1;1H{END_S}"
        ));
        assert_eq!(0, f.g.pending_scrolls().len());
    }

    #[test]
    fn cursor_style_and_busy_hides_the_cursor() {
        let mut f = Fixture::new(2, 1);
        f.g.dirty.unset_all();
        f.s.cursor_modes.push(CursorMode {
            shape: Shape::Vertical,
            blink: true,
        });
        f.s.cursor_style_enabled = true;
        f.s.busy = true;
        f.expect_frame(&format!("{BEGIN_S}\x1b[5 q\x1b[0m\x1b[1;1H\x1b[?2026l"));
    }

    #[test]
    fn rows_are_cut_at_the_terminals_width_past_a_narrow_grid_is_cleared() {
        let mut f = Fixture::new(4, 1);
        for (i, c) in "ab".bytes().enumerate() {
            f.g.cells[i] = Cell::init([c], 1);
        }
        f.g.cells[2] = Cell::init("界", 1);
        f.g.cells[3] = Cell::init("", 1);
        let mut w = Vec::new();
        let mut current = None;
        // the wide char would straddle the edge, so a blank stands in
        draw_row(f.g.row(0), &f.s, 0, 3, &mut w, &mut current);
        assert_eq!(
            "\x1b[1;1H\x1b[0;1;38;2;255;0;0mab ",
            String::from_utf8(w.clone()).unwrap()
        );
        w.clear();
        draw_row(f.g.row(0), &f.s, 0, 6, &mut w, &mut current);
        assert_eq!("\x1b[1;1Hab界\x1b[0m\x1b[K", String::from_utf8(w).unwrap());
    }

    #[test]
    fn sgr_uses_defaults_for_null_colors() {
        let s = Style {
            default_fg: Some(0x010203),
            ..Style::default()
        };
        let mut w = Vec::new();
        write_sgr(
            &Attr {
                italic: true,
                underline: Underline::Curl,
                sp: Some(0x00ff00),
                ..Attr::default()
            },
            &s,
            &mut w,
        );
        assert_eq!(
            "\x1b[0;3;4:3;38;2;1;2;3;58;2;0;255;0m",
            String::from_utf8(w).unwrap()
        );
    }
}
