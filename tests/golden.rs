//! Golden tests: the renderer's output, fed to Ghostty's terminal emulator,
//! must produce the screen the `Grid` describes.
//!
//! Built with the `golden` feature, which needs Ghostty: run them with
//! `scripts/golden.sh`.

use std::error::Error;

use neopane::nvim::msgpack;
use neopane::nvim::nvim::{Nvim, UiOptions};
use neopane::nvim::rpc;
use neopane::nvim::windows::{self, Note};
use neopane::screen::grid::{Cell, Grid};
use neopane::screen::grids::{Grids, Placed, WinKind};
use neopane::screen::pane_view::{LaidOut, Opts, PaneView, Separator, Shown, tabline_slice};
use neopane::screen::render;
use neopane::screen::style::{Attr, STATUSLINE_BASE, Shape, Style, Underline};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Ghostty's terminal, as the golden tests read it.
mod vt {
    use ghostty_vt::fmt::{Format, Formatter, FormatterOptions};
    use ghostty_vt::render::{CursorVisualStyle, RenderState};
    use ghostty_vt::screen::{CellContentTag, CellWide};
    use ghostty_vt::style::{RgbColor, StyleColor, Underline};
    use ghostty_vt::terminal::{Mode, Point, PointCoordinate};
    use ghostty_vt::{Terminal, TerminalOptions};

    use super::{Look, Result};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CursorShape {
        Block,
        Underline,
        Bar,
        BlockHollow,
    }

    /// One cell of the emulator's screen.
    #[derive(Debug, Clone)]
    pub struct CellInfo {
        /// The first codepoint, then the rest of the grapheme cluster. Empty
        /// for a cell with no text.
        pub text: Vec<char>,
        pub spacer_tail: bool,
        /// Its look, or `PaletteColor` for a palette colour, which the renderer never sends.
        pub look: std::result::Result<Look, &'static str>,
    }

    pub struct Cursor {
        pub row: u32,
        pub col: u32,
        pub shape: CursorShape,
        pub blinking: bool,
    }

    pub struct Vt {
        term: Terminal,
    }

    fn rgb(v: RgbColor) -> u32 {
        (v.r as u32) << 16 | (v.g as u32) << 8 | v.b as u32
    }

    /// The renderer only sends RGB, so a palette colour is an error.
    fn color(c: StyleColor) -> std::result::Result<Option<u32>, &'static str> {
        match c {
            StyleColor::None => Ok(None),
            StyleColor::Rgb(v) => Ok(Some(rgb(v))),
            StyleColor::Palette(_) => Err("PaletteColor"),
        }
    }

    impl Vt {
        pub fn new(cols: u16, rows: u16) -> Result<Vt> {
            let term = Terminal::new(TerminalOptions {
                cols,
                rows,
                max_scrollback: 0,
            })?;
            Ok(Vt { term })
        }

        /// Feeds the terminal what a program wrote to it.
        pub fn feed(&mut self, bytes: &[u8]) {
            self.term.vt_write(bytes);
        }

        /// The cell at column `x`, row `y` of the active screen.
        pub fn cell(&self, x: u32, y: u32) -> Result<CellInfo> {
            let r = self.term.grid_ref(Point::Active(PointCoordinate {
                x: u16::try_from(x)?,
                y,
            }))?;
            let cell = r.cell()?;
            let s = r.style()?;
            let wide = cell.wide()?;
            let tag = cell.content_tag()?;
            let mut buf = ['\0'; 32];
            let n = r.graphemes(&mut buf)?;
            let underline: u8 = match s.underline {
                Underline::None => 0,
                Underline::Single => 1,
                Underline::Double => 2,
                Underline::Curly => 3,
                Underline::Dotted => 4,
                Underline::Dashed => 5,
                _ => return Err("unknown underline".into()),
            };
            // EL leaves blanks that carry only a background
            let bg = if tag == CellContentTag::BgColorRgb {
                Ok(Some(rgb(cell.bg_color_rgb()?)))
            } else {
                color(s.bg_color)
            };
            let look = (|| {
                Ok(Look {
                    fg: color(s.fg_color)?,
                    bg: bg?,
                    ul_color: if underline != 0 {
                        color(s.underline_color)?
                    } else {
                        None
                    },
                    underline,
                    bold: s.bold,
                    italic: s.italic,
                    dim: s.faint,
                    blink: s.blink,
                    reverse: s.inverse,
                    conceal: s.invisible,
                    strikethrough: s.strikethrough,
                    overline: s.overline,
                })
            })();
            Ok(CellInfo {
                text: buf[..n].to_vec(),
                spacer_tail: wide == CellWide::SpacerTail,
                look,
            })
        }

        /// `modes.get(.cursor_visible)`.
        pub fn cursor_visible(&self) -> Result<bool> {
            Ok(self.term.mode(Mode::CURSOR_VISIBLE)?)
        }

        /// `screen.cursor` (position and style) and `modes.get(.cursor_blinking)`.
        pub fn cursor(&self) -> Result<Cursor> {
            let mut rs = RenderState::new()?;
            let snap = rs.update(&self.term)?;
            let c = snap.cursor()?;
            let blinking = self.term.mode(Mode::CURSOR_BLINKING)?;
            assert_eq!(c.blinking, blinking);
            Ok(Cursor {
                row: self.term.cursor_y()? as u32,
                col: self.term.cursor_x()? as u32,
                shape: match c.visual_style {
                    CursorVisualStyle::Block => CursorShape::Block,
                    CursorVisualStyle::Underline => CursorShape::Underline,
                    CursorVisualStyle::Bar => CursorShape::Bar,
                    CursorVisualStyle::BlockHollow => CursorShape::BlockHollow,
                    _ => return Err("unknown cursor style".into()),
                },
                blinking,
            })
        }

        /// `plainString`: the screen's text.
        pub fn plain_string(&self) -> Result<String> {
            let mut f = Formatter::new(
                &self.term,
                FormatterOptions::new()
                    .with_format(Format::Plain)
                    .with_trim(true),
            )?;
            let b = f.format_alloc()?;
            Ok(String::from_utf8_lossy(b.as_ref()).into_owned())
        }
    }
}

use vt::{CursorShape, Vt};

/// How a cell looks, from either side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Look {
    fg: Option<u32>,
    bg: Option<u32>,
    ul_color: Option<u32>,
    underline: u8,
    bold: bool,
    italic: bool,
    dim: bool,
    blink: bool,
    reverse: bool,
    conceal: bool,
    strikethrough: bool,
    overline: bool,
}

impl Look {
    /// A blank has no glyph, so only what draws without one counts.
    fn blank(self) -> Look {
        Look {
            fg: if self.reverse { self.fg } else { None },
            bg: self.bg,
            reverse: self.reverse,
            underline: self.underline,
            ul_color: self.ul_color,
            strikethrough: self.strikethrough,
            overline: self.overline,
            ..Look::default()
        }
    }
}

fn grid_look(g: &Style, hl: u32) -> Look {
    let a: Attr = g.attr(hl);
    let underline = match a.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
        Underline::Curl => 3,
        Underline::Dotted => 4,
        Underline::Dashed => 5,
    };
    Look {
        fg: a.fg.or(g.default_fg),
        bg: a.bg.or(g.default_bg),
        ul_color: if underline != 0 {
            a.sp.or(g.default_sp)
        } else {
            None
        },
        underline,
        bold: a.bold,
        italic: a.italic,
        dim: a.dim,
        blink: a.blink,
        reverse: a.reverse,
        conceal: a.conceal,
        strikethrough: a.strikethrough,
        overline: a.overline,
    }
}

/// Fails if Ghostty's screen differs from the grid anywhere, or the cursor
/// isn't on the grid's.
fn expect_screen(t: &Vt, g: &Grid, style: &Style) -> Result<()> {
    expect_rows(t, g, style, 0, 0, g.height, g.width)?;
    expect_cursor(
        t,
        style,
        Some(render::Cursor {
            row: g.cursor_row,
            col: g.cursor_col,
        }),
    )
}

/// Grid rows `first..first + n`, cut at `cols`, must show at terminal rows
/// from `top`.
fn expect_rows(
    t: &Vt,
    g: &Grid,
    style: &Style,
    first: u32,
    top: u32,
    n: u32,
    cols: u32,
) -> Result<()> {
    for i in 0..n {
        for c in 0..g.width.min(cols) {
            let r = first + i;
            let cell = g.row(r)[c as usize];
            let pc = t.cell(c, top + i)?;
            // a wide char cut at the edge is drawn as a blank
            let cut = c + 1 == cols && c + 1 < g.width && g.row(r)[c as usize + 1].len == 0;
            let want = if cut { Cell::init(" ", cell.hl) } else { cell };
            if let Err(err) = expect_cell(&pc, style, want) {
                eprintln!(
                    "golden: grid row {r} col {c} ({}) at terminal row {}: {err}",
                    String::from_utf8_lossy(cell.str()),
                    top + i
                );
                dump(t, g)?;
                return Err(err);
            }
        }
    }
    Ok(())
}

/// The cursor must be at `want` and showing, or hidden if None.
fn expect_cursor(t: &Vt, style: &Style, want: Option<render::Cursor>) -> Result<()> {
    let Some(c) = want else {
        if t.cursor_visible()? {
            return Err("CursorVisibility".into());
        }
        return Ok(());
    };
    let cur = t.cursor()?;
    if cur.row != c.row || cur.col != c.col {
        eprintln!(
            "golden: cursor at {},{}, want {},{}",
            cur.row, cur.col, c.row, c.col
        );
        return Err("CursorMismatch".into());
    }
    if t.cursor_visible()? == style.busy {
        return Err("CursorVisibility".into());
    }
    if let Some(m) = style.cursor_mode() {
        let shape = match m.shape {
            Shape::Block => CursorShape::Block,
            Shape::Horizontal => CursorShape::Underline,
            Shape::Vertical => CursorShape::Bar,
        };
        if cur.shape != shape {
            return Err("CursorShape".into());
        }
        if cur.blinking != m.blink {
            return Err("CursorBlink".into());
        }
    }
    Ok(())
}

fn expect_cell(pc: &vt::CellInfo, style: &Style, cell: Cell) -> Result<()> {
    if cell.len == 0 {
        if !pc.spacer_tail {
            return Err("WantSpacer".into());
        }
        return Ok(());
    }
    if pc.spacer_tail {
        return Err("UnwantedSpacer".into());
    }

    // text: Ghostty keeps a cluster's first codepoint in the cell
    let want: Vec<char> = std::str::from_utf8(cell.str())?.chars().collect();
    let got: Vec<char> = if pc.text.is_empty() {
        vec![' ']
    } else {
        pc.text.clone()
    };
    if want != got {
        return Err("TextMismatch".into());
    }

    let mut expected = grid_look(style, cell.hl);
    let mut actual = pc.look.map_err(|e| e.to_string())?;
    if want == [' '] {
        expected = expected.blank();
        actual = actual.blank();
    }
    if expected != actual {
        eprintln!("want {expected:?}\n got {actual:?}");
        return Err("LookMismatch".into());
    }
    Ok(())
}

fn dump(t: &Vt, g: &Grid) -> Result<()> {
    eprintln!("ghostty:\n{}\ngrid:", t.plain_string()?);
    for r in 0..g.height {
        for c in g.row(r) {
            eprint!("{}", String::from_utf8_lossy(c.str()));
        }
        eprintln!();
    }
    Ok(())
}

/// Renders a frame of `g` into `t`, then checks them against each other.
fn check(t: &mut Vt, g: &mut Grid, style: &Style) -> Result<()> {
    let mut out = Vec::new();
    render::frame(g, style, &mut out);
    t.feed(&out);
    expect_screen(t, g, style)
}

fn test_terminal(cols: u16, rows: u16) -> Result<Vt> {
    Vt::new(cols, rows)
}

/// Starts Neovim for a test, or None (after a note) if it isn't installed,
/// so the test is skipped. Under `CI` (set and not empty) it panics instead,
/// as `nvim_missing` in src/nvim/nvim.rs does.
fn spawn_nvim(argv: &[&str]) -> Option<Nvim> {
    match Nvim::spawn(argv) {
        Ok(nvim) => Some(nvim),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if std::env::var_os("CI").is_some_and(|v| !v.is_empty()) {
                panic!("nvim isn't installed, and CI is set");
            }
            eprintln!("skipped: nvim isn't installed");
            None
        }
        Err(e) => panic!("spawning nvim: {e}"),
    }
}

/// Waits up to `ms` for `nvim` to have something to read.
fn readable(nvim: &Nvim, ms: i32) -> bool {
    let mut fds = [libc::pollfd {
        fd: nvim.read_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: `fds` is one valid pollfd.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) > 0 }
}

#[test]
fn styles_wide_chars_and_blanks() {
    let mut t = test_terminal(8, 3).unwrap();
    let mut style = Style {
        default_fg: Some(0xeeeeee),
        default_bg: Some(0x111111),
        ..Style::default()
    };
    let mut g = Grid::default();
    g.resize(8, 3);
    style.attrs.extend_from_slice(&[
        Attr::default(),
        Attr {
            bold: true,
            fg: Some(0xff0000),
            ..Attr::default()
        },
        Attr {
            bg: Some(0x0000ff),
            ..Attr::default()
        },
        Attr {
            reverse: true,
            ..Attr::default()
        },
        Attr {
            underline: Underline::Curl,
            sp: Some(0x00ff00),
            italic: true,
            ..Attr::default()
        },
        Attr {
            strikethrough: true,
            overline: true,
            dim: true,
            ..Attr::default()
        },
    ]);
    let text: [(&str, u32); 8] = [
        ("h", 1),
        ("i", 1),
        ("界", 0),
        ("", 0),
        ("u", 4),
        ("s", 5),
        (" ", 2),
        (" ", 2),
    ];
    for (i, c) in text.iter().enumerate() {
        g.cells[i] = Cell::init(c.0, c.1);
    }
    // a reverse line, as cursorline can make
    g.cells[8] = Cell::init("x", 3);
    for c in &mut g.cells[9..16] {
        *c = Cell::init(" ", 3);
    }
    for c in &mut g.cells[16..24] {
        *c = Cell::init(" ", 4);
    }
    g.cursor_row = 1;
    g.cursor_col = 1;
    check(&mut t, &mut g, &style).unwrap();

    // a second frame redraws only what changed
    g.cells[0] = Cell::init("H", 0);
    g.dirty.set(0);
    check(&mut t, &mut g, &style).unwrap();
}

/// Steps of keys for the live test. Each runs once the last has settled.
const STEPS: &[&str] = &[
    ":set number cursorline<CR>",
    ":call setline(1, map(range(1, 200), '\"line \" . v:val . \" foo 界 bar\"'))<CR>",
    "<C-e>",
    "<C-e><C-e>",
    "<C-y>",
    "<C-d>",
    "<C-u>",
    "G",
    "gg",
    "30j",
    ":hi CursorLine gui=reverse<CR>",
    "<C-e>",
    ":hi Error gui=undercurl,bold guisp=red<CR>:call matchadd('Error', 'foo')<CR>",
    "vjj",
    "<Esc>",
    ":vsplit<CR>",
    "<C-e><C-e>",
    "<C-w>l<C-d>",
    ":split<CR>",
    "<C-y>",
    ":only<CR>",
    ":set nonumber<CR>",
    "ohello",
    "<Esc>",
    "dd",
];

#[test]
fn live_neovim_session() {
    let cols = 40;
    let rows = 12;
    let Some(mut nvim) = spawn_nvim(&["nvim", "--clean", "--embed", "-n"]) else {
        return;
    };
    nvim.ui_attach(cols, rows, UiOptions::default()).unwrap();

    let mut t = test_terminal(cols as u16, rows as u16).unwrap();
    let mut gs = Grids::default();

    let mut stats = Stats::default();
    settle(&mut nvim, &mut t, &mut gs, &mut stats).unwrap();
    for keys in STEPS {
        nvim.request("nvim_input", 1);
        msgpack::write_str(nvim.writer(), keys);
        nvim.flush().unwrap();
        if let Err(err) = settle(&mut nvim, &mut t, &mut gs, &mut stats) {
            eprintln!("golden: after step {keys}");
            panic!("{err}");
        }
    }
    assert!(stats.frames > STEPS.len());
    assert!(stats.scrolls > 0);
    assert!(stats.bar_cursor > 0);
}

#[derive(Default)]
struct Stats {
    frames: usize,
    scrolls: usize,
    bar_cursor: usize,
}

/// Applies redraws until Neovim goes quiet, checking every frame.
fn settle(nvim: &mut Nvim, t: &mut Vt, gs: &mut Grids, stats: &mut Stats) -> Result<()> {
    loop {
        if !readable(nvim, 150) {
            return Ok(());
        }
        if !nvim.read()? {
            return Err("NvimExited".into());
        }
        while let Some(frame) = nvim.next()? {
            match frame.message()? {
                rpc::Message::Notification { method, mut params } => {
                    if method == b"redraw" && gs.apply_redraw(&mut params)? {
                        let Some(g) = gs.grids.get_mut(&1) else {
                            continue;
                        };
                        stats.scrolls += g.pending_scrolls().len();
                        check(t, g, &gs.style)?;
                        stats.frames += 1;
                        if let Some(m) = gs.style.cursor_mode() {
                            stats.bar_cursor += (m.shape == Shape::Vertical) as usize;
                        }
                    }
                }
                rpc::Message::Response { err, .. } => {
                    if err.is_some() {
                        return Err("NvimError".into());
                    }
                }
                rpc::Message::Request { id, .. } => nvim.respond_error(id, "golden test")?,
            }
        }
    }
}

/// A pane for the multigrid test: what it shows and Ghostty's screen of it.
struct TestPane {
    view: PaneView,
    t: Vt,
    win: i64,
}

#[derive(Default)]
struct MultiStats {
    frames: usize,
    msg_rows: usize,
    scrolls: usize,
    cursors: [usize; 2],
    floats: usize,
    float_cursor: usize,
}

/// Neovim with windows.lua, two panes, and the daemon's part played here.
struct Multi {
    nvim: Nvim,
    gs: Grids,
    panes: [TestPane; 2],
    current: i64,
    anchor: i64,
    chan_req: u32,
    setup_req: u32,
    /// Rows kept at the bottom of each pane for messages: 'cmdheight'.
    msg_rows: u32,
    /// The 'cmdheight' last reported, or None.
    reported_cmdheight: Option<i64>,
    stats: MultiStats,
}

impl Multi {
    fn input(&mut self, keys: &str) -> Result<()> {
        self.nvim.request("nvim_input", 1);
        msgpack::write_str(self.nvim.writer(), keys);
        self.nvim.flush()?;
        Ok(())
    }

    /// The daemon's part when 'cmdheight' changes: every window fits its
    /// pane again, less the rows that are now kept for messages.
    fn refit(&mut self, cmdheight: i64) -> Result<()> {
        self.reported_cmdheight = Some(cmdheight);
        self.msg_rows = u32::try_from(cmdheight)?;
        for p in &self.panes {
            if p.win != 0 {
                windows::size(
                    &mut self.nvim,
                    p.win,
                    p.view.cols,
                    p.view.rows - self.msg_rows,
                );
            }
        }
        self.nvim.flush()?;
        Ok(())
    }

    /// Applies messages until Neovim goes quiet, checking every frame.
    fn settle(&mut self) -> Result<()> {
        loop {
            if !readable(&self.nvim, 200) {
                return Ok(());
            }
            if !self.nvim.read()? {
                return Err("NvimExited".into());
            }
            while let Some(frame) = self.nvim.next()? {
                match frame.message()? {
                    rpc::Message::Notification { method, mut params } => {
                        if method == b"redraw" {
                            if self.gs.apply_redraw(&mut params)? {
                                self.flushed()?;
                            }
                        } else if let Some(note) = windows::parse_note(method, &mut params)? {
                            match note {
                                Note::Current(w) => self.current = w,
                                Note::Cmdheight(rows) => self.refit(rows)?,
                                Note::Reopen(_)
                                | Note::Reopened { .. }
                                | Note::Split(_)
                                | Note::Float(_)
                                | Note::Entered(_)
                                | Note::Tabline(_)
                                | Note::Direction(_)
                                | Note::Layout(_)
                                | Note::Options(_)
                                | Note::Resized { .. }
                                | Note::Unsaved { .. }
                                | Note::Cwd(_)
                                | Note::Terminals(_)
                                | Note::Opened(_)
                                | Note::Done { .. }
                                | Note::Statuslines => {}
                            }
                        }
                    }
                    rpc::Message::Response {
                        id,
                        err,
                        mut result,
                    } => {
                        if let Some(e) = err {
                            eprintln!("neovim: {}", String::from_utf8_lossy(e));
                            return Err("NvimError".into());
                        }
                        if id == self.chan_req {
                            let v = &self.panes[0].view;
                            let (cols, rows) = (v.cols, v.rows - self.msg_rows);
                            let chan = windows::parse_channel(&mut result)?;
                            self.setup_req = windows::setup(&mut self.nvim, chan, cols, rows);
                            self.nvim.flush()?;
                        } else if id == self.setup_req {
                            let wins = windows::parse_setup(&mut result)?.wins;
                            self.panes[0].win = wins[0];
                            self.anchor = wins[1];
                        }
                    }
                    rpc::Message::Request { id, .. } => {
                        self.nvim.respond_error(id, "golden test")?
                    }
                }
            }
        }
    }

    /// The daemon's part: adopt new windows into the free pane, then draw
    /// and check both panes.
    fn flushed(&mut self) -> Result<()> {
        if self.anchor == 0 {
            return Ok(());
        }
        for w in self.gs.wins.values() {
            if w.kind == WinKind::Ordinary && w.win != self.anchor && self.panes[1].win == 0 {
                let v = &self.panes[1].view;
                windows::adopt(&mut self.nvim, w.win, v.cols, v.rows - self.msg_rows);
                self.nvim.flush()?;
                self.panes[1].win = w.win;
            }
        }
        for i in 0..self.panes.len() {
            self.check(i)?;
        }
        if let Some(id) = self.gs.msg_grid {
            self.gs.get_mut(id).unwrap().dirty.unset_all();
        }
        let floats: Vec<u32> = self
            .gs
            .wins
            .iter()
            .filter(|(_, w)| w.kind == WinKind::Float)
            .map(|(&id, _)| id)
            .collect();
        for id in floats {
            if let Some(f) = self.gs.get_mut(id) {
                f.dirty.unset_all();
                f.scrolls_len = 0;
            }
        }
        self.stats.frames += 1;
        Ok(())
    }

    fn check(&mut self, i: usize) -> Result<()> {
        let p = &mut self.panes[i];
        let grid = if p.win != 0 {
            self.gs.grid_of(p.win)
        } else {
            None
        };
        let focused = p.win != 0 && p.win == self.current;
        if let Some(id) = grid {
            self.stats.scrolls += self.gs.get(id).unwrap().pending_scrolls().len();
        }
        let mut out = Vec::new();
        p.view.frame(
            &mut self.gs,
            grid,
            Opts {
                focused,
                ..Opts::default()
            },
            &mut out,
        );
        p.t.feed(&out);

        let v = &p.view;
        let style = &self.gs.style;
        let msg_total = if focused { self.gs.msg_rows() } else { 0 };
        let msg_n = msg_total.min(v.rows);
        let msg_top = v.rows - msg_n;
        let mut grid_rows: u32 = 0;
        let mut placed: Vec<Placed> = Vec::new();
        if let Some(id) = grid {
            let g = self.gs.get(id).unwrap();
            grid_rows = g.height.min(msg_top);
            placed = self.gs.floats_over(id, focused, &[]);
            self.stats.floats += placed.len();
            let shown = overlay(&self.gs, g, &placed, grid_rows, v.cols);
            expect_rows(&p.t, &shown, style, 0, 0, grid_rows, v.cols)?;
        }
        if msg_n > 0 {
            expect_rows(
                &p.t,
                self.gs.get(self.gs.msg_grid.unwrap()).unwrap(),
                style,
                msg_total - msg_n,
                msg_top,
                msg_n,
                v.cols,
            )?;
            self.stats.msg_rows += msg_n as usize;
        } else if grid_rows < v.rows {
            // the message line of a pane without focus stays blank, unless its
            // window still fills the pane, as before it is fitted to a new 'cmdheight'
            expect_blank(&p.t, v.rows - 1, v.cols)?;
        }

        // the cursor shows on its grid only, or on the messages of the focused pane
        let mut want: Option<render::Cursor> = None;
        if let Some(id) = grid
            && self.gs.cursor_grid == id
        {
            let g = self.gs.get(id).unwrap();
            if g.cursor_row < grid_rows {
                want = Some(render::Cursor {
                    row: g.cursor_row,
                    col: g.cursor_col,
                });
            }
        }
        for f in &placed {
            if self.gs.cursor_grid == f.grid {
                let fg = self.gs.get(f.grid).unwrap();
                let row = f.row + fg.cursor_row as i32;
                let col = f.col + fg.cursor_col as i32;
                if row >= 0 && (row as u32) < grid_rows && col >= 0 && (col as u32) < v.cols {
                    want = Some(render::Cursor {
                        row: row as u32,
                        col: col as u32,
                    });
                    self.stats.float_cursor += 1;
                }
            }
        }
        if msg_n > 0 && Some(self.gs.cursor_grid) == self.gs.msg_grid {
            let mg = self.gs.get(self.gs.msg_grid.unwrap()).unwrap();
            want = Some(render::Cursor {
                row: msg_top + mg.cursor_row - (msg_total - msg_n),
                col: mg.cursor_col,
            });
        }
        if want.is_some() {
            self.stats.cursors[i] += 1;
        }
        expect_cursor(&p.t, style, want)
    }
}

/// Grid `g` with `placed` floats laid over it, cell by cell, as wide as
/// the floats reach within `cols`.
fn overlay(gs: &Grids, g: &Grid, placed: &[Placed], rows: u32, cols: u32) -> Grid {
    let mut width = g.width;
    for f in placed {
        let reach = (f.col + gs.get(f.grid).unwrap().width as i32).clamp(0, cols as i32);
        width = width.max(reach as u32);
    }
    let mut out = Grid::default();
    out.resize(width, g.height);
    let w = width as usize;
    for r in 0..g.height {
        out.cells[r as usize * w..][..g.width as usize].copy_from_slice(g.row(r));
    }
    for f in placed {
        let fg = gs.get(f.grid).unwrap();
        for fr in 0..fg.height {
            for fc in 0..fg.width {
                let r = f.row + fr as i32;
                let c = f.col + fc as i32;
                if r < 0 || r >= rows as i32 || c < 0 || c >= width as i32 {
                    continue;
                }
                let line = &mut out.cells[r as usize * w..][..w];
                let x = c as usize;
                // half a wide char under the float's edge shows as a blank
                if fc == 0 && x > 0 && line[x].len == 0 {
                    line[x - 1] = Cell::init(" ", line[x - 1].hl);
                }
                line[x] = fg.row(fr)[fc as usize];
                if fc == 0 && line[x].len == 0 {
                    line[x] = Cell::init(" ", line[x].hl);
                }
                if fc + 1 == fg.width && x + 1 < w && line[x + 1].len == 0 {
                    line[x + 1] = Cell::init(" ", line[x + 1].hl);
                }
            }
        }
    }
    out
}

fn expect_blank(t: &Vt, row: u32, cols: u32) -> Result<()> {
    for c in 0..cols {
        let pc = t.cell(c, row)?;
        // codepoint 0 or a space
        if pc.text.first().is_some_and(|&cp| cp != ' ') {
            return Err("NotBlank".into());
        }
    }
    Ok(())
}

const MULTI_STEPS: &[&str] = &[
    "ihello<Esc>",
    ":split<CR>",
    "othere<Esc>",
    ":echo 'one line'<CR>",
    ":echo \"a\\nb\\nc\"<CR>",
    "<CR>",
    "<C-w>w",
    ":call setline(1, map(range(1, 60), '\"row \" . v:val'))<CR>",
    "<C-d>",
    "<C-e><C-e>",
    "<C-w>w",
    ":set number<CR>",
    "G",
    ":ec",
    "<Esc>",
    // floats: over a window, over the screen, then the popup menu
    ":lua B=vim.api.nvim_create_buf(false,true) vim.api.nvim_buf_set_lines(B,0,-1,false,{'float one','x'})<CR>",
    ":lua F1=vim.api.nvim_open_win(B,false,{relative='win',row=1,col=3,width=12,height=2,border='single'})<CR>",
    ":lua F2=vim.api.nvim_open_win(B,false,{relative='editor',anchor='SE',row=6,col=23,width=9,height=1,zindex=60})<CR>",
    "<C-e>",
    "<C-y>",
    "oapple apricot<CR>ap<C-n>",
    "<C-n>",
    "<Esc>",
    ":lua vim.api.nvim_win_set_config(F1,{relative='win',row=3,col=0})<CR>",
    ":lua vim.api.nvim_set_current_win(F1)<CR>",
    "Afloat text<Esc>",
    ":lua vim.api.nvim_win_close(F1,true) vim.api.nvim_win_close(F2,true)<CR>",
];

#[test]
fn live_neovim_with_a_pane_per_window() {
    pane_per_window(1).unwrap();
}

#[test]
fn live_neovim_with_a_pane_per_window_and_no_row_reserved_for_messages() {
    pane_per_window(0).unwrap();
}

fn pane_per_window(cmdheight: u32) -> Result<()> {
    let set = format!("set cmdheight={cmdheight}");
    let Some(nvim) = spawn_nvim(&["nvim", "--clean", "--embed", "-n", "--cmd", &set]) else {
        return Ok(());
    };
    let mut m = Multi {
        msg_rows: cmdheight,
        nvim,
        gs: Grids::default(),
        panes: [
            TestPane {
                view: PaneView::init(30, 8),
                t: test_terminal(30, 8)?,
                win: 0,
            },
            TestPane {
                view: PaneView::init(24, 6),
                t: test_terminal(24, 6)?,
                win: 0,
            },
        ],
        current: 0,
        anchor: 0,
        chan_req: 0,
        setup_req: 0,
        reported_cmdheight: None,
        stats: MultiStats::default(),
    };
    m.nvim.ui_attach(
        30,
        8,
        UiOptions {
            ext_multigrid: true,
            ..UiOptions::default()
        },
    )?;
    m.chan_req = windows::ask_channel(&mut m.nvim);
    m.nvim.flush()?;
    m.settle()?;
    for keys in MULTI_STEPS {
        m.input(keys)?;
        if let Err(err) = m.settle() {
            eprintln!("golden: after step {keys}");
            return Err(err);
        }
    }
    assert!(m.panes[1].win != 0);
    assert_eq!(m.reported_cmdheight, Some(cmdheight as i64));
    assert!(m.stats.frames > MULTI_STEPS.len());
    // three message rows, so the scrolled-up message was drawn
    assert!(m.stats.msg_rows > 3);
    assert!(m.stats.scrolls > 0);
    assert!(m.stats.cursors[0] > 0 && m.stats.cursors[1] > 0);
    assert!(m.stats.floats > 0);
    assert!(m.stats.float_cursor > 0);
    // all closed
    for p in &m.panes {
        let grid = m.gs.grid_of(p.win).unwrap();
        assert_eq!(m.gs.floats_over(grid, true, &[]).len(), 0);
    }

    // a new 'cmdheight' is reported, and the windows fit their panes again
    for rows in [2u32, 0, 1] {
        m.input(&format!(":set cmdheight={rows}<CR>"))?;
        m.settle()?;
        assert_eq!(m.reported_cmdheight, Some(rows as i64));
        for p in &m.panes {
            let grid = m.gs.grid_of(p.win).unwrap();
            assert_eq!(p.view.rows - rows, m.gs.get(grid).unwrap().height);
        }
    }
    Ok(())
}

// Several windows in one pane, as Neovim lays out splits: each grid at its
// rect, its statusline included, and separators between windows side by
// side. What each must look like is written out cell by cell below.

/// Highlights of the split frames: text, a statusline, a separator.
const TEXT: u32 = 0;
const STL: u32 = 1;
const SEP: u32 = 2;

fn split_style() -> Style {
    let mut style = Style {
        default_fg: Some(0xdddddd),
        default_bg: Some(0x101010),
        ..Style::default()
    };
    style.attrs.extend_from_slice(&[
        Attr::default(),
        Attr {
            reverse: true,
            bold: true,
            ..Attr::default()
        },
        Attr {
            fg: Some(0x5f87af),
            ..Attr::default()
        },
    ]);
    style
}

/// Window grid `id`, `w` wide: `text` rows of `letter` lines, then a
/// statusline naming it.
fn add_window(gs: &mut Grids, id: u32, w: u32, text: u32, letter: char) {
    let mut g = Grid::default();
    g.resize(w, text + 1);
    for r in 0..text {
        let line = format!("{letter}{r}");
        for (c, ch) in line.chars().enumerate() {
            g.cells[(r * w) as usize + c] = Cell::init(ch.to_string(), TEXT);
        }
    }
    let stl = format!("[{letter}]");
    for c in 0..w as usize {
        let ch = stl.chars().nth(c).unwrap_or(' ');
        g.cells[(text * w) as usize + c] = Cell::init(ch.to_string(), STL);
    }
    gs.grids.insert(id, g);
}

/// The screen the frame must show: each row's text and, under it, a mark
/// per cell: `.` text, `S` statusline, `|` separator.
fn expect_written(t: &Vt, style: &Style, rows: &[(&str, &str)]) -> Result<()> {
    for (r, (text, marks)) in rows.iter().enumerate() {
        let text: Vec<char> = text.chars().collect();
        let marks: Vec<char> = marks.chars().collect();
        assert_eq!(text.len(), marks.len(), "row {r} of the expected screen");
        for (c, (&ch, &m)) in text.iter().zip(&marks).enumerate() {
            let hl = match m {
                'S' => STL,
                '|' => SEP,
                _ => TEXT,
            };
            let pc = t.cell(c as u32, r as u32)?;
            if let Err(err) = expect_cell(&pc, style, Cell::init(ch.to_string(), hl)) {
                eprintln!(
                    "golden: row {r} col {c}: want {ch:?}\nghostty:\n{}",
                    t.plain_string()?
                );
                return Err(err);
            }
        }
    }
    Ok(())
}

fn separator() -> Cell {
    Cell::init("│", SEP)
}

#[test]
fn two_windows_side_by_side_with_a_separator_between() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    add_window(&mut gs, 2, 6, 3, 'a');
    add_window(&mut gs, 3, 5, 3, 'b');
    gs.cursor_grid = 3;
    gs.get_mut(3).unwrap().cursor_row = 1;
    gs.get_mut(3).unwrap().cursor_col = 1;
    let wins = [
        Shown {
            grid: 2,
            row: 0,
            col: 0,
            cols: 6,
            rows: 4,
            statusline: None,
        },
        Shown {
            grid: 3,
            row: 0,
            col: 7,
            cols: 5,
            rows: 4,
            statusline: None,
        },
    ];
    let seps = [Separator {
        row: 0,
        col: 6,
        rows: 4,
    }];
    let mut view = PaneView::init(12, 5);
    let mut t = test_terminal(12, 5)?;
    let mut out = Vec::new();
    let laid = LaidOut {
        wins: &wins,
        separators: &seps,
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("a0    │b0   ", "......|....."),
            ("a1    │b1   ", "......|....."),
            ("a2    │b2   ", "......|....."),
            ("[a]   │[b]  ", "SSSSSS|SSSSS"),
        ],
    )?;
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 1, col: 8 }))?;

    // one window changes: only its rows, and the rest still shows
    gs.get_mut(2).unwrap().cells[0] = Cell::init("z", TEXT);
    gs.get_mut(2).unwrap().dirty.set(0);
    out.clear();
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    expect_written(&t, &gs.style, &[("z0    │b0   ", "......|.....")])?;
    // nothing changed: nothing drawn
    out.clear();
    assert!(!view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    Ok(())
}

#[test]
fn a_screen_float_too_wide_for_its_pane_is_moved_in_and_cut() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    add_window(&mut gs, 2, 12, 3, 'a');
    // a picker's line, 16 wide, at column 20 of a screen the pane starts
    // 10 columns into
    let mut f = Grid::default();
    f.resize(16, 1);
    for (c, ch) in "PICKER-012345678".chars().enumerate() {
        f.cells[c] = Cell::init(ch.to_string(), TEXT);
    }
    gs.grids.insert(5, f);
    gs.float_on_screen(5, 1005, 1, 20);
    let want = [
        ("a0          ", "............"),
        ("PICKER-01234", "............"),
        ("a2          ", "............"),
        ("[a]         ", "SSSSSSSSSSSS"),
    ];
    // in the current pane, and in another it is drawn in as its own
    for opts in [
        Opts {
            focused: true,
            origin: [0, 10],
            ..Opts::default()
        },
        Opts {
            origin: [0, 10],
            own: &[5],
            ..Opts::default()
        },
    ] {
        let mut view = PaneView::init(12, 5);
        let mut t = test_terminal(12, 5)?;
        let mut out = Vec::new();
        assert!(view.frame(&mut gs, Some(2), opts, &mut out));
        t.feed(&out);
        expect_written(&t, &gs.style, &want)?;
    }
    // and in no other
    let mut view = PaneView::init(12, 5);
    let mut t = test_terminal(12, 5)?;
    let mut out = Vec::new();
    assert!(view.frame(&mut gs, Some(2), Opts::default(), &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("a0          ", "............"),
            ("a1          ", "............"),
        ],
    )?;
    Ok(())
}

#[test]
fn three_windows_under_the_tabline_one_beside_two_stacked() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    add_window(&mut gs, 2, 5, 4, 'a');
    add_window(&mut gs, 3, 6, 1, 'b');
    add_window(&mut gs, 4, 6, 2, 'c');
    gs.cursor_grid = 4;
    gs.get_mut(4).unwrap().cursor_row = 1;
    let wins = [
        Shown {
            grid: 2,
            row: 0,
            col: 0,
            cols: 5,
            rows: 5,
            statusline: None,
        },
        Shown {
            grid: 3,
            row: 0,
            col: 6,
            cols: 6,
            rows: 2,
            statusline: None,
        },
        Shown {
            grid: 4,
            row: 2,
            col: 6,
            cols: 6,
            rows: 3,
            statusline: None,
        },
    ];
    let seps = [Separator {
        row: 0,
        col: 5,
        rows: 5,
    }];
    let tabline: Vec<Cell> = "1 tab"
        .chars()
        .map(|c| Cell::init(c.to_string(), STL))
        .collect();
    let mut view = PaneView::init(12, 7);
    let mut t = test_terminal(12, 7)?;
    let mut out = Vec::new();
    let opts = Opts {
        tabline: Some(&tabline),
        ..Opts::default()
    };
    let laid = LaidOut {
        wins: &wins,
        separators: &seps,
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, opts, &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("1 tab", "SSSSS"),
            ("a0   │b0    ", ".....|......"),
            ("a1   │[b]   ", ".....|SSSSSS"),
            ("a2   │c0    ", ".....|......"),
            ("a3   │c1    ", ".....|......"),
            ("[a]  │[c]   ", "SSSSS|SSSSSS"),
        ],
    )?;
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 4, col: 6 }))
}

#[test]
fn a_squeezed_window_shows_its_statusline_and_the_current_keeps_a_row() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    // Neovim keeps a row of text in each: the pane shows less
    add_window(&mut gs, 2, 8, 1, 'a');
    add_window(&mut gs, 3, 8, 1, 'b');
    add_window(&mut gs, 4, 8, 1, 'c');
    gs.cursor_grid = 3;
    let wins = [
        Shown {
            grid: 2,
            row: 0,
            col: 0,
            cols: 8,
            rows: 1,
            statusline: None,
        },
        Shown {
            grid: 3,
            row: 1,
            col: 0,
            cols: 8,
            rows: 2,
            statusline: None,
        },
        Shown {
            grid: 4,
            row: 3,
            col: 0,
            cols: 8,
            rows: 1,
            statusline: None,
        },
    ];
    let mut view = PaneView::init(8, 5);
    let mut t = test_terminal(8, 5)?;
    let mut out = Vec::new();
    let laid = LaidOut {
        wins: &wins,
        separators: &[],
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("[a]     ", "SSSSSSSS"),
            ("b0      ", "........"),
            ("[b]     ", "SSSSSSSS"),
            ("[c]     ", "SSSSSSSS"),
        ],
    )?;
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 1, col: 0 }))
}

#[test]
fn with_laststatus_0_the_window_above_shows_a_blank_statusline_drawn_for_it() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    // Neovim draws neither a statusline: each grid is text only
    add_window(&mut gs, 2, 6, 2, 'a');
    add_window(&mut gs, 3, 6, 2, 'b');
    for id in [2, 3] {
        gs.get_mut(id).unwrap().resize(6, 2);
    }
    gs.cursor_grid = 3;
    let wins = [
        Shown {
            grid: 2,
            row: 0,
            col: 0,
            cols: 6,
            rows: 3,
            statusline: Some(STL),
        },
        Shown {
            grid: 3,
            row: 3,
            col: 0,
            cols: 6,
            rows: 2,
            statusline: None,
        },
    ];
    let mut view = PaneView::init(6, 6);
    let mut t = test_terminal(6, 6)?;
    let mut out = Vec::new();
    let laid = LaidOut {
        wins: &wins,
        separators: &[],
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("a0    ", "......"),
            ("a1    ", "......"),
            ("      ", "SSSSSS"),
            ("b0    ", "......"),
            ("b1    ", "......"),
        ],
    )?;
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 3, col: 0 }))
}

#[test]
fn with_laststatus_0_the_window_above_shows_its_statusline_text_drawn_for_it() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    // Neovim draws neither a statusline: each grid is text only
    add_window(&mut gs, 2, 8, 2, 'a');
    add_window(&mut gs, 3, 8, 2, 'b');
    for id in [2, 3] {
        gs.get_mut(id).unwrap().resize(8, 2);
    }
    gs.cursor_grid = 3;
    // the statusline Neovim would draw the window above, `%f%1*%m%*`, not
    // current: StatusLineNC, then User1 combined with it, as Neovim evaluates
    // it, to the window's width but the fill; highlights by index
    let nc = Attr {
        reverse: true,
        ..Attr::default()
    };
    let user1 = Attr {
        fg: Some(0xff0000),
        ..nc
    };
    let line = |text: &str, user1_from: usize| -> Vec<Cell> {
        text.chars()
            .enumerate()
            .map(|(i, c)| {
                let a = if i < user1_from { 0 } else { 1 };
                Cell::init(c.to_string(), a)
            })
            .collect()
    };
    gs.set_statuslines(
        vec![nc, user1],
        [(2, line("a.txt", 5))].into_iter().collect(),
    );
    let wins = [
        Shown {
            grid: 2,
            row: 0,
            col: 0,
            cols: 8,
            rows: 3,
            statusline: Some(STL),
        },
        Shown {
            grid: 3,
            row: 3,
            col: 0,
            cols: 8,
            rows: 2,
            statusline: None,
        },
    ];
    let mut view = PaneView::init(8, 6);
    let mut t = test_terminal(8, 6)?;
    let mut out = Vec::new();
    let laid = LaidOut {
        wins: &wins,
        separators: &[],
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    // `n` StatusLineNC, `u` User1 over it; the rest the fill, in the
    // statusline's own highlight
    let expect = |t: &Vt, gs: &Grids, row: (&str, &str)| -> Result<()> {
        let (text, marks) = row;
        for (c, (ch, m)) in text.chars().zip(marks.chars()).enumerate() {
            let hl = match m {
                'n' => STATUSLINE_BASE,
                'u' => STATUSLINE_BASE + 1,
                _ => STL,
            };
            let pc = t.cell(c as u32, 2)?;
            if let Err(err) = expect_cell(&pc, &gs.style, Cell::init(ch.to_string(), hl)) {
                eprintln!(
                    "golden: col {c}: want {ch:?}\nghostty:\n{}",
                    t.plain_string()?
                );
                return Err(err);
            }
        }
        Ok(())
    };
    expect_written(
        &t,
        &gs.style,
        &[("a0      ", "........"), ("a1      ", "........")],
    )?;
    expect(&t, &gs, ("a.txt   ", "nnnnnSSS"))?;
    for (r, text) in [(3, "b0"), (4, "b1")] {
        for (c, ch) in text.chars().enumerate() {
            let pc = t.cell(c as u32, r)?;
            expect_cell(&pc, &gs.style, Cell::init(ch.to_string(), TEXT))?;
        }
    }
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 3, col: 0 }))?;

    // the buffer is modified: Neovim evaluates `a.txt [+]`, cut to the
    // window's width; only that row is drawn again
    assert!(gs.set_statuslines(
        vec![nc, user1],
        [(2, line("a.txt [+]", 5))].into_iter().collect(),
    ));
    out.clear();
    assert!(view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    t.feed(&out);
    expect(&t, &gs, ("a.txt [+", "nnnnnuuu"))?;
    // the same again: nothing drawn
    assert!(!gs.set_statuslines(
        vec![nc, user1],
        [(2, line("a.txt [+]", 5))].into_iter().collect(),
    ));
    out.clear();
    assert!(!view.frame_laid_out(&mut gs, laid, Opts::default(), &mut out));
    Ok(())
}

/// The tabline `text`, in statusline cells.
fn tabline_of(text: &str) -> Vec<Cell> {
    text.chars()
        .map(|c| Cell::init(c.to_string(), STL))
        .collect()
}

// A pane showing tab page 2's layout (ADR 0012): `gt` swaps the whole pane
// from one tab page's windows to the other's, nothing of the first left.
#[test]
fn a_pane_swapped_to_tab_page_two_shows_its_layout_whole() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    // tab page 1: a beside b; tab page 2: c above d
    add_window(&mut gs, 2, 6, 3, 'a');
    add_window(&mut gs, 3, 5, 3, 'b');
    add_window(&mut gs, 4, 12, 1, 'c');
    add_window(&mut gs, 5, 12, 1, 'd');
    gs.cursor_grid = 2;
    let shown = |grid, row, col, cols, rows| Shown {
        grid,
        row,
        col,
        cols,
        rows,
        statusline: None,
    };
    let first = [shown(2, 0, 0, 6, 4), shown(3, 0, 7, 5, 4)];
    let seps = [Separator {
        row: 0,
        col: 6,
        rows: 4,
    }];
    let tabline = tabline_of("1:ab  2:cd ");
    let mut view = PaneView::init(12, 6);
    let mut t = test_terminal(12, 6)?;
    let mut out = Vec::new();
    let opts = Opts {
        tabline: Some(&tabline),
        ..Opts::default()
    };
    let laid = LaidOut {
        wins: &first,
        separators: &seps,
        separator: separator(),
    };
    assert!(view.frame_laid_out(&mut gs, laid, opts, &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("1:ab  2:cd ", "SSSSSSSSSSS"),
            ("a0    │b0   ", "......|....."),
        ],
    )?;

    // `gt`: tab page 2's windows, current d, under the tabline it redrew
    gs.cursor_grid = 5;
    let second = [shown(4, 0, 0, 12, 2), shown(5, 2, 0, 12, 2)];
    let tabline = tabline_of("1:ab  2:cd ");
    let opts = Opts {
        tabline: Some(&tabline),
        tabline_dirty: true,
        ..Opts::default()
    };
    let laid = LaidOut {
        wins: &second,
        separators: &[],
        separator: separator(),
    };
    out.clear();
    assert!(view.frame_laid_out(&mut gs, laid, opts, &mut out));
    t.feed(&out);
    expect_written(
        &t,
        &gs.style,
        &[
            ("1:ab  2:cd ", "SSSSSSSSSSS"),
            ("c0          ", "............"),
            ("[c]         ", "SSSSSSSSSSSS"),
            ("d0          ", "............"),
            ("[d]         ", "SSSSSSSSSSSS"),
        ],
    )?;
    expect_cursor(&t, &gs.style, Some(render::Cursor { row: 3, col: 0 }))
}

// The tabline over layout areas (ADR 0008): fitted to the current pane's
// area, whose tabline panes show it end to end; a pane of another area
// shows the same row from its start, cut at its width or blank past it.

/// Draws window grid `grid`, under the tabline from column `col` of grid 1,
/// into a pane of `cols` x 4, and checks what it shows.
fn pane_under_the_tabline(
    gs: &mut Grids,
    grid: u32,
    col: u32,
    cols: u32,
    rows: &[(&str, &str)],
) -> Result<()> {
    let tabline = tabline_slice(gs.get(1).unwrap(), col, cols);
    let mut view = PaneView::init(cols, 4);
    let mut t = test_terminal(cols as u16, 4)?;
    let mut out = Vec::new();
    let opts = Opts {
        tabline: Some(&tabline),
        tabline_col: col,
        ..Opts::default()
    };
    assert!(view.frame(gs, Some(grid), opts, &mut out));
    t.feed(&out);
    expect_written(&t, &gs.style, rows)
}

#[test]
fn a_fitted_tabline_in_the_current_area_and_a_cut_one_in_another() -> Result<()> {
    let mut gs = Grids::default();
    gs.style = split_style();
    // the screen is the current area, 20 wide: its tabline is fitted to it
    let mut screen = Grid::default();
    screen.resize(20, 4);
    for (c, ch) in "1:one 2:two 3:three ".chars().enumerate() {
        screen.cells[c] = Cell::init(ch.to_string(), STL);
    }
    gs.grids.insert(1, screen);
    add_window(&mut gs, 2, 12, 2, 'a');
    add_window(&mut gs, 3, 8, 2, 'b');
    add_window(&mut gs, 4, 8, 2, 'c');
    add_window(&mut gs, 5, 26, 2, 'd');

    // the current area's tabline panes, 12 and 8 wide: every tab, once. The
    // columns are those `Geometry::tabline_start` gives each area's panes, as
    // the daemon's `each_areas_tabline_panes_slice_the_screens_tabline_from_its_start`
    // checks for this same screen
    pane_under_the_tabline(
        &mut gs,
        2,
        0,
        12,
        &[
            ("1:one 2:two ", "SSSSSSSSSSSS"),
            ("a0          ", "............"),
        ],
    )?;
    pane_under_the_tabline(
        &mut gs,
        3,
        12,
        8,
        &[("3:three ", "SSSSSSSS"), ("b0      ", "........")],
    )?;
    // another area's: from its start, cut at a narrow one's width...
    pane_under_the_tabline(
        &mut gs,
        4,
        0,
        8,
        &[("1:one 2:", "SSSSSSSS"), ("c0      ", "........")],
    )?;
    // ...and blank past the screen in a wide one
    pane_under_the_tabline(
        &mut gs,
        5,
        0,
        26,
        &[
            ("1:one 2:two 3:three       ", "SSSSSSSSSSSSSSSSSSSS......"),
            ("d0                        ", ".........................."),
        ],
    )
}
