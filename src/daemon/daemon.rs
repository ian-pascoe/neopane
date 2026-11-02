//! The running daemon: Neovim, its grids, the Rex connection, a pane per
//! window and a floating pane per group of floats that took focus. After
//! each Neovim flush, `reconcile` gives new windows and float groups panes
//! and hangs up panes whose windows closed; then every pane is drawn.
// TODO: Needs a pretty major rewrite
//
// Ownership notes:
// - The daemon owns the loop and the Neovim and Rex clients (`event_loop`,
//   `nvim.client`, `rex.client`), and drops them after its panes (see `Drop`).
// - A pane is passed around by its id, looked up again after anything that
//   may add or remove panes (see `Panes`).
// - `NvimSession` aims forwarders through `PaneForwarders`, which borrows just
//   the panes and grids: the daemon itself can't be borrowed whole while
//   `self.nvim` is.

use std::borrow::Cow;
use std::os::fd::RawFd;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{Value, json};

use super::event_loop::{Event, Kind, Loop};
use super::floating_panes::{self, FloatingPanes};
use super::focus_gate::AimForwarder;
use super::geometry::{self, Geometry, REX_DEFAULT_SIZE, TAB_LAYER_BOUNDS};
use super::nvim_session::NvimSession;
use super::pane::{self, Close, Fit, LastAskedWinSize, Pane, PaneFd, RexBlock};
use super::pane_helper;
use super::panes::Panes;
use super::rex_layout::{self, LayoutSnapshot};
use super::surface::{self, Surface};
use super::tab_placement::{self, Action, PaneMove, Tab, WindowState};
use super::window_changes::WindowChanges;
use crate::nvim::forwarder::{self, Forwarder};
use crate::nvim::msgpack::{self, Decoder};
use crate::nvim::rpc;
use crate::nvim::windows::{self, Layout};
use crate::rex::session::{self as rex_session, Conn, Request, Session as RexSession};
use crate::screen::float_groups;
use crate::screen::grid::{Cell, Grid, blank_cut_wide};
use crate::screen::grids::{Grids, WinKind};
use crate::screen::pane_view::{Opts, Rect};
use crate::term::input;

/// The daemon is a top layer: any error will do.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub struct Daemon {
    /// Our executable and the helpers' socket, for new panes' command.
    pub exe: String,
    pub sock: String,
    pub grids: Grids,
    pub panes: Panes,
    pub changes: WindowChanges,
    /// Rows the screen's tabline takes, as Neovim last said. Drawn over the
    /// panes along the top of the Rex window, as it would be over the screen.
    pub tabline_rows: u32,
    /// Rows at the bottom of every pane kept for Neovim's messages and command
    /// line: its 'cmdheight', as last said. One until it says. Each tab page has
    /// its own 'cmdheight', and this follows the current one, for every pane:
    /// those of other tab pages, and of a tab page's floating pane, too. Messages
    /// that show beyond them are drawn over the window, in the focused pane.
    pub msg_rows: u32,
    /// Where Neovim has the windows of each tab page made after setup, as it
    /// last said, and what was done to place their panes to match.
    pub tab_layouts: IndexMap<i64, Tab>,
    /// The Rex layout last applied to the panes.
    pub rex_layout: Option<LayoutSnapshot>,
    /// When the escape timer is set to fire, if it is.
    escape_timer_due: Option<Instant>,
    /// Set by `stop`; `run` returns it.
    pub stopped: Option<StopReason>,
    /// Whether the loop still watches Rex's fd.
    rex_watched: bool,
    // Dropped last, in this order: the loop, then Rex, then Neovim.
    pub event_loop: Loop,
    pub rex: RexSession,
    pub nvim: NvimSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    NvimExited,
    PaneGone,
    Signal,
}

impl StopReason {
    /// The reason's name, as logged.
    pub fn tag_name(self) -> &'static str {
        match self {
            StopReason::NvimExited => "nvim_exited",
            StopReason::PaneGone => "pane_gone",
            StopReason::Signal => "signal",
        }
    }
}

/// What a loop event's token names; pane tokens are the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Token {
    Signal,
    NvimSocket,
    HelperListener,
    RexSocket,
    FocusTimer,
    LayoutTimer,
    SwitchTimer,
    EscapeTimer,
}

impl Token {
    /// The fixed token `token` is, or None for a pane's.
    pub fn of(token: usize) -> Option<Token> {
        Some(match token {
            0 => Token::Signal,
            1 => Token::NvimSocket,
            2 => Token::HelperListener,
            3 => Token::RexSocket,
            4 => Token::FocusTimer,
            5 => Token::LayoutTimer,
            6 => Token::SwitchTimer,
            7 => Token::EscapeTimer,
            _ => return None,
        })
    }
}

/// An event on one of a pane's fds, naming the pane by its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneEvent {
    pub pane: u32,
    pub fd: PaneFd,
}

// the fixed tokens must stay below the pane tokens
const _: () = assert!((Token::EscapeTimer as usize) < pane::TOKEN_BASE);

/// The id `Pane::startup` gives the pane neorex was run in.
const STARTUP_PANE_ID: u32 = 0;

/// How long losing focus waits before Neovim hears of it: focus moving
/// from one pane to another, such as into a floating pane, arrives as an
/// out then an in, and shouldn't fire FocusLost and FocusGained.
const FOCUS_OUT_DELAY_MS: u32 = 80;

/// How often a window switch still unanswered is checked for waiting behind
/// a command that needs a key.
const SWITCH_RECHECK_MS: u32 = 100;

/// How long a new split waits for the app to size it before trusting its terminal's size.
const LAYOUT_WAIT_MS: u32 = 1000;

/// A count on a directional window command repeats the move this many times
/// at most: a window has few panes to cross, and a stray `9999<C-w>l` must not
/// send Rex thousands of requests.
const MAX_DIRECTION_COUNT: u32 = 20;

/// More rows than any terminal has: a 'cmdheight' past it is as good.
const MAX_MSG_ROWS: u32 = 1000;

/// Most cells of the tabline one pane draws.
const TABLINE_MAX: usize = 256;

/// Most grids kept out of the panes that aren't floating panes.
const SKIP_MAX: usize = float_groups::MAX_GROUPS * 16;

/// What a pane draws of the screen's tabline, and the column it starts at.
struct Tabline {
    cells: Option<Vec<Cell>>,
    col: u32,
}

/// The Rex blocks of the panes a move is between.
struct Blocks {
    moved: RexBlock,
    anchor: RexBlock,
}

/// The panes, as `NvimSession` needs them to send input: borrowed apart from the daemon's `nvim`.
pub struct PaneForwarders<'a> {
    pub panes: &'a mut Panes,
    pub grids: &'a Grids,
}

impl AimForwarder for PaneForwarders<'_> {
    /// Pane `id`'s forwarder, aimed at the grid of its window; None once the
    /// pane is gone.
    fn aim_forwarder(&mut self, id: u32) -> Option<&mut Forwarder> {
        let pane = self.panes.by_id_mut(id)?;
        let grid = geometry::grid_of_pane(self.grids, pane);
        let floating = pane.layer.is_some();
        let surface = pane.terminal_surface.as_mut()?;
        // a floating pane's default grid is set as it draws
        if !floating {
            surface.forwarder.grid = grid.unwrap_or(0);
        }
        Some(&mut surface.forwarder)
    }
}

/// Hangs up the panes first; the loop and the clients go after, as their
/// fields drop.
impl Drop for Daemon {
    fn drop(&mut self) {
        self.panes.unregister_all(&self.event_loop);
        self.panes.deinit();
    }
}

impl Daemon {
    /// A daemon on `event_loop`, Neovim and Rex, with no panes yet.
    pub fn new(
        event_loop: Loop,
        nvim: NvimSession,
        rex: RexSession,
        exe: String,
        sock: String,
    ) -> Daemon {
        Daemon {
            exe,
            sock,
            grids: Grids::default(),
            panes: Panes::default(),
            changes: WindowChanges::default(),
            tabline_rows: 0,
            msg_rows: 1,
            tab_layouts: IndexMap::new(),
            rex_layout: None,
            escape_timer_due: None,
            stopped: None,
            rex_watched: true,
            event_loop,
            rex,
            nvim,
        }
    }

    /// Adds the launcher's pane, already connected, and starts talking to
    /// Neovim and Rex.
    pub fn start(&mut self, launch_pane: Pane) -> Result<()> {
        // only ever called on startup
        debug_assert!(self.panes.list.is_empty());

        let pane = self.panes.add(launch_pane);
        pane.register_with(&self.event_loop)?;
        let view = &pane.terminal_surface.as_ref().unwrap().view;
        let (cols, rows) = (view.cols, view.rows);
        self.nvim.attach_ui(cols, rows)?;
        self.rex.attach()?;
        Ok(())
    }

    /// Runs the loop until something calls `stop`, returns why.
    pub fn run(&mut self, server: &UnixListener) -> Result<StopReason> {
        let mut events = [Event::default(); 16];
        while self.stopped.is_none() {
            let mut helper_waiting = false;
            let n = self.event_loop.wait(&mut events)?.len();
            for &e in &events[..n] {
                if Token::of(e.token) == Some(Token::HelperListener) {
                    helper_waiting = true;
                }
                self.handle(e)?;
                if self.stopped.is_some() {
                    break;
                }
            }
            // between batches, so no stale event reaches a new pane
            if helper_waiting
                && self.stopped.is_none()
                && let Err(err) = self.accept(server)
            {
                eprintln!("neorex daemon: a pane failed: {err}");
            }
            if self.stopped.is_none() {
                self.after_batch()?;
            }
        }
        Ok(self.stopped.unwrap())
    }

    /// Ends `run` once the current event is handled.
    fn stop(&mut self, reason: StopReason) {
        self.stopped = Some(reason);
    }

    /// Starts, or restarts, one of our timers.
    fn add_timer(&self, t: Token, ms: u32) -> Result<()> {
        Ok(self.event_loop.add_timer(t as usize, ms, t as usize)?)
    }

    /// Handles one loop event.
    fn handle(&mut self, e: Event) -> Result<()> {
        match Token::of(e.token) {
            Some(Token::Signal) => self.stop(StopReason::Signal),
            Some(Token::NvimSocket) => self.on_nvim()?,
            Some(Token::RexSocket) => self.on_rex()?,
            Some(Token::FocusTimer) => self.send_focus(true)?,
            Some(Token::LayoutTimer) => self.layout_wait_over()?,
            Some(Token::SwitchTimer) => self.recheck_switch()?,
            Some(Token::EscapeTimer) => self.flush_pending_escapes()?,
            Some(Token::HelperListener) => {}
            None => self.on_pane(e)?,
        }
        Ok(())
    }

    /// After each batch of events: tells Neovim whether any pane has focus.
    /// Losing it waits a moment, in case another pane gains it.
    fn after_batch(&mut self) -> Result<()> {
        self.send_focus(false)?;
        self.stop_watching_closed_rex()?;
        if self.nvim.watch_switch() {
            self.add_timer(Token::SwitchTimer, SWITCH_RECHECK_MS)?;
        }
        Ok(())
    }

    /// Starts the escape timer for the earliest deadline among the panes' waiting
    /// ESCs, unless it already runs for one that early: reads of other panes
    /// don't put an ESC's deadline back.
    fn arm_escape_timer(&mut self) -> Result<()> {
        let Some(due) = self
            .panes
            .list
            .iter()
            .filter_map(Pane::escape_deadline)
            .min()
        else {
            return Ok(());
        };
        if self.escape_timer_due.is_some_and(|armed| armed <= due) {
            return Ok(());
        }
        let now = Instant::now();
        // round up, so the timer doesn't fire before the deadline
        let ms = due
            .saturating_duration_since(now)
            .as_micros()
            .div_ceil(1000);
        self.escape_timer_due = Some(due);
        self.add_timer(Token::EscapeTimer, u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// The escape timer fired: an ESC still waiting in a pane whose deadline
    /// has passed is the Escape key. Tries again later for one held up by a
    /// window switch, and starts the timer again for ESCs not yet due.
    fn flush_pending_escapes(&mut self) -> Result<()> {
        self.escape_timer_due = None;
        let now = Instant::now();
        let ids: Vec<u32> = self
            .panes
            .list
            .iter()
            .filter(|p| p.escape_deadline().is_some_and(|d| d <= now))
            .map(|p| p.id)
            .collect();
        for id in ids {
            let mut fwds = PaneForwarders {
                panes: &mut self.panes,
                grids: &self.grids,
            };
            if !self.nvim.flush_pending(&mut fwds, id)? {
                self.escape_timer_due =
                    Some(now + Duration::from_millis(u64::from(input::ESCAPE_TIMEOUT_MS)));
                return self.add_timer(Token::EscapeTimer, input::ESCAPE_TIMEOUT_MS);
            }
        }
        self.arm_escape_timer()
    }

    fn recheck_switch(&mut self) -> Result<()> {
        if self.nvim.recheck_switch()? {
            self.add_timer(Token::SwitchTimer, SWITCH_RECHECK_MS)?;
        }
        Ok(())
    }

    fn send_focus(&mut self, waited: bool) -> Result<()> {
        let mut known = false;
        let mut any = false;
        for p in &self.panes.list {
            if let Some(f) = p
                .terminal_surface
                .as_ref()
                .and_then(|s| s.forwarder.focused)
            {
                known = true;
                any = any || f;
            }
        }
        if !known || any == self.nvim.ui_focused {
            return Ok(());
        }
        if !any && !waited {
            return self.add_timer(Token::FocusTimer, FOCUS_OUT_DELAY_MS);
        }
        self.nvim.ui_focused = any;
        let _ = self.nvim.client.request("nvim_ui_set_focus", 1);
        msgpack::write_bool(self.nvim.client.writer(), any);
        self.nvim.client.flush()?;
        Ok(())
    }

    // panes

    /// A new helper connected: match its hello to a pane waiting for it.
    fn accept(&mut self, server: &UnixListener) -> Result<()> {
        let hello = pane_helper::accept_hello(server)?;
        // its window may have closed before it connected
        let Some(id) = hello.id.filter(|&id| self.panes.by_id(id).is_some()) else {
            eprintln!("neorex daemon: a helper for a pane that's gone");
            hello.close();
            return Ok(());
        };
        // a second hello for a pane keeps the terminal it has
        if self
            .panes
            .by_id(id)
            .is_some_and(|p| p.terminal_surface.is_some())
        {
            eprintln!("neorex daemon: a second helper for pane {id}; refused");
            hello.close();
            return Ok(());
        }
        // (a failing `Surface::create` closes the helper's socket and terminal
        // itself)
        let surface = Surface::create(hello.stream, hello.tty)?;
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.terminal_surface = Some(surface);
        pane.register_with(&self.event_loop)?;
        if pane.awaiting_layout {
            let v = &pane.terminal_surface.as_ref().unwrap().view;
            if v.cols != REX_DEFAULT_SIZE[0] || v.rows != REX_DEFAULT_SIZE[1] {
                pane.awaiting_layout = false;
            } else {
                self.add_timer(Token::LayoutTimer, LAYOUT_WAIT_MS)?;
            }
        }
        self.size_window(id);
        if self.panes.by_id(id).unwrap().layer.is_some()
            && let Some(req) = self.floating().follow_layer(id)
            && let Some(l) = &mut self.panes.by_id_mut(id).unwrap().layer
        {
            l.fit = Fit::Asked(req);
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    /// The app never resized a new split: its terminal's size is the real one.
    fn layout_wait_over(&mut self) -> Result<()> {
        let mut any = false;
        for i in 0..self.panes.list.len() {
            let p = &mut self.panes.list[i];
            if p.awaiting_layout && p.terminal_surface.is_some() {
                p.awaiting_layout = false;
                any = true;
                let id = p.id;
                self.size_window(id);
            }
        }
        if !any {
            return Ok(());
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    // Decode pane info from token, get the pane object, forward to
    // terminal or helper_socket handler depending on which fd the event came from
    fn on_pane(&mut self, e: Event) -> Result<()> {
        let decoded = pane::decode_token(e.token);
        // pane might be gone
        if self.panes.by_id(decoded.id).is_none() {
            return Ok(());
        }
        match decoded.source_fd {
            PaneFd::HelperSocket => self.on_helper(decoded.id),
            PaneFd::Terminal if e.kind == Kind::Write => self.on_writable(decoded.id),
            PaneFd::Terminal => self.on_input(decoded.id),
        }
    }

    /// Pane `id`'s terminal has room for the output waiting for it. Once it
    /// has taken it all, a pane that had frames dropped is drawn in full.
    fn on_writable(&mut self, id: u32) -> Result<()> {
        let s = self
            .panes
            .by_id_mut(id)
            .and_then(|p| p.terminal_surface.as_deref_mut())
            .expect("a pane with events has a terminal");
        let fd = s.terminal.fd;
        if s.backlog.drain(fd).is_err() {
            return self.pane_gone(id);
        }
        if !s.backlog.is_empty() {
            return Ok(());
        }
        let repaint = s.backlog.take_dropped();
        if repaint {
            s.view.full = true;
        }
        self.event_loop.remove_write(fd)?;
        if repaint {
            self.draw()?;
        }
        Ok(())
    }

    fn on_helper(&mut self, id: u32) -> Result<()> {
        let mut buf = [0u8; 64];
        let fd = self.surface_of(id).helper_fd();
        let n = read_fd(fd, &mut buf);
        if n == 0 {
            return self.pane_gone(id);
        }

        for &byte in &buf[..n] {
            match pane_helper::Message::from_byte(byte) {
                pane_helper::Message::Resized => return self.handle_resize(id),
                pane_helper::Message::Hello | pane_helper::Message::Other(_) => {}
            }
        }
        Ok(())
    }

    fn handle_resize(&mut self, id: u32) -> Result<()> {
        let Ok(window_size) = self.surface_of(id).terminal.size() else {
            return self.pane_gone(id);
        };
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.awaiting_layout = false;
        let surface = pane.terminal_surface.as_mut().unwrap();
        surface
            .view
            .resize(u32::from(window_size.ws_col), u32::from(window_size.ws_row));
        self.size_window(id);
        let _ = self.floating().follow_layer(id);
        if self.focused_pane_id() == Some(id) {
            self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    fn on_input(&mut self, id: u32) -> Result<()> {
        let mut buf = [0u8; 4096];
        let fd = self.surface_of(id).terminal.fd;
        let n = read_fd(fd, &mut buf);
        if n == 0 {
            return self.pane_gone(id);
        }

        let bytes = &buf[..n];
        let pane = self.panes.by_id(id).unwrap();
        let (win, block) = (pane.neovim_window_handle, pane.rex_block);
        let rex_focused = self.rex.has_focused(block);
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        if self
            .nvim
            .takes_focus(&mut fwds, id, win, rex_focused, bytes)
        {
            self.make_current(id)?;
            self.rex.focus(block)?;
        }
        // behind the switch above, if any: sent straight, the keys would reach the old window
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        self.nvim.type_into(&mut fwds, id, bytes)?;
        self.arm_escape_timer()
    }

    /// Pane `id`'s surface, which a pane that gets fd events has.
    fn surface_of(&self, id: u32) -> &Surface {
        self.panes
            .by_id(id)
            .and_then(|p| p.terminal_surface.as_deref())
            .expect("a pane with events has a terminal")
    }

    fn new_pane(&mut self, win: i64) -> Result<()> {
        if self.rex.conn == Conn::Closed {
            return Ok(()); // no Rex: the window stays in Neovim
        }
        let sp = self.changes.split_of(win);
        // beside the window it came from, else any pane Rex knows; None for a
        // tab page's floating pane
        let anchor: Option<RexBlock> = if sp.tab != 0 {
            match self.tab_anchor(sp) {
                Some(b) => Some(b),
                None => {
                    // its tab page's floating pane is on its way: split that once
                    // it's made, or ask for its own if the request failed
                    if self.tab_floating_pane_pending(sp.tab) {
                        return Ok(());
                    }
                    None
                }
            }
        } else {
            let from = self
                .split_from(sp)
                .filter(|f| f.tab == 0)
                .and_then(|f| f.rex_block);
            let any = || {
                self.panes
                    .list
                    .iter()
                    .filter(|q| q.layer.is_none() && q.tab == 0)
                    .find_map(|q| q.rex_block)
            };
            match from.or_else(any) {
                Some(b) => Some(b),
                None => return Ok(()), // no anchor yet: next flush
            }
        };
        // starts at the guess, not Rex's default
        let size = self.new_pane_size(win);
        let pane = self.panes.add_for_window(win);
        pane.tab = sp.tab;
        let pane_id = pane.id;
        self.changes.forget_split(win);

        let id = pane_id.to_string();
        let block = json!({
            "flavor": "com.superlogical.terminal.shell",
            "label": "neorex",
            "options": {
                "shell": "none",
                "command": [self.exe, "pane", self.sock, id],
                "size": { "columns": size[0], "rows": size[1] },
            },
        });
        let Some(a) = anchor else {
            self.rex
                .new_layer(pane_id, TAB_LAYER_BOUNDS, json!({ "block": block }))?;
            return Ok(());
        };
        self.panes.by_id_mut(pane_id).unwrap().awaiting_layout = true;
        self.rex.request(
            Request::NewSplit(pane_id),
            "session.new_split",
            json!({
                "session_id": self.rex.session_id,
                "anchor_id": block_str(&a),
                "direction": if sp.side_by_side { "horizontal" } else { "vertical" },
                "side": if sp.before { "before" } else { "after" },
                "layout": { "block": block },
            }),
        )?;
        Ok(())
    }

    /// Where a window of a tab page made after setup splits: beside a pane of
    /// its tab page, the one it came from if it can. Neovim shows the tab page
    /// over every other window, so they share a Rex floating pane over theirs;
    /// None for the first.
    fn tab_anchor(&self, sp: windows::Split) -> Option<RexBlock> {
        if let Some(f) = self.panes.by_win(sp.from)
            && f.tab == sp.tab
            && let Some(b) = f.rex_block
        {
            return Some(b);
        }
        self.panes
            .list
            .iter()
            .filter(|q| q.tab == sp.tab && q.layer.is_none())
            .find_map(|q| q.rex_block)
    }

    /// True while a pane of tab page `tab` asked Rex for its floating pane
    /// and has no answer: a refused request is no longer on its way.
    fn tab_floating_pane_pending(&self, tab: i64) -> bool {
        self.panes
            .list
            .iter()
            .any(|q| q.tab == tab && q.layer.is_none() && q.closing != Some(Close::RequestFailed))
    }

    /// The helper or terminal went away: Rex closed the pane. Close its
    /// window, or end if it was the last pane.
    fn pane_gone(&mut self, id: u32) -> Result<()> {
        let i = self.panes.index_of(id).unwrap();
        let p = &mut self.panes.list[i];
        p.closing = Some(Close::RexPaneClosed);
        if let Some(l) = p.layer {
            // closed in Rex: close its floats, which the plugin notices
            self.panes.remove(i, &self.event_loop);
            for &top in l.group.top_grids() {
                if let Some(w) = self.grids.win_on(top) {
                    windows::close(&mut self.nvim.client, w.win);
                }
            }
            self.nvim.client.flush()?;
            return Ok(());
        }
        let others =
            self.panes.list.iter().any(|q| {
                q.id != id && q.closing != Some(Close::RequestFailed) && q.layer.is_none()
            });
        if !others {
            self.stop(StopReason::PaneGone);
            return Ok(());
        }
        let maybe_win = self.panes.list[i].neovim_window_handle;
        self.panes.remove(i, &self.event_loop);
        let Some(win) = maybe_win else {
            return Ok(());
        };
        self.changes.mark_closing(win);
        windows::close(&mut self.nvim.client, win);
        self.nvim.client.flush()?;
        Ok(())
    }

    // Neovim

    fn on_nvim(&mut self) -> Result<()> {
        if !self.nvim.client.read()? {
            self.stop(StopReason::NvimExited);
            return Ok(());
        }
        while let Some(frame) = self.nvim.client.next()? {
            match frame.message()? {
                rpc::Message::Notification { method, mut params } => {
                    if method == b"redraw" {
                        if self.grids.apply_redraw(&mut params)? {
                            self.reconcile()?;
                            self.draw()?;
                        }
                    } else if let Some(note) = parse_note_or_log(method, &mut params) {
                        self.on_note(note)?;
                    }
                }
                rpc::Message::Request { id, .. } => {
                    self.nvim
                        .client
                        .respond_error(id, "neorex serves no requests")?;
                }
                rpc::Message::Response { id, err, result } => {
                    self.on_response(id, err, result)?;
                }
            }
        }
        Ok(())
    }

    /// One of the Lua's notes.
    fn on_note(&mut self, note: windows::Note) -> Result<()> {
        match note {
            windows::Note::Current(win) => self.on_current(win)?,
            windows::Note::Float(win) => self.on_current_float(win)?,
            windows::Note::Entered(win) => self.on_entered_float(win)?,
            windows::Note::Tabline(rows) => self.on_tabline(rows.clamp(0, 1) as u32)?,
            windows::Note::Cmdheight(rows) => {
                self.on_cmdheight(rows.min(i64::from(MAX_MSG_ROWS)) as u32)?
            }
            windows::Note::Direction(m) => self.on_direction(m)?,
            windows::Note::Reopen(win) => {
                if self.panes.by_win(win).is_some() {
                    self.changes.mark_reopening(win);
                }
            }
            windows::Note::Reopened { old, new } => self.on_reopened(old, new),
            windows::Note::Split(sp) => {
                if self.panes.by_win(sp.win).is_none() {
                    self.changes.note_split(sp);
                }
            }
            windows::Note::Layout(l) => self.on_tab_layout(l)?,
        }
        Ok(())
    }

    /// Neovim answered request `id`.
    fn on_response(&mut self, id: u32, err: Option<&[u8]>, result: Decoder<'_>) -> Result<()> {
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        self.nvim.answered(&mut fwds, id, err, result)?;
        // held input may have gone through and left an ESC waiting
        self.arm_escape_timer()?;
        let mut show_now = false;
        for p in self.panes.list.iter_mut() {
            if let Some(l) = &mut p.layer
                && l.fit == Fit::Asked(id)
            {
                // nothing moved, so no redraw is coming: show it now
                let mut res = result;
                let moved = err.is_none() && rpc::expect_bool(&mut res).unwrap_or(false);
                l.fit = if moved { Fit::Answered } else { Fit::None };
                show_now = !moved;
            }
        }
        if show_now {
            self.draw()?;
        }
        if let Some(e) = err {
            eprintln!("neovim error: {}", String::from_utf8_lossy(e));
        } else if id == self.nvim.channel_req {
            let size = self
                .geometry()
                .window_size(self.panes.by_id(STARTUP_PANE_ID).unwrap());
            let mut result = result;
            let chan = windows::parse_channel(&mut result)?;
            self.nvim.setup_req = windows::setup(&mut self.nvim.client, chan, size[0], size[1]);
            self.nvim.client.flush()?;
        } else if id == self.nvim.setup_req {
            let mut result = result;
            let wins = windows::parse_setup(&mut result)?;
            if let Some(launch) = self.panes.by_id_mut(STARTUP_PANE_ID) {
                launch.neovim_window_handle = Some(wins[0]);
            }
            self.nvim.ready = true;
            // its window is in its pane now: the editor can span the Rex window
            if let Some(f) = self.focused_pane_id() {
                self.nvim.fit_ui_to(self.panes.by_id(f).unwrap())?;
            }
            self.reconcile()?;
            self.draw()?;
        }
        Ok(())
    }

    /// Neovim's current window changed: focus its pane in Rex.
    fn on_current(&mut self, win: i64) -> Result<()> {
        // sent before our own change ran; the report after it is coming
        if self.nvim.switch_in_flight().is_some() {
            return Ok(());
        }
        self.nvim.current_float = 0;
        self.nvim.current_win = win;
        let Some(p) = self.panes.by_win(win) else {
            return Ok(());
        };
        let id = p.id;
        self.nvim.fit_ui_to(p)?;
        self.draw()?;
        // (looked up again: drawing can remove a pane whose terminal is gone)
        let block = self.panes.by_id(id).and_then(|p| p.rex_block);
        self.rex.focus(block)?;
        Ok(())
    }

    /// `<C-w>h/j/k/l`: Rex moves focus to the pane that way, if any. Its view
    /// brings the new focus back, and Neovim's current window follows.
    fn on_direction(&mut self, m: windows::Move) -> Result<()> {
        let dir = match m.dir {
            windows::Direction::Left => rex_session::Direction::Left,
            windows::Direction::Down => rex_session::Direction::Down,
            windows::Direction::Up => rex_session::Direction::Up,
            windows::Direction::Right => rex_session::Direction::Right,
        };
        for _ in 0..m.count.min(MAX_DIRECTION_COUNT) {
            self.rex.focus_direction(dir)?;
        }
        Ok(())
    }

    /// A float became current: if it's in a floating pane, focus that.
    fn on_current_float(&mut self, win: i64) -> Result<()> {
        self.nvim.current_float = win;
        let Some(p) = floating_panes::layer_of_win(&self.grids, &self.panes, win) else {
            return Ok(());
        };
        let block = p.rex_block;
        self.rex.focus(block)?;
        Ok(())
    }

    /// A float took focus. Usually before it's drawn, so the next flush gives
    /// its group a floating pane; if it's already showing in a pane, now.
    fn on_entered_float(&mut self, win: i64) -> Result<()> {
        self.nvim.note_entered(win);
        if !self.nvim.ready
            || self.grids.grid_of(win).is_none()
            || floating_panes::layer_of_win(&self.grids, &self.panes, win).is_some()
        {
            return Ok(());
        }
        self.floating().reconcile_layers()?;
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Matches panes to windows: new external windows get panes, ordinary ones
    /// are made external, and panes whose windows closed hang up.
    fn reconcile(&mut self) -> Result<()> {
        if !self.nvim.ready {
            return Ok(());
        }
        // this flush has the refitted floats
        for p in self.panes.list.iter_mut() {
            if let Some(l) = &mut p.layer
                && l.fit == Fit::Answered
            {
                l.fit = Fit::None;
            }
        }
        let mut i = self.panes.list.len();
        while i > 0 {
            i -= 1;
            let p = &mut self.panes.list[i];
            let Some(win) = p.neovim_window_handle else {
                continue;
            };
            if self.grids.grid_of(win).is_some() {
                continue;
            }
            // its pane waits for the window that reopens it to be drawn
            if self.changes.awaits_reopen(win) {
                continue;
            }
            p.closing = Some(Close::WindowClosed);
            // hanging up closes the Rex pane only once its helper has connected
            if p.terminal_surface.is_none()
                && let Some(b) = p.rex_block
            {
                self.rex.close_block(&block_str(&b))?;
            }
            self.panes.remove(i, &self.event_loop);
        }
        if self.panes.list.is_empty() {
            self.stop(StopReason::PaneGone);
            return Ok(());
        }
        self.changes.forget_gone(&self.grids);

        // a copy: making panes below doesn't touch the grids, but borrows all of `self`
        let wins: Vec<_> = self.grids.wins.values().copied().collect();
        for w in wins {
            match w.kind {
                WinKind::Ordinary => {
                    if !self.changes.should_adopt(w.win) {
                        continue;
                    }
                    let size = match self.panes.by_win(w.win) {
                        Some(p) => self.geometry().window_size(p),
                        None => self.new_window_size(w.win),
                    };
                    windows::adopt(&mut self.nvim.client, w.win, size[0], size[1]);
                }
                WinKind::External => {
                    self.changes.became_external(w.win);
                    if self.panes.by_win(w.win).is_some() || self.changes.is_closing(w.win) {
                        continue;
                    }
                    self.new_pane(w.win)?;
                }
                WinKind::Float | WinKind::Hidden => {}
            }
        }
        self.size_windows();
        self.floating().reconcile_layers()?;
        self.place_tabs()?;
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Neovim has a new layout for a tab page's windows, or none.
    fn on_tab_layout(&mut self, layout: Layout) -> Result<()> {
        if layout.len == 0 {
            self.tab_layouts.swap_remove(&layout.tab);
            return Ok(());
        }
        match self.tab_layouts.get_mut(&layout.tab) {
            Some(tab) => tab.replace(layout),
            None => {
                self.tab_layouts.insert(layout.tab, Tab::new(layout));
            }
        }
        self.place_tabs()
    }

    /// Puts the panes of each tab page's windows where its layout has them,
    /// once they all have a Rex block, and again when the windows, their order
    /// or their sizes change. Rex has them in the order their windows were made,
    /// which isn't where Neovim ends up putting them.
    fn place_tabs(&mut self) -> Result<()> {
        if self.rex.conn == Conn::Closed {
            return Ok(());
        }
        let mut i = self.tab_layouts.len();
        while i > 0 {
            i -= 1;
            let tab = &mut self.tab_layouts[i];
            let mut states = [WindowState::Skipped; Layout::MAX_NODES];
            let layout = tab.layout;
            for (at, node) in layout.items().iter().enumerate() {
                states[at] = if node.kind == windows::Kind::Window {
                    observe_window(&self.grids, &self.panes, &self.changes, tab, at, node.win)
                } else {
                    WindowState::Skipped
                };
            }
            match tab_placement::decide(&tab.layout, &states[..tab.layout.len], tab.placed.as_ref())
            {
                Action::Wait | Action::Nothing => {}
                Action::Forget => {
                    self.tab_layouts.swap_remove_index(i);
                }
                Action::Rearrange(plan) => {
                    tab.placed = Some(plan);
                    for m in plan.items() {
                        let Some(blocks) = self.blocks_of(m) else {
                            continue;
                        };
                        self.rex.move_block(
                            &block_str(&blocks.moved),
                            &block_str(&blocks.anchor),
                            m.side_by_side,
                            m.ratio,
                        )?;
                    }
                }
                Action::Resize(plan) => {
                    tab.placed = Some(plan);
                    for m in plan.items() {
                        let Some(blocks) = self.blocks_of(m) else {
                            continue;
                        };
                        self.rex.resize_pane(
                            &block_str(&blocks.anchor),
                            &block_str(&blocks.moved),
                            m.side_by_side,
                            m.ratio,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The Rex blocks of the panes a move is between, if both have one.
    fn blocks_of(&self, m: &PaneMove) -> Option<Blocks> {
        let moved = self.panes.by_win(m.win)?.rex_block?;
        let anchor = self.panes.by_win(m.anchor)?.rex_block?;
        Some(Blocks { moved, anchor })
    }

    /// Window `new` reopened `old`, closing: it takes `old`'s pane, which
    /// waits for it to be drawn, then sizes it to the pane.
    fn on_reopened(&mut self, old: i64, new: i64) {
        if !self.changes.take_reopening(old) {
            return;
        }
        self.changes.mark_reopened(new);
        let Some(p) = self.panes.by_win_mut(old) else {
            return;
        };
        p.neovim_window_handle = Some(new);
        p.last_asked_win_size = LastAskedWinSize::default();
        if self.nvim.current_win == old {
            self.nvim.current_win = new;
        }
    }

    /// A guess until the new pane says its size: half the pane it splits.
    fn new_window_size(&self, win: i64) -> [u32; 2] {
        let size = self.new_pane_size(win);
        surface::window_fitting(size[0], size[1], self.msg_rows)
    }

    /// A guess at a new split's terminal size, cols x rows: half the pane it splits.
    fn new_pane_size(&self, win: i64) -> [u32; 2] {
        let sp = self.changes.split_of(win);
        // a tab page's first: its floating pane's size
        let first_of_tab = sp.tab != 0 && self.tab_anchor(sp).is_none();
        self.geometry()
            .new_pane_size(sp, self.split_from(sp), first_of_tab)
    }

    /// The pane a new window splits: its origin's, else the focused one.
    fn split_from(&self, sp: windows::Split) -> Option<&Pane> {
        if sp.from != 0
            && let Some(p) = self.panes.by_win(sp.from)
        {
            return Some(p);
        }
        self.focused_pane()
    }

    // Rex

    fn on_rex(&mut self) -> Result<()> {
        self.rex.read();
        self.drain_rex()?;
        self.stop_watching_closed_rex()
    }

    /// Rex is closed, however it came to be: stop watching its fd, and fail
    /// the requests still waiting on it.
    fn stop_watching_closed_rex(&mut self) -> Result<()> {
        if self.rex.conn == Conn::Closed && self.rex_watched {
            self.rex_watched = false;
            self.event_loop.remove(self.rex.client.fd())?;
            self.drain_rex()?;
        }
        Ok(())
    }

    fn drain_rex(&mut self) -> Result<()> {
        while let Some(update) = self.rex.next()? {
            match update {
                rex_session::Update::SplitMade { pane, block } => {
                    self.on_split_made(pane, &block)?
                }
                // a floating pane that failed leaves its floats drawn in the pane
                rex_session::Update::SplitFailed(failed)
                | rex_session::Update::LayerFailed(failed) => {
                    if let Some(p) = self.panes.by_id_mut(failed.pane) {
                        p.closing = Some(Close::RequestFailed);
                        // windows of its tab page that waited for it ask for their own
                        self.reconcile()?;
                        self.draw()?;
                    }
                }
                rex_session::Update::LayerMade { pane, block, layer } => {
                    self.on_layer_made(pane, &block, layer.as_deref())?
                }
                rex_session::Update::Layout(wins) => self.on_layout(&wins)?,
                rex_session::Update::FocusMoved(block) => self.on_rex_focus(&block)?,
            }
        }
        Ok(())
    }

    /// Rex made the pane's split: link the block and let Rex size it.
    fn on_split_made(&mut self, pane_id: u32, block: &str) -> Result<()> {
        // its window closed while Rex made it: nothing will draw there
        let Some(p) = self.panes.by_id_mut(pane_id) else {
            self.rex.close_block(block)?;
            return Ok(());
        };
        p.rex_block = parse_block(block);
        let (handle, rex_block) = (p.neovim_window_handle, p.rex_block);
        self.rex.disown(block)?;
        if handle == Some(self.nvim.current_win) {
            self.rex.focus(rex_block)?;
        }
        self.place_tabs()
    }

    /// Rex made the floating pane's layer: link it, and focus it if its float is current.
    fn on_layer_made(&mut self, pane_id: u32, block: &str, layer: Option<&str>) -> Result<()> {
        // its floats closed while Rex made it: nothing will draw there
        let Some(p) = self.panes.by_id_mut(pane_id) else {
            match layer {
                Some(id) => self.rex.close_layer(id)?,
                None => self.rex.close_block(block)?,
            }
            return Ok(());
        };
        p.rex_block = parse_block(block);
        let (handle, rex_block, floating) =
            (p.neovim_window_handle, p.rex_block, p.layer.is_some());
        self.rex.disown(block)?;
        // a tab page's window: closing its last block closes the layer
        if !floating {
            if handle == Some(self.nvim.current_win) {
                self.rex.focus(rex_block)?;
            }
            // its tab page's other windows split it
            self.reconcile()?;
            return self.draw();
        }
        if let Some(id) = layer
            && let Some(l) = &mut self.panes.by_id_mut(pane_id).unwrap().layer
        {
            l.id = pane::rex_layer(id.as_bytes()).ok();
        }
        if self.nvim.current_float != 0
            && floating_panes::layer_of_win(&self.grids, &self.panes, self.nvim.current_float)
                .map(|p| p.id)
                == Some(pane_id)
        {
            self.rex.focus(rex_block)?;
        }
        Ok(())
    }

    /// The user focused one of our panes in Rex: make its window current.
    fn on_rex_focus(&mut self, block: &str) -> Result<()> {
        let Some(p) = self.panes.by_block(block.as_bytes()) else {
            return Ok(());
        };
        let id = p.id;
        if let Some(l) = &p.layer {
            // clicked into a floating pane: into one of its floats, if not there
            if self.nvim.current_float != 0
                && floating_panes::layer_of_win(&self.grids, &self.panes, self.nvim.current_float)
                    .map(|q| q.id)
                    == Some(id)
            {
                return Ok(());
            }
            let Some(win) = floating_panes::focusable_in(&self.grids, &l.group) else {
                return Ok(());
            };
            self.nvim.current_float = win;
            self.nvim.switch_to_float(id, win)?;
            return Ok(());
        }
        let Some(win) = p.neovim_window_handle else {
            return Ok(());
        };
        // back from a float to the window under it counts as a move
        if win == self.nvim.current_win && self.nvim.current_float == 0 {
            return Ok(());
        }
        self.nvim.current_float = 0;
        self.make_current(id)?;
        self.nvim.client.flush()?;
        self.draw()
    }

    /// A newer view: where our panes sit, so the editor and floats fit. A view
    /// that says what the last did, to panes that are as it left them, changes
    /// nothing to size or draw.
    fn on_layout(&mut self, wins: &[Value]) -> Result<()> {
        let snapshot = rex_layout::read(wins);
        let changed = self.apply_layout(&snapshot);
        if !changed && self.rex_layout.as_ref() == Some(&snapshot) {
            return Ok(());
        }
        self.rex_layout = Some(snapshot);
        // a pane moved to or from the top: the tabline's over it, or not
        self.size_windows();
        if let Some(f) = self.focused_pane_id() {
            self.nvim.fit_ui_to(self.panes.by_id(f).unwrap())?;
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Gives our panes their rects, remembers which Rex window holds them, and
    /// has the floats of a floating pane the user moved follow it. Whether
    /// any pane or the window differed from it.
    fn apply_layout(&mut self, snapshot: &LayoutSnapshot) -> bool {
        let mut changed = false;
        for (block, rect) in &snapshot.rects {
            if let Some(p) = self.panes.by_block_mut(block.as_bytes())
                && p.rect != Some(*rect)
            {
                p.rect = Some(*rect);
                changed = true;
            }
        }
        for w in &snapshot.windows {
            let ours = w
                .blocks
                .iter()
                .any(|b| self.panes.by_block(b.as_bytes()).is_some());
            if ours
                && let Some(id) = w.id
                && self.rex.window != Some(id)
            {
                self.rex.window = Some(id);
                changed = true;
            }
        }
        for l in &snapshot.layer_bounds {
            changed |= self.floating().follow_moved_layer(l);
        }
        changed
    }

    /// The borrows of the floating panes' lifecycle (see `floating_panes`).
    fn floating(&mut self) -> FloatingPanes<'_> {
        FloatingPanes {
            exe: &self.exe,
            sock: &self.sock,
            grids: &self.grids,
            panes: &mut self.panes,
            event_loop: &self.event_loop,
            rex: &mut self.rex,
            nvim: &mut self.nvim,
        }
    }

    // shared

    /// Draws every pane that changed.
    fn draw(&mut self) -> Result<()> {
        let focused = self.focused_pane_id();
        // floats in floating panes stay out of the others
        let mut skip: Vec<u32> = Vec::with_capacity(SKIP_MAX);
        for p in &self.panes.list {
            if !p.layer_hides() {
                continue;
            }
            for &t in p.layer.as_ref().unwrap().group.top_grids() {
                if skip.len() == SKIP_MAX {
                    break;
                }
                skip.push(t);
            }
        }
        // the screen's tabline: grid 1's top row
        let tabline_dirty = self
            .grids
            .get(1)
            .is_some_and(|g| g.height > 0 && (g.dirty.is_set(0) || g.scrolls_len > 0));
        let mut i = self.panes.list.len();
        while i > 0 {
            i -= 1;
            // backwards, so removing this pane leaves the ones still to draw in place
            let p = &self.panes.list[i];
            if p.terminal_surface.is_none() {
                continue;
            }
            let id = p.id;
            // a pane that closes in this loop leaves the panes drawn before it, to its right, on their old columns for this frame: the next draw finds their start moved and redraws the row
            let tabline = self.prepare_tabline(id);
            let p = &self.panes.list[i];
            // a floating pane waiting on its refit shows nothing yet
            if p.layer.is_some() && !p.layer_shown() {
                continue;
            }
            // frames were dropped for a terminal that wasn't reading: it's
            // drawn in full once it has caught up
            if p.terminal_surface.as_ref().unwrap().backlog.dropped() {
                continue;
            }
            let drawn = if let Some(l) = p.layer {
                let members = float_groups::members(&self.grids, &l.group);
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                // the group's own editor float, even when the cap on members dropped it
                s.forwarder.grid = l.group.top_grids().first().copied().unwrap_or(1);
                s.view.frame_floats(
                    &mut self.grids,
                    &members,
                    [l.group.row, l.group.col],
                    &mut s.terminal.writer,
                )
            } else {
                let grid = geometry::grid_of_pane(&self.grids, p);
                let origin = self.geometry().pane_origin(p);
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                s.view.frame(
                    &mut self.grids,
                    grid,
                    Opts {
                        focused: focused == Some(id),
                        origin,
                        skip: &skip,
                        tabline: tabline.cells.as_deref(),
                        tabline_col: tabline.col,
                        tabline_dirty,
                    },
                    &mut s.terminal.writer,
                )
            };
            let p = &mut self.panes.list[i];
            let token = p.token(PaneFd::Terminal);
            let s: &mut Surface = p.terminal_surface.as_deref_mut().unwrap();
            set_targets(&mut s.forwarder, s.view.drawn_floats());
            if !drawn {
                continue;
            }
            // never waits: what the terminal can't take yet waits until it has room
            match s.send_frame() {
                Err(_) => self.pane_gone(id)?,
                Ok(true) => self.event_loop.add_write(s.terminal.fd, token)?,
                Ok(false) => {}
            }
        }
        // the panes along the top drew the tabline
        if tabline_dirty && let Some(g) = self.grids.get_mut(1) {
            g.dirty.unset(0);
            g.scrolls_len = 0;
        }
        // drawn into one pane at most; done with them
        if let Some(id) = self.grids.msg_grid
            && let Some(m) = self.grids.get_mut(id)
        {
            m.dirty.unset_all();
            m.scrolls_len = 0;
        }
        for (id, w) in &self.grids.wins {
            if w.kind == WinKind::Float
                && let Some(f) = self.grids.grids.get_mut(id)
            {
                f.dirty.unset_all();
                f.scrolls_len = 0;
            }
        }
        Ok(())
    }

    /// Asks Neovim for the window to fit the pane, unless it already was.
    fn size_window(&mut self, id: u32) {
        let Some(p) = self.panes.by_id(id) else {
            return;
        };
        if p.terminal_surface.is_none() || p.awaiting_layout {
            return;
        }
        let Some(win) = p.neovim_window_handle else {
            return;
        };
        let Some(grid) = self.grids.grid_of(win) else {
            return;
        };
        let size = self.geometry().window_size(p);
        let last = p.last_asked_win_size;
        if last.grid == grid && last.cols == size[0] && last.rows == size[1] {
            return;
        }
        self.panes.by_id_mut(id).unwrap().last_asked_win_size = LastAskedWinSize {
            grid,
            cols: size[0],
            rows: size[1],
        };
        windows::size(&mut self.nvim.client, win, size[0], size[1]);
    }

    /// `size_window` for every pane.
    fn size_windows(&mut self) {
        for i in 0..self.panes.list.len() {
            let id = self.panes.list[i].id;
            self.size_window(id);
        }
    }

    /// Makes the pane's window Neovim's current one, sized to the pane.
    fn make_current(&mut self, id: u32) -> Result<()> {
        let win = self.panes.by_id(id).unwrap().neovim_window_handle.unwrap();
        self.nvim.make_current(id, win)?;
        self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        Ok(())
    }

    /// The screen and pane geometry, read off the daemon's parts.
    fn geometry(&self) -> Geometry<'_> {
        Geometry {
            grids: &self.grids,
            panes: &self.panes,
            tabline_rows: self.tabline_rows,
            msg_rows: self.msg_rows,
        }
    }

    /// The pane showing Neovim's current window; before setup, the launcher's.
    fn focused_pane(&self) -> Option<&Pane> {
        if self.nvim.current_win == 0 {
            return self.panes.by_id(STARTUP_PANE_ID);
        }
        self.panes.by_win(self.nvim.current_win)
    }

    fn focused_pane_id(&self) -> Option<u32> {
        self.focused_pane().map(|p| p.id)
    }

    /// What pane `id` draws of the screen's tabline: its slice of grid 1's top
    /// row, and the column it starts at. Also points the pane's mouse events on
    /// that row at the same cells.
    fn prepare_tabline(&mut self, id: u32) -> Tabline {
        let Some(p) = self.panes.by_id(id) else {
            return Tabline {
                cells: None,
                col: 0,
            };
        };
        let Some(s) = &p.terminal_surface else {
            return Tabline {
                cells: None,
                col: 0,
            };
        };
        let found = self.geometry().tabline_start(p);
        let col = found.unwrap_or(0);
        let cells = match found {
            Some(_) => self
                .grids
                .get(1)
                .map(|g| tabline_slice(g, col, s.view.cols)),
            None => None,
        };
        let s = self
            .panes
            .by_id_mut(id)
            .unwrap()
            .terminal_surface
            .as_mut()
            .unwrap();
        s.forwarder.tabline_rows = if cells.is_some() { 1 } else { 0 };
        s.forwarder.tabline_col = col.min(u32::from(u16::MAX)) as u16;
        Tabline { cells, col }
    }

    /// The tabline came or went: the panes under it fit their windows again.
    fn on_tabline(&mut self, rows: u32) -> Result<()> {
        if rows == self.tabline_rows {
            return Ok(());
        }
        self.tabline_rows = rows;
        self.refit_windows()
    }

    /// 'cmdheight' changed: every window fits its pane again, now that the rows
    /// kept for messages are others.
    fn on_cmdheight(&mut self, rows: u32) -> Result<()> {
        if rows == self.msg_rows {
            return Ok(());
        }
        self.msg_rows = rows;
        self.refit_windows()
    }

    /// Every window is sized to its pane again, and drawn.
    fn refit_windows(&mut self) -> Result<()> {
        self.size_windows();
        self.nvim.client.flush()?;
        self.draw()
    }
}

/// How far window `win`, node `at` of `tab`'s layout, has got toward a pane;
/// notes down in `tab` that it was drawn. A window that's undrawn keeps its
/// tab page waiting, with no time limit: Neovim draws a window it made, or
/// tells us the layout without it once it's gone. (It takes the daemon's fields
/// it reads, since `tab` is borrowed from it.)
fn observe_window(
    grids: &Grids,
    panes: &Panes,
    changes: &WindowChanges,
    tab: &mut Tab,
    at: usize,
    win: i64,
) -> WindowState {
    if grids.grid_of(win).is_none() {
        return if tab.seen.is_set(at) {
            WindowState::Gone
        } else {
            WindowState::Undrawn
        };
    }
    tab.seen.set(at);
    let Some(p) = panes.by_win(win) else {
        return if changes.is_closing(win) || changes.is_reopening(win) {
            WindowState::Skipped
        } else {
            WindowState::Pending
        };
    };
    if p.closing.is_some() {
        return WindowState::Skipped;
    }
    if p.rex_block.is_none() {
        WindowState::Pending
    } else {
        WindowState::Ready
    }
}

/// A note, or None for one that's no note of ours or we can't read: a
/// message we don't understand shouldn't end the daemon.
fn parse_note_or_log(method: &[u8], params: &mut Decoder<'_>) -> Option<windows::Note> {
    match windows::parse_note(method, params) {
        Ok(note) => note,
        Err(err) => {
            eprintln!(
                "neorex daemon: unreadable {} note: {err}",
                String::from_utf8_lossy(method)
            );
            None
        }
    }
}

/// A block id from Rex, or None (logged) for one that isn't.
fn parse_block(block: &str) -> Option<RexBlock> {
    match pane::rex_block(block.as_bytes()) {
        Ok(b) => Some(b),
        Err(_) => {
            eprintln!("neorex daemon: not a block id: {block}");
            None
        }
    }
}

/// A Rex id as the string Rex knows it by.
pub(super) fn block_str(id: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(id)
}

/// Reads what's there from `fd`; 0 at its end, or if the read failed.
fn read_fd(fd: RawFd, buf: &mut [u8]) -> usize {
    loop {
        // SAFETY: `buf` is valid for `buf.len()` bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return n as usize;
        }
        if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return 0;
        }
    }
}

/// Grid 1's top row from column `col`, `cols` wide. A wide character cut at
/// either edge leaves a blank.
fn tabline_slice(screen: &Grid, col: u32, cols: u32) -> Vec<Cell> {
    let row = screen.row(0);
    let from = (col as usize).min(row.len());
    let n = (row.len() - from).min(cols as usize).min(TABLINE_MAX);
    let mut buf = row[from..from + n].to_vec();
    blank_cut_wide(&mut buf, row.get(from + n));
    if n > 0 && buf[0].len == 0 {
        buf[0] = Cell {
            hl: buf[0].hl,
            ..Cell::default()
        };
    }
    buf
}

/// Mouse events on the floats drawn go to their grids.
fn set_targets(f: &mut Forwarder, rects: &[Rect]) {
    let n = rects.len().min(forwarder::MAX_TARGETS);
    let targets: Vec<forwarder::Target> = rects[..n]
        .iter()
        .map(|r| forwarder::Target {
            grid: r.grid,
            row: r.row,
            col: r.col,
            width: r.width,
            height: r.height,
        })
        .collect();
    f.set_targets(&targets);
}

#[cfg(test)]
mod slow_terminal_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::nvim::for_test_on;
    use crate::nvim::windows::{Kind, Node};
    use crate::rex::client::Client;
    use crate::screen::float_groups::Group;
    use crate::screen::grids::{Float, Win};
    use serde_json::json;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    /// A daemon with no Neovim or Rex behind it, only sockets that nothing
    /// answers. `rex_end` reads what it sends Rex.
    struct Fixture {
        d: Daemon,
        rex_end: UnixStream,
        nvim_end: UnixStream,
    }

    fn fixture() -> Fixture {
        let (nvim_ours, nvim_end) = UnixStream::pair().unwrap();
        let (rex_ours, rex_end) = UnixStream::pair().unwrap();
        let mut rex = RexSession::new(Client::from_stream(rex_ours), "session:s".to_string());
        rex.conn = Conn::Attached;
        let d = Daemon::new(
            Loop::init().unwrap(),
            NvimSession::new(for_test_on(nvim_ours)),
            rex,
            String::new(),
            String::new(),
        );
        Fixture {
            d,
            rex_end,
            nvim_end,
        }
    }

    #[test]
    fn a_second_hello_for_a_pane_is_refused_and_the_first_terminal_stays() {
        use crate::daemon::{fdpass, pane_helper};
        let mut fx = fixture();
        fx.d.panes
            .add(Pane::startup(None, Surface::for_test(80, 24)));
        let before =
            fx.d.panes
                .by_id(0)
                .unwrap()
                .terminal_surface
                .as_ref()
                .unwrap()
                .helper_fd();

        let path = std::env::temp_dir().join(format!("neorex-dup-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let mut pipe: [i32; 2] = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(pipe.as_mut_ptr()) });
        let mut buf = [0u8; 16];
        fdpass::send(
            &client,
            pane_helper::hello_bytes(&mut buf, Some(0)).unwrap(),
            pipe[1],
        )
        .unwrap();

        fx.d.accept(&server).unwrap();

        let after =
            fx.d.panes
                .by_id(0)
                .unwrap()
                .terminal_surface
                .as_ref()
                .unwrap()
                .helper_fd();
        assert_eq!(before, after);
        // the refused helper was hung up on
        let mut client = client;
        let mut byte = [0u8; 1];
        assert_eq!(0, client.read(&mut byte).unwrap());
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A daemon with a 200 x 50 screen and the tabline on, and panes to place.
    /// Only its panes and grids are set up.
    struct TablineFixture {
        fx: Fixture,
    }

    impl TablineFixture {
        fn init() -> TablineFixture {
            let mut fx = fixture();
            fx.d.tabline_rows = 1;
            fx.d.grids.grids.insert(1, Grid::default());
            fx.d.grids.get_mut(1).unwrap().resize(200, 50);
            TablineFixture { fx }
        }

        fn d(&mut self) -> &mut Daemon {
            &mut self.fx.d
        }

        /// A window's pane, for slot `slot`, of `cols` columns, at `rect` in
        /// the Rex window; None for one Rex hasn't placed yet. Its id.
        fn add(&mut self, slot: u32, cols: u32, rect: Option<[f64; 4]>) -> u32 {
            let mut p = Pane::from_neovim_window(slot + 1, 1000 + i64::from(slot));
            p.terminal_surface = Some(Surface::for_test(cols, 20));
            p.rect = rect;
            self.fx.d.panes.add(p).id
        }

        fn start(&self, at: usize) -> Option<u32> {
            self.fx
                .d
                .geometry()
                .tabline_start(&self.fx.d.panes.list[at])
        }
    }

    #[test]
    fn a_tabline_slice_cut_through_a_wide_char_blanks_its_left_half() {
        let mut g = Grid::default();
        g.resize(8, 1);
        g.cells[3] = Cell::init("x", 0);
        g.cells[4] = Cell::init("\u{3042}", 0);
        g.cells[5] = Cell {
            len: 0,
            ..Cell::default()
        };
        let right = tabline_slice(&g, 0, 5);
        assert_eq!(5, right.len());
        assert_eq!("x".as_bytes(), right[3].str());
        assert_eq!(" ".as_bytes(), right[4].str());
        let whole = tabline_slice(&g, 0, 6);
        assert_eq!("\u{3042}".as_bytes(), whole[4].str());
    }

    #[test]
    fn with_a_shell_to_the_left_the_first_neovim_pane_starts_the_tabline_and_the_next_continues_it()
    {
        let mut fx = TablineFixture::init();
        // a shell holds x = 0 to 0.4; the Neovim panes sit right of it
        let left = fx.add(0, 60, Some([0.4, 0.0, 0.3, 1.0]));
        let right = fx.add(1, 60, Some([0.7, 0.0, 0.3, 1.0]));
        assert_eq!(Some(0), fx.start(0));
        assert_eq!(Some(60), fx.start(1));

        // every tab shows once: the slices, joined, are the tabline's cells
        {
            let screen = fx.d().grids.get_mut(1).unwrap();
            for (i, c) in screen.cells[0..200].iter_mut().enumerate() {
                *c = Cell::init([b'a' + (i % 26) as u8], 0);
            }
        }
        let d = &fx.fx.d;
        let screen = d.grids.get(1).unwrap();
        let ls = tabline_slice(screen, fx.start(0).unwrap(), 60);
        let rs = tabline_slice(screen, fx.start(1).unwrap(), 60);
        assert_eq!(60, ls.len());
        assert_eq!(60, rs.len());
        for (a, b) in ls.iter().zip(&screen.row(0)[0..60]) {
            assert_eq!(b.str(), a.str());
        }
        for (a, b) in rs.iter().zip(&screen.row(0)[60..120]) {
            assert_eq!(b.str(), a.str());
        }

        // what draw sets up: the slice, and the forwarder's column for clicks
        let want = d.grids.get(1).unwrap().row(0)[60];
        let prepared = fx.d().prepare_tabline(right);
        assert_eq!(60, prepared.col);
        assert_eq!(want.str(), prepared.cells.unwrap()[0].str());
        let fwd = |fx: &mut TablineFixture, id: u32| {
            let f = &fx
                .d()
                .panes
                .by_id(id)
                .unwrap()
                .terminal_surface
                .as_ref()
                .unwrap()
                .forwarder;
            (f.tabline_col, f.tabline_rows)
        };
        assert_eq!(60, fwd(&mut fx, right).0);
        assert_eq!(1, fwd(&mut fx, right).1);
        let first = fx.d().prepare_tabline(left);
        assert_eq!(0, first.col);
        assert_eq!(0, fwd(&mut fx, left).0);

        // the panes swapped places in the list, not in the window
        fx.d().panes.list.swap(0, 1);
        assert_eq!(Some(0), fx.start(1));
        assert_eq!(Some(60), fx.start(0));
    }

    #[test]
    fn a_pane_that_closes_moves_the_ones_to_its_right_left_and_only_top_panes_show_the_tabline() {
        let mut fx = TablineFixture::init();
        fx.add(0, 50, Some([0.0, 0.0, 0.25, 0.5]));
        fx.add(1, 70, Some([0.25, 0.0, 0.35, 0.5]));
        fx.add(2, 80, Some([0.6, 0.0, 0.4, 0.5]));
        // under them, and on another tab page: neither shows it, or takes room
        fx.add(3, 90, Some([0.0, 0.5, 1.0, 0.5]));
        assert_eq!(None, fx.start(3));
        assert_eq!(Some(120), fx.start(2));
        fx.d().panes.list[1].tab = 2;
        assert_eq!(None, fx.start(1));
        assert_eq!(Some(50), fx.start(2));
        // the first closes, as in the middle of `draw`
        fx.d().panes.list.remove(0);
        assert_eq!(Some(0), fx.start(1));
        // no tabline: no pane shows any
        fx.d().tabline_rows = 0;
        assert_eq!(None, fx.start(1));
    }

    #[test]
    fn a_pane_rex_hasnt_placed_yet_doesnt_take_the_top_left_unless_none_is_placed() {
        let mut fx = TablineFixture::init();
        // a new split, before the layout names its place
        fx.add(0, 40, None);
        // no layout yet at all: it's the window
        assert_eq!(Some(0), fx.start(0));
        fx.add(1, 60, Some([0.4, 0.0, 0.6, 1.0]));
        assert_eq!(None, fx.start(0));
        // and the placed pane starts the tabline, not after it
        assert_eq!(Some(0), fx.start(1));
    }

    /// The JSON of each text frame in `bytes`, as a Client wrote them: masked
    /// frames with 7-bit or 16-bit lengths. (Dropping a `Client` also
    /// sends a close frame, which isn't text.)
    fn client_frames(bytes: &[u8]) -> Vec<String> {
        let mut frames = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let opcode = bytes[i] & 0x0f;
            let mut len = (bytes[i + 1] & 0x7f) as usize;
            i += 2;
            if len == 126 {
                len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                i += 2;
            }
            let mask = &bytes[i..i + 4];
            let text: Vec<u8> = bytes[i + 4..i + 4 + len]
                .iter()
                .enumerate()
                .map(|(k, b)| b ^ mask[k % 4])
                .collect();
            if opcode == 1 {
                frames.push(String::from_utf8(text).unwrap());
            }
            i += 4 + len;
        }
        frames
    }

    /// The requests a daemon with a Rex connection to nothing but a socket sends
    /// while `body` does something with it, as Rex reads them.
    fn fake_rex_requests(body: impl FnOnce(&mut Daemon)) -> Vec<String> {
        let Fixture {
            mut d,
            mut rex_end,
            nvim_end: _,
        } = fixture();
        body(&mut d);
        drop(d); // the end of what it sent

        let mut all = Vec::new();
        rex_end.read_to_end(&mut all).unwrap();
        client_frames(&all)
    }

    /// What `on_direction` asks Rex for with `m`: the requests Rex reads.
    fn direction_requests(m: windows::Move) -> Vec<String> {
        fake_rex_requests(|d| d.on_direction(m).unwrap())
    }

    #[test]
    fn a_directional_move_asks_rex_to_focus_that_way_without_wrapping_once_per_count() {
        let down = direction_requests(windows::Move {
            dir: windows::Direction::Down,
            count: 3,
        });
        assert_eq!(3, down.len());
        assert_eq!(
            r#"{"type":"request","id":"1","method":"session.focus_direction","payload":{"session_id":"session:s","direction":"down","wrap":false}}"#,
            down[0]
        );
        assert_eq!(
            r#"{"type":"request","id":"3","method":"session.focus_direction","payload":{"session_id":"session:s","direction":"down","wrap":false}}"#,
            down[2]
        );

        // Neovim's hjkl are Rex's left, down, up and right
        for (dir, name) in [
            (windows::Direction::Left, "left"),
            (windows::Direction::Up, "up"),
            (windows::Direction::Right, "right"),
        ] {
            let one = direction_requests(windows::Move { dir, count: 1 });
            assert_eq!(1, one.len());
            assert!(one[0].contains(&format!("\"direction\":\"{name}\"")));
        }

        // a stray huge count doesn't flood Rex
        let many = direction_requests(windows::Move {
            dir: windows::Direction::Left,
            count: 9999,
        });
        assert_eq!(20, many.len());
    }

    /// A layout of one row of windows 1 to `n`, `widths` wide, in tab page 2.
    fn window_row(widths: &[u32]) -> Layout {
        let mut layout = Layout::new(2);
        let total: u32 = widths.iter().sum();
        layout.nodes[0] = Node {
            kind: Kind::Row,
            w: total,
            h: 10,
            ..Node::default()
        };
        for (i, &w) in widths.iter().enumerate() {
            let at = i + 1;
            layout.nodes[at] = Node {
                kind: Kind::Window,
                win: at as i64,
                w,
                h: 10,
                end: at + 1,
            };
        }
        layout.len = widths.len() + 1;
        layout.nodes[0].end = layout.len;
        layout
    }

    fn methods_of(frames: &[String]) -> Vec<&str> {
        let key = "\"method\":\"";
        frames
            .iter()
            .map(|f| {
                let at = f.find(key).unwrap() + key.len();
                &f[at..at + f[at..].find('"').unwrap()]
            })
            .collect()
    }

    fn block_id(n: u32) -> String {
        let id = format!("block:00000000-0000-0000-0000-{n:0>12}");
        assert_eq!(42, id.len());
        id
    }

    #[test]
    fn a_reopened_windows_pane_waits_for_it_to_be_drawn_across_flushes() {
        let mut fx = fixture();
        let d = &mut fx.d;
        d.nvim.ready = true;
        let draw = |d: &mut Daemon, grid: u32, win: i64| {
            d.grids.wins.insert(
                grid,
                Win {
                    win,
                    kind: WinKind::External,
                    float: Float::default(),
                },
            );
        };
        draw(d, 2, 1000);
        let id = d.panes.add(Pane::from_neovim_window(1, 1000)).id;
        d.reconcile().unwrap();
        assert!(d.panes.by_id(id).is_some());

        // :tabclose takes window 1000, and the plugin reopens it as 1010
        d.on_note(windows::Note::Reopen(1000)).unwrap();
        d.grids.wins.swap_remove(&2);
        d.reconcile().unwrap();
        assert!(d.panes.by_id(id).is_some());
        d.on_note(windows::Note::Reopened {
            old: 1000,
            new: 1010,
        })
        .unwrap();
        assert_eq!(Some(1010), d.panes.by_id(id).unwrap().neovim_window_handle);

        // a flush before 1010 is drawn, as a slow autocommand can make
        d.reconcile().unwrap();
        assert!(
            d.panes.by_id(id).is_some(),
            "the pane hung up before its window was drawn"
        );
        assert_eq!(None, d.stopped);

        draw(d, 3, 1010);
        d.reconcile().unwrap();
        assert!(d.panes.by_id(id).is_some());

        // drawn, then closed: its pane goes
        d.grids.wins.swap_remove(&3);
        d.reconcile().unwrap();
        assert!(d.panes.by_id(id).is_none());
    }

    #[test]
    fn a_tab_pages_panes_are_moved_once_they_all_have_blocks_and_only_resized_when_its_sizes_change()
     {
        let frames = fake_rex_requests(|d| {
            // windows 1, 2 and 3 are drawn; the layout puts 3, made last, first
            for n in [1u32, 2, 3] {
                d.grids.wins.insert(
                    n + 10,
                    Win {
                        win: i64::from(n),
                        kind: WinKind::External,
                        float: Float::default(),
                    },
                );
                let p = d.panes.add(Pane::from_neovim_window(n, i64::from(n)));
                p.tab = 2;
                // the layout comes before the last block does
                if n < 3 {
                    p.rex_block = Some(pane::rex_block(block_id(n).as_bytes()).unwrap());
                }
            }
            let mut layout = window_row(&[20, 40, 40]);
            layout.nodes[1].win = 3;
            layout.nodes[3].win = 1;
            d.on_tab_layout(layout).unwrap();
            // 1 request would be a subset placed, to be moved again
            d.place_tabs().unwrap();
            d.panes.by_win_mut(3).unwrap().rex_block =
                Some(pane::rex_block(block_id(3).as_bytes()).unwrap());
            d.place_tabs().unwrap();
            // nothing changed: nothing sent
            d.place_tabs().unwrap();
            // sizes only
            layout.nodes[1].w = 30;
            d.on_tab_layout(layout).unwrap();
        });
        let methods = methods_of(&frames);
        assert_eq!(4, methods.len());
        assert_eq!("session.move_block", methods[0]);
        assert_eq!("session.move_block", methods[1]);
        assert_eq!("session.resize_pane", methods[2]);
        assert_eq!("session.resize_pane", methods[3]);
        // the last pane of the layout goes after the first one's, then the middle one
        assert!(frames[0].contains(&format!(
            "\"block_id\":\"{}\",\"anchor_block_id\":\"{}\"",
            block_id(1),
            block_id(3)
        )));
        assert!(frames[1].contains(&format!(
            "\"block_id\":\"{}\",\"anchor_block_id\":\"{}\"",
            block_id(2),
            block_id(3)
        )));
    }

    /// The text frame a Rex server sends: unmasked.
    fn server_frame(text: &str) -> Vec<u8> {
        let mut f = vec![0x81];
        match text.len() {
            n if n < 126 => f.push(n as u8),
            n => {
                f.push(126);
                f.extend((n as u16).to_be_bytes());
            }
        }
        f.extend(text.as_bytes());
        f
    }

    /// A daemon whose windows 1 and 2 are drawn, in tab page 2, and don't have
    /// panes yet.
    fn two_windows_of_a_tab_page() -> Fixture {
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        for n in [1i64, 2] {
            fx.d.grids.wins.insert(
                n as u32 + 10,
                Win {
                    win: n,
                    kind: WinKind::External,
                    float: Float::default(),
                },
            );
            fx.d.changes.note_split(windows::Split {
                win: n,
                side_by_side: true,
                before: false,
                from: 0,
                tab: 2,
            });
        }
        fx
    }

    /// Every request the daemon sent Rex, as methods.
    fn sent_methods(mut fx: Fixture) -> Vec<String> {
        drop(fx.d);
        let mut all = Vec::new();
        fx.rex_end.read_to_end(&mut all).unwrap();
        methods_of(&client_frames(&all))
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn a_window_waiting_for_its_tab_pages_floating_pane_asks_for_its_own_once_rex_refuses_it() {
        let mut fx = two_windows_of_a_tab_page();
        // the first window asks for the floating pane; the second waits for it
        fx.d.new_pane(1).unwrap();
        fx.d.reconcile().unwrap();
        assert_eq!(1, fx.d.panes.list.len());
        assert!(fx.d.panes.by_win(2).is_none());

        // Rex refuses request 1
        use std::io::Write;
        let refusal = r#"{"type":"response","id":"1","seq":0,"error":{"code":"refused","message":"no","status":400}}"#;
        fx.rex_end.write_all(&server_frame(refusal)).unwrap();
        fx.d.on_rex().unwrap();

        let (first, second) = (
            fx.d.panes.by_win(1).unwrap().id,
            fx.d.panes.by_win(2).unwrap().id,
        );
        assert_eq!(vec![first, second], pane_ids(&fx.d));
        assert_eq!(
            vec!["session.new_layer", "session.new_layer"],
            sent_methods(fx)
        );
    }

    #[test]
    fn a_window_keeps_waiting_for_its_tab_pages_floating_pane_still_on_its_way() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.new_pane(1).unwrap();
        fx.d.reconcile().unwrap();
        let first = fx.d.panes.by_win(1).unwrap().id;
        assert_eq!(vec![first], pane_ids(&fx.d));
        assert_eq!(vec!["session.new_layer"], sent_methods(fx));
    }

    #[test]
    fn the_window_leaves_cmdheight_rows_for_messages_then_the_tablines() {
        let mut fx = TablineFixture::init();
        let id = fx.add(0, 60, None);
        let size = |fx: &mut TablineFixture| {
            let d = fx.d();
            d.geometry().window_size(d.panes.by_id(id).unwrap())
        };

        // before Neovim says, one row, as the default 'cmdheight'
        assert_eq!([60, 18], size(&mut fx));
        fx.d().msg_rows = 0;
        assert_eq!([60, 19], size(&mut fx));
        fx.d().msg_rows = 2;
        assert_eq!([60, 17], size(&mut fx));
        fx.d().tabline_rows = 0;
        assert_eq!([60, 18], size(&mut fx));
    }

    #[test]
    fn a_window_adopted_before_its_pane_exists_leaves_the_same_rows_for_messages() {
        let mut fx = TablineFixture::init();
        // no pane to split from: Rex's default size, 80 x 24
        assert_eq!([80, 23], fx.d().new_window_size(1000));
        fx.d().msg_rows = 0;
        assert_eq!([80, 24], fx.d().new_window_size(1000));
        fx.d().msg_rows = 2;
        assert_eq!([80, 22], fx.d().new_window_size(1000));
    }

    // Rex's layout and floating panes

    /// Gives pane `id` the Rex block `block_id(n)`.
    fn set_block(d: &mut Daemon, id: u32, n: u32) {
        d.panes.by_id_mut(id).unwrap().rex_block =
            Some(pane::rex_block(block_id(n).as_bytes()).unwrap());
    }

    fn layer_bounds_of(d: &Daemon, id: u32) -> [f64; 4] {
        d.panes.by_id(id).unwrap().layer.as_ref().unwrap().bounds
    }

    /// Reads `view` as Rex's, and sends what that asked of Neovim.
    fn read_and_flush(d: &mut Daemon, view: Value) {
        apply_views(d, &[view]);
        d.nvim.client.flush().unwrap();
    }

    fn apply_views(d: &mut Daemon, wins: &[Value]) -> bool {
        d.apply_layout(&rex_layout::read(wins))
    }

    fn approx(a: [f64; 4], b: [f64; 4]) {
        for i in 0..4 {
            assert!((a[i] - b[i]).abs() < 1e-9, "{a:?} != {b:?}");
        }
    }

    fn window_id(n: u32) -> String {
        let id = format!("window:00000000-0000-0000-0000-{n:0>12}");
        assert_eq!(43, id.len());
        id
    }

    /// A group at row 4, column 10 of grid 1, 60 x 20 cells.
    fn picker_group() -> Group {
        let mut g = Group {
            key: 1002,
            row: 4,
            col: 10,
            width: 60,
            height: 20,
            ..Group::default()
        };
        g.tops[0] = 3;
        g.tops_len = 1;
        g
    }

    /// Grid 1 of 200 x 50, and an entered picker float, as one group, on top.
    fn show_picker(d: &mut Daemon) {
        d.grids.grids.insert(1, Grid::default());
        d.grids.get_mut(1).unwrap().resize(200, 50);
        d.grids.grids.insert(3, Grid::default());
        d.grids.get_mut(3).unwrap().resize(60, 20);
        d.grids.wins.insert(
            3,
            Win {
                win: 1002,
                kind: WinKind::Float,
                float: Float {
                    screen: Some([4, 10]),
                    ..Float::default()
                },
            },
        );
        d.nvim.note_entered(1002);
    }

    #[test]
    fn a_floating_panes_bounds_are_fractions_of_the_screen_around_its_group_and_its_frame() {
        let mut fx = TablineFixture::init();
        let d = fx.d();
        // 3 x 2 cells of frame: half of each around the group
        approx(
            [0.045, 0.06, 0.315, 0.44],
            floating_panes::layer_bounds(&d.grids, &picker_group()).unwrap(),
        );

        // too big for the screen: it fills it
        let mut big = picker_group();
        big.width = 300;
        big.height = 80;
        approx(
            [0.0, 0.0, 1.0, 1.0],
            floating_panes::layer_bounds(&d.grids, &big).unwrap(),
        );

        // at the right and bottom edges: pulled back in, so it's whole
        let mut edge = picker_group();
        edge.col = 190;
        edge.row = 45;
        let b = floating_panes::layer_bounds(&d.grids, &edge).unwrap();
        approx([1.0 - b[2], 1.0 - b[3], 63.0 / 200.0, 22.0 / 50.0], b);

        // off the top left: pulled in to it
        let mut off = picker_group();
        off.col = 0;
        off.row = 0;
        let b = floating_panes::layer_bounds(&d.grids, &off).unwrap();
        assert_eq!([0.0, 0.0], [b[0], b[1]]);

        // no screen to measure by, or an empty one
        d.grids.get_mut(1).unwrap().resize(0, 0);
        assert_eq!(
            None,
            floating_panes::layer_bounds(&d.grids, &picker_group())
        );
        d.grids.grids.swap_remove(&1);
        assert_eq!(
            None,
            floating_panes::layer_bounds(&d.grids, &picker_group())
        );
    }

    #[test]
    fn bounds_within_half_a_cell_of_grid_1_are_the_same() {
        let mut fx = TablineFixture::init();
        let d = fx.d();
        let a = [0.1, 0.2, 0.3, 0.4];
        // a cell is 1/200 wide and 1/50 high
        assert!(floating_panes::same_bounds(&d.grids, a, a));
        assert!(floating_panes::same_bounds(
            &d.grids,
            a,
            [0.1 + 0.0024, 0.2 + 0.009, 0.3 - 0.0024, 0.4 - 0.009]
        ));
        assert!(!floating_panes::same_bounds(
            &d.grids,
            a,
            [0.1 + 0.0026, 0.2, 0.3, 0.4]
        ));
        assert!(!floating_panes::same_bounds(
            &d.grids,
            a,
            [0.1, 0.2 + 0.0101, 0.3, 0.4]
        ));
        assert!(!floating_panes::same_bounds(
            &d.grids,
            a,
            [0.1, 0.2, 0.3 + 0.0026, 0.4]
        ));
        assert!(!floating_panes::same_bounds(
            &d.grids,
            a,
            [0.1, 0.2, 0.3, 0.4 + 0.0101]
        ));
        // no screen: nothing to tell them apart by
        d.grids.grids.swap_remove(&1);
        assert!(floating_panes::same_bounds(
            &d.grids,
            a,
            [0.9, 0.9, 0.9, 0.9]
        ));
    }

    #[test]
    fn the_layout_gives_our_panes_their_rects_and_the_window_holding_them_its_id() {
        let mut fx = TablineFixture::init();
        let a = fx.add(0, 60, None);
        let b = fx.add(1, 60, None);
        let other = fx.add(2, 60, None);
        let d = fx.d();
        set_block(d, a, 1);
        set_block(d, b, 2);
        set_block(d, other, 3);
        let wins = vec![
            // no panes of ours: its id isn't ours
            json!({ "window_id": window_id(8), "layers": [ { "kind": "tiled", "layout": {
                "kind": "leaf", "block_id": block_id(99), "rect": [0, 0, 1, 1] } } ] }),
            json!({ "window_id": window_id(9), "layers": [ { "kind": "tiled", "layout": {
                "kind": "split", "children": [
                    { "kind": "leaf", "block_id": block_id(1), "rect": [0.0, 0.0, 0.5, 1.0] },
                    { "kind": "split", "children": [
                        { "kind": "leaf", "block_id": block_id(2), "rect": [0.5, 0.0, 0.5, 0.25] },
                        // a bad rect: kept as it was
                        { "kind": "leaf", "block_id": block_id(3), "rect": [0.5, 0.25, 0.5] },
                    ] },
                ] } } ] }),
        ];
        apply_views(d, &wins);
        assert_eq!(Some([0.0, 0.0, 0.5, 1.0]), d.panes.by_id(a).unwrap().rect);
        assert_eq!(Some([0.5, 0.0, 0.5, 0.25]), d.panes.by_id(b).unwrap().rect);
        assert_eq!(None, d.panes.by_id(other).unwrap().rect);
        assert_eq!(
            Some(*window_id(9).as_bytes().first_chunk().unwrap()),
            d.rex.window
        );
    }

    #[test]
    fn applying_the_same_layout_again_changes_nothing() {
        let mut fx = TablineFixture::init();
        let a = fx.add(0, 60, None);
        let d = fx.d();
        set_block(d, a, 1);
        let view = json!({ "window_id": window_id(9), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 0.5, 1.0] } } ] });
        assert!(apply_views(d, std::slice::from_ref(&view)));
        assert!(!apply_views(d, &[view]));
        // a pane that gets its block later still gets its rect
        let b = d.panes.add(Pane::from_neovim_window(50, 1000)).id;
        set_block(d, b, 2);
        let two = json!({ "window_id": window_id(9), "layers": [ { "kind": "tiled", "layout": {
            "block_id": block_id(2), "rect": [0.5, 0.0, 0.5, 1.0] } } ] });
        assert!(apply_views(d, &[two]));
        assert!(d.panes.by_id(b).unwrap().rect.is_some());
    }

    #[test]
    fn a_window_id_of_the_wrong_length_or_without_our_panes_isnt_kept() {
        let mut fx = TablineFixture::init();
        let a = fx.add(0, 60, None);
        let d = fx.d();
        set_block(d, a, 1);
        let layers = json!([ { "kind": "tiled", "layout": {
            "block_id": block_id(1), "rect": [0.0, 0.0, 1.0, 1.0] } } ]);
        apply_views(
            d,
            &[json!({ "window_id": "window:short", "layers": layers })],
        );
        assert_eq!(None, d.rex.window);
        assert!(d.panes.by_id(a).unwrap().rect.is_some());
        apply_views(d, &[json!({ "layers": layers })]);
        assert_eq!(None, d.rex.window);
        // our pane only in a floating layer: the tiled ones hold the window
        apply_views(
            d,
            &[
                json!({ "window_id": window_id(4), "layers": [ { "kind": "floating",
            "layout": { "block_id": block_id(1), "rect": [0.1, 0.1, 0.2, 0.2] } } ] }),
            ],
        );
        assert_eq!(None, d.rex.window);
        assert_eq!(Some([0.1, 0.1, 0.2, 0.2]), d.panes.by_id(a).unwrap().rect);
    }

    #[test]
    fn a_float_group_asks_rex_for_a_floating_pane_at_its_bounds_and_its_close_closes_it() {
        let Fixture {
            mut d,
            mut rex_end,
            nvim_end: _,
        } = fixture();
        show_picker(&mut d);
        d.floating().reconcile_layers().unwrap();
        // already has one
        d.floating().reconcile_layers().unwrap();
        assert_eq!(1, d.panes.list.len());
        let id = d.panes.list[0].id;
        approx(
            [0.045, 0.06, 0.315, 0.44],
            d.panes.by_id(id).unwrap().layer.as_ref().unwrap().bounds,
        );
        // Rex answers with its layer
        d.panes.by_id_mut(id).unwrap().layer.as_mut().unwrap().id =
            Some(pane::rex_layer(block_id(5).replace("block:", "layer:").as_bytes()).unwrap());
        // the float closes
        d.grids.wins.swap_remove(&3);
        d.floating().reconcile_layers().unwrap();
        assert!(d.panes.list.is_empty());
        drop(d);
        let mut all = Vec::new();
        rex_end.read_to_end(&mut all).unwrap();
        let frames = client_frames(&all);
        assert_eq!(
            vec!["session.new_layer", "session.close_layer"],
            methods_of(&frames)
        );
        let new: serde_json::Value = serde_json::from_str(&frames[0]).unwrap();
        assert_eq!("session:s", new["payload"]["session_id"]);
        assert_eq!(json!([0.045, 0.06, 0.315, 0.44]), new["payload"]["bounds"]);
        assert_eq!(
            json!(["neorex", "pane"]),
            json!([
                new["payload"]["layout"]["block"]["label"],
                new["payload"]["layout"]["block"]["options"]["command"][1]
            ])
        );
        let close: serde_json::Value = serde_json::from_str(&frames[1]).unwrap();
        assert_eq!(
            block_id(5).replace("block:", "layer:"),
            close["payload"]["layer_id"].as_str().unwrap()
        );
    }

    #[test]
    fn a_float_group_that_closes_before_rex_answers_sends_no_close() {
        let frames = fake_rex_requests(|d| {
            show_picker(d);
            d.floating().reconcile_layers().unwrap();
            d.grids.wins.swap_remove(&3);
            d.floating().reconcile_layers().unwrap();
            assert!(d.panes.list.is_empty());
        });
        assert_eq!(vec!["session.new_layer"], methods_of(&frames));
    }

    #[test]
    fn an_unentered_float_or_a_closed_rex_connection_gets_no_floating_pane() {
        let frames = fake_rex_requests(|d| {
            show_picker(d);
            d.nvim = NvimSession::new(for_test_on(UnixStream::pair().unwrap().0));
            d.floating().reconcile_layers().unwrap();
            assert!(d.panes.list.is_empty());
            d.nvim.note_entered(1002);
            d.rex.conn = Conn::Closed;
            d.floating().reconcile_layers().unwrap();
            assert!(d.panes.list.is_empty());
        });
        assert!(frames.is_empty());
    }

    /// A floating pane of the picker group, with a surface of `cols` x `rows`,
    /// that Rex has answered, in `d`. Its id and block.
    fn made_picker_pane(d: &mut Daemon, cols: u32, rows: u32) -> u32 {
        show_picker(d);
        d.floating().reconcile_layers().unwrap();
        let p = &mut d.panes.list[0];
        p.terminal_surface = Some(Surface::for_test(cols, rows));
        let id = p.id;
        set_block(d, id, 7);
        id
    }

    fn floating_layer_view(bounds: [f64; 4]) -> Value {
        json!({ "window_id": window_id(1), "layers": [ { "kind": "floating",
            "bounds": bounds, "blocks": [ { "block_id": block_id(7) } ],
            "layout": { "block_id": block_id(7), "rect": [0.0, 0.0, 1.0, 1.0] } } ] })
    }

    /// How many times the daemon asked Neovim to place floats, as it wrote them.
    fn place_group_requests(mut nvim_end: UnixStream) -> usize {
        let mut all = Vec::new();
        nvim_end.read_to_end(&mut all).unwrap();
        String::from_utf8_lossy(&all)
            .matches("neorex.place_group")
            .count()
    }

    fn requests_sent(mut end: UnixStream) -> Vec<String> {
        let mut all = Vec::new();
        end.read_to_end(&mut all).unwrap();
        client_frames(&all)
    }

    #[test]
    fn a_floating_pane_the_user_moved_moves_its_floats_once_and_doesnt_oscillate() {
        let Fixture {
            mut d,
            rex_end,
            nvim_end,
        } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        let asked = layer_bounds_of(&d, id);

        // Rex reports the bounds we asked for: nothing to follow
        read_and_flush(&mut d, floating_layer_view(asked));
        // within half a cell: the same
        let near = [asked[0] + 0.002, asked[1], asked[2], asked[3]];
        read_and_flush(&mut d, floating_layer_view(near));
        assert_eq!(asked, layer_bounds_of(&d, id));

        // the user moves it 20 columns right: its floats follow, once
        let moved = [asked[0] + 0.1, asked[1], asked[2], asked[3]];
        read_and_flush(&mut d, floating_layer_view(moved));
        assert_eq!(moved, layer_bounds_of(&d, id));
        // Neovim has put them there: the next flush's group, and Rex's view again
        d.grids.wins.get_mut(&3).unwrap().float.screen = Some([4, 30]);
        d.floating().reconcile_layers().unwrap();
        assert_eq!(30, d.panes.list[0].layer.as_ref().unwrap().group.col);
        read_and_flush(&mut d, floating_layer_view(moved));
        read_and_flush(&mut d, floating_layer_view(moved));
        assert_eq!(1, d.panes.list.len());
        drop(d);
        assert_eq!(1, place_group_requests(nvim_end));
        // and Rex is told nothing of it but the pane we asked for
        assert_eq!(
            vec!["session.new_layer"],
            methods_of(&requests_sent(rex_end))
        );
    }

    #[test]
    fn a_floating_panes_floats_stay_if_it_is_already_where_rex_says() {
        let Fixture {
            mut d,
            rex_end: _rex_end,
            nvim_end,
        } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        // a change of size beyond a half cell, but not of its place: the
        // floats' size is the pane's, which is theirs already
        let asked = layer_bounds_of(&d, id);
        let wider = [asked[0], asked[1], asked[2] + 0.1, asked[3]];
        read_and_flush(&mut d, floating_layer_view(wider));
        assert_eq!(wider, layer_bounds_of(&d, id));
        drop(d);
        assert_eq!(0, place_group_requests(nvim_end));
    }

    #[test]
    fn a_floating_layer_that_isnt_one_of_ours_to_follow_changes_nothing() {
        let Fixture {
            mut d,
            rex_end: _rex_end,
            nvim_end,
        } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        let asked = layer_bounds_of(&d, id);
        let moved = [asked[0] + 0.1, asked[1], asked[2], asked[3]];
        let view = |layer: Value| json!({ "window_id": window_id(1), "layers": [layer] });
        for layer in [
            // no bounds
            json!({ "kind": "floating", "blocks": [ { "block_id": block_id(7) } ] }),
            // bounds that aren't four numbers
            json!({ "kind": "floating", "bounds": [0.1, 0.2],
                "blocks": [ { "block_id": block_id(7) } ] }),
            // no blocks, or none
            json!({ "kind": "floating", "bounds": moved }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [] }),
            // its first isn't ours, or has no id
            json!({ "kind": "floating", "bounds": moved,
                "blocks": [ { "block_id": block_id(8) }, { "block_id": block_id(7) } ] }),
            json!({ "kind": "floating", "bounds": moved, "blocks": [ {} ] }),
        ] {
            read_and_flush(&mut d, view(layer));
            assert_eq!(asked, layer_bounds_of(&d, id));
        }

        // a pane of ours that has no floating pane: a window's, in a layer
        let w = d.panes.add(Pane::from_neovim_window(50, 1000)).id;
        set_block(&mut d, w, 9);
        let before = d.panes.by_id(w).unwrap().rect;
        read_and_flush(
            &mut d,
            view(json!({ "kind": "floating", "bounds": moved,
                "blocks": [ { "block_id": block_id(9) } ] })),
        );
        assert_eq!(before, d.panes.by_id(w).unwrap().rect);
        assert!(d.panes.by_id(w).unwrap().layer.is_none());
        assert_eq!(asked, layer_bounds_of(&d, id));
        drop(d);
        assert_eq!(0, place_group_requests(nvim_end));
    }

    /// A fixture whose Rex socket the loop watches, and a pane waiting on a
    /// split Rex hasn't answered.
    fn rex_with_a_split_pending() -> (Fixture, u32) {
        let mut fx = fixture();
        fx.d.event_loop
            .add_read(fx.d.rex.client.fd(), Token::RexSocket as usize)
            .unwrap();
        let id = fx.d.panes.add(Pane::from_neovim_window(1, 1001)).id;
        fx.d.rex
            .request(Request::NewSplit(id), "block.split", json!({}))
            .unwrap();
        (fx, id)
    }

    /// Checks Rex counts as closed, unwatched, and the pane's split failed.
    fn assert_rex_closed_and_split_failed(d: &Daemon, pane: u32) {
        assert_eq!(Conn::Closed, d.rex.conn);
        assert!(!d.rex_watched);
        assert!(d.rex.pending_requests.is_empty());
        assert!(matches!(
            d.panes.by_id(pane).unwrap().closing,
            Some(Close::RequestFailed)
        ));
    }

    #[test]
    fn a_malformed_rex_message_closes_rex_and_fails_the_pending_split_without_stopping_the_daemon()
    {
        let (mut fx, id) = rex_with_a_split_pending();
        // JSON, but without a `type`
        let body = br#"{"payload":{}}"#;
        let mut frame = vec![0x81, body.len() as u8];
        frame.extend_from_slice(body);
        fx.rex_end.write_all(&frame).unwrap();
        fx.d.on_rex().unwrap();
        assert_rex_closed_and_split_failed(&fx.d, id);
        assert_eq!(None, fx.d.stopped);
    }

    #[test]
    fn an_abrupt_rex_hangup_fails_the_pending_split() {
        let (mut fx, id) = rex_with_a_split_pending();
        let Fixture { rex_end, .. } = &mut fx;
        rex_end.shutdown(std::net::Shutdown::Both).unwrap();
        fx.d.on_rex().unwrap();
        assert_rex_closed_and_split_failed(&fx.d, id);
        assert_eq!(None, fx.d.stopped);
    }

    #[test]
    fn a_failed_rex_write_closes_rex_and_fails_the_pending_split() {
        let (fx, id) = rex_with_a_split_pending();
        let Fixture { mut d, rex_end, .. } = fx;
        drop(rex_end); // writes to a socket nobody holds fail
        d.rex.focus_direction(rex_session::Direction::Left).unwrap();
        d.after_batch().unwrap();
        assert_rex_closed_and_split_failed(&d, id);
        assert_eq!(None, d.stopped);
    }

    #[test]
    fn a_failed_write_of_the_split_request_itself_fails_that_split() {
        let mut fx = fixture();
        fx.d.event_loop
            .add_read(fx.d.rex.client.fd(), Token::RexSocket as usize)
            .unwrap();
        let id = fx.d.panes.add(Pane::from_neovim_window(1, 1001)).id;
        let Fixture { mut d, rex_end, .. } = fx;
        drop(rex_end);
        d.rex
            .request(Request::NewSplit(id), "block.split", json!({}))
            .unwrap();
        d.after_batch().unwrap();
        assert_rex_closed_and_split_failed(&d, id);
        assert_eq!(None, d.stopped);
    }

    // Pane lifecycle. Already pinned above: a window waiting for its tab
    // page's floating pane that is on its way (and asking for its own once
    // Rex refuses it), and Rex closing failing the pending splits.

    /// A pane of window `win`, tab page `tab` (0 for none), in Rex block
    /// `block_id(n)`. Its id.
    fn pane_in_block(d: &mut Daemon, win: i64, tab: i64, n: u32) -> u32 {
        let p = d.panes.add_for_window(win);
        p.tab = tab;
        let id = p.id;
        set_block(d, id, n);
        id
    }

    fn json_of(frame: &str) -> Value {
        serde_json::from_str(frame).unwrap()
    }

    /// An ordinary window `win` that came from `from`, side by side or not.
    fn note_split_from(d: &mut Daemon, win: i64, from: i64, side_by_side: bool, before: bool) {
        d.changes.note_split(windows::Split {
            win,
            side_by_side,
            before,
            from,
            tab: 0,
        });
    }

    /// Everything a daemon wrote to Rex and to Neovim.
    struct Sent {
        rex: Vec<String>,
        nvim: String,
    }

    impl Sent {
        fn rex_methods(&self) -> Vec<&str> {
            methods_of(&self.rex)
        }

        /// How many windows it asked Neovim to close.
        fn neorex_closes(&self) -> usize {
            self.nvim.matches("neorex.close").count()
        }

        /// The payload of the `n`th request to Rex.
        fn payload(&self, n: usize) -> Value {
            json_of(&self.rex[n])["payload"].clone()
        }
    }

    /// Drops the daemon, ending what it sent, and reads that.
    fn sent_by(fx: Fixture) -> Sent {
        let Fixture {
            d,
            mut rex_end,
            mut nvim_end,
        } = fx;
        drop(d);
        let (mut rex, mut nvim) = (Vec::new(), Vec::new());
        rex_end.read_to_end(&mut rex).unwrap();
        nvim_end.read_to_end(&mut nvim).unwrap();
        Sent {
            rex: client_frames(&rex),
            nvim: String::from_utf8_lossy(&nvim).into_owned(),
        }
    }

    fn pane_ids(d: &Daemon) -> Vec<u32> {
        d.panes.list.iter().map(|p| p.id).collect()
    }

    #[test]
    fn a_new_window_splits_beside_the_window_it_came_from() {
        for (side_by_side, before, direction, side) in [
            (true, false, "horizontal", "after"),
            (false, true, "vertical", "before"),
        ] {
            let mut fx = fixture();
            // without `from`, the fallback would anchor on block 1, the first
            // pane in the list; the expected anchor is block 2
            let first = pane_in_block(&mut fx.d, 1, 0, 1);
            let from = pane_in_block(&mut fx.d, 2, 0, 2);
            note_split_from(&mut fx.d, 3, 2, side_by_side, before);
            fx.d.new_pane(3).unwrap();

            let new = fx.d.panes.by_win(3).unwrap();
            assert_eq!(0, new.tab);
            assert!(new.awaiting_layout);
            let new = new.id;
            assert_eq!(vec![first, from, new], pane_ids(&fx.d));
            let sent = sent_by(fx);
            assert_eq!(vec!["session.new_split"], sent.rex_methods());
            let req = sent.payload(0);
            assert_eq!(block_id(2), req["anchor_id"]);
            assert_eq!(direction, req["direction"]);
            assert_eq!(side, req["side"]);
            assert_eq!("session:s", req["session_id"]);
            assert_eq!(0, sent.neorex_closes());
        }
    }

    #[test]
    fn a_new_window_without_a_known_source_splits_beside_any_pane_rex_has_placed() {
        let mut fx = fixture();
        let placed = pane_in_block(&mut fx.d, 1, 0, 1);
        note_split_from(&mut fx.d, 3, 99, true, false);
        fx.d.new_pane(3).unwrap();
        let new = fx.d.panes.by_win(3).unwrap().id;
        assert_eq!(vec![placed, new], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["session.new_split"], sent.rex_methods());
        assert_eq!(block_id(1), sent.payload(0)["anchor_id"]);
    }

    #[test]
    fn a_new_window_with_no_placed_pane_makes_none_until_the_next_flush() {
        let mut fx = fixture();
        // a pane Rex hasn't answered has no block to split
        let unplaced = fx.d.panes.add_for_window(1).id;
        note_split_from(&mut fx.d, 3, 1, true, false);
        fx.d.new_pane(3).unwrap();
        assert_eq!(vec![unplaced], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert!(sent.rex.is_empty());
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn a_tab_pages_first_window_asks_for_its_floating_pane() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.new_pane(1).unwrap();
        let p = fx.d.panes.by_win(1).unwrap();
        assert_eq!(2, p.tab);
        assert!(p.layer.is_none());
        let first = p.id;
        assert_eq!(vec![first], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["session.new_layer"], sent.rex_methods());
        let req = sent.payload(0);
        assert_eq!(json!(TAB_LAYER_BOUNDS), req["bounds"]);
        assert_eq!("neorex", req["layout"]["block"]["label"]);
    }

    #[test]
    fn a_tab_pages_next_window_splits_beside_a_pane_of_its_tab_page_once_it_has_a_block() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.new_pane(1).unwrap();
        let first = fx.d.panes.by_win(1).unwrap().id;
        // Rex answers the floating pane's request
        set_block(&mut fx.d, first, 4);
        fx.d.new_pane(2).unwrap();
        let second = fx.d.panes.by_win(2).unwrap();
        assert_eq!(2, second.tab);
        let second = second.id;
        assert_eq!(vec![first, second], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(
            vec!["session.new_layer", "session.new_split"],
            sent.rex_methods()
        );
        assert_eq!(block_id(4), sent.payload(1)["anchor_id"]);
    }

    #[test]
    fn with_rex_closed_a_new_window_stays_in_neovim() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.rex.conn = Conn::Closed;
        fx.d.new_pane(1).unwrap();
        note_split_from(&mut fx.d, 5, 0, true, false);
        fx.d.new_pane(5).unwrap();
        assert!(fx.d.panes.list.is_empty());
        let sent = sent_by(fx);
        assert!(sent.rex.is_empty());
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn closing_the_last_non_floating_pane_stops_the_daemon_and_closes_no_window() {
        let mut fx = fixture();
        let id = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.pane_gone(id).unwrap();
        assert_eq!(Some(StopReason::PaneGone), fx.d.stopped);
        // it stays for the shutdown
        assert_eq!(vec![id], pane_ids(&fx.d));
        assert_eq!(
            Some(Close::RexPaneClosed),
            fx.d.panes.by_id(id).unwrap().closing
        );
        let sent = sent_by(fx);
        assert!(sent.rex.is_empty());
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn a_pane_whose_request_failed_doesnt_count_as_another_pane() {
        let mut fx = fixture();
        let failed = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.panes.by_id_mut(failed).unwrap().closing = Some(Close::RequestFailed);
        let id = pane_in_block(&mut fx.d, 2, 0, 2);
        fx.d.pane_gone(id).unwrap();
        assert_eq!(Some(StopReason::PaneGone), fx.d.stopped);
        assert_eq!(vec![failed, id], pane_ids(&fx.d));
        assert_eq!(
            Some(Close::RexPaneClosed),
            fx.d.panes.by_id(id).unwrap().closing
        );
        let sent = sent_by(fx);
        assert!(sent.rex.is_empty());
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn a_floating_pane_doesnt_count_as_another_pane_either() {
        let mut fx = fixture();
        let floating = made_picker_pane(&mut fx.d, 10, 5);
        let id = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.pane_gone(id).unwrap();
        assert_eq!(Some(StopReason::PaneGone), fx.d.stopped);
        assert_eq!(vec![floating, id], pane_ids(&fx.d));
        let sent = sent_by(fx);
        // only the floating pane's own request, from the setup
        assert_eq!(vec!["session.new_layer"], sent.rex_methods());
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn a_closed_pane_with_others_left_closes_its_neovim_window() {
        let mut fx = fixture();
        let gone = pane_in_block(&mut fx.d, 1, 0, 1);
        let kept = pane_in_block(&mut fx.d, 2, 0, 2);
        fx.d.pane_gone(gone).unwrap();
        assert_eq!(None, fx.d.stopped);
        assert_eq!(vec![kept], pane_ids(&fx.d));
        // so the next flush doesn't make it a pane again
        assert!(fx.d.changes.is_closing(1));
        assert!(!fx.d.changes.is_closing(2));
        let sent = sent_by(fx);
        // Rex closed the pane itself
        assert!(sent.rex.is_empty());
        assert_eq!(1, sent.neorex_closes());
    }

    #[test]
    fn a_closed_floating_pane_closes_its_floats_and_keeps_the_others() {
        let mut fx = fixture();
        let id = made_picker_pane(&mut fx.d, 10, 5);
        let kept = pane_in_block(&mut fx.d, 5, 0, 1);
        fx.d.pane_gone(id).unwrap();
        assert_eq!(None, fx.d.stopped);
        assert_eq!(vec![kept], pane_ids(&fx.d));
        let sent = sent_by(fx);
        // only the floating pane's own request, from the setup: Rex closed it
        assert_eq!(vec!["session.new_layer"], sent.rex_methods());
        assert_eq!(1, sent.neorex_closes());
    }

    /// A ready daemon with panes in blocks for windows 1 and 2, only window 2
    /// still drawn. Their pane ids.
    fn window_1_gone_from_two_panes() -> (Fixture, [u32; 2]) {
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        let ids = [
            pane_in_block(&mut fx.d, 1, 0, 1),
            pane_in_block(&mut fx.d, 2, 0, 2),
        ];
        fx.d.grids.wins.insert(
            12,
            Win {
                win: 2,
                kind: WinKind::External,
                float: Float::default(),
            },
        );
        (fx, ids)
    }

    #[test]
    fn after_a_flush_a_pane_whose_window_is_gone_is_removed_and_its_rex_pane_closed() {
        let (mut fx, [_, kept]) = window_1_gone_from_two_panes();
        fx.d.reconcile().unwrap();
        assert_eq!(None, fx.d.stopped);
        assert_eq!(vec![kept], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["block.close"], sent.rex_methods());
        assert_eq!(block_id(1), sent.payload(0)["block_id"]);
        // the window is already gone
        assert_eq!(0, sent.neorex_closes());
    }

    #[test]
    fn after_a_flush_a_pane_whose_window_waits_to_reopen_stays() {
        for reopened in [false, true] {
            let (mut fx, ids) = window_1_gone_from_two_panes();
            fx.d.changes.mark_reopening(1);
            if reopened {
                // window 7 took its pane and isn't drawn yet
                fx.d.on_reopened(1, 7);
            }
            fx.d.reconcile().unwrap();
            let waiting = if reopened { 7 } else { 1 };
            assert_eq!(
                Some(ids[0]),
                fx.d.panes.by_win(waiting).map(|p| p.id),
                "reopened={reopened}"
            );
            assert_eq!(ids.to_vec(), pane_ids(&fx.d));
            assert_eq!(None, fx.d.stopped);
            let sent = sent_by(fx);
            assert!(sent.rex.is_empty(), "reopened={reopened}");
            assert_eq!(0, sent.neorex_closes());
        }
    }

    #[test]
    fn after_a_flush_with_no_panes_left_the_daemon_stops() {
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.list.is_empty());
        assert_eq!(Some(StopReason::PaneGone), fx.d.stopped);
        let sent = sent_by(fx);
        assert_eq!(vec!["block.close"], sent.rex_methods());
        assert_eq!(0, sent.neorex_closes());
    }
}
