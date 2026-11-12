//! Our side of a Rex session, on the wire: request builders, the requests
//! in flight matched to their answers by id, and the session view, fetched
//! one at a time with any change meanwhile fetched again once. What the
//! answers and views mean is for whoever drives it: Rex's pane provider.
//! It knows nothing of who connects: the client name is given.

use serde_json::{Value, json};

use super::client::{self, Client, ClientError};
use super::web_socket::Close;
use crate::log::{log_info, log_warn};

/// What a request asked for; tags match Rex's methods. A request made for
/// someone else's purpose carries their number for it, and its answer comes
/// back to them as `Update::Answered`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Request {
    Attach,
    NewSplit(u64),
    NewLayer(u64),
    FocusBlock(u64),
    FocusDirection(u64),
    View,
    DisownBlock(u64),
    CloseBlock(u64),
    CloseLayer(u64),
    MoveBlock(u64),
    ResizePane(u64),
    /// `com.superlogical.terminal.process`, for a `find`.
    Process(u64),
}

impl Request {
    /// The variant's name, as logged.
    pub(crate) fn tag_name(&self) -> &'static str {
        match self {
            Request::Attach => "attach",
            Request::NewSplit(_) => "new_split",
            Request::NewLayer(_) => "new_layer",
            Request::FocusBlock(_) => "focus_block",
            Request::FocusDirection(_) => "focus_direction",
            Request::View => "view",
            Request::DisownBlock(_) => "disown_block",
            Request::CloseBlock(_) => "close_block",
            Request::CloseLayer(_) => "close_layer",
            Request::MoveBlock(_) => "move_block",
            Request::ResizePane(_) => "resize_pane",
            Request::Process(_) => "terminal.process",
        }
    }
}

/// Sent, not answered yet.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pending {
    pub id: u64,
    pub req: Request,
}

/// Nothing is sent once closed.
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

pub(crate) struct Session {
    /// (Owned: its driver reaches it as `session.client`, e.g. for
    /// `client.fd()`.)
    pub client: Client,
    pub session_id: String,
    /// Who we are to Rex, as our log lines say.
    pub client_name: String,

    pub pending_requests: Vec<Pending>,

    pub conn: Conn,

    pub view: View,

    /// Why Rex closed, once it has; told once as `Update::Closed`.
    pub closed: Option<String>,
    closed_told: bool,
}

/// What the driver has to act on. (Owned, so it outlives the next call to
/// `next`.)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Update {
    /// Rex answered a request that carries its asker's number.
    Answered {
        req: Request,
        result: client::ResponseResult,
    },
    /// A view newer than any read before.
    View(Value),
    /// Rex closed or failed, for the reason in `closed`: nothing more will be
    /// answered.
    Closed,
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
    /// A session that is attaching, as the client `client_name`; `attach`
    /// joins it.
    pub(crate) fn new(client: Client, session_id: String, client_name: &str) -> Session {
        Session {
            client,
            session_id,
            client_name: client_name.to_string(),
            pending_requests: Vec::new(),
            conn: Conn::Attaching,
            view: View::default(),
            closed: None,
            closed_told: false,
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

    /// Sends what the socket takes of what waits to go to Rex. If that
    /// failed, logs it and closes Rex.
    pub(crate) fn flush(&mut self) {
        if let Err(e) = self.client.flush() {
            self.close_rex(&format!("failed: {e}"));
        }
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

    /// Any Rex error closes Rex.
    fn close_rex(&mut self, reason: &str) {
        log_warn!("{}: Rex {reason}, treating it as closed", self.client_name);
        self.conn = Conn::Closed;
        self.closed.get_or_insert_with(|| format!("Rex {reason}"));
    }

    /// The next update for the driver, or None when there are none.
    #[allow(clippy::should_implement_trait)]
    pub(crate) fn next(&mut self) -> Result<Option<Update>, ClientError> {
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
        if self.conn == Conn::Closed && !self.closed_told {
            self.closed_told = true;
            self.pending_requests.clear();
            return Ok(Some(Update::Closed));
        }
        Ok(None)
    }

    /// Asks Rex to focus `block`, switching to its Rex window and tab.
    pub(crate) fn focus_block(&mut self, tag: u64, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::FocusBlock(tag),
            "session.focus_block",
            json!({ "session_id": self.session_id, "block_id": block }),
        )
    }

    /// Asks Rex to focus the nearest block `dir` of the focused one. At an edge
    /// nothing moves. The new focus comes back in the view, as the user's does.
    pub(crate) fn focus_direction(&mut self, tag: u64, dir: Direction) -> Result<(), ClientError> {
        // no wrap: Rex wraps by default on some versions, and an edge must stay put
        self.request(
            Request::FocusDirection(tag),
            "session.focus_direction",
            json!({
                "session_id": self.session_id,
                "direction": dir.tag_name(),
                "wrap": false,
            }),
        )
    }

    /// Asks for a split beside `anchor`, holding `layout`: to its left or
    /// right if `side_by_side`, else above or below it; before it if `before`.
    /// It never takes focus.
    pub(crate) fn new_split(
        &mut self,
        tag: u64,
        anchor: &str,
        side_by_side: bool,
        before: bool,
        layout: Value,
    ) -> Result<(), ClientError> {
        self.request(
            Request::NewSplit(tag),
            "session.new_split",
            json!({
                "session_id": self.session_id,
                "anchor_id": anchor,
                "direction": if side_by_side { "horizontal" } else { "vertical" },
                "side": if before { "before" } else { "after" },
                "layout": layout,
            }),
        )
    }

    /// The creator owns a block and sizes it; this lets the app size it instead.
    pub(crate) fn disown(&mut self, tag: u64, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::DisownBlock(tag),
            "session.disown_block",
            json!({ "session_id": self.session_id, "block_ids": [block] }),
        )
    }

    /// Closes a block we made. Its helper, if it ever connects, finds no pane and leaves.
    pub(crate) fn close_block(&mut self, tag: u64, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::CloseBlock(tag),
            "block.close",
            json!({ "session_id": self.session_id, "block_id": block }),
        )
    }

    /// Moves `block` beside `anchor`: right of it, or below if not
    /// `side_by_side`. `anchor` is left the fraction `ratio` of the room they
    /// share.
    pub(crate) fn move_block(
        &mut self,
        tag: u64,
        block: &str,
        anchor: &str,
        side_by_side: bool,
        ratio: f64,
    ) -> Result<(), ClientError> {
        self.request(
            Request::MoveBlock(tag),
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
        tag: u64,
        before: &str,
        after: &str,
        side_by_side: bool,
        ratio: f64,
    ) -> Result<(), ClientError> {
        self.request(
            Request::ResizePane(tag),
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

    /// Asks for the processes of `block`'s terminal.
    pub(crate) fn process(&mut self, tag: u64, block: &str) -> Result<(), ClientError> {
        self.request(
            Request::Process(tag),
            "com.superlogical.terminal.process",
            json!({ "session_id": self.session_id, "block_id": block }),
        )
    }

    /// Closes a floating layer we made.
    pub(crate) fn close_layer(&mut self, tag: u64, layer: &str) -> Result<(), ClientError> {
        self.request(
            Request::CloseLayer(tag),
            "session.close_layer",
            json!({ "session_id": self.session_id, "layer_id": layer }),
        )
    }

    /// Asks for a floating layer at `bounds` in Rex window `window`, Rex's
    /// active one if None, holding `layout`.
    pub(crate) fn new_layer(
        &mut self,
        tag: u64,
        window: Option<&str>,
        bounds: [f64; 4],
        layout: Value,
    ) -> Result<(), ClientError> {
        let mut payload = serde_json::Map::new();
        payload.insert("session_id".into(), Value::String(self.session_id.clone()));
        if let Some(w) = window {
            payload.insert("window_id".into(), Value::String(w.to_string()));
        }
        payload.insert(
            "bounds".into(),
            Value::Array(bounds.iter().map(|&b| json_f64(b)).collect()),
        );
        payload.insert("layout".into(), layout);
        self.request(
            Request::NewLayer(tag),
            "session.new_layer",
            Value::Object(payload),
        )
    }

    /// Sends a request, and adds it to the requests in flight.
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
        // a `find`'s answer is its verdict, logged as one
        if let client::ResponseResult::Err(e) = &res.result
            && !matches!(req, Request::Process(_))
        {
            self.log_request_error(req, e);
        }
        match req {
            Request::View => self.on_view_result(&res.result),
            Request::Attach => self.on_attach_result(&res.result),
            _ => Ok(Some(Update::Answered {
                req,
                result: res.result,
            })),
        }
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
        let reason = match c.code {
            Some(code) => format!("Rex closed {code} {}", String::from_utf8_lossy(&c.reason)),
            None => format!("Rex closed null {}", String::from_utf8_lossy(&c.reason)),
        };
        log_info!("{}: {reason}", self.client_name);
        self.closed.get_or_insert(reason);
        self.conn = Conn::Closed;
    }

    /// Removes and returns the request `id` answers.
    pub(crate) fn take_pending_request(&mut self, id: u64) -> Option<Request> {
        let i = self.pending_requests.iter().position(|p| p.id == id)?;
        Some(self.pending_requests.swap_remove(i).req)
    }

    /// Whether a request for `req` is in flight.
    pub(crate) fn asking(&self, req: impl Fn(Request) -> bool) -> bool {
        self.pending_requests.iter().any(|p| req(p.req))
    }

    /// Attached: the answer's view is the first, or one is fetched. Refused,
    /// Rex is closed.
    fn on_attach_result(
        &mut self,
        result: &client::ResponseResult,
    ) -> Result<Option<Update>, ClientError> {
        match result {
            client::ResponseResult::Ok(ok) => {
                self.conn = Conn::Attached;
                if let Some(view) = client::get(ok, "view") {
                    return Ok(self.newer_view(view));
                }
                self.fetch_view()?;
            }
            client::ResponseResult::Err(e) => {
                let why = format!("Rex refused session.attach: {}: {}", e.code, e.message);
                self.closed.get_or_insert(why);
                self.conn = Conn::Closed;
            }
        }
        Ok(None)
    }

    /// Tells the view, then fetches again if it was already out of date.
    fn on_view_result(
        &mut self,
        result: &client::ResponseResult,
    ) -> Result<Option<Update>, ClientError> {
        let stale = self.view.fetch == Fetch::Stale;
        self.view.fetch = Fetch::Idle;
        let update = match result {
            client::ResponseResult::Ok(val) => self.newer_view(val),
            client::ResponseResult::Err(_) => None,
        };
        if stale {
            self.fetch_view()?;
        }
        Ok(update)
    }

    /// Fetches the session view, or another after the one on its way.
    pub(crate) fn fetch_view(&mut self) -> Result<(), ClientError> {
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

    /// `snapshot`, if it's newer than every view read before.
    fn newer_view(&mut self, snapshot: &Value) -> Option<Update> {
        let rev = client::get_int(snapshot, "revision")?;
        if rev <= self.view.rev {
            return None;
        }
        self.view.rev = rev;
        Some(Update::View(snapshot.clone()))
    }

    /// Rex refused a request.
    fn log_request_error(&self, req: Request, e: &client::Error) {
        log_warn!(
            "{}: Rex {} failed: {}: {}",
            self.client_name,
            req.tag_name(),
            e.code,
            e.message
        );
    }

    /// Rex said yes, in a shape we can't read.
    pub(crate) fn log_unexpected_response(&self, req: Request, what: &str) {
        log_warn!(
            "{}: Rex {} answer has no {what}",
            self.client_name,
            req.tag_name()
        );
    }
}

/// The block a split or layer made.
pub(crate) fn new_block_id(ok: &Value) -> Option<&str> {
    let Value::Array(block_ids) = client::get(ok, "block_ids")? else {
        return None;
    };
    client::str(block_ids.first())
}

// These check the JSON written to Rex, matching answers and fetching views.
// Rex's pane provider's tests reuse the helpers.
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;
    use std::os::unix::net::UnixStream;

    /// The text frames the client wrote to `theirs`, unmasked.
    pub(crate) fn sent(mut theirs: UnixStream) -> Vec<String> {
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

    /// The text frame a Rex server sends: unmasked.
    pub(crate) fn server_frame(text: &str) -> Vec<u8> {
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

    pub(crate) fn session() -> (Session, UnixStream) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut s = Session::new(Client::from_stream(ours), "session:s".to_string(), "test");
        s.conn = Conn::Attached;
        (s, theirs)
    }

    #[test]
    fn requests_as_json() {
        let (mut s, theirs) = session();
        s.attach().unwrap();
        s.focus_direction(0, Direction::Left).unwrap();
        s.move_block(0, "block:a", "block:b", true, 0.5).unwrap();
        s.resize_pane(0, "block:a", "block:b", false, 1.0).unwrap();
        s.disown(0, "block:a").unwrap();
        s.new_layer(3, None, [1.0, 2.5, 80.0, 24.0], json!({ "kind": "leaf" }))
            .unwrap();
        s.new_layer(4, Some("window:w"), [0.0, 0.0, 1.0, 1.0], json!(null))
            .unwrap();
        s.focus_block(5, "block:a").unwrap();
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
        assert!(payload(6).contains(r#""window_id":"window:w""#));
        assert_eq!(
            r#"{"session_id":"session:s","block_id":"block:a"}"#,
            payload(7)
        );
        assert_eq!(8, wire.len());
    }

    #[test]
    fn only_a_view_newer_than_the_last_is_told() {
        let (mut s, _theirs) = session();
        let view = |rev: i64| json!({ "revision": rev, "windows": [] });
        assert_eq!(Some(Update::View(view(1))), s.newer_view(&view(1)));
        assert_eq!(None, s.newer_view(&view(1)));
        assert_eq!(None, s.newer_view(&view(0)));
        assert_eq!(Some(Update::View(view(2))), s.newer_view(&view(2)));
    }

    pub(crate) fn ok(v: Value) -> client::ResponseResult {
        client::ResponseResult::Ok(v)
    }

    /// The ids of the requests still waiting for `kind`, oldest first.
    pub(crate) fn pending(s: &Session, kind: Request) -> Vec<u64> {
        s.pending_requests
            .iter()
            .filter(|p| p.req == kind)
            .map(|p| p.id)
            .collect()
    }

    pub(crate) fn answer(
        s: &mut Session,
        id: u64,
        result: client::ResponseResult,
    ) -> Option<Update> {
        s.on_response(client::Response { id, seq: 0, result })
            .unwrap()
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
    fn an_answer_to_a_request_made_for_someone_comes_back_to_them() {
        let (mut s, _theirs) = session();
        s.new_split(7, "block:a", true, false, json!({})).unwrap();
        let id = pending(&s, Request::NewSplit(7))[0];
        assert_eq!(
            Some(Update::Answered {
                req: Request::NewSplit(7),
                result: ok(json!({ "block_ids": ["block:x"] })),
            }),
            answer(&mut s, id, ok(json!({ "block_ids": ["block:x"] })))
        );
    }

    #[test]
    fn rex_closing_is_told_once_and_drops_what_was_pending() {
        let (mut s, _theirs) = session();
        s.new_split(1, "block:a", true, false, json!({})).unwrap();
        s.on_closed(&Close {
            code: Some(1000),
            reason: Vec::new(),
        });
        assert_eq!(Some(Update::Closed), s.next().unwrap());
        assert_eq!(Some("Rex closed 1000 "), s.closed.as_deref());
        assert!(s.pending_requests.is_empty());
        assert_eq!(None, s.next().unwrap());
    }
}
