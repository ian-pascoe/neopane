//! Rex's pane provider, built in (#92): what's asked through the seam, as
//! Rex requests on a `rex::session::Session`, the wire.
//!
//! - `open_beside` is `session.new_split` beside the origin's block,
//!   `open_tab_page` a `session.new_layer` at the tab page bounds, and
//!   `open_float` one at the float group's bounds with room for Rex's frame
//!   (`bounds.rs`).
//!   None sends a `focus` key, so none moves Rex's focus. Each is done once
//!   its block is disowned, so Rex, not the daemon, sizes it.
//! - `close` is always `block.close`: a floating layer goes with its last
//!   block. `close_layer` only cleans up a `new_layer` answered with no block
//!   id, which then fails.
//! - `place_beside` is `session.move_block` and `resize`
//!   `session.resize_pane`, given the anchor's share, `1 − ratio`.
//! - `focus` is `session.focus_block`, which switches Rex windows and tabs
//!   itself; `focus_direction` is `count` identical `session.focus_direction`
//!   requests with `wrap: false`, done once the last is answered.
//! - An error `method_not_found` is unsupported, any other fails the act. Rex
//!   hanging up, failing, or sending what can't be read is `Gone`.
//!
//! - Each view is told as a `LiveLayout` (`layout.rs`), one layout area per
//!   Rex window, and `report_layout` is answered from the last. A float
//!   group's block is placed at its floating layer's bounds, its frame the
//!   inset. A new layer opens in its origin's Rex window.
//! - Each view's focused block is told as `Focused`, unless our own focus
//!   change is still landing (`RexProvider::landing`): the daemon tells the
//!   user's moves from what it knew (`ProviderFocus`).
//! - `find` asks for the processes of the block `REX_BLOCK` names, and
//!   compares its terminal with the attach's (`find.rs`).
//!

pub(crate) mod bounds;
pub(crate) mod find;
pub(crate) mod layout;

use std::collections::VecDeque;
use std::os::fd::RawFd;

use serde_json::{Value, json};

use crate::daemon::live_layout::LiveLayout;
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, CapabilitySet, Direction, Outcome, PaneProvider, ProviderEvent,
    TerminalId,
};
use crate::log::{self, Point, log_info};
use crate::name::NAME;
use crate::rex::client::{self, ResponseResult};
use crate::rex::ids;
use crate::rex::session::{self, Conn, Request, Session, Update};

/// What Rex is asked through the seam.
const CAPABILITIES: &[Capability] = &[
    Capability::OpenBeside,
    Capability::OpenFloat,
    Capability::OpenTabPage,
    Capability::Close,
    Capability::PlaceBeside,
    Capability::Resize,
    Capability::Find,
    Capability::Focus,
    Capability::FocusDirection,
    Capability::ReportLayout,
    // views come unasked
    Capability::Events,
];

/// A `find` asked of Rex: its block, and the attaching terminal's device.
struct Finding {
    tag: u64,
    block: String,
    device: Option<u64>,
}

/// A `focus_direction` ask, done once Rex answered its last request.
struct Directing {
    tag: u64,
    /// Its requests still unanswered.
    left: u32,
    /// The first refusal, which fails it.
    refused: Option<client::Error>,
}

pub(crate) struct RexProvider {
    /// The daemon reaches its connection's fd through it.
    pub session: Session,
    /// The blocks opened for asks, each waiting for its disown's answer.
    opened: Vec<(u64, TerminalId)>,
    /// The `open_float` asks whose layer has no block yet.
    float_asks: Vec<u64>,
    /// The blocks opened for float groups, framed in the layout.
    float_blocks: Vec<TerminalId>,
    /// The `focus_direction` asks waiting for their requests' answers.
    directions: Vec<Directing>,
    /// The layout of the last view.
    layout: Option<LiveLayout>,
    events: VecDeque<ProviderEvent>,
    /// Told `Gone`: nothing more comes.
    gone: bool,
    /// The `find` asks waiting for Rex's `terminal.process`.
    finding: Vec<Finding>,
    /// The controlling terminal of a process; a fake in tests.
    pub(crate) tty_of_pid: find::TtyOfPid,
    /// A `find` trusted `REX_BLOCK` unproved, which is logged once.
    pub(crate) trusted: bool,
    /// What was asked before Rex attached us to its session, asked once it
    /// has: Rex runs some requests out of order.
    deferred: Vec<(AskId, Ask)>,
    /// The revision our last focus change landed at: a view older than it
    /// says the focus from before.
    applied_rev: i64,
}

impl RexProvider {
    pub(crate) fn new(session: Session) -> RexProvider {
        RexProvider {
            session,
            opened: Vec::new(),
            float_asks: Vec::new(),
            float_blocks: Vec::new(),
            directions: Vec::new(),
            layout: None,
            events: VecDeque::new(),
            gone: false,
            finding: Vec::new(),
            tty_of_pid: find::tty_of_pid,
            trusted: false,
            deferred: Vec::new(),
            applied_rev: 0,
        }
    }

    /// Rex's pane provider on `stream`, a new connection to the Rex server,
    /// for session `session_id`: connecting, then attaching to the session,
    /// as Rex answers, without blocking. What is asked meanwhile waits.
    pub(crate) fn connecting(
        stream: std::os::unix::net::UnixStream,
        session_id: &str,
    ) -> Result<RexProvider, client::ClientError> {
        let client = client::Client::start(stream, NAME)?;
        let mut session = Session::new(client, session_id.to_string(), NAME);
        session.attach()?;
        Ok(RexProvider::new(session))
    }

    /// Whether asks wait for Rex to attach us, and it has.
    fn may_ask_deferred(&self) -> bool {
        !self.deferred.is_empty() && self.session.conn == Conn::Attached
    }

    /// Sends what `ask` asks of Rex. False if it can't be asked of Rex.
    fn send(&mut self, tag: u64, ask: Ask) -> Result<bool, client::ClientError> {
        match ask {
            Ask::OpenBeside {
                attach,
                origin,
                dir,
                cols,
                rows,
                ..
            } => {
                let layout = json!({ "block": block(&attach, Some([cols, rows])) });
                self.session
                    .new_split(tag, origin.as_str(), dir.across(), dir.before(), layout)?;
            }
            // `cols` x `rows` is the origin's layout area: the Rex window
            Ask::OpenTabPage {
                attach,
                origin,
                cols,
                rows,
                ..
            } => {
                let size = bounds::tab_page_size(cols, rows);
                let layout = json!({ "block": block(&attach, Some(size)) });
                let window = self.window_of(&origin);
                self.session
                    .new_layer(tag, window.as_deref(), bounds::TAB_PAGE_BOUNDS, layout)?;
            }
            Ask::OpenFloat {
                attach,
                origin,
                x,
                y,
                cols,
                rows,
                origin_cols,
                origin_rows,
                area_bounds,
                ..
            } => {
                let Some(bounds) = bounds::float_bounds(
                    [x, y],
                    [cols, rows],
                    [origin_cols, origin_rows],
                    area_bounds,
                ) else {
                    self.answer(tag, Outcome::Failed("no room for a floating pane".into()));
                    return Ok(true);
                };
                let layout = json!({ "block": block(&attach, None) });
                let window = self.window_of(&origin);
                self.float_asks.push(tag);
                self.session
                    .new_layer(tag, window.as_deref(), bounds, layout)?;
            }
            // `session.move_block` puts it only right of or below `{origin}`
            Ask::PlaceBeside { dir, .. } if dir.before() => {
                let why = "Rex places a terminal only right of or below another";
                self.answer(tag, Outcome::Failed(why.into()));
                return Ok(true);
            }
            // Rex's ratio is the anchor's share, the ask's the placed terminal's
            Ask::PlaceBeside {
                terminal,
                origin,
                dir,
                ratio,
                ..
            } => self.session.move_block(
                tag,
                terminal.as_str(),
                origin.as_str(),
                dir.across(),
                1.0 - ratio,
            )?,
            // `{origin}` is before the split, `{terminal}` after it
            Ask::Resize {
                terminal,
                origin,
                dir,
                ratio,
                ..
            } => self.session.resize_pane(
                tag,
                origin.as_str(),
                terminal.as_str(),
                dir.across(),
                1.0 - ratio,
            )?,
            Ask::Close { terminal } => {
                self.float_blocks.retain(|b| *b != terminal);
                self.session.close_block(tag, terminal.as_str())?
            }
            Ask::Focus { terminal } => {
                if block_of(&terminal).is_none() {
                    self.answer(tag, Outcome::Failed(format!("{terminal} is no Rex block")));
                    return Ok(true);
                }
                self.session.focus_block(tag, terminal.as_str())?;
                log::trace(Point::Focus, 0);
            }
            Ask::FocusDirection { dir, count } => {
                // the daemon asks one at least
                debug_assert!(count > 0);
                self.directions.push(Directing {
                    tag,
                    left: count,
                    refused: None,
                });
                for _ in 0..count {
                    self.session.focus_direction(tag, rex_direction(dir))?;
                    log::trace(Point::Focus, 0);
                }
            }
            Ask::Find { env, device, .. } => match find::block_in(&env) {
                Ok(block) => {
                    self.session.process(tag, &block)?;
                    self.finding.push(Finding { tag, block, device });
                }
                Err(why) => self.answer(tag, Outcome::Failed(why)),
            },
            Ask::ReportLayout => {
                let outcome = match &self.layout {
                    Some(l) => Outcome::Done(Answer::Layout(l.clone())),
                    None => Outcome::Failed("no view from Rex yet".into()),
                };
                self.answer(tag, outcome);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The Rex window of terminal `origin`, as the last view had it, where
    /// a new layer opens; None, Rex's active window, if it didn't.
    fn window_of(&self, origin: &TerminalId) -> Option<String> {
        let l = self.layout.as_ref()?;
        let area = l
            .areas
            .iter()
            .find(|a| a.terminals.iter().any(|t| t.terminal == *origin))?;
        Some(area.id.clone())
    }

    fn answer(&mut self, tag: u64, outcome: Outcome) {
        self.events.push_back(ProviderEvent::Answered {
            id: AskId(tag),
            outcome,
        });
    }

    /// What one of Rex's updates tells the daemon, if anything.
    fn on_update(&mut self, u: Update) -> Result<(), client::ClientError> {
        match u {
            Update::Answered { req, result } => self.on_answer(req, result)?,
            Update::View(view) => self.on_view(&view),
            Update::Closed => self.go(),
        }
        Ok(())
    }

    /// A newer view: told as a live layout, then its focused block as
    /// `Focused`, unless our own focus change is still landing.
    fn on_view(&mut self, view: &Value) {
        let rev = client::get_int(view, "revision").unwrap_or(0);
        let focused = layout::focused_block(view);
        if focused.is_none() {
            self.session
                .log_unexpected_response(Request::View, "focused block");
        }
        // a view from before our focus change landed says the old focus
        let current = focused.filter(|_| !self.landing(rev));
        if let Some(Value::Array(windows)) = client::get(view, "windows") {
            let l = layout::read(windows, current.as_deref(), &self.float_blocks);
            self.layout = Some(l.clone());
            self.events.push_back(ProviderEvent::Layout(l));
        }
        if let Some(block) = current {
            let t = TerminalId::from(block.as_str());
            self.events.push_back(ProviderEvent::Focused(t));
        }
    }

    /// Whether a focus change of ours is unanswered.
    fn focusing(&self) -> bool {
        self.session.asking(|r| matches!(r, Request::FocusBlock(_)))
    }

    /// Whether a view at `rev` may say the focus from before our own change:
    /// it is unanswered, or landed after `rev`.
    fn landing(&self, rev: i64) -> bool {
        self.focusing() || rev < self.applied_rev
    }

    /// Rex answered a `focus_block`: the revision it landed at is recorded.
    /// One refused, with no other on its way, fetches the view: where focus
    /// really is.
    fn on_focus_block_result(
        &mut self,
        result: &ResponseResult,
    ) -> Result<(), client::ClientError> {
        match result {
            ResponseResult::Ok(val) => {
                let rev = client::get_int(val, "revision").unwrap_or(0);
                self.applied_rev = self.applied_rev.max(rev);
            }
            ResponseResult::Err(_) if !self.focusing() => self.session.fetch_view()?,
            ResponseResult::Err(_) => {}
        }
        Ok(())
    }

    /// Rex answered a request made for an ask.
    fn on_answer(
        &mut self,
        req: Request,
        result: ResponseResult,
    ) -> Result<(), client::ClientError> {
        if matches!(req, Request::FocusBlock(_)) {
            self.on_focus_block_result(&result)?;
        }
        match (req, result) {
            (Request::NewSplit(tag) | Request::NewLayer(tag), ResponseResult::Ok(ok)) => {
                let float = self.took_float_ask(tag);
                if let Some(block) = session::new_block_id(&ok) {
                    // framed in views from now on, before the open is done
                    if float {
                        self.float_blocks.push(TerminalId::from(block));
                    }
                    // done once Rex, not us, sizes it
                    self.session.disown(tag, block)?;
                    self.opened.push((tag, TerminalId::from(block)));
                    return Ok(());
                }
                self.session.log_unexpected_response(req, "block_ids");
                match client::get_str(&ok, "layer_id") {
                    // the layer would stay, empty, for good
                    Some(layer) if matches!(req, Request::NewLayer(_)) => {
                        self.session.close_layer(tag, layer)?
                    }
                    _ => self.answer(tag, no_block()),
                }
            }
            (Request::DisownBlock(tag), _) => {
                if let Some(i) = self.opened.iter().position(|(t, _)| *t == tag) {
                    let (_, block) = self.opened.swap_remove(i);
                    self.answer(tag, Outcome::Done(Answer::Terminal(block)));
                }
            }
            (Request::CloseLayer(tag), _) => self.answer(tag, no_block()),
            (
                Request::CloseBlock(tag)
                | Request::MoveBlock(tag)
                | Request::ResizePane(tag)
                | Request::FocusBlock(tag),
                ResponseResult::Ok(_),
            ) => self.answer(tag, Outcome::Done(Answer::Nothing)),
            (Request::FocusDirection(tag), result) => self.on_direction_answer(tag, result),
            (Request::Process(tag), result) => self.on_process_answer(tag, &result),
            // a block already gone is closed (#86 D10)
            (Request::CloseBlock(tag), ResponseResult::Err(e)) if e.code == "not_found" => {
                self.answer(tag, Outcome::Done(Answer::Nothing))
            }
            (
                Request::NewSplit(tag)
                | Request::NewLayer(tag)
                | Request::CloseBlock(tag)
                | Request::MoveBlock(tag)
                | Request::ResizePane(tag)
                | Request::FocusBlock(tag),
                ResponseResult::Err(e),
            ) => {
                self.took_float_ask(tag);
                self.answer(tag, outcome_of_error(&e))
            }
            _ => {}
        }
        Ok(())
    }

    /// One of the requests of `focus_direction` ask `tag` was answered: the
    /// last answers the ask, failed if any was refused.
    fn on_direction_answer(&mut self, tag: u64, result: ResponseResult) {
        let Some(i) = self.directions.iter().position(|d| d.tag == tag) else {
            return;
        };
        let d = &mut self.directions[i];
        d.left -= 1;
        if let ResponseResult::Err(e) = result {
            d.refused.get_or_insert(e);
        }
        if d.left > 0 {
            return;
        }
        let outcome = match self.directions.swap_remove(i).refused {
            Some(e) => outcome_of_error(&e),
            None => Outcome::Done(Answer::Nothing),
        };
        self.answer(tag, outcome);
    }

    /// Rex told the processes of `find` ask `tag`'s block: its verdict.
    fn on_process_answer(&mut self, tag: u64, result: &ResponseResult) {
        let Some(i) = self.finding.iter().position(|f| f.tag == tag) else {
            return;
        };
        let Finding { block, device, .. } = self.finding.swap_remove(i);
        let outcome = match find::verdict(result, device, self.tty_of_pid) {
            find::Verdict::Proven => Outcome::Done(Answer::Terminal(block.as_str().into())),
            find::Verdict::Disproved(why) => Outcome::Failed(why),
            find::Verdict::Unprovable(why) => {
                if !self.trusted {
                    self.trusted = true;
                    log_info!("{NAME} daemon: rex: trusting {}: {why}", find::BLOCK_VAR);
                }
                Outcome::Done(Answer::Terminal(block.as_str().into()))
            }
        };
        self.answer(tag, outcome);
    }

    /// Whether ask `tag` was an `open_float` waiting for its block; it no
    /// longer is.
    fn took_float_ask(&mut self, tag: u64) -> bool {
        let before = self.float_asks.len();
        self.float_asks.retain(|&t| t != tag);
        self.float_asks.len() != before
    }

    /// Rex is gone: tells it once; nothing asked will be answered.
    fn go(&mut self) {
        if self.gone {
            return;
        }
        self.gone = true;
        self.deferred.clear();
        self.session.conn = Conn::Closed;
        let reason = self
            .session
            .closed
            .clone()
            .unwrap_or_else(|| "Rex closed".into());
        self.events.push_back(ProviderEvent::Gone { reason });
    }
}

impl PaneProvider for RexProvider {
    fn name(&self) -> &str {
        "rex"
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::of(CAPABILITIES)
    }

    fn ask(&mut self, id: AskId, ask: Ask) {
        if self.gone {
            return;
        }
        if self.session.conn == Conn::Attaching {
            self.deferred.push((id, ask));
            return;
        }
        match self.send(id.0, ask) {
            Ok(true) => {}
            // not built in
            Ok(false) => self.answer(id.0, Outcome::Unsupported),
            Err(_) => self.go(),
        }
    }

    fn on_ready(&mut self, _token: usize) {
        self.session.flush();
        self.session.read();
    }

    fn next(&mut self) -> Option<ProviderEvent> {
        loop {
            if let Some(e) = self.events.pop_front() {
                return Some(e);
            }
            if self.gone {
                return None;
            }
            if self.may_ask_deferred() {
                for (id, ask) in std::mem::take(&mut self.deferred) {
                    self.ask(id, ask);
                }
                continue;
            }
            match self.session.next() {
                Ok(Some(u)) => {
                    if self.on_update(u).is_err() {
                        self.go();
                    }
                }
                Ok(None) if self.may_ask_deferred() => {}
                Ok(None) => return None,
                Err(_) => self.go(),
            }
        }
    }

    fn unlaid_size(&self) -> Option<[u32; 2]> {
        Some(bounds::UNLAID_SIZE)
    }

    fn context_vars(&self) -> &[&str] {
        &["REX_SERVER", "REX_SESSION"]
    }

    fn find_vars(&self) -> &[&str] {
        &[find::BLOCK_VAR]
    }

    fn fd(&self) -> Option<RawFd> {
        Some(self.session.client.fd())
    }

    fn wants_write(&self) -> bool {
        self.session.client.wants_write()
    }
}

/// A Rex block running `attach`, labelled with our name, at `size` if given.
fn block(attach: &[String], size: Option<[u32; 2]>) -> Value {
    let mut options = json!({ "shell": "none", "command": attach });
    if let Some([columns, rows]) = size {
        options["size"] = json!({ "columns": columns, "rows": rows });
    }
    json!({
        "flavor": "com.superlogical.terminal.shell",
        "label": NAME,
        "options": options,
    })
}

/// Rex's name for the way `dir` goes.
fn rex_direction(dir: Direction) -> session::Direction {
    match dir {
        Direction::Left => session::Direction::Left,
        Direction::Right => session::Direction::Right,
        Direction::Up => session::Direction::Up,
        Direction::Down => session::Direction::Down,
    }
}

/// A Rex block id, if `t` is one.
fn block_of(t: &TerminalId) -> Option<ids::Block> {
    ids::block(t.as_str().as_bytes()).ok()
}

/// An open Rex answered with no block.
fn no_block() -> Outcome {
    Outcome::Failed("Rex answered with no block".into())
}

/// What Rex refusing a request means for its act (#92 decision 5).
fn outcome_of_error(e: &client::Error) -> Outcome {
    if e.code == "method_not_found" {
        return Outcome::Unsupported;
    }
    Outcome::Failed(format!("{}: {}", e.code, e.message))
}

#[cfg(test)]
mod tests;
