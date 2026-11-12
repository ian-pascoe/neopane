//! The running daemon: Neovim, its grids, the pane provider, a pane per
//! window and a floating pane per group of floats that took focus. After
//! each Neovim flush, `reconcile` gives new windows and float groups panes
//! and hangs up panes whose windows closed; then every pane is drawn.
// TODO: Needs a pretty major rewrite
//
// Ownership notes:
// - The daemon owns the loop, the Neovim client and the pane provider
//   (`event_loop`, `nvim.client`, `provider`), and drops them after its panes
//   (see `Drop`).
// - A pane is passed around by its id, looked up again after anything that
//   may add or remove panes (see `Panes`).
// - `NvimSession` aims forwarders through `PaneForwarders`, which borrows just
//   the panes and grids: the daemon itself can't be borrowed whole while
//   `self.nvim` is.

use std::os::fd::RawFd;
use std::os::unix::net::UnixListener;
use std::time::{Duration, Instant};

use super::asks::{self, Asks, Step};
use super::attach_conn::Pending;
use super::event_loop::{Event, Kind, Loop};
use super::floating_panes::{self, FloatingPanes};
use super::focus_gate::AimForwarder;
use super::geometry::{self, Geometry};
use super::live_layout::{Area, LiveLayout};
use super::nvim_session::NvimSession;
use super::pane::{self, Close, Fit, LastAskedWinSize, Pane, PaneFd};
use super::pane_layout::{self, Axis, PaneLayout};
use super::panes::Panes;
use super::provider::{Answer, Ask, Capability, Direction, ProviderEvent, TerminalId};
use super::provider_focus::ProviderFocus;
use super::surface::{self, Sent, Surface};
use super::tab_placement::{self, Action, PaneMove, Tab, WindowState};
use super::window_changes::WindowChanges;
use crate::attach::{Ending, Size, Transport};
use crate::log::{self, Point, log_info, log_warn};
use crate::name::NAME;
use crate::nvim::forwarder::{self, FocusReport, Forwarder, Taking};
use crate::nvim::msgpack::{self, Decoder};
use crate::nvim::rpc;
use crate::nvim::windows::{self, Layout};
use crate::providers::Held;
use crate::providers::rex::RexProvider;
use crate::screen::float_groups;
use crate::screen::grid::Cell;
use crate::screen::grids::{Grids, WinKind};
use crate::screen::pane_view::{LaidOut, Opts, Rect, Separator, Shown, tabline_slice};
use crate::term::input;
use indexmap::IndexMap;

mod attaches;
use attaches::PENDING_BASE;

/// The daemon is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) struct Daemon {
    /// Our executable and the helpers' socket, for new panes' command.
    pub exe: String,
    pub sock: super::dir::Address,
    /// How requested panes' helpers attach: `<NAME>_TRANSPORT` as the
    /// daemon started (ADR 0013).
    pub transport: Transport,
    /// The daemon's name and its log's path, as an accepted attach is told.
    pub name: String,
    pub log: String,
    /// Connections waiting for their hello, by loop token.
    pending: Vec<(usize, Pending)>,
    /// The next pending connection's token, past `PENDING_BASE`: never
    /// reused, so a stale event can't reach a newer connection.
    next_pending: usize,
    /// When the hello timer is set to fire, if it is.
    hello_timer_due: Option<Instant>,
    /// The user a connection's peer must run as: ours.
    uid: u32,
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
    /// The options panes laying out several windows split and close them by.
    pub split_options: windows::SplitOptions,
    /// Sizes Neovim gave windows not yet in a pane: window, cols, rows, as a
    /// `Resized` note has them. A window that joins a pane layout takes its.
    pub pending_sizes: Vec<(i64, u32, u32)>,
    /// Where Neovim has the windows of each tab page with a tab page area, as it
    /// last said, and what was done to place their panes to match.
    pub tab_layouts: IndexMap<i64, Tab>,
    /// The live layout last applied to the panes.
    pub live_layout: Option<LiveLayout>,
    /// When the area timer is set to fire, if it is.
    area_timer_due: Option<Instant>,
    /// When the escape timer is set to fire, if it is.
    escape_timer_due: Option<Instant>,
    /// Set by `stop`; `run` returns it.
    pub stopped: Option<StopReason>,
    /// The acts asked of the pane provider, or waiting to be.
    pub asks: Asks,
    /// When the ask timer is set to fire, if it is.
    ask_timer_due: Option<Instant>,
    /// Which terminal the pane provider has focused, as last said or asked.
    pub focus: ProviderFocus,
    /// A pane with no terminal id whose terminal reported a focus-in, and
    /// when: it becomes current if the report stands `FOCUS_IN_SETTLE_MS`.
    focus_in: Option<(u32, Instant)>,
    /// Terminals just opened whose windows weren't current, until their first
    /// focus report or a deadline: one that took focus gives it back (#86 D8).
    just_opened: Vec<(TerminalId, Instant)>,
    // Dropped last, in this order: the loop, then the pane provider, then
    // Neovim.
    pub event_loop: Loop,
    /// None without one, or once it failed as a whole.
    pub provider: Option<Held>,
    pub nvim: NvimSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    NvimExited,
    PaneGone,
    Signal,
}

impl StopReason {
    /// The reason's name, as logged.
    pub(crate) fn tag_name(self) -> &'static str {
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
pub(crate) enum Token {
    Signal,
    NvimChannel,
    HelperListener,
    /// The pane provider's sources.
    Provider,
    FocusTimer,
    LayoutTimer,
    SwitchTimer,
    EscapeTimer,
    HelloTimer,
    AskTimer,
    AreaTimer,
    FocusInTimer,
}

impl Token {
    /// The fixed token `token` is, or None for a pane's.
    pub(crate) fn of(token: usize) -> Option<Token> {
        Some(match token {
            0 => Token::Signal,
            1 => Token::NvimChannel,
            2 => Token::HelperListener,
            3 => Token::Provider,
            4 => Token::FocusTimer,
            5 => Token::LayoutTimer,
            6 => Token::SwitchTimer,
            7 => Token::EscapeTimer,
            8 => Token::HelloTimer,
            9 => Token::AskTimer,
            10 => Token::AreaTimer,
            11 => Token::FocusInTimer,
            _ => return None,
        })
    }
}

// the fixed tokens must stay below the pane tokens
const _: () = assert!((Token::FocusInTimer as usize) < pane::TOKEN_BASE);

/// The id `Pane::startup` gives the pane neorex was run in.
const STARTUP_PANE_ID: u32 = 0;

/// How long losing focus waits before Neovim hears of it: focus moving
/// from one pane to another, such as into a floating pane, arrives as an
/// out then an in, and shouldn't fire FocusLost and FocusGained.
const FOCUS_OUT_DELAY_MS: u32 = 80;

/// How long a focus-in report from a pane the pane provider has no id for
/// must stand before the pane becomes current: one that a focus-out follows
/// sooner was focus passing through (#92 decision 19).
const FOCUS_IN_SETTLE_MS: u32 = 80;

/// How long after its open is done a terminal's first focus report may still
/// be the focus it took as it was made (#86 D8).
const JUST_OPENED_MS: u64 = 1000;

/// How often a window switch still unanswered is checked for waiting behind
/// a command that needs a key.
const SWITCH_RECHECK_MS: u32 = 100;

/// How long a new split waits for the app to size it before trusting its
/// terminal's size.
const SPLIT_SIZE_WAIT_MS: u32 = 1000;

/// How long a requested pane whose terminal attached with its id waits for
/// the live layout to report it, before it is its own layout area.
const LAYOUT_WAIT_MS: u32 = 1000;

/// A count on a directional window command repeats the move this many times
/// at most: a window has few panes to cross, and a stray `9999<C-w>l` must not
/// send Rex thousands of requests.
const MAX_DIRECTION_COUNT: u32 = 20;

/// More rows than any terminal has: a 'cmdheight' past it is as good.
const MAX_MSG_ROWS: u32 = 1000;

/// Most sizes kept for windows not yet in a pane.
const MAX_PENDING_SIZES: usize = 16;

/// Most grids kept out of the panes that aren't floating panes.
const SKIP_MAX: usize = float_groups::MAX_GROUPS * 16;

/// What a pane draws of the screen's tabline, and the column it starts at.
struct Tabline {
    cells: Option<Vec<Cell>>,
    col: u32,
}

/// The terminals of the panes a move is between: the one placed, and the
/// one it is placed beside.
struct Terminals {
    moved: TerminalId,
    origin: TerminalId,
}

/// The panes, as `NvimSession` needs them to send input: borrowed apart from the daemon's `nvim`.
pub(crate) struct PaneForwarders<'a> {
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
        let ending = match self.stopped {
            Some(StopReason::NvimExited) => Ending::Quit,
            Some(StopReason::Signal) => Ending::Stopped,
            Some(StopReason::PaneGone) => Ending::Closed,
            None => Ending::Failed,
        };
        self.panes.deinit(ending);
        log::flush_traces();
    }
}

impl Daemon {
    /// A daemon on `event_loop`, Neovim and `provider`, with no panes yet.
    pub(crate) fn new(
        event_loop: Loop,
        nvim: NvimSession,
        provider: Option<Held>,
        exe: String,
        sock: super::dir::Address,
    ) -> Daemon {
        Daemon {
            exe,
            sock,
            transport: Transport::Fd,
            name: String::new(),
            log: String::new(),
            pending: Vec::new(),
            next_pending: 0,
            hello_timer_due: None,
            uid: super::attach_conn::euid(),
            grids: Grids::default(),
            panes: Panes::default(),
            changes: WindowChanges::default(),
            tabline_rows: 0,
            msg_rows: 1,
            split_options: windows::SplitOptions::default(),
            pending_sizes: Vec::new(),
            tab_layouts: IndexMap::new(),
            live_layout: None,
            area_timer_due: None,
            escape_timer_due: None,
            stopped: None,
            asks: Asks::default(),
            ask_timer_due: None,
            focus: ProviderFocus::default(),
            focus_in: None,
            just_opened: Vec::new(),
            event_loop,
            provider,
            nvim,
        }
    }

    /// Adds the launcher's pane, already connected, and starts talking to
    /// Neovim and the pane provider.
    pub(crate) fn start(&mut self, launch_pane: Pane) -> Result<()> {
        // only ever called on startup
        debug_assert!(self.panes.list.is_empty());

        let pane = self.panes.add(launch_pane);
        pane.register_with(&self.event_loop)?;
        let view = &pane.terminal_surface.as_ref().unwrap().view;
        let (cols, rows) = (view.cols, view.rows);
        self.nvim.attach_ui(cols, rows)?;
        if let Some(r) = self.rex() {
            r.session.attach()?;
        }
        // no requested pane: its own area until a live layout places it,
        // drawn and fitted as before there was any
        self.panes.by_id_mut(STARTUP_PANE_ID).unwrap().area = Area::Own;
        Ok(())
    }

    /// Runs the loop until something calls `stop`, returns why.
    pub(crate) fn run(&mut self, server: &UnixListener) -> Result<StopReason> {
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
                log_warn!("{NAME} daemon: a pane failed: {err}");
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
        if e.token >= PENDING_BASE {
            return self.on_pending(e.token);
        }
        match Token::of(e.token) {
            Some(Token::Signal) => self.stop(StopReason::Signal),
            Some(Token::NvimChannel) => self.on_nvim()?,
            Some(Token::Provider) => self.on_provider(e.token)?,
            Some(Token::FocusTimer) => self.send_focus(true)?,
            Some(Token::LayoutTimer) => self.layout_wait_over()?,
            Some(Token::SwitchTimer) => self.recheck_switch()?,
            Some(Token::EscapeTimer) => self.flush_pending_escapes()?,
            Some(Token::HelloTimer) => self.time_out_hellos(Instant::now())?,
            Some(Token::AskTimer) => self.time_out_asks()?,
            Some(Token::AreaTimer) => self.time_out_reports(Instant::now())?,
            Some(Token::FocusInTimer) => self.focus_in_stood(Instant::now())?,
            Some(Token::HelperListener) => {}
            None => self.on_pane(e)?,
        }
        Ok(())
    }

    /// After each batch of events: tells Neovim whether any pane has focus,
    /// and the tabline width. Losing focus waits a moment, in case another
    /// pane gains it.
    fn after_batch(&mut self) -> Result<()> {
        self.send_focus(false)?;
        self.sync_tabline_width()?;
        // what was asked in the batch, and anything a write that failed left
        self.drain_provider()?;
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
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        if self.nvim.recheck_switch(&mut fwds)? {
            self.add_timer(Token::SwitchTimer, SWITCH_RECHECK_MS)?;
        }
        Ok(())
    }

    /// Tells Neovim the tabline width when it changed. While the current
    /// pane is in no layout area, it stays.
    fn sync_tabline_width(&mut self) -> Result<()> {
        let Some(current) = self.current_pane() else {
            return Ok(());
        };
        if current.area == Area::Unreported {
            return Ok(());
        }
        let cols = self.geometry().tabline_width(current);
        self.nvim.tell_tabline_width(cols);
        self.nvim.client.flush()?;
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

    /// The pane provider never laid a new split out, in a second, or is gone:
    /// its terminal's size is the real one.
    fn layout_wait_over(&mut self) -> Result<()> {
        let mut any = false;
        for i in 0..self.panes.list.len() {
            let p = &mut self.panes.list[i];
            if p.awaiting_size && p.terminal_surface.is_some() {
                p.awaiting_size = false;
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
        let fd = s.output_fd();
        let Ok(finished) = s.backlog.drain(fd) else {
            return self.pane_gone(id);
        };
        if !s.backlog.is_empty() {
            return Ok(());
        }
        // the rest of the frame went out here, not in a draw's send since
        if finished {
            log::trace(Point::Frame, u64::from(id));
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

    /// Pane `id`'s terminal is `size` now, as its helper says.
    fn handle_resize(&mut self, id: u32, size: Size) -> Result<()> {
        log::trace(Point::Resize, u64::from(id));
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.awaiting_size = false;
        let surface = pane.terminal_surface.as_mut().unwrap();
        surface
            .view
            .resize(u32::from(size.cols), u32::from(size.rows));
        let asked = self.size_window(id);
        let _ = self.floating().follow_layer(id);
        if self.current_pane_id() == Some(id) {
            self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        }
        self.nvim.client.flush()?;
        if asked {
            log::trace(Point::ResizeSent, u64::from(id));
        }
        self.draw()
    }

    /// Pane `id`'s terminal, handed over, has input.
    fn on_input(&mut self, id: u32) -> Result<()> {
        let mut buf = [0u8; 4096];
        let Some(fd) = self.surface_of(id).io.input_fd() else {
            return Ok(());
        };
        let n = read_fd(fd, &mut buf);
        if n == 0 {
            return self.pane_gone(id);
        }
        self.on_keys(id, &buf[..n])
    }

    /// What was typed in pane `id`'s terminal, read or relayed.
    fn on_keys(&mut self, id: u32, bytes: &[u8]) -> Result<()> {
        // the key's latency is timed from here
        let at = Instant::now();
        self.on_read(id, bytes, at)
    }

    /// `bytes`, read or relayed from pane `id`'s terminal at `at`. Keys and
    /// clicks make the pane current at once, and have the pane provider focus
    /// it, except keys typed while a focus asked is unanswered (and `ECHO_MS`
    /// after): typed into the terminal focus is leaving, they go to Neovim's
    /// current window. A pane with no terminal id becomes current from a focus-in
    /// that stands (`FOCUS_IN_SETTLE_MS`).
    fn on_read(&mut self, id: u32, bytes: &[u8], at: Instant) -> Result<()> {
        let pane = self.panes.by_id(id).unwrap();
        let (win, terminal) = (pane.neovim_window_handle, pane.terminal.clone());
        if terminal.is_none() {
            let report = pane
                .terminal_surface
                .as_ref()
                .and_then(|s| s.forwarder.focus_report(bytes));
            self.watch_focus_in(id, report, at)?;
        }
        let focused_there = terminal.as_ref().is_some_and(|t| self.focus.has_focused(t));
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        let taking = self
            .nvim
            .takes_focus(&mut fwds, id, win, focused_there, bytes);
        let rerouted = taking == Taking::Key && self.focus.unanswered(at);
        if taking != Taking::Nothing && !rerouted {
            self.make_current(id)?;
            self.focus_terminal(terminal.as_ref());
        }
        // behind the switch above, if any: sent straight, the keys would reach the old window
        let mut fwds = PaneForwarders {
            panes: &mut self.panes,
            grids: &self.grids,
        };
        self.nvim.type_into_at(&mut fwds, id, bytes, at)?;
        self.arm_escape_timer()
    }

    /// Pane `id`, with no terminal id, reported focus `report` at `at`: a
    /// focus-in starts the wait for it to stand, unless its window is current
    /// or it may be the echo of a focus asked; a focus-out ends the wait.
    fn watch_focus_in(&mut self, id: u32, report: Option<FocusReport>, at: Instant) -> Result<()> {
        match report {
            Some(FocusReport::In) => {
                let win = self.panes.by_id(id).and_then(|p| p.neovim_window_handle);
                if win.is_some_and(|w| self.is_current_win(w)) || self.focus.unanswered(at) {
                    return Ok(());
                }
                self.focus_in = Some((id, at));
                self.add_timer(Token::FocusInTimer, FOCUS_IN_SETTLE_MS)
            }
            Some(FocusReport::Out) => {
                if self.focus_in.is_some_and(|(p, _)| p == id) {
                    self.focus_in = None;
                }
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// The focus-in timer fired, at `now`: a pane whose focus-in has stood
    /// long enough becomes current.
    fn focus_in_stood(&mut self, now: Instant) -> Result<()> {
        let Some((id, since)) = self.focus_in else {
            return Ok(());
        };
        let settle = Duration::from_millis(u64::from(FOCUS_IN_SETTLE_MS));
        if now < since + settle {
            return Ok(());
        }
        self.focus_in = None;
        let Some(p) = self.panes.by_id(id) else {
            return Ok(());
        };
        let Some(win) = p.neovim_window_handle else {
            return Ok(());
        };
        if p.layer.is_some() || self.is_current_win(win) {
            return Ok(());
        }
        let from_tab = self.current_pane().map(|p| p.tab);
        self.nvim.current_float = 0;
        self.make_current(id)?;
        self.trace_tab_move(from_tab, id);
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Whether `win` is Neovim's current window, no float over it current.
    fn is_current_win(&self, win: i64) -> bool {
        win == self.nvim.current_win && self.nvim.current_float == 0
    }

    /// Whether pane `id` shows what is current in Neovim: its window, or
    /// the current float, in a float group pane.
    fn shows_current(&self, id: u32) -> bool {
        let Some(p) = self.panes.by_id(id) else {
            return false;
        };
        if p.neovim_window_handle == Some(self.nvim.current_win) {
            return true;
        }
        self.nvim.current_float != 0
            && floating_panes::layer_of_win(&self.grids, &self.panes, self.nvim.current_float)
                .is_some_and(|q| q.id == id)
    }

    /// Pane `id`'s surface, which a pane that gets fd events has.
    fn surface_of(&self, id: u32) -> &Surface {
        self.panes
            .by_id(id)
            .and_then(|p| p.terminal_surface.as_deref())
            .expect("a pane with events has a terminal")
    }

    fn new_pane(&mut self, win: i64) -> Result<()> {
        // a size asked of it before it was external counts only in a pane
        // layout; a pane of its own is its size
        let size = self
            .pending_sizes
            .iter()
            .position(|&(w, ..)| w == win)
            .map(|i| self.pending_sizes.swap_remove(i));
        if self.provider.is_none() {
            // no pane provider: the window is drawn in its origin pane
            self.lay_out_in_origin(win, size);
            return Ok(());
        }
        let sp = self.changes.split_of(win);
        // a window opens beside its origin pane's terminal: the pane it came
        // from, else any with a terminal. A tab page's first window opens its
        // Rex floating pane (`open_tab_page`) from the pane it came from.
        let (origin, beside) = if sp.tab != 0 {
            match self.tab_page_origin(sp) {
                Some(t) => (Some(t), true),
                None => {
                    // its tab page's Rex floating pane is on its way: open
                    // beside that once it's made, or ask for its own if the
                    // open failed
                    if self.tab_floating_pane_pending(sp.tab) {
                        return Ok(());
                    }
                    let from = self.split_from(sp).and_then(|f| f.terminal.clone());
                    (from.or_else(|| self.any_terminal(|_| true)), false)
                }
            }
        } else {
            let from = self
                .split_from(sp)
                .filter(|f| f.tab == 0)
                .and_then(|f| f.terminal.clone());
            let any = || self.any_terminal(|q| q.layer.is_none() && q.tab == 0);
            (from.or_else(any), true)
        };
        let Some(origin) = origin else {
            return Ok(()); // no origin yet: next flush
        };
        // starts at the guess, not Rex's default
        let size = self.new_pane_size(win);
        let [cols, rows] = size;
        let origin_size = self
            .panes
            .by_terminal(origin.as_str())
            .and_then(|f| f.terminal_surface.as_ref())
            .map(|s| [s.view.cols, s.view.rows]);
        let pane = self.panes.add_for_window(win);
        pane.tab = sp.tab;
        let (pane_id, id) = (pane.id, pane.attach_id.expect("a requested pane"));
        let attach = pane::attach_words(&self.exe, &self.sock, self.transport, pane);
        let title = pane::title(&id);
        self.changes.forget_split(win);

        if !beside {
            self.act(Ask::OpenTabPage {
                pane: id,
                attach,
                title,
                origin,
                cols,
                rows,
            });
            return Ok(());
        }
        // SPI-663 (#125): a pane opened by `open` and then moved by
        // `place_beside` should wait the same way, while its placement is
        // pending and its hello says the unlaid size; nothing asks `open` yet
        self.panes.by_id_mut(pane_id).unwrap().awaiting_size = true;
        let dir = Direction::of_split(sp.side_by_side, sp.before);
        let axis = usize::from(!dir.across());
        let along = size[axis];
        // its share of the room it splits with its origin
        let ratio = match origin_size {
            Some(o) if o[axis] > 0 => (f64::from(along) / f64::from(o[axis])).clamp(0.0, 1.0),
            _ => 0.5,
        };
        self.act(Ask::OpenBeside {
            pane: id,
            attach,
            title,
            origin,
            dir,
            size: along,
            ratio,
            cols,
            rows,
        });
        log::trace(Point::Split, u64::from(pane_id));
        Ok(())
    }

    /// Window `win` had no pane of its own: it joins its origin pane's layout,
    /// else the current pane's, beside its origin window as Neovim put it,
    /// at `size` if Neovim gave it one. A window of a tab page made after
    /// setup stays unshown (#118).
    fn lay_out_in_origin(&mut self, win: i64, size: Option<(i64, u32, u32)>) {
        let sp = self.changes.split_of(win);
        if sp.tab != 0 {
            return;
        }
        let from = self
            .panes
            .by_win(sp.from)
            .or_else(|| self.current_pane())
            .filter(|p| p.layer.is_none() && p.tab == 0);
        let Some(id) = from.map(|p| p.id) else {
            return;
        };
        let equalalways = self.split_options.equalalways;
        let p = self.panes.by_id_mut(id).unwrap();
        let Some(shown) = p.neovim_window_handle else {
            return; // before setup: no window to split
        };
        let origin = if p.holds(sp.from) { sp.from } else { shown };
        p.layout
            .get_or_insert_with(|| PaneLayout::new(shown))
            .split(win, origin, sp.side_by_side, sp.before, equalalways);
        // Neovim said it was current before it was drawn
        if self.nvim.current_win == win {
            p.neovim_window_handle = Some(win);
        }
        self.changes.forget_split(win);
        if let Some((_, cols, rows)) = size {
            self.resize_in_layout(id, win, cols, rows);
        }
    }

    /// The origin pane's terminal for a window of a tab page with a tab page
    /// area: the pane it came from if that's in the same area, else any
    /// requested pane of the area. None for the area's first, or while its
    /// terminal is on its way.
    fn tab_page_origin(&self, sp: windows::Split) -> Option<TerminalId> {
        if let Some(f) = self.panes.by_win(sp.from)
            && f.tab == sp.tab
            && let Some(t) = &f.terminal
        {
            return Some(t.clone());
        }
        self.any_terminal(|q| q.tab == sp.tab && q.layer.is_none() && q.attach_id.is_some())
    }

    /// The terminal of the first pane `keep` keeps that has one.
    fn any_terminal(&self, keep: impl Fn(&Pane) -> bool) -> Option<TerminalId> {
        self.panes
            .list
            .iter()
            .filter(|q| keep(q))
            .find_map(|q| q.terminal.clone())
    }

    /// True while a pane of tab page `tab` asked for its Rex floating pane
    /// and has no answer: a refused request is no longer on its way.
    fn tab_floating_pane_pending(&self, tab: i64) -> bool {
        self.panes
            .list
            .iter()
            .any(|q| q.tab == tab && q.layer.is_none() && q.closing != Some(Close::RequestFailed))
    }

    /// The helper or terminal went away: its terminal hung up. Close its
    /// window, or end if it was the last pane.
    fn pane_gone(&mut self, id: u32) -> Result<()> {
        let i = self.panes.index_of(id).unwrap();
        let p = &mut self.panes.list[i];
        p.closing = Some(Close::TerminalHungUp);
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
        let wins = self.panes.list[i].windows();
        self.panes.remove(i, &self.event_loop);
        for &win in &wins {
            self.changes.mark_closing(win);
            windows::close(&mut self.nvim.client, win);
        }
        if !wins.is_empty() {
            self.nvim.client.flush()?;
        }
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
                        .respond_error(id, &format!("{NAME} serves no requests"))?;
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
            windows::Note::Options(o) => self.on_split_options(o)?,
            windows::Note::Resized { win, cols, rows } => self.on_resized(win, cols, rows)?,
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
            log_warn!("neovim error: {}", String::from_utf8_lossy(e));
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
            if let Some(f) = self.current_pane_id() {
                self.nvim.fit_ui_to(self.panes.by_id(f).unwrap())?;
            }
            self.reconcile()?;
            self.draw()?;
        }
        Ok(())
    }

    /// Neovim's current window changed: the pane provider focuses its pane.
    fn on_current(&mut self, win: i64) -> Result<()> {
        // sent before our own change ran; the report after it is coming
        if self.nvim.switch_in_flight().is_some() {
            return Ok(());
        }
        let from_tab = self.current_pane().map(|p| p.tab);
        self.nvim.current_float = 0;
        self.nvim.current_win = win;
        let Some(p) = self.panes.by_win_mut(win) else {
            return Ok(());
        };
        let id = p.id;
        // of several in a pane: it keeps a row, and isn't cut
        if p.layout.is_some() && p.neovim_window_handle != Some(win) {
            p.neovim_window_handle = Some(win);
            self.size_window(id);
        }
        self.trace_tab_move(from_tab, id);
        let p = self.panes.by_id(id).unwrap();
        self.nvim.fit_ui_to(p)?;
        self.draw()?;
        // (looked up again: drawing can remove a pane whose terminal is gone)
        let terminal = self.panes.by_id(id).and_then(|p| p.terminal.clone());
        self.focus_terminal(terminal.as_ref());
        Ok(())
    }

    /// The options a split or close goes by changed. A new separator is drawn
    /// in every pane laying out several windows.
    fn on_split_options(&mut self, o: windows::SplitOptions) -> Result<()> {
        let redraw = o.separator != self.split_options.separator;
        let refit = o.statuslines != self.split_options.statuslines;
        self.split_options = o;
        if !redraw && !refit {
            return Ok(());
        }
        for p in self.panes.list.iter_mut() {
            if p.layout.is_some()
                && let Some(s) = &mut p.terminal_surface
            {
                s.view.full = true;
            }
        }
        if refit {
            // statuslines of ours came or went: windows above others resize
            self.size_windows();
            self.nvim.client.flush()?;
        }
        self.draw()
    }

    /// Neovim sized window `win` itself, `cols` x `rows` with its statusline,
    /// as `:resize` does: in a pane laying out several windows, it takes that
    /// size there, and every window is fitted again. Elsewhere its pane's size
    /// is its size.
    fn on_resized(&mut self, win: i64, cols: u32, rows: u32) -> Result<()> {
        let Some(p) = self.panes.by_win(win) else {
            // not in a pane yet: for when it joins one
            self.pending_sizes.retain(|&(w, ..)| w != win);
            if self.pending_sizes.len() < MAX_PENDING_SIZES {
                self.pending_sizes.push((win, cols, rows));
            }
            return Ok(());
        };
        if p.layout.is_none() {
            return Ok(());
        }
        let id = p.id;
        self.resize_in_layout(id, win, cols, rows);
        self.size_window(id);
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Window `win` of pane `id`'s layout takes `cols` x `rows` there, a
    /// way given as 0 left as it is. Its windows are asked their sizes again.
    fn resize_in_layout(&mut self, id: u32, win: i64, cols: u32, rows: u32) {
        let p = self.panes.by_id(id).unwrap();
        let Some(layout) = &p.layout else {
            return;
        };
        let area = self.geometry().window_size(p);
        let current = p.neovim_window_handle.unwrap_or(win);
        let was = layout.lay_out(area, current).of(win);
        // told without the statusline neorex draws it
        let rows = match was {
            Some(w) if rows != 0 && self.draws_statusline(&w, area) => rows + 1,
            _ => rows,
        };
        let p = self.panes.by_id_mut(id).unwrap();
        let layout = p.layout.as_mut().unwrap();
        if cols != 0 && was.is_none_or(|w| w.cols != cols) {
            layout.resize(win, Axis::Cols, cols, area, current);
        }
        if rows != 0 && was.is_none_or(|w| w.rows != rows) {
            layout.resize(win, Axis::Rows, rows, area, current);
        }
        // asked again, at least the window Neovim sized
        p.layout_asked.clear();
    }

    /// `<C-w>h/j/k/l`: the pane provider moves focus to the pane that way, if
    /// any. It tells the new focus as the user's, and Neovim's current window
    /// follows. Without `focus_direction`, nothing moves.
    fn on_direction(&mut self, m: windows::Move) -> Result<()> {
        let dir = match m.dir {
            windows::Direction::Left => Direction::Left,
            windows::Direction::Down => Direction::Down,
            windows::Direction::Up => Direction::Up,
            windows::Direction::Right => Direction::Right,
        };
        let p = self.provider.as_ref().map(Held::seam_ref);
        if !self.asks.may_ask(p, Capability::FocusDirection) {
            return Ok(());
        }
        self.act(Ask::FocusDirection {
            dir,
            count: m.count.clamp(1, MAX_DIRECTION_COUNT),
        });
        Ok(())
    }

    /// A float became current: if it's in a floating pane, focus that.
    fn on_current_float(&mut self, win: i64) -> Result<()> {
        self.nvim.current_float = win;
        let Some(p) = floating_panes::layer_of_win(&self.grids, &self.panes, win) else {
            return Ok(());
        };
        let terminal = p.terminal.clone();
        self.focus_terminal(terminal.as_ref());
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
        self.reconcile_layers();
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
            if self.close_laid_out_windows(i) {
                continue;
            }
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
            // hanging up closes the terminal only once its helper has connected
            if p.terminal_surface.is_none()
                && let Some(t) = p.terminal.clone()
            {
                self.act(asks::close(t));
            }
            self.panes.remove(i, &self.event_loop);
        }
        if self.panes.list.is_empty() {
            self.stop(StopReason::PaneGone);
            return Ok(());
        }
        self.changes.forget_gone(&self.grids);
        let grids = &self.grids;
        self.pending_sizes
            .retain(|&(w, ..)| grids.grid_of(w).is_some());

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
        self.reconcile_layers();
        self.place_tab_pages()?;
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Takes the windows that closed out of pane `i`'s layout, if it lays out
    /// several. True if it still shows a window; else its last window is
    /// `neovim_window_handle` and it is left to end as a pane of one does.
    fn close_laid_out_windows(&mut self, i: usize) -> bool {
        let opts = pane_layout::Options {
            equalalways: self.split_options.equalalways,
            splitbelow: self.split_options.splitbelow,
            splitright: self.split_options.splitright,
        };
        let p = &mut self.panes.list[i];
        let Some(layout) = &mut p.layout else {
            return false;
        };
        let gone = |w: i64| self.grids.grid_of(w).is_none() && !self.changes.awaits_reopen(w);
        for w in layout.windows() {
            if gone(w) && !layout.close(w, opts) {
                // its last window: the pane ends with it
                p.neovim_window_handle = Some(w);
                p.layout = None;
                p.layout_asked.clear();
                return false;
            }
        }
        let left = layout.windows();
        if !p.neovim_window_handle.is_some_and(|w| left.contains(&w)) {
            p.neovim_window_handle = left.first().copied();
        }
        p.layout_asked.retain(|(w, _)| left.contains(w));
        if !layout.is_split() {
            // one window again: it fills the pane
            p.layout = None;
            p.layout_asked.clear();
            p.last_asked_win_size = LastAskedWinSize::default();
        }
        true
    }

    /// Neovim has a new layout for a tab page's windows, or none. Only a tab
    /// page with a tab page area, or one on its way, is placed: Neovim tells
    /// them all.
    fn on_tab_layout(&mut self, layout: Layout) -> Result<()> {
        if layout.len == 0 || !self.has_tab_page_area(layout.tab) {
            self.tab_layouts.swap_remove(&layout.tab);
            return Ok(());
        }
        match self.tab_layouts.get_mut(&layout.tab) {
            Some(tab) => tab.replace(layout),
            None => {
                self.tab_layouts.insert(layout.tab, Tab::new(layout));
            }
        }
        self.place_tab_pages()
    }

    /// Puts the panes of each tab page's windows where its layout has them,
    /// once they all have a terminal, and again when the windows, their order
    /// or their sizes change: the pane provider has them in the order their
    /// windows were made, which isn't where Neovim ends up putting them. Each
    /// move is a `place_beside` and each new size a `resize`, through the
    /// layout queue. Without `place_beside` nothing is placed; without
    /// `resize`, a change of sizes alone is left.
    fn place_tab_pages(&mut self) -> Result<()> {
        let p = self.provider.as_ref().map(Held::seam_ref);
        if !self.asks.may_ask(p, Capability::PlaceBeside) {
            return Ok(());
        }
        let resizes = self.asks.may_ask(p, Capability::Resize);
        let mut plans = Vec::new();
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
            let action =
                tab_placement::decide(&tab.layout, &states[..tab.layout.len], tab.placed.as_ref());
            match action {
                Action::Wait | Action::Nothing => {}
                Action::Forget => {
                    self.tab_layouts.swap_remove_index(i);
                }
                Action::Rearrange(plan) => {
                    tab.placed = Some(plan);
                    log::trace(Point::Place, layout.tab as u64);
                    plans.push((plan, Capability::PlaceBeside));
                }
                Action::Resize(plan) => {
                    tab.placed = Some(plan);
                    if resizes {
                        log::trace(Point::Place, layout.tab as u64);
                        plans.push((plan, Capability::Resize));
                    }
                }
            }
        }
        for (plan, capability) in plans {
            for m in plan.items() {
                if let Some(ask) = self.placement_ask(m, capability) {
                    self.act(ask);
                }
            }
        }
        Ok(())
    }

    /// The ask of `capability` for move `m`: the `place_beside` that makes it,
    /// or the `resize` that sizes its split. None if either of its panes has
    /// no terminal, or for any other capability. The ask's ratio is the moved
    /// terminal's share, `m.ratio` that of the pane it goes beside.
    fn placement_ask(&self, m: &PaneMove, capability: Capability) -> Option<Ask> {
        let Terminals { moved, origin } = self.terminals_of(m)?;
        let dir = Direction::of_split(m.side_by_side, false);
        let (size, ratio) = (m.size, 1.0 - m.ratio);
        Some(match capability {
            Capability::PlaceBeside => Ask::PlaceBeside {
                terminal: moved,
                origin,
                dir,
                size,
                ratio,
            },
            Capability::Resize => Ask::Resize {
                terminal: moved,
                origin,
                dir,
                size,
                ratio,
            },
            _ => return None,
        })
    }

    /// Whether tab page `tab` has a tab page area: a pane of its windows
    /// opened by `open_tab_page` or beside one, still wanted, or a window of
    /// it waiting for its pane.
    fn has_tab_page_area(&self, tab: i64) -> bool {
        if tab == 0 {
            return false;
        }
        let panes =
            self.panes.list.iter().any(|p| {
                p.tab == tab && p.layer.is_none() && p.closing != Some(Close::RequestFailed)
            });
        panes || self.changes.splits.values().any(|s| s.split.tab == tab)
    }

    /// The terminals of the panes a move is between, if both have one.
    fn terminals_of(&self, m: &PaneMove) -> Option<Terminals> {
        let moved = self.panes.by_win(m.win)?.terminal.clone()?;
        let origin = self.panes.by_win(m.beside)?.terminal.clone()?;
        Some(Terminals { moved, origin })
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
        // a tab page's first opens its tab page area: Neovim's screen's size
        let first_of_tab = sp.tab != 0 && self.tab_page_origin(sp).is_none();
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
        self.current_pane()
    }

    // The pane provider

    /// Rex's pane provider, for its session's attach and connection, which
    /// aren't on the seam until #115.
    fn rex(&mut self) -> Option<&mut RexProvider> {
        self.provider.as_mut().and_then(Held::rex)
    }

    fn rex_ref(&self) -> Option<&RexProvider> {
        match &self.provider {
            Some(Held::Rex(r)) => Some(r),
            _ => None,
        }
    }

    /// Asks the pane provider to focus terminal `t`, unless it has or will
    /// once a focus asked lands, or can't.
    fn focus_terminal(&mut self, t: Option<&TerminalId>) {
        let Some(t) = t else {
            return;
        };
        let p = self.provider.as_ref().map(Held::seam_ref);
        if self.focus.has_focused(t) || !self.asks.may_ask(p, Capability::Focus) {
            return;
        }
        self.focus.asked(t.clone());
        self.act(Ask::Focus {
            terminal: t.clone(),
        });
    }

    /// Asks the pane provider for `ask`, or falls back without it. What
    /// became of it, `run_steps` acts on.
    pub(crate) fn act(&mut self, ask: Ask) {
        let p = self.provider.as_mut().map(Held::seam);
        self.asks.act(p, ask, Instant::now());
    }

    /// Matches float group panes to groups of floats (see `floating_panes`).
    fn reconcile_layers(&mut self) {
        let p = self.provider.as_ref().map(Held::seam_ref);
        let open_float = self.asks.may_ask(p, Capability::OpenFloat);
        let origin = self
            .current_pane()
            .and_then(|f| f.terminal.clone())
            .or_else(|| self.any_terminal(|_| true));
        for ask in self.floating().reconcile_layers(open_float, origin) {
            self.act(ask);
        }
    }

    fn on_provider(&mut self, token: usize) -> Result<()> {
        if let Some(p) = &mut self.provider {
            p.seam().on_ready(token);
        }
        self.drain_provider()
    }

    /// Handles what the pane provider has to tell, then what became of the
    /// acts.
    pub(crate) fn drain_provider(&mut self) -> Result<()> {
        while let Some(e) = self.provider.as_mut().and_then(|p| p.seam().next()) {
            match e {
                ProviderEvent::Answered { id, outcome } => {
                    if let Some(p) = &mut self.provider {
                        self.asks.answered(p.seam(), id, outcome, Instant::now());
                    }
                }
                ProviderEvent::Layout(l) => self.on_live_layout(l)?,
                ProviderEvent::Focused(t) => self.on_focused(&t, Instant::now())?,
                ProviderEvent::Closed(t) => {
                    if let Some(id) = self.panes.by_terminal(t.as_str()).map(|p| p.id) {
                        self.pane_gone(id)?;
                    }
                }
                ProviderEvent::Gone { reason } => self.provider_gone(&reason)?,
            }
        }
        self.run_steps()?;
        self.arm_ask_timer()
    }

    /// The pane provider failed as a whole: it's dropped, and every act
    /// asked of it or waiting falls back. Its live layout goes: a pane it
    /// hadn't reported is its own area, and the rest stay as they are.
    fn provider_gone(&mut self, reason: &str) -> Result<()> {
        log_warn!("{NAME} daemon: dropping the pane provider: {reason}");
        if let Some(fd) = self.rex_ref().map(|r| r.session.client.fd()) {
            let _ = self.event_loop.remove(fd);
        }
        self.provider = None;
        self.focus = ProviderFocus::default();
        self.just_opened.clear();
        self.asks.gone();
        self.live_layout = None;
        // nothing will lay a new split out now: its size is its own
        self.layout_wait_over()?;
        self.make_own(|p| p.area == Area::Unreported)
    }

    /// The ask timer fired: the acts whose time is up fail.
    fn time_out_asks(&mut self) -> Result<()> {
        self.ask_timer_due = None;
        if let Some(p) = &mut self.provider {
            self.asks.time_out(p.seam(), Instant::now());
        }
        self.run_steps()?;
        self.arm_ask_timer()
    }

    /// Starts the ask timer for the soonest act's deadline, unless it
    /// already runs for one that soon.
    fn arm_ask_timer(&mut self) -> Result<()> {
        let Some(due) = self.asks.deadline() else {
            return Ok(());
        };
        if self.ask_timer_due.is_some_and(|armed| armed <= due) {
            return Ok(());
        }
        // round up, so the timer doesn't fire before the deadline
        let ms = due
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        self.ask_timer_due = Some(due);
        self.add_timer(Token::AskTimer, u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// Does what became of the acts.
    fn run_steps(&mut self) -> Result<()> {
        while let Some(step) = self.asks.next_step() {
            if self.asks.settles_placement(&step) {
                log::trace(Point::Placed, 0);
            }
            match step {
                Step::Done(ask, Answer::Terminal(t)) => self.on_opened(ask, t)?,
                Step::Done(Ask::ReportLayout, Answer::Layout(l)) => self.on_live_layout(l)?,
                Step::Done(Ask::Focus { .. }, _) => self.focus.ended(true, Instant::now()),
                Step::Done(..) => {}
                Step::FallBack(Ask::Focus { .. }) | Step::Replaced(Ask::Focus { .. }) => {
                    self.focus.ended(false, Instant::now())
                }
                Step::Replaced(_) => {}
                Step::FallBack(ask) => self.fall_back(ask)?,
                Step::Warn(line) => {
                    self.warn_in_nvim(&line);
                    self.nvim.client.flush()?;
                }
            }
        }
        Ok(())
    }

    /// The pane provider opened the terminal `ask` asked for, as `t`: link
    /// it to its pane, and focus it if its window is current.
    fn on_opened(&mut self, ask: Ask, t: TerminalId) -> Result<()> {
        let Some(attach_id) = opened_pane(&ask) else {
            return Ok(());
        };
        // its window or floats closed while it opened: nothing will draw there
        let Some(pane_id) = self.panes.by_attach_id(&attach_id).map(|p| p.id) else {
            self.act(asks::close(t));
            return Ok(());
        };
        let p = self.panes.by_id_mut(pane_id).unwrap();
        p.terminal = Some(t.clone());
        let handle = p.neovim_window_handle;
        // a layout that came before the terminal's id placed it already
        if let Some(l) = self.live_layout.clone() {
            self.on_live_layout(l)?;
        }
        self.wait_for_report(pane_id, Instant::now())?;
        // made, it may take focus from the current window's: given back,
        // whether its focus was told before this or comes after (#86 D8)
        if !self.shows_current(pane_id) {
            if self.focus.settled_on(&t) {
                self.give_focus_back();
            } else {
                let due = Instant::now() + Duration::from_millis(JUST_OPENED_MS);
                self.just_opened.push((t.clone(), due));
            }
        }
        match ask {
            Ask::OpenBeside { .. } => {
                if handle == Some(self.nvim.current_win) {
                    self.focus_terminal(Some(&t));
                }
                self.place_tab_pages()
            }
            Ask::OpenTabPage { .. } => {
                if handle == Some(self.nvim.current_win) {
                    self.focus_terminal(Some(&t));
                }
                // its tab page's other windows split it
                self.reconcile()?;
                self.draw()
            }
            Ask::OpenFloat { .. } => {
                if self.nvim.current_float != 0
                    && floating_panes::layer_of_win(
                        &self.grids,
                        &self.panes,
                        self.nvim.current_float,
                    )
                    .map(|p| p.id)
                        == Some(pane_id)
                {
                    self.focus_terminal(Some(&t));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// An act that couldn't be done. A pane whose open failed is given up,
    /// as before the seam: a float group pane's floats stay drawn in the pane,
    /// and a tab page's other windows ask for their own (#112 draws a window
    /// in its origin pane instead).
    fn fall_back(&mut self, ask: Ask) -> Result<()> {
        if ask == Ask::ReportLayout {
            return self.report_failed();
        }
        let Some(attach_id) = opened_pane(&ask) else {
            return Ok(());
        };
        let Some(pane_id) = self.panes.by_attach_id(&attach_id).map(|p| p.id) else {
            return Ok(());
        };
        self.panes.by_id_mut(pane_id).unwrap().closing = Some(Close::RequestFailed);
        self.reconcile()?;
        self.draw()
    }

    /// The pane provider says it has focused `t`, at `now`. The user's move
    /// is followed; a terminal just opened that took focus gives it back.
    fn on_focused(&mut self, t: &TerminalId, now: Instant) -> Result<()> {
        self.just_opened.retain(|(_, due)| now < *due);
        let fresh = match self.just_opened.iter().position(|(o, _)| o == t) {
            Some(i) => {
                self.just_opened.swap_remove(i);
                true
            }
            None => false,
        };
        if !self.focus.reported(t) {
            return Ok(());
        }
        let shown = self.panes.by_terminal(t.as_str()).map(|p| p.id);
        if fresh && shown.is_some_and(|id| !self.shows_current(id)) {
            self.give_focus_back();
            return Ok(());
        }
        self.on_provider_focus(t)
    }

    /// Asks the pane provider to focus the current pane's terminal again.
    fn give_focus_back(&mut self) {
        let current = self.current_pane().and_then(|p| p.terminal.clone());
        self.focus_terminal(current.as_ref());
    }

    /// The user focused one of our panes's terminals: make its window current.
    fn on_provider_focus(&mut self, t: &TerminalId) -> Result<()> {
        let Some(p) = self.panes.by_terminal(t.as_str()) else {
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
        if self.is_current_win(win) {
            return Ok(());
        }
        let from_tab = self.current_pane().map(|p| p.tab);
        self.nvim.current_float = 0;
        self.make_current(id)?;
        self.trace_tab_move(from_tab, id);
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Traces pane `id` becoming the current pane after a pane of tab page
    /// `from_tab`, if that's another tab page's: its panes are to be drawn. Not from a pane
    /// that's gone.
    fn trace_tab_move(&self, from_tab: Option<i64>, id: u32) {
        if log::tracing() && self.moves_tab_page(from_tab, id) {
            log::trace(Point::TabMove, u64::from(id));
        }
    }

    /// Whether pane `id` is of another tab page than `from_tab`, a pane's
    /// that's still there.
    fn moves_tab_page(&self, from_tab: Option<i64>, id: u32) -> bool {
        from_tab.is_some_and(|from| self.panes.by_id(id).is_some_and(|p| p.tab != from))
    }

    /// A live layout came: the panes it names join their areas (ADR 0008).
    /// One that says what the last did, to panes that are as it left them,
    /// changes nothing to size or draw. No terminal is moved.
    fn on_live_layout(&mut self, l: LiveLayout) -> Result<()> {
        let changed = self.apply_live_layout(&l);
        if !changed && self.live_layout.as_ref() == Some(&l) {
            return Ok(());
        }
        self.live_layout = Some(l);
        self.arm_area_timer()?;
        self.refit_to_areas()
    }

    /// Gives the panes `l` names their areas, and has the floats of a float
    /// group pane the user moved follow it. A pane it doesn't name keeps its
    /// area. Whether any pane's area changed.
    fn apply_live_layout(&mut self, l: &LiveLayout) -> bool {
        let mut changed = false;
        for i in 0..self.panes.list.len() {
            let p = &mut self.panes.list[i];
            let Some(area) = p.terminal.as_ref().and_then(|t| l.area_of(t)) else {
                continue;
            };
            p.report_due = None;
            if p.area == area {
                continue;
            }
            let (id, layer, rect) = (p.id, p.layer.is_some(), area.rect());
            let inset = p
                .terminal
                .as_ref()
                .and_then(|t| l.placed(t))
                .map_or([0; 4], |(_, t)| t.inset);
            p.area = area;
            changed = true;
            if layer && let Some(rect) = rect {
                self.floating().follow_moved_terminal(id, rect, inset);
            }
        }
        changed
    }

    /// The panes' areas changed: windows fit their panes again, with or
    /// without the tabline, Neovim's screen fits the current pane's area, and
    /// everything is drawn.
    fn refit_to_areas(&mut self) -> Result<()> {
        self.size_windows();
        if let Some(f) = self.current_pane_id() {
            self.nvim.fit_ui_to(self.panes.by_id(f).unwrap())?;
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Whether the pane provider can give a live layout.
    fn reports_layouts(&self) -> bool {
        let p = self.provider.as_ref().map(Held::seam_ref);
        self.asks.may_ask(p, Capability::ReportLayout)
    }

    /// Pane `id` may have just got the last of its terminal and its terminal
    /// id: an `Unreported` one then waits a second for the live layout to
    /// report it, and asks for a report of a pane provider that doesn't tell
    /// them unasked. With no live layout to come, or no id to be found by,
    /// it is its own layout area at once.
    fn wait_for_report(&mut self, id: u32, now: Instant) -> Result<()> {
        let reports = self.reports_layouts();
        let Some(p) = self.panes.by_id_mut(id) else {
            return Ok(());
        };
        if p.area != Area::Unreported || p.terminal_surface.is_none() || p.report_due.is_some() {
            return Ok(());
        }
        // a requested pane's id is on its way
        if p.terminal.is_none() && p.attach_id.is_some() && reports {
            return Ok(());
        }
        if !reports || p.terminal.is_none() {
            p.area = Area::Own;
            // before setup, setup fits everything
            if !self.nvim.ready {
                return Ok(());
            }
            return self.refit_to_areas();
        }
        p.report_due = Some(now + Duration::from_millis(u64::from(LAYOUT_WAIT_MS)));
        self.arm_area_timer()?;
        let told = self
            .provider
            .as_ref()
            .is_some_and(|p| p.seam_ref().capabilities().has(Capability::Events));
        if !told {
            self.act(Ask::ReportLayout);
        }
        Ok(())
    }

    /// The area timer fired: a pane still unreported a second after its
    /// terminal attached with its id is its own layout area.
    fn time_out_reports(&mut self, now: Instant) -> Result<()> {
        self.area_timer_due = None;
        self.make_own(|p| p.area == Area::Unreported && p.report_due.is_some_and(|d| d <= now))?;
        self.arm_area_timer()
    }

    /// Makes each pane `pick` picks its own layout area, waiting for no
    /// report, and fits everything again if any changed.
    fn make_own(&mut self, pick: impl Fn(&Pane) -> bool) -> Result<()> {
        let mut any = false;
        for p in self.panes.list.iter_mut().filter(|p| pick(p)) {
            p.report_due = None;
            if p.area != Area::Own {
                p.area = Area::Own;
                any = true;
            }
        }
        if any {
            self.refit_to_areas()?;
        }
        Ok(())
    }

    /// Starts the area timer for the soonest pane's wait for its report,
    /// unless it already runs for one that soon.
    fn arm_area_timer(&mut self) -> Result<()> {
        let Some(due) = self.panes.list.iter().filter_map(|p| p.report_due).min() else {
            return Ok(());
        };
        if self.area_timer_due.is_some_and(|armed| armed <= due) {
            return Ok(());
        }
        // round up, so the timer doesn't fire before the deadline
        let ms = due
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        self.area_timer_due = Some(due);
        self.add_timer(Token::AreaTimer, u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// A `report_layout` couldn't be done. A failed one keeps the last
    /// layout; one found unsupported makes every pane its own area.
    fn report_failed(&mut self) -> Result<()> {
        if self.provider.is_none() || self.reports_layouts() {
            return Ok(());
        }
        self.live_layout = None;
        self.make_own(|_| true)
    }

    /// The borrows of the floating panes' lifecycle (see `floating_panes`).
    fn floating(&mut self) -> FloatingPanes<'_> {
        FloatingPanes {
            exe: &self.exe,
            sock: &self.sock,
            transport: self.transport,
            grids: &self.grids,
            panes: &mut self.panes,
            event_loop: &self.event_loop,
            nvim: &mut self.nvim,
        }
    }

    // shared

    /// Draws every pane that changed.
    fn draw(&mut self) -> Result<()> {
        let focused = self.current_pane_id();
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
            // frames were dropped for a terminal that wasn't reading: no frame
            // until it has caught up, when it's drawn in full; where its input
            // goes is kept current all the same
            let caught_up = !p.terminal_surface.as_ref().unwrap().backlog.dropped();
            let drawn = if let Some(l) = p.layer {
                let members = float_groups::members(&self.grids, &l.group);
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                // the group's own editor float, even when the cap on members dropped it
                s.forwarder.grid = l.group.top_grids().first().copied().unwrap_or(1);
                caught_up
                    && s.view.frame_floats(
                        &mut self.grids,
                        &members,
                        [l.group.row, l.group.col],
                        &mut s.writer,
                    )
            } else if caught_up && p.layout.is_some() {
                let (wins, separators) = self.laid_out_view(p);
                let origin = self.geometry().pane_origin(p);
                let mut buf = [0u8; 4];
                let separator = Cell::init(
                    self.split_options.separator.encode_utf8(&mut buf),
                    self.grids.separator_hl,
                );
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                s.view.frame_laid_out(
                    &mut self.grids,
                    LaidOut {
                        wins: &wins,
                        separators: &separators,
                        separator,
                    },
                    Opts {
                        focused: focused == Some(id),
                        origin,
                        skip: &skip,
                        tabline: tabline.cells.as_deref(),
                        tabline_col: tabline.col,
                        tabline_dirty,
                    },
                    &mut s.writer,
                )
            } else if caught_up {
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
                    &mut s.writer,
                )
            } else {
                false
            };
            let p = &mut self.panes.list[i];
            let token = p.token(PaneFd::Terminal);
            let s: &mut Surface = p.terminal_surface.as_deref_mut().unwrap();
            // the floats of the frame last drawn: unchanged while frames are
            // held back, as no frame is built
            set_targets(&mut s.forwarder, s.view.drawn_floats());
            if !drawn {
                continue;
            }
            // never waits: what the terminal can't take yet waits until it has room
            match s.send_frame() {
                Err(_) => self.pane_gone(id)?,
                // traced once the rest goes out (see `on_writable`)
                Ok(Sent::Waiting) => self.event_loop.add_write(s.output_fd(), token)?,
                Ok(Sent::All) => log::trace(Point::Frame, u64::from(id)),
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
    /// Whether it asked.
    fn size_window(&mut self, id: u32) -> bool {
        let Some(p) = self.panes.by_id(id) else {
            return false;
        };
        if p.terminal_surface.is_none() || p.awaiting_size {
            return false;
        }
        if p.layout.is_some() {
            return self.size_laid_out_windows(id);
        }
        let Some(win) = p.neovim_window_handle else {
            return false;
        };
        let Some(grid) = self.grids.grid_of(win) else {
            return false;
        };
        let size = self.geometry().window_size(p);
        let last = p.last_asked_win_size;
        if last.grid == grid && last.cols == size[0] && last.rows == size[1] {
            return false;
        }
        self.panes.by_id_mut(id).unwrap().last_asked_win_size = LastAskedWinSize {
            grid,
            cols: size[0],
            rows: size[1],
        };
        windows::size(&mut self.nvim.client, win, size[0], size[1]);
        true
    }

    /// Asks Neovim for each window of pane `id`'s layout to fit its place,
    /// unless it already was. A window cut for want of room keeps its size.
    /// Whether it asked for any.
    fn size_laid_out_windows(&mut self, id: u32) -> bool {
        let p = self.panes.by_id(id).unwrap();
        let area = self.geometry().window_size(p);
        let laid = p
            .layout
            .as_ref()
            .unwrap()
            .lay_out(area, p.neovim_window_handle.unwrap_or(0));
        let mut asks = Vec::new();
        for w in &laid.wins {
            let Some(grid) = self.grids.grid_of(w.win) else {
                continue;
            };
            // a statusline of ours takes the last row
            let ours = u32::from(self.draws_statusline(w, area));
            let want = LastAskedWinSize {
                grid,
                cols: w.cols,
                rows: (w.rows - ours).max(1),
            };
            if !p.layout_asked.contains(&(w.win, want)) {
                asks.push((w.win, want));
            }
        }
        let p = self.panes.by_id_mut(id).unwrap();
        for &(win, want) in &asks {
            p.layout_asked.retain(|(w, _)| *w != win);
            p.layout_asked.push((win, want));
            windows::size(&mut self.nvim.client, win, want.cols, want.rows);
        }
        !asks.is_empty()
    }

    /// The windows of pane `p`'s layout as drawn, and the separators between.
    fn laid_out_view(&self, p: &Pane) -> (Vec<Shown>, Vec<Separator>) {
        let Some(layout) = &p.layout else {
            return (Vec::new(), Vec::new());
        };
        let area = self.geometry().window_size(p);
        let laid = layout.lay_out(area, p.neovim_window_handle.unwrap_or(0));
        let wins = laid
            .wins
            .iter()
            .filter_map(|w| {
                let hl = if w.win == self.nvim.current_win {
                    self.grids.statusline_hl
                } else {
                    self.grids.statusline_nc_hl
                };
                Some(Shown {
                    grid: self.grids.grid_of(w.win)?,
                    row: w.row,
                    col: w.col,
                    cols: w.cols,
                    rows: w.rows,
                    statusline: self.draws_statusline(w, area).then_some(hl),
                })
            })
            .collect();
        (wins, laid.separators)
    }

    /// Whether neorex draws window `w`, laid out in `area`, its statusline: a
    /// window with another below it always has one, and with `'laststatus'`
    /// 0 Neovim draws it none.
    fn draws_statusline(&self, w: &pane_layout::Placed, area: [u32; 2]) -> bool {
        !self.split_options.statuslines && w.row + w.rows < area[1]
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
    fn current_pane(&self) -> Option<&Pane> {
        if self.nvim.current_win == 0 {
            return self.panes.by_id(STARTUP_PANE_ID);
        }
        self.panes.by_win(self.nvim.current_win)
    }

    fn current_pane_id(&self) -> Option<u32> {
        self.current_pane().map(|p| p.id)
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
    // a self-attached pane is never moved
    if p.closing.is_some() || p.attach_id.is_none() {
        return WindowState::Skipped;
    }
    if p.terminal.is_none() {
        WindowState::Pending
    } else {
        WindowState::Ready
    }
}

/// The pane an open asked a terminal for.
fn opened_pane(ask: &Ask) -> Option<crate::attach::PaneId> {
    match ask {
        Ask::Open { pane, .. }
        | Ask::OpenBeside { pane, .. }
        | Ask::OpenFloat { pane, .. }
        | Ask::OpenTabPage { pane, .. } => Some(*pane),
        _ => None,
    }
}

/// A note, or None for one that's no note of ours or we can't read: a
/// message we don't understand shouldn't end the daemon.
fn parse_note_or_log(method: &[u8], params: &mut Decoder<'_>) -> Option<windows::Note> {
    match windows::parse_note(method, params) {
        Ok(note) => note,
        Err(err) => {
            log_warn!(
                "{NAME} daemon: unreadable {} note: {err}",
                String::from_utf8_lossy(method)
            );
            None
        }
    }
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
    use super::super::live_layout::{LayoutArea, PlacedTerminal};
    use super::super::provider::fake::FakeProvider;
    use super::super::provider::{CapabilitySet, Outcome};
    use super::*;
    use crate::nvim::nvim::for_test_on;
    use crate::nvim::windows::{Kind, Node};
    use crate::rex::client::Client;
    use crate::rex::session::tests::server_frame;
    use crate::rex::session::{Conn, Session as RexSession};
    use crate::screen::grid::Grid;
    use crate::screen::grids::{Float, Win};
    use serde_json::Value;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    /// A daemon with no Neovim behind it, only a socket that nothing
    /// answers, and a fake pane provider that can open and close.
    struct Fixture {
        d: Daemon,
        nvim_end: UnixStream,
    }

    fn fixture() -> Fixture {
        with_provider(Some(Held::Fake(FakeProvider::opening())))
    }

    fn with_provider(provider: Option<Held>) -> Fixture {
        let (nvim_ours, nvim_end) = UnixStream::pair().unwrap();
        let d = Daemon::new(
            Loop::init().unwrap(),
            NvimSession::new(for_test_on(nvim_ours)),
            provider,
            String::new(),
            crate::daemon::dir::Address::from_arg(Default::default()),
        );
        Fixture { d, nvim_end }
    }

    /// A fixture whose pane provider is Rex's, over a socket nothing answers,
    /// for what isn't asked through the seam yet: focus and layout. Rex's end
    /// reads what it is sent.
    fn rex_fixture() -> (Fixture, UnixStream) {
        let (rex_ours, rex_end) = UnixStream::pair().unwrap();
        let mut rex = RexSession::new(Client::from_stream(rex_ours), "session:s".to_string());
        rex.conn = Conn::Attached;
        let rex = Held::Rex(Box::new(RexProvider::new(rex)));
        (with_provider(Some(rex)), rex_end)
    }

    /// The fake pane provider of `d`.
    fn fake(d: &mut Daemon) -> &mut FakeProvider {
        d.provider.as_mut().unwrap().fake()
    }

    /// A daemon address for one test, listening.
    struct Address {
        path: std::path::PathBuf,
        server: UnixListener,
    }

    impl Address {
        fn new(tag: &str) -> Address {
            let path =
                std::env::temp_dir().join(format!("{NAME}-{tag}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let server = UnixListener::bind(&path).unwrap();
            Address { path, server }
        }

        /// A helper's connection that sends `hello`, `tty` riding on it.
        fn send(&self, hello: &[u8], tty: Option<i32>) -> UnixStream {
            let helper = UnixStream::connect(&self.path).unwrap();
            match tty {
                Some(fd) => crate::daemon::fdpass::send(&helper, hello, fd).unwrap(),
                None => (&helper).write_all(hello).unwrap(),
            }
            helper
        }
    }

    impl Drop for Address {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A pty of `cols` x `rows`, closed as it drops.
    struct Pty {
        master: i32,
        slave: i32,
    }

    impl Pty {
        fn new(cols: u16, rows: u16) -> Pty {
            let (master, slave) = surface::open_pty(cols, rows);
            Pty { master, slave }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.master);
                libc::close(self.slave);
            }
        }
    }

    /// The first frame `helper` is sent, within a second.
    fn answer(helper: &UnixStream) -> crate::attach::Message {
        helper
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(m) = crate::daemon::attach_conn::next_message(&mut got).unwrap() {
                return m;
            }
            let n = (&*helper).read(&mut buf).unwrap();
            assert!(n > 0, "hung up without an answer");
            got.extend_from_slice(&buf[..n]);
        }
    }

    /// The refusal `helper` was sent, before it was hung up on.
    fn refusal(helper: &UnixStream) -> crate::attach::End {
        let said = crate::daemon::attach_conn::read_all_frames(helper);
        match &said[..] {
            [crate::attach::Message::End(z)] => z.clone(),
            _ => panic!("{said:?}"),
        }
    }

    /// What the daemon has sent Neovim and Neovim hasn't read, as text.
    fn sent_to_nvim(fx: &mut Fixture) -> String {
        fx.nvim_end.set_nonblocking(true).unwrap();
        let mut sent = Vec::new();
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = fx.nvim_end.read(&mut buf) {
            sent.extend_from_slice(&buf[..n]);
        }
        String::from_utf8_lossy(&sent).into_owned()
    }

    /// A window's pane waiting for its terminal, its window drawn at 80 x 23:
    /// its id and pane id.
    fn requested_pane(fx: &mut Fixture) -> (u32, crate::attach::PaneId) {
        draw_external(&mut fx.d, 2, 1000, 80, 23);
        let p = fx.d.panes.add_for_window(1000);
        (p.id, p.attach_id.unwrap())
    }

    fn hello_for(pane: crate::attach::PaneId, cols: u16, rows: u16) -> Vec<u8> {
        use crate::daemon::attach_conn::{requested_hello, test_terminal};
        requested_hello(pane, test_terminal(cols, rows))
    }

    /// Whether a split's pane, opened beside its origin, still waits for its
    /// layout once its terminal attached saying `cols` x `rows`, the fake
    /// starting its terminals at `unlaid`.
    fn waits_for_layout(unlaid: Option<[u32; 2]>, cols: u16, rows: u16) -> Fixture {
        let mut fx = fixture();
        fake(&mut fx.d).unlaid = unlaid;
        let (id, pane) = requested_pane(&mut fx);
        fx.d.panes.by_id_mut(id).unwrap().awaiting_size = true;
        let at = Address::new(&format!("wait-{cols}"));
        let pty = Pty::new(cols, rows);
        let helper = at.send(&hello_for(pane, cols, rows), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        let crate::attach::Message::Accept(_) = answer(&helper) else {
            panic!("not accepted")
        };
        fx
    }

    fn awaiting(fx: &Fixture) -> bool {
        fx.d.panes.list.iter().any(|p| p.awaiting_size)
    }

    #[test]
    fn a_split_waits_for_its_layout_only_at_the_pane_providers_unlaid_size_until_it_is_dropped() {
        assert!(awaiting(&waits_for_layout(Some([80, 24]), 80, 24)));
        // laid out already, or a pane provider whose terminals start at no size
        assert!(!awaiting(&waits_for_layout(Some([80, 24]), 70, 20)));
        assert!(!awaiting(&waits_for_layout(None, 80, 24)));
        // the pane provider dropped: nothing will lay it out
        let mut fx = waits_for_layout(Some([80, 24]), 80, 24);
        fake(&mut fx.d).push(ProviderEvent::Gone {
            reason: "hung up".into(),
        });
        fx.d.drain_provider().unwrap();
        assert!(!awaiting(&fx));
    }

    #[test]
    fn a_requested_attach_is_accepted_at_its_hellos_size() {
        let mut fx = fixture();
        fx.d.name = "proj".into();
        fx.d.log = "/tmp/proj/log".into();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("accept");
        let pty = Pty::new(90, 30);
        // the hello says 70 x 20: the daemon never asks the terminal
        let helper = at.send(&hello_for(pane, 70, 20), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();

        let crate::attach::Message::Accept(a) = answer(&helper) else {
            panic!("not accepted")
        };
        assert_eq!(
            (
                crate::attach::ATTACH_PROTOCOL_VERSION,
                crate::attach::build()
            ),
            (a.v, a.build.as_str())
        );
        assert_eq!(
            ("proj", &b"/tmp/proj/log"[..]),
            (a.daemon.as_str(), &a.log[..])
        );
        let p = fx.d.panes.by_id(id).unwrap();
        let v = &p.terminal_surface.as_ref().unwrap().view;
        assert_eq!((70, 20), (v.cols, v.rows));
        assert_eq!(
            (70, 19),
            (p.last_asked_win_size.cols, p.last_asked_win_size.rows)
        );
        assert!(fx.d.pending.is_empty());
    }

    /// Pane `pane`'s terminal of `size`, attached over the relay by the
    /// relay test tool, with no pty; its hello is read as it comes.
    fn attach_relayed(
        fx: &mut Fixture,
        at: &Address,
        pane: crate::attach::PaneId,
        size: crate::attach::Size,
    ) -> crate::debug::relay::RelayPane {
        let stream = UnixStream::connect(&at.path).unwrap();
        let mut term = crate::debug::relay::RelayPane::on(stream, pane, size).unwrap();
        fx.d.accept(&at.server).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(&(token, _)) = fx.d.pending.first() {
            assert!(Instant::now() < deadline, "no hello");
            term.flush().unwrap();
            fx.d.on_pending(token).unwrap();
        }
        term
    }

    /// A pane attached over the relay, by the relay test tool: no pty.
    #[test]
    fn a_pane_attached_over_the_relay_round_trips_typing_and_drawing() {
        use crate::debug::relay::RelayPane;
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        fx.d.nvim.current_win = 1000;
        let (id, pane) = requested_pane(&mut fx);
        let g = fx.d.grids.get_mut(2).unwrap();
        for (i, c) in "drawn".chars().enumerate() {
            g.cells[i] = Cell::init(c.to_string(), 0);
        }
        let at = Address::new("relay");
        let mut term = attach_relayed(&mut fx, &at, pane, surface::cells(80, 24));

        // drawn, in `o` frames
        let shown = |t: &RelayPane| String::from_utf8_lossy(t.output()).contains("drawn");
        assert!(term.read_until(Duration::from_secs(2), shown).unwrap());
        assert!(term.accepted().is_some());
        assert_eq!(&crate::term::modes::undo(), term.undo());
        let p = fx.d.panes.by_id(id).unwrap();
        let v = &p.terminal_surface.as_ref().unwrap().view;
        assert_eq!((80, 24), (v.cols, v.rows));

        // typed, in `i` frames, and sized, in an `s` frame
        let _ = sent_to_nvim(&mut fx);
        term.type_bytes(b"typed").unwrap();
        term.resize(surface::cells(100, 30)).unwrap();
        fx.d.on_helper(id).unwrap();
        let sent = sent_to_nvim(&mut fx);
        assert!(sent.contains("typed"), "{sent:?}");
        let v = &fx.d.surface_of(id).view;
        assert_eq!((100, 30), (v.cols, v.rows));

        // the end: a `z`, after the last `o`
        fx.d.stop(StopReason::PaneGone);
        drop(fx);
        let ended = |t: &RelayPane| t.end().is_some();
        assert!(term.read_until(Duration::from_secs(2), ended).unwrap());
        let z = term.end().unwrap();
        assert_eq!(
            (crate::attach::Ending::Closed, false),
            (z.as_ending(), z.cut)
        );
    }

    #[test]
    fn an_s_frame_resizes_the_window() {
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("resize");
        let pty = Pty::new(80, 24);
        let mut helper = at.send(&hello_for(pane, 80, 24), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        assert!(matches!(answer(&helper), crate::attach::Message::Accept(_)));

        let size = crate::attach::Size {
            cols: 100,
            rows: 40,
            px: Some((1000, 800)),
        };
        helper
            .write_all(&crate::attach::Message::Size(size).encode().unwrap())
            .unwrap();
        fx.d.on_helper(id).unwrap();
        let p = fx.d.panes.by_id(id).unwrap();
        let v = &p.terminal_surface.as_ref().unwrap().view;
        assert_eq!((100, 40), (v.cols, v.rows));
        assert_eq!(
            (100, 39),
            (p.last_asked_win_size.cols, p.last_asked_win_size.rows)
        );
        // the pty kept its size: only the frame said
        let ws = crate::daemon::pane_helper::size_of(pty.slave).unwrap();
        assert_eq!((80, 24), (ws.cols, ws.rows));
    }

    #[test]
    fn a_hello_of_another_version_is_refused_naming_both_builds_and_versions() {
        use crate::attach::{Hello, Message, Purpose, Refusal};
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("version");
        let mut hello = Hello::new(Purpose::Requested {
            pane,
            terminal: crate::daemon::attach_conn::test_terminal(80, 24),
        });
        hello.v = 2;
        hello.build = format!("{NAME} 9.9.9");
        let helper = at.send(&Message::Hello(hello).encode().unwrap(), None);
        fx.d.accept(&at.server).unwrap();
        let z = refusal(&helper);
        assert_eq!(Some(Refusal::Version), z.as_refusal());
        for part in [
            format!("{NAME} 9.9.9 speaks attach protocol 2"),
            format!("{} speaks 1", crate::attach::build()),
        ] {
            assert!(z.message.contains(&part), "{}", z.message);
        }
        // its pane, named in the hello, is given up, and Neovim warned
        assert_eq!(
            Some(Close::RequestFailed),
            fx.d.panes.by_id(id).unwrap().closing
        );
        assert_eq!(Err(Refusal::PaneExpired), fx.d.panes.ids.check(&pane));
        let sent = sent_to_nvim(&mut fx);
        assert!(
            sent.contains("nvim_echo") && sent.contains("9.9.9"),
            "{sent:?}"
        );
    }

    #[test]
    fn a_requested_panes_hello_lacking_a_field_gives_its_pane_up_and_warns() {
        use crate::attach::{Frame, Refusal};
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("lacking");
        let mut payload = vec![0x84];
        for (k, v) in [
            ("v", None),
            ("build", Some("x")),
            ("purpose", Some("requested")),
            ("pane", None),
        ] {
            crate::nvim::msgpack::write_str(&mut payload, k);
            match (k, v) {
                ("v", _) => crate::nvim::msgpack::write_int(&mut payload, 1),
                (_, Some(v)) => crate::nvim::msgpack::write_str(&mut payload, v),
                _ => crate::nvim::msgpack::write_str(&mut payload, pane.to_string()),
            }
        }
        let mut bytes = Vec::new();
        Frame::encode_into(b'h', &payload, &mut bytes).unwrap();
        let helper = at.send(&bytes, None);
        fx.d.accept(&at.server).unwrap();
        let z = refusal(&helper);
        assert_eq!(
            (Some(Refusal::Hello), "missing field transport"),
            (z.as_refusal(), z.message.as_str())
        );
        assert_eq!(
            Some(Close::RequestFailed),
            fx.d.panes.by_id(id).unwrap().closing
        );
        assert!(sent_to_nvim(&mut fx).contains("missing field transport"));
    }

    #[test]
    fn a_requested_hello_naming_no_pane_still_warns() {
        use crate::attach::Frame;
        let mut fx = fixture();
        let (id, _) = requested_pane(&mut fx);
        let at = Address::new("nopane");
        let mut payload = vec![0x81];
        crate::nvim::msgpack::write_str(&mut payload, "purpose");
        crate::nvim::msgpack::write_str(&mut payload, "requested");
        let mut bytes = Vec::new();
        Frame::encode_into(b'h', &payload, &mut bytes).unwrap();
        let helper = at.send(&bytes, None);
        fx.d.accept(&at.server).unwrap();
        refusal(&helper);
        assert!(sent_to_nvim(&mut fx).contains("missing field v"));
        // no pane to give up
        assert!(fx.d.panes.by_id(id).unwrap().closing.is_none());
    }

    #[test]
    fn a_hello_not_a_requested_panes_is_refused_and_only_logged() {
        use crate::attach::Frame;
        let mut fx = fixture();
        let at = Address::new("notreq");
        let mut self_without_fields = vec![0x81];
        crate::nvim::msgpack::write_str(&mut self_without_fields, "purpose");
        crate::nvim::msgpack::write_str(&mut self_without_fields, "self");
        for (kind, payload) in [
            (b'h', &b"\x80"[..]),
            (b'h', &self_without_fields[..]),
            (b'h', &b"\xc1"[..]),
            (b's', &b"\x80"[..]),
        ] {
            let mut bytes = Vec::new();
            Frame::encode_into(kind, payload, &mut bytes).unwrap();
            let helper = at.send(&bytes, None);
            fx.d.accept(&at.server).unwrap();
            refusal(&helper);
        }
        assert_eq!("", sent_to_nvim(&mut fx));
    }

    #[test]
    fn a_terminal_no_pane_waits_for_is_refused_for_its_id_first_and_only_logged() {
        use crate::attach::{PaneId, Refusal, Transport};
        use crate::daemon::attach_conn::{requested_hello, test_terminal};
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let gone = fx.d.panes.add_for_window(1001).attach_id.unwrap();
        let i =
            fx.d.panes
                .index_of(fx.d.panes.by_attach_id(&gone).unwrap().id)
                .unwrap();
        fx.d.panes.list[i].closing = Some(Close::WindowClosed);
        fx.d.panes.remove(i, &fx.d.event_loop);
        let unknown = PaneId::parse(&format!("{}-zzzzzzzzzz", pane.number)).unwrap();
        let at = Address::new("idfirst");
        for (named, want) in [
            (unknown, Refusal::PaneUnknown),
            (gone, Refusal::PaneExpired),
        ] {
            // the relay, and no terminal: wrong too, but the id is checked first
            let mut t = test_terminal(80, 24);
            t.transport = Transport::Relay;
            let helper = at.send(&requested_hello(named, t), None);
            fx.d.accept(&at.server).unwrap();
            assert_eq!(Some(want), refusal(&helper).as_refusal());
            let helper = at.send(&hello_for(named, 80, 24), None);
            fx.d.accept(&at.server).unwrap();
            assert_eq!(Some(want), refusal(&helper).as_refusal());
        }
        assert_eq!("", sent_to_nvim(&mut fx));
        // the pane waiting for its own is untouched
        assert!(fx.d.panes.by_id(id).unwrap().closing.is_none());
        assert_eq!(Ok(()), fx.d.panes.ids.check(&pane));
    }

    #[test]
    fn a_peer_that_hangs_up_before_its_hello_leaves_the_daemon_running() {
        let mut fx = fixture();
        let at = Address::new("hangup");
        // gone before the daemon reads it
        drop(UnixStream::connect(&at.path).unwrap());
        fx.d.accept(&at.server).unwrap();
        // gone while it waits
        let helper = UnixStream::connect(&at.path).unwrap();
        fx.d.accept(&at.server).unwrap();
        let token = fx.d.pending[0].0;
        drop(helper);
        fx.d.on_pending(token).unwrap();
        assert!(fx.d.pending.is_empty());
        assert!(fx.d.stopped.is_none());
    }

    #[test]
    fn a_hello_that_cant_be_read_is_refused_naming_why() {
        use crate::attach::{Frame, Refusal};
        let mut fx = fixture();
        let at = Address::new("hello");
        for (kind, payload, why) in [
            (b'h', &b"\x80"[..], "missing field v"),
            (b's', &b"\x80"[..], "the first frame is s, not a hello"),
        ] {
            let mut bytes = Vec::new();
            Frame::encode_into(kind, payload, &mut bytes).unwrap();
            let helper = at.send(&bytes, None);
            fx.d.accept(&at.server).unwrap();
            let z = refusal(&helper);
            assert_eq!(
                (Some(Refusal::Hello), why),
                (z.as_refusal(), z.message.as_str())
            );
        }
        // a requested pane's hello without one of its fields
        let mut bytes = Vec::new();
        let mut payload = vec![0x83];
        for (k, v) in [("v", 1u8), ("purpose", 0), ("build", 0)] {
            crate::nvim::msgpack::write_str(&mut payload, k);
            match k {
                "v" => crate::nvim::msgpack::write_int(&mut payload, v),
                "purpose" => crate::nvim::msgpack::write_str(&mut payload, "requested"),
                _ => crate::nvim::msgpack::write_str(&mut payload, "x"),
            }
        }
        Frame::encode_into(b'h', &payload, &mut bytes).unwrap();
        let helper = at.send(&bytes, None);
        fx.d.accept(&at.server).unwrap();
        assert_eq!("missing field pane", refusal(&helper).message);
    }

    #[test]
    fn a_file_thats_no_terminal_is_refused_and_its_pane_given_up() {
        use crate::attach::Refusal;
        let at = Address::new("terminal");
        // under fd: none at all, or a pipe
        for tty in [false, true] {
            let mut fx = fixture();
            let (id, pane) = requested_pane(&mut fx);
            let mut pipe = [0; 2];
            assert_eq!(0, unsafe { libc::pipe(pipe.as_mut_ptr()) });
            let helper = at.send(&hello_for(pane, 80, 24), tty.then_some(pipe[0]));
            fx.d.accept(&at.server).unwrap();
            assert_eq!(
                Some(Refusal::Terminal),
                refusal(&helper).as_refusal(),
                "{tty}"
            );
            assert_eq!(
                Some(Close::RequestFailed),
                fx.d.panes.by_id(id).unwrap().closing
            );
            unsafe {
                libc::close(pipe[0]);
                libc::close(pipe[1]);
            }
        }
    }

    #[test]
    fn a_peer_of_another_user_is_refused() {
        let mut fx = fixture();
        fx.d.uid += 1;
        let at = Address::new("user");
        let helper = UnixStream::connect(&at.path).unwrap();
        fx.d.accept(&at.server).unwrap();
        assert_eq!(
            Some(crate::attach::Refusal::User),
            refusal(&helper).as_refusal()
        );
        assert!(fx.d.pending.is_empty());
    }

    #[test]
    fn an_id_never_minted_is_refused_as_unknown() {
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("unknown");
        let pty = Pty::new(80, 24);
        // another daemon's, or another token
        let other = crate::attach::PaneId::parse(&format!("{}-zzzzzzzzzz", pane.number)).unwrap();
        let helper = at.send(&hello_for(other, 80, 24), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        assert_eq!(
            Some(crate::attach::Refusal::PaneUnknown),
            refusal(&helper).as_refusal()
        );
        // the pane still waits for its own
        let p = fx.d.panes.by_id(id).unwrap();
        assert!(p.closing.is_none() && p.terminal_surface.is_none());
    }

    #[test]
    fn a_pane_closed_before_its_helper_connects_is_expired_and_the_helper_exits_0_quietly() {
        use crate::daemon::pane_helper::{self, Exit, Want};
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        // its window closed: nothing wants the pane
        let i = fx.d.panes.index_of(id).unwrap();
        fx.d.panes.list[i].closing = Some(Close::WindowClosed);
        fx.d.panes.remove(i, &fx.d.event_loop);
        let at = Address::new("expired");
        let helper_end = UnixStream::connect(&at.path).unwrap();
        let helper = std::thread::spawn(move || {
            let pty = Pty::new(80, 24);
            let how = pane_helper::Attach::new(Want::Requested(pane), crate::attach::Transport::Fd);
            pane_helper::attach_on(helper_end, pty.slave, &how, None)
        });
        fx.d.accept(&at.server).unwrap();
        // the hello may not be in yet
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(&(token, _)) = fx.d.pending.first() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
            fx.d.on_pending(token).unwrap();
        }
        let exit: Exit = helper.join().unwrap();
        assert_eq!((0, None), (exit.status, exit.message));
    }

    #[test]
    fn a_second_hello_for_a_pane_is_refused_as_taken_and_the_first_terminal_stays() {
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("taken");
        let first_pty = Pty::new(80, 24);
        let first = at.send(&hello_for(pane, 80, 24), Some(first_pty.slave));
        fx.d.accept(&at.server).unwrap();
        assert!(matches!(answer(&first), crate::attach::Message::Accept(_)));
        let before = fx.d.surface_of(id).helper_fd();

        let second_pty = Pty::new(80, 24);
        let second = at.send(&hello_for(pane, 80, 24), Some(second_pty.slave));
        fx.d.accept(&at.server).unwrap();
        let z = refusal(&second);
        assert_eq!(Some(crate::attach::Refusal::PaneTaken), z.as_refusal());

        assert_eq!(before, fx.d.surface_of(id).helper_fd());
        assert!(fx.d.panes.by_id(id).unwrap().closing.is_none());
        // the first helper is still attached
        first.set_nonblocking(true).unwrap();
        let mut byte = [0u8; 1];
        assert!((&first).read(&mut byte).is_err());
    }

    #[test]
    fn a_silent_connection_doesnt_delay_another_attach() {
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("silent");
        let silent = UnixStream::connect(&at.path).unwrap();
        let started = Instant::now();
        fx.d.accept(&at.server).unwrap();
        assert_eq!(1, fx.d.pending.len());

        let pty = Pty::new(80, 24);
        let helper = at.send(&hello_for(pane, 80, 24), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        assert!(matches!(answer(&helper), crate::attach::Message::Accept(_)));
        assert!(fx.d.panes.by_id(id).unwrap().terminal_surface.is_some());
        assert!(started.elapsed() < Duration::from_secs(1));

        // still waiting, until its 2 s are up
        fx.d.time_out_hellos(Instant::now()).unwrap();
        assert_eq!(1, fx.d.pending.len());
        fx.d.time_out_hellos(Instant::now() + Duration::from_secs(3))
            .unwrap();
        assert!(fx.d.pending.is_empty());
        assert_eq!(
            Some(crate::attach::Refusal::Hello),
            refusal(&silent).as_refusal()
        );
    }

    #[test]
    fn a_hello_with_a_64_kb_environment_is_accepted() {
        use crate::daemon::attach_conn::{requested_hello, test_terminal};
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        let at = Address::new("big-env");
        let pty = Pty::new(80, 24);
        let mut t = test_terminal(80, 24);
        t.env = (0..512)
            .map(|i| (format!("VAR_{i}").into_bytes(), vec![b'x'; 128]))
            .collect();
        let hello = requested_hello(pane, t);
        assert!(hello.len() > 64 << 10);
        // more than a socket holds: sent as the daemon reads
        let helper = UnixStream::connect(&at.path).unwrap();
        let slave = pty.slave;
        let sender = std::thread::spawn(move || {
            crate::daemon::fdpass::send(&helper, &hello, slave).unwrap();
            helper
        });
        fx.d.accept(&at.server).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while let Some(&(token, _)) = fx.d.pending.first() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
            fx.d.on_pending(token).unwrap();
        }
        let helper = sender.join().unwrap();
        assert!(matches!(answer(&helper), crate::attach::Message::Accept(_)));
        assert!(fx.d.panes.by_id(id).unwrap().terminal_surface.is_some());
    }

    #[test]
    fn a_later_self_attach_or_control_request_is_refused_for_now() {
        use crate::daemon::attach_conn::{self_hello, test_terminal};
        let mut fx = fixture();
        let at = Address::new("self");
        let pty = Pty::new(80, 24);
        let helper = at.send(&self_hello(test_terminal(80, 24)), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        let z = refusal(&helper);
        assert_eq!(Some(crate::attach::Refusal::Hello), z.as_refusal());
        assert_eq!("this daemon takes no self hello yet", z.message);
    }

    /// The daemon ends each attach saying why: Neovim quit, the daemon was
    /// stopped, or the pane closed.
    #[test]
    fn an_attach_ends_saying_why() {
        use crate::attach::{Ending, Message};
        for (stopped, want) in [
            (StopReason::NvimExited, Ending::Quit),
            (StopReason::Signal, Ending::Stopped),
            (StopReason::PaneGone, Ending::Closed),
        ] {
            let mut fx = fixture();
            let (_, pane) = requested_pane(&mut fx);
            let at = Address::new("end");
            let pty = Pty::new(80, 24);
            let helper = at.send(&hello_for(pane, 80, 24), Some(pty.slave));
            fx.d.accept(&at.server).unwrap();
            fx.d.stop(stopped);
            drop(fx);
            let said = crate::daemon::attach_conn::read_all_frames(&helper);
            // the undo bytes come with the answer, before any mode is set
            let [Message::Accept(_), Message::Undo(undo), Message::End(z)] = &said[..] else {
                panic!("{said:?}")
            };
            assert_eq!(&crate::term::modes::undo(), undo);
            assert_eq!(want, z.as_ending(), "{stopped:?}");
        }
    }

    /// A daemon with a 200 x 50 screen and the tabline on, and panes to place
    /// in layout areas. Its pane provider tells its live layouts unasked.
    /// Only its panes and grids are set up.
    struct TablineFixture {
        /// The other ends of the panes' terminals, so drawing into them
        /// works. Closed first, so the daemon needn't wait for them as it ends.
        terminals: Vec<TerminalEnd>,
        fx: Fixture,
    }

    /// A pty's master and a helper's socket, held open while a test runs.
    struct TerminalEnd {
        master: i32,
        _helper: UnixStream,
    }

    impl Drop for TerminalEnd {
        fn drop(&mut self) {
            // SAFETY: the master is ours, and nothing else closes it.
            unsafe { libc::close(self.master) };
        }
    }

    impl TablineFixture {
        fn init() -> TablineFixture {
            TablineFixture::on(with_provider(Some(Held::Fake(FakeProvider::telling()))))
        }

        fn on(mut fx: Fixture) -> TablineFixture {
            fx.d.tabline_rows = 1;
            fx.d.grids.grids.insert(1, Grid::default());
            fx.d.grids.get_mut(1).unwrap().resize(200, 50);
            TablineFixture {
                fx,
                terminals: Vec::new(),
            }
        }

        /// A surface of `cols` x `rows` over a terminal that takes what is
        /// drawn.
        fn surface(&mut self, cols: u32, rows: u32) -> Box<Surface> {
            let (master, slave) = surface::open_pty(cols as u16, rows as u16);
            let (socket, helper) = UnixStream::pair().unwrap();
            let mut s = Surface::create(socket, slave, surface::cells(cols as u16, rows as u16));
            s.writer.clear();
            self.terminals.push(TerminalEnd {
                master,
                _helper: helper,
            });
            s
        }

        fn d(&mut self) -> &mut Daemon {
            &mut self.fx.d
        }

        /// A window's pane, for slot `slot`, of `cols` x 20 cells, in `area`.
        /// Its id.
        fn add(&mut self, slot: u32, cols: u32, area: Area) -> u32 {
            let mut p = Pane::from_neovim_window(slot + 1, 1000 + i64::from(slot));
            p.terminal_surface = Some(self.surface(cols, 20));
            p.area = area;
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
    fn with_a_shell_to_the_left_the_first_neovim_pane_starts_the_tabline_and_the_next_continues_it()
    {
        let mut fx = TablineFixture::init();
        // a shell holds x = 0 to 0.4; the Neovim panes sit right of it
        let left = fx.add(0, 60, live("w", [0.4, 0.0, 0.3, 1.0]));
        let right = fx.add(1, 60, live("w", [0.7, 0.0, 0.3, 1.0]));
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

    /// Area `id` of a live layout, at `rect` in it, tiled.
    fn live(id: &str, rect: [f64; 4]) -> Area {
        Area::Live {
            id: id.into(),
            rect,
            floating: false,
        }
    }

    /// Area `id` of a live layout, at `rect` in it, floating.
    fn floating_in(id: &str, rect: [f64; 4]) -> Area {
        Area::Live {
            id: id.into(),
            rect,
            floating: true,
        }
    }

    #[test]
    fn a_pane_that_closes_moves_the_ones_to_its_right_left_and_only_top_panes_show_the_tabline() {
        let mut fx = TablineFixture::init();
        fx.add(0, 50, live("w", [0.0, 0.0, 0.25, 0.5]));
        fx.add(1, 70, live("w", [0.25, 0.0, 0.35, 0.5]));
        fx.add(2, 80, live("w", [0.6, 0.0, 0.4, 0.5]));
        // under them: it shows none, and takes no room
        fx.add(3, 90, live("w", [0.0, 0.5, 1.0, 0.5]));
        assert_eq!(None, fx.start(3));
        assert_eq!(Some(120), fx.start(2));
        // floating: the same
        fx.d().panes.list[1].area = floating_in("w", [0.25, 0.0, 0.35, 0.5]);
        assert_eq!(None, fx.start(1));
        assert_eq!(Some(50), fx.start(2));
        // the first closes, as in the middle of `draw`
        fx.d().panes.list.remove(0);
        assert_eq!(Some(0), fx.start(1));
        // no tabline: no pane shows any
        fx.d().tabline_rows = 0;
        assert_eq!(None, fx.start(1));
    }

    /// The tabline widths a daemon sent Neovim, in order; None for nil.
    fn tabline_widths_sent(fx: TablineFixture) -> Vec<Option<i64>> {
        let TablineFixture { terminals, fx } = fx;
        drop(terminals);
        let Fixture {
            d, mut nvim_end, ..
        } = fx;
        drop(d);
        let mut all = Vec::new();
        nvim_end.read_to_end(&mut all).unwrap();
        let call = format!("{NAME}.set_tabline_width(...)");
        let call = call.as_bytes();
        let mut sent = Vec::new();
        let mut at = 0;
        while let Some(i) = all[at..].windows(call.len()).position(|w| w == call) {
            at += i + call.len();
            let mut args = msgpack::Decoder::of(&all[at..]);
            assert_eq!(Some(msgpack::Token::Array(1)), args.next().unwrap());
            sent.push(match args.next().unwrap() {
                Some(msgpack::Token::Int(n)) => Some(n),
                Some(msgpack::Token::Nil) => None,
                t => panic!("not a width: {t:?}"),
            });
        }
        sent
    }

    #[test]
    fn the_tabline_panes_columns_are_sent_as_the_tabline_width_when_they_change() {
        let mut fx = TablineFixture::init();
        fx.d().nvim.ready = true;
        // a foreign pane holds x = 0 to 0.4; two window panes sit right of it
        fx.add(0, 60, live("w", [0.4, 0.0, 0.3, 1.0]));
        fx.add(1, 60, live("w", [0.7, 0.0, 0.3, 1.0]));
        fx.d().nvim.current_win = 1000;
        fx.d().sync_tabline_width().unwrap();
        // the same again: nothing
        fx.d().sync_tabline_width().unwrap();
        // the foreign pane closes: the panes take the whole width, and hide nothing
        fx.d().panes.list[0].area = live("w", [0.0, 0.0, 0.5, 1.0]);
        fx.d().panes.list[0].terminal_surface = Some(Surface::for_test(100, 20));
        fx.d().panes.list[1].area = live("w", [0.5, 0.0, 0.5, 1.0]);
        fx.d().panes.list[1].terminal_surface = Some(Surface::for_test(100, 20));
        fx.d().sync_tabline_width().unwrap();
        assert_eq!(vec![Some(120), None], tabline_widths_sent(fx));
    }

    #[test]
    fn the_tabline_width_stays_while_the_tabline_is_off_and_goes_with_the_tabline_panes() {
        let mut fx = TablineFixture::init();
        fx.d().nvim.ready = true;
        fx.add(0, 120, live("w", [0.4, 0.0, 0.6, 0.5]));
        // under it, and a floating tab page's: neither shows the tabline
        fx.add(1, 200, live("w", [0.0, 0.5, 1.0, 0.5]));
        fx.add(2, 30, floating_in("w", [0.0, 0.0, 0.15, 0.5]));
        fx.d().nvim.current_win = 1000;
        fx.d().sync_tabline_width().unwrap();
        // ready for when it's on again
        fx.d().tabline_rows = 0;
        fx.d().sync_tabline_width().unwrap();
        // a foreign pane takes the top: there are no tabline panes
        fx.d().panes.list[0].area = live("w", [0.4, 0.3, 0.6, 0.2]);
        fx.d().sync_tabline_width().unwrap();
        assert_eq!(vec![Some(120), None], tabline_widths_sent(fx));
    }

    #[test]
    fn a_layout_that_moves_a_pane_to_the_top_sends_the_tabline_width_after_its_batch() {
        let mut fx = TablineFixture::init();
        // under a foreign pane: it shows no tabline
        let id = fx.add(0, 120, live("w", [0.4, 0.5, 0.6, 0.5]));
        fx.d().panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from("t1"));
        fx.d().nvim.current_win = 1000;
        // windows.lua isn't running yet
        fx.d().after_batch().unwrap();
        fx.d().nvim.ready = true;
        fx.d().after_batch().unwrap();

        // the user moves it to the top, a foreign pane left of it
        let l = layout_of(&[(
            "w",
            &[
                ("t9", [0.0, 0.0, 0.4, 1.0], false),
                ("t1", [0.4, 0.0, 0.6, 1.0], false),
            ],
        )]);
        fake(fx.d()).push(ProviderEvent::Layout(l));
        fx.d().drain_provider().unwrap();
        fx.d().after_batch().unwrap();
        assert_eq!(vec![Some(120)], tabline_widths_sent(fx));
    }

    #[test]
    fn a_pane_in_no_layout_area_yet_is_no_tabline_pane_and_takes_no_room() {
        let mut fx = TablineFixture::init();
        // a new split, before the live layout names its place
        fx.add(0, 40, Area::Unreported);
        assert_eq!(None, fx.start(0));
        fx.add(1, 60, live("w", [0.4, 0.0, 0.6, 1.0]));
        assert_eq!(None, fx.start(0));
        // and the placed pane starts the tabline, not after it
        assert_eq!(Some(0), fx.start(1));
        // its window leaves no row for the tabline
        let d = fx.d();
        assert_eq!([40, 19], d.geometry().window_size(&d.panes.list[0]));
    }

    /// A terminal of a live layout: its id, rect, and whether it floats.
    type Placing<'a> = (&'a str, [f64; 4], bool);

    /// A live layout of `areas`: each its id, and its terminals.
    fn layout_of(areas: &[(&str, &[Placing<'_>])]) -> LiveLayout {
        LiveLayout {
            areas: areas
                .iter()
                .map(|(id, ts)| LayoutArea {
                    id: id.to_string(),
                    terminals: ts
                        .iter()
                        .map(|&(t, rect, floating)| PlacedTerminal {
                            terminal: TerminalId::from(t),
                            rect,
                            floating,
                            inset: [0; 4],
                        })
                        .collect(),
                })
                .collect(),
            current: None,
        }
    }

    #[test]
    fn a_layout_of_two_areas_makes_tabline_panes_per_area_and_the_screen_follows_the_current_pane()
    {
        let mut fx = TablineFixture::init();
        fx.d().nvim.ready = true;
        let a = fx.add(0, 100, Area::Unreported);
        let b = fx.add(1, 100, Area::Unreported);
        let c = fx.add(2, 60, Area::Unreported);
        for (id, n) in [(a, 1), (b, 2), (c, 3)] {
            set_block(fx.d(), id, n);
        }
        fx.d().nvim.current_win = 1000;
        // two layout areas: a and b side by side; c over a foreign pane
        let l = layout_of(&[
            (
                "w1",
                &[
                    (block_id(1).as_str(), [0.0, 0.0, 0.5, 1.0], false),
                    (block_id(2).as_str(), [0.5, 0.0, 0.5, 1.0], false),
                ],
            ),
            (
                "w2",
                &[
                    (block_id(3).as_str(), [0.0, 0.0, 0.5, 0.5], false),
                    (block_id(9).as_str(), [0.0, 0.5, 1.0, 0.5], false),
                ],
            ),
        ]);
        fake(fx.d()).push(ProviderEvent::Layout(l));
        fx.d().drain_provider().unwrap();
        fx.d().after_batch().unwrap();
        assert_eq!(
            live("w2", [0.0, 0.0, 0.5, 0.5]),
            fx.d().panes.by_id(c).unwrap().area
        );
        // each area's tabline starts at its left
        let starts: Vec<Option<u32>> = (0..3).map(|i| fx.start(i)).collect();
        assert_eq!(vec![Some(0), Some(100), Some(0)], starts);
        // the screen is a's area, in its cells; its tabline panes fill it
        assert_eq!([200, 20], fx.d().nvim.editor_size);

        // into c: its area, measured in its cells; its tabline fitted to it
        fx.d().on_current(1002).unwrap();
        fx.d().after_batch().unwrap();
        assert_eq!([120, 40], fx.d().nvim.editor_size);
        // back into b
        fx.d().on_current(1001).unwrap();
        fx.d().after_batch().unwrap();
        assert_eq!([200, 20], fx.d().nvim.editor_size);
        // no terminal was moved for it
        assert!(fake(fx.d()).asks.is_empty());
        assert_eq!(vec![Some(60), None], tabline_widths_sent(fx));
    }

    #[test]
    fn the_launch_pane_is_its_own_area_until_a_live_layout_places_it() {
        let mut fx = TablineFixture::init();
        let s = fx.surface(80, 24);
        fx.d()
            .start(Pane::startup(Some(TerminalId::from("t0")), s))
            .unwrap();
        // no wait for a report: a tabline pane, the screen its size
        let d = fx.d();
        assert_eq!(Area::Own, d.panes.list[0].area);
        assert_eq!(None, d.panes.list[0].report_due);
        assert_eq!(None, d.area_timer_due);
        assert_eq!(Some([80, 24]), geometry::area_size(&d.panes.list[0]));
        assert!(fake(d).asks.is_empty());
        assert_eq!(Some(0), fx.start(0));

        let rect = [0.0, 0.0, 0.5, 1.0];
        fake(fx.d()).push(ProviderEvent::Layout(layout_of(&[(
            "w",
            &[("t0", rect, false)],
        )])));
        fx.d().drain_provider().unwrap();
        let d = fx.d();
        assert_eq!(live("w", rect), d.panes.list[0].area);
        assert_eq!(Some([160, 24]), geometry::area_size(&d.panes.list[0]));
    }

    #[test]
    fn each_areas_tabline_panes_slice_the_screens_tabline_from_its_start() {
        let mut fx = TablineFixture::init();
        {
            let screen = fx.d().grids.get_mut(1).unwrap();
            screen.resize(20, 10);
            for (c, ch) in "1:one 2:two 3:three ".chars().enumerate() {
                screen.cells[c] = Cell::init(ch.to_string(), 0);
            }
        }
        let a = fx.add(0, 12, live("w1", [0.0, 0.0, 0.6, 1.0]));
        let b = fx.add(1, 8, live("w1", [0.6, 0.0, 0.4, 1.0]));
        let c = fx.add(2, 8, live("w2", [0.0, 0.0, 0.3, 1.0]));
        let d = fx.add(3, 26, Area::Own);
        let shown = |fx: &mut TablineFixture, id: u32| {
            let t = fx.d().prepare_tabline(id);
            let text: String = t
                .cells
                .unwrap()
                .iter()
                .map(|c| String::from_utf8_lossy(c.str()).into_owned())
                .collect();
            (t.col, text)
        };
        // the current area's, end to end: every tab, once
        assert_eq!((0, "1:one 2:two ".to_string()), shown(&mut fx, a));
        assert_eq!((12, "3:three ".to_string()), shown(&mut fx, b));
        // another area's, from its start: cut at its width, or ending with
        // the screen's row, past which the pane shows it blank
        assert_eq!((0, "1:one 2:".to_string()), shown(&mut fx, c));
        assert_eq!((0, "1:one 2:two 3:three ".to_string()), shown(&mut fx, d));
    }

    #[test]
    fn the_area_timer_fires_once_the_report_wait_is_over_and_not_before() {
        let mut fx = with_provider(Some(Held::Fake(FakeProvider::telling())));
        let (id, pane) = requested_pane(&mut fx);
        fx.d.panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from("t1"));
        let _attached = attach_terminal(&mut fx, pane, 70, 20);
        let due = fx.d.panes.by_id(id).unwrap().report_due.unwrap();
        assert_eq!(Some(due), fx.d.area_timer_due);
        let mut events = [Event::default(); 8];
        let fired = loop {
            let n = fx.d.event_loop.wait(&mut events).unwrap().len();
            if let Some(e) = events[..n]
                .iter()
                .find(|e| Token::of(e.token) == Some(Token::AreaTimer))
            {
                break *e;
            }
        };
        // rounded up: never early
        assert!(Instant::now() >= due);
        fx.d.handle(fired).unwrap();
        assert_eq!(Area::Own, area_of(&fx, id));
        assert_eq!(None, fx.d.area_timer_due);
    }

    #[test]
    fn with_no_pane_provider_each_pane_is_its_own_area_with_its_own_tabline() {
        let mut fx = TablineFixture::on(with_provider(None));
        let s = fx.surface(80, 24);
        fx.d().start(Pane::startup(None, s)).unwrap();
        fx.d().nvim.ready = true;
        let other = fx.add(1, 60, Area::Unreported);
        fx.d().wait_for_report(other, Instant::now()).unwrap();
        let d = fx.d();
        for p in &d.panes.list {
            assert_eq!(Area::Own, p.area);
        }
        assert_eq!(Some([80, 24]), geometry::area_size(&d.panes.list[0]));
        assert_eq!(Some([60, 20]), geometry::area_size(&d.panes.list[1]));
        assert_eq!(None, d.geometry().tabline_width(&d.panes.list[0]));
        assert_eq!(Some(0), fx.start(0));
        assert_eq!(Some(0), fx.start(1));
    }

    /// Pane `pane`'s terminal attaches, `cols` x `rows`: the helper's end,
    /// and what it rides on, to keep while the test runs.
    fn attach_terminal(
        fx: &mut Fixture,
        pane: crate::attach::PaneId,
        cols: u16,
        rows: u16,
    ) -> (UnixStream, Pty, Address) {
        let at = Address::new(&format!("attach-{}", pane.token()));
        let pty = Pty::new(cols, rows);
        let helper = at.send(&hello_for(pane, cols, rows), Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        (helper, pty, at)
    }

    fn area_of(fx: &Fixture, id: u32) -> Area {
        fx.d.panes.by_id(id).unwrap().area.clone()
    }

    #[test]
    fn an_unreported_pane_becomes_its_own_area_a_second_after_its_terminal_attached_with_its_id() {
        let mut fx = with_provider(Some(Held::Fake(FakeProvider::telling())));
        let (id, pane) = requested_pane(&mut fx);
        // its open is done: it has its terminal id
        fx.d.panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from("t1"));
        let before = Instant::now();
        let _attached = attach_terminal(&mut fx, pane, 70, 20);
        assert_eq!(Area::Unreported, area_of(&fx, id));
        // it tells its layouts unasked: nothing is asked
        assert!(fake(&mut fx.d).asks.is_empty());
        fx.d.time_out_reports(before + Duration::from_millis(900))
            .unwrap();
        assert_eq!(Area::Unreported, area_of(&fx, id));
        fx.d.time_out_reports(Instant::now() + Duration::from_secs(1))
            .unwrap();
        assert_eq!(Area::Own, area_of(&fx, id));
    }

    #[test]
    fn with_no_live_layout_to_come_an_attached_pane_is_its_own_area_at_once() {
        let mut fx = fixture();
        let (id, pane) = requested_pane(&mut fx);
        fx.d.panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from("t1"));
        let _attached = attach_terminal(&mut fx, pane, 70, 20);
        assert_eq!(Area::Own, area_of(&fx, id));
    }

    #[test]
    fn a_pane_provider_that_doesnt_tell_layouts_is_asked_and_only_unsupported_drops_its_layout() {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::ReportLayout);
        let mut fx = with_provider(Some(Held::Fake(f)));
        let (id, pane) = requested_pane(&mut fx);
        fx.d.panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from("t1"));
        let _attached = attach_terminal(&mut fx, pane, 70, 20);
        assert_eq!(vec![Ask::ReportLayout], fake(&mut fx.d).asked());
        let rect = [0.5, 0.0, 0.5, 1.0];
        let l = layout_of(&[("w", &[("t1", rect, false)])]);
        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Done(Answer::Layout(l)));
        fx.d.drain_provider().unwrap();
        assert_eq!(live("w", rect), area_of(&fx, id));

        // a report that fails keeps the last layout
        fx.d.act(Ask::ReportLayout);
        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Failed("no".into()));
        fx.d.drain_provider().unwrap();
        assert_eq!(live("w", rect), area_of(&fx, id));
        assert!(fx.d.live_layout.is_some());
        // one found unsupported makes every pane its own area
        fx.d.act(Ask::ReportLayout);
        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Unsupported);
        fx.d.drain_provider().unwrap();
        assert_eq!(Area::Own, area_of(&fx, id));
        assert!(fx.d.live_layout.is_none());
    }

    #[test]
    fn a_pane_provider_gone_makes_its_unreported_panes_their_own_areas_and_leaves_the_rest() {
        let mut fx = TablineFixture::init();
        let placed = fx.add(0, 60, live("w", [0.0, 0.0, 0.5, 1.0]));
        let waiting = fx.add(1, 60, Area::Unreported);
        fake(fx.d()).push(ProviderEvent::Gone {
            reason: "hung up".into(),
        });
        fx.d().drain_provider().unwrap();
        let area = |fx: &mut TablineFixture, id| fx.d().panes.by_id(id).unwrap().area.clone();
        assert_eq!(live("w", [0.0, 0.0, 0.5, 1.0]), area(&mut fx, placed));
        assert_eq!(Area::Own, area(&mut fx, waiting));
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
        let (Fixture { mut d, .. }, mut rex_end) = rex_fixture();
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

    /// A daemon whose fake pane provider can open, close and `caps`, with
    /// windows 1, 2 and 3 drawn in tab page 2, each in a requested pane of
    /// its own, all but window 3's with the terminal `block_id(n)`.
    fn tab_page_of_three(caps: &[Capability]) -> Fixture {
        let mut f = FakeProvider::opening();
        for &c in caps {
            f.caps.insert(c);
        }
        let mut fx = with_provider(Some(Held::Fake(f)));
        for n in [1u32, 2, 3] {
            fx.d.grids.wins.insert(
                n + 10,
                Win {
                    win: i64::from(n),
                    kind: WinKind::External,
                    float: Float::default(),
                },
            );
            let p = fx.d.panes.add_for_window(i64::from(n));
            p.tab = 2;
            if n < 3 {
                p.terminal = Some(TerminalId::from(block_id(n).as_str()));
            }
        }
        fx
    }

    /// Windows 3, 2 and 1 in a row, `widths` wide: 3, made last, first.
    fn three_reversed(widths: &[u32]) -> Layout {
        let mut layout = window_row(widths);
        layout.nodes[1].win = 3;
        layout.nodes[3].win = 1;
        layout
    }

    fn place(terminal: u32, origin: u32, size: u32, ratio: f64) -> Ask {
        Ask::PlaceBeside {
            terminal: TerminalId::from(block_id(terminal).as_str()),
            origin: TerminalId::from(block_id(origin).as_str()),
            dir: Direction::Right,
            size,
            ratio,
        }
    }

    fn resize(terminal: u32, origin: u32, size: u32, ratio: f64) -> Ask {
        Ask::Resize {
            terminal: TerminalId::from(block_id(terminal).as_str()),
            origin: TerminalId::from(block_id(origin).as_str()),
            dir: Direction::Right,
            size,
            ratio,
        }
    }

    /// `asks` with their ratios to nine places, to compare.
    fn ratios_rounded(asks: &[Ask]) -> Vec<Ask> {
        let round = |r: f64| (r * 1e9).round() / 1e9;
        asks.iter()
            .cloned()
            .map(|mut a| {
                if let Ask::PlaceBeside { ratio, .. } | Ask::Resize { ratio, .. } = &mut a {
                    *ratio = round(*ratio);
                }
                a
            })
            .collect()
    }

    /// Answers the fake's last ask as done, and drains it.
    fn done(d: &mut Daemon) {
        let f = fake(d);
        f.answer(f.last_id(), Outcome::Done(Answer::Nothing));
        d.drain_provider().unwrap();
    }

    #[test]
    fn a_tab_pages_panes_are_placed_one_at_a_time_once_all_have_terminals_and_resized_when_sizes_change()
     {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside, Capability::Resize]);
        let d = &mut fx.d;
        let layout = three_reversed(&[20, 40, 40]);
        d.on_tab_layout(layout).unwrap();
        // a subset placed would only be moved again
        assert!(fake(d).asks.is_empty());
        d.panes.by_win_mut(3).unwrap().terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.place_tab_pages().unwrap();
        // the last of the layout goes beside the first, at its share: the
        // second waits for the first's answer
        assert_eq!(
            ratios_rounded(&[place(1, 3, 40, 0.4)]),
            ratios_rounded(&fake(d).asked())
        );
        done(d);
        assert_eq!(
            ratios_rounded(&[place(1, 3, 40, 0.4), place(2, 3, 40, 40.0 / 60.0)]),
            ratios_rounded(&fake(d).asked())
        );
        done(d);
        // nothing changed: nothing asked
        d.place_tab_pages().unwrap();
        assert_eq!(2, fake(d).asks.len());
        // sizes only: the same splits, resized, one at a time
        d.on_tab_layout(three_reversed(&[30, 40, 40])).unwrap();
        assert_eq!(
            ratios_rounded(&[resize(1, 3, 40, 40.0 / 110.0)]),
            ratios_rounded(&fake(d).asked()[2..])
        );
        done(d);
        assert_eq!(
            ratios_rounded(&[resize(2, 3, 40, 40.0 / 70.0)]),
            ratios_rounded(&fake(d).asked()[3..])
        );
    }

    #[test]
    fn with_place_beside_alone_a_tab_page_is_arranged_and_its_sizes_left() {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside]);
        let d = &mut fx.d;
        d.panes.by_win_mut(3).unwrap().terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        done(d);
        done(d);
        assert_eq!(vec!["place_beside"; 2], asked_capabilities(d));
        d.on_tab_layout(three_reversed(&[30, 40, 40])).unwrap();
        assert_eq!(2, fake(d).asks.len());
        // windows in another order are arranged again
        d.on_tab_layout(window_row(&[20, 40, 40])).unwrap();
        assert_eq!(vec!["place_beside"; 3], asked_capabilities(d));
    }

    #[test]
    fn with_resize_alone_a_tab_page_is_not_placed() {
        let mut fx = tab_page_of_three(&[Capability::Resize]);
        let d = &mut fx.d;
        d.panes.by_win_mut(3).unwrap().terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        d.on_tab_layout(three_reversed(&[30, 40, 40])).unwrap();
        assert!(fake(d).asks.is_empty());
    }

    #[test]
    fn twenty_quick_resizes_leave_the_final_ratios() {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside, Capability::Resize]);
        let d = &mut fx.d;
        d.panes.by_win_mut(3).unwrap().terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        done(d);
        done(d);
        // the first window a cell wider each time, faster than answers come
        for n in 1..=20 {
            d.on_tab_layout(three_reversed(&[20 + n, 40, 40])).unwrap();
            if n % 3 == 0 {
                done(d);
            }
        }
        // asked one at a time, in order, until none is left
        loop {
            let before = fake(d).asks.len();
            done(d);
            if d.asks.waiting() == 0 {
                break;
            }
            assert_eq!(before + 1, fake(d).asks.len());
        }
        let asked = fake(d).asked();
        assert_eq!(
            ratios_rounded(&[
                resize(1, 3, 40, 40.0 / 120.0),
                resize(2, 3, 40, 40.0 / 80.0)
            ]),
            ratios_rounded(&asked[asked.len() - 2..])
        );
    }

    #[test]
    fn a_tab_pages_layout_is_kept_only_while_it_has_a_tab_page_area_or_one_on_its_way() {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside, Capability::Resize]);
        let d = &mut fx.d;
        // tab page 2's windows have panes in its area
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        assert!(d.tab_layouts.contains_key(&2));
        // tab page 5 has none, and no window waiting for one: as the tab
        // page made at setup
        let mut other = window_row(&[20, 40]);
        other.tab = 5;
        d.on_tab_layout(other).unwrap();
        assert!(!d.tab_layouts.contains_key(&5));
        // its first window is on its way to its area
        d.changes.note_split(windows::Split {
            win: 7,
            side_by_side: false,
            before: false,
            from: 0,
            tab: 5,
        });
        d.on_tab_layout(other).unwrap();
        assert!(d.tab_layouts.contains_key(&5));
    }

    #[test]
    fn a_self_attached_pane_in_a_tab_pages_area_is_never_moved() {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside, Capability::Resize]);
        let d = &mut fx.d;
        // window 1's terminal attached by hand
        let i = d.panes.index_of(d.panes.by_win(1).unwrap().id).unwrap();
        d.panes.list[i].attach_id = None;
        d.panes.by_win_mut(3).unwrap().terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        done(d);
        assert_eq!(
            ratios_rounded(&[place(2, 3, 40, 40.0 / 60.0)]),
            ratios_rounded(&fake(d).asked())
        );
    }

    #[test]
    fn a_self_attached_pane_is_never_what_others_are_placed_beside() {
        let mut fx = tab_page_of_three(&[Capability::PlaceBeside, Capability::Resize]);
        let d = &mut fx.d;
        // window 3's terminal, first in the layout, attached by hand: it may
        // be anywhere, so the next of the layout is what the rest go beside
        let i = d.panes.index_of(d.panes.by_win(3).unwrap().id).unwrap();
        d.panes.list[i].attach_id = None;
        d.panes.list[i].terminal = Some(TerminalId::from(block_id(3).as_str()));
        d.on_tab_layout(three_reversed(&[20, 40, 40])).unwrap();
        done(d);
        assert_eq!(
            ratios_rounded(&[place(1, 2, 40, 0.5)]),
            ratios_rounded(&fake(d).asked())
        );
    }

    #[test]
    fn a_tab_pages_window_opens_beside_a_requested_pane_of_its_area() {
        let mut fx = fixture();
        let d = &mut fx.d;
        // a self-attached pane of tab page 2 first, then a requested one
        d.panes.add(Pane::from_neovim_window(50, 50)).tab = 2;
        set_block(d, 50, 50);
        pane_in_block(d, 51, 2, 51);
        let sp = windows::Split {
            win: 60,
            side_by_side: true,
            before: false,
            from: 0,
            tab: 2,
        };
        assert_eq!(
            Some(TerminalId::from(block_id(51).as_str())),
            d.tab_page_origin(sp)
        );
        // its origin, in the same area, comes first, self-attached or not
        assert_eq!(
            Some(TerminalId::from(block_id(50).as_str())),
            d.tab_page_origin(windows::Split { from: 50, ..sp })
        );
    }

    #[test]
    fn only_a_pane_of_another_tab_page_is_a_move_between_tab_pages() {
        let mut fx = fixture();
        fx.d.panes.add(Pane::from_neovim_window(1, 1));
        fx.d.panes.add(Pane::from_neovim_window(2, 2)).tab = 2;
        fx.d.panes.add(Pane::from_neovim_window(3, 3)).tab = 2;
        assert!(fx.d.moves_tab_page(Some(0), 2));
        assert!(fx.d.moves_tab_page(Some(2), 1));
        assert!(!fx.d.moves_tab_page(Some(2), 3));
        // from a pane that's gone, or to one
        assert!(!fx.d.moves_tab_page(None, 2));
        assert!(!fx.d.moves_tab_page(Some(0), 9));
    }

    /// A daemon whose windows 1 and 2 are drawn, in tab page 2, and don't have
    /// panes yet. Window 9's pane, of no tab page, has the terminal
    /// `block_id(9)` to open the tab page's Rex floating pane from.
    fn two_windows_of_a_tab_page() -> Fixture {
        let mut fx = fixture();
        pane_in_block(&mut fx.d, 9, 0, 9);
        fx.d.nvim.ready = true;
        for n in [1i64, 2, 9] {
            fx.d.grids.wins.insert(
                n as u32 + 10,
                Win {
                    win: n,
                    kind: WinKind::External,
                    float: Float::default(),
                },
            );
            if n == 9 {
                continue;
            }
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

    /// The capabilities the daemon asked of the fake, in order.
    fn asked_capabilities(d: &mut Daemon) -> Vec<&'static str> {
        fake(d)
            .asks
            .iter()
            .map(|(_, a)| a.capability().name())
            .collect()
    }

    #[test]
    fn a_window_waiting_for_its_tab_pages_rex_floating_pane_asks_for_its_own_once_its_open_fails() {
        let mut fx = two_windows_of_a_tab_page();
        // the first window asks for the Rex floating pane; the second waits for it
        fx.d.new_pane(1).unwrap();
        fx.d.reconcile().unwrap();
        assert_eq!(2, fx.d.panes.list.len());
        assert!(fx.d.panes.by_win(2).is_none());

        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Failed("refused: no".into()));
        fx.d.drain_provider().unwrap();

        let (origin, first, second) = (
            fx.d.panes.by_win(9).unwrap().id,
            fx.d.panes.by_win(1).unwrap().id,
            fx.d.panes.by_win(2).unwrap().id,
        );
        assert_eq!(vec![origin, first, second], pane_ids(&fx.d));
        assert_eq!(
            vec!["open_tab_page", "open_tab_page"],
            asked_capabilities(&mut fx.d)
        );
    }

    #[test]
    fn a_window_keeps_waiting_for_its_tab_pages_floating_pane_still_on_its_way() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.new_pane(1).unwrap();
        fx.d.reconcile().unwrap();
        let first = fx.d.panes.by_win(1).unwrap().id;
        assert_eq!(first, pane_ids(&fx.d)[1]);
        assert_eq!(2, pane_ids(&fx.d).len());
        assert_eq!(vec!["open_tab_page"], asked_capabilities(&mut fx.d));
    }

    #[test]
    fn the_window_leaves_cmdheight_rows_for_messages_then_the_tablines() {
        let mut fx = TablineFixture::init();
        let id = fx.add(0, 60, Area::Own);
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
        d.panes.by_id_mut(id).unwrap().terminal = Some(TerminalId::from(block_id(n).as_str()));
    }

    /// Where the live layout last had float group pane `id`'s terminal.
    fn layer_rect_of(d: &Daemon, id: u32) -> Option<[f64; 4]> {
        d.panes.by_id(id).unwrap().layer.as_ref().unwrap().rect
    }

    /// Applies live layout `l`, and sends what that asked of Neovim.
    fn apply_and_flush(d: &mut Daemon, l: &LiveLayout) {
        d.apply_live_layout(l);
        d.nvim.client.flush().unwrap();
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
        // the current pane, whose terminal a float group pane opens from
        if d.panes.by_win(999).is_none() {
            let p = d.panes.add(Pane::from_neovim_window(90, 999));
            p.terminal = Some(TerminalId::from("block:o"));
            d.nvim.current_win = 999;
        }
    }

    /// The float group pane's id.
    fn picker_pane(d: &Daemon) -> u32 {
        d.panes.list.iter().find(|p| p.layer.is_some()).unwrap().id
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
    fn a_live_layout_gives_the_panes_it_names_their_areas_and_the_rest_keep_theirs() {
        let mut fx = TablineFixture::init();
        let a = fx.add(0, 60, Area::Unreported);
        let b = fx.add(1, 60, Area::Unreported);
        let other = fx.add(2, 60, Area::Own);
        let d = fx.d();
        set_block(d, a, 1);
        set_block(d, b, 2);
        set_block(d, other, 3);
        let one = [0.0, 0.0, 0.5, 1.0];
        let two = [0.5, 0.0, 0.5, 0.25];
        let l = |b2: &str| {
            layout_of(&[
                ("w8", &[(block_id(99).as_str(), one, false)]),
                ("w9", &[(block_id(1).as_str(), one, false), (b2, two, true)]),
            ])
        };
        assert!(d.apply_live_layout(&l(&block_id(2))));
        assert_eq!(live("w9", one), d.panes.by_id(a).unwrap().area);
        assert_eq!(floating_in("w9", two), d.panes.by_id(b).unwrap().area);
        assert_eq!(Area::Own, d.panes.by_id(other).unwrap().area);
        // the same again: nothing changes
        assert!(!d.apply_live_layout(&l(&block_id(2))));
        // one that no longer names a pane leaves it where it was
        assert!(!d.apply_live_layout(&l(&block_id(5))));
        assert_eq!(floating_in("w9", two), d.panes.by_id(b).unwrap().area);
        // and nothing was asked of the pane provider: no terminal moves
        assert!(fake(d).asks.is_empty());
    }

    #[test]
    fn a_float_group_asks_for_a_float_group_pane_at_its_bounds_and_its_close_closes_it() {
        let Fixture { mut d, .. } = fixture();
        show_picker(&mut d);
        d.reconcile_layers();
        // already has one
        d.reconcile_layers();
        assert_eq!(2, d.panes.list.len());
        let id = picker_pane(&d);
        // nowhere until the live layout says
        assert_eq!(None, layer_rect_of(&d, id));
        let asked = fake(&mut d).asked();
        assert_eq!(1, asked.len());
        let Ask::OpenFloat {
            pane,
            attach,
            title,
            origin,
            x,
            y,
            cols,
            rows,
            origin_cols,
            origin_rows,
            area_bounds,
        } = &asked[0]
        else {
            panic!("{asked:?}");
        };
        // its group's cells from the current pane, in no live area: from the
        // 200 x 50 screen's top left, the screen its size
        assert_eq!(
            (10, 4, 60, 20, 200, 50, None),
            (
                *x,
                *y,
                *cols,
                *rows,
                *origin_cols,
                *origin_rows,
                *area_bounds
            )
        );
        assert_eq!("block:o", origin.as_str());
        // `{attach}`, with the pane id minted for it
        assert_eq!(1, pane.number);
        assert_eq!(&format!("{NAME}-{}", pane.token()), title);
        assert_eq!(
            vec!["attach", "--address", "", "--pane"],
            attach[1..5].to_vec()
        );
        assert_eq!(pane.to_string(), attach[5]);

        // it opens
        let f = fake(&mut d);
        f.opened(f.last_id(), "block:5");
        d.drain_provider().unwrap();
        assert_eq!(
            Some("block:5"),
            d.panes
                .by_id(id)
                .unwrap()
                .terminal
                .as_ref()
                .map(TerminalId::as_str)
        );
        // the float closes
        d.grids.wins.swap_remove(&3);
        d.reconcile_layers();
        assert_eq!(1, d.panes.list.len());
        assert_eq!(
            asks::close(TerminalId::from("block:5")),
            fake(&mut d).asked()[1]
        );
    }

    #[test]
    fn a_float_groups_cells_are_from_its_origin_pane_with_its_bounds_in_their_area() {
        let Fixture { mut d, .. } = fixture();
        show_picker(&mut d);
        // the current pane is the right half of the screen's area, 100 x 50
        let origin = d.panes.by_win_mut(999).unwrap();
        origin.area = Area::Live {
            id: "w".into(),
            rect: [0.5, 0.0, 0.5, 1.0],
            floating: false,
        };
        origin.terminal_surface = Some(Surface::for_test(100, 50));
        d.grids.wins.get_mut(&3).unwrap().float.screen = Some([4, 110]);
        d.reconcile_layers();
        let asked = fake(&mut d).asked();
        let [
            Ask::OpenFloat {
                x,
                y,
                cols,
                rows,
                origin_cols,
                origin_rows,
                area_bounds: Some(b),
                ..
            },
        ] = &asked[..]
        else {
            panic!("{asked:?}");
        };
        assert_eq!(
            (10, 4, 60, 20, 100, 50),
            (*x, *y, *cols, *rows, *origin_cols, *origin_rows)
        );
        approx([0.55, 0.08, 0.3, 0.4], *b);
    }

    #[test]
    fn a_float_groups_cells_and_size_are_the_screens_while_its_origin_pane_has_no_live_area() {
        let Fixture { mut d, .. } = fixture();
        show_picker(&mut d);
        // a terminal of 100 x 50, but no live area: the screen is the origin
        d.panes.by_win_mut(999).unwrap().terminal_surface = Some(Surface::for_test(100, 50));
        d.reconcile_layers();
        let asked = fake(&mut d).asked();
        let [
            Ask::OpenFloat {
                x,
                y,
                origin_cols,
                origin_rows,
                area_bounds,
                ..
            },
        ] = &asked[..]
        else {
            panic!("{asked:?}");
        };
        assert_eq!(
            (10, 4, 200, 50, None),
            (*x, *y, *origin_cols, *origin_rows, *area_bounds)
        );
    }

    #[test]
    fn a_float_group_that_closes_before_it_opens_closes_its_terminal_once_it_does() {
        let Fixture { mut d, .. } = fixture();
        show_picker(&mut d);
        d.reconcile_layers();
        let open = fake(&mut d).last_id();
        d.grids.wins.swap_remove(&3);
        d.reconcile_layers();
        assert_eq!(1, d.panes.list.len());
        assert_eq!(1, fake(&mut d).asks.len());

        fake(&mut d).opened(open, "block:5");
        d.drain_provider().unwrap();
        assert_eq!(
            asks::close(TerminalId::from("block:5")),
            fake(&mut d).asked()[1]
        );
    }

    #[test]
    fn an_unentered_float_or_no_open_float_gets_no_float_group_pane() {
        let Fixture { mut d, .. } = fixture();
        show_picker(&mut d);
        d.nvim = NvimSession::new(for_test_on(UnixStream::pair().unwrap().0));
        d.reconcile_layers();
        assert_eq!(1, d.panes.list.len());
        d.nvim.note_entered(1002);
        fake(&mut d).caps = CapabilitySet::of(&[Capability::OpenBeside]);
        d.reconcile_layers();
        assert_eq!(1, d.panes.list.len());
        assert!(fake(&mut d).asks.is_empty());
        d.provider = None;
        d.reconcile_layers();
        assert_eq!(1, d.panes.list.len());
    }

    /// A floating pane of the picker group, with a surface of `cols` x `rows`,
    /// that Rex has answered, in `d`. Its id and block.
    fn made_picker_pane(d: &mut Daemon, cols: u32, rows: u32) -> u32 {
        show_picker(d);
        d.reconcile_layers();
        let id = picker_pane(d);
        d.panes.by_id_mut(id).unwrap().terminal_surface = Some(Surface::for_test(cols, rows));
        set_block(d, id, 7);
        // the pane it opened from has done its part
        let origin = d.panes.by_win(999).unwrap().id;
        d.panes
            .remove(d.panes.index_of(origin).unwrap(), &d.event_loop);
        id
    }

    /// A live layout with the float group pane's terminal at `bounds`, a
    /// frame of 3 x 2 cells around it, as a layout provider may draw.
    fn floating_layer_at(bounds: [f64; 4]) -> LiveLayout {
        let mut l = layout_of(&[("w", &[(block_id(7).as_str(), bounds, true)])]);
        l.areas[0].terminals[0].inset = [1, 1, 2, 1];
        l
    }

    /// Where the live layout has the picker's float group pane on the 200 x
    /// 50 screen: a cell left of and above it, 3 x 2 cells bigger.
    const PICKER_RECT: [f64; 4] = [0.045, 0.06, 0.315, 0.44];

    /// How many times the daemon asked Neovim to place floats, as it wrote them.
    fn place_group_requests(mut nvim_end: UnixStream) -> usize {
        let mut all = Vec::new();
        nvim_end.read_to_end(&mut all).unwrap();
        String::from_utf8_lossy(&all)
            .matches(&format!("{NAME}.place_group"))
            .count()
    }

    #[test]
    fn a_floating_pane_the_user_moved_moves_its_floats_once_and_doesnt_oscillate() {
        let Fixture { mut d, nvim_end } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        let asked = PICKER_RECT;

        // the live layout has it around its floats: nothing to follow
        apply_and_flush(&mut d, &floating_layer_at(asked));
        // within half a cell: the same
        let near = [asked[0] + 0.002, asked[1], asked[2], asked[3]];
        apply_and_flush(&mut d, &floating_layer_at(near));
        assert_eq!(Some(asked), layer_rect_of(&d, id));

        // the user moves it 20 columns right: its floats follow, once
        let moved = [asked[0] + 0.1, asked[1], asked[2], asked[3]];
        apply_and_flush(&mut d, &floating_layer_at(moved));
        assert_eq!(Some(moved), layer_rect_of(&d, id));
        // Neovim has put them there: the next flush's group, and the live layout again
        d.grids.wins.get_mut(&3).unwrap().float.screen = Some([4, 30]);
        d.reconcile_layers();
        let col = |d: &Daemon| d.panes.by_id(id).unwrap().layer.as_ref().unwrap().group.col;
        assert_eq!(30, col(&d));
        apply_and_flush(&mut d, &floating_layer_at(moved));
        apply_and_flush(&mut d, &floating_layer_at(moved));
        assert_eq!(1, d.panes.list.len());
        // and the pane provider is asked nothing of it but the pane
        assert_eq!(1, fake(&mut d).asks.len());
        drop(d);
        assert_eq!(1, place_group_requests(nvim_end));
    }

    #[test]
    fn a_floating_panes_floats_stay_if_it_is_already_where_rex_says() {
        let Fixture { mut d, nvim_end } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        // a change of size beyond a half cell, but not of its place: the
        // floats' size is the pane's, which is theirs already
        let asked = PICKER_RECT;
        let wider = [asked[0], asked[1], asked[2] + 0.1, asked[3]];
        apply_and_flush(&mut d, &floating_layer_at(wider));
        assert_eq!(Some(wider), layer_rect_of(&d, id));
        drop(d);
        assert_eq!(0, place_group_requests(nvim_end));
    }

    #[test]
    fn a_floating_terminal_that_isnt_a_float_group_panes_moves_no_floats() {
        let Fixture { mut d, nvim_end } = fixture();
        let id = made_picker_pane(&mut d, 60, 20);
        let asked = PICKER_RECT;
        let moved = [asked[0] + 0.1, asked[1], asked[2], asked[3]];
        // another terminal, unknown, floating where it would have moved to
        apply_and_flush(
            &mut d,
            &layout_of(&[("w", &[(block_id(8).as_str(), moved, true)])]),
        );
        assert_eq!(None, layer_rect_of(&d, id));

        // a window's pane of ours, floating: no float group pane
        let w = d.panes.add(Pane::from_neovim_window(50, 1000)).id;
        set_block(&mut d, w, 9);
        apply_and_flush(
            &mut d,
            &layout_of(&[("w", &[(block_id(9).as_str(), moved, true)])]),
        );
        assert_eq!(floating_in("w", moved), d.panes.by_id(w).unwrap().area);
        assert!(d.panes.by_id(w).unwrap().layer.is_none());
        assert_eq!(None, layer_rect_of(&d, id));
        drop(d);
        assert_eq!(0, place_group_requests(nvim_end));
    }

    /// A pane of window 1001 waiting on the open asked for it. Its id.
    fn opening(d: &mut Daemon) -> u32 {
        let p = d.panes.add_for_window(1001);
        let (id, pane) = (p.id, p.attach_id.unwrap());
        p.awaiting_size = true;
        d.act(Ask::OpenBeside {
            pane,
            attach: Vec::new(),
            title: String::new(),
            origin: TerminalId::from(block_id(1).as_str()),
            dir: Direction::Right,
            size: 40,
            ratio: 0.5,
            cols: 40,
            rows: 24,
        });
        id
    }

    fn assert_given_up(d: &Daemon, pane: u32) {
        assert_eq!(
            Some(Close::RequestFailed),
            d.panes.by_id(pane).unwrap().closing
        );
    }

    #[test]
    fn an_open_done_links_its_terminal_to_its_pane() {
        let mut fx = fixture();
        let id = opening(&mut fx.d);
        let f = fake(&mut fx.d);
        f.opened(f.last_id(), "block:n");
        fx.d.drain_provider().unwrap();
        let p = fx.d.panes.by_id(id).unwrap();
        assert_eq!(Some("block:n"), p.terminal.as_ref().map(TerminalId::as_str));
        assert_eq!(None, p.closing);
    }

    #[test]
    fn a_layout_rex_sends_before_an_open_is_done_still_places_its_pane() {
        let (mut fx, mut rex_end) = rex_fixture();
        let id = opening(&mut fx.d);
        let mut rex_says = |d: &mut Daemon, v: Value| {
            rex_end.write_all(&server_frame(&v.to_string())).unwrap();
            d.on_provider(Token::Provider as usize).unwrap();
        };
        let response = |id: u64, payload: Value| json!({ "type": "response", "id": id.to_string(), "seq": 0, "payload": payload });
        // 1: new_split, answered; 2: disown_block, not yet
        rex_says(
            &mut fx.d,
            response(1, json!({ "block_ids": [block_id(5)] })),
        );
        // the view changes, and 3, its fetch, is answered with the new block in it
        rex_says(
            &mut fx.d,
            json!({ "type": "event", "seq": 0, "payload": {
                "type": "session_view_changed", "payload": { "revision": 1 } } }),
        );
        let view = json!({ "revision": 1, "active_window_id": window_id(1),
            "windows": [ { "window_id": window_id(1), "focused_block_id": block_id(5),
                "layers": [ { "kind": "tiled", "layout": {
                    "block_id": block_id(5), "rect": [0.5, 0.0, 0.5, 1.0] } } ] } ] });
        rex_says(&mut fx.d, response(3, view));
        assert_eq!(Area::Unreported, fx.d.panes.by_id(id).unwrap().area);
        // the disown answered: the open is done, and the pane where the view has it
        rex_says(&mut fx.d, response(2, json!({})));
        let p = fx.d.panes.by_id(id).unwrap();
        assert_eq!(
            Some(block_id(5).as_str()),
            p.terminal.as_ref().map(TerminalId::as_str)
        );
        assert_eq!(live(&window_id(1), [0.5, 0.0, 0.5, 1.0]), p.area);
    }

    #[test]
    fn a_failed_open_gives_its_pane_up_and_warns_in_neovim_once() {
        let mut fx = fixture();
        let (one, two) = (opening(&mut fx.d), opening(&mut fx.d));
        for _ in 0..2 {
            let f = fake(&mut fx.d);
            f.answer(f.last_id(), Outcome::Failed("refused: no".into()));
            fx.d.drain_provider().unwrap();
        }
        assert_given_up(&fx.d, one);
        assert_given_up(&fx.d, two);
        assert_eq!(None, fx.d.stopped);
        let sent = sent_by(fx);
        let warning = format!("{NAME}: fake: open_beside failed: refused: no");
        assert_eq!(1, sent.nvim.matches(&warning).count());
    }

    #[test]
    fn a_pane_provider_gone_is_dropped_and_its_pending_open_given_up() {
        let mut fx = fixture();
        let id = opening(&mut fx.d);
        fake(&mut fx.d).push(ProviderEvent::Gone {
            reason: "hung up".into(),
        });
        fx.d.drain_provider().unwrap();
        assert!(fx.d.provider.is_none());
        assert_given_up(&fx.d, id);
        assert_eq!(None, fx.d.stopped);
        assert_eq!(0, fx.d.asks.waiting());

        // a later window is drawn in its origin pane, nothing asked
        let origin = fx.d.panes.add(Pane::from_neovim_window(90, 1000)).id;
        note_split_from(&mut fx.d, 1002, 1000, true, false);
        fx.d.new_pane(1002).unwrap();
        assert_eq!(Some(origin), fx.d.panes.by_win(1002).map(|p| p.id));
        assert!(fx.d.panes.by_id(origin).unwrap().layout.is_some());
    }

    #[test]
    fn a_failed_write_to_rex_on_the_direct_focus_path_drops_rex_and_gives_up_its_opens() {
        let (mut fx, rex_end) = rex_fixture();
        let id = opening(&mut fx.d);
        drop(rex_end); // writes to a socket nobody holds fail
        fx.d.on_direction(windows::Move {
            dir: windows::Direction::Left,
            count: 1,
        })
        .unwrap();
        fx.d.after_batch().unwrap();
        assert!(fx.d.provider.is_none());
        assert_given_up(&fx.d, id);
        assert_eq!(None, fx.d.stopped);
    }

    #[test]
    fn rex_hanging_up_drops_it_and_gives_its_pending_open_up() {
        let (mut fx, rex_end) = rex_fixture();
        let fd = fx.d.rex_ref().unwrap().session.client.fd();
        fx.d.event_loop
            .add_read(fd, Token::Provider as usize)
            .unwrap();
        let id = opening(&mut fx.d);
        rex_end.shutdown(std::net::Shutdown::Both).unwrap();
        fx.d.on_provider(Token::Provider as usize).unwrap();
        assert!(fx.d.provider.is_none());
        assert_given_up(&fx.d, id);
        assert_eq!(None, fx.d.stopped);
    }

    #[test]
    fn a_terminal_opened_for_a_pane_since_gone_is_closed() {
        let mut fx = fixture();
        let id = opening(&mut fx.d);
        let i = fx.d.panes.index_of(id).unwrap();
        fx.d.panes.remove(i, &fx.d.event_loop);
        let f = fake(&mut fx.d);
        f.opened(f.last_id(), "block:n");
        fx.d.drain_provider().unwrap();
        assert_eq!(
            asks::close(TerminalId::from("block:n")),
            fake(&mut fx.d).asked()[1]
        );
    }

    /// A daemon whose fake pane provider can also focus, its panes drawn.
    fn focusing() -> Fixture {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::Focus);
        f.caps.insert(Capability::FocusDirection);
        let mut fx = with_provider(Some(Held::Fake(f)));
        fx.d.nvim.ready = true;
        fx
    }

    /// A pane of window `win`, its terminal `terminal` if it has an id. Its id.
    fn focus_pane(fx: &mut Fixture, id: u32, win: i64, terminal: Option<&str>) -> u32 {
        draw_external(&mut fx.d, id + 2, win, 80, 23);
        let mut p = Pane::from_neovim_window(id, win);
        p.terminal_surface = Some(Surface::for_test(80, 24));
        p.terminal = terminal.map(TerminalId::from);
        p.area = Area::Own;
        fx.d.panes.add(p).id
    }

    fn focus_ask(t: &str) -> Ask {
        Ask::Focus {
            terminal: TerminalId::from(t),
        }
    }

    /// The pane provider says it has focused `t`.
    fn provider_focused(fx: &mut Fixture, t: &str) {
        fake(&mut fx.d).push(ProviderEvent::Focused(TerminalId::from(t)));
        fx.d.drain_provider().unwrap();
    }

    /// The focus ask running is answered.
    fn focus_done(fx: &mut Fixture) {
        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Done(Answer::Nothing));
        fx.d.drain_provider().unwrap();
    }

    /// Past the echo span of a focus that worked by now.
    fn after_echo() -> Instant {
        Instant::now() + Duration::from_millis(super::super::provider_focus::ECHO_MS)
    }

    #[test]
    fn focus_asks_coalesce_to_one_running_and_the_newest_waiting() {
        let mut fx = focusing();
        for t in ["t1", "t2", "t3"] {
            fx.d.focus_terminal(Some(&TerminalId::from(t)));
        }
        // asked already: nothing more
        fx.d.focus_terminal(Some(&TerminalId::from("t3")));
        fx.d.drain_provider().unwrap();
        assert_eq!(vec![focus_ask("t1")], fake(&mut fx.d).asked());
        focus_done(&mut fx);
        assert_eq!(
            vec![focus_ask("t1"), focus_ask("t3")],
            fake(&mut fx.d).asked()
        );
        focus_done(&mut fx);
        // focused: asking again does nothing
        fx.d.focus_terminal(Some(&TerminalId::from("t3")));
        assert_eq!(2, fake(&mut fx.d).asks.len());
    }

    #[test]
    fn another_terminal_reported_while_our_focus_lands_is_ignored_and_a_user_move_just_after_followed()
     {
        let mut fx = focusing();
        let a = focus_pane(&mut fx, 1, 1000, Some("t1"));
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1000;
        provider_focused(&mut fx, "t1");
        // Neovim moved to t2's window: the pane provider is asked to follow
        fx.d.nvim.current_win = 1001;
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        // from before ours landed
        provider_focused(&mut fx, "t1");
        assert_eq!(1001, fx.d.nvim.current_win);
        focus_done(&mut fx);
        provider_focused(&mut fx, "t2");
        // the user moves back in the pane provider, within 250 ms: followed
        provider_focused(&mut fx, "t1");
        assert_eq!(1000, fx.d.nvim.current_win);
        assert_eq!(Some(a), fx.d.current_pane_id());
    }

    #[test]
    fn a_directional_move_just_after_a_focus_is_followed() {
        let mut fx = focusing();
        focus_pane(&mut fx, 1, 1000, Some("t1"));
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        focus_pane(&mut fx, 3, 1002, Some("t3"));
        fx.d.nvim.current_win = 1000;
        provider_focused(&mut fx, "t1");
        // <C-w>w, then <C-w>l at once: the direction waits behind the focus
        fx.d.nvim.current_win = 1001;
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        fx.d.on_direction(windows::Move {
            dir: windows::Direction::Right,
            count: 1,
        })
        .unwrap();
        assert_eq!(1, fake(&mut fx.d).asks.len());
        focus_done(&mut fx);
        let right = Ask::FocusDirection {
            dir: Direction::Right,
            count: 1,
        };
        assert_eq!(Some(&right), fake(&mut fx.d).asked().last());
        focus_done(&mut fx);
        // the pane provider moved focus right, inside the echo span
        provider_focused(&mut fx, "t3");
        assert_eq!(1002, fx.d.nvim.current_win);
    }

    #[test]
    fn keys_typed_while_a_focus_is_unanswered_go_to_the_current_window_and_a_click_moves() {
        let mut fx = focusing();
        let a = focus_pane(&mut fx, 1, 1000, Some("t1"));
        let b = focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1001;
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        let now = Instant::now();
        sent_to_nvim(&mut fx);
        // typed into t1 as focus leaves it: sent as it is, with no switch,
        // to Neovim's current window, t2's
        fx.d.on_read(a, b"x", now).unwrap();
        let sent = sent_to_nvim(&mut fx);
        assert!(
            sent.contains("nvim_input") && sent.contains('x'),
            "{sent:?}"
        );
        assert!(!sent.contains("nvim_set_current_win"), "{sent:?}");
        assert_eq!(1001, fx.d.nvim.current_win);
        assert_eq!(vec![focus_ask("t2")], fake(&mut fx.d).asked());
        // a click is no echo: it moves at once
        fx.d.on_read(a, b"\x1b[<0;5;5M", now).unwrap();
        assert!(sent_to_nvim(&mut fx).contains("nvim_set_current_win"));
        assert_eq!(1000, fx.d.nvim.current_win);
        focus_done(&mut fx);
        assert_eq!(
            vec![focus_ask("t2"), focus_ask("t1")],
            fake(&mut fx.d).asked()
        );
        focus_done(&mut fx);
        // once focus has landed, keys move too
        fx.d.on_read(b, b"x", after_echo()).unwrap();
        assert_eq!(1001, fx.d.nvim.current_win);
        assert_eq!(focus_ask("t2"), fake(&mut fx.d).asked()[2]);
    }

    #[test]
    fn a_focus_in_from_a_pane_with_no_id_makes_it_current_once_it_stands_80_ms() {
        let mut fx = focusing();
        let a = focus_pane(&mut fx, 1, 1000, None);
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1001;
        let ms = |n| Duration::from_millis(n);
        let at = Instant::now();
        // a shorter one doesn't
        fx.d.on_read(a, b"\x1b[I", at).unwrap();
        fx.d.on_read(a, b"\x1b[O", at + ms(10)).unwrap();
        fx.d.focus_in_stood(at + ms(80)).unwrap();
        assert_eq!(1001, fx.d.nvim.current_win);

        fx.d.on_read(a, b"\x1b[I", at).unwrap();
        fx.d.focus_in_stood(at + ms(79)).unwrap();
        assert_eq!(1001, fx.d.nvim.current_win);
        fx.d.focus_in_stood(at + ms(80)).unwrap();
        assert_eq!(1000, fx.d.nvim.current_win);
        // nothing to ask: the pane provider has no id for it
        assert!(fake(&mut fx.d).asked().is_empty());
    }

    #[test]
    fn a_focus_in_within_the_echo_span_is_ignored() {
        let mut fx = focusing();
        let a = focus_pane(&mut fx, 1, 1000, None);
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1001;
        let before = Instant::now();
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        focus_done(&mut fx);
        // focus passing through, as ours landed
        fx.d.on_read(a, b"\x1b[I", before).unwrap();
        assert_eq!(None, fx.d.focus_in);
        // after the span, it waits to stand
        let at = after_echo();
        fx.d.on_read(a, b"\x1b[I", at).unwrap();
        assert_eq!(Some((a, at)), fx.d.focus_in);
    }

    /// A pane waiting on its open whose window isn't current, beside the
    /// current pane, of terminal t1: the pane's id and the open's.
    fn opening_beside_current(fx: &mut Fixture) -> (u32, super::super::provider::AskId) {
        focus_pane(fx, 50, 1000, Some("t1"));
        fx.d.nvim.current_win = 1000;
        provider_focused(fx, "t1");
        let id = opening(&mut fx.d);
        (id, fake(&mut fx.d).last_id())
    }

    fn assert_focus_given_back(fx: &mut Fixture, id: u32) {
        assert_eq!(Some(50), fx.d.current_pane_id());
        assert_ne!(Some(id), fx.d.current_pane_id());
        assert_eq!(Some(&focus_ask("t1")), fake(&mut fx.d).asked().last());
    }

    #[test]
    fn a_new_terminal_that_took_focus_before_its_open_was_done_gives_it_back() {
        let mut fx = focusing();
        let (id, open) = opening_beside_current(&mut fx);
        provider_focused(&mut fx, "block:n");
        fake(&mut fx.d).opened(open, "block:n");
        fx.d.drain_provider().unwrap();
        assert_focus_given_back(&mut fx, id);
    }

    #[test]
    fn a_new_terminal_whose_focus_is_told_after_its_open_was_done_gives_it_back() {
        let mut fx = focusing();
        let (id, open) = opening_beside_current(&mut fx);
        fake(&mut fx.d).opened(open, "block:n");
        fx.d.drain_provider().unwrap();
        assert_eq!(1, fake(&mut fx.d).asks.len());
        provider_focused(&mut fx, "block:n");
        assert_focus_given_back(&mut fx, id);
        // only its first report: later, focus there is the user's
        focus_done(&mut fx);
        provider_focused(&mut fx, "t1");
        let now = Instant::now();
        fx.d.on_focused(&TerminalId::from("block:n"), now).unwrap();
        assert_eq!(Some(id), fx.d.current_pane_id());
    }

    // Pane lifecycle. Already pinned above: a window waiting for its tab
    // page's floating pane that is on its way (and asking for its own once
    // its open fails), and the pane provider going failing the pending opens.

    /// A pane of window `win`, tab page `tab` (0 for none), in Rex block
    /// `block_id(n)`. Its id.
    fn pane_in_block(d: &mut Daemon, win: i64, tab: i64, n: u32) -> u32 {
        let p = d.panes.add_for_window(win);
        p.tab = tab;
        let id = p.id;
        set_block(d, id, n);
        id
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

    /// Everything a daemon asked of its pane provider, and wrote to Neovim.
    struct Sent {
        asks: Vec<Ask>,
        nvim: String,
        nvim_bytes: Vec<u8>,
    }

    impl Sent {
        /// The capabilities asked, in order.
        fn capabilities(&self) -> Vec<&'static str> {
            self.asks.iter().map(|a| a.capability().name()).collect()
        }

        /// How many windows it asked Neovim to close.
        fn neorex_closes(&self) -> usize {
            self.nvim.matches(&format!("{NAME}.close")).count()
        }

        /// The origin of the `n`th ask, an open.
        fn origin(&self, n: usize) -> &str {
            match &self.asks[n] {
                Ask::OpenBeside { origin, .. }
                | Ask::OpenTabPage { origin, .. }
                | Ask::OpenFloat { origin, .. } => origin.as_str(),
                a => panic!("not an open: {a:?}"),
            }
        }
    }

    /// Drops the daemon, ending what it sent, and reads that.
    fn sent_by(fx: Fixture) -> Sent {
        let Fixture {
            mut d,
            mut nvim_end,
        } = fx;
        let asks = d
            .provider
            .as_mut()
            .map(|p| p.fake().asked())
            .unwrap_or_default();
        drop(d);
        let mut nvim = Vec::new();
        nvim_end.read_to_end(&mut nvim).unwrap();
        Sent {
            asks,
            nvim: String::from_utf8_lossy(&nvim).into_owned(),
            nvim_bytes: nvim,
        }
    }

    fn pane_ids(d: &Daemon) -> Vec<u32> {
        d.panes.list.iter().map(|p| p.id).collect()
    }

    #[test]
    fn a_new_window_opens_beside_the_window_it_came_from() {
        for (side_by_side, before, direction) in [
            (true, false, Direction::Right),
            (false, true, Direction::Up),
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
            assert!(new.awaiting_size);
            let new = new.id;
            assert_eq!(vec![first, from, new], pane_ids(&fx.d));
            let sent = sent_by(fx);
            assert_eq!(vec!["open_beside"], sent.capabilities());
            assert_eq!(block_id(2), sent.origin(0));
            let Ask::OpenBeside { dir, .. } = sent.asks[0] else {
                unreachable!()
            };
            assert_eq!(direction, dir);
            assert_eq!(0, sent.neorex_closes());
        }
    }

    #[test]
    fn a_new_window_without_a_known_source_opens_beside_any_pane_with_a_terminal() {
        let mut fx = fixture();
        let placed = pane_in_block(&mut fx.d, 1, 0, 1);
        note_split_from(&mut fx.d, 3, 99, true, false);
        fx.d.new_pane(3).unwrap();
        let new = fx.d.panes.by_win(3).unwrap().id;
        assert_eq!(vec![placed, new], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["open_beside"], sent.capabilities());
        assert_eq!(block_id(1), sent.origin(0));
    }

    #[test]
    fn a_new_window_with_no_pane_with_a_terminal_makes_none_until_the_next_flush() {
        let mut fx = fixture();
        // a pane whose open isn't answered has no terminal to open beside
        let unplaced = fx.d.panes.add_for_window(1).id;
        note_split_from(&mut fx.d, 3, 1, true, false);
        fx.d.new_pane(3).unwrap();
        assert_eq!(vec![unplaced], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
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
        assert_eq!(2, pane_ids(&fx.d).len());
        assert_eq!(first, pane_ids(&fx.d)[1]);
        let sent = sent_by(fx);
        assert_eq!(vec!["open_tab_page"], sent.capabilities());
        // from the pane its window came from, else any with a terminal
        assert_eq!(block_id(9), sent.origin(0));
    }

    #[test]
    fn a_tab_pages_next_window_splits_beside_a_pane_of_its_tab_page_once_it_has_a_block() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.new_pane(1).unwrap();
        let first = fx.d.panes.by_win(1).unwrap().id;
        // the Rex floating pane opens: the next window opens beside it, as the
        // daemon reconciles
        let f = fake(&mut fx.d);
        f.opened(f.last_id(), &block_id(4));
        fx.d.drain_provider().unwrap();
        let second = fx.d.panes.by_win(2).unwrap();
        assert_eq!(2, second.tab);
        let second = second.id;
        assert_eq!(&[first, second], &pane_ids(&fx.d)[1..]);
        let sent = sent_by(fx);
        assert_eq!(vec!["open_tab_page", "open_beside"], sent.capabilities());
        assert_eq!(block_id(4), sent.origin(1));
    }

    #[test]
    fn with_no_pane_provider_a_tab_pages_window_or_one_with_no_pane_to_join_stays_unshown() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.provider = None;
        fx.d.new_pane(1).unwrap();
        note_split_from(&mut fx.d, 5, 0, true, false);
        fx.d.new_pane(5).unwrap();
        assert_eq!(1, fx.d.panes.list.len());
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
        assert_eq!(0, sent.neorex_closes());
    }

    /// The sizes a daemon asked Neovim to give windows, in order: window,
    /// cols, rows.
    fn sizes_sent(all: &[u8]) -> Vec<[i64; 3]> {
        let call = format!("{NAME}.size(...)");
        let call = call.as_bytes();
        let mut sent = Vec::new();
        let mut at = 0;
        while let Some(i) = all[at..].windows(call.len()).position(|w| w == call) {
            at += i + call.len();
            let mut args = msgpack::Decoder::of(&all[at..]);
            assert_eq!(Some(msgpack::Token::Array(3)), args.next().unwrap());
            let mut n = || match args.next().unwrap() {
                Some(msgpack::Token::Int(n)) => n,
                t => panic!("not a size: {t:?}"),
            };
            sent.push([n(), n(), n()]);
        }
        sent
    }

    /// Window `win`, external, drawn on grid `grid` of `cols` x `rows`.
    fn draw_external(d: &mut Daemon, grid: u32, win: i64, cols: u32, rows: u32) {
        d.grids.wins.insert(
            grid,
            Win {
                win,
                kind: WinKind::External,
                float: Float::default(),
            },
        );
        d.grids.grids.insert(grid, Grid::default());
        d.grids.get_mut(grid).unwrap().resize(cols, rows);
    }

    #[test]
    fn sizing_a_window_says_whether_it_asked_neovim() {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::for_test(81, 24));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 81, 23);
        assert!(fx.d.size_window(id));
        // already that size: nothing to ask
        assert!(!fx.d.size_window(id));
        let resize = |d: &mut Daemon, cols, rows| {
            let s = d.panes.by_id_mut(id).unwrap().terminal_surface.as_mut();
            s.unwrap().view.resize(cols, rows);
        };
        resize(&mut fx.d, 60, 24);
        assert!(fx.d.size_window(id));

        // two windows laid out in the pane
        fx.d.reconcile().unwrap();
        note_split_from(&mut fx.d, 1001, 1000, true, true);
        fx.d.on_note(windows::Note::Current(1001)).unwrap();
        draw_external(&mut fx.d, 3, 1001, 30, 23);
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.by_id(id).unwrap().layout.is_some());
        assert!(!fx.d.size_window(id));
        resize(&mut fx.d, 81, 24);
        assert!(fx.d.size_window(id));
        assert!(!fx.d.size_window(id));
    }

    #[test]
    fn with_rex_closed_vsplit_draws_both_windows_side_by_side_in_one_pane_and_q_gives_the_space_back()
     {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        // the pane neorex was run in, 81 x 24: its window 81 x 23, over the
        // command line
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::for_test(81, 24));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 81, 23);
        fx.d.reconcile().unwrap();

        // :vsplit, without 'splitright': the new window on the left, current
        // before it's drawn
        note_split_from(&mut fx.d, 1001, 1000, true, true);
        fx.d.on_note(windows::Note::Current(1001)).unwrap();
        draw_external(&mut fx.d, 3, 1001, 40, 23);
        fx.d.reconcile().unwrap();
        assert_eq!(vec![id], pane_ids(&fx.d));
        let pane = fx.d.panes.by_id(id).unwrap();
        assert_eq!(vec![1001, 1000], pane.windows());
        // keys typed in the pane go to it, not back to the window it came from
        assert_eq!(Some(1001), pane.neovim_window_handle);
        // 40 columns each, the separator between
        let (wins, separators) = fx.d.laid_out_view(pane);
        assert_eq!(
            vec![
                Shown {
                    grid: 3,
                    row: 0,
                    col: 0,
                    cols: 40,
                    rows: 23,
                    statusline: None
                },
                Shown {
                    grid: 2,
                    row: 0,
                    col: 41,
                    cols: 40,
                    rows: 23,
                    statusline: None
                },
            ],
            wins
        );
        assert_eq!(
            vec![Separator {
                row: 0,
                col: 40,
                rows: 23
            }],
            separators
        );

        // :q in the new window: the other gets the pane back
        fx.d.grids.wins.swap_remove(&3);
        fx.d.reconcile().unwrap();
        assert_eq!(vec![id], pane_ids(&fx.d));
        let pane = fx.d.panes.by_id(id).unwrap();
        assert_eq!(vec![1000], pane.windows());
        assert!(pane.layout.is_none());
        assert_eq!(None, fx.d.stopped);

        // and :q in the last ends the pane
        fx.d.grids.wins.swap_remove(&2);
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.list.is_empty());
        assert_eq!(Some(StopReason::PaneGone), fx.d.stopped);

        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
        assert_eq!(
            vec![
                [1000, 81, 23],
                [1001, 40, 23],
                [1000, 40, 23],
                [1000, 81, 23]
            ],
            sizes_sent(&sent.nvim_bytes)
        );
    }

    #[test]
    fn with_rex_closed_a_pane_of_two_windows_draws_both_and_the_separator_between() {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        // a terminal whose output the test reads
        let (master, slave) = surface::open_pty(81, 24);
        let (socket, _helper) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::create(socket, slave, surface::cells(81, 24)));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 81, 23);
        note_split_from(&mut fx.d, 1001, 1000, true, true);
        draw_external(&mut fx.d, 3, 1001, 40, 23);
        fx.d.reconcile().unwrap();
        for (grid, text) in [(2, "right"), (3, "left")] {
            let g = fx.d.grids.get_mut(grid).unwrap();
            for (i, c) in text.chars().enumerate() {
                g.cells[i] = Cell::init(c.to_string(), 0);
            }
        }
        fx.d.grids.get_mut(2).unwrap().resize(40, 23);

        fx.d.draw().unwrap();
        assert!(fx.d.panes.by_id(id).is_some());
        // SAFETY: reads what the frame wrote from the pty's master, without
        // waiting, then closes it.
        let written = unsafe {
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = libc::read(master, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n as usize]);
            }
            libc::close(master);
            String::from_utf8_lossy(&all).into_owned()
        };
        // the left window from the first column, the right after the separator
        assert!(written.contains("\x1b[1;1Hleft"), "{written:?}");
        assert!(written.contains("\u{2502}right"), "{written:?}");
    }

    #[test]
    fn with_rex_closed_a_resize_neovim_makes_in_a_pane_of_several_windows_is_kept() {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::for_test(80, 21));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 80, 20);
        // :split, without 'splitbelow': the new window above
        note_split_from(&mut fx.d, 1001, 1000, false, true);
        draw_external(&mut fx.d, 3, 1001, 80, 10);
        fx.d.reconcile().unwrap();
        fx.d.on_note(windows::Note::Current(1001)).unwrap();
        // :resize 5 in it: 4 rows of text and its statusline
        fx.d.on_note(windows::Note::Resized {
            win: 1001,
            cols: 80,
            rows: 5,
        })
        .unwrap();
        let pane = fx.d.panes.by_id(id).unwrap();
        let (wins, _) = fx.d.laid_out_view(pane);
        assert_eq!(
            vec![(3, 0, 5), (2, 5, 15)],
            wins.iter()
                .map(|w| (w.grid, w.row, w.rows))
                .collect::<Vec<_>>()
        );
        let sent = sent_by(fx);
        let sizes = sizes_sent(&sent.nvim_bytes);
        assert_eq!(&[[1001, 80, 5], [1000, 80, 15]], &sizes[sizes.len() - 2..]);
    }

    #[test]
    fn with_rex_closed_a_size_asked_before_a_window_joins_its_pane_is_kept_as_it_joins() {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::for_test(80, 21));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 80, 20);
        // `:split | resize 5`, told as the new window is made external
        note_split_from(&mut fx.d, 1001, 1000, false, true);
        draw_external(&mut fx.d, 3, 1001, 80, 4);
        fx.d.on_note(windows::Note::Resized {
            win: 1001,
            cols: 0,
            rows: 5,
        })
        .unwrap();
        fx.d.reconcile().unwrap();
        let pane = fx.d.panes.by_id(id).unwrap();
        let (wins, _) = fx.d.laid_out_view(pane);
        assert_eq!(
            vec![(3, 0, 80, 5), (2, 5, 80, 15)],
            wins.iter()
                .map(|w| (w.grid, w.row, w.cols, w.rows))
                .collect::<Vec<_>>()
        );
        assert!(fx.d.pending_sizes.is_empty());
    }

    #[test]
    fn with_laststatus_0_a_window_above_another_gets_a_statusline_drawn_for_it() {
        let mut fx = fixture();
        fx.d.provider = None;
        fx.d.nvim.ready = true;
        fx.d.grids.statusline_hl = 7;
        fx.d.grids.statusline_nc_hl = 8;
        fx.d.on_note(windows::Note::Options(windows::SplitOptions {
            statuslines: false,
            ..windows::SplitOptions::default()
        }))
        .unwrap();
        let mut p = Pane::from_neovim_window(1, 1000);
        p.terminal_surface = Some(Surface::for_test(80, 21));
        let id = fx.d.panes.add(p).id;
        fx.d.nvim.current_win = 1000;
        draw_external(&mut fx.d, 2, 1000, 80, 20);
        // :split, the new window above and current
        note_split_from(&mut fx.d, 1001, 1000, false, true);
        fx.d.on_note(windows::Note::Current(1001)).unwrap();
        draw_external(&mut fx.d, 3, 1001, 80, 9);
        fx.d.reconcile().unwrap();
        let pane = fx.d.panes.by_id(id).unwrap();
        let (wins, _) = fx.d.laid_out_view(pane);
        // the one above: its last row a statusline of ours, in StatusLine as
        // it's current; the one below has none, as in Neovim
        assert_eq!(
            vec![(3, 0, 10, Some(7)), (2, 10, 10, None)],
            wins.iter()
                .map(|w| (w.grid, w.row, w.rows, w.statusline))
                .collect::<Vec<_>>()
        );
        let sent = sent_by(fx);
        let sizes = sizes_sent(&sent.nvim_bytes);
        // Neovim's window above is a row short of its place
        assert_eq!(&[[1001, 80, 9], [1000, 80, 10]], &sizes[sizes.len() - 2..]);
    }

    #[test]
    fn a_size_asked_before_a_window_is_external_goes_once_it_gets_a_pane_or_none() {
        let told = |d: &mut Daemon, win: i64| {
            d.on_note(windows::Note::Resized {
                win,
                cols: 0,
                rows: 5,
            })
            .unwrap();
            assert_eq!(1, d.pending_sizes.len());
        };
        // Rex gives it a pane of its own
        let mut fx = fixture();
        pane_in_block(&mut fx.d, 1000, 0, 1);
        note_split_from(&mut fx.d, 1001, 1000, false, true);
        told(&mut fx.d, 1001);
        fx.d.new_pane(1001).unwrap();
        assert!(fx.d.pending_sizes.is_empty());
        // with Rex closed, a window of a later tab page stays unshown
        let mut fx = two_windows_of_a_tab_page();
        fx.d.provider = None;
        told(&mut fx.d, 1);
        fx.d.new_pane(1).unwrap();
        assert!(fx.d.pending_sizes.is_empty());
        // and with no pane to join
        let mut fx = fixture();
        fx.d.provider = None;
        note_split_from(&mut fx.d, 1001, 1000, false, true);
        told(&mut fx.d, 1001);
        fx.d.new_pane(1001).unwrap();
        assert!(fx.d.pending_sizes.is_empty());
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
            Some(Close::TerminalHungUp),
            fx.d.panes.by_id(id).unwrap().closing
        );
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
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
            Some(Close::TerminalHungUp),
            fx.d.panes.by_id(id).unwrap().closing
        );
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
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
        assert_eq!(vec!["open_float"], sent.capabilities());
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
        assert!(sent.asks.is_empty());
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
        assert_eq!(vec!["open_float"], sent.capabilities());
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
    fn after_a_flush_a_pane_whose_window_is_gone_is_removed_and_its_terminal_closed() {
        let (mut fx, [_, kept]) = window_1_gone_from_two_panes();
        fx.d.reconcile().unwrap();
        assert_eq!(None, fx.d.stopped);
        assert_eq!(vec![kept], pane_ids(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["close"], sent.capabilities());
        assert_eq!(
            asks::close(TerminalId::from(block_id(1).as_str())),
            sent.asks[0]
        );
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
            assert!(sent.asks.is_empty(), "reopened={reopened}");
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
        assert_eq!(vec!["close"], sent.capabilities());
        assert_eq!(0, sent.neorex_closes());
    }
}
