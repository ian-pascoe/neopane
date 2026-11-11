//! Rex's pane provider, built in (#92): the opens and `close` asked through
//! the seam, as Rex requests on today's `rex::session::Session`.
//!
//! - `open_beside` is `session.new_split` beside the origin's block,
//!   `open_tab_page` a `session.new_layer` at the tab page bounds, and
//!   `open_float` one at the float group's bounds with room for Rex's frame.
//!   None sends a `focus` key, so none moves Rex's focus. Each is done once
//!   its block is disowned, so Rex, not the daemon, sizes it.
//! - `close` is always `block.close`: a floating layer goes with its last
//!   block. `close_layer` only cleans up a `new_layer` answered with no block
//!   id, which then fails.
//! - An error `method_not_found` is unsupported, any other fails the act. Rex
//!   hanging up, failing, or sending what can't be read is `Gone`.
//!
//! Until #109–#111, the daemon also focuses and places blocks through
//! `session` itself, and reads Rex's view as a `LayoutSnapshot`.

use std::collections::VecDeque;

use serde_json::{Value, json};

use crate::daemon::floating_panes;
use crate::daemon::geometry::{REX_DEFAULT_SIZE, TAB_LAYER_BOUNDS};
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, CapabilitySet, Outcome, PaneProvider, ProviderEvent, TerminalId,
};
use crate::daemon::rex_layout;
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
];

pub(crate) struct RexProvider {
    /// The daemon still focuses and places through it directly (#109–#111).
    pub session: Session,
    /// The blocks opened for asks, each waiting for its disown's answer.
    opened: Vec<(u64, TerminalId)>,
    events: VecDeque<ProviderEvent>,
    /// Told `Gone`: nothing more comes.
    gone: bool,
}

impl RexProvider {
    pub(crate) fn new(session: Session) -> RexProvider {
        RexProvider {
            session,
            opened: Vec::new(),
            events: VecDeque::new(),
            gone: false,
        }
    }

    /// Asks Rex to focus terminal `t`, unless it already has or will.
    pub(crate) fn focus(&mut self, t: Option<&TerminalId>) -> Result<(), client::ClientError> {
        self.session.focus(t.and_then(block_of))
    }

    /// Whether Rex has focused terminal `t`, or will once asked.
    pub(crate) fn has_focused(&self, t: Option<&TerminalId>) -> bool {
        self.session.has_focused(t.and_then(block_of))
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
            Ask::OpenTabPage {
                attach, cols, rows, ..
            } => {
                let layout = json!({ "block": block(&attach, Some([cols, rows])) });
                self.session.new_layer(tag, TAB_LAYER_BOUNDS, layout)?;
            }
            Ask::OpenFloat {
                attach,
                x,
                y,
                cols,
                rows,
                origin_cols,
                origin_rows,
                ..
            } => {
                let Some(bounds) =
                    floating_panes::bounds_around(x, y, cols, rows, origin_cols, origin_rows)
                else {
                    self.answer(tag, Outcome::Failed("no room for a floating pane".into()));
                    return Ok(true);
                };
                let layout = json!({ "block": block(&attach, None) });
                self.session.new_layer(tag, bounds, layout)?;
            }
            Ask::Close { terminal } => self.session.close_block(tag, terminal.as_str())?,
            _ => return Ok(false),
        }
        Ok(true)
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
            Update::Layout(wins) => self
                .events
                .push_back(ProviderEvent::Layout(rex_layout::read(&wins))),
            Update::FocusMoved(block) => self
                .events
                .push_back(ProviderEvent::Focused(TerminalId::from(block.as_str()))),
            Update::Closed => self.go(),
        }
        Ok(())
    }

    /// Rex answered a request made for an ask.
    fn on_answer(
        &mut self,
        req: Request,
        result: ResponseResult,
    ) -> Result<(), client::ClientError> {
        match (req, result) {
            (Request::NewSplit(tag) | Request::NewLayer(tag), ResponseResult::Ok(ok)) => {
                if let Some(block) = session::new_block_id(&ok) {
                    // done once Rex, not us, sizes it
                    self.session.disown(tag, block)?;
                    self.opened.push((tag, TerminalId::from(block)));
                    return Ok(());
                }
                session::log_unexpected_response(req, "block_ids");
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
            (Request::CloseBlock(tag), ResponseResult::Ok(_)) => {
                self.answer(tag, Outcome::Done(Answer::Nothing))
            }
            (
                Request::NewSplit(tag) | Request::NewLayer(tag) | Request::CloseBlock(tag),
                ResponseResult::Err(e),
            ) => self.answer(tag, outcome_of_error(&e)),
            _ => {}
        }
        Ok(())
    }

    /// Rex is gone: tells it once; nothing asked will be answered.
    fn go(&mut self) {
        if self.gone {
            return;
        }
        self.gone = true;
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
        match self.send(id.0, ask) {
            Ok(true) => {}
            // not built in
            Ok(false) => self.answer(id.0, Outcome::Unsupported),
            Err(_) => self.go(),
        }
    }

    fn on_ready(&mut self, _token: usize) {
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
            match self.session.next() {
                Ok(Some(u)) => {
                    if self.on_update(u).is_err() {
                        self.go();
                    }
                }
                Ok(None) => return None,
                Err(_) => self.go(),
            }
        }
    }

    fn unlaid_size(&self) -> Option<[u32; 2]> {
        Some(REX_DEFAULT_SIZE)
    }

    fn context_vars(&self) -> &[&str] {
        &["REX_SERVER", "REX_SESSION"]
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
