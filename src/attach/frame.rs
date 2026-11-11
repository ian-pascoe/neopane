//! Frames: a kind byte, a big-endian `u32` payload length, then the payload.

use super::Error;

/// Bytes before a frame's payload.
pub const HEADER_LEN: usize = 5;

/// The most a hello's payload may hold: it carries a whole environment.
pub const HELLO_LIMIT: u32 = 4 << 20;

/// The most any other frame's payload may hold. Senders split `i` and `o`
/// to fit.
pub const FRAME_LIMIT: u32 = 64 << 10;

/// One frame. Its kind is an ASCII letter, known or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// The most a frame of `kind` may carry.
pub fn limit(kind: u8) -> u32 {
    if kind == b'h' {
        HELLO_LIMIT
    } else {
        FRAME_LIMIT
    }
}

fn check(kind: u8, len: usize) -> Result<(), Error> {
    if !kind.is_ascii_alphabetic() {
        return Err(Error::Kind(kind));
    }
    match u32::try_from(len) {
        Ok(n) if n <= limit(kind) => Ok(()),
        _ => Err(Error::TooLong {
            kind,
            len: len as u64,
        }),
    }
}

impl Frame {
    /// Appends the frame to `out`. Over the limit, nothing is written.
    pub fn encode_into(kind: u8, payload: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
        check(kind, payload.len())?;
        out.reserve(HEADER_LEN + payload.len());
        out.push(kind);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        Ok(())
    }

    /// The frame's bytes.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        Self::encode_into(self.kind, &self.payload, &mut out)?;
        Ok(out)
    }

    /// The first frame in `buf` and the bytes it took, or None until all of
    /// it has arrived. A bad kind or an over-long length is an error as soon
    /// as the header is in, so a reader never buffers past the limit.
    pub fn decode(buf: &[u8]) -> Result<Option<(Frame, usize)>, Error> {
        let Some(header) = buf.get(..HEADER_LEN) else {
            if let Some(&kind) = buf.first() {
                check(kind, 0)?;
            }
            return Ok(None);
        };
        let kind = header[0];
        let len = u32::from_be_bytes([header[1], header[2], header[3], header[4]]) as usize;
        check(kind, len)?;
        let Some(payload) = buf.get(HEADER_LEN..HEADER_LEN + len) else {
            return Ok(None);
        };
        let frame = Frame {
            kind,
            payload: payload.to_vec(),
        };
        Ok(Some((frame, HEADER_LEN + len)))
    }
}

/// Splits `bytes` into `i` or `o` frames of at most `FRAME_LIMIT` each,
/// appended to `out`. Empty `bytes` sends nothing.
pub fn encode_split(kind: u8, bytes: &[u8], out: &mut Vec<u8>) -> Result<(), Error> {
    for chunk in bytes.chunks(FRAME_LIMIT as usize) {
        Frame::encode_into(kind, chunk, out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let f = Frame {
            kind: b'o',
            payload: b"hi".to_vec(),
        };
        assert_eq!(b"o\0\0\0\x02hi".to_vec(), f.encode().unwrap());
    }

    #[test]
    fn round_trip_split_anywhere() {
        let mut bytes = Vec::new();
        for (kind, payload) in [(b'h', &b"hello"[..]), (b'i', b""), (b'X', b"\0\xff")] {
            Frame::encode_into(kind, payload, &mut bytes).unwrap();
        }
        for cut in 0..bytes.len() {
            // decoding from a prefix waits for the rest
            let mut buf = bytes[..cut].to_vec();
            let mut got = Vec::new();
            let drain = |buf: &mut Vec<u8>, got: &mut Vec<Frame>| {
                while let Some((f, n)) = Frame::decode(buf).unwrap() {
                    buf.drain(..n);
                    got.push(f);
                }
            };
            drain(&mut buf, &mut got);
            buf.extend_from_slice(&bytes[cut..]);
            drain(&mut buf, &mut got);
            assert!(buf.is_empty());
            let kinds: Vec<u8> = got.iter().map(|f| f.kind).collect();
            assert_eq!(b"hiX".to_vec(), kinds, "cut at {cut}");
            assert_eq!(b"\0\xff".to_vec(), got[2].payload);
        }
    }

    #[test]
    fn limits() {
        let mut out = Vec::new();
        let at = |n: u32| vec![0u8; n as usize];
        assert!(Frame::encode_into(b'h', &at(HELLO_LIMIT), &mut out).is_ok());
        assert!(Frame::encode_into(b's', &at(FRAME_LIMIT), &mut out).is_ok());
        out.clear();
        assert_eq!(
            Err(Error::TooLong {
                kind: b'h',
                len: HELLO_LIMIT as u64 + 1
            }),
            Frame::encode_into(b'h', &at(HELLO_LIMIT + 1), &mut out)
        );
        assert!(Frame::encode_into(b'o', &at(FRAME_LIMIT + 1), &mut out).is_err());
        assert!(Frame::encode_into(b'q', &at(FRAME_LIMIT + 1), &mut out).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn an_over_long_header_is_refused_before_its_payload() {
        let mut h = vec![b'o'];
        h.extend_from_slice(&(FRAME_LIMIT + 1).to_be_bytes());
        assert!(matches!(Frame::decode(&h), Err(Error::TooLong { .. })));
        let mut h = vec![b'h'];
        h.extend_from_slice(&(HELLO_LIMIT + 1).to_be_bytes());
        assert!(matches!(Frame::decode(&h), Err(Error::TooLong { .. })));
        let mut h = vec![b'h'];
        h.extend_from_slice(&HELLO_LIMIT.to_be_bytes());
        assert_eq!(Ok(None), Frame::decode(&h));
    }

    #[test]
    fn a_kind_must_be_a_letter() {
        assert_eq!(Err(Error::Kind(b'1')), Frame::decode(b"1"));
        assert_eq!(Err(Error::Kind(0)), Frame::decode(b"\0\0\0\0\0"));
        assert!(Frame::encode_into(b'~', b"", &mut Vec::new()).is_err());
        assert_eq!(Ok(None), Frame::decode(b""));
    }

    #[test]
    fn senders_split_input_and_output() {
        let bytes: Vec<u8> = (0..FRAME_LIMIT * 2 + 3).map(|n| n as u8).collect();
        let mut out = Vec::new();
        encode_split(b'o', &bytes, &mut out).unwrap();
        let mut joined = Vec::new();
        let mut lens = Vec::new();
        let mut rest = &out[..];
        while let Some((f, n)) = Frame::decode(rest).unwrap() {
            lens.push(f.payload.len() as u32);
            joined.extend_from_slice(&f.payload);
            rest = &rest[n..];
        }
        assert_eq!(vec![FRAME_LIMIT, FRAME_LIMIT, 3], lens);
        assert_eq!(bytes, joined);
        out.clear();
        encode_split(b'i', b"", &mut out).unwrap();
        assert!(out.is_empty());
    }
}
