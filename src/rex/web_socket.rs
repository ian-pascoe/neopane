//! A minimal RFC 6455 WebSocket client: the handshake and framing, no I/O.
//! No extensions, so no compression.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

/// What can go wrong in the handshake or in framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotUpgraded,
    UnexpectedExtension,
    BadAccept,
    BadProtocol,
    ReservedBits,
    MaskedFrame,
    BadControlFrame,
    MessageTooLarge,
    UnfinishedMessage,
    UnexpectedContinuation,
    BinaryMessage,
    BadOpcode,
    InvalidUtf8,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Fills `buf` with random bytes.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn random(buf: &mut [u8]) {
    // SAFETY: `buf` is valid for writes of its length.
    unsafe { libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len()) }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn random(buf: &mut [u8]) {
    let mut filled = 0;
    while filled < buf.len() {
        // SAFETY: `buf[filled..]` is valid for writes, and getrandom writes at most
        // `buf.len() - filled` bytes starting inside `buf`.
        let n =
            unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            // only a bad pointer or flags fail, which is a bug here
            panic!("getrandom failed");
        }
        filled += n as usize;
    }
}

pub struct WebSocket {
    /// Bytes received: handed-out frames, then ones not yet parsed.
    pub buf: Vec<u8>,
    /// Offset of the first frame not yet parsed.
    pub pos: usize,
    /// A fragmented message being joined in place in `buf`.
    joined: Option<Joined>,
    /// Largest message accepted, in bytes.
    pub max_message: usize,
}

impl Default for WebSocket {
    fn default() -> Self {
        WebSocket {
            buf: Vec::new(),
            pos: 0,
            joined: None,
            max_message: 1 << 20,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Joined {
    start: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Continuation = 0,
    Text = 1,
    Binary = 2,
    Close = 8,
    Ping = 9,
    Pong = 10,
}

impl Opcode {
    /// The opcode for a 4-bit value, or `None` for one that isn't defined.
    fn from_u4(n: u8) -> Option<Opcode> {
        Some(match n {
            0 => Opcode::Continuation,
            1 => Opcode::Text,
            2 => Opcode::Binary,
            8 => Opcode::Close,
            9 => Opcode::Ping,
            10 => Opcode::Pong,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message<'a> {
    Text(&'a str),
    Ping(&'a [u8]),
    Pong(&'a [u8]),
    Close(Close),
}

/// A close frame. (`reason` is owned, not a slice of the buffer, so a close
/// can outlive the next `feed`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Close {
    /// None when the frame had no body.
    pub code: Option<u16>,
    pub reason: Vec<u8>,
}

/// A parsed frame, as ranges of `buf`; see `WebSocket::parse`.
enum Parsed {
    Text(usize, usize),
    Ping(usize, usize),
    Pong(usize, usize),
    Close(Option<u16>, usize, usize),
}

impl WebSocket {
    /// Adds bytes read from the socket. Invalidates messages from `next`.
    pub fn feed(&mut self, bytes: &[u8]) {
        // drop parsed frames, keeping a message still being joined
        let drop = match self.joined {
            Some(j) => j.start,
            None => self.pos,
        };
        self.buf.drain(..drop);
        self.pos -= drop;
        if let Some(j) = &mut self.joined {
            j.start = 0;
        }

        self.buf.extend_from_slice(bytes);
    }

    /// The next whole message, or None until more is fed. Valid until `next` or `feed`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Message<'_>>, Error> {
        // (the parsing loop yields ranges, so the borrow handed out starts
        // only after it)
        let Some(parsed) = self.parse()? else {
            return Ok(None);
        };
        Ok(Some(match parsed {
            Parsed::Text(start, len) => Message::Text(check_utf8(&self.buf[start..][..len])?),
            Parsed::Ping(start, len) => Message::Ping(&self.buf[start..][..len]),
            Parsed::Pong(start, len) => Message::Pong(&self.buf[start..][..len]),
            Parsed::Close(code, start, len) => Message::Close(Close {
                code,
                reason: self.buf[start..][..len].to_vec(),
            }),
        }))
    }

    /// Reads frames until one is a whole message.
    fn parse(&mut self) -> Result<Option<Parsed>, Error> {
        loop {
            let rest = &self.buf[self.pos..];
            if rest.len() < 2 {
                return Ok(None);
            }
            let fin = rest[0] & 0x80 != 0;
            if rest[0] & 0x70 != 0 {
                return Err(Error::ReservedBits);
            }
            let nibble = rest[0] & 0x0f;
            let opcode = Opcode::from_u4(nibble);
            // servers never mask
            if rest[1] & 0x80 != 0 {
                return Err(Error::MaskedFrame);
            }

            let len7 = rest[1] & 0x7f;
            let header: usize = match len7 {
                126 => 4,
                127 => 10,
                _ => 2,
            };
            if rest.len() < header {
                return Ok(None);
            }
            let len: u64 = match len7 {
                126 => u16::from_be_bytes([rest[2], rest[3]]) as u64,
                127 => u64::from_be_bytes(rest[2..10].try_into().unwrap()),
                _ => len7 as u64,
            };
            let is_control = nibble >= 8;
            if is_control && (!fin || len > 125) {
                return Err(Error::BadControlFrame);
            }
            let so_far = self.joined.map_or(0, |j| j.len);
            if len > (self.max_message - so_far) as u64 {
                return Err(Error::MessageTooLarge);
            }
            let n = len as usize;
            if rest.len() - header < n {
                return Ok(None);
            }

            let start = self.pos + header;
            self.pos = start + n;

            match opcode {
                Some(Opcode::Text) => {
                    if self.joined.is_some() {
                        return Err(Error::UnfinishedMessage);
                    }
                    if fin {
                        return Ok(Some(Parsed::Text(start, n)));
                    }
                    self.joined = Some(Joined { start, len: n });
                }
                Some(Opcode::Continuation) => {
                    let Some(j) = &mut self.joined else {
                        return Err(Error::UnexpectedContinuation);
                    };
                    // joined data always sits before this frame
                    self.buf.copy_within(start..start + n, j.start + j.len);
                    j.len += n;
                    if fin {
                        let (jstart, jlen) = (j.start, j.len);
                        self.joined = None;
                        return Ok(Some(Parsed::Text(jstart, jlen)));
                    }
                }
                Some(Opcode::Ping) => return Ok(Some(Parsed::Ping(start, n))),
                Some(Opcode::Pong) => return Ok(Some(Parsed::Pong(start, n))),
                Some(Opcode::Close) => {
                    if n == 1 {
                        return Err(Error::BadControlFrame);
                    }
                    if n == 0 {
                        return Ok(Some(Parsed::Close(None, start, 0)));
                    }
                    let code = u16::from_be_bytes([self.buf[start], self.buf[start + 1]]);
                    return Ok(Some(Parsed::Close(Some(code), start + 2, n - 2)));
                }
                Some(Opcode::Binary) => return Err(Error::BinaryMessage),
                None => return Err(Error::BadOpcode),
            }
        }
    }
}

fn check_utf8(text: &[u8]) -> Result<&str, Error> {
    std::str::from_utf8(text).map_err(|_| Error::InvalidUtf8)
}

/// A `Sec-WebSocket-Key`: 16 random bytes in base64.
pub type Key = [u8; 24];

pub fn new_key() -> Key {
    let mut nonce = [0u8; 16];
    random(&mut nonce);
    let mut key = [0u8; 24];
    STANDARD
        .encode_slice(nonce, &mut key)
        .expect("16 bytes encode to 24");
    key
}

/// The `Sec-WebSocket-Accept` a server must answer `key` with.
pub fn accept_for(key: &Key) -> [u8; 28] {
    let mut h = sha1_smol::Sha1::new();
    h.update(key);
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let digest = h.digest().bytes();
    let mut accept = [0u8; 28];
    STANDARD
        .encode_slice(digest, &mut accept)
        .expect("20 bytes encode to 28");
    accept
}

/// Writes the HTTP upgrade request.
pub fn write_request(w: &mut Vec<u8>, host: &str, path: &str, protocol: &str, key: &Key) {
    let key = String::from_utf8_lossy(key);
    w.extend_from_slice(
        format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host}\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Protocol: {protocol}\r\n\
             \r\n"
        )
        .as_bytes(),
    );
}

/// Length of the response head through its blank line, or None if it's cut off.
pub fn head_end(bytes: &[u8]) -> Option<usize> {
    let i = bytes.windows(4).position(|w| w == b"\r\n\r\n")?;
    Some(i + 4)
}

/// `bytes` without leading and trailing spaces and tabs.
fn trim_blanks(mut bytes: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = bytes {
        bytes = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = bytes {
        bytes = rest;
    }
    bytes
}

/// Checks the server's response head: 101, the accept, and the subprotocol.
pub fn check_response(head: &[u8], key: &Key, protocol: &str) -> Result<(), Error> {
    // split on "\r\n"
    let mut lines = Vec::new();
    let mut from = 0;
    while let Some(i) = head[from..].windows(2).position(|w| w == b"\r\n") {
        lines.push(&head[from..from + i]);
        from += i + 2;
    }
    lines.push(&head[from..]);
    let mut lines = lines.into_iter();

    let status = lines.next().unwrap();
    if !status.starts_with(b"HTTP/1.1 101") {
        return Err(Error::NotUpgraded);
    }

    let mut upgrade = false;
    let mut accepted = false;
    let mut chosen = false;
    for line in lines {
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        let name = &line[..colon];
        let value = trim_blanks(&line[colon + 1..]);
        if name.eq_ignore_ascii_case(b"upgrade") {
            upgrade = value.eq_ignore_ascii_case(b"websocket");
        } else if name.eq_ignore_ascii_case(b"sec-websocket-accept") {
            accepted = value == accept_for(key);
        } else if name.eq_ignore_ascii_case(b"sec-websocket-protocol") {
            chosen = value == protocol.as_bytes();
        } else if name.eq_ignore_ascii_case(b"sec-websocket-extensions") {
            // none were offered
            return Err(Error::UnexpectedExtension);
        }
    }
    if !upgrade {
        return Err(Error::NotUpgraded);
    }
    if !accepted {
        return Err(Error::BadAccept);
    }
    if !chosen {
        return Err(Error::BadProtocol);
    }
    Ok(())
}

/// Writes one masked frame with a random mask. Doesn't flush.
pub fn write_frame(w: &mut Vec<u8>, opcode: Opcode, payload: &[u8]) {
    let mut mask = [0u8; 4];
    random(&mut mask);
    write_frame_masked(w, opcode, payload, mask);
}

/// Writes one final frame masked with `mask`.
pub fn write_frame_masked(w: &mut Vec<u8>, opcode: Opcode, payload: &[u8], mask: [u8; 4]) {
    w.push(0x80 | opcode as u8);
    if payload.len() < 126 {
        w.push(0x80 | payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        w.push(0x80 | 126);
        w.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        w.push(0x80 | 127);
        w.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    w.extend_from_slice(&mask);
    w.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
}

/// Writes a close frame with `code`. Doesn't flush.
pub fn write_close(w: &mut Vec<u8>, code: u16) {
    write_frame(w, Opcode::Close, &code.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unmasked server frame.
    fn server_frame(fin: bool, opcode: Opcode, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push((if fin { 0x80 } else { 0 }) | opcode as u8);
        if payload.len() < 126 {
            out.push(payload.len() as u8);
        } else if payload.len() <= u16::MAX as usize {
            out.push(126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            out.push(127);
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        out.extend_from_slice(payload);
        out
    }

    fn expect_text(ws: &mut WebSocket, expected: &[u8]) {
        let m = ws.next().unwrap().expect("TestExpectedMessage");
        match m {
            Message::Text(t) => assert_eq!(expected, t.as_bytes()),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn rfc_6455_accept_example() {
        let key: Key = *b"dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", &accept_for(&key));
    }

    #[test]
    fn request_and_response() {
        let key: Key = *b"dGhlIHNhbXBsZSBub25jZQ==";
        let mut w = Vec::new();
        write_request(&mut w, "localhost", "/api/control", "rex.control.v1", &key);
        let text = std::str::from_utf8(&w).unwrap();
        assert!(text.starts_with("GET /api/control HTTP/1.1\r\n"));
        assert!(text.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(text.contains("Sec-WebSocket-Protocol: rex.control.v1\r\n"));
        assert!(text.ends_with("\r\n\r\n"));

        let good: &[u8] = b"HTTP/1.1 101 Switching Protocols\r\nupgrade: WebSocket\r\nConnection: Upgrade\r\n\
Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\nSec-WebSocket-Protocol: rex.control.v1\r\n\r\n";
        let with_frame = [good, b"\x81\x00"].concat();
        assert_eq!(Some(good.len()), head_end(&with_frame));
        assert_eq!(None, head_end(&good[..good.len() - 1]));
        check_response(good, &key, "rex.control.v1").unwrap();
        assert_eq!(Err(Error::BadProtocol), check_response(good, &key, "other"));
        let other_key: Key = *b"AAAAAAAAAAAAAAAAAAAAAA==";
        assert_eq!(
            Err(Error::BadAccept),
            check_response(good, &other_key, "rex.control.v1")
        );
        assert_eq!(
            Err(Error::NotUpgraded),
            check_response(b"HTTP/1.1 400 Bad Request\r\n\r\n", &key, "rex.control.v1")
        );
    }

    #[test]
    fn each_length_form() {
        let mut ws = WebSocket::default();
        for len in [0usize, 125, 126, 65535, 65536] {
            let payload = vec![b'a'; len];
            let frame = server_frame(true, Opcode::Text, &payload);
            let overhead = if len < 126 {
                2
            } else if len < 65536 {
                4
            } else {
                10
            };
            assert_eq!(overhead, frame.len() - len);
            ws.feed(&frame);
            expect_text(&mut ws, &payload);
            assert_eq!(None, ws.next().unwrap());
        }
    }

    #[test]
    fn a_frame_split_across_feeds_byte_by_byte() {
        let mut ws = WebSocket::default();
        let payload = [b'x'; 300];
        let frame = server_frame(true, Opcode::Text, &payload);
        for &b in &frame[..frame.len() - 1] {
            ws.feed(&[b]);
            assert_eq!(None, ws.next().unwrap());
        }
        ws.feed(&frame[frame.len() - 1..]);
        expect_text(&mut ws, &payload);
    }

    #[test]
    fn fragments_joined_with_a_ping_between_them() {
        let mut ws = WebSocket::default();
        let parts = [
            server_frame(false, Opcode::Text, b"hel"),
            server_frame(false, Opcode::Continuation, b"lo, "),
            server_frame(true, Opcode::Ping, b"p"),
            server_frame(true, Opcode::Continuation, b"world"),
            server_frame(true, Opcode::Text, b"next"),
        ];
        ws.feed(&parts[0]);
        ws.feed(&parts[1]);
        assert_eq!(None, ws.next().unwrap());
        // compaction keeps the joined part
        ws.feed(&parts[2]);
        assert_eq!(Some(Message::Ping(b"p")), ws.next().unwrap());
        let tail = parts[3..].concat();
        ws.feed(&tail);
        expect_text(&mut ws, b"hello, world");
        expect_text(&mut ws, b"next");
    }

    #[test]
    fn ping_pong_and_close() {
        let mut ws = WebSocket::default();
        ws.feed(b"\x89\x02hi\x8a\x00\x88\x07\x03\xe9going\x88\x00");
        assert_eq!(Some(Message::Ping(b"hi")), ws.next().unwrap());
        assert_eq!(Some(Message::Pong(b"")), ws.next().unwrap());
        let Some(Message::Close(c)) = ws.next().unwrap() else {
            panic!("expected close");
        };
        assert_eq!(Some(1001), c.code);
        assert_eq!(b"going".as_slice(), c.reason);
        let Some(Message::Close(c)) = ws.next().unwrap() else {
            panic!("expected close");
        };
        assert_eq!(None, c.code);
    }

    #[test]
    fn oddities_are_rejected() {
        let cases: [(&[u8], Error); 10] = [
            (b"\x81\x81\x00\x00\x00\x00x", Error::MaskedFrame),
            (b"\x82\x01x", Error::BinaryMessage),
            (b"\xc1\x01x", Error::ReservedBits),
            (b"\x83\x00", Error::BadOpcode),
            (b"\x09\x00", Error::BadControlFrame), // fragmented ping
            (b"\x88\x01x", Error::BadControlFrame),
            (b"\x80\x01x", Error::UnexpectedContinuation),
            (b"\x01\x01x\x81\x01y", Error::UnfinishedMessage),
            (b"\x81\x02\xc3\x28", Error::InvalidUtf8),
            (
                b"\x81\x7f\x00\x00\x00\x00\x00\x10\x00\x01",
                Error::MessageTooLarge,
            ),
        ];
        for (bytes, err) in cases {
            let mut ws = WebSocket::default();
            ws.feed(bytes);
            assert_eq!(Err(err), ws.next());
        }
        // fragments count toward the limit too
        let mut ws = WebSocket {
            max_message: 4,
            ..WebSocket::default()
        };
        ws.feed(b"\x01\x03abc\x80\x02de");
        assert_eq!(Err(Error::MessageTooLarge), ws.next());
    }

    #[test]
    fn sent_frames_are_masked() {
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let mut w = Vec::new();
        // RFC 6455 section 5.7: masked "Hello"
        write_frame_masked(&mut w, Opcode::Text, b"Hello", mask);
        assert_eq!(
            b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58".as_slice(),
            w
        );

        for len in [126usize, 300, 65536] {
            w.clear();
            let payload = [b'z'; 65536];
            write_frame_masked(&mut w, Opcode::Pong, &payload[..len], mask);
            let header = if len < 65536 { 8 } else { 14 };
            assert_eq!(header + len, w.len());
            assert_eq!(0x8a, w[0]);
            assert_eq!(mask.as_slice(), &w[header - 4..header]);
            for (i, b) in w[header..].iter().enumerate() {
                assert_eq!(b'z', b ^ mask[i % 4]);
            }
        }

        w.clear();
        write_close(&mut w, 1000);
        assert_eq!(b"\x88\x82".as_slice(), &w[0..2]);
        assert_eq!(0x03, w[6] ^ w[2]);
        assert_eq!(0xe8, w[7] ^ w[3]);
    }
}
