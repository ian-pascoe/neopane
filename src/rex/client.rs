//! A Rex control connection: a WebSocket on the server's Unix socket
//! carrying JSON requests, responses and events. The caller's loop reads;
//! see `read` and `next`.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::{Value, json};

use super::web_socket::{self, Close, Opcode, WebSocket};

pub(crate) const PROTOCOL: &str = "rex.control.v1";
pub(crate) const API_REVISION: i64 = 7;

/// What can go wrong on the connection.
#[derive(Debug)]
pub(crate) enum ClientError {
    NotUnixUrl,
    NameTooLong,
    ResponseTooLarge,
    ConnectionClosed,
    NotWelcome,
    BadProtocol,
    BadMessage,
    MessageTooLarge,
    /// A handshake or framing error.
    WebSocket(web_socket::Error),
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::WebSocket(e) => write!(f, "{e}"),
            ClientError::Json(e) => write!(f, "{e}"),
            ClientError::Io(e) => write!(f, "{e}"),
            other => write!(f, "{other:?}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<web_socket::Error> for ClientError {
    fn from(e: web_socket::Error) -> Self {
        ClientError::WebSocket(e)
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        ClientError::Json(e)
    }
}

impl From<std::io::Error> for ClientError {
    fn from(e: std::io::Error) -> Self {
        ClientError::Io(e)
    }
}

/// A Rex control connection.
pub(crate) struct Client {
    pub stream: UnixStream,
    pub ws: WebSocket,
    /// Request ids are this counter as strings.
    pub next_id: u64,
    /// The server's `limits.max_message_bytes`.
    pub max_message: usize,
    /// Scratch for building outgoing JSON.
    pub out: Vec<u8>,
    /// Scratch for building outgoing frames.
    pub frame: Vec<u8>,
    /// Upgraded, and no close frame sent yet.
    pub open: bool,
}

/// A message from the server. (Owned, not slices of the parsed JSON, so the
/// caller can keep using the client while it handles one.)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Message {
    Response(Response),
    Event(Event),
    /// The server closed the connection.
    Closed(Close),
}

/// A response's outcome: the value, or the error. (Not named `Result`, so it
/// doesn't hide `std::result::Result`.)
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ResponseResult {
    Ok(Value),
    Err(Error),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Response {
    pub id: u64,
    pub seq: i64,
    pub result: ResponseResult,
}

/// Rex's answer to a request that it refused.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Error {
    pub code: String,
    pub message: String,
    pub status: i64,
}

/// A server event, or a session event unwrapped from `session_event`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Event {
    pub seq: i64,
    /// Set for session events.
    pub session_id: Option<String>,
    pub name: String,
    /// `Value::Null` when the event has no fields.
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Welcome {
    pub max_message_bytes: usize,
    pub api_revision: i64,
}

/// One read from a socket, retrying when a signal interrupts it.
fn read_fd(mut stream: &UnixStream, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match stream.read(buf) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

/// Writes one frame to the server, building it in `frame`.
fn send_frame(
    mut stream: &UnixStream,
    frame: &mut Vec<u8>,
    opcode: Opcode,
    payload: &[u8],
) -> std::io::Result<()> {
    frame.clear();
    web_socket::write_frame(frame, opcode, payload);
    stream.write_all(frame)
}

/// Sends a close frame with `code`.
fn send_close(mut stream: &UnixStream, frame: &mut Vec<u8>, code: u16) -> std::io::Result<()> {
    frame.clear();
    web_socket::write_close(frame, code);
    stream.write_all(frame)
}

impl Client {
    /// A client on a stream that has already been upgraded, welcomed
    /// with the default limits. (For tests, which talk over a socket pair.)
    pub(crate) fn from_stream(stream: UnixStream) -> Client {
        Client {
            stream,
            ws: WebSocket::default(),
            next_id: 1,
            max_message: 65536,
            out: Vec::new(),
            frame: Vec::new(),
            open: true,
        }
    }

    /// Connects to `server_url` (`REX_SERVER`), then exchanges hello and welcome,
    /// as the client `client_name`. Blocks until welcomed.
    pub(crate) fn connect(server_url: &str, client_name: &str) -> Result<Client, ClientError> {
        let mut path_buf = [0u8; 256];
        let path = socket_path(server_url, &mut path_buf)?;
        let stream = UnixStream::connect(Path::new(std::ffi::OsStr::from_bytes(path)))?;
        Client::upgrade_and_greet(stream, client_name)
    }

    /// Upgrades `stream`, a new connection to Rex, then exchanges hello and
    /// welcome as the client `client_name`. Blocks until welcomed.
    fn upgrade_and_greet(stream: UnixStream, client_name: &str) -> Result<Client, ClientError> {
        let mut me = Client::from_stream(stream);
        // not upgraded yet: nothing to close
        me.open = false;
        // (a failure below drops `me`, which closes the socket)

        let key = web_socket::new_key();
        // any host works on the Unix socket
        let mut request = Vec::new();
        web_socket::write_request(&mut request, "localhost", "/api/control", PROTOCOL, &key);
        (&me.stream).write_all(&request)?;

        let mut head = [0u8; 4096];
        let mut len = 0;
        let end = loop {
            if len == head.len() {
                return Err(ClientError::ResponseTooLarge);
            }
            let n = read_fd(&me.stream, &mut head[len..])?;
            if n == 0 {
                return Err(ClientError::ConnectionClosed);
            }
            len += n;
            if let Some(e) = web_socket::head_end(&head[..len]) {
                break e;
            }
        };
        web_socket::check_response(&head[..end], &key, PROTOCOL)?;
        me.open = true;
        me.ws.feed(&head[end..len]);

        let hello = serde_json::to_vec(&json!({
            "type": "hello",
            "api_revision": API_REVISION,
            "client": { "kind": "other", "name": client_name },
        }))?;
        send_frame(&me.stream, &mut me.frame, Opcode::Text, &hello)?;

        let welcome = loop {
            if let Some(m) = me.ws.next()? {
                match m {
                    web_socket::Message::Text(t) => {
                        let root: Value = serde_json::from_str(t)?;
                        break parse_welcome(&root)?;
                    }
                    web_socket::Message::Close(_) => return Err(ClientError::ConnectionClosed),
                    _ => {}
                }
            }
            if !me.read()? {
                return Err(ClientError::ConnectionClosed);
            }
        };
        me.max_message = welcome.max_message_bytes;
        me.ws.max_message = welcome.max_message_bytes;
        Ok(me)
    }

    /// The socket, for the caller's loop to watch.
    pub(crate) fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Sends a request and returns its id. A None `payload` is left out.
    pub(crate) fn request(
        &mut self,
        method: &str,
        payload: Option<&Value>,
    ) -> Result<u64, ClientError> {
        let id = self.next_id;
        self.next_id += 1;

        // {"type":"request","id":"<id>","method":..., "payload":...}
        self.out.clear();
        self.out.extend_from_slice(
            format!("{{\"type\":\"request\",\"id\":\"{id}\",\"method\":").as_bytes(),
        );
        serde_json::to_writer(&mut self.out, method)?;
        if let Some(payload) = payload {
            self.out.extend_from_slice(b",\"payload\":");
            serde_json::to_writer(&mut self.out, payload)?;
        }
        self.out.push(b'}');

        // the server closes the connection on a request over its limit
        if self.out.len() > self.max_message {
            return Err(ClientError::MessageTooLarge);
        }
        send_frame(&self.stream, &mut self.frame, Opcode::Text, &self.out)?;
        Ok(id)
    }

    /// One read from the socket. False when the server hung up.
    pub(crate) fn read(&mut self) -> Result<bool, ClientError> {
        let mut buf = [0u8; 16384];
        let n = match read_fd(&self.stream, &mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        if n == 0 {
            return Ok(false);
        }
        self.ws.feed(&buf[..n]);
        Ok(true)
    }

    /// The next message read, or None until more is read. Answers pings and
    /// skips unknown types.
    #[allow(clippy::should_implement_trait)]
    pub(crate) fn next(&mut self) -> Result<Option<Message>, ClientError> {
        while let Some(m) = self.ws.next()? {
            match m {
                web_socket::Message::Ping(p) => {
                    send_frame(&self.stream, &mut self.frame, Opcode::Pong, p)?;
                }
                web_socket::Message::Pong(_) => {}
                web_socket::Message::Close(c) => {
                    if self.open {
                        // echo the code, as RFC 6455 asks
                        let _ = send_close(&self.stream, &mut self.frame, c.code.unwrap_or(1000));
                        self.open = false;
                    }
                    return Ok(Some(Message::Closed(c)));
                }
                web_socket::Message::Text(t) => {
                    let root: Value = serde_json::from_str(t)?;
                    if let Some(msg) = parse_message(&root)? {
                        return Ok(Some(msg));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Sends a close frame, then closes the socket (as the stream drops).
impl Drop for Client {
    fn drop(&mut self) {
        if self.open {
            let _ = send_close(&self.stream, &mut self.frame, 1000);
        }
    }
}

/// The path in a `unix://` URL, percent-decoded into `buf`.
pub(crate) fn socket_path<'a>(url: &str, buf: &'a mut [u8]) -> Result<&'a [u8], ClientError> {
    let scheme = "unix://";
    let Some(encoded) = url.strip_prefix(scheme) else {
        return Err(ClientError::NotUnixUrl);
    };
    let encoded = encoded.as_bytes();
    if encoded.len() > buf.len() {
        return Err(ClientError::NameTooLong);
    }
    // decode `%XX`; anything else is kept as is
    let mut n = 0;
    let mut i = 0;
    while i < encoded.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if encoded[i] == b'%'
            && i + 2 < encoded.len()
            && let (Some(hi), Some(lo)) = (hex(encoded[i + 1]), hex(encoded[i + 2]))
        {
            buf[n] = (hi * 16 + lo) as u8;
            i += 3;
        } else {
            buf[n] = encoded[i];
            i += 1;
        }
        n += 1;
    }
    Ok(&buf[..n])
}

pub(crate) fn parse_welcome(root: &Value) -> Result<Welcome, ClientError> {
    if !eql(get_str(root, "type"), "welcome") {
        return Err(ClientError::NotWelcome);
    }
    if !eql(get_str(root, "protocol"), PROTOCOL) {
        return Err(ClientError::BadProtocol);
    }
    let Some(max) = get_int(get(root, "limits"), "max_message_bytes") else {
        return Err(ClientError::NotWelcome);
    };
    if max < 65536 {
        return Err(ClientError::NotWelcome);
    }
    Ok(Welcome {
        max_message_bytes: max as usize,
        api_revision: get_int(root, "api_revision").unwrap_or(0),
    })
}

/// A response or event, or None for a type we don't know.
pub(crate) fn parse_message(root: &Value) -> Result<Option<Message>, ClientError> {
    let Some(kind) = get_str(root, "type") else {
        return Err(ClientError::BadMessage);
    };
    if kind == "response" {
        let Some(id_str) = get_str(root, "id") else {
            return Err(ClientError::BadMessage);
        };
        // not one of ours
        let Ok(id) = id_str.parse::<u64>() else {
            return Ok(None);
        };
        let seq = get_int(root, "seq").unwrap_or(0);
        if let Some(e @ Value::Object(_)) = get(root, "error") {
            return Ok(Some(Message::Response(Response {
                id,
                seq,
                result: ResponseResult::Err(Error {
                    code: get_str(e, "code").unwrap_or("").to_string(),
                    message: get_str(e, "message").unwrap_or("").to_string(),
                    status: get_int(e, "status").unwrap_or(0),
                }),
            })));
        }
        return Ok(Some(Message::Response(Response {
            id,
            seq,
            result: ResponseResult::Ok(get(root, "payload").cloned().unwrap_or(Value::Null)),
        })));
    }
    if kind == "event" {
        let seq = get_int(root, "seq").unwrap_or(0);
        let Some(mut event) = get(root, "payload") else {
            return Err(ClientError::BadMessage);
        };
        let Some(mut name) = get_str(event, "type") else {
            return Err(ClientError::BadMessage);
        };
        let mut session_id = None;
        if name == "session_event" {
            let inner = get(event, "payload");
            session_id = get_str(inner, "session_id");
            let Some(e) = get(inner, "event") else {
                return Err(ClientError::BadMessage);
            };
            event = e;
            let Some(n) = get_str(event, "type") else {
                return Err(ClientError::BadMessage);
            };
            name = n;
        }
        return Ok(Some(Message::Event(Event {
            seq,
            session_id: session_id.map(str::to_string),
            name: name.to_string(),
            payload: get(event, "payload").cloned().unwrap_or(Value::Null),
        })));
    }
    Ok(None)
}

/// A field of an object, or None.
pub(crate) fn get<'a>(v: impl Into<Option<&'a Value>>, name: &str) -> Option<&'a Value> {
    match v.into()? {
        Value::Object(obj) => obj.get(name),
        _ => None,
    }
}

pub(crate) fn str<'a>(v: impl Into<Option<&'a Value>>) -> Option<&'a str> {
    match v.into()? {
        Value::String(s) => Some(s),
        _ => None,
    }
}

pub(crate) fn int<'a>(v: impl Into<Option<&'a Value>>) -> Option<i64> {
    match v.into()? {
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

/// The integer field `name` of an object, or None.
pub(crate) fn get_int<'a>(v: impl Into<Option<&'a Value>>, name: &str) -> Option<i64> {
    int(get(v, name))
}

/// The string field `name` of an object, or None.
pub(crate) fn get_str<'a>(v: impl Into<Option<&'a Value>>, name: &str) -> Option<&'a str> {
    str(get(v, name))
}

fn eql(a: Option<&str>, b: &str) -> bool {
    a == Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::NAME;

    fn parse_sample(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn socket_path_from_rex_server() {
        let mut buf = [0u8; 128];
        assert_eq!(
            b"/Users/a/Library/Application Support/rex/server.sock".as_slice(),
            socket_path(
                "unix:///Users/a/Library/Application%20Support/rex/server.sock",
                &mut buf
            )
            .unwrap()
        );
        assert!(matches!(
            socket_path("ws://localhost", &mut buf),
            Err(ClientError::NotUnixUrl)
        ));
        let mut small = [0u8; 4];
        assert!(matches!(
            socket_path("unix:///abcde", &mut small),
            Err(ClientError::NameTooLong)
        ));
    }

    #[test]
    fn welcome() {
        let p = parse_sample(
            r#"{"type":"welcome","protocol":"rex.control.v1","limits":{"max_message_bytes":1048576},
 "client":{"id":"client:x","reconnect_token":"t"},"api_revision":7,"new_field":[1]}"#,
        );
        let w = parse_welcome(&p).unwrap();
        assert_eq!(1048576, w.max_message_bytes);
        assert_eq!(7, w.api_revision);

        let bad = parse_sample(
            r#"{"type":"welcome","protocol":"other","limits":{"max_message_bytes":1048576}}"#,
        );
        assert!(matches!(parse_welcome(&bad), Err(ClientError::BadProtocol)));
    }

    #[test]
    fn responses_payload_and_error() {
        let ok =
            parse_sample(r#"{"type":"response","id":"3","seq":5,"payload":{"rex_version":"1.0"}}"#);
        let Some(Message::Response(r)) = parse_message(&ok).unwrap() else {
            panic!("expected a response");
        };
        assert_eq!(3, r.id);
        assert_eq!(5, r.seq);
        let ResponseResult::Ok(payload) = &r.result else {
            panic!("expected ok");
        };
        assert_eq!("1.0", get_str(payload, "rex_version").unwrap());

        let err = parse_sample(
            r#"{"type":"response","id":"2","seq":0,"error":{"code":"not_found","message":"block not found","status":404}}"#,
        );
        let Some(Message::Response(Response {
            result: ResponseResult::Err(e),
            ..
        })) = parse_message(&err).unwrap()
        else {
            panic!("expected an error response");
        };
        assert_eq!("not_found", e.code);
        assert_eq!("block not found", e.message);
        assert_eq!(404, e.status);

        let foreign = parse_sample(r#"{"type":"response","id":"abc","seq":0}"#);
        assert_eq!(None, parse_message(&foreign).unwrap());
    }

    #[test]
    fn events_plain_session_wrapped_and_unknown_types() {
        let plain = parse_sample(
            r#"{"type":"event","seq":1,"payload":{"type":"session_created","payload":{"session":{"session_id":"session:s"}}}}"#,
        );
        let Some(Message::Event(a)) = parse_message(&plain).unwrap() else {
            panic!("expected an event");
        };
        assert_eq!(1, a.seq);
        assert_eq!("session_created", a.name);
        assert_eq!(None, a.session_id);

        let wrapped = parse_sample(
            r#"{"type":"event","seq":2,"payload":{"type":"session_event","payload":{"session_id":"session:s",
 "event":{"type":"block_created","payload":{"block_id":"block:b"}}}}}"#,
        );
        let Some(Message::Event(b)) = parse_message(&wrapped).unwrap() else {
            panic!("expected an event");
        };
        assert_eq!("block_created", b.name);
        assert_eq!("session:s", b.session_id.unwrap());
        assert_eq!("block:b", get_str(&b.payload, "block_id").unwrap());

        let bare = parse_sample(r#"{"type":"event","seq":3,"payload":{"type":"keymap_changed"}}"#);
        let Some(Message::Event(c)) = parse_message(&bare).unwrap() else {
            panic!("expected an event");
        };
        assert_eq!(Value::Null, c.payload);

        let unknown = parse_sample(r#"{"type":"something_new","x":1}"#);
        assert_eq!(None, parse_message(&unknown).unwrap());

        let broken = parse_sample(r#"{"seq":1}"#);
        assert!(matches!(
            parse_message(&broken),
            Err(ClientError::BadMessage)
        ));
    }

    /// Reads until the response to `id`, skipping events.
    fn wait_for(c: &mut Client, id: u64) -> Result<Response, ClientError> {
        loop {
            while let Some(m) = c.next()? {
                match m {
                    Message::Response(r) if r.id == id => return Ok(r),
                    Message::Response(_) | Message::Event(_) => {}
                    Message::Closed(_) => return Err(ClientError::ConnectionClosed),
                }
            }
            if !c.read()? {
                return Err(ClientError::ConnectionClosed);
            }
        }
    }

    #[test]
    fn live_server_version_and_an_unknown_method() {
        let Ok(url) = std::env::var("REX_SERVER") else {
            eprintln!("skipping: REX_SERVER is not set");
            return;
        };
        let mut c = Client::connect(&url, NAME).unwrap();
        assert!(c.max_message >= 65536);

        let id = c.request("server.version", None).unwrap();
        let r = wait_for(&mut c, id).unwrap();
        let ResponseResult::Ok(payload) = &r.result else {
            panic!("expected ok");
        };
        assert!(!get_str(payload, "rex_version").unwrap().is_empty());

        let bad = c
            .request(&format!("{NAME}.no_such_method"), Some(&json!({ "x": 1 })))
            .unwrap();
        let ResponseResult::Err(e) = wait_for(&mut c, bad).unwrap().result else {
            panic!("expected an error");
        };
        assert_eq!("method_not_found", e.code);
        assert_eq!(404, e.status);
    }

    /// Reads one masked frame from `stream` and returns its payload.
    fn read_client_frame(stream: &mut UnixStream) -> Vec<u8> {
        let mut head = [0u8; 2];
        stream.read_exact(&mut head).unwrap();
        let len = match head[1] & 0x7f {
            126 => {
                let mut n = [0u8; 2];
                stream.read_exact(&mut n).unwrap();
                u16::from_be_bytes(n) as usize
            }
            127 => panic!("a frame too big for a hello"),
            n => n as usize,
        };
        let mut mask = [0u8; 4];
        stream.read_exact(&mut mask).unwrap();
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).unwrap();
        body.iter()
            .enumerate()
            .map(|(k, b)| b ^ mask[k % 4])
            .collect()
    }

    // The hello names the client as the caller asked: Rex shows it.
    #[test]
    fn hello_names_the_client() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while web_socket::head_end(&head).is_none() {
                theirs.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            let key = head
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap();
            let accept = web_socket::accept_for(key.as_bytes().try_into().unwrap());
            let accept = std::str::from_utf8(&accept).unwrap();
            write!(
                theirs,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\
                 Sec-WebSocket-Protocol: {PROTOCOL}\r\n\r\n"
            )
            .unwrap();
            let hello: Value = serde_json::from_slice(&read_client_frame(&mut theirs)).unwrap();
            let welcome = json!({
                "type": "welcome",
                "protocol": PROTOCOL,
                "limits": { "max_message_bytes": 65536 },
            })
            .to_string();
            // a server frame is unmasked
            let mut frame = vec![0x81, 126];
            frame.extend_from_slice(&(welcome.len() as u16).to_be_bytes());
            frame.extend_from_slice(welcome.as_bytes());
            theirs.write_all(&frame).unwrap();
            hello
        });
        let c = Client::upgrade_and_greet(ours, "sometool").unwrap();
        let hello = server.join().unwrap();
        assert_eq!("hello", hello["type"]);
        assert_eq!(
            json!({ "kind": "other", "name": "sometool" }),
            hello["client"]
        );
        assert_eq!(65536, c.max_message);
    }

    // The bytes a request puts on the wire.
    #[test]
    fn request_wire_format() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let mut c = Client::from_stream(ours);
        let a = c.request("server.version", None).unwrap();
        let b = c
            .request("session.view", Some(&json!({ "session_id": "session:s" })))
            .unwrap();
        assert_eq!((1, 2), (a, b));
        drop(c);
        let mut bytes = Vec::new();
        theirs.read_to_end(&mut bytes).unwrap();

        // unmask each frame
        let mut texts = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let opcode = bytes[i] & 0x0f;
            let len = (bytes[i + 1] & 0x7f) as usize;
            let mask: [u8; 4] = bytes[i + 2..i + 6].try_into().unwrap();
            let body: Vec<u8> = bytes[i + 6..i + 6 + len]
                .iter()
                .enumerate()
                .map(|(k, b)| b ^ mask[k % 4])
                .collect();
            i += 6 + len;
            texts.push((opcode, body));
        }
        assert_eq!(
            vec![
                (1, br#"{"type":"request","id":"1","method":"server.version"}"#.to_vec()),
                (
                    1,
                    br#"{"type":"request","id":"2","method":"session.view","payload":{"session_id":"session:s"}}"#
                        .to_vec()
                ),
                // closed with 1000
                (8, vec![0x03, 0xe8]),
            ],
            texts
        );
    }
}
