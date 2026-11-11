//! Our side of the Rex session: requests in flight, the session view,
//! and which block Rex has focused.

use serde_json::{Value, json};

use super::client::{self, Client, ClientError};
use super::ids;
use super::web_socket::Close;
use crate::name::NAME;

/// What a request asked for; tags match Rex's methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    Attach,
    /// pane
    NewSplit(u32),
    /// pane
    NewLayer(u32),
    FocusBlock,
    FocusDirection,
    View,
    DisownBlock,
    CloseBlock,
    CloseLayer,
    MoveBlock,
    ResizePane,
}

impl Request {
    /// The variant's name, as logged.
    pub(crate) fn tag_name(&self) -> &'static str {
        match self {
            Request::Attach => "attach",
            Request::NewSplit(_) => "new_split",
            Request::NewLayer(_) => "new_layer",
            Request::FocusBlock => "focus_block",
            Request::FocusDirection => "focus_direction",
            Request::View => "view",
            Request::DisownBlock => "disown_block",
            Request::CloseBlock => "close_block",
            Request::CloseLayer => "close_layer",
            Request::MoveBlock => "move_block",
            Request::ResizePane => "resize_pane",
        }
    }
}

/// Sent, not answered yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pending {
    pub id: u64,
    pub req: Request,
}

/// Nothing is sent once closed; windows keep working without Rex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Conn {
    Attaching,
    Attached,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fetch {
    Idle,
    Running,
    Stale,
}

/// The newest view read, and whether one is on its way.
#[derive(Debug, Clone, Copy)]
pub(crate) struct View {
    pub rev: i64,
    pub fetch: Fetch,
}

impl Default for View {
    fn default() -> Self {
        View {
            rev: -1,
            fetch: Fetch::Idle,
        }
    }
}

/// The block Rex has focused, and our change to it still landing.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FocusedBlock {
    Unknown,
    Settled(ids::Block),
    Requesting {
        block: ids::Block,
        in_flight: u32,
        applied_rev: i64,
    },
}

pub(crate) struct Session {
    /// (Owned: the daemon reaches it as `session.client`, e.g. for
    /// `client.fd()`.)
    pub client: Client,
    pub session_id: String,

    pub pending_requests: Vec<Pending>,

    pub conn: Conn,

    pub view: View,

    pub focused_block: FocusedBlock,

    /// The Rex window our panes are in, as the daemon last found in a view.
    pub window: Option<[u8; 43]>,

    /// A focus change from the same view as a `layout` update, sent after it.
    pub queued_focus: Option<String>,
}

/// What the daemon has to act on. (Owned, so it outlives the next call to
/// `next`.)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    /// The split asked for `pane` exists, as `block`.
    SplitMade { pane: u32, block: String },
    /// Rex refused the split asked for this pane.
    SplitFailed(Failed),
    /// The floating layer asked for `pane` exists, as `layer` holding `block`.
    LayerMade {
        pane: u32,
        block: String,
        layer: Option<String>,
    },
    /// Rex refused the floating layer asked for this pane.
    LayerFailed(Failed),
    /// A newer view's windows, to read where our panes sit.
    Layout(Vec<Value>),
    /// The user focused this block in Rex.
    FocusMoved(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Failed {
    pub pane: u32,
}

/// Which way `focus_direction` moves; Rex's names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub(crate) fn tag_name(&self) -> &'static str {
        match self {
            Direction::Left => "left",
            Direction::Right => "right",
            Direction::Up => "up",
            Direction::Down => "down",
        }
    }
}

/// A block id as the JSON string Rex knows it by.
fn block_json(block: &ids::Block) -> Value {
    Value::String(String::from_utf8_lossy(block).into_owned())
}

/// `v` as JSON: a whole number without a fraction (`1`, not `1.0`), so Rex
/// reads it the same wherever it wants an integer.
fn json_f64(v: f64) -> Value {
    if !v.is_finite() {
        // JSON has no non-finite numbers: write them as strings
        return Value::String(if v.is_nan() {
            "nan".into()
        } else {
            format!("{v}")
        });
    }
    if v.fract() == 0.0 && v.abs() < (1u64 << 53) as f64 && !(v == 0.0 && v.is_sign_negative()) {
        return Value::from(v as i64);
    }
    serde_json::Number::from_f64(v).map_or(Value::Null, Value::Number)
}

impl Session {
    /// A session that is attaching; `attach` joins it.
    pub(crate) fn new(client: Client, session_id: String) -> Session {
        Session {
            client,
            session_id,
            pending_requests: Vec::new(),
            conn: Conn::Attaching,
            view: View::default(),
            focused_block: FocusedBlock::Unknown,
            window: None,
            queued_focus: None,
        }
    }

    /// Joins our Rex session, so we hear its events and can split it.
    pub(crate) fn attach(&mut self) -> Result<(), ClientError> {
        self.request(
            Request::Attach,
            "session.attach",
            json!({ "session_id": self.session_id }),
        )
    }

    /// One read from Rex. If it hung up or failed, logs it and closes Rex; then
    /// call `next` until None.
    pub(crate) fn read(&mut self) {
        match self.client.read() {
            Ok(true) => {}
            Ok(false) => self.close_rex("hung up"),
            Err(e) => self.close_rex(&format!("failed: {e}")),
        }
    }

    /// Any Rex error closes Rex: the windows keep working without it.
    fn close_rex(&mut self, reason: &str) {
        eprintln!("{NAME} daemon: Rex {reason}, treating it as closed");
        self.conn = Conn::Closed;
    }

    /// The next update for the daemon, or None when there are none.
    #[allow(clippy::should_implement_trait)]
    pub(crate) fn next(&mut self) -> Result<Option<Update>, ClientError> {
        if let Some(block) = self.queued_focus.take() {
            return Ok(Some(Update::FocusMoved(block)));
        }
        loop {
            let message = match self.client.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    self.close_rex(&format!("failed: {e}"));
                    break;
                }
            };
            match message {
                client::Message::Response(r) => {
                    if let Some(update) = self.on_response(r)? {
                        return Ok(Some(update));
                    }
                }
                client::Message::Event(ev) => self.on_event(ev)?,
                client::Message::Closed(c) => self.on_closed(&c),
            }
        }
        if self.conn == Conn::Closed {
            return Ok(self.fail_pending_split());
        }
        Ok(None)
    }

    /// Asks Rex to focus `maybe_block`, unless it already has or will.
    pub(crate) fn focus(&mut self, maybe_block: Option<ids::Block>) -> Result<(), ClientError> {
        if self.conn == Conn::Closed {
            return Ok(());
        }
        let Some(block) = maybe_block else {
            return Ok(());
        };
        if self.has_focused(Some(block)) {
            return Ok(());
        }
        self.request(
            Request::FocusBlock,
            "session.focus_block",
            json!({
                "session_id": self.session_id,
                "block_id": block_json(&block),
            }),
        )?;
        match &mut self.focused_block {
            FocusedBlock::Requesting {
                block: pending_block,
                in_flight,
                ..
            } => {
                *pending_block = block;
                *in_flight += 1;
            }
            other => {
                *other = FocusedBlock::Requesting {
                    block,
                    in_flight: 1,
                    applied_rev: 0,
                }
            }
        }
        Ok(())
    }

    /// Asks Rex to focus the nearest block `dir` of the focused one. At an edge
    /// nothing moves. The new focus comes back in the view, as the user's does.
    pub(crate) fn focus_direction(&mut self, dir: Direction) -> Result<(), ClientError> {
        // no wrap: Rex wraps by default on some versions, and an edge must stay put
        self.request(
            Request::FocusDirection,
            "session.focus_direction",
            json!({
                "session_id": self.session_id,
                "direction": dir.tag_name(),
                "wrap": false,
            }),
        )
    }

    /// Rex has focused `maybe_block`, or will once our request lands.
    pub(crate) fn has_focused(&self, maybe_block: Option<ids::Block>) -> bool {
        let Some(focused_block) = self.current_focus() else {
            return false;
        };
        let Some(block) = maybe_block else {
            return false;
        };
        ids::same_block(focused_block, block)
    }

    /// The creator owns a block and sizes it; this lets the app size it instead.
    pub(crate) fn disown(&mut self, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::DisownBlock,
            "session.disown_block",
            json!({ "session_id": self.session_id, "block_ids": [block] }),
        )
    }

    /// Closes a block we made. Its helper, if it ever connects, finds no pane and leaves.
    pub(crate) fn close_block(&mut self, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::CloseBlock,
            "block.close",
            json!({ "session_id": self.session_id, "block_id": block }),
        )
    }

    /// Moves `block` beside `anchor`, after it (right of it, or below if not `side_by_side`),
    /// leaving `anchor` the fraction `ratio` of the room they share.
    pub(crate) fn move_block(
        &mut self,
        block: &str,
        anchor: &str,
        side_by_side: bool,
        ratio: f64,
    ) -> Result<(), ClientError> {
        self.request(
            Request::MoveBlock,
            "session.move_block",
            json!({
                "session_id": self.session_id,
                "block_id": block,
                "anchor_block_id": anchor,
                "direction": if side_by_side { "horizontal" } else { "vertical" },
                "side": "after",
                "ratio": json_f64(ratio),
            }),
        )
    }

    /// Moves the border between `before` and `after`, which are left of it and right of it
    /// (above and below, if not `side_by_side`), so `before` gets the fraction `ratio` of the room.
    pub(crate) fn resize_pane(
        &mut self,
        before: &str,
        after: &str,
        side_by_side: bool,
        ratio: f64,
    ) -> Result<(), ClientError> {
        self.request(
            Request::ResizePane,
            "session.resize_pane",
            json!({
                "session_id": self.session_id,
                "before_block_id": before,
                "after_block_id": after,
                "direction": if side_by_side { "horizontal" } else { "vertical" },
                "ratio": json_f64(ratio),
            }),
        )
    }

    /// Closes a floating layer we made.
    pub(crate) fn close_layer(&mut self, layer: &str) -> Result<(), ClientError> {
        self.request(
            Request::CloseLayer,
            "session.close_layer",
            json!({ "session_id": self.session_id, "layer_id": layer }),
        )
    }

    /// Asks for a floating layer at `bounds` in our window, holding `layout`.
    /// (`layout` is a JSON value.)
    pub(crate) fn new_layer(
        &mut self,
        pane_id: u32,
        bounds: [f64; 4],
        layout: Value,
    ) -> Result<(), ClientError> {
        let mut payload = serde_json::Map::new();
        payload.insert("session_id".into(), Value::String(self.session_id.clone()));
        if let Some(w) = &self.window {
            payload.insert(
                "window_id".into(),
                Value::String(String::from_utf8_lossy(w).into_owned()),
            );
        }
        payload.insert(
            "bounds".into(),
            Value::Array(bounds.iter().map(|&b| json_f64(b)).collect()),
        );
        payload.insert("layout".into(), layout);
        self.request(
            Request::NewLayer(pane_id),
            "session.new_layer",
            Value::Object(payload),
        )
    }

    /// Send a request to the Rex client, add it to list of pending requests
    pub(crate) fn request(
        &mut self,
        req: Request,
        method: &str,
        payload: Value,
    ) -> Result<(), ClientError> {
        if self.conn == Conn::Closed {
            return Ok(());
        }
        match self.client.request(method, Some(&payload)) {
            Ok(id) => self.pending_requests.push(Pending { id, req }),
            Err(e) => {
                // no answer will come: stays pending so closing fails it
                self.pending_requests.push(Pending { id: 0, req });
                self.close_rex(&format!("failed: {e}"));
            }
        }
        Ok(())
    }

    /// Hands an answer to its request's handler.
    pub(crate) fn on_response(
        &mut self,
        res: client::Response,
    ) -> Result<Option<Update>, ClientError> {
        let Some(req) = self.take_pending_request(res.id) else {
            return Ok(None);
        };
        if let client::ResponseResult::Err(e) = &res.result {
            log_request_error(req, e);
        }
        match req {
            Request::NewSplit(pane) => return Ok(Some(on_new_split_result(pane, &res.result))),
            Request::NewLayer(pane) => return Ok(Some(on_new_layer_result(pane, &res.result))),
            Request::View => return self.on_view_result(&res.result),
            Request::FocusBlock => self.on_focus_block_result(&res.result)?,
            Request::Attach => self.on_attach_result(&res.result)?,
            Request::FocusDirection
            | Request::DisownBlock
            | Request::CloseBlock
            | Request::CloseLayer
            | Request::MoveBlock
            | Request::ResizePane => {}
        }
        Ok(None)
    }

    /// Only a newer session view matters.
    fn on_event(&mut self, event: client::Event) -> Result<(), ClientError> {
        if event.name != "session_view_changed" {
            return Ok(());
        }
        let Some(rev) = client::get_int(&event.payload, "revision") else {
            return Ok(());
        };
        if rev > self.view.rev {
            self.fetch_view()?;
        }
        Ok(())
    }

    fn on_closed(&mut self, c: &Close) {
        match c.code {
            Some(code) => eprintln!(
                "{NAME} daemon: Rex closed {code} {}",
                String::from_utf8_lossy(&c.reason)
            ),
            None => eprintln!(
                "{NAME} daemon: Rex closed null {}",
                String::from_utf8_lossy(&c.reason)
            ),
        }
        self.conn = Conn::Closed;
    }

    /// Closed: no answers will come, so fail the splits still waiting.
    fn fail_pending_split(&mut self) -> Option<Update> {
        while let Some(p) = self.pending_requests.pop() {
            match p.req {
                Request::NewSplit(pane) => return Some(Update::SplitFailed(Failed { pane })),
                Request::NewLayer(pane) => return Some(Update::LayerFailed(Failed { pane })),
                _ => {}
            }
        }
        None
    }

    /// Removes and returns the request `id` answers.
    pub(crate) fn take_pending_request(&mut self, id: u64) -> Option<Request> {
        let i = self.pending_requests.iter().position(|p| p.id == id)?;
        Some(self.pending_requests.swap_remove(i).req)
    }

    /// Attached: a view gives the starting focus.
    fn on_attach_result(&mut self, result: &client::ResponseResult) -> Result<(), ClientError> {
        match result {
            client::ResponseResult::Ok(_) => {
                self.conn = Conn::Attached;
                self.fetch_view()?;
            }
            client::ResponseResult::Err(_) => self.conn = Conn::Closed,
        }
        Ok(())
    }

    /// Records the revision our focus change landed at.
    fn on_focus_block_result(
        &mut self,
        result: &client::ResponseResult,
    ) -> Result<(), ClientError> {
        let FocusedBlock::Requesting {
            in_flight,
            applied_rev,
            ..
        } = &mut self.focused_block
        else {
            return Ok(());
        };
        *in_flight -= 1;
        match result {
            client::ResponseResult::Ok(val) => {
                let rev = client::get_int(val, "revision").unwrap_or(0);
                if rev > *applied_rev {
                    *applied_rev = rev;
                }
            }
            // refused => need to ask for view
            client::ResponseResult::Err(_) => {
                if *in_flight == 0 {
                    self.fetch_view()?;
                }
            }
        }
        Ok(())
    }

    /// Applies the view, then fetches again if it was already out of date.
    fn on_view_result(
        &mut self,
        result: &client::ResponseResult,
    ) -> Result<Option<Update>, ClientError> {
        let stale = self.view.fetch == Fetch::Stale;
        self.view.fetch = Fetch::Idle;
        let update = match result {
            client::ResponseResult::Ok(val) => self.apply_view(val),
            client::ResponseResult::Err(_) => None,
        };
        if stale {
            self.fetch_view()?;
        }
        Ok(update)
    }

    /// Fetches the session view, or another after the one on its way.
    fn fetch_view(&mut self) -> Result<(), ClientError> {
        match self.view.fetch {
            Fetch::Idle => {
                if self.conn == Conn::Closed {
                    return Ok(());
                }
                self.request(
                    Request::View,
                    "session.view",
                    json!({ "session_id": self.session_id }),
                )?;
                self.view.fetch = Fetch::Running;
            }
            // the answer will be out of date: fetch again when it arrives
            Fetch::Running => self.view.fetch = Fetch::Stale,
            Fetch::Stale => {}
        }
        Ok(())
    }

    /// Skips views no newer than the last one read. Layout comes first, then
    /// any focus change.
    fn apply_view(&mut self, snapshot: &Value) -> Option<Update> {
        let rev = client::get_int(snapshot, "revision")?;
        if rev <= self.view.rev {
            return None;
        }
        self.view.rev = rev;
        let focus_update = match find_focused_block(snapshot) {
            Some(block) => self.apply_focus(block, rev),
            None => {
                log_unexpected_response(Request::View, "focused block");
                None
            }
        };
        let Some(windows) = client::get(snapshot, "windows") else {
            return focus_update;
        };
        let Value::Array(items) = windows else {
            return focus_update;
        };
        if let Some(Update::FocusMoved(block)) = focus_update {
            self.queued_focus = Some(block);
        }
        Some(Update::Layout(items.clone()))
    }

    /// Reports focus the user moved; echoes of our own change are ignored.
    fn apply_focus(&mut self, block: ids::Block, rev: i64) -> Option<Update> {
        match self.focused_block {
            FocusedBlock::Requesting {
                block: pending_block,
                in_flight,
                applied_rev,
            } => {
                if in_flight > 0 || rev < applied_rev {
                    return None;
                }
                if pending_block == block {
                    self.focused_block = FocusedBlock::Settled(block);
                    return None;
                }
            }
            FocusedBlock::Settled(last_focused_block) => {
                if last_focused_block == block {
                    return None;
                }
            }
            FocusedBlock::Unknown => {}
        }

        let report = !matches!(self.focused_block, FocusedBlock::Unknown);
        self.focused_block = FocusedBlock::Settled(block);
        if report {
            Some(Update::FocusMoved(
                String::from_utf8_lossy(&block).into_owned(),
            ))
        } else {
            None
        }
    }

    /// The block Rex has focused, as we last saw or asked.
    fn current_focus(&self) -> Option<ids::Block> {
        match self.focused_block {
            FocusedBlock::Unknown => None,
            FocusedBlock::Settled(last_focused) => Some(last_focused),
            FocusedBlock::Requesting { block, .. } => Some(block),
        }
    }
}

/// Always an update: the pane is waiting either way.
pub(crate) fn on_new_split_result(pane_id: u32, result: &client::ResponseResult) -> Update {
    let ok = match result {
        client::ResponseResult::Ok(val) => val,
        client::ResponseResult::Err(_) => return Update::SplitFailed(Failed { pane: pane_id }),
    };
    let Some(block) = new_block_id(ok) else {
        log_unexpected_response(Request::NewSplit(pane_id), "block_ids");
        return Update::SplitFailed(Failed { pane: pane_id });
    };
    Update::SplitMade {
        pane: pane_id,
        block: block.to_string(),
    }
}

/// Always an update: the pane is waiting either way.
fn on_new_layer_result(pane_id: u32, result: &client::ResponseResult) -> Update {
    let ok = match result {
        client::ResponseResult::Ok(val) => val,
        client::ResponseResult::Err(_) => return Update::LayerFailed(Failed { pane: pane_id }),
    };
    let Some(block) = new_block_id(ok) else {
        log_unexpected_response(Request::NewLayer(pane_id), "block_ids");
        return Update::LayerFailed(Failed { pane: pane_id });
    };
    Update::LayerMade {
        pane: pane_id,
        block: block.to_string(),
        layer: client::get_str(ok, "layer_id").map(str::to_string),
    }
}

/// The block a split made.
fn new_block_id(ok: &Value) -> Option<&str> {
    let Value::Array(block_ids) = client::get(ok, "block_ids")? else {
        return None;
    };
    client::str(block_ids.first())
}

/// The focused block of the active window.
fn find_focused_block(snapshot: &Value) -> Option<ids::Block> {
    let active_id = client::get_str(snapshot, "active_window_id")?;
    let Value::Array(windows) = client::get(snapshot, "windows")? else {
        return None;
    };
    for win in windows {
        let Some(win_id) = client::get_str(win, "window_id") else {
            continue;
        };
        if win_id == active_id {
            let block = client::get_str(win, "focused_block_id")?;
            return ids::block(block.as_bytes()).ok();
        }
    }
    None
}

// Loggers

/// Rex refused a request.
pub(crate) fn log_request_error(req: Request, e: &client::Error) {
    match req {
        Request::NewSplit(pane) => eprintln!(
            "{NAME} daemon: Rex new_split for pane {pane} failed: {}: {}",
            e.code, e.message
        ),
        _ => eprintln!(
            "{NAME} daemon: Rex {} failed: {}: {}",
            req.tag_name(),
            e.code,
            e.message
        ),
    }
}

/// Rex said yes, in a shape we can't read.
fn log_unexpected_response(req: Request, what: &str) {
    eprintln!("{NAME} daemon: Rex {} answer has no {what}", req.tag_name());
}

// These check the JSON written to Rex and the view/focus flow.
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    /// The text frames the client wrote to `theirs`, unmasked.
    fn sent(mut theirs: UnixStream) -> Vec<String> {
        let mut bytes = Vec::new();
        theirs.read_to_end(&mut bytes).unwrap();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let opcode = bytes[i] & 0x0f;
            let (len, header) = match bytes[i + 1] & 0x7f {
                126 => (u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize, 4),
                n => (n as usize, 2),
            };
            let mask: [u8; 4] = bytes[i + header..i + header + 4].try_into().unwrap();
            let start = i + header + 4;
            let body: Vec<u8> = (0..len).map(|k| bytes[start + k] ^ mask[k % 4]).collect();
            i = start + len;
            if opcode == 1 {
                out.push(String::from_utf8(body).unwrap());
            }
        }
        out
    }

    fn session() -> (Session, UnixStream) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut s = Session::new(Client::from_stream(ours), "session:s".to_string());
        s.conn = Conn::Attached;
        (s, theirs)
    }

    fn block(c: char) -> ids::Block {
        let mut b = [c as u8; 42];
        b[..6].copy_from_slice(b"block:");
        b
    }

    #[test]
    fn requests_as_json() {
        let (mut s, theirs) = session();
        s.attach().unwrap();
        s.focus_direction(Direction::Left).unwrap();
        s.move_block("block:a", "block:b", true, 0.5).unwrap();
        s.resize_pane("block:a", "block:b", false, 1.0).unwrap();
        s.disown("block:a").unwrap();
        s.new_layer(3, [1.0, 2.5, 80.0, 24.0], json!({ "kind": "leaf" }))
            .unwrap();
        let mut window = [b'0'; 43];
        window[..7].copy_from_slice(b"window:");
        s.window = Some(window);
        s.new_layer(4, [0.0, 0.0, 1.0, 1.0], json!(null)).unwrap();
        s.focus(Some(block('a'))).unwrap();
        // already asked for
        s.focus(Some(block('a'))).unwrap();
        assert_eq!(8, s.pending_requests.len());
        drop(s);
        let wire = sent(theirs);
        let payload = |n: usize| {
            let v: Value = serde_json::from_str(&wire[n]).unwrap();
            serde_json::to_string(&v["payload"]).unwrap()
        };
        assert_eq!(r#"{"session_id":"session:s"}"#, payload(0));
        assert_eq!(
            r#"{"session_id":"session:s","direction":"left","wrap":false}"#,
            payload(1)
        );
        assert_eq!(
            r#"{"session_id":"session:s","block_id":"block:a","anchor_block_id":"block:b","direction":"horizontal","side":"after","ratio":0.5}"#,
            payload(2)
        );
        assert_eq!(
            r#"{"session_id":"session:s","before_block_id":"block:a","after_block_id":"block:b","direction":"vertical","ratio":1}"#,
            payload(3)
        );
        assert_eq!(
            r#"{"session_id":"session:s","block_ids":["block:a"]}"#,
            payload(4)
        );
        assert_eq!(
            r#"{"session_id":"session:s","bounds":[1,2.5,80,24],"layout":{"kind":"leaf"}}"#,
            payload(5)
        );
        assert!(payload(6).contains(r#""window_id":"window:"#));
        assert_eq!(8, wire.len());
    }

    #[test]
    fn split_and_view_results() {
        let (mut s, _theirs) = session();
        let ok = |v: Value| client::ResponseResult::Ok(v);
        assert_eq!(
            Update::SplitMade {
                pane: 1,
                block: "block:x".into()
            },
            on_new_split_result(1, &ok(json!({ "block_ids": ["block:x"] })))
        );
        assert_eq!(
            Update::SplitFailed(Failed { pane: 2 }),
            on_new_split_result(2, &ok(json!({})))
        );

        let (a, b) = (block('a'), block('b'));
        let view = |rev: i64, focused: &ids::Block| {
            json!({
                "revision": rev,
                "active_window_id": "window:w",
                "windows": [{ "window_id": "window:w",
                              "focused_block_id": String::from_utf8_lossy(focused) }],
            })
        };
        // the first view only learns the focus
        let Some(Update::Layout(w)) = s.apply_view(&view(1, &a)) else {
            panic!("expected a layout");
        };
        assert_eq!(1, w.len());
        assert!(s.has_focused(Some(a)));
        // an older view is skipped
        assert_eq!(None, s.apply_view(&view(1, &b)));
        // the user moved focus: layout, then the focus
        let Some(Update::Layout(_)) = s.apply_view(&view(2, &b)) else {
            panic!("expected a layout");
        };
        assert_eq!(
            Some(Update::FocusMoved(String::from_utf8_lossy(&b).into_owned())),
            s.next().unwrap()
        );
    }

    fn ok(v: Value) -> client::ResponseResult {
        client::ResponseResult::Ok(v)
    }

    fn refused() -> client::ResponseResult {
        client::ResponseResult::Err(client::Error {
            code: "refused".into(),
            message: "no".into(),
            status: 400,
        })
    }

    /// The ids of the requests still waiting for `kind`, oldest first.
    fn pending(s: &Session, kind: Request) -> Vec<u64> {
        s.pending_requests
            .iter()
            .filter(|p| p.req == kind)
            .map(|p| p.id)
            .collect()
    }

    fn answer(s: &mut Session, id: u64, result: client::ResponseResult) -> Option<Update> {
        s.on_response(client::Response { id, seq: 0, result })
            .unwrap()
    }

    /// Applies a view at `rev` focused on `focused`, and returns the focus
    /// change it reports, if any (the layout is dropped).
    fn view_focus(s: &mut Session, rev: i64, focused: &ids::Block) -> Option<Update> {
        let view = json!({
            "revision": rev,
            "active_window_id": "window:w",
            "windows": [{ "window_id": "window:w",
                          "focused_block_id": String::from_utf8_lossy(focused) }],
        });
        let Some(Update::Layout(_)) = s.apply_view(&view) else {
            panic!("expected a layout");
        };
        s.queued_focus.take().map(Update::FocusMoved)
    }

    fn view_changed(rev: i64) -> client::Event {
        client::Event {
            seq: 0,
            session_id: Some("session:s".into()),
            name: "session_view_changed".into(),
            payload: json!({ "revision": rev }),
        }
    }

    #[test]
    fn only_the_last_of_two_focus_requests_settles() {
        let (mut s, _theirs) = session();
        let (a, b) = (block('a'), block('b'));
        s.focus(Some(a)).unwrap();
        s.focus(Some(b)).unwrap();
        let ids = pending(&s, Request::FocusBlock);
        assert_eq!(2, ids.len());

        // the first echo lands while the second is still out
        answer(&mut s, ids[0], ok(json!({ "revision": 5 })));
        assert_eq!(None, view_focus(&mut s, 6, &a));
        assert!(matches!(
            s.focused_block,
            FocusedBlock::Requesting { in_flight: 1, .. }
        ));
        assert!(s.has_focused(Some(b)));

        // the second answers: its echo settles, and reports nothing
        answer(&mut s, ids[1], ok(json!({ "revision": 7 })));
        assert_eq!(None, view_focus(&mut s, 8, &b));
        assert!(matches!(s.focused_block, FocusedBlock::Settled(x) if x == b));
    }

    #[test]
    fn view_older_than_applied_revision_is_ignored() {
        let (mut s, _theirs) = session();
        let (a, b) = (block('a'), block('b'));
        s.focus(Some(a)).unwrap();
        let id = pending(&s, Request::FocusBlock)[0];
        answer(&mut s, id, ok(json!({ "revision": 10 })));

        // from before our change landed: not the user's move
        assert_eq!(None, view_focus(&mut s, 9, &b));
        assert!(matches!(
            s.focused_block,
            FocusedBlock::Requesting {
                applied_rev: 10,
                ..
            }
        ));
        assert!(s.has_focused(Some(a)));

        // at the applied revision, a different block is the user's move
        assert_eq!(
            Some(Update::FocusMoved(String::from_utf8_lossy(&b).into_owned())),
            view_focus(&mut s, 10, &b)
        );
        assert!(matches!(s.focused_block, FocusedBlock::Settled(x) if x == b));
    }

    #[test]
    fn refused_focus_fetches_view_only_when_nothing_is_in_flight() {
        let (mut s, _theirs) = session();
        s.focus(Some(block('a'))).unwrap();
        s.focus(Some(block('b'))).unwrap();
        let ids = pending(&s, Request::FocusBlock);

        answer(&mut s, ids[0], refused());
        assert!(pending(&s, Request::View).is_empty());
        assert_eq!(Fetch::Idle, s.view.fetch);

        answer(&mut s, ids[1], refused());
        assert_eq!(1, pending(&s, Request::View).len());
        assert_eq!(Fetch::Running, s.view.fetch);
    }

    #[test]
    fn event_during_view_fetch_refetches_once() {
        let (mut s, _theirs) = session();
        s.on_event(view_changed(1)).unwrap();
        assert_eq!(Fetch::Running, s.view.fetch);
        let first = pending(&s, Request::View);
        assert_eq!(1, first.len());

        // while it runs: marked stale, no second request yet
        s.on_event(view_changed(2)).unwrap();
        s.on_event(view_changed(3)).unwrap();
        assert_eq!(Fetch::Stale, s.view.fetch);
        assert_eq!(1, pending(&s, Request::View).len());

        // its answer triggers exactly one more fetch
        answer(&mut s, first[0], ok(json!({ "revision": 1 })));
        assert_eq!(Fetch::Running, s.view.fetch);
        let second = pending(&s, Request::View);
        assert_eq!(1, second.len());
        assert_ne!(first[0], second[0]);

        answer(&mut s, second[0], ok(json!({ "revision": 3 })));
        assert_eq!(Fetch::Idle, s.view.fetch);
        assert!(pending(&s, Request::View).is_empty());

        // an event no newer than the view read does not fetch
        s.on_event(view_changed(3)).unwrap();
        assert_eq!(Fetch::Idle, s.view.fetch);
    }

    #[test]
    fn repeated_focus_requests_to_one_block_are_sent_once() {
        let (mut s, _theirs) = session();
        let (a, b) = (block('a'), block('b'));
        s.focus(Some(a)).unwrap();
        s.focus(Some(a)).unwrap();
        assert_eq!(1, pending(&s, Request::FocusBlock).len());
        assert!(matches!(
            s.focused_block,
            FocusedBlock::Requesting { in_flight: 1, .. }
        ));

        // a different block is a new request; asking again for it is not
        s.focus(Some(b)).unwrap();
        s.focus(Some(b)).unwrap();
        assert_eq!(2, pending(&s, Request::FocusBlock).len());
        assert!(matches!(
            s.focused_block,
            FocusedBlock::Requesting { in_flight: 2, .. }
        ));

        // once settled, asking for the focused block sends nothing
        for id in pending(&s, Request::FocusBlock) {
            answer(&mut s, id, ok(json!({ "revision": 1 })));
        }
        view_focus(&mut s, 2, &b);
        s.focus(Some(b)).unwrap();
        assert!(pending(&s, Request::FocusBlock).is_empty());
    }

    #[test]
    fn rex_closing_fails_each_pending_split_and_layer_once() {
        let (mut s, _theirs) = session();
        s.request(Request::NewSplit(1), "session.new_split", json!({}))
            .unwrap();
        s.request(Request::NewLayer(2), "session.new_layer", json!({}))
            .unwrap();
        s.request(Request::View, "session.view", json!({})).unwrap();
        s.request(Request::NewSplit(3), "session.new_split", json!({}))
            .unwrap();
        s.on_closed(&Close {
            code: Some(1000),
            reason: Vec::new(),
        });

        let mut failures = Vec::new();
        while let Some(update) = s.next().unwrap() {
            failures.push(update);
        }
        let has = |u: Update| failures.iter().filter(|f| **f == u).count();
        assert_eq!(3, failures.len());
        assert_eq!(1, has(Update::SplitFailed(Failed { pane: 1 })));
        assert_eq!(1, has(Update::LayerFailed(Failed { pane: 2 })));
        assert_eq!(1, has(Update::SplitFailed(Failed { pane: 3 })));
        assert_eq!(None, s.next().unwrap());
    }
}
