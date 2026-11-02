//! Splits a byte stream back into whole msgpack messages.

use super::msgpack::{self, Decoder, Token};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The bytes aren't msgpack.
    InvalidByte,
    /// Nested deeper than `Framer` follows.
    TooDeep,
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

#[derive(Debug, Default)]
pub struct Framer {
    /// Bytes received: handed-out messages, then ones not yet handed out.
    pub buf: Vec<u8>,
    /// Offset of the first byte not yet handed out.
    start: usize,
}

impl Framer {
    /// Adds bytes read from the stream. Invalidates messages from `next`.
    pub fn feed(&mut self, bytes: &[u8]) {
        // drop handed-out messages
        self.buf.drain(..self.start);
        self.start = 0;

        self.buf.extend_from_slice(bytes);
    }

    /// The next whole message, or None until more is fed. Valid until `feed`.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<&[u8]>, Error> {
        let rest = &self.buf[self.start..];
        if rest.is_empty() {
            return Ok(None);
        }
        let Some(end) = message_end(rest)? else {
            return Ok(None);
        };
        let start = self.start;
        self.start += end;
        Ok(Some(&self.buf[start..start + end]))
    }
}

/// Length of the first whole msgpack value in `bytes`, or None if it's cut off.
fn message_end(bytes: &[u8]) -> Result<Option<usize>, Error> {
    let mut d = Decoder::of(bytes);

    // values still owed at each open level; the message itself is one
    let mut stack = [0u64; 32];
    stack[0] = 1;
    let mut depth: usize = 1;
    // bytes still owed for the current str, bin or ext
    let mut owed: u32 = 0;

    while let Some(token) = d.next()? {
        if let Token::Chunk(c) = token {
            owed -= c.len() as u32;
        } else {
            stack[depth - 1] -= 1;
            match token {
                Token::Array(n) if n > 0 => push(&mut stack, &mut depth, n as u64)?,
                // u64 so n * 2 can't overflow
                Token::Map(n) if n > 0 => push(&mut stack, &mut depth, n as u64 * 2)?,
                Token::Str(len) | Token::Bin(len) => owed = len,
                Token::Ext(e) => owed = e.len,
                _ => {}
            }
        }
        while depth > 0 && stack[depth - 1] == 0 {
            depth -= 1;
        }
        if depth == 0 && owed == 0 {
            return Ok(Some(bytes.len() - d.input.len()));
        }
    }
    Ok(None)
}

/// Opens a level owing `n` values.
fn push(stack: &mut [u64; 32], depth: &mut usize, n: u64) -> Result<(), Error> {
    if *depth == stack.len() {
        return Err(Error::TooDeep);
    }
    stack[*depth] = n;
    *depth += 1;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_next(f: &mut Framer, expected: Option<&[u8]>) {
        assert_eq!(f.next().unwrap(), expected);
    }

    #[test]
    fn nothing_fed() {
        let mut f = Framer::default();
        expect_next(&mut f, None);
    }

    #[test]
    fn two_messages_split_across_feeds() {
        let mut f = Framer::default();
        f.feed(b"\x94\x01\x01"); // half of message 1
        expect_next(&mut f, None);
        f.feed(b"\xc0\x03\x94\x01"); // rest of 1, start of 2
        expect_next(&mut f, Some(b"\x94\x01\x01\xc0\x03"));
        expect_next(&mut f, None);
        f.feed(b"\x01\xc0\x03"); // rest of 2
        expect_next(&mut f, Some(b"\x94\x01\x01\xc0\x03"));
        expect_next(&mut f, None);
    }

    #[test]
    fn split_inside_a_string() {
        // ["hi"] cut after "h": complete only once "i" arrives
        let mut f = Framer::default();
        f.feed(b"\x91\xa2h");
        expect_next(&mut f, None);
        f.feed(b"i");
        expect_next(&mut f, Some(b"\x91\xa2hi"));
    }

    #[test]
    fn several_messages_in_one_feed() {
        // [1, ["hi"], nil], then 5, [] and {}
        let mut f = Framer::default();
        f.feed(b"\x93\x01\x91\xa2hi\xc0\x05\x90\x80");
        expect_next(&mut f, Some(b"\x93\x01\x91\xa2hi\xc0"));
        expect_next(&mut f, Some(b"\x05"));
        expect_next(&mut f, Some(b"\x90"));
        expect_next(&mut f, Some(b"\x80"));
        expect_next(&mut f, None);
    }

    #[test]
    fn maps_owe_keys_and_values() {
        // {1: [2, 3]} then 4
        let mut f = Framer::default();
        f.feed(b"\x81\x01\x92\x02\x03\x04");
        expect_next(&mut f, Some(b"\x81\x01\x92\x02\x03"));
        expect_next(&mut f, Some(b"\x04"));
    }

    #[test]
    fn leftover_bytes_survive_compaction() {
        let mut f = Framer::default();
        f.feed(b"\x05\x92\x01");
        expect_next(&mut f, Some(b"\x05"));
        expect_next(&mut f, None);
        f.feed(b"\x02");
        expect_next(&mut f, Some(b"\x92\x01\x02"));
        // the handed-out 0x05 was dropped by the second feed
        assert_eq!(f.buf.len(), 3);
    }

    #[test]
    fn neovims_api_info_in_random_pieces() {
        let data: &[u8] = include_bytes!("testdata/nvim_get_api_info.mpack");
        // xorshift, seeded, so the pieces are the same every run
        let mut state: u64 = 1;
        let mut random = |max: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            1 + (state % max as u64) as usize
        };
        for _ in 0..50 {
            let mut f = Framer::default();
            let mut off = 0;
            while off < data.len() {
                let n = (data.len() - off).min(random(500));
                f.feed(&data[off..off + n]);
                off += n;
                if off < data.len() {
                    expect_next(&mut f, None);
                }
            }
            expect_next(&mut f, Some(data));
            expect_next(&mut f, None);
        }
    }

    #[test]
    fn errors() {
        let mut f = Framer::default();
        f.feed(b"\xc1");
        assert_eq!(f.next(), Err(Error::InvalidByte));

        let mut g = Framer::default();
        let deep = [0x91u8; 40]; // [[[[...]]]]
        g.feed(&deep);
        assert_eq!(g.next(), Err(Error::TooDeep));
    }
}
