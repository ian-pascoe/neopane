//! The pane provider seam (#92 decisions 1–3, ADR 0006): the daemon asks a
//! pane provider for a capability (`Ask`), and reads its answers, and what it
//! tells unasked, as `ProviderEvent`s from the event loop.
//!
//! A pane provider never blocks in `ask` and never answers inline: every
//! answer comes back through `next`, after the loop said its sources are
//! ready (`on_ready`). What the daemon does with acts, their queue, deadlines
//! and fallbacks, is `asks.rs`'s.
//!
//! The opens, `close`, `place_beside`, `resize`, `report_layout`, `focus`
//! and `focus_direction` are asked through it.

use std::fmt;
use std::time::Duration;

use super::live_layout::LiveLayout;
use crate::attach::PaneId;

#[cfg(test)]
pub(crate) mod fake;

/// One thing a pane provider can do when asked (ADR 0006), by its
/// snake_case name (#93).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Capability {
    Open,
    OpenBeside,
    OpenFloat,
    OpenTabPage,
    Close,
    Find,
    PlaceBeside,
    Resize,
    Focus,
    FocusDirection,
    ReportLayout,
    /// It tells live layouts and focus unasked, as Rex's views and the
    /// command runner's `events` do.
    Events,
}

impl Capability {
    /// Its name, as logs, warnings and configs spell it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Capability::Open => "open",
            Capability::OpenBeside => "open_beside",
            Capability::OpenFloat => "open_float",
            Capability::OpenTabPage => "open_tab_page",
            Capability::Close => "close",
            Capability::Find => "find",
            Capability::PlaceBeside => "place_beside",
            Capability::Resize => "resize",
            Capability::Focus => "focus",
            Capability::FocusDirection => "focus_direction",
            Capability::ReportLayout => "report_layout",
            Capability::Events => "events",
        }
    }

    /// How long an ask of it may take before it counts as failed: ADR 0006's
    /// defaults (#93 decision 10), Rex's the same.
    pub(crate) fn default_timeout(self) -> Duration {
        Duration::from_millis(match self {
            Capability::Open
            | Capability::OpenBeside
            | Capability::OpenFloat
            | Capability::OpenTabPage => 5000,
            Capability::Find => 3000,
            Capability::ReportLayout => 1000,
            Capability::Close
            | Capability::PlaceBeside
            | Capability::Resize
            | Capability::Focus
            | Capability::FocusDirection
            | Capability::Events => 2000,
        })
    }

    /// Whether its acts wait their turn in the layout queue, one at a time:
    /// Rex applies some back-to-back layout requests out of order.
    pub(crate) fn is_layout(self) -> bool {
        !matches!(self, Capability::Focus | Capability::FocusDirection)
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of capabilities.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CapabilitySet(u16);

impl CapabilitySet {
    pub(crate) fn of(caps: &[Capability]) -> CapabilitySet {
        let mut set = CapabilitySet::default();
        for &c in caps {
            set.insert(c);
        }
        set
    }

    pub(crate) fn has(self, c: Capability) -> bool {
        self.0 & (1 << c as u8) != 0
    }

    pub(crate) fn insert(&mut self, c: Capability) {
        self.0 |= 1 << c as u8;
    }
}

/// The daemon's number for one ask, unique for its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct AskId(pub u64);

/// A pane provider's id for a terminal, opaque, compared byte for byte.
/// Rex's is a full `block:<UUIDv7>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TerminalId(pub Box<str>);

impl TerminalId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for TerminalId {
    fn from(s: &str) -> TerminalId {
        TerminalId(s.into())
    }
}

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Which side of its origin a terminal goes, or which way focus moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The side of its origin a split puts the new window: Neovim's
    /// `side_by_side` and `before`.
    pub(crate) fn of_split(side_by_side: bool, before: bool) -> Direction {
        match (side_by_side, before) {
            (true, true) => Direction::Left,
            (true, false) => Direction::Right,
            (false, true) => Direction::Up,
            (false, false) => Direction::Down,
        }
    }

    /// Whether it splits side by side, left and right.
    pub(crate) fn across(self) -> bool {
        matches!(self, Direction::Left | Direction::Right)
    }

    /// Whether it goes before its origin: left of it or above it.
    pub(crate) fn before(self) -> bool {
        matches!(self, Direction::Left | Direction::Up)
    }
}

/// One ask of a pane provider: a variant per capability, its fields named
/// after ADR 0006's inputs (#92 decision 2).
#[derive(Debug, Clone, PartialEq)]
// `open` is asked by the command runner (#125), and some of `find`'s
// fields only by it
#[allow(dead_code)]
pub(crate) enum Ask {
    Open {
        pane: PaneId,
        /// `{attach}`: the words of the command the terminal runs.
        attach: Vec<String>,
        title: String,
        cols: u32,
        rows: u32,
    },
    OpenBeside {
        pane: PaneId,
        attach: Vec<String>,
        title: String,
        origin: TerminalId,
        dir: Direction,
        /// The new terminal's size along the split, and its share of the
        /// room it splits with `origin`.
        size: u32,
        ratio: f64,
        cols: u32,
        rows: u32,
    },
    OpenFloat {
        pane: PaneId,
        attach: Vec<String>,
        title: String,
        origin: TerminalId,
        /// The float group's top-left, in cells from `origin`'s top left,
        /// and its size; `origin_cols` x `origin_rows` is `origin`'s size.
        x: i32,
        y: i32,
        cols: u32,
        rows: u32,
        origin_cols: u32,
        origin_rows: u32,
        /// The float as fractions of `origin`'s layout area, if that's a
        /// live one.
        area_bounds: Option<[f64; 4]>,
    },
    OpenTabPage {
        pane: PaneId,
        attach: Vec<String>,
        title: String,
        origin: TerminalId,
        cols: u32,
        rows: u32,
    },
    Close {
        terminal: TerminalId,
    },
    Find {
        pane: PaneId,
        pid: u32,
        tty: String,
        /// The attaching terminal's device (`st_rdev`), as the daemon read it
        /// from the terminal handed over, or from `tty` when relayed.
        device: Option<u64>,
        title: String,
        /// The `find` variables' values, as the attach's environment has them.
        env: Vec<(Vec<u8>, Vec<u8>)>,
    },
    PlaceBeside {
        terminal: TerminalId,
        origin: TerminalId,
        dir: Direction,
        size: u32,
        ratio: f64,
    },
    Resize {
        terminal: TerminalId,
        origin: TerminalId,
        dir: Direction,
        size: u32,
        ratio: f64,
    },
    Focus {
        terminal: TerminalId,
    },
    FocusDirection {
        dir: Direction,
        count: u32,
    },
    ReportLayout,
}

impl Ask {
    /// The capability it asks for.
    pub(crate) fn capability(&self) -> Capability {
        match self {
            Ask::Open { .. } => Capability::Open,
            Ask::OpenBeside { .. } => Capability::OpenBeside,
            Ask::OpenFloat { .. } => Capability::OpenFloat,
            Ask::OpenTabPage { .. } => Capability::OpenTabPage,
            Ask::Close { .. } => Capability::Close,
            Ask::Find { .. } => Capability::Find,
            Ask::PlaceBeside { .. } => Capability::PlaceBeside,
            Ask::Resize { .. } => Capability::Resize,
            Ask::Focus { .. } => Capability::Focus,
            Ask::FocusDirection { .. } => Capability::FocusDirection,
            Ask::ReportLayout => Capability::ReportLayout,
        }
    }
}

/// What a pane provider gives back for an ask that worked.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer {
    Nothing,
    Terminal(TerminalId),
    /// `report_layout`'s.
    Layout(LiveLayout),
}

/// How an ask ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    Done(Answer),
    /// The pane provider can't do it, now or until it is chosen again.
    Unsupported,
    /// This act failed, and only it: why, for the log and the warning.
    Failed(String),
}

/// What a pane provider tells the daemon, from `next`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ProviderEvent {
    Answered {
        id: AskId,
        outcome: Outcome,
    },
    /// The terminals' places, as they changed.
    Layout(LiveLayout),
    /// The terminals' places or focus may have changed: the daemon asks for
    /// a report, through its layout queue. The command runner's `events`
    /// tells it for each line.
    LayoutChanged,
    /// The user focused this terminal.
    Focused(TerminalId),
    /// This terminal closed: only logged for now.
    Closed(TerminalId),
    /// The pane provider failed as a whole: nothing it was asked will be
    /// answered.
    Gone {
        reason: String,
    },
}

/// A pane provider: Rex's, built in, and later the command runner; in
/// tests, `FakeProvider` (#92 decision 1).
pub(crate) trait PaneProvider {
    /// Its name, as warnings and the log show it.
    fn name(&self) -> &str;

    /// What it can be asked.
    fn capabilities(&self) -> CapabilitySet;

    /// How long an ask of `cap` may take before it counts as failed.
    fn timeout(&self, cap: Capability) -> Duration {
        cap.default_timeout()
    }

    /// Starts `ask`, whose answer comes from `next` as
    /// `ProviderEvent::Answered` with `id`. Never blocks, never answers inline.
    fn ask(&mut self, id: AskId, ask: Ask);

    /// A source it registered on the loop under `Token::Provider` is ready.
    fn on_ready(&mut self, token: usize);

    /// The next thing it has to tell, or None until a source is ready again.
    fn next(&mut self) -> Option<ProviderEvent>;

    /// The size a terminal it opened starts at before it is laid out, if it
    /// has one: a hello of that size may not be the terminal's real size yet.
    fn unlaid_size(&self) -> Option<[u32; 2]>;

    /// The variables a self-attached terminal must share with the choosing
    /// attach to be in its reach.
    fn context_vars(&self) -> &[&str];

    /// The variables `find` is given, as the attach's environment has them.
    fn find_vars(&self) -> &[&str] {
        &[]
    }

    /// How long a terminal it opened has to attach, from its open being
    /// done, before its window is drawn in its origin pane (#88 decision
    /// 15). The daemon cuts it to `MAX_TIMEOUT`.
    fn attach_timeout(&self) -> Duration {
        default_attach_timeout()
    }
}

/// The attach timeout of a pane provider that sets none.
pub(crate) fn default_attach_timeout() -> Duration {
    crate::config::check::ATTACH_TIMEOUT
}
