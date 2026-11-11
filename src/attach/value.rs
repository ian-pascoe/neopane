//! Control payloads as whole msgpack values: decoded into a tree, read by
//! key, and written back.

use super::Error;
use crate::nvim::msgpack::{self, Decoder, Token};

/// Containers nested deeper than this are refused, so a hostile payload
/// can't run the decoder out of stack. Nothing sent nests past 3.
const MAX_DEPTH: usize = 16;

/// One decoded msgpack value.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    /// Only integers above i64::MAX.
    Uint(u64),
    Float(f64),
    Str(Vec<u8>),
    Bin(Vec<u8>),
    Array(Vec<Value>),
    Map(Vec<(Value, Value)>),
    /// An ext value; its type and bytes are of no use here.
    Ext,
}

/// Decodes `payload`, which must hold exactly one value.
pub(crate) fn decode(payload: &[u8]) -> Result<Value, Error> {
    let mut d = Decoder::of(payload);
    let v = value(&mut d, 0)?;
    if !d.input.is_empty() {
        return Err(Error::Malformed("bytes after the payload's value"));
    }
    Ok(v)
}

fn token<'a>(d: &mut Decoder<'a>) -> Result<Token<'a>, Error> {
    match d.next() {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(Error::Malformed("payload ends inside a value")),
        Err(_) => Err(Error::Malformed("payload isn't msgpack")),
    }
}

/// The `len` bytes of a str, bin or ext.
fn blob(d: &mut Decoder<'_>, len: u32) -> Result<Vec<u8>, Error> {
    // the payload holds the whole value, so its bytes come as one chunk
    if len == 0 {
        return Ok(Vec::new());
    }
    match token(d)? {
        Token::Chunk(c) if c.len() == len as usize => Ok(c.to_vec()),
        _ => Err(Error::Malformed("payload ends inside a value")),
    }
}

fn value(d: &mut Decoder<'_>, depth: usize) -> Result<Value, Error> {
    Ok(match token(d)? {
        Token::Nil => Value::Nil,
        Token::Bool(b) => Value::Bool(b),
        Token::Int(n) => Value::Int(n),
        Token::Uint(n) => Value::Uint(n),
        Token::Float(f) => Value::Float(f),
        Token::Str(len) => Value::Str(blob(d, len)?),
        Token::Bin(len) => Value::Bin(blob(d, len)?),
        Token::Ext(e) => {
            blob(d, e.len)?;
            Value::Ext
        }
        Token::Array(len) => {
            if depth == MAX_DEPTH {
                return Err(Error::Malformed("payload nested too deep"));
            }
            // every value takes at least one byte, so a count beyond what's
            // left is a lie: don't reserve for it
            let mut items = Vec::with_capacity((len as usize).min(d.input.len()));
            for _ in 0..len {
                items.push(value(d, depth + 1)?);
            }
            Value::Array(items)
        }
        Token::Map(len) => {
            if depth == MAX_DEPTH {
                return Err(Error::Malformed("payload nested too deep"));
            }
            let mut pairs = Vec::with_capacity((len as usize).min(d.input.len() / 2));
            for _ in 0..len {
                let k = value(d, depth + 1)?;
                let v = value(d, depth + 1)?;
                pairs.push((k, v));
            }
            Value::Map(pairs)
        }
        Token::Chunk(_) => unreachable!("chunks only follow a str, bin or ext header"),
    })
}

/// A map of bin to bin, in order, as `env` is.
pub(crate) type BinMap = Vec<(Vec<u8>, Vec<u8>)>;

/// A control payload's map, read by `str` key. A key given twice keeps its
/// first value; keys nothing asks for are ignored.
pub(crate) struct Fields(Vec<(Vec<u8>, Value)>);

impl Fields {
    pub(crate) fn decode(payload: &[u8]) -> Result<Self, Error> {
        match decode(payload)? {
            Value::Map(pairs) => Self::of(pairs),
            _ => Err(Error::Malformed("payload isn't a map")),
        }
    }

    /// The fields of a nested map.
    fn of(pairs: Vec<(Value, Value)>) -> Result<Self, Error> {
        let mut out: Vec<(Vec<u8>, Value)> = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let Value::Str(k) = k else {
                return Err(Error::Malformed("a map key isn't a str"));
            };
            if !out.iter().any(|(seen, _)| *seen == k) {
                out.push((k, v));
            }
        }
        Ok(Self(out))
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(k, _)| k == key.as_bytes())
            .map(|(_, v)| v)
    }

    /// Present and not nil.
    fn opt(&self, key: &'static str) -> Option<&Value> {
        self.get(key).filter(|v| **v != Value::Nil)
    }

    fn req(&self, key: &'static str) -> Result<&Value, Error> {
        self.opt(key).ok_or(Error::Missing(key))
    }

    pub(crate) fn int(&self, key: &'static str) -> Result<i64, Error> {
        as_int(self.req(key)?, key)
    }

    pub(crate) fn opt_int(&self, key: &'static str) -> Result<Option<i64>, Error> {
        self.opt(key).map(|v| as_int(v, key)).transpose()
    }

    /// An int that fits `T`.
    pub(crate) fn int_in<T: TryFrom<i64>>(&self, key: &'static str) -> Result<T, Error> {
        T::try_from(self.int(key)?).map_err(|_| Error::Invalid(key))
    }

    pub(crate) fn opt_int_in<T: TryFrom<i64>>(
        &self,
        key: &'static str,
    ) -> Result<Option<T>, Error> {
        self.opt_int(key)?
            .map(|n| T::try_from(n).map_err(|_| Error::Invalid(key)))
            .transpose()
    }

    pub(crate) fn bool(&self, key: &'static str) -> Result<bool, Error> {
        match self.req(key)? {
            Value::Bool(b) => Ok(*b),
            _ => Err(Error::Invalid(key)),
        }
    }

    pub(crate) fn opt_bool(&self, key: &'static str) -> Result<Option<bool>, Error> {
        match self.opt(key) {
            None => Ok(None),
            Some(Value::Bool(b)) => Ok(Some(*b)),
            Some(_) => Err(Error::Invalid(key)),
        }
    }

    pub(crate) fn str(&self, key: &'static str) -> Result<String, Error> {
        as_str(self.req(key)?, key)
    }

    pub(crate) fn opt_str(&self, key: &'static str) -> Result<Option<String>, Error> {
        self.opt(key).map(|v| as_str(v, key)).transpose()
    }

    pub(crate) fn bin(&self, key: &'static str) -> Result<Vec<u8>, Error> {
        as_bin(self.req(key)?, key)
    }

    pub(crate) fn opt_bin(&self, key: &'static str) -> Result<Option<Vec<u8>>, Error> {
        self.opt(key).map(|v| as_bin(v, key)).transpose()
    }

    pub(crate) fn strs(&self, key: &'static str) -> Result<Vec<String>, Error> {
        as_array(self.req(key)?, key)?
            .iter()
            .map(|v| as_str(v, key))
            .collect()
    }

    pub(crate) fn opt_strs(&self, key: &'static str) -> Result<Option<Vec<String>>, Error> {
        self.opt(key)
            .map(|v| as_array(v, key)?.iter().map(|v| as_str(v, key)).collect())
            .transpose()
    }

    pub(crate) fn bins(&self, key: &'static str) -> Result<Vec<Vec<u8>>, Error> {
        as_array(self.req(key)?, key)?
            .iter()
            .map(|v| as_bin(v, key))
            .collect()
    }

    pub(crate) fn opt_bins(&self, key: &'static str) -> Result<Option<Vec<Vec<u8>>>, Error> {
        self.opt(key)
            .map(|v| as_array(v, key)?.iter().map(|v| as_bin(v, key)).collect())
            .transpose()
    }

    /// A nested map, read the same way.
    pub(crate) fn map(&self, key: &'static str) -> Result<Fields, Error> {
        match self.req(key)? {
            Value::Map(pairs) => Self::of(pairs.clone()),
            _ => Err(Error::Invalid(key)),
        }
    }

    /// A map of bin to bin, in order; a key given twice keeps its first value.
    pub(crate) fn bin_map(&self, key: &'static str) -> Result<BinMap, Error> {
        let Value::Map(pairs) = self.req(key)? else {
            return Err(Error::Invalid(key));
        };
        let mut out: BinMap = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let (k, v) = (as_bin(k, key)?, as_bin(v, key)?);
            if !out.iter().any(|(seen, _)| *seen == k) {
                out.push((k, v));
            }
        }
        Ok(out)
    }
}

fn as_int(v: &Value, key: &'static str) -> Result<i64, Error> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err(Error::Invalid(key)),
    }
}

fn as_str(v: &Value, key: &'static str) -> Result<String, Error> {
    match v {
        Value::Str(s) => String::from_utf8(s.clone()).map_err(|_| Error::Invalid(key)),
        _ => Err(Error::Invalid(key)),
    }
}

fn as_bin(v: &Value, key: &'static str) -> Result<Vec<u8>, Error> {
    match v {
        Value::Bin(b) => Ok(b.clone()),
        _ => Err(Error::Invalid(key)),
    }
}

fn as_array<'v>(v: &'v Value, key: &'static str) -> Result<&'v [Value], Error> {
    match v {
        Value::Array(items) => Ok(items),
        _ => Err(Error::Invalid(key)),
    }
}

/// Writes a control payload's map, leaving out the fields that are `None`.
#[derive(Default)]
pub(crate) struct MapWriter {
    count: u32,
    body: Vec<u8>,
}

impl MapWriter {
    fn key(&mut self, key: &str) -> &mut Vec<u8> {
        self.count += 1;
        msgpack::write_str(&mut self.body, key);
        &mut self.body
    }

    pub(crate) fn int(&mut self, key: &str, n: impl Into<i64>) -> &mut Self {
        msgpack::write_int(self.key(key), n);
        self
    }

    pub(crate) fn bool(&mut self, key: &str, b: bool) -> &mut Self {
        msgpack::write_bool(self.key(key), b);
        self
    }

    pub(crate) fn str(&mut self, key: &str, s: &str) -> &mut Self {
        msgpack::write_str(self.key(key), s);
        self
    }

    pub(crate) fn bin(&mut self, key: &str, b: &[u8]) -> &mut Self {
        msgpack::write_bin(self.key(key), b);
        self
    }

    pub(crate) fn int_pair(
        &mut self,
        key: &str,
        a: impl Into<i64>,
        b: impl Into<i64>,
    ) -> &mut Self {
        let w = self.key(key);
        msgpack::write_array_header(w, 2);
        msgpack::write_int(w, a);
        msgpack::write_int(w, b);
        self
    }

    pub(crate) fn strs(&mut self, key: &str, items: &[String]) -> &mut Self {
        let w = self.key(key);
        msgpack::write_array_header(w, len32(items.len()));
        for s in items {
            msgpack::write_str(w, s);
        }
        self
    }

    pub(crate) fn bins(&mut self, key: &str, items: &[Vec<u8>]) -> &mut Self {
        let w = self.key(key);
        msgpack::write_array_header(w, len32(items.len()));
        for b in items {
            msgpack::write_bin(w, b);
        }
        self
    }

    pub(crate) fn bin_map(&mut self, key: &str, pairs: &[(Vec<u8>, Vec<u8>)]) -> &mut Self {
        let w = self.key(key);
        msgpack::write_map_header(w, len32(pairs.len()));
        for (k, v) in pairs {
            msgpack::write_bin(w, k);
            msgpack::write_bin(w, v);
        }
        self
    }

    pub(crate) fn map(&mut self, key: &str, inner: MapWriter) -> &mut Self {
        let w = self.key(key);
        w.extend_from_slice(&inner.finish());
        self
    }

    /// The whole map: its header, then its pairs.
    pub(crate) fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() + 5);
        msgpack::write_map_header(&mut out, self.count);
        out.extend_from_slice(&self.body);
        out
    }
}

/// Panics past u32::MAX items, which no frame within the limits can hold.
fn len32(n: usize) -> u32 {
    u32::try_from(n).expect("too many items for msgpack")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bins_of_every_length_header() {
        for len in [0, 1, 255, 256, 65535, 65536] {
            let b = vec![7u8; len];
            let mut w = MapWriter::default();
            w.bin("b", &b);
            assert_eq!(b, Fields::decode(&w.finish()).unwrap().bin("b").unwrap());
        }
    }

    #[test]
    fn first_key_wins_and_unknown_keys_are_ignored() {
        let mut w = MapWriter::default();
        w.int("n", 1).int("n", 2).str("other", "x");
        let f = Fields::decode(&w.finish()).unwrap();
        assert_eq!(1, f.int("n").unwrap());
    }

    #[test]
    fn a_missing_or_nil_field_is_named() {
        let mut w = MapWriter::default();
        w.int("here", 1);
        let mut payload = w.finish();
        // add `gone: nil`
        payload[0] += 1;
        msgpack::write_str(&mut payload, "gone");
        msgpack::write_nil(&mut payload);
        let f = Fields::decode(&payload).unwrap();
        assert_eq!(Err(Error::Missing("gone")), f.int("gone"));
        assert_eq!(Err(Error::Missing("absent")), f.str("absent"));
        assert_eq!(Ok(None), f.opt_int("gone"));
        assert_eq!(Err(Error::Invalid("here")), f.str("here"));
    }

    #[test]
    fn malformed_payloads_are_refused() {
        assert!(Fields::decode(&[]).is_err(), "empty");
        assert!(Fields::decode(&[0x90]).is_err(), "an array");
        assert!(Fields::decode(&[0x81, 0x01, 0x01]).is_err(), "int key");
        assert!(Fields::decode(&[0x81, 0xa1, b'k']).is_err(), "cut short");
        assert!(Fields::decode(&[0x80, 0x80]).is_err(), "trailing bytes");
        assert!(Fields::decode(&[0xc1]).is_err(), "0xc1");
        // a count far beyond the bytes there
        assert!(Fields::decode(&[0xdf, 0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing() {
        let mut deep = vec![0x91u8; 100_000];
        deep.push(0xc0);
        assert!(decode(&deep).is_err());
        let mut fine = vec![0x91u8; MAX_DEPTH];
        fine.push(0xc0);
        assert!(decode(&fine).is_ok());
    }
}
