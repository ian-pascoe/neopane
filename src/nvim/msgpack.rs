//! MessagePack encoding and decoding.
//! Encoding: each function writes one value. Arrays and maps write only a
//! header; the caller writes the items after.
//! Decoding: `Decoder` turns bytes into tokens, in the design of libmpack.
// needs review

/// Decoding errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// 0xc1, which is never used.
    InvalidByte,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub fn write_nil(w: &mut Vec<u8>) {
    w.push(0xc0);
}

pub fn write_bool(w: &mut Vec<u8>, b: bool) {
    w.push(if b { 0xc3 } else { 0xc2 });
}

/// Smallest encoding that fits.
pub fn write_int(w: &mut Vec<u8>, n: impl Into<i64>) {
    let n: i64 = n.into();
    if n >= 0 {
        if n <= 0x7f {
            return w.push(n as u8); // positive fixint
        }
        if n <= u8::MAX as i64 {
            return write_tagged(w, 0xcc, &(n as u8).to_be_bytes());
        }
        if n <= u16::MAX as i64 {
            return write_tagged(w, 0xcd, &(n as u16).to_be_bytes());
        }
        if n <= u32::MAX as i64 {
            return write_tagged(w, 0xce, &(n as u32).to_be_bytes());
        }
        return write_tagged(w, 0xcf, &(n as u64).to_be_bytes());
    }
    if n >= -32 {
        return w.push(n as i8 as u8); // negative fixint
    }
    if n >= i8::MIN as i64 {
        return write_tagged(w, 0xd0, &(n as i8).to_be_bytes());
    }
    if n >= i16::MIN as i64 {
        return write_tagged(w, 0xd1, &(n as i16).to_be_bytes());
    }
    if n >= i32::MIN as i64 {
        return write_tagged(w, 0xd2, &(n as i32).to_be_bytes());
    }
    write_tagged(w, 0xd3, &n.to_be_bytes())
}

pub fn write_float(w: &mut Vec<u8>, f: f64) {
    write_tagged(w, 0xcb, &f.to_bits().to_be_bytes());
}

/// Panics if `s` is longer than u32 can say, which nothing here sends.
pub fn write_str(w: &mut Vec<u8>, s: impl AsRef<[u8]>) {
    let s = s.as_ref();
    write_str_header(
        w,
        u32::try_from(s.len()).expect("string too long for msgpack"),
    );
    w.extend_from_slice(s);
}

/// Follow with `len` bytes of string.
pub fn write_str_header(w: &mut Vec<u8>, len: u32) {
    if len <= 31 {
        return w.push(0xa0 | len as u8); // fixstr
    }
    if len <= u8::MAX as u32 {
        return write_tagged(w, 0xd9, &(len as u8).to_be_bytes());
    }
    if len <= u16::MAX as u32 {
        return write_tagged(w, 0xda, &(len as u16).to_be_bytes());
    }
    write_tagged(w, 0xdb, &len.to_be_bytes())
}

/// Follow with `len` values.
pub fn write_array_header(w: &mut Vec<u8>, len: u32) {
    if len <= 15 {
        return w.push(0x90 | len as u8); // fixarray
    }
    if len <= u16::MAX as u32 {
        return write_tagged(w, 0xdc, &(len as u16).to_be_bytes());
    }
    write_tagged(w, 0xdd, &len.to_be_bytes())
}

/// Follow with `len` key-value pairs.
pub fn write_map_header(w: &mut Vec<u8>, len: u32) {
    if len <= 15 {
        return w.push(0x80 | len as u8); // fixmap
    }
    if len <= u16::MAX as u32 {
        return write_tagged(w, 0xde, &(len as u16).to_be_bytes());
    }
    write_tagged(w, 0xdf, &len.to_be_bytes())
}

/// A type byte, then the value's big-endian bytes.
fn write_tagged(w: &mut Vec<u8>, tag: u8, big_endian: &[u8]) {
    w.push(tag);
    w.extend_from_slice(big_endian);
}

/// One decoded piece of a value. Containers and blobs are headers: an array
/// is followed by `len` values, a map by `len` key-value pairs, and a str,
/// bin or ext by chunks totalling `len` bytes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Token<'a> {
    Nil,
    Bool(bool),
    /// Any integer that fits in i64.
    Int(i64),
    /// Only integers above i64::MAX.
    Uint(u64),
    Float(f64),
    Array(u32),
    Map(u32),
    Str(u32),
    Bin(u32),
    Ext(Ext),
    /// Bytes of the current str, bin or ext. Points into the fed bytes.
    Chunk(&'a [u8]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ext {
    pub r#type: i8,
    pub len: u32,
}

/// Streaming decoder. Feed bytes as they arrive, then call `next` until it
/// returns None. Values may be split anywhere across feeds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Decoder<'a> {
    pub input: &'a [u8],
    /// A header split across feeds, kept until it's complete.
    pending: [u8; 9],
    pending_len: u8,
    /// Bytes of str, bin or ext data still to hand out as chunks.
    remaining: u32,
}

impl<'a> Decoder<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// A decoder already fed `bytes`.
    pub fn of(bytes: &'a [u8]) -> Self {
        let mut d = Self::default();
        d.feed(bytes);
        d
    }

    /// Chunks point into `bytes`, so they live as long as `bytes` does.
    pub fn feed(&mut self, bytes: &'a [u8]) {
        debug_assert!(self.input.is_empty());
        self.input = bytes;
    }

    /// The next token, or None when the fed bytes are used up.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Token<'a>>, Error> {
        if self.input.is_empty() {
            return Ok(None);
        }

        if self.remaining > 0 {
            let n = (self.remaining as usize).min(self.input.len());
            self.remaining -= n as u32;
            return Ok(Some(Token::Chunk(self.take(n))));
        }

        let first = if self.pending_len > 0 {
            self.pending[0]
        } else {
            self.input[0]
        };
        let need = header_len(first).ok_or(Error::InvalidByte)? as usize;

        let token = if self.pending_len == 0 && self.input.len() >= need {
            decode_header(self.take(need))
        } else {
            // header split across feeds: collect it in `pending`
            let have = self.pending_len as usize;
            let n = (need - have).min(self.input.len());
            let bytes = self.take(n);
            self.pending[have..have + n].copy_from_slice(bytes);
            self.pending_len += n as u8;
            if (self.pending_len as usize) < need {
                return Ok(None);
            }
            self.pending_len = 0;
            let header = self.pending;
            decode_header(&header[..need])
        };

        match token {
            Token::Str(len) | Token::Bin(len) => self.remaining = len,
            Token::Ext(e) => self.remaining = e.len,
            _ => {}
        }
        Ok(Some(token))
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (head, rest) = self.input.split_at(n);
        self.input = rest;
        head
    }
}

/// Header size in bytes, including the type byte. None for 0xc1, which is
/// never used.
fn header_len(b: u8) -> Option<u8> {
    Some(match b {
        0x00..=0xbf | 0xc0 | 0xc2 | 0xc3 | 0xe0..=0xff => 1,
        0xc1 => return None,
        0xc4 | 0xcc | 0xd0 | 0xd9 => 2, // 8-bit length or value
        0xc5 | 0xcd | 0xd1 | 0xda | 0xdc | 0xde => 3, // 16-bit
        0xc6 | 0xca | 0xce | 0xd2 | 0xdb | 0xdd | 0xdf => 5, // 32-bit
        0xcb | 0xcf | 0xd3 => 9,        // 64-bit
        0xc7 => 3,                      // ext 8: length, then type
        0xc8 => 4,                      // ext 16
        0xc9 => 6,                      // ext 32
        0xd4..=0xd8 => 2,               // fixext: type only
    })
}

/// Decodes a complete header, as sized by `header_len`.
fn decode_header<'a>(h: &[u8]) -> Token<'a> {
    let be8 = || h[1];
    let be16 = || u16::from_be_bytes([h[1], h[2]]);
    let be32 = || u32::from_be_bytes([h[1], h[2], h[3], h[4]]);
    let be64 = || u64::from_be_bytes(h[1..9].try_into().unwrap());
    match h[0] {
        0x00..=0x7f => Token::Int(h[0] as i64),
        0x80..=0x8f => Token::Map((h[0] & 0x0f) as u32),
        0x90..=0x9f => Token::Array((h[0] & 0x0f) as u32),
        0xa0..=0xbf => Token::Str((h[0] & 0x1f) as u32),
        0xc0 => Token::Nil,
        0xc2 => Token::Bool(false),
        0xc3 => Token::Bool(true),
        0xc4 => Token::Bin(be8() as u32),
        0xc5 => Token::Bin(be16() as u32),
        0xc6 => Token::Bin(be32()),
        0xc7 => Token::Ext(Ext {
            len: be8() as u32,
            r#type: h[2] as i8,
        }),
        0xc8 => Token::Ext(Ext {
            len: be16() as u32,
            r#type: h[3] as i8,
        }),
        0xc9 => Token::Ext(Ext {
            len: be32(),
            r#type: h[5] as i8,
        }),
        0xca => Token::Float(f32::from_bits(be32()) as f64),
        0xcb => Token::Float(f64::from_bits(be64())),
        0xcc => Token::Int(be8() as i64),
        0xcd => Token::Int(be16() as i64),
        0xce => Token::Int(be32() as i64),
        0xcf => {
            let n = be64();
            if n <= i64::MAX as u64 {
                Token::Int(n as i64)
            } else {
                Token::Uint(n)
            }
        }
        0xd0 => Token::Int(be8() as i8 as i64),
        0xd1 => Token::Int(be16() as i16 as i64),
        0xd2 => Token::Int(be32() as i32 as i64),
        0xd3 => Token::Int(be64() as i64),
        0xd4..=0xd8 => Token::Ext(Ext {
            len: 1u32 << (h[0] - 0xd4),
            r#type: h[1] as i8,
        }),
        0xd9 => Token::Str(be8() as u32),
        0xda => Token::Str(be16() as u32),
        0xdb => Token::Str(be32()),
        0xdc => Token::Array(be16() as u32),
        0xdd => Token::Array(be32()),
        0xde => Token::Map(be16() as u32),
        0xdf => Token::Map(be32()),
        0xe0..=0xff => Token::Int(h[0] as i8 as i64),
        0xc1 => unreachable!(), // rejected by header_len
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_int(n: i64, expected: &[u8]) {
        let mut w = Vec::new();
        write_int(&mut w, n);
        assert_eq!(w, expected, "{n}");
    }

    #[test]
    fn nil_and_bool() {
        let mut w = Vec::new();
        write_nil(&mut w);
        write_bool(&mut w, true);
        write_bool(&mut w, false);
        assert_eq!(w, [0xc0, 0xc3, 0xc2]);
    }

    #[test]
    fn int_boundaries() {
        expect_int(0, &[0x00]);
        expect_int(127, &[0x7f]);
        expect_int(128, &[0xcc, 0x80]);
        expect_int(255, &[0xcc, 0xff]);
        expect_int(256, &[0xcd, 0x01, 0x00]);
        expect_int(65535, &[0xcd, 0xff, 0xff]);
        expect_int(65536, &[0xce, 0x00, 0x01, 0x00, 0x00]);
        expect_int(4294967295, &[0xce, 0xff, 0xff, 0xff, 0xff]);
        expect_int(
            4294967296,
            &[0xcf, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00],
        );
        expect_int(
            i64::MAX,
            &[0xcf, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );

        expect_int(-1, &[0xff]);
        expect_int(-32, &[0xe0]);
        expect_int(-33, &[0xd0, 0xdf]);
        expect_int(-128, &[0xd0, 0x80]);
        expect_int(-129, &[0xd1, 0xff, 0x7f]);
        expect_int(-32768, &[0xd1, 0x80, 0x00]);
        expect_int(-32769, &[0xd2, 0xff, 0xff, 0x7f, 0xff]);
        expect_int(i32::MIN as i64, &[0xd2, 0x80, 0x00, 0x00, 0x00]);
        expect_int(
            i32::MIN as i64 - 1,
            &[0xd3, 0xff, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff],
        );
        expect_int(
            i64::MIN,
            &[0xd3, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        );
    }

    #[test]
    fn float() {
        let mut w = Vec::new();
        write_float(&mut w, 1.5);
        assert_eq!(w, [0xcb, 0x3f, 0xf8, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn str_boundaries() {
        let s = [b'x'; 65536];
        // length, then the header bytes expected before the data
        let cases: [(usize, &[u8]); 7] = [
            (0, &[0xa0]),
            (31, &[0xbf]),
            (32, &[0xd9, 32]),
            (255, &[0xd9, 0xff]),
            (256, &[0xda, 0x01, 0x00]),
            (65535, &[0xda, 0xff, 0xff]),
            (65536, &[0xdb, 0x00, 0x01, 0x00, 0x00]),
        ];
        for (len, header) in cases {
            let mut w = Vec::new();
            write_str(&mut w, &s[..len]);
            assert_eq!(&w[..header.len()], header);
            assert_eq!(w.len(), header.len() + len);
        }
    }

    #[test]
    fn array_and_map_header_boundaries() {
        type Write = fn(&mut Vec<u8>, u32);
        let cases: [(Write, u32, &[u8]); 7] = [
            (write_array_header, 15, &[0x9f]),
            (write_array_header, 16, &[0xdc, 0x00, 0x10]),
            (write_array_header, 65536, &[0xdd, 0x00, 0x01, 0x00, 0x00]),
            (write_map_header, 0, &[0x80]),
            (write_map_header, 15, &[0x8f]),
            (write_map_header, 16, &[0xde, 0x00, 0x10]),
            (write_map_header, 65536, &[0xdf, 0x00, 0x01, 0x00, 0x00]),
        ];
        for (f, len, expected) in cases {
            let mut w = Vec::new();
            f(&mut w, len);
            assert_eq!(w, expected);
        }
    }

    #[test]
    fn an_rpc_request() {
        // [0, 1, "nvim_input", ["<C-a>"]]
        let mut w = Vec::new();
        write_array_header(&mut w, 4);
        write_int(&mut w, 0);
        write_int(&mut w, 1);
        write_str(&mut w, "nvim_input");
        write_array_header(&mut w, 1);
        write_str(&mut w, "<C-a>");
        assert_eq!(w, b"\x94\x00\x01\xaanvim_input\x91\xa5<C-a>");
    }

    /// Decodes `parts`, fed one after another, into a line of text. Chunks are
    /// written raw, so a str reads the same however its bytes were split.
    fn expect_decoded(parts: &[&[u8]], expected: &[u8]) {
        let mut w: Vec<u8> = Vec::new();
        let mut d = Decoder::new();
        for part in parts {
            d.feed(part);
            while let Some(token) = d.next().unwrap() {
                match token {
                    Token::Nil => w.extend_from_slice(b"nil "),
                    Token::Bool(b) => w.extend_from_slice(format!("{b} ").as_bytes()),
                    Token::Int(n) => w.extend_from_slice(format!("{n} ").as_bytes()),
                    Token::Uint(n) => w.extend_from_slice(format!("u{n} ").as_bytes()),
                    Token::Float(f) => w.extend_from_slice(format!("{f} ").as_bytes()),
                    Token::Array(n) => w.extend_from_slice(format!("[{n}] ").as_bytes()),
                    Token::Map(n) => w.extend_from_slice(format!("{{{n}}} ").as_bytes()),
                    Token::Str(n) => w.extend_from_slice(format!("str{n}:").as_bytes()),
                    Token::Bin(n) => w.extend_from_slice(format!("bin{n}:").as_bytes()),
                    Token::Ext(e) => {
                        w.extend_from_slice(format!("ext{}/{}:", e.r#type, e.len).as_bytes())
                    }
                    Token::Chunk(c) => w.extend_from_slice(c),
                }
            }
        }
        assert_eq!(w, expected, "{:?}", String::from_utf8_lossy(&w));
    }

    #[test]
    fn decode_scalars() {
        expect_decoded(&[b"\xc0\xc2\xc3"], b"nil false true ");
        expect_decoded(&[b"\x00\x7f\xff\xe0"], b"0 127 -1 -32 ");
        expect_decoded(
            &[b"\xcc\x80\xcd\x01\x00\xd0\xdf\xd1\xff\x7f"],
            b"128 256 -33 -129 ",
        );
        expect_decoded(
            &[b"\xcf\x7f\xff\xff\xff\xff\xff\xff\xff"],
            b"9223372036854775807 ",
        );
        expect_decoded(
            &[b"\xcf\xff\xff\xff\xff\xff\xff\xff\xff"],
            b"u18446744073709551615 ",
        );
        expect_decoded(
            &[b"\xd3\x80\x00\x00\x00\x00\x00\x00\x00"],
            b"-9223372036854775808 ",
        );
        expect_decoded(
            &[
                b"\xca\x3f\xc0\x00\x00",
                b"\xcb\x3f\xf8\x00\x00\x00\x00\x00\x00",
            ],
            b"1.5 1.5 ",
        );
    }

    #[test]
    fn decode_containers_and_blobs() {
        expect_decoded(&[b"\x92\x01\x02"], b"[2] 1 2 ");
        expect_decoded(&[b"\x81\xa1k\x01"], b"{1} str1:k1 ");
        expect_decoded(&[b"\xa0\x01"], b"str0:1 ");
        expect_decoded(&[b"\xc4\x02ab"], b"bin2:ab");
        // Neovim sends handles as ext: buffer (0), window (1), tabpage (2)
        expect_decoded(&[b"\xd4\x01\x05"], b"ext1/1:\x05");
        expect_decoded(&[b"\xc7\x02\x02\x01\x02"], b"ext2/2:\x01\x02");
    }

    #[test]
    fn decode_neovims_reply() {
        // nvim_eval("1+2") → [1, 1, nil, 3]
        expect_decoded(&[b"\x94\x01\x01\xc0\x03"], b"[4] 1 1 nil 3 ");
    }

    #[test]
    fn decode_split_at_every_byte() {
        let msg: &[u8] = b"\x93\xcd\x01\x00\xa5hello\xcb\x3f\xf8\x00\x00\x00\x00\x00\x00";
        let expected = b"[3] 256 str5:hello1.5 ";
        expect_decoded(&[msg], expected);
        for i in 1..msg.len() {
            expect_decoded(&[&msg[..i], &msg[i..]], expected);
        }

        let one_byte: Vec<&[u8]> = (0..msg.len()).map(|i| &msg[i..i + 1]).collect();
        expect_decoded(&one_byte, expected);
    }

    #[test]
    fn decode_what_the_encoder_wrote() {
        let mut w = Vec::new();
        write_array_header(&mut w, 4);
        write_int(&mut w, -129);
        write_str(&mut w, "nvim_input");
        write_nil(&mut w);
        write_map_header(&mut w, 0);
        expect_decoded(&[&w], b"[4] -129 str10:nvim_inputnil {0} ");
    }

    #[test]
    fn decode_rejects_0xc1() {
        let mut d = Decoder::of(b"\xc1");
        assert_eq!(d.next(), Err(Error::InvalidByte));
    }
}
