//! Windows as panes, Neovim's side: `windows.lua` keeps every window except
//! a hidden anchor external and reports the current window. These calls
//! write requests; the caller flushes.

use super::msgpack::{self, Decoder};
use super::nvim::Nvim;
use super::rpc;

const LUA: &str = include_str!("windows.lua");

/// Installs the Lua on our channel `chan` and makes the current window
/// external at `cols` x `rows`. Its response is `[first window, anchor]`.
#[doc(hidden)]
pub fn setup(nvim: &mut Nvim, chan: i64, cols: u32, rows: u32) -> u32 {
    let id = nvim.exec_lua(LUA, 3);
    msgpack::write_int(nvim.writer(), chan);
    msgpack::write_int(nvim.writer(), cols);
    msgpack::write_int(nvim.writer(), rows);
    id
}

/// Makes ordinary window `win` external at `cols` x `rows`; anchors stay.
#[doc(hidden)]
pub fn adopt(nvim: &mut Nvim, win: i64, cols: u32, rows: u32) {
    nvim.exec_lua("neorex.adopt(...)", 3);
    msgpack::write_int(nvim.writer(), win);
    msgpack::write_int(nvim.writer(), cols.max(1));
    msgpack::write_int(nvim.writer(), rows.max(1));
}

/// Sizes external window `win` to fill `cols` x `rows`, its statusline included.
#[doc(hidden)]
pub fn size(nvim: &mut Nvim, win: i64, cols: u32, rows: u32) {
    nvim.exec_lua("neorex.size(...)", 3);
    msgpack::write_int(nvim.writer(), win);
    msgpack::write_int(nvim.writer(), cols.max(1));
    msgpack::write_int(nvim.writer(), rows.max(1));
}

/// Fits every non-empty `'tabline'` to the tabline width `cols`, the tabline
/// panes' columns, while that's narrower than Neovim's screen; None while it
/// isn't.
pub(crate) fn set_tabline_width(nvim: &mut Nvim, cols: Option<u32>) {
    nvim.exec_lua("neorex.set_tabline_width(...)", 1);
    match cols {
        Some(c) => msgpack::write_int(nvim.writer(), c),
        None => msgpack::write_nil(nvim.writer()),
    }
}

/// Fits the floats hanging from editor floats `tops` to `w` x `h` at `row`,
/// `col`; `cur` is where they are now: row, col, width, height.
/// Returns the request's id.
pub(crate) fn place_group(
    nvim: &mut Nvim,
    tops: &[i64],
    row: i32,
    col: i32,
    w: u32,
    h: u32,
    cur: [i64; 4],
) -> u32 {
    let id = nvim.exec_lua("neorex.place_group(...)", 6);
    let wr = nvim.writer();
    msgpack::write_array_header(wr, tops.len() as u32);
    for &t in tops {
        msgpack::write_int(wr, t);
    }
    msgpack::write_int(wr, row);
    msgpack::write_int(wr, col);
    msgpack::write_int(wr, w.max(1));
    msgpack::write_int(wr, h.max(1));
    msgpack::write_array_header(wr, 4);
    for v in cur {
        msgpack::write_int(wr, v);
    }
    id
}

/// Closes `win`, keeping unsaved changes in its buffer.
pub(crate) fn close(nvim: &mut Nvim, win: i64) {
    nvim.exec_lua("neorex.close(...)", 1);
    msgpack::write_int(nvim.writer(), win);
}

/// Returns the request's id.
pub(crate) fn set_current(nvim: &mut Nvim, win: i64) -> u32 {
    let id = nvim.request("nvim_set_current_win", 1);
    msgpack::write_int(nvim.writer(), win);
    id
}

/// Asks for our channel id: the response to the returned id starts with it.
#[doc(hidden)]
pub fn ask_channel(nvim: &mut Nvim) -> u32 {
    nvim.request("nvim_get_api_info", 0)
}

/// What the Lua tells the daemon.
// A layout is held inline; see `Layout`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq)]
#[doc(hidden)]
pub enum Note {
    /// The current window is now this one.
    Current(i64),
    /// This window is closing and reopening; keep its pane for the new one.
    Reopen(i64),
    /// The window that reopened `old`, which gets its pane.
    Reopened { old: i64, new: i64 },
    /// A new window, and where Neovim put it.
    Split(Split),
    /// The current window is now this float.
    Float(i64),
    /// This float was just entered; sent before the redraw that shows it.
    Entered(i64),
    /// The tabline's rows at the top of the screen.
    Tabline(i64),
    /// Neovim's 'cmdheight' in the current tab page: the rows at the bottom of
    /// the screen it keeps for messages and the command line. Never negative.
    Cmdheight(i64),
    /// `<C-w>h/j/k/l` was typed: Rex knows where the panes sit, not Neovim.
    Direction(Move),
    /// Where the windows of a tab page made after setup sit in it.
    Layout(Layout),
}

/// The layout of a tab page's windows: nested rows, columns and windows,
/// each with the size Neovim gave it, in cells. Empty if the tab page has
/// none the daemon can place, or has closed.
///
/// A note holds it inline, about 1.5 KB: `parse_note` has no allocator, a
/// note is used and dropped at once, and the daemon keeps one per tab page.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct Layout {
    pub(crate) tab: i64,
    pub(crate) len: usize,
    /// Parents before their kids.
    pub(crate) nodes: [Node; Layout::MAX_NODES],
}

impl std::fmt::Debug for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layout")
            .field("tab", &self.tab)
            .field("len", &self.len)
            .field("nodes", &self.items())
            .finish()
    }
}

/// Two layouts are equal if they have the same tab page and nodes in use.
impl PartialEq for Layout {
    fn eq(&self, other: &Layout) -> bool {
        self.tab == other.tab && self.items() == other.items()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Node {
    pub(crate) kind: Kind,
    /// A window's handle.
    pub(crate) win: i64,
    pub(crate) w: u32,
    pub(crate) h: u32,
    /// The index after this node and everything in it.
    pub(crate) end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Kind {
    #[default]
    Window,
    Row,
    Column,
}

impl Layout {
    /// The most nodes there are room for. The same as `MAX_LAYOUT_NODES` in
    /// windows.lua, which sends none for a bigger layout.
    pub(crate) const MAX_NODES: usize = 63;

    /// An empty layout of tab page `tab`.
    pub(crate) fn new(tab: i64) -> Layout {
        Layout {
            tab,
            len: 0,
            nodes: [Node::default(); Layout::MAX_NODES],
        }
    }

    /// The nodes in use.
    pub(crate) fn items(&self) -> &[Node] {
        &self.nodes[0..self.len]
    }

    /// The indices of the kids of the node at `at`, in order.
    pub(crate) fn kids(&self, at: usize) -> Kids<'_> {
        Kids {
            layout: self,
            next_at: at + 1,
            end: self.nodes[at].end,
        }
    }

    /// Reads one node and what's in it, from `[kind, win or kids, w, h]`.
    fn read(&mut self, d: &mut Decoder<'_>) -> rpc::Result<()> {
        if self.len == Layout::MAX_NODES {
            return Err(rpc::Error::BadMessage);
        }
        let at = self.len;
        self.len += 1;
        let kind = match rpc::expect_int(d)? {
            0 => Kind::Window,
            1 => Kind::Row,
            2 => Kind::Column,
            _ => return Err(rpc::Error::BadMessage),
        };
        let win_or_kids = rpc::expect_int(d)?;
        let w = u32::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)?;
        let h = u32::try_from(rpc::expect_int(d)?).map_err(|_| rpc::Error::BadMessage)?;
        self.nodes[at] = Node {
            kind,
            w,
            h,
            ..Node::default()
        };
        if kind == Kind::Window {
            self.nodes[at].win = win_or_kids;
        } else {
            let kid_count = u32::try_from(win_or_kids).map_err(|_| rpc::Error::BadMessage)?;
            // a row or column of one is no row
            if kid_count < 2 {
                return Err(rpc::Error::BadMessage);
            }
            for _ in 0..kid_count {
                self.read(d)?;
            }
        }
        self.nodes[at].end = self.len;
        Ok(())
    }
}

/// The indices of a node's kids; see `Layout::kids`.
pub(crate) struct Kids<'a> {
    layout: &'a Layout,
    next_at: usize,
    end: usize,
}

impl Iterator for Kids<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        if self.next_at >= self.end {
            return None;
        }
        let at = self.next_at;
        self.next_at = self.layout.nodes[at].end;
        Some(at)
    }
}

/// Which way a window command goes: `h`, `j`, `k` or `l`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Down,
    Up,
    Right,
}

/// A directional window command, `count` times as typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub struct Move {
    pub(crate) dir: Direction,
    pub(crate) count: u32,
}

/// Where a new window sits in Neovim's layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[doc(hidden)]
pub struct Split {
    pub(crate) win: i64,
    /// Side by side (`:vsplit`), else one above the other.
    pub(crate) side_by_side: bool,
    /// Left of or above the window beside it.
    pub(crate) before: bool,
    /// The window it was split from, or 0.
    pub(crate) from: i64,
    /// The tab page made after setup it belongs to, which Neovim shows
    /// over the first one's windows, else 0.
    pub(crate) tab: i64,
}

/// A notification from the Lua, or None for any other.
#[doc(hidden)]
pub fn parse_note(method: &[u8], params: &mut Decoder<'_>) -> rpc::Result<Option<Note>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Tag {
        Current,
        Reopen,
        Reopened,
        Split,
        Float,
        Entered,
        Tabline,
        Cmdheight,
        Layout,
    }
    if method == b"neorex_direction" {
        return parse_direction(params).map(Some);
    }
    let tag = match method {
        b"neorex_win" => Tag::Current,
        b"neorex_reopen" => Tag::Reopen,
        b"neorex_reopened" => Tag::Reopened,
        b"neorex_split" => Tag::Split,
        b"neorex_float" => Tag::Float,
        b"neorex_entered" => Tag::Entered,
        b"neorex_tabline" => Tag::Tabline,
        b"neorex_cmdheight" => Tag::Cmdheight,
        b"neorex_layout" => Tag::Layout,
        _ => return Ok(None),
    };
    let want: u32 = match tag {
        Tag::Split => 5,
        Tag::Reopened | Tag::Layout => 2,
        _ => 1,
    };
    if rpc::expect_array(params)? < want {
        return Err(rpc::Error::BadMessage);
    }
    let win = rpc::expect_handle(params)?;
    Ok(Some(match tag {
        Tag::Current => Note::Current(win),
        Tag::Reopen => Note::Reopen(win),
        Tag::Reopened => Note::Reopened {
            old: win,
            new: rpc::expect_handle(params)?,
        },
        Tag::Float => Note::Float(win),
        Tag::Entered => Note::Entered(win),
        Tag::Tabline => Note::Tabline(win),
        Tag::Cmdheight => {
            if win < 0 {
                return Err(rpc::Error::BadMessage);
            }
            Note::Cmdheight(win)
        }
        Tag::Layout => {
            let mut layout = Layout::new(win);
            // the nodes are flattened into one array, four numbers each
            let count = rpc::expect_array(params)?;
            if count > 0 {
                layout.read(params)?;
            }
            if count as usize != layout.len * 4 {
                return Err(rpc::Error::BadMessage);
            }
            Note::Layout(layout)
        }
        Tag::Split => Note::Split(Split {
            win,
            side_by_side: rpc::expect_bool(params)?,
            before: rpc::expect_bool(params)?,
            from: rpc::expect_handle(params)?,
            tab: rpc::expect_handle(params)?,
        }),
    }))
}

/// `[key, count]`: the `wincmd` key, one of `hjkl`, and how many times.
fn parse_direction(params: &mut Decoder<'_>) -> rpc::Result<Note> {
    if rpc::expect_array(params)? < 2 {
        return Err(rpc::Error::BadMessage);
    }
    let key = rpc::expect_str(params)?;
    let count = rpc::expect_int(params)?;
    if key.len() != 1 || count < 1 {
        return Err(rpc::Error::BadMessage);
    }
    let dir = match key[0] {
        b'h' => Direction::Left,
        b'j' => Direction::Down,
        b'k' => Direction::Up,
        b'l' => Direction::Right,
        _ => return Err(rpc::Error::BadMessage),
    };
    Ok(Note::Direction(Move {
        dir,
        count: count.min(u32::MAX as i64) as u32,
    }))
}

/// `[first, anchor]` from `setup`'s response.
#[doc(hidden)]
pub fn parse_setup(result: &mut Decoder<'_>) -> rpc::Result<[i64; 2]> {
    if rpc::expect_array(result)? < 2 {
        return Err(rpc::Error::BadMessage);
    }
    Ok([rpc::expect_handle(result)?, rpc::expect_handle(result)?])
}

/// Our channel id from `ask_channel`'s response.
#[doc(hidden)]
pub fn parse_channel(result: &mut Decoder<'_>) -> rpc::Result<i64> {
    if rpc::expect_array(result)? < 1 {
        return Err(rpc::Error::BadMessage);
    }
    rpc::expect_int(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::nvim::{UiOptions, spawn_for_test_with};
    use crate::screen::grid::Grid;
    use crate::screen::grids::{Grids, WinKind};

    /// Made-up reports the tests wait for: a real window handle is never this big.
    const PING_BASE: i64 = 4000;
    const FIGHT_AT_1S: i64 = 6000;
    const FIGHT_AT_END: i64 = 7000;

    const CLEAN: [&str; 4] = ["nvim", "--clean", "--embed", "-n"];

    /// A Neovim running windows.lua, and what it has said.
    struct Harness {
        nvim: Nvim,
        grids: Grids,
        notes: Vec<Note>,
        setup_id: u32,
        chan_id: u32,
        wins: [i64; 2],
        /// The windows a check waits for.
        want: i64,
        gone: i64,
        /// Made-up reports so far, to know Neovim got that far.
        pings: i64,

        /// A tab page and its windows, for checks that wait on them.
        tab: i64,
        watch: [i64; 3],

        /// Grid 1's top row as of each frame (flush) received while it's
        /// `Some`, consecutive repeats dropped.
        tabline_log: Option<Vec<String>>,
    }

    impl Harness {
        /// Spawns Neovim with `argv`, attaches the UI and asks for our
        /// channel. None (the test skips) without `nvim`.
        fn spawn(argv: &[&str], cols: u32, rows: u32) -> Option<Harness> {
            let nvim = spawn_for_test_with(argv)?;
            let mut h = Harness {
                nvim,
                grids: Grids::default(),
                notes: Vec::new(),
                setup_id: 0,
                chan_id: 0,
                wins: [0, 0],
                want: 0,
                gone: 0,
                pings: 0,
                tab: 0,
                watch: [0; 3],
                tabline_log: None,
            };
            h.nvim
                .ui_attach(
                    cols,
                    rows,
                    UiOptions {
                        ext_multigrid: true,
                        ..Default::default()
                    },
                )
                .unwrap();
            h.chan_id = ask_channel(&mut h.nvim);
            h.nvim.flush().unwrap();
            Some(h)
        }

        /// Spawns a clean Neovim and boots it (`boot`). None (the test
        /// skips) without `nvim`.
        fn start() -> Option<Harness> {
            Harness::start_sized(40, 10)
        }

        /// `start`, with a screen of `cols` x `rows`.
        fn start_sized(cols: u32, rows: u32) -> Option<Harness> {
            let mut h = Harness::spawn(&CLEAN, cols, rows)?;
            h.boot();
            Some(h)
        }

        /// Waits until the first window is external, windows.lua having started.
        fn boot(&mut self) {
            assert!(self.until(|x| {
                x.wins[0] != 0
                    && x.last_current() == x.wins[0]
                    && x.kind(x.wins[0]) == Some(WinKind::External)
            }));
        }

        /// Reads until `done`, or false if Neovim exits.
        fn until(&mut self, done: impl Fn(&Harness) -> bool) -> bool {
            for _ in 0..200 {
                if done(self) {
                    return true;
                }
                let mut fds = [libc::pollfd {
                    fd: self.nvim.read_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                }];
                // SAFETY: `fds` is one valid pollfd.
                if unsafe { libc::poll(fds.as_mut_ptr(), 1, 3000) } <= 0 {
                    panic!("TestTimedOut");
                }
                if !self.nvim.read().unwrap() {
                    return false;
                }
                while let Some(frame) = self.nvim.next().unwrap() {
                    match frame.message().unwrap() {
                        rpc::Message::Notification { method, mut params } => {
                            if method == b"redraw" {
                                if self.grids.apply_redraw(&mut params).unwrap() {
                                    self.log_tabline();
                                }
                            } else if let Some(note) = parse_note(method, &mut params).unwrap() {
                                self.notes.push(note);
                            }
                        }
                        rpc::Message::Response {
                            id,
                            err,
                            mut result,
                        } => {
                            if let Some(e) = err {
                                eprintln!("neovim: {}", String::from_utf8_lossy(e));
                                panic!("NvimError");
                            }
                            if id == self.chan_id {
                                let chan = parse_channel(&mut result).unwrap();
                                self.setup_id = setup(&mut self.nvim, chan, 40, 9);
                                self.nvim.flush().unwrap();
                            } else if id == self.setup_id {
                                self.wins = parse_setup(&mut result).unwrap();
                            }
                        }
                        rpc::Message::Request { id, .. } => {
                            self.nvim.respond_error(id, "test").unwrap();
                        }
                    }
                }
            }
            panic!("TestTimedOut");
        }

        fn input(&mut self, keys: &str) {
            self.nvim.request("nvim_input", 1);
            msgpack::write_str(self.nvim.writer(), keys);
            self.nvim.flush().unwrap();
        }

        /// Splits the current window and makes the split external at 20 x 9,
        /// as the daemon does. Returns it.
        fn adopt_split(&mut self) -> i64 {
            self.input(":split<CR>");
            assert!(self.until(|x| {
                let cur = x.last_current();
                cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
            }));
            let split = self.last_current();
            adopt(&mut self.nvim, split, 20, 9);
            self.nvim.flush().unwrap();
            assert!(self.until_external(split));
            split
        }

        fn until_external(&mut self, win: i64) -> bool {
            self.want = win;
            self.until(|x| x.kind(x.want) == Some(WinKind::External))
        }

        /// Runs Lua `code` in Neovim, as a plugin would, and waits until it has
        /// run. An error in it fails the test.
        fn sync(&mut self, code: &str) {
            self.pings += 1;
            let ping = PING_BASE + self.pings;
            self.lua(&format!("{code}\nvim.rpcnotify(0, 'neorex_win', {ping})"));
            self.want = ping;
            assert!(self.until(Harness::is_current));
        }

        /// Waits for `win`, which a plugin moved back, to be external again.
        fn expect_external(&mut self, win: i64) {
            self.sync(&format!(
                "assert(vim.wait(2500, function() return vim.api.nvim_win_get_config({win}).external end), 'not external')"
            ));
            assert!(self.until_external(win));
        }

        /// Types something, and waits until Neovim has taken it.
        fn typed(&mut self) {
            self.pings += 1;
            self.input(&format!(
                ":lua vim.rpcnotify(0, 'neorex_win', {})<CR>",
                PING_BASE + self.pings
            ));
            self.want = PING_BASE + self.pings;
            assert!(self.until(Harness::is_current));
        }

        /// Runs `code` in Neovim.
        fn lua(&mut self, code: &str) {
            self.nvim.exec_lua(code, 0);
            self.nvim.flush().unwrap();
        }

        /// Runs the Ex command `cmd` through `nvim_command`.
        fn command(&mut self, cmd: &str) {
            self.nvim.request("nvim_command", 1);
            msgpack::write_str(self.nvim.writer(), cmd);
            self.nvim.flush().unwrap();
        }

        fn last_current(&self) -> i64 {
            for n in self.notes.iter().rev() {
                if let Note::Current(w) = n {
                    return *w;
                }
            }
            0
        }

        /// The tabline rows last reported, or -1.
        fn last_tabline(&self) -> i64 {
            for n in self.notes.iter().rev() {
                if let Note::Tabline(t) = n {
                    return *t;
                }
            }
            -1
        }

        /// The layout last reported for tab page `tab`.
        fn last_layout(&self, tab: i64) -> Option<&Layout> {
            for n in self.notes.iter().rev() {
                if let Note::Layout(l) = n
                    && l.tab == tab
                {
                    return Some(l);
                }
            }
            None
        }

        /// The 'cmdheight' last reported, if any.
        fn last_cmdheight(&self) -> Option<i64> {
            for n in self.notes.iter().rev() {
                if let Note::Cmdheight(c) = n {
                    return Some(*c);
                }
            }
            None
        }

        fn watched_external(&self) -> bool {
            self.watch
                .iter()
                .all(|&win| self.kind(win) == Some(WinKind::External))
        }

        fn split_note(&self, win: i64) -> Option<Split> {
            self.notes.iter().find_map(|n| match n {
                Note::Split(s) if s.win == win => Some(*s),
                _ => None,
            })
        }

        fn is_current(&self) -> bool {
            self.last_current() == self.want
        }

        fn kind(&self, win: i64) -> Option<WinKind> {
            let grid = self.grids.grid_of(win)?;
            Some(self.grids.win_on(grid).unwrap().kind)
        }

        fn grid_of_win(&self, win: i64) -> Option<&Grid> {
            self.grids.get(self.grids.grid_of(win)?)
        }

        fn grid_height(&self, win: i64) -> u32 {
            self.grid_of_win(win).map_or(0, |g| g.height)
        }

        /// Grid row `r` of window `win` starts with `want`.
        fn row_starts(&self, win: i64, r: u32, want: &str) -> bool {
            let Some(g) = self.grid_of_win(win) else {
                return false;
            };
            if r >= g.height {
                return false;
            }
            g.row_text(r).starts_with(want)
        }

        /// Waits until Neovim runs a command that says `neorex_win` 4242, after
        /// everything typed before it.
        fn barrier(&mut self) {
            self.input(":call rpcnotify(0, 'neorex_win', 4242)<CR>");
            self.want = 4242;
            assert!(self.until(Harness::is_current));
        }

        /// `want` is what the notes since the last clear say, in order, then the barrier's.
        fn expect_notes(&self, want: &[Note]) {
            assert_eq!(want.len() + 1, self.notes.len());
            for (w, got) in want.iter().zip(&self.notes[0..want.len()]) {
                assert_eq!(w, got);
            }
            assert_eq!(Note::Current(4242), self.notes[want.len()]);
        }

        /// Splits the current window, which is already set up, and adopts the
        /// new one as a 20 x 9 pane. Returns it.
        fn split_and_adopt(&mut self) -> i64 {
            self.notes.clear();
            self.input(":vsplit<CR>");
            assert!(self.until(|x| {
                let cur = x.last_current();
                cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
            }));
            let split = self.last_current();
            adopt(&mut self.nvim, split, 20, 9);
            self.nvim.flush().unwrap();
            self.want = split;
            assert!(self.until(|x| {
                x.kind(x.want) == Some(WinKind::External) && x.grid_height(x.want) == 9
            }));
            split
        }

        /// Waits until both panes `a` and `b` show `want` in their last row.
        fn both_show(&mut self, a: i64, b: i64, want: &str) {
            assert!(self.until(|x| x.row_starts(a, 8, want) && x.row_starts(b, 8, want)));
        }
    }

    #[test]
    fn windows_become_external_the_anchor_stays_hidden() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let [first, anchor] = h.wins;
        assert_eq!(Some(WinKind::Ordinary), h.kind(anchor));

        // a split is ordinary until the daemon adopts it
        h.notes.clear();
        h.input(":split<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let split = h.last_current();
        assert!(split != first && split != anchor);
        assert_eq!(
            Split {
                win: split,
                side_by_side: false,
                before: true,
                from: first,
                tab: 0
            },
            h.split_note(split).unwrap()
        );
        adopt(&mut h.nvim, split, 20, 9);
        // the anchor never becomes external
        adopt(&mut h.nvim, anchor, 20, 9);
        h.nvim.flush().unwrap();
        h.want = split;
        assert!(h.until(|x| x.kind(x.want) == Some(WinKind::External)));

        // <C-w>w wraps through the anchor to the first window, <C-w>W back
        h.notes.clear();
        h.input("<C-w>w");
        h.want = first;
        assert!(h.until(Harness::is_current));
        h.notes.clear();
        h.input("<C-w>W");
        h.want = split;
        assert!(h.until(Harness::is_current));
        assert_eq!(Some(WinKind::Ordinary), h.kind(anchor));

        // :vsplit with splitright: beside, after
        h.notes.clear();
        h.input(":set splitright | vsplit<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let vsplit = h.last_current();
        assert_eq!(
            Split {
                win: vsplit,
                side_by_side: true,
                before: false,
                from: split,
                tab: 0
            },
            h.split_note(vsplit).unwrap()
        );
        close(&mut h.nvim, vsplit);
        h.nvim.flush().unwrap();
        h.gone = vsplit;
        assert!(h.until(|x| x.kind(x.gone).is_none()));
        // back to `split`, wherever Neovim went
        h.notes.clear();
        set_current(&mut h.nvim, first);
        set_current(&mut h.nvim, split);
        h.nvim.flush().unwrap();
        h.want = split;
        assert!(h.until(Harness::is_current));

        // its pane went away
        h.notes.clear();
        close(&mut h.nvim, split);
        h.nvim.flush().unwrap();
        h.want = first;
        h.gone = split;
        assert!(h.until(|x| x.is_current() && x.kind(x.gone).is_none()));

        // :close on the last window reopens it
        h.notes.clear();
        h.input(":close<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0
                && cur != x.want
                && x.kind(cur) == Some(WinKind::External)
                && x.kind(x.want).is_none()
        }));
        assert_eq!(Note::Reopen(first), h.notes[0]);

        // :q on the last window quits
        h.input(":q<CR>");
        assert!(!h.until(|_| false));
    }

    #[test]
    fn a_window_a_plugin_moved_back_into_the_layout_is_external_again_and_keeps_focus() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let anchor = h.wins[1];
        let split = h.adopt_split();

        // as edgy.nvim does on WinResized: back into the layout. Nothing told the
        // daemon, so only Neovim can put it back.
        h.sync(&format!(
            "vim.api.nvim_win_set_config({split}, {{ split = 'left', win = {anchor} }})"
        ));
        h.expect_external(split);
        h.sync(&format!(
            "local win = {split}\n\
             assert(vim.api.nvim_get_current_win() == win, 'lost focus')\n\
             assert(vim.w[win].edgy_disable, 'edgy.nvim would move it again')"
        ));
        assert!(h.until_external(split));
    }

    /// What happens between pull-backs.
    enum Between {
        Typing,
        Resizing,
    }

    /// Typed, or a pane resized, between pull-backs more often than a fight's
    /// limit: each is something the user did.
    fn pull_backs_between(between: Between) {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let anchor = h.wins[1];
        let split = h.adopt_split();

        for i in 0..12 {
            h.sync(&format!(
                "vim.api.nvim_win_set_config({split}, {{ split = 'left', win = {anchor} }})"
            ));
            h.expect_external(split);
            match between {
                Between::Typing => h.typed(),
                Between::Resizing => {
                    size(&mut h.nvim, split, 20 + i, 9);
                    h.nvim.flush().unwrap();
                }
            }
        }
    }

    #[test]
    fn typing_between_pull_backs_isnt_a_fight() {
        pull_backs_between(Between::Typing);
    }

    #[test]
    fn resizing_a_pane_between_pull_backs_isnt_a_fight() {
        pull_backs_between(Between::Resizing);
    }

    /// A plugin that puts the window back in the layout every 10 ms for 1.5 s,
    /// running `pull_extra` as well, and the daemon, which asks for it external
    /// and sized to its pane every 50 ms as it does for a window it sees
    /// ordinary. The plugin says how often it did, at 1 s and at 1.5 s, as
    /// `FIGHT_AT_1S` + n and `FIGHT_AT_END` + n.
    fn fight_ends_soon(pull_extra: &str) {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let anchor = h.wins[1];
        let split = h.adopt_split();

        size(&mut h.nvim, split, 17, 9);
        h.lua(&format!(
            "local win, anchor, pulls = {split}, {anchor}, 0\n\
             local timer, daemon = vim.uv.new_timer(), vim.uv.new_timer()\n\
             timer:start(10, 10, vim.schedule_wrap(function()\n\
             \x20 if vim.api.nvim_win_get_config(win).external then\n\
             \x20   pulls = pulls + 1\n\
             \x20   vim.api.nvim_win_set_config(win, {{ split = 'left', win = anchor }})\n\
             \x20   {pull_extra}\n\
             \x20 end\n\
             end))\n\
             daemon:start(50, 50, vim.schedule_wrap(function()\n\
             \x20 neorex.adopt(win, 17, 9)\n\
             \x20 neorex.size(win, 17, 9)\n\
             end))\n\
             vim.defer_fn(function() vim.rpcnotify(0, 'neorex_win', {FIGHT_AT_1S} + pulls) end, 1000)\n\
             vim.defer_fn(function()\n\
             \x20 timer:stop()\n\
             \x20 daemon:stop()\n\
             \x20 vim.rpcnotify(0, 'neorex_win', {FIGHT_AT_END} + pulls)\n\
             end, 1500)"
        ));
        assert!(h.until(|x| x.last_current() >= FIGHT_AT_END));
        let mut pulls_at_1s: i64 = 0;
        for n in &h.notes {
            if let Note::Current(c) = n
                && *c >= FIGHT_AT_1S
                && *c < FIGHT_AT_END
            {
                pulls_at_1s = *c - FIGHT_AT_1S;
            }
        }
        let pulls = h.last_current() - FIGHT_AT_END;
        // it gave up, as the plugin left it no other way, and didn't go on
        assert!((1..=10).contains(&pulls));
        assert_eq!(pulls_at_1s, pulls);
        // and sizing it to its pane would resize the window in Neovim's layout
        h.sync(&format!(
            "local win = {split}\n\
             assert(not vim.api.nvim_win_get_config(win).external, 'was never given up')\n\
             assert(vim.api.nvim_win_get_width(win) ~= 17, 'sized as if in its pane')"
        ));

        // a pane resized while it waits: the window fits it once it's back
        size(&mut h.nvim, split, 25, 9);
        // once the plugin stops and the wait is over, it goes back in its pane
        h.expect_external(split);
        h.sync(&format!(
            "assert(vim.api.nvim_win_get_width({split}) == 25, 'not the pane the user left it')"
        ));
    }

    #[test]
    fn a_plugin_that_always_moves_a_window_back_doesnt_start_a_fight_without_end() {
        fight_ends_soon("");
    }

    #[test]
    fn a_plugin_that_runs_normal_and_feedkeys_to_move_a_window_back_doesnt_either() {
        // none of that is the user typing
        fight_ends_soon(
            "vim.cmd.normal({ 'lh', bang = true }); vim.api.nvim_feedkeys('l', 'n', false)",
        );
    }

    #[test]
    fn a_fight_that_goes_on_is_left_alone_twice_as_long_each_time() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let anchor = h.wins[1];
        let split = h.adopt_split();

        // A plugin that puts the window back whenever it can, noting when each
        // fight starts, until the third has gone on a second, and a heartbeat so
        // the wait doesn't look like a hang.
        h.lua(&format!(
            "local win, anchor = {split}, {anchor}\n\
             local function now() return vim.uv.hrtime() / 1e6 end\n\
             local began = now()\n\
             local last = 0\n\
             bursts = {{}}\n\
             local timer, beat = vim.uv.new_timer(), vim.uv.new_timer()\n\
             timer:start(10, 10, vim.schedule_wrap(function()\n\
             \x20 if not vim.api.nvim_win_get_config(win).external then return end\n\
             \x20 local t = now()\n\
             \x20 if t - last > 500 then table.insert(bursts, t) end\n\
             \x20 last = t\n\
             \x20 vim.api.nvim_win_set_config(win, {{ split = 'left', win = anchor }})\n\
             end))\n\
             beat:start(500, 500, vim.schedule_wrap(function()\n\
             \x20 vim.rpcnotify(0, 'neorex_tabline', 0)\n\
             \x20 if (#bursts >= 3 and now() - bursts[3] > 1000) or now() - began > 40000 then\n\
             \x20   timer:stop()\n\
             \x20   beat:stop()\n\
             \x20   vim.rpcnotify(0, 'neorex_win', {FIGHT_AT_END})\n\
             \x20 end\n\
             end))"
        ));
        assert!(h.until(|x| x.last_current() >= FIGHT_AT_END));
        // each wait at least as long as it should be, 2 s then 4 s, and the
        // window goes back after each
        h.sync(
            "assert(#bursts >= 3, 'did not fight three times')\n\
             assert(bursts[2] - bursts[1] >= 1900, 'first wait too short')\n\
             assert(bursts[3] - bursts[2] >= 3900, 'second wait too short')",
        );
    }

    fn mv(dir: Direction, count: u32) -> Note {
        Note::Direction(Move { dir, count })
    }

    #[test]
    fn c_w_hjkl_send_a_direction_and_leave_the_current_window_c_w_w_still_cycles() {
        let Some(mut h) = Harness::start() else {
            return;
        };
        let first = h.wins[0];
        let split = h.split_and_adopt();

        // Neovim can't tell where the panes sit: it only says which way
        h.notes.clear();
        h.input("<C-w>l<C-w><C-k>3<C-w>j");
        // the other spellings: <C-h> arrives as <BS> from a terminal
        h.input("<C-w><BS><C-w><Left><C-w><Down><C-w><Up><C-w><Right>");
        // from Visual mode too
        h.input("v<C-w>h<Esc>");
        // a mapping built on them, as LazyVim's
        h.input(":nmap <lt>C-h> <lt>C-w>h<CR><C-h>");
        h.barrier();
        h.expect_notes(&[
            mv(Direction::Right, 1),
            mv(Direction::Up, 1),
            mv(Direction::Down, 3),
            mv(Direction::Left, 1),
            mv(Direction::Left, 1),
            mv(Direction::Down, 1),
            mv(Direction::Up, 1),
            mv(Direction::Right, 1),
            mv(Direction::Left, 1),
            mv(Direction::Left, 1),
        ]);

        // <C-w>w and <C-w>W cycle as before: through the anchor to the first window
        h.notes.clear();
        h.input("<C-w>w");
        h.want = first;
        assert!(h.until(Harness::is_current));
        h.notes.clear();
        h.input("<C-w>W");
        h.want = split;
        assert!(h.until(Harness::is_current));
    }

    #[test]
    fn a_new_tab_pages_window_is_marked_and_closing_it_closes_the_tab_page() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let first = h.wins[0];

        h.notes.clear();
        h.input(":tabnew<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let tab = h.last_current();
        let tab_note = h.split_note(tab).unwrap();
        assert!(tab_note.tab != 0);
        assert_eq!(
            Split {
                win: tab,
                side_by_side: false,
                before: false,
                from: 0,
                tab: tab_note.tab
            },
            tab_note
        );
        adopt(&mut h.nvim, tab, 30, 8);
        h.nvim.flush().unwrap();
        h.want = tab;
        assert!(h.until(|x| x.kind(x.want) == Some(WinKind::External)));

        h.notes.clear();
        h.input(":vsplit<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && cur != x.want && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let split = h.last_current();
        // a split in it belongs to it
        assert_eq!(tab_note.tab, h.split_note(split).unwrap().tab);
        // its first window closing leaves the tab page to its other windows, as
        // Diffview replaces it
        close(&mut h.nvim, tab);
        h.nvim.flush().unwrap();
        h.gone = tab;
        assert!(h.until(|x| x.kind(x.gone).is_none()));
        h.command(&format!(
            "if tabpagenr('$') != 2 || !nvim_win_is_valid({split}) | throw 'tab page closed' | endif | call rpcnotify(0, 'neorex_win', 4242)"
        ));
        h.want = 4242;
        assert!(h.until(Harness::is_current));

        // the last pane went away: the tab page goes with its anchor, back to the first
        h.notes.clear();
        close(&mut h.nvim, split);
        h.nvim.flush().unwrap();
        h.want = first;
        h.gone = split;
        assert!(h.until(|x| x.is_current() && x.kind(x.gone).is_none()));
        // an error fails the wait; else a made-up report ends it
        h.command(&format!(
            "if tabpagenr('$') != 1 || win_getid() != {first} | throw 'tab page left' | endif | call rpcnotify(0, 'neorex_win', 4242)"
        ));
        h.want = 4242;
        assert!(h.until(Harness::is_current));
    }

    #[test]
    fn c_w_l_sends_a_direction_from_a_tab_pages_window_too_but_not_from_a_float() {
        let Some(mut h) = Harness::start() else {
            return;
        };

        // a tab page's window shows in a Rex floating pane
        h.input(":tabnew<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let tab = h.last_current();
        assert!(h.split_note(tab).unwrap().tab != 0);
        adopt(&mut h.nvim, tab, 30, 8);
        h.nvim.flush().unwrap();
        h.want = tab;
        assert!(h.until(|x| x.kind(x.want) == Some(WinKind::External)));

        h.notes.clear();
        h.input("<C-w>h");
        h.barrier();
        h.expect_notes(&[mv(Direction::Left, 1)]);

        // a float isn't a pane: Neovim moves out of it itself
        h.input(":lua vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, { relative = 'editor', row = 1, col = 1, width = 8, height = 2 })<CR>");
        h.notes.clear();
        h.input("<C-w>l");
        h.barrier();
        for n in &h.notes {
            assert!(!matches!(n, Note::Direction(_)));
        }
    }

    #[test]
    fn tabclose_from_a_tab_pages_window_reopens_the_windows_that_followed_it_in() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        h.boot();
        let first = h.wins[0];

        // two windows: the last one would reopen anyway, the other wouldn't
        h.input(":vsplit<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && cur != x.wins[0] && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let split = h.last_current();
        adopt(&mut h.nvim, split, 20, 9);
        h.nvim.flush().unwrap();
        h.want = split;
        assert!(h.until(|x| x.kind(x.want) == Some(WinKind::External)));

        h.input(":tabnew<CR>");
        assert!(h.until(|x| {
            let cur = x.last_current();
            cur != 0 && cur != x.want && x.kind(cur) == Some(WinKind::Ordinary)
        }));
        let tab = h.last_current();
        adopt(&mut h.nvim, tab, 30, 8);
        h.nvim.flush().unwrap();
        h.want = tab;
        assert!(h.until(|x| x.kind(x.want) == Some(WinKind::External)));

        // closes every other window in the tab page, which holds them all
        h.notes.clear();
        h.input(":tabclose<CR>");
        assert!(h.until(|x| {
            let mut n = 0;
            for note in &x.notes {
                if let Note::Reopened { new, .. } = note
                    && x.kind(*new) == Some(WinKind::External)
                {
                    n += 1;
                }
            }
            n == 2 && x.kind(x.want).is_none()
        }));
        let mut olds: Vec<i64> = Vec::new();
        for note in &h.notes {
            if let Note::Reopened { old, .. } = note {
                olds.push(*old);
            }
        }
        olds.sort();
        assert_eq!(vec![first, split], olds);
    }

    #[test]
    fn the_tabline_is_reported_and_drawn_on_grid_1() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        assert!(h.until(|x| {
            x.wins[0] != 0 && x.kind(x.wins[0]) == Some(WinKind::External) && x.last_tabline() == 0
        }));

        // as bufferline does, with a second buffer
        h.input(":file one | lua vim.o.showtabline = 2<CR>");
        assert!(h.until(|x| {
            let Some(screen) = x.grids.get(1) else {
                return false;
            };
            x.last_tabline() == 1 && screen.height > 0 && screen.row_text(0).contains("one")
        }));

        // a second tab page shows it with showtabline=1
        h.input(":set showtabline=1<CR>");
        assert!(h.until(|x| x.last_tabline() == 0));
        h.input(":tabnew<CR>");
        assert!(h.until(|x| x.last_tabline() == 1));
    }

    /// A Lua tabline, as bufferline's: tabs from the left, then `R` and the
    /// width it laid out to, right-aligned. Shown with `showtabline=2`.
    const LUA_TABLINE: &str = "_G.tl = function() return 'L%=R' .. vim.o.columns end\n\
         vim.o.tabline = '%!v:lua.tl()'\n\
         vim.o.showtabline = 2";

    impl Harness {
        /// Grid 1's top row, the tabline.
        fn tabline_text(&self) -> String {
            self.grids.get(1).map_or(String::new(), |g| {
                if g.height == 0 {
                    String::new()
                } else {
                    g.row_text(0)
                }
            })
        }

        /// Waits until the tabline reads `want`, then blanks to the screen's edge.
        fn until_tabline(&mut self, want: &str) {
            let want = want.to_string();
            let ok = self.until(|x| {
                let t = x.tabline_text();
                t.starts_with(&want) && t[want.len()..].trim().is_empty()
            });
            assert!(ok, "tabline: {:?}", self.tabline_text());
        }

        /// Waits until a tabline such as `LUA_TABLINE`, `left` then `right` and
        /// the width it read, right-aligned, shows laid out to `cols` columns.
        fn until_laid_out_to(&mut self, cols: usize, left: char, right: char) {
            let label = format!("{right}{cols}");
            let gap = cols - 1 - label.len();
            self.until_tabline(&format!("{left}{}{label}", " ".repeat(gap)));
        }

        fn set_tabline_width(&mut self, cols: Option<u32>) {
            set_tabline_width(&mut self.nvim, cols);
            self.nvim.flush().unwrap();
        }

        /// The highlight of each cell of grid 1's top row.
        fn tabline_hls(&self) -> Vec<u32> {
            self.grids.get(1).map_or(Vec::new(), |g| {
                if g.height == 0 {
                    Vec::new()
                } else {
                    g.row(0).iter().map(|c| c.hl).collect()
                }
            })
        }

        /// Notes grid 1's top row, if a log is being kept.
        fn log_tabline(&mut self) {
            if self.tabline_log.is_none() {
                return;
            }
            let row = self.tabline_text();
            let log = self.tabline_log.as_mut().unwrap();
            if log.last() != Some(&row) {
                log.push(row);
            }
        }

        /// Waits until the tabline reads `left`, then `right` ending at column
        /// `cols - 1`, then blanks to the screen's edge.
        fn until_right_ends_at(&mut self, cols: usize, left: &str, right: &str) {
            self.until_tabline(&right_aligned(cols, left, right));
        }
    }

    /// `left` and `right` in `cols` columns, `right` ending the last.
    fn right_aligned(cols: usize, left: &str, right: &str) -> String {
        let gap = cols - left.len() - right.len();
        format!("{left}{}{right}", " ".repeat(gap))
    }

    #[test]
    fn a_lua_tabline_lays_out_to_the_tabline_width_and_blanks_the_rest() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(LUA_TABLINE);
        h.until_laid_out_to(60, 'L', 'R');

        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');
        h.sync("assert(vim.g.neorex_tabline_width == 30)");
        // the screen is as wide as before
        h.sync("assert(vim.o.columns == 60)");
    }

    #[test]
    fn a_tabline_that_ignores_the_width_keeps_its_first_columns() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // as wide as the screen: padding it must not cut its start off
        h.sync(
            "_G.tl = function() return 'A' .. string.rep('x', 58) .. 'Z' end\n\
             vim.o.tabline = '%!v:lua.tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.sync("assert(vim.g.neorex_tabline_width == 30)");
        // the first 30 columns, as they'd be without padding
        let want = format!("A{}", "x".repeat(29));
        assert!(
            h.until(|x| x.tabline_text().starts_with(&want)),
            "tabline: {:?}",
            h.tabline_text()
        );
    }

    #[test]
    fn a_tabline_wider_than_the_width_it_read_keeps_its_start() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // it reads the width, then lays out wider than it anyway
        h.sync(
            "_G.tl = function() local _ = vim.o.columns; return 'A' .. string.rep('x', 40) .. 'Z' end\n\
             vim.o.tabline = '%!v:lua.tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.sync("assert(vim.g.neorex_tabline_width == 30)");
        let want = format!("A{}Z", "x".repeat(40));
        assert!(
            h.until(|x| x.tabline_text().starts_with(&want)),
            "tabline: {:?}",
            h.tabline_text()
        );
    }

    #[test]
    fn with_no_tabline_width_the_tabline_is_as_it_was() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(LUA_TABLINE);
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');

        h.set_tabline_width(None);
        h.until_laid_out_to(60, 'L', 'R');
        h.sync(
            "assert(vim.g.neorex_tabline_width == nil)\n\
             assert(vim.o.tabline == '%!v:lua.tl()', vim.o.tabline)",
        );
    }

    #[test]
    fn a_tabline_that_fails_leaves_vim_o_as_it_was() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(
            "_G.real_o = vim.o\n\
             _G.tl = function() local w = vim.o.columns; if _G.boom then error('boom') end return 'L%=R' .. w end\n\
             vim.o.tabline = '%!v:lua.tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');

        h.sync("_G.boom = true\npcall(vim.cmd.redrawtabline)");
        h.sync(
            "assert(rawequal(vim.o, _G.real_o))\n\
             assert(vim.o.columns == 60)",
        );
    }

    #[test]
    fn a_vim_o_kept_from_a_fitted_tabline_reads_the_screen_width_after() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // as a module first required while the tabline is drawn, which keeps it
        h.sync(
            "_G.tl = function() _G.kept = vim.o; return 'L%=R' .. vim.o.columns end\n\
             vim.o.tabline = '%!v:lua.tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');
        h.sync(
            "assert(not rawequal(_G.kept, vim.o))\n\
             assert(_G.kept.columns == 60)\n\
             _G.kept.cmdheight = 2\n\
             assert(vim.o.cmdheight == 2)",
        );
    }

    #[test]
    fn a_tabline_set_again_is_fitted_again_from_an_autocommand_too() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(LUA_TABLINE);
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');

        // as bufferline's setup() running again
        h.sync("_G.tl2 = function() return 'M%=Q' .. vim.o.columns end\nvim.o.tabline = '%!v:lua.tl2()'");
        h.until_laid_out_to(30, 'M', 'Q');

        // set in an autocommand, where OptionSet doesn't fire
        h.lua(
            "_G.tl3 = function() return 'N%=P' .. vim.o.columns end\n\
             vim.api.nvim_create_autocmd('User', { pattern = 'Tl', callback = function() vim.o.tabline = '%!v:lua.tl3()' end })\n\
             vim.api.nvim_exec_autocmds('User', { pattern = 'Tl' })",
        );
        h.until_laid_out_to(30, 'N', 'P');
    }

    /// Set at the end of a timer a test runs, as a made-up report.
    const TIMER_DONE: i64 = 8000;

    #[test]
    fn a_format_string_tabline_is_padded_in_place_so_its_right_end_shows() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // as lualine sets it: a format string, with a `%=` right part
        h.sync("vim.o.tabline = '%#TabLine#L%<%=R'\nvim.o.showtabline = 2");
        h.until_right_ends_at(60, "L", "R");

        h.set_tabline_width(Some(30));
        h.until_right_ends_at(30, "L", "R");
        // still a format string, not a `%!` wrapper, and the screen as wide as before
        h.sync(
            "assert(vim.o.tabline:sub(1, 2) ~= '%!', vim.o.tabline)\n\
             assert(vim.o.columns == 60)",
        );
    }

    #[test]
    fn a_format_string_too_wide_is_cut_at_its_own_percent_less_than() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        let x = "x".repeat(40);
        h.sync(&format!(
            "vim.o.tabline = 'AB%<{x}Z'\nvim.o.showtabline = 2"
        ));
        h.set_tabline_width(Some(30));
        // as nvim_eval_statusline with maxwidth=30 gives it
        h.until_tabline(&format!("AB<{}Z", "x".repeat(26)));
    }

    #[test]
    fn a_format_string_too_wide_without_one_keeps_its_first_columns() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        let x = "x".repeat(40);
        h.sync(&format!("vim.o.tabline = 'AB{x}Z'\nvim.o.showtabline = 2"));
        h.set_tabline_width(Some(30));
        let want = format!("AB{}", "x".repeat(28));
        assert!(
            h.until(|x| x.tabline_text().starts_with(&want)),
            "tabline: {:?}",
            h.tabline_text()
        );
    }

    #[test]
    fn a_lua_tabline_that_reads_the_tabline_width_right_aligns_to_it() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // reads g:neorex_tabline_width, never vim.o.columns
        h.sync(
            "_G.tl = function() return 'L%=R' .. (vim.g.neorex_tabline_width or 60) end\n\
             vim.o.tabline = '%!v:lua.tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');
    }

    #[test]
    fn a_vimscript_tabline_that_reads_the_tabline_width_right_aligns_to_it() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(
            "vim.cmd([[\n\
             function! Tl()\n\
             \x20 return 'L%=R' . get(g:, 'neorex_tabline_width', 60)\n\
             endfunction]])\n\
             vim.o.tabline = '%!Tl()'\n\
             vim.o.showtabline = 2",
        );
        h.set_tabline_width(Some(30));
        h.until_laid_out_to(30, 'L', 'R');
    }

    /// Runs `set` (Lua that sets 'tabline') with a tabline width coming,
    /// changing and going, and checks 'tabline' reads exactly what `set` set
    /// after, and `padded` (a Lua expression, given the screen's width and
    /// the tabline width) while there's a width, if it's given.
    fn tabline_comes_back_exactly(set: &str, padded: Option<&str>) {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync(&format!(
            "{set}\n_G.orig = vim.o.tabline\nvim.o.showtabline = 2"
        ));
        for w in [30, 45, 30] {
            h.set_tabline_width(Some(w));
            // twice: the SafeState in between must leave it as it is
            h.sync("assert(vim.o.tabline ~= '')");
            let check = padded.map_or(String::new(), |p| {
                format!(
                    "local cols, width = vim.o.columns, {w}\n\
                     assert(vim.o.tabline == {p}, vim.o.tabline)"
                )
            });
            h.sync(&check);
        }
        h.set_tabline_width(None);
        h.sync("assert(vim.o.tabline == _G.orig, vim.o.tabline)");
        h.sync("assert(vim.o.tabline == _G.orig, vim.o.tabline)");
    }

    #[test]
    fn a_format_string_comes_back_byte_for_byte() {
        tabline_comes_back_exactly(
            "vim.o.tabline = '%#TabLine#L%<%=R'",
            Some("_G.orig .. '%<' .. string.rep(' ', cols - width)"),
        );
    }

    #[test]
    fn a_percent_bang_expression_comes_back_byte_for_byte() {
        tabline_comes_back_exactly(
            "_G.tl = function() return 'L%=R' end\nvim.o.tabline = '%!v:lua.tl()'",
            None,
        );
    }

    #[test]
    fn a_format_string_ending_in_percent_less_than_and_blanks_is_never_shortened() {
        // the user's own, so padded after it, once
        tabline_comes_back_exactly(
            "vim.o.tabline = 'L%=R%<   '",
            Some("_G.orig .. '%<' .. string.rep(' ', cols - width)"),
        );
    }

    #[test]
    fn a_new_width_or_screen_width_moves_the_right_part_to_the_new_edge() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.tabline = 'L%=R'\nvim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.until_right_ends_at(30, "L", "R");
        h.set_tabline_width(Some(20));
        h.until_right_ends_at(20, "L", "R");
        h.set_tabline_width(Some(40));
        h.until_right_ends_at(40, "L", "R");

        // a wider screen: the same width, so the same edge
        h.nvim.ui_try_resize(80, 10).unwrap();
        assert!(h.until(|x| x.grids.get(1).is_some_and(|g| g.width == 80)));
        h.until_right_ends_at(40, "L", "R");
        // a narrower one
        h.nvim.ui_try_resize(50, 10).unwrap();
        assert!(h.until(|x| x.grids.get(1).is_some_and(|g| g.width == 50)));
        h.until_right_ends_at(40, "L", "R");
    }

    #[test]
    fn the_same_tabline_width_again_leaves_tabline_alone() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.tabline = 'L%=R'\nvim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.until_right_ends_at(30, "L", "R");
        h.sync(
            "_G.sets, _G.padded = 0, vim.o.tabline\n\
             vim.api.nvim_create_autocmd('OptionSet', {\n\
             \x20 pattern = 'tabline',\n\
             \x20 callback = function() _G.sets = _G.sets + 1 end,\n\
             })",
        );

        h.set_tabline_width(Some(30));
        h.sync("assert(_G.sets == 0, _G.sets)\nassert(vim.o.tabline == _G.padded)");
    }

    #[test]
    fn a_format_string_set_again_every_20_ms_is_never_seen_unpadded() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.tabline = '%#TabLine#L%<%=R'\nvim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.until_right_ends_at(30, "L", "R");

        // as lualine does, from a scheduled callback, for about a second
        h.tabline_log = Some(Vec::new());
        h.lua(&format!(
            "local n = 0\n\
             local timer = vim.uv.new_timer()\n\
             timer:start(20, 20, vim.schedule_wrap(function()\n\
             \x20 vim.o.tabline = '%#TabLine#L%<%=R'\n\
             \x20 n = n + 1\n\
             \x20 if n == 50 then\n\
             \x20   timer:stop()\n\
             \x20   vim.rpcnotify(0, 'neorex_win', {TIMER_DONE})\n\
             \x20 end\n\
             end))"
        ));
        h.want = TIMER_DONE;
        assert!(h.until(Harness::is_current));

        let want = format!("{:<60}", right_aligned(30, "L", "R"));
        let log = h.tabline_log.take().unwrap();
        assert!(log.iter().all(|row| *row == want), "rows: {log:?}");
    }

    #[test]
    fn a_format_string_set_in_an_autocommand_shows_padded() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.sync(
            "vim.api.nvim_create_autocmd('BufEnter', {\n\
             \x20 callback = function() vim.o.tabline = 'L%=R' end,\n\
             })",
        );
        h.input(":enew<CR>");
        h.until_right_ends_at(30, "L", "R");
    }

    #[test]
    fn a_padded_tabline_starts_with_the_format_string_as_it_was_set() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.tabline = '%#lualine_c_normal#L%<%=R'\nvim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.sync(
            "local orig = '%#lualine_c_normal#L%<%=R'\n\
             assert(#vim.o.tabline > #orig)\n\
             assert(vim.o.tabline:sub(1, #orig) == orig, vim.o.tabline)\n\
             assert(vim.o.tabline:find('lualine', 1, true))",
        );
    }

    #[test]
    fn padding_keeps_the_highlights_the_tabline_had() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        // ends in a highlight of its own
        h.sync(
            "vim.api.nvim_set_hl(0, 'TabLineSel', { bg = 'red', ctermbg = 1 })\n\
             vim.api.nvim_set_hl(0, 'TabLineFill', { bg = 'blue', ctermbg = 4 })\n\
             vim.o.tabline = '%#TabLineSel#x%#TabLineFill#y'\n\
             vim.o.showtabline = 2",
        );
        h.until_tabline("xy");
        // Neovim fills what's left in the last highlight
        let [sel, fill] = [h.tabline_hls()[0], h.tabline_hls()[1]];
        assert_ne!(sel, fill);
        assert_eq!(fill, h.tabline_hls()[20]);

        h.set_tabline_width(Some(30));
        // never unpadded, so once it's changed it's seen padded
        h.sync("vim.o.tabline = '%#TabLineSel#x%#TabLineFill#z'");
        h.until_tabline("xz");
        let hls = h.tabline_hls();
        assert_eq!([sel, fill, fill], [hls[0], hls[1], hls[20]]);
    }

    #[test]
    fn an_empty_tabline_is_left_alone() {
        let Some(mut h) = Harness::start_sized(60, 10) else {
            return;
        };
        h.sync("vim.o.tabline = ''\nvim.o.showtabline = 2");
        h.set_tabline_width(Some(30));
        h.sync("assert(vim.o.tabline == '', vim.o.tabline)");
        h.sync("assert(vim.o.tabline == '', vim.o.tabline)");
        // and the one set after is padded
        h.sync("vim.o.tabline = 'L%=R'");
        h.until_right_ends_at(30, "L", "R");
    }

    #[test]
    fn cmdheight_is_reported_at_setup_and_when_it_changes_from_an_autocommand_too() {
        let Some(mut h) = Harness::start() else {
            return;
        };
        // the default, said at setup
        assert_eq!(Some(1), h.last_cmdheight());

        h.input(":set cmdheight=0<CR>");
        assert!(h.until(|x| x.last_cmdheight() == Some(0)));

        // set in an autocommand, as plugins do, where OptionSet doesn't fire
        h.lua(
            "vim.api.nvim_create_autocmd('User', { pattern = 'Bump', callback = function() vim.o.cmdheight = 2 end })\n\
             vim.api.nvim_exec_autocmds('User', { pattern = 'Bump' })",
        );
        assert!(h.until(|x| x.last_cmdheight() == Some(2)));

        // each tab page has its own: going back reports the first's
        h.input(":tabnew<CR>");
        h.input(":set cmdheight=1<CR>");
        assert!(h.until(|x| x.last_cmdheight() == Some(1)));
        h.input(":tabprevious<CR>");
        assert!(h.until(|x| x.last_cmdheight() == Some(2)));
    }

    #[test]
    fn each_window_shows_a_statusline_inside_its_pane() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };

        // setup sizes the first window to 40 x 9: 8 rows of text and the statusline
        assert!(h.until(|x| {
            x.wins[0] != 0
                && x.grid_height(x.wins[0]) == 9
                && x.row_starts(x.wins[0], 8, "[No Name]")
        }));

        // :set statusline reaches the window
        h.input(":set statusline=GLOBAL<CR>");
        assert!(h.until(|x| x.row_starts(x.wins[0], 8, "GLOBAL")));

        // no statusline: the text gets the row back
        h.input(":set laststatus=0<CR>");
        assert!(h.until(|x| x.grid_height(x.wins[0]) == 9 && x.row_starts(x.wins[0], 8, "~")));

        // the user's own statusline for a window stays
        h.input(
            ":set laststatus=2 | setlocal statusline=MINE | lua vim.go.statusline = 'OTHER'<CR>",
        );
        assert!(h.until(|x| x.grid_height(x.wins[0]) == 9 && x.row_starts(x.wins[0], 8, "MINE")));
    }

    #[test]
    fn laststatus_3_shows_the_statusline_in_every_windows_pane() {
        let argv = [
            "nvim",
            "--clean",
            "--embed",
            "-n",
            "--cmd",
            "set laststatus=3 statusline=GLOBAL",
        ];
        let Some(mut h) = Harness::spawn(&argv, 40, 10) else {
            return;
        };

        // set before neorex started: Neovim alone would draw no statusline here
        assert!(h.until(|x| {
            x.wins[0] != 0 && x.grid_height(x.wins[0]) == 9 && x.row_starts(x.wins[0], 8, "GLOBAL")
        }));
        let first = h.wins[0];
        let split = h.split_and_adopt();
        h.both_show(first, split, "GLOBAL");

        // set after it started: the value changes from 2, so OptionSet fires
        h.input(":set laststatus=3 statusline=LATER<CR>");
        h.both_show(first, split, "LATER");

        // set in an autocommand, as LazyVim does for lualine: no OptionSet
        h.input(":set laststatus=2<CR>:autocmd User Later ++once set laststatus=3 statusline=INAUTO<CR>:doautocmd User Later<CR>");
        h.both_show(first, split, "INAUTO");

        // From here no pane's statusline may go blank. A global statusline plugin
        // keeps only the current window's 'statusline' up to date: the other
        // panes show that line, whichever window is current.
        h.input(":lua vim.api.nvim_create_autocmd('OptionSet', { pattern = 'statusline', callback = function() if vim.v.option_new == '' then vim.g.blanked = 1 end end })<CR>");
        h.input(":setlocal statusline=CURRENT<CR>");
        h.both_show(first, split, "CURRENT");
        // the other window current: its own line, set for that window only
        h.input(&format!(
            ":lua vim.api.nvim_set_current_win({first}); vim.wo[0][0].statusline = 'OTHER'<CR>"
        ));
        h.both_show(first, split, "OTHER");
        // and back: the pane that showed a copy has its own line again
        h.input(&format!(":call nvim_set_current_win({split})<CR>"));
        h.both_show(first, split, "CURRENT");
        // changed in an autocommand: no OptionSet
        h.input(
            ":autocmd User Chg ++once setlocal statusline=INAUTOCMD<CR>:doautocmd User Chg<CR>",
        );
        h.both_show(first, split, "INAUTOCMD");

        // a focused float: the panes keep their line
        h.input(":lua vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, { relative = 'editor', row = 1, col = 1, width = 8, height = 2 })<CR>");
        h.input(":set laststatus=3<CR>");
        h.both_show(first, split, "INAUTOCMD");
        h.input(":close<CR>:setlocal statusline=AFTER<CR>");
        h.both_show(first, split, "AFTER");

        h.command(
            "if get(g:, 'blanked') | throw 'a pane lost its statusline' | endif | call rpcnotify(0, 'neorex_win', 4242)",
        );
        h.want = 4242;
        assert!(h.until(Harness::is_current));
    }

    #[test]
    fn a_windows_own_statusline_under_laststatus_3_goes_when_it_loses_focus() {
        let argv = [
            "nvim",
            "--clean",
            "--embed",
            "-n",
            "--cmd",
            "set laststatus=3 statusline=GLOBAL",
        ];
        let Some(mut h) = Harness::spawn(&argv, 40, 10) else {
            return;
        };
        assert!(h.until(|x| x.wins[0] != 0 && x.grid_height(x.wins[0]) == 9));
        let first = h.wins[0];
        let split = h.split_and_adopt();

        // a window such as quickfix sets its own statusline: every pane shows it
        h.input(":setlocal statusline=QF<CR>");
        h.both_show(first, split, "QF");
        // and leaves it with the focus
        h.input(&format!(":call nvim_set_current_win({first})<CR>"));
        h.both_show(first, split, "GLOBAL");
        // each pane has its own again when mirroring ends: split's from before
        h.input(":set laststatus=2 | setlocal statusline=MINE<CR>");
        assert!(h.until(|x| x.row_starts(first, 8, "MINE") && x.row_starts(split, 8, "QF")));
    }

    #[test]
    fn a_windows_own_statusline_leaves_no_blank_row_with_laststatus_0() {
        let Some(mut h) = Harness::spawn(&CLEAN, 40, 10) else {
            return;
        };
        assert!(h.until(|x| x.wins[0] != 0 && x.grid_height(x.wins[0]) == 9));

        h.input(":setlocal statusline=MINE | set laststatus=0<CR>");
        assert!(h.until(|x| x.grid_height(x.wins[0]) == 9 && x.row_starts(x.wins[0], 8, "~")));
    }

    /// The windows of `layout`, left to right, if it's one row of them: into `out`.
    fn layout_row(layout: &Layout, out: &mut [Node]) {
        let nodes = layout.items();
        assert_eq!(Kind::Row, nodes[0].kind);
        let mut n: usize = 0;
        for kid in layout.kids(0) {
            assert_eq!(Kind::Window, nodes[kid].kind);
            if n < out.len() {
                out[n] = nodes[kid];
            }
            n += 1;
        }
        assert_eq!(out.len(), n);
    }

    #[test]
    fn a_tab_page_built_as_diffview_does_reports_its_layout_final_order_and_sizes() {
        let Some(mut h) = Harness::start_sized(120, 30) else {
            return;
        };
        let first = h.wins[0];

        // a tab page with a pivot window; two windows left of it, the pivot
        // closed, then a narrow file panel left of all: made in the order
        // old, new, panel, shown as panel, old, new
        h.notes.clear();
        h.lua(
            "local api = vim.api\n\
             vim.cmd('tabnew')\n\
             local pivot = api.nvim_get_current_win()\n\
             for _ = 1, 2 do\n\
             \x20 api.nvim_win_call(pivot, function() vim.cmd('aboveleft vsplit') end)\n\
             end\n\
             api.nvim_win_close(pivot, true)\n\
             vim.cmd('topleft vsplit')\n\
             vim.cmd('vertical resize 20')\n\
             vim.rpcnotify(0, 'neorex_win', 4242)",
        );
        // the layout comes once the command is done
        assert!(h.until(|x| x.notes.iter().any(|n| matches!(n, Note::Layout(_)))));
        let mut made: Vec<i64> = Vec::new();
        let mut tab: i64 = 0;
        for note in &h.notes {
            if let Note::Split(s) = note {
                made.push(s.win);
                if s.tab != 0 {
                    tab = s.tab;
                }
            }
        }
        // the pivot, old, new, panel: the layout names the three left, by size
        assert_eq!(4, made.len());
        let mut shown = [Node::default(); 3];
        let layout = *h.last_layout(tab).expect("NoLayout");
        layout_row(&layout, &mut shown);
        assert_eq!(made[3], shown[0].win);
        assert_eq!(made[1], shown[1].win);
        assert_eq!(made[2], shown[2].win);
        assert!(shown[0].w >= 15 && shown[0].w <= 25);
        assert!(shown[1].w > 30 && shown[2].w > 30);

        // once external, they're out of Neovim's own layout: it's still told
        h.tab = tab;
        h.watch = [made[1], made[2], made[3]];
        for win in h.watch {
            adopt(&mut h.nvim, win, 30, 8);
        }
        h.nvim.flush().unwrap();
        assert!(h.until(Harness::watched_external));
        layout_row(h.last_layout(tab).unwrap(), &mut shown);
        assert_eq!(made[3], shown[0].win);
        assert_eq!(made[1], shown[1].win);
        assert_eq!(made[2], shown[2].win);
        assert!(shown[0].w < shown[1].w && shown[0].w < shown[2].w);

        // the panel closing leaves the other two
        h.notes.clear();
        close(&mut h.nvim, made[3]);
        h.nvim.flush().unwrap();
        assert!(h.until(|x| x.last_layout(x.tab).is_some_and(|l| l.len == 3)));
        let mut two = [Node::default(); 2];
        layout_row(h.last_layout(tab).unwrap(), &mut two);
        assert_eq!(made[1], two[0].win);
        assert_eq!(made[2], two[1].win);

        // a file panel again, toggled back on: leftmost again, the others keeping their shares
        h.notes.clear();
        h.lua("vim.cmd('topleft vsplit')\nvim.cmd('vertical resize 20')");
        assert!(h.until(|x| x.last_layout(x.tab).is_some_and(|l| l.len == 4)));
        layout_row(h.last_layout(tab).unwrap(), &mut shown);
        assert_eq!(made[1], shown[1].win);
        assert_eq!(made[2], shown[2].win);
        assert!(shown[0].w >= 15 && shown[0].w <= 25);
        // 59 and 39 cells before
        assert!(shown[1].w > shown[2].w + 10);

        // Diffview's Panel:resize, on a window that's external: Neovim's size
        // for it is the layout's, not what its pane fits it to
        h.notes.clear();
        h.lua(&format!("vim.api.nvim_win_set_width({}, 45)", made[1]));
        assert!(h.until(|x| {
            let Some(l) = x.last_layout(x.tab) else {
                return false;
            };
            l.items()
                .iter()
                .any(|node| node.kind == Kind::Window && node.w == 45)
        }));

        // a window as wide as the editor leaves the layout's windows a cell
        // for all of them: every size is still at least one
        h.notes.clear();
        h.lua("vim.cmd('vnew')\nvim.cmd('vertical resize 999')");
        assert!(h.until(|x| x.last_layout(x.tab).is_some_and(|l| l.len == 5)));
        for node in h.last_layout(tab).unwrap().items() {
            assert!(node.w >= 1 && node.h >= 1);
        }

        // :tabclose, as :DiffviewClose does: the windows go, and the window
        // current before comes back, reopened as it was
        h.notes.clear();
        h.input(":tabclose<CR>");
        assert!(h.until(|x| {
            for note in &x.notes {
                if let Note::Reopened { new, .. } = note
                    && x.kind(*new) == Some(WinKind::External)
                {
                    return x.last_current() == *new
                        && x.kind(x.watch[0]).is_none()
                        && x.kind(x.watch[1]).is_none();
                }
            }
            false
        }));
        for note in &h.notes {
            if let Note::Reopened { old, .. } = note {
                assert_eq!(first, *old);
            }
        }
        h.command(
            "if tabpagenr('$') != 1 | throw 'tab page left' | endif | call rpcnotify(0, 'neorex_win', 4242)",
        );
        h.want = 4242;
        assert!(h.until(Harness::is_current));
    }

    #[test]
    fn a_cmdheight_note_is_read_and_one_with_no_rows_or_a_negative_count_is_an_error() {
        for rows in [0i64, 1, 2] {
            let mut buf = Vec::new();
            msgpack::write_array_header(&mut buf, 1);
            msgpack::write_int(&mut buf, rows);
            let mut d = Decoder::of(&buf);
            match parse_note(b"neorex_cmdheight", &mut d).unwrap().unwrap() {
                Note::Cmdheight(got) => assert_eq!(rows, got),
                other => panic!("not a cmdheight note: {other:?}"),
            }
        }

        let mut none = Decoder::of(b"\x90");
        assert_eq!(
            Err(rpc::Error::BadMessage),
            parse_note(b"neorex_cmdheight", &mut none)
        );
        let mut buf = Vec::new();
        msgpack::write_array_header(&mut buf, 1);
        msgpack::write_int(&mut buf, -1);
        let mut negative = Decoder::of(&buf);
        assert_eq!(
            Err(rpc::Error::BadMessage),
            parse_note(b"neorex_cmdheight", &mut negative)
        );
    }

    /// Parses a `neorex_layout` note of tab page 7 with `flat` numbers.
    fn parse_layout_note(flat: &[i64]) -> rpc::Result<Option<Note>> {
        let mut buf = Vec::new();
        msgpack::write_array_header(&mut buf, 2);
        msgpack::write_int(&mut buf, 7);
        msgpack::write_array_header(&mut buf, flat.len() as u32);
        for &n in flat {
            msgpack::write_int(&mut buf, n);
        }
        let mut d = Decoder::of(&buf);
        parse_note(b"neorex_layout", &mut d)
    }

    fn layout_of(note: Option<Note>) -> Layout {
        match note.unwrap() {
            Note::Layout(l) => l,
            other => panic!("not a layout note: {other:?}"),
        }
    }

    #[test]
    fn a_layout_note_is_read_empty_or_not_and_a_note_that_doesnt_add_up_is_an_error() {
        let empty = layout_of(parse_layout_note(&[]).unwrap());
        assert_eq!(7, empty.tab);
        assert_eq!(0, empty.len);

        // a row of two windows
        let row =
            layout_of(parse_layout_note(&[1, 2, 30, 5, 0, 1000, 10, 5, 0, 1001, 20, 5]).unwrap());
        assert_eq!(3, row.len);
        let mut kids = row.kids(0);
        assert_eq!(Some(1), kids.next());
        assert_eq!(Some(2), kids.next());
        assert_eq!(None, kids.next());
        assert_eq!(1001, row.nodes[2].win);

        // a negative size, a row of one, an unknown kind, too few or too many numbers
        let bad = Err(rpc::Error::BadMessage);
        assert_eq!(
            bad,
            parse_layout_note(&[1, 2, -3, 5, 0, 1000, 10, 5, 0, 1001, 20, 5])
        );
        assert_eq!(bad, parse_layout_note(&[1, 1, 30, 5, 0, 1000, 30, 5]));
        assert_eq!(bad, parse_layout_note(&[9, 1000, 30, 5]));
        assert_eq!(bad, parse_layout_note(&[1, 2, 30, 5, 0, 1000, 10, 5]));
        assert_eq!(bad, parse_layout_note(&[0, 1000, 30, 5, 0, 1001, 10, 5]));
    }

    #[test]
    fn a_tab_pages_layout_follows_windows_moved_before_theyre_external_and_none_is_told_once_its_too_big()
     {
        let Some(mut h) = Harness::start_sized(120, 30) else {
            return;
        };

        // two windows; the one made last, on the left, moved to the far right
        h.notes.clear();
        h.lua("vim.cmd('tabnew')\nvim.cmd('vsplit')");
        assert!(h.until(|x| x.notes.iter().any(|n| matches!(n, Note::Layout(_)))));
        let mut made: Vec<i64> = Vec::new();
        for note in &h.notes {
            if let Note::Split(s) = note {
                made.push(s.win);
                h.tab = s.tab;
            }
        }
        assert_eq!(2, made.len());
        let mut two = [Node::default(); 2];
        layout_row(h.last_layout(h.tab).unwrap(), &mut two);
        assert_eq!(made[1], two[0].win);

        h.notes.clear();
        h.lua("vim.cmd('wincmd L')");
        assert!(h.until(|x| {
            let Some(l) = x.last_layout(x.tab) else {
                return false;
            };
            l.nodes[1].win != x.watch[0] && l.len == 3
        }));
        layout_row(h.last_layout(h.tab).unwrap(), &mut two);
        assert_eq!(made[0], two[0].win);
        assert_eq!(made[1], two[1].win);

        // more windows than the layout has room for: the daemon is told to
        // drop the layout it has
        h.notes.clear();
        h.lua(
            "for _ = 1, 6 do vim.cmd('vsplit') end\n\
             for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do\n\
             \x20 if not vim.api.nvim_win_get_config(win).external then\n\
             \x20   vim.api.nvim_win_call(win, function() for _ = 1, 6 do vim.cmd('split') end end)\n\
             \x20 end\n\
             end",
        );
        assert!(h.until(|x| x.last_layout(x.tab).is_some_and(|l| l.len == 0)));
        // not a layout of some of them before
        let splits = h
            .notes
            .iter()
            .filter(|n| matches!(n, Note::Split(_)))
            .count();
        assert!(splits >= 54);
    }

    #[test]
    fn an_idle_floating_tab_page_doesnt_keep_neovim_busy() {
        let Some(mut h) = Harness::start_sized(120, 30) else {
            return;
        };
        h.lua("vim.cmd('tabnew')\nvim.cmd('vsplit')");
        assert!(h.until(|x| x.notes.iter().any(|n| matches!(n, Note::Layout(_)))));

        // SafeState fires again after every scheduled callback, so one that
        // schedules work keeps Neovim looping, while a floating tab page is current
        h.lua(
            "_G.safe_states = 0\n\
             vim.api.nvim_create_autocmd('SafeState', { callback = function() _G.safe_states = _G.safe_states + 1 end })\n\
             vim.defer_fn(function() vim.rpcnotify(0, 'neorex_tabline', 1000 + _G.safe_states) end, 300)",
        );
        assert!(h.until(|x| x.last_tabline() >= 1000));
        assert!(h.last_tabline() - 1000 < 20);
    }
}
