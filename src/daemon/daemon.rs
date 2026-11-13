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
use super::exe_stamp::VersionRefused;
use super::float_homes::FloatHomes;
use super::floating_panes::{self, FloatingPanes};
use super::focus_gate::AimForwarder;
use super::geometry::{self, Geometry};
use super::live_layout::{Area, LiveLayout};
use super::nvim_session::{NvimSession, Shrink};
use super::pane::{self, Close, Fit, LastAskedWinSize, Pane, PaneFd, Placing, SelfAttach};
use super::pane_layout::{self, Axis};
use super::panes::Panes;
use super::provider::{
    Answer, Ask, Capability, Direction, PaneProvider, ProviderEvent, TerminalId,
};
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
use crate::screen::float_groups;
use crate::screen::grid::Cell;
use crate::screen::grids::{Grids, WinKind};
use crate::screen::pane_view::{self, LaidOut, Opts, Rect, Separator, Shown, tabline_slice};
use crate::term::input;
use indexmap::IndexMap;

mod attaches;
mod choosing;
pub(crate) mod control;
mod lifetime;
mod open;
mod self_attach;
mod statuslines;
use attaches::PENDING_BASE;

/// The daemon is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

pub(crate) struct Daemon {
    /// Our executable and the helpers' socket, for new panes' command.
    pub exe: std::path::PathBuf,
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
    /// What a self-attached pane asked of Neovim for its window, while
    /// unanswered: one at a time, in the order they attached.
    window_ask: Option<self_attach::WindowAsk>,
    /// Self-attaches that may come from Neovim's own terminals, waiting for
    /// it to list them.
    nested_checks: Vec<attaches::NestedCheck>,
    /// The `open` control requests not yet answered.
    opens: open::Opens,
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
    /// The ask for the statuslines neopane draws (`ask_statuslines`).
    pub statuslines_ask: statuslines::OneAsk,
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
    /// Whether the daemon has a pane, or is past its last (ADR 0003).
    pub(crate) lifetime: lifetime::Lifetime,
    /// What Neovim told of itself that `status` shows, never asking it.
    pub(crate) told: control::Told,
    /// Ending, as a `stop` asked: attaches are refused, and it stops once
    /// the terminals it requested are closed.
    stopping: bool,
    /// What `status` shows while there's no pane provider: `none`, and why
    /// if it was refused or skipped.
    no_provider: control::NoProvider,
    /// Neovim's exit status, once it has quit and exited: the self-attached
    /// panes' helpers exit with it.
    pub quit_status: Option<u8>,
    /// The acts asked of the pane provider, or waiting to be.
    pub asks: Asks,
    /// When the ask timer is set to fire, if it is.
    ask_timer_due: Option<Instant>,
    /// When the attach timer is set to fire, if it is.
    attach_timer_due: Option<Instant>,
    /// Set once a requested pane's terminal was refused for its version:
    /// while it holds, new windows are drawn in their origin panes (#91
    /// decision 24).
    version_refused: Option<VersionRefused>,
    /// Unshown windows of the first tab page whose terminal won't come,
    /// each waiting for a pane that can draw it, and how it was split off.
    unshown_awaiting_origin: Vec<(i64, windows::Split)>,
    /// Where float groups with no float group pane are drawn (ADR 0010).
    float_homes: FloatHomes,
    /// The tab pages with no tab page area that would get one, each drawn
    /// whole in the pane it opened in, for its life (ADR 0012).
    tabs_in_panes: Vec<i64>,
    /// Which terminal the pane provider has focused, as last said or asked.
    pub focus: ProviderFocus,
    /// A pane with no terminal id whose terminal reported a focus-in, and
    /// when: it becomes current if the report stands `FOCUS_IN_SETTLE_MS`.
    focus_in: Option<(u32, Instant)>,
    /// Terminals just opened whose windows weren't current, until their first
    /// focus report or a deadline: one that took focus gives it back (#86 D8).
    just_opened: Vec<(TerminalId, Instant)>,
    /// The choice running, while its `find` hasn't confirmed it.
    choosing: Option<choosing::Choosing>,
    /// The config file, as found at start: the config chooses if it is
    /// there at a choice (ADR 0007).
    pub config_file: Option<crate::providers::choose::ConfigFile>,
    /// The config running for a choice, and the pane of the choosing
    /// attach, until it answers.
    config_run: Option<(u32, crate::providers::choose::ConfigRun)>,
    /// Self-attaches that came while a choice ran, waiting their turn.
    waiting_choice: Vec<u32>,
    /// The choosing attach's values of the pane provider's context
    /// variables, in its order: a later self-attach's must match.
    context: Vec<Option<Vec<u8>>>,
    /// Whether the loop watches the pane provider's fd for room to write.
    provider_writes: bool,
    /// What tests have a self-attach choose, given its hello, in place of
    /// the built-in default.
    #[cfg(test)]
    pub test_choose: Option<TestChoose>,
    /// What the first self-attach chose, before Neovim started, for `start`.
    pub first_choice: Option<crate::providers::choose::Chosen>,
    /// The config the first self-attach started running, before Neovim
    /// started, for `start` to wait for.
    pub first_config: Option<crate::providers::choose::ConfigRun>,
    /// Warnings for Neovim held until it is set up (`warn_when_ready`).
    held_warnings: Vec<String>,
    // Dropped last, in this order: the loop, then the pane provider, then
    // Neovim.
    pub event_loop: Loop,
    /// The pane provider, one at a time: None before a self-attach chooses
    /// one, or once it is dropped, gone or with no terminal in its reach.
    pub provider: Option<Box<dyn PaneProvider>>,
    pub nvim: NvimSession,
}

/// What a test has a self-attach choose, given its hello.
#[cfg(test)]
pub(crate) type TestChoose = Box<dyn FnMut(&pane::AttachHello) -> crate::providers::choose::Chosen>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopReason {
    NvimExited,
    PaneGone,
    Signal,
    /// A `stop` control request.
    Stopped,
}

impl StopReason {
    /// The reason's name, as logged.
    pub(crate) fn tag_name(self) -> &'static str {
        match self {
            StopReason::NvimExited => "nvim_exited",
            StopReason::PaneGone => "pane_gone",
            StopReason::Signal => "signal",
            StopReason::Stopped => "stopped",
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
    AttachTimer,
    ChoiceTimer,
    UnattachTimer,
    StopTimer,
    /// The config's run for a choice is done.
    Config,
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
            12 => Token::AttachTimer,
            13 => Token::ChoiceTimer,
            14 => Token::UnattachTimer,
            15 => Token::StopTimer,
            16 => Token::Config,
            _ => return None,
        })
    }
}

// the fixed tokens must stay below the pane tokens
const _: () = assert!((Token::Config as usize) < pane::TOKEN_BASE);

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

/// Neovim's screen in a kept daemon before its first attach: a terminal's
/// usual size.
const KEPT_SIZE: [u32; 2] = [80, 24];

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
            Some(StopReason::Signal | StopReason::Stopped) => Ending::Stopped,
            Some(StopReason::PaneGone) => Ending::Closed,
            None => Ending::Failed,
        };
        self.panes.deinit(ending, self.quit_status);
        log::flush_traces();
    }
}

impl Daemon {
    /// A daemon on `event_loop`, Neovim and `provider`, with no panes yet.
    pub(crate) fn new(
        event_loop: Loop,
        nvim: NvimSession,
        provider: Option<Box<dyn PaneProvider>>,
        exe: std::path::PathBuf,
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
            window_ask: None,
            nested_checks: Vec::new(),
            opens: open::Opens::default(),
            uid: super::attach_conn::euid(),
            grids: Grids::default(),
            panes: Panes::default(),
            changes: WindowChanges::default(),
            tabline_rows: 0,
            msg_rows: 1,
            split_options: windows::SplitOptions::default(),
            statuslines_ask: statuslines::OneAsk::default(),
            pending_sizes: Vec::new(),
            tab_layouts: IndexMap::new(),
            live_layout: None,
            area_timer_due: None,
            escape_timer_due: None,
            stopped: None,
            lifetime: lifetime::Lifetime::default(),
            told: control::Told::default(),
            stopping: false,
            no_provider: control::NoProvider::None,
            quit_status: None,
            asks: Asks::default(),
            ask_timer_due: None,
            attach_timer_due: None,
            version_refused: None,
            unshown_awaiting_origin: Vec::new(),
            tabs_in_panes: Vec::new(),
            focus: ProviderFocus::default(),
            float_homes: FloatHomes::default(),
            focus_in: None,
            just_opened: Vec::new(),
            choosing: None,
            config_file: None,
            config_run: None,
            waiting_choice: Vec::new(),
            context: Vec::new(),
            provider_writes: false,
            #[cfg(test)]
            test_choose: None,
            first_choice: None,
            first_config: None,
            held_warnings: Vec::new(),
            event_loop,
            provider,
            nvim,
        }
    }

    /// Takes the `first` self-attach, already accepted, as a pane, and
    /// starts talking to Neovim, sized to it. It takes what it chose
    /// (`first_choice`), or chooses now. On a failure, `first` is given back,
    /// for its attach to end with why.
    pub(crate) fn start(&mut self, first: &mut Option<SelfAttach>) -> Result<()> {
        // only ever called on startup
        debug_assert!(self.panes.list.is_empty());

        let attach = first.take().expect("the first self-attach is taken once");
        let id = self.panes.add_self_attached(attach).id;
        let started = self.start_with(id);
        if started.is_err() {
            let pane = self.panes.list.pop().expect("the pane just added");
            pane.unregister_from(&self.event_loop);
            *first = pane.into_self_attach();
        }
        started
    }

    /// Starts a kept daemon (`--detach`): talking to Neovim at `KEPT_SIZE`,
    /// unattached and asked to stay, with no pane. The first attach takes
    /// the current window, and the rest of its tab page follows it.
    pub(crate) fn start_kept(&mut self) -> Result<()> {
        debug_assert!(self.panes.list.is_empty());
        self.lifetime.asked_to_stay = true;
        self.lifetime.stage = lifetime::Stage::Staying;
        let [cols, rows] = KEPT_SIZE;
        self.nvim
            .attach_ui(cols, rows)
            .map_err(|e| format!("couldn't talk to Neovim: {e}"))?;
        Ok(())
    }

    /// `start`, once pane `id` is added.
    fn start_with(&mut self, id: u32) -> Result<()> {
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.register_with(&self.event_loop)?;
        // no requested pane: its own area until a live layout places it,
        // drawn and fitted as before there was any
        pane.area = Area::Own;
        let view = &pane.terminal_surface.as_ref().unwrap().view;
        let (cols, rows) = (view.cols, view.rows);
        self.nvim
            .attach_ui(cols, rows)
            .map_err(|e| format!("couldn't talk to Neovim: {e}"))?;
        match (self.first_choice.take(), self.first_config.take()) {
            (Some(chosen), _) => self.take_choice(id, chosen)?,
            (None, Some(run)) => self.wait_for_config(id, run)?,
            (None, None) => self.choose_or_reach(id)?,
        }
        self.nvim.client.flush()?;
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
                let handled = self.handle(e);
                self.unless_nvim_gone(handled)?;
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
                let done = self.after_batch();
                self.unless_nvim_gone(done)?;
            }
        }
        Ok(self.stopped.unwrap())
    }

    /// `result`, unless it failed as Neovim's stdin had closed: Neovim is
    /// gone, or going, as when `:q` exits after its last redraw and the
    /// answer to that can't be written. That ends the loop as Neovim
    /// exiting, for its status to be waited for, rather than as a failure.
    fn unless_nvim_gone(&mut self, result: Result<()>) -> Result<()> {
        match result {
            Err(_) if self.nvim.client.is_closed() => {
                log_info!("{NAME} daemon: Neovim closed its stdin");
                self.stop(StopReason::NvimExited);
                Ok(())
            }
            r => r,
        }
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
            Some(Token::AttachTimer) => self.time_out_attaches(Instant::now())?,
            Some(Token::ChoiceTimer) => self.time_out_choice(Instant::now())?,
            Some(Token::UnattachTimer) => self.unattach_wait_over()?,
            Some(Token::StopTimer) => self.stop_wait_over(),
            Some(Token::Config) => self.on_config()?,
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
        self.tell_panes_show()?;
        // what was asked in the batch, and anything a write that failed left
        self.drain_provider()?;
        if self.stopping_done() {
            return Ok(());
        }
        self.drop_provider_out_of_reach()?;
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

    /// Tells Neovim the windows panes show, when they changed.
    fn tell_panes_show(&mut self) -> Result<()> {
        let panes = &self.panes;
        self.nvim.tell_panes_show(|wins| {
            for p in &panes.list {
                if p.layer.is_some() || p.closing.is_some() {
                    continue;
                }
                match &p.layout {
                    Some(l) => l.add_windows(wins),
                    None => wins.extend(p.neovim_window_handle),
                }
            }
        });
        // `gt` goes to a tab page's window last current in the current pane
        let held = panes
            .list
            .iter()
            .filter(|p| !p.held.is_empty() && p.closing.is_none())
            .map(Pane::windows)
            .collect();
        self.nvim.tell_panes_hold(held);
        Ok(self.nvim.client.flush()?)
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
        self.read_layout_again();
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
        // looked for only where it's used: not on every key Rex's panes read
        let report = pane
            .terminal_surface
            .as_ref()
            .filter(|_| terminal.is_none() || self.reads_after_changes())
            .and_then(|s| s.forwarder.focus_report(bytes));
        if report == Some(FocusReport::In) {
            self.read_layout_again();
        }
        if terminal.is_none() {
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
        // a key or click in a float group pane goes into its floats
        let pane = self.panes.by_id(id).unwrap();
        if pane.layer.is_some()
            && pane
                .terminal_surface
                .as_ref()
                .is_some_and(|s| s.forwarder.taking(bytes) != Taking::Nothing)
        {
            self.enter_float_group_pane(id)?;
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
        // unshown until there's a pane provider to open it, or none
        if self.waits_for_reach(self.changes.split_of(win)) {
            return Ok(());
        }
        // a size asked of it before it was external counts only in a pane
        // layout; a pane of its own is its size
        let size = self
            .pending_sizes
            .iter()
            .position(|&(w, ..)| w == win)
            .map(|i| self.pending_sizes.swap_remove(i));
        let sp = self.changes.split_of(win);
        if self.provider.is_none()
            || (sp.tab == 0 && !self.opens_terminal(sp))
            || self.tab_page_in_pane(sp)
        {
            // drawn in its origin pane (#112), a tab page's whole (ADR 0012)
            self.lay_out_in_origin(win, sp, size);
            return Ok(());
        }
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
        let p = self.provider.as_deref();
        let open_beside = self.asks.may_ask(p, Capability::OpenBeside);
        let opens = self.asks.may_ask(p, Capability::Open);
        let places = self.asks.may_ask(p, Capability::PlaceBeside);
        // a terminal opened and never placed needs no origin
        let unplaced = beside && !open_beside && !places;
        if origin.is_none() && !unplaced {
            return Ok(()); // no origin yet: next flush
        }
        // starts at the guess, not Rex's default
        let size = self.new_pane_size(win);
        let [cols, rows] = size;
        let origin_size = origin
            .as_ref()
            .and_then(|o| self.panes.by_terminal(o.as_str()))
            .and_then(|f| f.terminal_surface.as_ref())
            .map(|s| [s.view.cols, s.view.rows]);
        let pane = self.panes.add_for_window(win);
        pane.tab = sp.tab;
        pane.shown_tab = sp.tab;
        pane.split = Some(sp);
        let (pane_id, id) = (pane.id, pane.attach_id.expect("a requested pane"));
        let attach = pane::attach_words(&self.exe, &self.sock, self.transport, pane);
        let title = pane::title(&id);
        self.changes.forget_split(win);

        if !beside {
            self.act(Ask::OpenTabPage {
                pane: id,
                attach,
                title,
                origin: origin.expect("a tab page's origin"),
                cols,
                rows,
            });
            return Ok(());
        }
        // its terminal waits for its layout while it says the unlaid size,
        // whether opened beside its origin or opened and then placed
        self.panes.by_id_mut(pane_id).unwrap().awaiting_size = true;
        let dir = Direction::of_split(sp.side_by_side, sp.before);
        let axis = usize::from(!dir.across());
        let along = size[axis];
        // its share of the room it splits with its origin
        let ratio = match origin_size {
            Some(o) if o[axis] > 0 => (f64::from(along) / f64::from(o[axis])).clamp(0.0, 1.0),
            _ => 0.5,
        };
        if !open_beside && opens {
            // opened, then placed once it attaches (ADR 0006), if it can be
            if let Some(origin) = origin.filter(|_| places) {
                self.panes.by_id_mut(pane_id).unwrap().placing = Some(Placing {
                    origin,
                    dir,
                    size: along,
                    ratio,
                });
            }
            self.act(Ask::Open {
                pane: id,
                attach,
                title,
                cols,
                rows,
            });
            log::trace(Point::Split, u64::from(pane_id));
            return Ok(());
        }
        self.act(Ask::OpenBeside {
            pane: id,
            attach,
            title,
            origin: origin.expect("an origin to open beside"),
            dir,
            size: along,
            ratio,
            cols,
            rows,
        });
        log::trace(Point::Split, u64::from(pane_id));
        Ok(())
    }

    /// Whether a window of the first tab page, split off as `sp`, gets a
    /// terminal of its own (#112). It does when:
    /// - the pane provider can open one, beside its origin pane or to be
    ///   placed there, and that pane is in reach;
    /// - opened without being entered, focus can be given back to the
    ///   current pane's terminal (#86 D5);
    /// - no version refusal holds (#91 decision 24).
    fn opens_terminal(&mut self, sp: windows::Split) -> bool {
        let p = self.provider.as_deref();
        let opens =
            self.asks.may_ask(p, Capability::OpenBeside) || self.asks.may_ask(p, Capability::Open);
        if !opens || !self.in_reach(sp) {
            return false;
        }
        if sp.win != self.nvim.current_win && !self.can_give_focus_back() {
            return false;
        }
        let Some(refused) = self.version_refused else {
            return true;
        };
        if refused.holds(&self.exe) {
            return false;
        }
        log_info!(
            "{NAME} daemon: {} changed: new windows get terminals again",
            self.exe.display()
        );
        self.version_refused = None;
        true
    }

    /// Whether a window of a tab page that gets a place of its own (a
    /// split's `tab`), split off as `sp`, is drawn in a pane with the rest of
    /// its tab page (ADR 0012): its tab page already is, or has no tab page
    /// area and can't ask for one, the pane provider having no
    /// `open_tab_page` or the origin pane being out of its reach.
    fn tab_page_in_pane(&self, sp: windows::Split) -> bool {
        if sp.tab == 0 {
            return false;
        }
        if self.tabs_in_panes.contains(&sp.tab) {
            return true;
        }
        let p = self.provider.as_deref();
        !self.has_area_pane(sp.tab)
            && (!self.asks.may_ask(p, Capability::OpenTabPage) || !self.in_reach(sp))
    }

    /// Whether `give_focus_back` can have the pane provider focus the
    /// current pane's terminal: it can focus, and has an id for it, or will
    /// have. A current window made in the same flush, as `:vsplit | vsplit`
    /// or a session load make them, may have no pane yet, or one whose open
    /// is still on its way: its id comes with the open.
    fn can_give_focus_back(&self) -> bool {
        let p = self.provider.as_deref();
        if !self.asks.may_ask(p, Capability::Focus) {
            return false;
        }
        match self.current_pane() {
            Some(q) => q.terminal.is_some() || (!q.is_self_attached() && q.closing.is_none()),
            None => true,
        }
    }

    /// Whether the origin pane of a window split off as `sp` is in the pane
    /// provider's reach (#86 D9).
    fn in_reach(&self, sp: windows::Split) -> bool {
        self.split_from(sp)
            .is_none_or(|f| f.reach != pane::Reach::Out)
    }

    /// The pane a window split off as `sp` is drawn in when it has no
    /// terminal of its own: its origin window's, else the current one's, else
    /// the first that can take it. Only a window pane in no tab page area,
    /// whatever tab pages it holds, already showing a window, and not one
    /// still waiting for a terminal of its own.
    fn origin_pane(&self, sp: windows::Split) -> Option<u32> {
        let takes = |p: &&Pane| {
            p.layer.is_none()
                && p.tab == 0
                && p.closing.is_none()
                && p.neovim_window_handle.is_some()
                && (p.is_self_attached() || p.terminal_surface.is_some())
        };
        self.panes
            .by_win(sp.from)
            .filter(takes)
            .or_else(|| self.current_pane().filter(takes))
            .or_else(|| self.panes.list.iter().find(takes))
            .map(|p| p.id)
    }

    /// Window `win` had no pane of its own: it joins its origin pane's layout,
    /// beside its origin window as Neovim put it (`sp`), at `size` if Neovim
    /// gave it one. A window of another tab page than the pane shows joins
    /// that tab page's layout there, held, or starts one: the pane shows it
    /// once one of its windows is current (ADR 0012). One with no pane to
    /// join stays unshown. Whether it joined one.
    fn lay_out_in_origin(
        &mut self,
        win: i64,
        sp: windows::Split,
        size: Option<(i64, u32, u32)>,
    ) -> bool {
        let Some(id) = self.origin_pane(sp) else {
            return false;
        };
        if sp.tab != 0 && !self.tabs_in_panes.contains(&sp.tab) {
            self.tabs_in_panes.push(sp.tab);
        }
        let equalalways = self.split_options.equalalways;
        let p = self.panes.by_id_mut(id).unwrap();
        p.join(windows::Split { win, ..sp }, equalalways);
        // Neovim said it was current before it was drawn
        if self.nvim.current_win == win {
            p.show_tab_of(win);
            p.neovim_window_handle = Some(win);
        }
        self.changes.forget_split(win);
        if let Some((_, cols, rows)) = size
            && self.panes.by_id(id).unwrap().shows(win)
        {
            self.resize_in_layout(id, win, cols, rows);
        }
        true
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
        self.any_terminal(|q| q.tab == sp.tab && q.layer.is_none() && !q.is_self_attached())
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
        self.read_layout_again();
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
        if !self.panes_count_but(Some(id)) && self.lifetime.is_attached() {
            return self.go_unattached(Some(id));
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
                            self.ask_statuslines()?;
                            // once Neovim has drawn its start: before, it
                            // would wait at a hit-enter prompt
                            if self.nvim.ready && !self.held_warnings.is_empty() {
                                self.warn_held();
                                self.nvim.client.flush()?;
                            }
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
            windows::Note::Opened(win) => self.on_window_opened(win)?,
            windows::Note::Split(sp) => {
                if self.panes.by_win(sp.win).is_none() {
                    self.changes.note_split(sp);
                }
            }
            windows::Note::Layout(l) => self.on_tab_layout(l)?,
            windows::Note::Options(o) => self.on_split_options(o)?,
            windows::Note::Resized { win, cols, rows } => self.on_resized(win, cols, rows)?,
            windows::Note::Unsaved { files, ask } => self.on_unsaved(files, ask)?,
            windows::Note::Cwd(dir) => self.told.dir = Some(dir),
            windows::Note::Terminals(ptys) => self.told.terminals = ptys,
            windows::Note::Done { buf, by_pane } => self.on_done(buf, by_pane),
            windows::Note::Statuslines => self.ask_statuslines()?,
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
        if self.on_statuslines_answer(id, err, result)?
            || self.on_window_answer(id, err, result)?
            || self.on_nested_answer(id, err, result)?
            || self.on_open_answer(id, err, result)?
        {
            return Ok(());
        }
        if let Some(e) = err {
            log_warn!("neovim error: {}", String::from_utf8_lossy(e));
        } else if id == self.nvim.channel_req {
            // the first window is the self-attached pane's (#83); a daemon
            // started with --detach sets it up at the screen's size but its
            // last row, and an attach that came meanwhile is served after
            let size = match self.panes.self_attached_awaiting_window() {
                _ if self.lifetime.asked_to_stay => [KEPT_SIZE[0], KEPT_SIZE[1] - self.msg_rows],
                Some(first) => self.geometry().window_size(first),
                None => return Ok(()),
            };
            let mut result = result;
            let chan = windows::parse_channel(&mut result)?;
            self.nvim.setup_req = windows::setup(&mut self.nvim.client, chan, size[0], size[1]);
            self.nvim.client.flush()?;
        } else if id == self.nvim.setup_req {
            let mut result = result;
            let setup = windows::parse_setup(&mut result)?;
            let first_win = setup.wins[0];
            if self.lifetime.asked_to_stay {
                // a daemon started with --detach on no file: its first window
                // becomes the window kept for the next attach, as one a last
                // pane leaves, so a file attach closes it. Windows its
                // arguments opened stay, for the first attach to show.
                if setup.throwaway {
                    self.lifetime.kept = Some(lifetime::Kept {
                        win: first_win,
                        tab: 0,
                    });
                }
            } else if let Some(first) = self.panes.self_attached_awaiting_window_mut() {
                first.neovim_window_handle = Some(first_win);
            }
            self.nvim.ready = true;
            // its window is in its pane now: the editor can span the Rex window
            if let Some(f) = self.current_pane_id() {
                self.nvim.fit_ui_to(self.panes.by_id(f).unwrap())?;
            }
            // self-attaches that came meanwhile, in the order they came
            self.give_windows()?;
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
        self.end_shrink()?;
        let Some(p) = self.panes.by_win_mut(win) else {
            return Ok(());
        };
        let id = p.id;
        self.trace_tab_move(from_tab, id);
        self.show_in_pane(id, win)?;
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
        // a window held for another tab page keeps its share there
        if p.layout.is_none() || !p.shows(win) {
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
        // told without the statusline neopane draws it
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

    /// `<C-w>h/j/k/l`: in a pane laying out several windows, the move steps
    /// across them first. The pane provider moves focus the rest of the way,
    /// to the pane that way, if any. It tells the new focus as the user's, and
    /// Neovim's current window follows. Without `focus_direction`, a move
    /// stops at the pane's edge.
    fn on_direction(&mut self, m: windows::Move) -> Result<()> {
        let dir = match m.dir {
            windows::Direction::Left => Direction::Left,
            windows::Direction::Down => Direction::Down,
            windows::Direction::Up => Direction::Up,
            windows::Direction::Right => Direction::Right,
        };
        let count = self.step_in_layout(dir, m.count.max(1))?;
        let p = self.provider.as_deref();
        if count == 0 || !self.asks.may_ask(p, Capability::FocusDirection) {
            return Ok(());
        }
        self.act(Ask::FocusDirection {
            dir,
            count: count.min(MAX_DIRECTION_COUNT),
        });
        Ok(())
    }

    /// Moves Neovim's current window `count` windows `dir` across its pane's
    /// layout, as far as it goes. The moves left at the pane's edge.
    fn step_in_layout(&mut self, dir: Direction, count: u32) -> Result<u32> {
        let win = self.nvim.current_win;
        let Some(p) = self.panes.by_win(win) else {
            return Ok(count);
        };
        let Some(layout) = &p.layout else {
            return Ok(count);
        };
        let area = self.geometry().window_size(p);
        // the cursor's cell in the pane, where Neovim last put it in the
        // window, on a row it shows
        let grid = self.grids.grid_of(win);
        let (shown, _) = self.laid_out_view(p);
        let at = match (shown.iter().find(|s| Some(s.grid) == grid), grid) {
            (Some(s), Some(grid)) => {
                let g = self.grids.get(grid).unwrap();
                let row = pane_view::rect_row(g, s, g.cursor_row);
                [s.col + g.cursor_col.min(s.cols), s.row + row]
            }
            _ => [0, 0],
        };
        let (to, left) = layout.step(win, dir, count, area, at);
        if to != win {
            let id = p.id;
            self.nvim.make_current(id, to)?;
            self.show_in_pane(id, to)?;
            self.nvim.client.flush()?;
            self.draw()?;
        }
        Ok(left)
    }

    /// A float became current: if it's in a floating pane, focus that.
    /// Its group, if drawn in a pane, is drawn in the current pane from now
    /// on; from a window, whether it shrinks the screen is decided.
    fn on_current_float(&mut self, win: i64) -> Result<()> {
        self.nvim.current_float = win;
        if let Some(id) = self.current_pane_id() {
            self.float_homes.became_current(win, id);
        }
        if !self.nvim.shrink.is_shrunk() {
            self.nvim.shrink = Shrink::Deciding(win);
        }
        self.decide_shrink()?;
        if let Some(p) = floating_panes::layer_of_win(&self.grids, &self.panes, win) {
            let terminal = p.terminal.clone();
            self.focus_terminal(terminal.as_ref());
        }
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Decides, once its group is drawn, whether the float that became
    /// current shrinks the screen (ADR 0010): its group has no float group
    /// pane, ready or on its way, and is bigger than the current pane's
    /// room, its width and its rows below its tabline row and above
    /// `'cmdheight'`. Then Neovim's screen takes the current pane's size
    /// until a window is current again (`end_shrink`).
    fn decide_shrink(&mut self) -> Result<()> {
        let Shrink::Deciding(win) = self.nvim.shrink else {
            return Ok(());
        };
        if self.nvim.current_float != win {
            self.nvim.shrink = Shrink::No;
            return Ok(());
        }
        let Some(g) = float_groups::group_of_win(&self.grids, self.nvim.entered_floats(), win)
        else {
            // not drawn yet
            return Ok(());
        };
        self.nvim.shrink = Shrink::No;
        if self
            .panes
            .by_layer_key(g.key)
            .is_some_and(|p| p.layer_hides())
        {
            return Ok(());
        }
        let Some(p) = self.current_pane() else {
            return Ok(());
        };
        let Some(s) = &p.terminal_surface else {
            return Ok(());
        };
        let kept = self.geometry().tabline_rows_of(p) + self.msg_rows;
        let room = [s.view.cols, s.view.rows.saturating_sub(kept)];
        if g.width <= room[0] && g.height <= room[1] {
            return Ok(());
        }
        let id = p.id;
        self.nvim.shrink = Shrink::Shrunk(win);
        self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        Ok(())
    }

    /// A window is current again: a decision on a shrink is dropped, and
    /// Neovim's screen fits the current pane's layout area, growing back if
    /// it was shrunk. The floats of the group it shrank for, if still open,
    /// then run their buffer-local `VimResized` autocommands again, as
    /// pickers listen only there.
    fn end_shrink(&mut self) -> Result<()> {
        let shrink = std::mem::take(&mut self.nvim.shrink);
        if let Some(id) = self.current_pane_id() {
            self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        }
        let Shrink::Shrunk(win) = shrink else {
            return Ok(());
        };
        if let Some(g) = float_groups::group_of_win(&self.grids, self.nvim.entered_floats(), win) {
            let wins = float_groups::wins(&self.grids, &g);
            windows::resized_again(&mut self.nvim.client, &wins);
        }
        Ok(self.nvim.client.flush()?)
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
        self.decide_shrink()?;
        self.nvim.client.flush()?;
        self.draw()
    }

    /// Matches panes to windows: new external windows get panes, ordinary ones
    /// are made external, and panes whose windows closed hang up.
    fn reconcile(&mut self) -> Result<()> {
        // stopping, its panes are gone already, and it gets no more
        if !self.nvim.ready || self.stopping {
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
            let close = self.close_id(&self.panes.list[i]);
            let p = &mut self.panes.list[i];
            let Some(win) = p.neovim_window_handle else {
                continue;
            };
            if self.grids.grid_of(win).is_some() {
                continue;
            }
            // its pane waits for the window that reopens it to be drawn
            if self.changes.awaits_drawing(win) {
                continue;
            }
            p.closing = Some(Close::WindowClosed);
            // a requested terminal is closed, whether or not its helper
            // connected; a self-attached one goes back to its shell (#86 D10)
            if !p.is_self_attached()
                && let Some(t) = close
            {
                self.act(asks::close(t));
            }
            self.panes.remove(i, &self.event_loop);
        }
        if self.lifetime.is_attached() && !self.panes_count_but(None) {
            return self.go_unattached(None);
        }
        self.forget_kept_if_gone();
        if !self.lifetime.is_attached() {
            // unattached: no window gets a pane until an attach comes
            return Ok(self.nvim.client.flush()?);
        }
        self.changes.forget_gone(&self.grids);
        let grids = &self.grids;
        self.pending_sizes
            .retain(|&(w, ..)| grids.grid_of(w).is_some());
        self.home_unshown()?;

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
                    if self.panes.by_win(w.win).is_some()
                        || self.lifetime.kept.is_some_and(|k| k.win == w.win)
                        || self.changes.is_closing(w.win)
                        || self
                            .unshown_awaiting_origin
                            .iter()
                            .any(|&(a, _)| a == w.win)
                    {
                        continue;
                    }
                    self.new_pane(w.win)?;
                }
                WinKind::Float | WinKind::Hidden => {}
            }
        }
        self.size_windows();
        self.reconcile_layers();
        self.decide_shrink()?;
        self.place_tab_pages()?;
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Takes the windows that closed out of pane `i`'s layouts, the one it
    /// shows and those it holds. True if it still shows a window, of another
    /// tab page if none of the one it showed is left; else its last window is
    /// `neovim_window_handle` and it is left to end as a pane of one does.
    fn close_laid_out_windows(&mut self, i: usize) -> bool {
        let opts = pane_layout::Options {
            equalalways: self.split_options.equalalways,
            splitbelow: self.split_options.splitbelow,
            splitright: self.split_options.splitright,
        };
        let gone = |w: i64| self.grids.grid_of(w).is_none() && !self.changes.awaits_drawing(w);
        self.panes.list[i].close_gone(gone, opts)
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
        let p = self.provider.as_deref();
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
        if tab == 0 || self.tabs_in_panes.contains(&tab) {
            return false;
        }
        self.has_area_pane(tab) || self.changes.splits.values().any(|s| s.split.tab == tab)
    }

    /// Whether a window pane of tab page `tab`'s tab page area is there or
    /// on its way, and not closing. Only one whose request failed stays
    /// closing: any other is removed in the call that marks it, and float
    /// group panes don't count.
    fn has_area_pane(&self, tab: i64) -> bool {
        self.panes
            .list
            .iter()
            .any(|p| p.tab == tab && p.layer.is_none() && p.closing.is_none())
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
        p.replace_window(old, new);
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

    /// Asks the pane provider to focus terminal `t`, unless it has or will
    /// once a focus asked lands, or can't.
    fn focus_terminal(&mut self, t: Option<&TerminalId>) {
        let Some(t) = t else {
            return;
        };
        let p = self.provider.as_deref();
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
        let p = self
            .provider
            .as_mut()
            .map(|p| &mut **p as &mut dyn PaneProvider);
        self.asks.act(p, ask, Instant::now());
    }

    /// Matches float group panes to groups of floats (see `floating_panes`).
    fn reconcile_layers(&mut self) {
        let p = self.provider.as_deref();
        let open_float = self.asks.may_ask(p, Capability::OpenFloat);
        let origin = self
            .current_pane()
            .and_then(|f| f.terminal.clone())
            .or_else(|| self.any_terminal(|_| true));
        let shrunk = self.nvim.shrink.is_shrunk();
        for ask in self.floating().reconcile_layers(open_float, origin, shrunk) {
            self.act(ask);
        }
        // their floats move into whichever tab page is current
        let mut groups = Vec::new();
        for p in &self.panes.list {
            if let Some(l) = &p.layer {
                let wins = l.group.top_grids().iter();
                groups.push(
                    wins.filter_map(|&g| self.grids.win_on(g))
                        .map(|w| w.win)
                        .collect(),
                );
            }
        }
        self.nvim.tell_carried(groups);
    }

    fn on_provider(&mut self, token: usize) -> Result<()> {
        if let Some(p) = &mut self.provider {
            p.on_ready(token);
        }
        self.drain_provider()
    }

    /// Handles what the pane provider has to tell, then what became of the
    /// acts.
    pub(crate) fn drain_provider(&mut self) -> Result<()> {
        while let Some(e) = self.provider.as_mut().and_then(|p| p.next()) {
            match e {
                ProviderEvent::Answered { id, outcome } => {
                    if let Some(p) = &mut self.provider {
                        self.asks.answered(p.as_mut(), id, outcome, Instant::now());
                    }
                }
                ProviderEvent::Layout(l) => self.on_live_layout(l)?,
                ProviderEvent::LayoutChanged => self.ask_for_report(),
                ProviderEvent::Focused(t) => self.on_focused(&t, Instant::now())?,
                // its pane hangs up by itself; ending a requested pane's
                // wait on it is for later
                ProviderEvent::Closed(t) => log_info!("{NAME} daemon: {t} closed"),
                ProviderEvent::Gone { reason } => self.provider_gone(&reason)?,
            }
        }
        self.run_steps()?;
        self.watch_provider_writes()?;
        self.arm_ask_timer()
    }

    /// The pane provider failed as a whole (`Gone`), for `reason`: it's
    /// dropped at once, with one warning, and every act asked of it or
    /// waiting falls back (#92 decision 27).
    fn provider_gone(&mut self, reason: &str) -> Result<()> {
        let line = format!("{}: {reason}; no pane provider", self.provider_name());
        log_warn!("{NAME} daemon: {line}");
        self.warn_when_ready(format!("{NAME}: {line}"));
        self.nvim.client.flush()?;
        self.drop_provider()
    }

    /// Drops the pane provider once no terminal in its reach is attached or
    /// on its way, and nothing asked of it is unanswered, as when the
    /// daemon goes unattached: the next self-attach chooses again.
    fn drop_provider_out_of_reach(&mut self) -> Result<()> {
        let ending = self.stopped.is_some();
        if self.provider.is_none() || self.choosing.is_some() || self.asks.busy() || ending {
            return Ok(());
        }
        if self.reaches(None) {
            return Ok(());
        }
        log_info!(
            "{NAME} daemon: dropping {}: no terminal in its reach",
            self.provider_name()
        );
        self.drop_provider()?;
        self.run_steps()
    }

    /// Drops the pane provider, ending what it runs, such as its connection
    /// or its `events`. Every act asked of it or
    /// waiting falls back, and what it was found not to support is
    /// forgotten. Its terminal ids and live layout go: a pane it hadn't
    /// reported is its own area, and the rest stay as they are. Every pane
    /// is out of reach until the next choice checks it again.
    fn drop_provider(&mut self) -> Result<()> {
        if let Some(fd) = self.provider.as_ref().and_then(|p| p.fd()) {
            let _ = self.event_loop.remove(fd);
        }
        self.provider = None;
        self.no_provider = control::NoProvider::None;
        self.provider_writes = false;
        self.context.clear();
        // dropped before it was confirmed: none was chosen after all
        if let Some(c) = self.choosing.take() {
            log_info!(
                "{NAME} daemon: pane {} chose no pane provider after all",
                c.id
            );
        }
        for p in &mut self.panes.list {
            p.terminal = None;
            p.close_terminal = None;
        }
        self.out_of_reach_but(None);
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
            self.asks.time_out(p.as_mut(), Instant::now());
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
        // what an act of its own that ran did, or may have done if it
        // failed, is read back; a report asked once a layout act ended, as
        // they go one at a time, sees it already. One that timed out is
        // read back once its command is gone, as the command runner tells.
        for cap in self.asks.take_ran() {
            if cap != Capability::ReportLayout
                && !(cap.is_layout() && self.asks.asked(Capability::ReportLayout))
            {
                self.read_layout_again();
            }
        }
        while let Some(step) = self.asks.next_step() {
            if self.asks.settles_placement(&step) {
                log::trace(Point::Placed, 0);
            }
            match step {
                Step::Done(ask @ Ask::Find { .. }, answer) => self.found(&ask, answer)?,
                Step::NotFound(ask, why) => self.not_found(&ask, &why)?,
                Step::FallBack(ask @ Ask::Find { .. }) => self.find_fell_back(&ask)?,
                Step::Done(ask, Answer::Terminal(t)) => self.on_opened(ask, t)?,
                Step::Done(ask, Answer::Nothing) if opened_pane(&ask).is_some() => {
                    self.opened_without_id(&ask)?
                }
                Step::Done(Ask::ReportLayout, Answer::Layout(l)) => self.on_reported(l)?,
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

    /// Pane `id`, opened by `open`, is placed beside its origin once it has
    /// its terminal id and its terminal has attached, whichever comes last.
    pub(crate) fn place_opened(&mut self, id: u32) {
        let Some(p) = self.panes.by_id_mut(id) else {
            return;
        };
        if p.terminal_surface.is_none() {
            return;
        }
        let Some(t) = p.terminal.clone() else {
            return;
        };
        if let Some(placing) = p.placing.take() {
            self.act(placing.ask(t));
        }
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
        let wait = self.attach_timeout();
        let pair = self.is_pair();
        let p = self.panes.by_id_mut(pane_id).unwrap();
        p.open_answered = true;
        let handle = p.neovim_window_handle;
        let attaching = p.terminal_surface.is_none();
        if attaching {
            p.attach_due = Some(Instant::now() + wait);
        }
        if pair {
            // its id serves only `close`: the layout provider's `find`, as
            // its terminal attaches, gives the one that places it
            p.close_terminal = Some(t);
            if attaching {
                self.arm_attach_timer()?;
            }
            return self.place_tab_pages();
        }
        p.terminal = Some(t.clone());
        self.place_opened(pane_id);
        if attaching {
            self.arm_attach_timer()?;
        }
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
            Ask::Open { .. } | Ask::OpenBeside { .. } => {
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

    /// The pane provider opened the terminal `ask` asked for, but printed
    /// no id for it: a whole table's `find` is asked for one once it has
    /// attached too.
    fn opened_without_id(&mut self, ask: &Ask) -> Result<()> {
        let Some(attach_id) = opened_pane(ask) else {
            return Ok(());
        };
        let Some(p) = self.panes.by_attach_id_mut(&attach_id) else {
            return Ok(());
        };
        p.open_answered = true;
        let (id, attached) = (p.id, p.terminal_surface.is_some());
        if attached && !self.is_pair() {
            self.find_requested(id)?;
        }
        Ok(())
    }

    /// Whether the pane provider is a pair (ADR 0006).
    pub(super) fn is_pair(&self) -> bool {
        self.provider.as_ref().is_some_and(|p| p.is_pair())
    }

    /// The id `close` takes for pane `p`'s terminal: in a pair, the one
    /// the terminal provider's open printed, if any; else its terminal's.
    pub(super) fn close_id(&self, p: &Pane) -> Option<TerminalId> {
        if self.is_pair() {
            p.close_terminal.clone()
        } else {
            p.terminal.clone()
        }
    }

    /// An act that couldn't be done. A pane whose open failed is given up.
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
        self.give_up(pane_id)
    }

    /// Gives up requested pane `id`, whose terminal won't come: its id
    /// expires, and a terminal it has is closed. The pane of a window of the
    /// first tab page goes, and its window is drawn in its origin pane, or
    /// once a pane can take it (#112). So does the pane `open_tab_page` asked
    /// for, and its tab page is drawn whole in a pane for its life (ADR
    /// 0012). Any other pane stays given up: a float group pane's floats are
    /// drawn in the pane, and a tab page area's other windows ask for their
    /// own.
    pub(super) fn give_up(&mut self, id: u32) -> Result<()> {
        let terminal = self.close_id(self.panes.by_id(id).unwrap());
        let p = self.panes.by_id_mut(id).unwrap();
        p.closing = Some(Close::RequestFailed);
        p.attach_due = None;
        let attach_id = p.attach_id;
        if let Some(a) = attach_id {
            self.panes.ids.expire(&a);
        }
        if let Some(t) = terminal {
            self.act(asks::close(t));
        }
        // a float group pane that won't come while one of its floats is
        // current: its group, now drawn in a pane, may shrink the screen
        let float = self.nvim.current_float;
        if let Some(l) = self.panes.by_id(id).unwrap().layer
            && float != 0
            && !self.nvim.shrink.is_shrunk()
            && float_groups::has_win(&self.grids, &l.group, float)
        {
            self.nvim.shrink = Shrink::Deciding(float);
            self.decide_shrink()?;
        }
        let p = self.panes.by_id(id).unwrap();
        // given up as closing, it's no pane of its tab page area
        if p.layer.is_none()
            && p.tab != 0
            && !self.has_area_pane(p.tab)
            && !self.tabs_in_panes.contains(&p.tab)
        {
            self.tabs_in_panes.push(p.tab);
        }
        let alone = p.layer.is_none()
            && (p.tab == 0 || self.tabs_in_panes.contains(&p.tab))
            && p.layout.is_none();
        if let Some(win) = p.neovim_window_handle.filter(|_| alone) {
            let sp = p.split.unwrap_or_else(|| self.changes.split_of(win));
            let i = self.panes.index_of(id).unwrap();
            self.panes.remove(i, &self.event_loop);
            if self.lay_out_in_origin(win, sp, None) {
                self.show_current_in_origin(win)?;
            } else {
                // no pane can take it yet: unshown, it waits for one, unasked
                self.unshown_awaiting_origin.push((win, sp));
            }
        }
        self.reconcile()?;
        self.draw()
    }

    /// Draws each unshown window waiting for a pane to join in one that can
    /// take it now, and forgets those that closed.
    fn home_unshown(&mut self) -> Result<()> {
        let grids = &self.grids;
        self.unshown_awaiting_origin
            .retain(|&(w, _)| grids.grid_of(w).is_some());
        for (win, sp) in std::mem::take(&mut self.unshown_awaiting_origin) {
            if self.lay_out_in_origin(win, sp, None) {
                self.show_current_in_origin(win)?;
            } else {
                self.unshown_awaiting_origin.push((win, sp));
            }
        }
        Ok(())
    }

    /// Window `win` was just drawn in its origin pane: if it's current, Neovim's
    /// screen fits that pane, and the pane provider focuses its terminal.
    fn show_current_in_origin(&mut self, win: i64) -> Result<()> {
        if !self.is_current_win(win) {
            return Ok(());
        }
        let Some(p) = self.panes.by_win(win) else {
            return Ok(());
        };
        let (id, terminal) = (p.id, p.terminal.clone());
        self.show_in_pane(id, win)?;
        self.focus_terminal(terminal.as_ref());
        Ok(())
    }

    /// Window `win`, current, is the one pane `id` shows: of several there,
    /// it keeps a row and isn't cut; of another tab page's, held there, the
    /// pane shows that tab page now. Neovim's screen fits the pane.
    fn show_in_pane(&mut self, id: u32, win: i64) -> Result<()> {
        let p = self.panes.by_id_mut(id).unwrap();
        let swapped = p.show_tab_of(win);
        p.neovim_window_handle = Some(win);
        // asks only what changed
        if swapped || p.layout.is_some() {
            self.size_window(id);
        }
        self.nvim.fit_ui_to(self.panes.by_id(id).unwrap())?;
        Ok(())
    }

    /// How long a terminal just opened has to attach.
    fn attach_timeout(&self) -> Duration {
        let wait = self
            .provider
            .as_ref()
            .map_or(super::provider::default_attach_timeout(), |p| {
                p.attach_timeout()
            });
        wait.min(crate::config::check::MAX_TIMEOUT)
    }

    /// The attach timer fired, at `now`: a requested pane whose terminal is
    /// open and hasn't attached in time is given up, with a warning. Its id
    /// expires, so a helper that attaches later is refused `pane-expired`.
    pub(super) fn time_out_attaches(&mut self, now: Instant) -> Result<()> {
        self.attach_timer_due = None;
        let late: Vec<u32> = self
            .panes
            .list
            .iter()
            .filter(|p| p.terminal_surface.is_none() && p.attach_due.is_some_and(|d| d <= now))
            .map(|p| p.id)
            .collect();
        for id in late {
            let Some(p) = self.panes.by_id(id) else {
                continue;
            };
            let t = p.terminal.as_ref().map_or("?", TerminalId::as_str);
            let line = format!("terminal {t} didn't attach in time");
            log_warn!("{NAME} daemon: {line}");
            self.warn_in_nvim(&format!("{NAME}: {line}"));
            self.give_up(id)?;
        }
        self.nvim.client.flush()?;
        self.arm_attach_timer()
    }

    /// Starts the attach timer for the soonest wait for an attach, unless it
    /// already runs for one that soon.
    fn arm_attach_timer(&mut self) -> Result<()> {
        let Some(due) = self.panes.list.iter().filter_map(|p| p.attach_due).min() else {
            return Ok(());
        };
        if self.attach_timer_due.is_some_and(|armed| armed <= due) {
            return Ok(());
        }
        // round up, so the timer doesn't fire before the deadline
        let ms = due
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        self.attach_timer_due = Some(due);
        self.add_timer(Token::AttachTimer, u32::try_from(ms).unwrap_or(u32::MAX))
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

    /// The user went into float group pane `id`, focusing it or typing or
    /// clicking there: into one of its floats, if not there already, the
    /// one that takes focus (#89 decision 15).
    fn enter_float_group_pane(&mut self, id: u32) -> Result<()> {
        let Some(l) = self.panes.by_id(id).and_then(|p| p.layer) else {
            return Ok(());
        };
        if self.nvim.current_float != 0
            && floating_panes::layer_of_win(&self.grids, &self.panes, self.nvim.current_float)
                .map(|q| q.id)
                == Some(id)
        {
            return Ok(());
        }
        // hidden in another tab page: Neovim goes there, to the float
        // in it that takes focus
        let hidden = float_groups::hidden(&self.grids, &l.group);
        let top = l
            .group
            .top_grids()
            .first()
            .and_then(|&t| self.grids.win_on(t));
        let found = match top {
            Some(w) if hidden => Some(w.win),
            _ => floating_panes::focusable_in(&self.grids, &l.group),
        };
        let Some(win) = found else {
            return Ok(());
        };
        self.nvim.current_float = win;
        self.nvim.switch_to_float(id, win)?;
        Ok(())
    }

    /// The user focused one of our panes's terminals: make its window current.
    fn on_provider_focus(&mut self, t: &TerminalId) -> Result<()> {
        let Some(p) = self.panes.by_terminal(t.as_str()) else {
            return Ok(());
        };
        let id = p.id;
        if p.layer.is_some() {
            return self.enter_float_group_pane(id);
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
        let p = self.provider.as_deref();
        self.asks.may_ask(p, Capability::ReportLayout)
    }

    /// Whether the pane provider has `events`: it tells focus moves, and
    /// its live layouts or when they changed, unasked.
    fn has_events(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|p| p.capabilities().has(Capability::Events))
    }

    /// Something may have moved the terminals or focus: a pane provider
    /// that reports layouts but doesn't tell them is asked for one, unless
    /// one waits its turn already, which will see it (#86 decision D5).
    /// Never on a timer.
    fn read_layout_again(&mut self) {
        if self.reads_after_changes() {
            self.ask_for_report();
        }
    }

    /// Asks for a report through the layout queue, unless one waits its
    /// turn already: as `events` says, or for `read_layout_again`.
    fn ask_for_report(&mut self) {
        if self.reports_layouts() && !self.asks.waits(Capability::ReportLayout) {
            self.act(Ask::ReportLayout);
        }
    }

    /// Whether the pane provider reports layouts but doesn't tell them: it is
    /// asked again after what may move terminals or focus.
    fn reads_after_changes(&self) -> bool {
        self.reports_layouts() && !self.has_events()
    }

    /// A report came: the live layout, and, from a pane provider that
    /// doesn't tell focus unasked, where focus is.
    fn on_reported(&mut self, l: LiveLayout) -> Result<()> {
        let current = l.current.clone().filter(|_| !self.has_events());
        self.on_live_layout(l)?;
        match current {
            Some(t) => self.on_focused(&t, Instant::now()),
            None => Ok(()),
        }
    }

    /// Pane `id` may have just got the last of its terminal and its terminal
    /// id: an `Unreported` one then waits a second for the live layout to
    /// report it. (What got it there, its attach or its open, has the
    /// layout read again.) With no live layout to come, or no id to be found
    /// by, it is its own layout area at once.
    fn wait_for_report(&mut self, id: u32, now: Instant) -> Result<()> {
        let reports = self.reports_layouts();
        let Some(p) = self.panes.by_id_mut(id) else {
            return Ok(());
        };
        if p.area != Area::Unreported || p.terminal_surface.is_none() || p.report_due.is_some() {
            return Ok(());
        }
        // a requested pane's id is on its way
        if p.terminal.is_none() && !p.is_self_attached() && reports {
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
        self.arm_area_timer()
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
            homes: &mut self.float_homes,
        }
    }

    // shared

    /// Draws every pane that changed.
    fn draw(&mut self) -> Result<()> {
        self.forget_unneeded_statuslines();
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
        // float groups with no float group pane: each in its own pane, and
        // while the screen shrinks for one, in none until Neovim has redrawn
        // at that size
        let homed = self.float_homes.in_panes(
            &self.grids,
            &self.panes,
            self.nvim.entered_floats(),
            focused,
        );
        let shrunk = self.nvim.shrink.is_shrunk();
        let redrawing = shrunk
            && self
                .grids
                .get(1)
                .is_some_and(|g| [g.width, g.height] != self.nvim.editor_size);
        let mut pane_skip: Vec<u32> = Vec::with_capacity(SKIP_MAX);
        let mut own: Vec<u32> = Vec::new();
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
            pane_skip.clear();
            pane_skip.extend_from_slice(&skip);
            own.clear();
            for (home, g) in &homed {
                if *home == id && !redrawing {
                    own.extend_from_slice(g.top_grids());
                } else {
                    pane_skip.extend_from_slice(g.top_grids());
                }
            }
            // a pane that closes in this loop leaves the panes drawn before it, to its right, on their old columns for this frame: the next draw finds their start moved and redraws the row
            let tabline = self.prepare_tabline(id);
            let p = &self.panes.list[i];
            // a floating pane waiting on its refit shows nothing yet, and one
            // whose floats hide in another tab page keeps its last frame
            if let Some(l) = &p.layer
                && (!p.layer_shown() || float_groups::hidden(&self.grids, &l.group))
            {
                continue;
            }
            // frames were dropped for a terminal that wasn't reading: no frame
            // until it has caught up, when it's drawn in full; where its input
            // goes is kept current all the same
            let caught_up = !p.terminal_surface.as_ref().unwrap().backlog.dropped();
            // its top left on grid 1; shrunk, the screen is the current pane
            let origin = if shrunk && focused == Some(id) {
                [0, 0]
            } else {
                self.geometry().pane_origin(p)
            };
            let drawn = if let Some(l) = p.layer {
                let members = float_groups::members(&self.grids, &l.group);
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                // the group's own screen float, even when the cap on members dropped it
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
                        skip: &pane_skip,
                        own: &own,
                        tabline: tabline.cells.as_deref(),
                        tabline_col: tabline.col,
                        tabline_dirty,
                    },
                    &mut s.writer,
                )
            } else if caught_up {
                let grid = geometry::grid_of_pane(&self.grids, p);
                let s: &mut Surface = self.panes.list[i].terminal_surface.as_deref_mut().unwrap();
                s.view.frame(
                    &mut self.grids,
                    grid,
                    Opts {
                        focused: focused == Some(id),
                        origin,
                        skip: &pane_skip,
                        own: &own,
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
            set_windows(&mut s.forwarder, &self.grids, s.view.drawn_windows());
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
        let (laid, separators) = self.laid_out_wins(p);
        let wins = laid
            .iter()
            .filter_map(|&(w, ours)| {
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
                    statusline: ours.then_some(hl),
                })
            })
            .collect();
        (wins, separators)
    }

    /// The windows of pane `p`'s layout, laid out in its window area, each
    /// with whether neopane draws its statusline (`draws_statusline`), and
    /// the separators between.
    fn laid_out_wins(&self, p: &Pane) -> (Vec<(pane_layout::Placed, bool)>, Vec<Separator>) {
        let Some(layout) = &p.layout else {
            return (Vec::new(), Vec::new());
        };
        let area = self.geometry().window_size(p);
        let laid = layout.lay_out(area, p.neovim_window_handle.unwrap_or(0));
        let wins = laid
            .wins
            .iter()
            .map(|w| (*w, self.draws_statusline(w, area)))
            .collect();
        (wins, laid.separators)
    }

    /// Whether neopane draws window `w`, laid out in `area`, its statusline: a
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
        // the screen fits the pane, growing back from a shrink
        self.end_shrink()?;
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

    /// The pane showing Neovim's current window; until Neovim says which
    /// that is, the first self-attached pane, which takes the first window.
    fn current_pane(&self) -> Option<&Pane> {
        if self.nvim.current_win == 0 {
            return self.panes.first_self_attached();
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
    if p.closing.is_some() || p.is_self_attached() {
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
            grid_row: 0,
        })
        .collect();
    f.set_targets(&targets);
}

/// Mouse events on the windows of a pane layout drawn go to the rows of
/// their grids they show there, a statusline neopane draws no grid's.
fn set_windows(f: &mut Forwarder, grids: &Grids, shown: &[Shown]) {
    f.set_windows(shown.iter().flat_map(|s| {
        let runs = grids
            .get(s.grid)
            .into_iter()
            .flat_map(|g| pane_view::runs(g, s));
        runs.map(|r| forwarder::Target {
            grid: s.grid,
            row: (s.row + r.row) as i32,
            col: s.col as i32,
            width: s.cols,
            height: r.rows,
            grid_row: r.grid_row,
        })
    }));
}

#[cfg(test)]
mod self_attach_tests;
#[cfg(test)]
mod slow_terminal_tests;
#[cfg(test)]
mod tab_area_tests;
#[cfg(test)]
mod unattached_tests;

#[cfg(test)]
mod tests {
    use super::super::live_layout::{LayoutArea, PlacedTerminal};
    use super::super::provider::fake::FakeProvider;
    use super::super::provider::{CapabilitySet, Outcome};
    use super::*;
    use crate::nvim::nvim::for_test_on;
    use crate::nvim::windows::{Kind, Node};
    use crate::screen::grid::Grid;
    use crate::screen::grids::{Float, Win};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    mod choosing;
    mod config;
    mod control;
    mod fallback;
    mod floats_in_panes;
    mod in_layout;
    mod statuslines;
    mod tab_layouts;
    mod tabs;
    mod unattached;

    /// A daemon with no Neovim behind it, only a socket that nothing
    /// answers, and a fake pane provider that can open and close.
    struct Fixture {
        d: Daemon,
        nvim_end: UnixStream,
    }

    fn fixture() -> Fixture {
        with_provider(Some(Box::new(FakeProvider::opening())))
    }

    fn with_provider(provider: Option<Box<dyn PaneProvider>>) -> Fixture {
        let (nvim_ours, nvim_end) = UnixStream::pair().unwrap();
        let d = Daemon::new(
            Loop::init().unwrap(),
            NvimSession::new(for_test_on(nvim_ours)),
            provider,
            std::path::PathBuf::new(),
            crate::daemon::dir::Address::from_arg(Default::default()),
        );
        Fixture { d, nvim_end }
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
        assert_given_up(&fx.d, id);
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
        assert_given_up(&fx.d, id);
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
            assert_given_up(&fx.d, id);
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

    /// A self-attach's hello from a terminal on `tty` whose `$<NAME>` is
    /// `address`, unset if empty.
    fn self_hello_from(address: &[u8], tty: &[u8]) -> Vec<u8> {
        use crate::daemon::attach_conn::{self_hello, test_terminal};
        let mut t = test_terminal(80, 24);
        if !address.is_empty() {
            t.env = vec![(
                crate::daemon::find::inside_var().into_bytes(),
                address.to_vec(),
            )];
        }
        t.tty = tty.to_vec();
        self_hello(t)
    }

    /// Sends `hello` to `fx`'s daemon, a pty handed over: the helper's end,
    /// and the pty, kept open while the answer waits. (On Linux, a pty whose
    /// master has closed is no terminal.)
    fn self_attach_with(fx: &mut Fixture, tag: &str, hello: &[u8]) -> (UnixStream, Pty) {
        // an address of its own: the tests run side by side
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let at = Address::new(&format!("{tag}-{n}"));
        let pty = Pty::new(80, 24);
        let helper = at.send(hello, Some(pty.slave));
        fx.d.accept(&at.server).unwrap();
        (helper, pty)
    }

    /// A fixture for daemon `proj` at `/tmp/x/proj/sock`, sent a
    /// self-attach from a terminal on `tty` with `$<NAME>` that address: the
    /// helper's end, and the pty.
    fn maybe_nested(tty: &[u8]) -> (Fixture, (UnixStream, Pty)) {
        let mut fx = fixture();
        fx.d.name = "proj".into();
        fx.d.sock = crate::daemon::dir::Address::from_arg("/tmp/x/proj/sock".into());
        let hello = self_hello_from(b"/tmp/x/proj/sock", tty);
        let attached = self_attach_with(&mut fx, "nested", &hello);
        (fx, attached)
    }

    /// Neovim's answer to the channel list the nested check asked for: one
    /// channel on each of `ptys`.
    fn chans_answer(fx: &mut Fixture, ptys: &[&str]) {
        let req = fx.d.nested_checks[0].req;
        let mut buf = Vec::new();
        msgpack::write_array_header(&mut buf, ptys.len() as u32);
        for pty in ptys {
            msgpack::write_map_header(&mut buf, 1);
            msgpack::write_str(&mut buf, "pty");
            msgpack::write_str(&mut buf, pty);
        }
        assert!(fx.d.on_nested_answer(req, None, Decoder::of(&buf)).unwrap());
    }

    fn assert_nested(fx: &Fixture, helper: &UnixStream) {
        let z = refusal(helper);
        assert_eq!(Some(crate::attach::Refusal::Nested), z.as_refusal());
        assert_eq!(
            "a terminal in proj's own Neovim can't attach to it",
            z.message
        );
        assert!(fx.d.panes.list.is_empty());
    }

    #[test]
    fn a_self_attach_from_one_of_the_daemons_own_terminals_is_refused_as_nested() {
        // relayed with no tty: at once
        let (fx, (helper, _pty)) = maybe_nested(b"");
        assert!(fx.d.nested_checks.is_empty());
        assert_nested(&fx, &helper);
        // on one of Neovim's terminals, once Neovim lists them
        let (mut fx, (helper, _pty)) = maybe_nested(b"/dev/ttys004");
        assert!(sent_to_nvim(&mut fx).contains("nvim_list_chans"));
        chans_answer(&mut fx, &["/dev/ttys001", "/dev/ttys004"]);
        assert_nested(&fx, &helper);
    }

    #[test]
    fn a_self_attach_that_only_inherited_the_daemons_address_is_accepted() {
        // a tmux started from a :terminal
        let (mut fx, (helper, _pty)) = maybe_nested(b"/dev/ttys009");
        chans_answer(&mut fx, &["/dev/ttys004"]);
        assert!(matches!(answer(&helper), crate::attach::Message::Accept(_)));
        let p = &fx.d.panes.list[0];
        assert!(p.is_self_attached() && p.attach_id.is_some());
        // another daemon's terminal: no question asked
        let mut fx = fixture();
        fx.d.sock = crate::daemon::dir::Address::from_arg("/tmp/x/proj/sock".into());
        let hello = self_hello_from(b"/tmp/x/other/sock", b"/dev/ttys004");
        let (helper, _pty) = self_attach_with(&mut fx, "other", &hello);
        assert!(matches!(answer(&helper), crate::attach::Message::Accept(_)));
    }

    #[test]
    fn a_self_attach_neovim_doesnt_list_terminals_for_in_time_is_refused_as_nested() {
        let (mut fx, (helper, _pty)) = maybe_nested(b"/dev/ttys009");
        fx.d.time_out_hellos(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert!(fx.d.nested_checks.is_empty());
        assert_nested(&fx, &helper);
    }

    /// A daemon set up, its first self-attached pane showing window 1000,
    /// and a second self-attached pane waiting for a window: its id.
    fn second_self_attach(provider: Option<Box<dyn PaneProvider>>) -> (Fixture, u32) {
        let mut fx = with_provider(provider);
        draw_external(&mut fx.d, 2, 1000, 80, 23);
        let first = fx.d.panes.add_self_attached(SelfAttach {
            surface: Surface::for_test(80, 24),
            terminal: None,
            hello: None,
        });
        first.neovim_window_handle = Some(1000);
        first.area = Area::Own;
        fx.d.nvim.ready = true;
        fx.d.nvim.current_win = 1000;
        let second = fx.d.panes.add_self_attached(SelfAttach {
            surface: Surface::for_test(80, 24),
            terminal: None,
            hello: None,
        });
        second.area = Area::Own;
        let id = second.id;
        (fx, id)
    }

    /// Neovim's answer to the window list: `current`, then `wins`.
    fn list_answer(fx: &mut Fixture, current: i64, wins: &[i64]) {
        let ask = fx.d.window_ask.expect("a window list asked");
        assert_eq!(self_attach::AskKind::List, ask.kind);
        let mut buf = Vec::new();
        msgpack::write_array_header(&mut buf, wins.len() as u32 + 1);
        msgpack::write_int(&mut buf, current);
        for &w in wins {
            msgpack::write_int(&mut buf, w);
        }
        assert!(
            fx.d.on_window_answer(ask.req, None, Decoder::of(&buf))
                .unwrap()
        );
    }

    #[test]
    fn a_window_being_shown_gets_no_other_pane_from_a_flush_before_the_answer() {
        let (mut fx, id) = second_self_attach(None);
        fx.d.give_windows().unwrap();
        list_answer(&mut fx, 1000, &[1000, 1001]);
        assert_eq!(
            Some(self_attach::AskKind::Show(1001)),
            fx.d.window_ask.map(|a| a.kind)
        );
        // a flush draws it before Neovim answers
        draw_external(&mut fx.d, 3, 1001, 80, 23);
        fx.d.reconcile().unwrap();
        assert_eq!(2, fx.d.panes.list.len());
        assert_eq!(Some(id), fx.d.panes.by_win(1001).map(|p| p.id));
    }

    #[test]
    fn a_window_being_opened_gets_no_other_pane_from_a_flush_before_the_answer() {
        let (mut fx, id) = second_self_attach(None);
        fx.d.give_windows().unwrap();
        // nothing unshown: a new window
        list_answer(&mut fx, 1000, &[1000]);
        assert_eq!(
            Some(self_attach::AskKind::Open),
            fx.d.window_ask.map(|a| a.kind)
        );
        // the Lua notes it as it makes it, and a flush draws it
        fx.d.on_note(windows::Note::Opened(1002)).unwrap();
        draw_external(&mut fx.d, 3, 1002, 80, 23);
        fx.d.reconcile().unwrap();
        assert_eq!(2, fx.d.panes.list.len());
        assert_eq!(Some(id), fx.d.panes.by_win(1002).map(|p| p.id));
    }

    /// The ask serving the second pane of `second_self_attach` for a new
    /// window, the Lua having noted window 1002 as it made it.
    fn opening_1002(provider: Option<Box<dyn PaneProvider>>) -> (Fixture, u32, u32) {
        let (mut fx, id) = second_self_attach(provider);
        fx.d.give_windows().unwrap();
        list_answer(&mut fx, 1000, &[1000]);
        let req = fx.d.window_ask.unwrap().req;
        fx.d.on_note(windows::Note::Opened(1002)).unwrap();
        (fx, id, req)
    }

    #[test]
    fn a_new_window_that_fails_ends_its_attach_and_closes_the_window() {
        let (mut fx, id, req) = opening_1002(None);
        let _ = sent_to_nvim(&mut fx);
        let failed =
            fx.d.on_window_answer(req, Some(b"E1: no"), Decoder::of(&[0xc0]));
        assert!(failed.unwrap());
        assert!(fx.d.panes.by_id(id).is_none());
        assert!(fx.d.changes.is_closing(1002));
        assert!(sent_to_nvim(&mut fx).contains(".close("));
        assert_eq!(None, fx.d.stopped);
        // a flush showing it gives it no pane
        draw_external(&mut fx.d, 3, 1002, 80, 23);
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.by_win(1002).is_none());
    }

    #[test]
    fn the_last_pane_failing_to_get_a_window_goes_unattached() {
        let (mut fx, id, req) = opening_1002(None);
        let first = fx.d.panes.list[0].id;
        let i = fx.d.panes.index_of(first).unwrap();
        fx.d.panes.remove(i, &fx.d.event_loop);
        fx.d.on_window_answer(req, Some(b"E1: no"), Decoder::of(&[0xc0]))
            .unwrap();
        assert!(fx.d.panes.by_id(id).is_none());
        assert!(deciding(&fx.d));
    }

    #[test]
    fn a_new_window_whose_pane_went_while_it_was_asked_closes_without_a_pane() {
        let (mut fx, id) = second_self_attach(None);
        fx.d.give_windows().unwrap();
        list_answer(&mut fx, 1000, &[1000]);
        let i = fx.d.panes.index_of(id).unwrap();
        fx.d.panes.remove(i, &fx.d.event_loop);
        let _ = sent_to_nvim(&mut fx);
        fx.d.on_note(windows::Note::Opened(1002)).unwrap();
        assert!(fx.d.changes.is_closing(1002));
        assert!(sent_to_nvim(&mut fx).contains(".close("));
        draw_external(&mut fx.d, 3, 1002, 80, 23);
        fx.d.reconcile().unwrap();
        assert_eq!(1, fx.d.panes.list.len());
        assert!(fx.d.panes.by_win(1002).is_none());
    }

    #[test]
    fn a_window_waiting_for_a_pane_without_one_isnt_taken() {
        let (mut fx, id) = second_self_attach(Some(Box::new(FakeProvider::opening())));
        // 1001 waits for its tab page's floating pane, or an origin; 1002
        // for a pane that can draw it
        fx.d.changes.note_split(windows::Split {
            win: 1001,
            side_by_side: true,
            before: false,
            from: 1000,
            tab: 5,
        });
        let sp = fx.d.changes.split_of(1002);
        fx.d.unshown_awaiting_origin.push((1002, sp));
        fx.d.give_windows().unwrap();
        list_answer(&mut fx, 1001, &[1000, 1001, 1002, 1003]);
        assert_eq!(
            Some(self_attach::AskKind::Show(1003)),
            fx.d.window_ask.map(|a| a.kind)
        );
        assert_eq!(Some(id), fx.d.panes.by_win(1003).map(|p| p.id));
    }

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
            TablineFixture::on(with_provider(Some(Box::new(FakeProvider::telling()))))
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
            inset: [0; 4],
        }
    }

    /// Area `id` of a live layout, at `rect` in it, floating.
    fn floating_in(id: &str, rect: [f64; 4]) -> Area {
        Area::Live {
            id: id.into(),
            rect,
            floating: true,
            inset: [0; 4],
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
    fn framed_tiled_panes_count_their_frames_in_their_areas_size() {
        let mut fx = TablineFixture::init();
        fx.d().nvim.ready = true;
        // side by side in a 200 x 21 area, framed as zellij frames them:
        // a has its top row and right column framed, b its top row
        let a = fx.add(0, 99, Area::Unreported);
        let b = fx.add(1, 100, Area::Unreported);
        set_block(fx.d(), a, 1);
        set_block(fx.d(), b, 2);
        fx.d().nvim.current_win = 1000;
        let framed = |n: u32, rect: [f64; 4], inset: [u16; 4]| PlacedTerminal {
            terminal: TerminalId::from(block_id(n).as_str()),
            rect,
            floating: false,
            inset,
        };
        let l = LiveLayout {
            areas: vec![LayoutArea {
                id: "w".into(),
                terminals: vec![
                    framed(1, [0.0, 0.0, 0.5, 1.0], [0, 1, 1, 0]),
                    framed(2, [0.5, 0.0, 0.5, 1.0], [0, 1, 0, 0]),
                ],
            }],
            current: None,
        };
        fake(fx.d()).push(ProviderEvent::Layout(l));
        fx.d().drain_provider().unwrap();
        fx.d().after_batch().unwrap();
        // Neovim's screen is exactly the area, from either pane
        assert_eq!([200, 21], fx.d().nvim.editor_size);
        fx.d().on_current(1001).unwrap();
        fx.d().after_batch().unwrap();
        assert_eq!([200, 21], fx.d().nvim.editor_size);
        // framed along the top, both are still tabline panes
        let d = fx.d();
        assert!(geometry::is_tabline_pane(d.panes.by_id(a).unwrap()));
        assert!(geometry::is_tabline_pane(d.panes.by_id(b).unwrap()));
    }

    #[test]
    fn a_neovim_gone_before_its_last_message_is_answered_ends_the_loop_as_it_exiting() {
        // what `:q` can do: Neovim's last message, here a request, comes
        // with its stdin already closed, so the answer can't be written
        let mut fx = fixture();
        let address = Address::new("nvim-gone");
        let fd = fx.d.nvim.client.read_fd();
        fx.d.event_loop
            .add_read(fd, Token::NvimChannel as usize)
            .unwrap();
        // [0, 1, "x", []]
        fx.nvim_end
            .write_all(&[0x94, 0x00, 0x01, 0xa1, b'x', 0x90])
            .unwrap();
        drop(fx.nvim_end);
        assert_eq!(StopReason::NvimExited, fx.d.run(&address.server).unwrap());
        assert!(fx.d.nvim.client.is_closed());
    }

    #[test]
    fn a_self_attached_pane_is_its_own_area_until_a_live_layout_places_it() {
        let mut fx = TablineFixture::init();
        let s = fx.surface(80, 24);
        fx.d()
            .start(&mut Some(SelfAttach {
                surface: s,
                terminal: Some(TerminalId::from("t0")),
                hello: None,
            }))
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
        let mut fx = with_provider(Some(Box::new(FakeProvider::telling())));
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
        fx.d()
            .start(&mut Some(SelfAttach {
                surface: s,
                terminal: None,
                hello: None,
            }))
            .unwrap();
        fx.d().nvim.ready = true;
        let other = fx.add(2, 60, Area::Unreported);
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
        let mut fx = with_provider(Some(Box::new(FakeProvider::telling())));
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
        let mut fx = with_provider(Some(Box::new(f)));
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

    /// What `on_direction` asks the pane provider for with `m`.
    fn direction_asks(m: windows::Move) -> Vec<Ask> {
        let mut fx = with_provider(Some(Box::new(FakeProvider::with(&[
            Capability::FocusDirection,
        ]))));
        fx.d.on_direction(m).unwrap();
        fake(&mut fx.d).asked()
    }

    #[test]
    fn a_directional_move_asks_to_focus_that_way_once_with_its_count() {
        let asked = |dir, count| direction_asks(windows::Move { dir, count });
        assert_eq!(
            vec![Ask::FocusDirection {
                dir: Direction::Down,
                count: 3
            }],
            asked(windows::Direction::Down, 3)
        );
        // Neovim's hjkl are left, down, up and right
        for (dir, want) in [
            (windows::Direction::Left, Direction::Left),
            (windows::Direction::Up, Direction::Up),
            (windows::Direction::Right, Direction::Right),
        ] {
            assert_eq!(
                vec![Ask::FocusDirection {
                    dir: want,
                    count: 1
                }],
                asked(dir, 1)
            );
        }
        // a stray huge count doesn't flood the pane provider
        assert_eq!(
            vec![Ask::FocusDirection {
                dir: Direction::Left,
                count: 20
            }],
            asked(windows::Direction::Left, 9999)
        );
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
        let mut fx = with_provider(Some(Box::new(f)));
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
        d.panes.list[i].self_attached = true;
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
        d.panes.list[i].self_attached = true;
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
        let p = d.panes.add(Pane::from_neovim_window(50, 50));
        p.tab = 2;
        p.self_attached = true;
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

    /// Window 9's pane, of `two_windows_of_a_tab_page`, attached, so it can
    /// draw windows: its id.
    fn attach_pane_9(d: &mut Daemon) -> u32 {
        let p = d.panes.by_win_mut(9).unwrap();
        p.terminal_surface = Some(Surface::for_test(80, 24));
        p.id
    }

    /// Pane `id` holds tab page 2's windows 1 and 2 and still shows window 9.
    fn assert_holds_tab_page_2(d: &Daemon, id: u32) {
        let p = d.panes.by_id(id).unwrap();
        assert_eq!((0, Some(9)), (p.shown_tab, p.neovim_window_handle));
        assert_eq!(1, p.held.len());
        let h = &p.held[0];
        assert_eq!(2, h.tab);
        assert_eq!(vec![1, 2], h.layout.as_ref().unwrap().windows());
        assert_eq!(vec![id], pane_ids(d));
    }

    #[test]
    fn a_failed_open_tab_page_draws_its_tab_page_whole_in_the_pane_it_opened_in_for_its_life() {
        let mut fx = two_windows_of_a_tab_page();
        let origin = attach_pane_9(&mut fx.d);
        // the first window asks for the Rex floating pane; the second waits for it
        fx.d.new_pane(1).unwrap();
        fx.d.reconcile().unwrap();
        assert_eq!(2, fx.d.panes.list.len());
        assert!(fx.d.panes.by_win(2).is_none());

        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Failed("refused: no".into()));
        fx.d.drain_provider().unwrap();
        assert_holds_tab_page_2(&fx.d, origin);
        assert_eq!(vec!["open_tab_page"], asked_capabilities(&mut fx.d));
    }

    #[test]
    fn after_a_failed_open_tab_page_the_next_tab_page_asks_for_its_own_again() {
        let mut fx = two_windows_of_a_tab_page();
        attach_pane_9(&mut fx.d);
        fx.d.new_pane(1).unwrap();
        let f = fake(&mut fx.d);
        f.answer(f.last_id(), Outcome::Failed("refused: no".into()));
        fx.d.drain_provider().unwrap();

        // tab page 3's first window, from window 9
        draw_external(&mut fx.d, 30, 3, 80, 23);
        fx.d.changes.note_split(windows::Split {
            win: 3,
            side_by_side: false,
            before: false,
            from: 9,
            tab: 3,
        });
        fx.d.new_pane(3).unwrap();
        let p = fx.d.panes.by_win(3).unwrap();
        assert_eq!((3, false), (p.tab, p.holds(9)));
        let sent = sent_by(fx);
        assert_eq!(vec!["open_tab_page", "open_tab_page"], sent.capabilities());
        assert_eq!(block_id(9), sent.origin(1));
    }

    #[test]
    fn without_open_tab_page_a_tab_page_is_drawn_whole_in_the_pane_it_opened_in() {
        let f = FakeProvider::with(&[Capability::OpenBeside, Capability::Close]);
        let mut fx = two_windows_of_a_tab_page();
        fx.d.provider = Some(Box::new(f));
        let origin = attach_pane_9(&mut fx.d);
        fx.d.reconcile().unwrap();
        assert_holds_tab_page_2(&fx.d, origin);
        assert!(fake(&mut fx.d).asked().is_empty());
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
            inset: [0; 4],
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
        opening_for(d, 1001)
    }

    /// Pane `win` asked for, its `open_beside` asked: its id.
    fn opening_for(d: &mut Daemon, win: i64) -> u32 {
        let p = d.panes.add_for_window(win);
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

    /// Pane `pane`, of a window of the first tab page, was given up: it's
    /// gone, and with no pane to draw its window in, the window waits for one.
    fn assert_given_up(d: &Daemon, pane: u32) {
        assert!(d.panes.by_id(pane).is_none());
        assert!(!d.unshown_awaiting_origin.is_empty());
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
    fn a_layout_told_before_an_open_is_done_still_places_its_pane() {
        let mut fx = with_provider(Some(Box::new(FakeProvider::telling())));
        let id = opening(&mut fx.d);
        let ask = fake(&mut fx.d).last_id();
        // the live layout has the new terminal before its open is done
        let rect = [0.5, 0.0, 0.5, 1.0];
        let l = layout_of(&[("w1", &[(block_id(5).as_str(), rect, false)])]);
        fake(&mut fx.d).push(ProviderEvent::Layout(l));
        fx.d.drain_provider().unwrap();
        assert_eq!(Area::Unreported, fx.d.panes.by_id(id).unwrap().area);
        // the open done: the pane where the live layout has it
        fake(&mut fx.d).opened(ask, &block_id(5));
        fx.d.drain_provider().unwrap();
        let p = fx.d.panes.by_id(id).unwrap();
        assert_eq!(
            Some(block_id(5).as_str()),
            p.terminal.as_ref().map(TerminalId::as_str)
        );
        assert_eq!(live("w1", rect), p.area);
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
        let origin = fx.d.panes.add(Pane::from_neovim_window(90, 1000));
        origin.self_attached = true;
        let origin = origin.id;
        note_split_from(&mut fx.d, 1002, 1000, true, false);
        fx.d.new_pane(1002).unwrap();
        assert_eq!(Some(origin), fx.d.panes.by_win(1002).map(|p| p.id));
        assert!(fx.d.panes.by_id(origin).unwrap().layout.is_some());
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
        let mut fx = with_provider(Some(Box::new(f)));
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

    /// A daemon whose pane provider runs `argv` to focus, found on this
    /// process's PATH, watched on its loop.
    fn focusing_by(argv: &[&str]) -> Fixture {
        commanded_by(&[(Capability::Focus, argv)])
    }

    /// A daemon whose pane provider runs a command per `(capability, argv)`,
    /// found on this process's PATH, watched on its loop.
    fn commanded_by(commands: &[(Capability, &[&str])]) -> Fixture {
        use crate::providers::command::{CapCommand, CommandProvider, Spec};
        let spec = Spec {
            name: "cmd".into(),
            commands: commands
                .iter()
                .map(|(cap, argv)| CapCommand {
                    cap: *cap,
                    argv: argv.iter().map(|w| w.as_bytes().to_vec()).collect(),
                    timeout: Duration::from_secs(5),
                    protocol: 1,
                })
                .collect(),
            ..Spec::named("cmd")
        };
        let path = std::env::var_os("PATH").unwrap_or_default();
        let env = [(b"PATH".to_vec(), path.as_encoded_bytes().to_vec())];
        let p = CommandProvider::new(spec, &env, b"/", None).unwrap();
        let mut fx = with_provider(Some(Box::new(p)));
        let fd = fx.d.provider.as_ref().and_then(|p| p.fd()).unwrap();
        fx.d.event_loop
            .add_read(fd, Token::Provider as usize)
            .unwrap();
        fx.d.nvim.ready = true;
        fx
    }

    /// Runs `fx`'s loop until `done`, for at most 5 s.
    fn pump_until(fx: &mut Fixture, done: impl Fn(&Daemon) -> bool) {
        let mut events = [Event::default(); 16];
        let started = Instant::now();
        fx.d.after_batch().unwrap();
        while !done(&fx.d) {
            assert!(started.elapsed() < Duration::from_secs(5), "not done");
            // a timer of the test's own, so it looks again
            fx.d.event_loop.add_timer(999, 100, usize::MAX).unwrap();
            let n = fx.d.event_loop.wait(&mut events).unwrap().len();
            for &e in events[..n].iter().filter(|e| e.token != usize::MAX) {
                fx.d.handle(e).unwrap();
            }
            fx.d.after_batch().unwrap();
        }
    }

    /// A file of `sh` a test's command runs, rewritten between its acts.
    struct Script(std::path::PathBuf);

    impl Script {
        fn new(tag: &str) -> Script {
            let dir = std::env::temp_dir().join(format!(
                "{NAME}-{tag}-{}-{}",
                std::process::id(),
                crate::util::thread_number()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Script(dir.join("script"))
        }

        fn set(&self, text: &str) {
            std::fs::write(&self.0, text).unwrap();
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }
    }

    impl Drop for Script {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    /// Asks `fx`'s pane provider for a report, and runs the loop until it
    /// is done with it.
    fn report(fx: &mut Fixture) {
        fx.d.act(Ask::ReportLayout);
        pump_until(fx, |d| d.asks.waiting() == 0);
    }

    #[test]
    fn a_report_commands_two_areas_set_the_panes_areas_and_tabline_panes() {
        let script = Script::new("two-areas");
        script.set(
            r#"echo '{"areas":[
                {"id":"w1","terminals":[
                    {"terminal":"t1","rect":[0,0,0.5,1]},{"terminal":"t2","rect":[0.5,0,0.5,1]}]},
                {"id":"w2","terminals":[
                    {"terminal":"t3","rect":[0,0,0.5,0.5]},{"terminal":"t9","rect":[0,0.5,1,0.5]}]}]}'"#,
        );
        let fx = commanded_by(&[(Capability::ReportLayout, &["sh", script.path()])]);
        let mut fx = TablineFixture::on(fx);
        fx.d().nvim.ready = true;
        let ids: Vec<u32> = [(0, 100), (1, 100), (2, 60)]
            .into_iter()
            .map(|(slot, cols)| fx.add(slot, cols, Area::Unreported))
            .collect();
        for (n, &id) in ids.iter().enumerate() {
            fx.d().panes.by_id_mut(id).unwrap().terminal =
                Some(TerminalId::from(format!("t{}", n + 1).as_str()));
        }
        fx.d().nvim.current_win = 1000;
        report(&mut fx.fx);
        let area = |fx: &mut TablineFixture, id: u32| fx.d().panes.by_id(id).unwrap().area.clone();
        assert_eq!(live("w1", [0.5, 0.0, 0.5, 1.0]), area(&mut fx, ids[1]));
        assert_eq!(live("w2", [0.0, 0.0, 0.5, 0.5]), area(&mut fx, ids[2]));
        // each area's tabline starts at its left
        let starts: Vec<Option<u32>> = (0..3).map(|i| fx.start(i)).collect();
        assert_eq!(vec![Some(0), Some(100), Some(0)], starts);
    }

    #[test]
    fn a_malformed_report_keeps_the_last_layout_and_exit_69_makes_every_pane_its_own_area() {
        let script = Script::new("malformed");
        let mut fx = commanded_by(&[(Capability::ReportLayout, &["sh", script.path()])]);
        fx.d.nvim.ready = true;
        let id = focus_pane(&mut fx, 1, 1000, Some("t1"));
        fx.d.panes.by_id_mut(id).unwrap().area = Area::Unreported;
        let rect = [0.0, 0.0, 0.5, 1.0];
        script.set(
            r#"echo '{"areas":[{"id":"w","terminals":[{"terminal":"t1","rect":[0,0,0.5,1]}]}]}'"#,
        );
        report(&mut fx);
        assert_eq!(live("w", rect), area_of(&fx, id));

        script.set(r#"echo '{"areas":[{"id":"w","terminals":'"#);
        report(&mut fx);
        assert_eq!(live("w", rect), area_of(&fx, id));
        assert!(fx.d.live_layout.is_some());

        script.set("exit 69");
        report(&mut fx);
        assert_eq!(Area::Own, area_of(&fx, id));
        assert!(fx.d.live_layout.is_none());
    }

    #[test]
    fn an_events_line_whose_report_exits_69_makes_every_pane_its_own_area() {
        let script = Script::new("events-69");
        script.set("exit 69");
        let mut fx = commanded_by(&[
            (Capability::Events, &["sh", "-c", "echo; exec sleep 30"]),
            (Capability::ReportLayout, &["sh", script.path()]),
        ]);
        fx.d.nvim.ready = true;
        let id = focus_pane(&mut fx, 1, 1000, Some("t1"));
        fx.d.panes.by_id_mut(id).unwrap().area = live("w", [0.0, 0.0, 0.5, 1.0]);
        pump_until(&mut fx, |d| d.panes.by_id(id).unwrap().area == Area::Own);
    }

    #[test]
    fn an_events_focus_line_makes_its_terminals_pane_current() {
        let script = Script::new("events-focus");
        // the first only tells where focus starts
        script.set(r#"echo '{"focus":"t1"}'; echo '{"focus":"t2"}'; exec sleep 30"#);
        let mut fx = commanded_by(&[(Capability::Events, &["sh", script.path()])]);
        focus_pane(&mut fx, 1, 1000, Some("t1"));
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1000;
        pump_until(&mut fx, |d| d.nvim.current_win == 1001);
    }

    #[test]
    fn dropping_a_command_runner_kills_its_events() {
        let script = Script::new("events-dropped");
        let pid_file = script.0.with_file_name("pid");
        script.set(&format!("echo $$ > {}; exec sleep 30", pid_file.display()));
        let mut fx = commanded_by(&[(Capability::Events, &["sh", script.path()])]);
        let started = Instant::now();
        let pid: libc::pid_t = loop {
            if let Ok(s) = std::fs::read_to_string(&pid_file)
                && s.ends_with('\n')
            {
                break s.trim().parse().unwrap();
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "events never ran"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        // SAFETY: signal 0 only checks the process is there.
        let alive = |pid| unsafe { libc::kill(pid, 0) } == 0;
        assert!(alive(pid));
        // no terminal in its reach
        fx.d.after_batch().unwrap();
        assert!(fx.d.provider.is_none());
        let gone = Instant::now();
        while alive(pid) {
            assert!(gone.elapsed() < Duration::from_secs(3), "events lives");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A daemon whose fake pane provider can focus and report layouts, but
    /// doesn't tell them: two panes, of windows 1000 and 1001, show `t1`
    /// and `t2`, the first current. The second's id.
    fn reporting_fixture() -> (TablineFixture, u32) {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::ReportLayout);
        f.caps.insert(Capability::Focus);
        let mut fx = TablineFixture::on(with_provider(Some(Box::new(f))));
        fx.d().nvim.ready = true;
        let mut b = 0;
        for slot in 0..2 {
            draw_external(fx.d(), slot + 2, 1000 + i64::from(slot), 80, 23);
            b = fx.add(slot, 80, Area::Own);
            fx.d().panes.by_id_mut(b).unwrap().terminal =
                Some(TerminalId::from(format!("t{}", slot + 1).as_str()));
        }
        fx.d().nvim.current_win = 1000;
        (fx, b)
    }

    /// How many reports the fake was asked for.
    fn reports_asked(fx: &mut TablineFixture) -> usize {
        fake(fx.d())
            .asked()
            .iter()
            .filter(|a| **a == Ask::ReportLayout)
            .count()
    }

    /// The last ask running is answered with `outcome`.
    fn answer_last(fx: &mut TablineFixture, outcome: Outcome) {
        let f = fake(fx.d());
        f.answer(f.last_id(), outcome);
        fx.d().drain_provider().unwrap();
    }

    #[test]
    fn without_events_the_layout_is_read_again_after_an_act_that_worked_one_waiting_at_most() {
        let (mut fx, _) = reporting_fixture();
        fx.d().act(Ask::Close {
            terminal: TerminalId::from("t9"),
        });
        assert_eq!(0, reports_asked(&mut fx));
        answer_last(&mut fx, Outcome::Done(Answer::Nothing));
        assert_eq!(1, reports_asked(&mut fx));
        // more while it runs: one more report waits, for them all
        for _ in 0..3 {
            fx.d().act(Ask::Close {
                terminal: TerminalId::from("t9"),
            });
        }
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        for _ in 0..3 {
            answer_last(&mut fx, Outcome::Done(Answer::Nothing));
        }
        assert_eq!(2, reports_asked(&mut fx));
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        // a report read is no act to read again after
        assert_eq!(2, reports_asked(&mut fx));
        assert_eq!(0, fx.d().asks.waiting());
    }

    #[test]
    fn without_events_a_resize_a_focus_in_or_a_hang_up_reads_the_layout_again() {
        let (mut fx, b) = reporting_fixture();
        fx.d()
            .handle_resize(
                b,
                crate::attach::Size {
                    cols: 70,
                    rows: 20,
                    px: None,
                },
            )
            .unwrap();
        assert_eq!(1, reports_asked(&mut fx));
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        fx.d().on_read(b, b"\x1b[I", Instant::now()).unwrap();
        assert_eq!(2, reports_asked(&mut fx));
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        fx.d().pane_gone(b).unwrap();
        assert_eq!(3, reports_asked(&mut fx));
    }

    #[test]
    fn with_events_no_act_resize_focus_in_or_hang_up_asks_for_a_report() {
        let (mut fx, b) = reporting_fixture();
        fake(fx.d()).caps.insert(Capability::Events);
        fx.d().act(Ask::Close {
            terminal: TerminalId::from("t9"),
        });
        answer_last(&mut fx, Outcome::Done(Answer::Nothing));
        fx.d()
            .handle_resize(
                b,
                crate::attach::Size {
                    cols: 70,
                    rows: 20,
                    px: None,
                },
            )
            .unwrap();
        fx.d().on_read(b, b"\x1b[I", Instant::now()).unwrap();
        fx.d().pane_gone(b).unwrap();
        assert_eq!(0, reports_asked(&mut fx));
        // what it says changed is read, through the layout queue
        fake(fx.d()).push(ProviderEvent::LayoutChanged);
        fake(fx.d()).push(ProviderEvent::LayoutChanged);
        fx.d().drain_provider().unwrap();
        assert_eq!(1, reports_asked(&mut fx));
    }

    #[test]
    fn without_events_an_act_that_failed_reads_the_layout_again_one_timed_out_once_its_gone() {
        let (mut fx, _) = reporting_fixture();
        fx.d().act(Ask::Close {
            terminal: TerminalId::from("t9"),
        });
        answer_last(&mut fx, Outcome::Failed("exit 1".into()));
        assert_eq!(1, reports_asked(&mut fx));
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        // timed out, its command may still be moving terminals
        fx.d().act(Ask::Close {
            terminal: TerminalId::from("t9"),
        });
        let late = Instant::now() + Duration::from_secs(60);
        let d = fx.d();
        d.asks.time_out(d.provider.as_mut().unwrap().as_mut(), late);
        d.run_steps().unwrap();
        assert_eq!(1, reports_asked(&mut fx));
        // the command runner says so once it has been killed
        fake(fx.d()).push(ProviderEvent::LayoutChanged);
        fx.d().drain_provider().unwrap();
        assert_eq!(2, reports_asked(&mut fx));
    }

    #[test]
    fn an_act_that_falls_back_unasked_reads_no_layout() {
        let (mut fx, _) = reporting_fixture();
        // no place_beside: it never runs
        fx.d().act(Ask::PlaceBeside {
            terminal: TerminalId::from("t2"),
            origin: TerminalId::from("t1"),
            dir: Direction::Right,
            size: 40,
            ratio: 0.5,
        });
        fx.d().drain_provider().unwrap();
        // nor does one found unsupported before
        let f = fake(fx.d());
        f.caps.insert(Capability::Resize);
        fx.d().act(Ask::Resize {
            terminal: TerminalId::from("t2"),
            origin: TerminalId::from("t1"),
            dir: Direction::Right,
            size: 40,
            ratio: 0.5,
        });
        answer_last(&mut fx, Outcome::Unsupported);
        assert_eq!(1, reports_asked(&mut fx));
        answer_last(
            &mut fx,
            Outcome::Done(Answer::Layout(LiveLayout::default())),
        );
        fx.d().act(Ask::Resize {
            terminal: TerminalId::from("t2"),
            origin: TerminalId::from("t1"),
            dir: Direction::Right,
            size: 40,
            ratio: 0.5,
        });
        fx.d().drain_provider().unwrap();
        assert_eq!(1, reports_asked(&mut fx));
    }

    #[test]
    fn without_events_a_reports_focus_moves_the_current_pane() {
        let (mut fx, _) = reporting_fixture();
        let with_focus = |t: &str| LiveLayout {
            current: Some(TerminalId::from(t)),
            ..LiveLayout::default()
        };
        // the first only tells where focus starts
        fx.d().act(Ask::ReportLayout);
        answer_last(&mut fx, Outcome::Done(Answer::Layout(with_focus("t1"))));
        assert_eq!(1000, fx.d().nvim.current_win);
        fx.d().act(Ask::ReportLayout);
        answer_last(&mut fx, Outcome::Done(Answer::Layout(with_focus("t2"))));
        assert_eq!(1001, fx.d().nvim.current_win);
    }

    #[test]
    fn a_closed_terminal_is_only_logged_for_now() {
        let (mut fx, _) = reporting_fixture();
        fake(fx.d()).push(ProviderEvent::Closed(TerminalId::from("t2")));
        fx.d().drain_provider().unwrap();
        assert!(fx.d().panes.by_terminal("t2").is_some());
    }

    #[test]
    fn a_focus_commands_answer_comes_through_the_daemons_loop() {
        let mut fx = focusing_by(&["true"]);
        focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        fx.d.after_batch().unwrap();
        assert_eq!(1, fx.d.asks.waiting());
        let mut events = [Event::default(); 16];
        let started = Instant::now();
        while fx.d.asks.waiting() > 0 {
            assert!(started.elapsed() < Duration::from_secs(5), "no answer");
            let n = fx.d.event_loop.wait(&mut events).unwrap().len();
            for &e in &events[..n] {
                fx.d.handle(e).unwrap();
            }
            fx.d.after_batch().unwrap();
        }
        assert!(fx.d.focus.has_focused(&TerminalId::from("t2")));
    }

    #[test]
    fn a_slow_focus_command_doesnt_delay_a_key() {
        let mut fx = focusing_by(&["sleep", "3"]);
        focus_pane(&mut fx, 1, 1000, Some("t1"));
        let b = focus_pane(&mut fx, 2, 1001, Some("t2"));
        fx.d.nvim.current_win = 1001;

        // focus asked for t2: a command that takes 3 s
        let started = Instant::now();
        fx.d.focus_terminal(Some(&TerminalId::from("t2")));
        fx.d.after_batch().unwrap();
        assert_eq!(1, fx.d.asks.waiting());
        sent_to_nvim(&mut fx);
        // keys typed meanwhile reach Neovim at once
        for key in ["x", "y"] {
            fx.d.on_keys(b, key.as_bytes()).unwrap();
            fx.d.after_batch().unwrap();
            let sent = sent_to_nvim(&mut fx);
            assert!(
                sent.contains("nvim_input") && sent.contains(key),
                "{sent:?}"
            );
        }
        // well under the command's 3 s, with room for a loaded machine
        assert!(started.elapsed() < Duration::from_millis(1500));
        // still running
        assert_eq!(1, fx.d.asks.waiting());
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
        assert!(!sent.contains(&format!("{NAME}.go")), "{sent:?}");
        assert_eq!(1001, fx.d.nvim.current_win);
        assert_eq!(vec![focus_ask("t2")], fake(&mut fx.d).asked());
        // a click is no echo: it moves at once
        fx.d.on_read(a, b"\x1b[<0;5;5M", now).unwrap();
        assert!(sent_to_nvim(&mut fx).contains(&format!("{NAME}.go")));
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
        fn name_closes(&self) -> usize {
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

    /// Whether `d` went unattached, and waits for Neovim's word on its
    /// unsaved changes.
    fn deciding(d: &Daemon) -> bool {
        d.stopped.is_none() && matches!(d.lifetime.stage, lifetime::Stage::Deciding { .. })
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
            // entered: the fake can't give focus back
            fx.d.nvim.current_win = 3;
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
            assert_eq!(0, sent.name_closes());
        }
    }

    #[test]
    fn without_open_beside_a_new_window_is_opened_then_placed_once_it_has_its_id_and_attached() {
        for id_first in [true, false] {
            let f =
                FakeProvider::with(&[Capability::Open, Capability::PlaceBeside, Capability::Close]);
            let mut fx = with_provider(Some(Box::new(f)));
            fake(&mut fx.d).unlaid = Some([80, 24]);
            pane_in_block(&mut fx.d, 2, 0, 2);
            note_split_from(&mut fx.d, 3, 2, true, false);
            // entered: the fake can't give focus back
            fx.d.nvim.current_win = 3;
            fx.d.new_pane(3).unwrap();
            let new = fx.d.panes.by_win(3).unwrap();
            // it waits for its layout, as one opened beside its origin does
            assert!(new.awaiting_size);
            let (id, pane) = (new.id, new.attach_id.unwrap());
            let open_id = fake(&mut fx.d).last_id();
            assert!(matches!(fake(&mut fx.d).asked()[..], [Ask::Open { .. }]));

            let opened = |fx: &mut Fixture| {
                fake(&mut fx.d).opened(open_id, "t9");
                fx.d.drain_provider().unwrap();
            };
            let at = Address::new(&format!("open-place-{id_first}"));
            let pty = Pty::new(80, 24);
            if id_first {
                opened(&mut fx);
            }
            assert_eq!(1, fake(&mut fx.d).asks.len(), "placed too soon");
            // at the size of a terminal not laid out yet
            let helper = at.send(&hello_for(pane, 80, 24), Some(pty.slave));
            fx.d.accept(&at.server).unwrap();
            let crate::attach::Message::Accept(_) = answer(&helper) else {
                panic!("not accepted")
            };
            if !id_first {
                assert_eq!(1, fake(&mut fx.d).asks.len(), "placed with no id");
                opened(&mut fx);
            }
            assert!(fx.d.panes.by_id(id).unwrap().awaiting_size);
            let asked = fake(&mut fx.d).asked();
            let [
                _,
                Ask::PlaceBeside {
                    terminal,
                    origin,
                    dir,
                    ..
                },
            ] = &asked[..]
            else {
                panic!("not placed: {asked:?}")
            };
            assert_eq!(
                ("t9", block_id(2).as_str(), Direction::Right),
                (terminal.as_str(), origin.as_str(), *dir)
            );
        }
    }

    #[test]
    fn without_place_beside_a_new_window_opened_by_open_is_an_unplaced_pane() {
        let f = FakeProvider::with(&[Capability::Open, Capability::Close]);
        let mut fx = with_provider(Some(Box::new(f)));
        pane_in_block(&mut fx.d, 2, 0, 2);
        note_split_from(&mut fx.d, 3, 2, true, false);
        fx.d.nvim.current_win = 3;
        fx.d.new_pane(3).unwrap();
        let p = fx.d.panes.by_win(3).unwrap();
        assert_eq!(None, p.placing);
        assert_eq!(vec!["open"], sent_by(fx).capabilities());
    }

    #[test]
    fn a_new_window_without_a_known_source_opens_beside_any_pane_with_a_terminal() {
        let mut fx = fixture();
        let placed = pane_in_block(&mut fx.d, 1, 0, 1);
        note_split_from(&mut fx.d, 3, 99, true, false);
        fx.d.nvim.current_win = 3;
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
        assert_eq!(0, sent.name_closes());
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
    fn with_no_pane_provider_a_tab_pages_windows_are_held_in_the_pane_they_opened_in() {
        let mut fx = two_windows_of_a_tab_page();
        fx.d.provider = None;
        // none can draw them yet: they wait unshown
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.by_win(1).is_none() && fx.d.panes.by_win(2).is_none());

        let origin = attach_pane_9(&mut fx.d);
        fx.d.reconcile().unwrap();
        assert_holds_tab_page_2(&fx.d, origin);
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
        assert_eq!(0, sent.name_closes());
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
        // the pane neopane was run in, 81 x 24: its window 81 x 23, over the
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
        assert!(deciding(&fx.d));

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
    fn closing_the_last_non_floating_pane_goes_unattached_and_closes_no_window() {
        let mut fx = fixture();
        let id = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.pane_gone(id).unwrap();
        assert!(deciding(&fx.d));
        assert!(fx.d.panes.list.is_empty());
        let sent = sent_by(fx);
        assert!(sent.asks.is_empty());
        assert_eq!(0, sent.name_closes());
    }

    #[test]
    fn a_pane_whose_request_failed_doesnt_count_as_another_pane() {
        let mut fx = fixture();
        let failed = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.panes.by_id_mut(failed).unwrap().closing = Some(Close::RequestFailed);
        let id = pane_in_block(&mut fx.d, 2, 0, 2);
        fx.d.pane_gone(id).unwrap();
        assert!(deciding(&fx.d));
        assert!(fx.d.panes.list.is_empty());
        let sent = sent_by(fx);
        // its terminal was closed as its request failed
        assert!(sent.asks.is_empty());
        assert_eq!(0, sent.name_closes());
    }

    #[test]
    fn a_floating_pane_doesnt_count_as_another_pane_either() {
        let mut fx = fixture();
        let floating = made_picker_pane(&mut fx.d, 10, 5);
        let id = pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.pane_gone(id).unwrap();
        assert!(deciding(&fx.d));
        // it goes too, its terminal closed
        assert!(fx.d.panes.by_id(floating).is_none());
        // its close waits its turn behind the open, unanswered
        assert_eq!(2, fx.d.asks.waiting());
        let sent = sent_by(fx);
        assert_eq!(vec!["open_float"], sent.capabilities());
        assert_eq!(0, sent.name_closes());
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
        assert_eq!(1, sent.name_closes());
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
        assert_eq!(1, sent.name_closes());
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
        assert_eq!(0, sent.name_closes());
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
            assert_eq!(0, sent.name_closes());
        }
    }

    #[test]
    fn after_a_flush_with_no_panes_left_the_daemon_goes_unattached() {
        let mut fx = fixture();
        fx.d.nvim.ready = true;
        pane_in_block(&mut fx.d, 1, 0, 1);
        fx.d.reconcile().unwrap();
        assert!(fx.d.panes.list.is_empty());
        assert!(deciding(&fx.d));
        let sent = sent_by(fx);
        assert_eq!(vec!["close"], sent.capabilities());
        assert_eq!(0, sent.name_closes());
    }
}
