//! msgpack-RPC messages: writing requests and parsing whole messages.

use super::msgpack::{self, Decoder, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum Error {
    /// Not the message, or the value, that was expected.
    BadMessage,
    /// The bytes aren't msgpack.
    InvalidByte,
}

impl From<msgpack::Error> for Error {
    fn from(e: msgpack::Error) -> Self {
        match e {
            msgpack::Error::InvalidByte => Error::InvalidByte,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// A parsed message. Slices and decoders point into its bytes.
#[derive(Debug, Clone, Copy)]
#[doc(hidden)]
pub enum Message<'a> {
    /// `[0, id, method, params]`
    Request {
        id: u32,
        method: &'a [u8],
        /// Positioned at the params array.
        params: Decoder<'a>,
    },
    /// `[1, id, error, result]`
    Response {
        id: u32,
        /// Neovim's error message, or None on success.
        err: Option<&'a [u8]>,
        /// Positioned at the result.
        result: Decoder<'a>,
    },
    /// `[2, method, params]`
    Notification {
        method: &'a [u8],
        /// Positioned at the params array.
        params: Decoder<'a>,
    },
}

/// Writes `[0, id, method, [`; follow with `param_count` params.
pub(crate) fn write_request_header(w: &mut Vec<u8>, id: u32, method: &str, param_count: u32) {
    msgpack::write_array_header(w, 4);
    msgpack::write_int(w, 0);
    msgpack::write_int(w, id);
    msgpack::write_str(w, method);
    msgpack::write_array_header(w, param_count);
}

/// Parses one whole message, as returned by `Framer::next`.
pub(crate) fn parse(bytes: &[u8]) -> Result<Message<'_>> {
    let mut d = Decoder::of(bytes);
    let len = expect_array(&mut d)?;
    match expect_int(&mut d)? {
        0 => {
            if len != 4 {
                return Err(Error::BadMessage);
            }
            let id = expect_id(&mut d)?;
            let method = expect_str(&mut d)?;
            Ok(Message::Request {
                id,
                method,
                params: d,
            })
        }
        1 => {
            if len != 4 {
                return Err(Error::BadMessage);
            }
            let id = expect_id(&mut d)?;
            let err = read_error(&mut d)?;
            Ok(Message::Response { id, err, result: d })
        }
        2 => {
            if len != 3 {
                return Err(Error::BadMessage);
            }
            let method = expect_str(&mut d)?;
            Ok(Message::Notification { method, params: d })
        }
        _ => Err(Error::BadMessage),
    }
}

/// Reads an array header, returning its length.
pub(crate) fn expect_array(d: &mut Decoder<'_>) -> Result<u32> {
    match next_token(d)? {
        Token::Array(n) => Ok(n),
        _ => Err(Error::BadMessage),
    }
}

pub(crate) fn expect_int(d: &mut Decoder<'_>) -> Result<i64> {
    match next_token(d)? {
        Token::Int(n) => Ok(n),
        _ => Err(Error::BadMessage),
    }
}

/// Reads a whole string. Needs a whole message, so it's one chunk.
pub(crate) fn expect_str<'a>(d: &mut Decoder<'a>) -> Result<&'a [u8]> {
    match next_token(d)? {
        Token::Str(len) => expect_bytes(d, len),
        _ => Err(Error::BadMessage),
    }
}

pub(crate) fn expect_bool(d: &mut Decoder<'_>) -> Result<bool> {
    match next_token(d)? {
        Token::Bool(b) => Ok(b),
        _ => Err(Error::BadMessage),
    }
}

/// Reads a map header, returning its number of pairs.
pub(crate) fn expect_map(d: &mut Decoder<'_>) -> Result<u32> {
    match next_token(d)? {
        Token::Map(n) => Ok(n),
        _ => Err(Error::BadMessage),
    }
}

/// A buffer, window or tabpage handle: an ext wrapping an int, or a plain int.
pub(crate) fn expect_handle(d: &mut Decoder<'_>) -> Result<i64> {
    match next_token(d)? {
        Token::Int(n) => Ok(n),
        Token::Ext(e) => {
            let mut inner = Decoder::of(expect_bytes(d, e.len)?);
            expect_int(&mut inner)
        }
        _ => Err(Error::BadMessage),
    }
}

/// Skips one whole value, however deeply nested.
pub(crate) fn skip(d: &mut Decoder<'_>) -> Result<()> {
    // values still owed; nesting doesn't matter when only counting
    let mut owed: u64 = 1;
    while owed > 0 {
        match next_token(d)? {
            Token::Array(n) => owed += n as u64,
            Token::Map(n) => owed += n as u64 * 2,
            Token::Str(len) | Token::Bin(len) => {
                expect_bytes(d, len)?;
            }
            Token::Ext(e) => {
                expect_bytes(d, e.len)?;
            }
            Token::Chunk(_) => return Err(Error::BadMessage),
            _ => {}
        }
        owed -= 1;
    }
    Ok(())
}

/// A token that must exist: the message is whole, so None means it's cut short.
pub(crate) fn next_token<'a>(d: &mut Decoder<'a>) -> Result<Token<'a>> {
    d.next()?.ok_or(Error::BadMessage)
}

/// The `len` bytes after a str, bin or ext header.
pub(crate) fn expect_bytes<'a>(d: &mut Decoder<'a>, len: u32) -> Result<&'a [u8]> {
    if len == 0 {
        return Ok(b"");
    }
    match next_token(d)? {
        Token::Chunk(c) if c.len() == len as usize => Ok(c),
        _ => Err(Error::BadMessage),
    }
}

fn expect_id(d: &mut Decoder<'_>) -> Result<u32> {
    u32::try_from(expect_int(d)?).map_err(|_| Error::BadMessage)
}

/// nil, or Neovim's `[type, message]`. Returns the message.
fn read_error<'a>(d: &mut Decoder<'a>) -> Result<Option<&'a [u8]>> {
    match next_token(d)? {
        Token::Nil => Ok(None),
        Token::Array(n) => {
            if n < 2 {
                return Err(Error::BadMessage);
            }
            skip(d)?; // error type
            let msg = expect_str(d)?;
            for _ in 2..n {
                skip(d)?;
            }
            Ok(Some(msg))
        }
        _ => Err(Error::BadMessage),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_header() {
        let mut w = Vec::new();
        write_request_header(&mut w, 7, "nvim_input", 1);
        msgpack::write_str(&mut w, "<C-a>");
        assert_eq!(w, b"\x94\x00\x07\xaanvim_input\x91\xa5<C-a>");
    }

    #[test]
    fn parse_response() {
        // [1, 1, nil, 3]
        let Message::Response {
            id,
            err,
            mut result,
        } = parse(b"\x94\x01\x01\xc0\x03").unwrap()
        else {
            panic!("not a response");
        };
        assert_eq!(id, 1);
        assert_eq!(err, None);
        assert_eq!(expect_int(&mut result), Ok(3));
    }

    #[test]
    fn parse_error_response() {
        // [1, 2, [0, "boom"], nil]
        let Message::Response { id, err, .. } = parse(b"\x94\x01\x02\x92\x00\xa4boom\xc0").unwrap()
        else {
            panic!("not a response");
        };
        assert_eq!(id, 2);
        assert_eq!(err, Some(&b"boom"[..]));
    }

    #[test]
    fn parse_notification() {
        // [2, "redraw", [["flush"]]]
        let Message::Notification { method, mut params } =
            parse(b"\x93\x02\xa6redraw\x91\x91\xa5flush").unwrap()
        else {
            panic!("not a notification");
        };
        assert_eq!(method, b"redraw");
        assert_eq!(expect_array(&mut params), Ok(1));
        assert_eq!(expect_array(&mut params), Ok(1));
        assert_eq!(expect_str(&mut params), Ok(&b"flush"[..]));
    }

    #[test]
    fn parse_request() {
        // [0, 7, "x", []]
        let Message::Request {
            id,
            method,
            mut params,
        } = parse(b"\x94\x00\x07\xa1x\x90").unwrap()
        else {
            panic!("not a request");
        };
        assert_eq!(id, 7);
        assert_eq!(method, b"x");
        assert_eq!(expect_array(&mut params), Ok(0));
    }

    #[test]
    fn parse_rejects_bad_messages() {
        let bad = |bytes: &[u8]| matches!(parse(bytes), Err(Error::BadMessage));
        assert!(bad(b"\x05")); // not an array
        assert!(bad(b"\x93\x03\x01\x02")); // unknown type
        assert!(bad(b"\x93\x01\x01\xc0")); // response too short
        assert!(bad(b"\x94\x01\xff\xc0\xc0")); // negative id
    }

    #[test]
    fn handles() {
        // window 1000 as ext 1, then a plain 7, then -1 as fixext 1
        let mut d = Decoder::of(b"\xc7\x03\x01\xcd\x03\xe8\x07\xd4\x01\xff");
        assert_eq!(expect_handle(&mut d), Ok(1000));
        assert_eq!(expect_handle(&mut d), Ok(7));
        assert_eq!(expect_handle(&mut d), Ok(-1));
    }

    #[test]
    fn skip_() {
        // [1, {2: "ab"}, nil] then 9
        let mut d = Decoder::of(b"\x93\x01\x81\x02\xa2ab\xc0\x09");
        skip(&mut d).unwrap();
        assert_eq!(expect_int(&mut d), Ok(9));
    }
}
