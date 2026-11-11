//! The plain tree: what a config's `provider` returned, as data the daemon
//! can check with its own rules (#93 decision 3). Its encoding is frozen: a
//! daemon reads the trees of config children newer than itself, after an
//! upgrade in place.
//!
//! Each value is a tag byte, then its body, little-endian:
//!
//! | Tag | Value | Body |
//! |---|---|---|
//! | 0 | nil | — |
//! | 1, 2 | false, true | — |
//! | 3 | number | 8 bytes, an IEEE 754 double |
//! | 4 | string | `u32` length, then the bytes |
//! | 5 | table | `u32` pair count, then each key and value |
//! | 6–9 | a function, userdata, coroutine, cycle | — |
//!
//! A tree is at most [`MAX_BYTES`] encoded and [`MAX_LEVELS`] tables deep.

use std::fmt;

/// The most bytes an encoded tree may take: 1 MiB.
pub const MAX_BYTES: usize = 1 << 20;
/// The deepest tables may nest: a table in a table is two levels.
pub const MAX_LEVELS: usize = 16;

/// A Lua value as plain data.
#[derive(Debug, Clone, PartialEq)]
pub enum Tree {
    Nil,
    Bool(bool),
    /// Every Lua 5.1 number is a double.
    Number(f64),
    /// Raw bytes.
    String(Vec<u8>),
    /// Each key and its value, keys keeping their kinds, in the order the
    /// child sorted them (`sort_pairs`).
    Table(Vec<(Tree, Tree)>),
    /// What plain data can't hold.
    Marker(Marker),
}

/// What stands in for a value plain data can't hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Function,
    Userdata,
    Coroutine,
    /// A table inside itself.
    Cycle,
}

/// Why a tree can't be sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Over [`MAX_BYTES`].
    TooBig,
    /// Over [`MAX_LEVELS`].
    TooDeep,
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Limit::TooBig => write!(f, "more than {} MiB", MAX_BYTES >> 20),
            Limit::TooDeep => write!(f, "tables nested more than {MAX_LEVELS} levels deep"),
        }
    }
}

/// Bytes that aren't an encoded tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

const NIL: u8 = 0;
const FALSE: u8 = 1;
const TRUE: u8 = 2;
const NUMBER: u8 = 3;
const STRING: u8 = 4;
const TABLE: u8 = 5;
const FUNCTION: u8 = 6;
const USERDATA: u8 = 7;
const COROUTINE: u8 = 8;
const CYCLE: u8 = 9;

impl Tree {
    /// Its type as Lua's `type()` names it; `cycle` for a cycle.
    pub fn type_name(&self) -> &'static str {
        match self {
            Tree::Nil => "nil",
            Tree::Bool(_) => "boolean",
            Tree::Number(_) => "number",
            Tree::String(_) => "string",
            Tree::Table(_) => "table",
            Tree::Marker(Marker::Function) => "function",
            Tree::Marker(Marker::Userdata) => "userdata",
            Tree::Marker(Marker::Coroutine) => "coroutine",
            Tree::Marker(Marker::Cycle) => "cycle",
        }
    }

    /// The value at string key `key`, if this is a table holding it.
    pub fn get(&self, key: &str) -> Option<&Tree> {
        match self {
            Tree::Table(pairs) => pairs.iter().find_map(|(k, v)| match k {
                Tree::String(s) if s == key.as_bytes() => Some(v),
                _ => None,
            }),
            _ => None,
        }
    }

    /// What it adds to an encoding, not counting what it holds.
    pub fn own_size(&self) -> usize {
        match self {
            Tree::Number(_) => 9,
            Tree::String(s) => 5 + s.len(),
            Tree::Table(_) => 5,
            _ => 1,
        }
    }
}

/// Puts a table's pairs in the order the child sends them: numbers
/// ascending, then strings by bytes, then booleans, then the rest as found.
pub fn sort_pairs(pairs: &mut [(Tree, Tree)]) {
    fn rank(k: &Tree) -> u8 {
        match k {
            Tree::Number(_) => 0,
            Tree::String(_) => 1,
            Tree::Bool(_) => 2,
            _ => 3,
        }
    }
    pairs.sort_by(|(a, _), (b, _)| {
        rank(a).cmp(&rank(b)).then_with(|| match (a, b) {
            (Tree::Number(x), Tree::Number(y)) => x.total_cmp(y),
            (Tree::String(x), Tree::String(y)) => x.cmp(y),
            (Tree::Bool(x), Tree::Bool(y)) => x.cmp(y),
            _ => std::cmp::Ordering::Equal,
        })
    });
}

/// `tree` in the frozen encoding, within the limits.
pub fn encode(tree: &Tree) -> Result<Vec<u8>, Limit> {
    let mut out = Vec::new();
    write(&mut out, tree, 0)?;
    Ok(out)
}

fn write(out: &mut Vec<u8>, tree: &Tree, levels: usize) -> Result<(), Limit> {
    if out.len() + tree.own_size() > MAX_BYTES {
        return Err(Limit::TooBig);
    }
    match tree {
        Tree::Nil => out.push(NIL),
        Tree::Bool(false) => out.push(FALSE),
        Tree::Bool(true) => out.push(TRUE),
        Tree::Number(n) => {
            out.push(NUMBER);
            out.extend_from_slice(&n.to_bits().to_le_bytes());
        }
        Tree::String(s) => {
            out.push(STRING);
            out.extend_from_slice(&(s.len() as u32).to_le_bytes());
            out.extend_from_slice(s);
        }
        Tree::Table(pairs) => {
            if levels == MAX_LEVELS {
                return Err(Limit::TooDeep);
            }
            out.push(TABLE);
            out.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
            for (k, v) in pairs {
                write(out, k, levels + 1)?;
                write(out, v, levels + 1)?;
            }
        }
        Tree::Marker(m) => out.push(match m {
            Marker::Function => FUNCTION,
            Marker::Userdata => USERDATA,
            Marker::Coroutine => COROUTINE,
            Marker::Cycle => CYCLE,
        }),
    }
    Ok(())
}

/// The tree `bytes` encode, all of them, within the limits.
pub fn decode(bytes: &[u8]) -> Result<Tree, DecodeError> {
    let mut r = Reader { bytes, at: 0 };
    let tree = r.tree()?;
    if r.at != bytes.len() {
        return Err(DecodeError(format!(
            "{} bytes after the tree",
            bytes.len() - r.at
        )));
    }
    Ok(tree)
}

/// Reads frozen-encoding values from a byte slice.
pub(crate) struct Reader<'a> {
    pub(crate) bytes: &'a [u8],
    pub(crate) at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, at: 0 }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|&e| e <= self.bytes.len())
            .ok_or_else(|| DecodeError("ends too soon".into()))?;
        let s = &self.bytes[self.at..end];
        self.at = end;
        Ok(s)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub(crate) fn bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let n = self.u32()? as usize;
        Ok(self.take(n)?.to_vec())
    }

    pub(crate) fn tree(&mut self) -> Result<Tree, DecodeError> {
        let start = self.at;
        let tree = self.value(0)?;
        if self.at - start > MAX_BYTES {
            return Err(DecodeError(format!("a tree over {MAX_BYTES} bytes")));
        }
        Ok(tree)
    }

    fn value(&mut self, levels: usize) -> Result<Tree, DecodeError> {
        Ok(match self.u8()? {
            NIL => Tree::Nil,
            FALSE => Tree::Bool(false),
            TRUE => Tree::Bool(true),
            NUMBER => Tree::Number(f64::from_bits(u64::from_le_bytes(
                self.take(8)?.try_into().unwrap(),
            ))),
            STRING => Tree::String(self.bytes()?),
            TABLE => {
                if levels == MAX_LEVELS {
                    return Err(DecodeError(format!(
                        "tables nested over {MAX_LEVELS} levels"
                    )));
                }
                let n = self.u32()? as usize;
                // each pair takes at least two bytes
                if n > (self.bytes.len() - self.at) / 2 {
                    return Err(DecodeError("ends too soon".into()));
                }
                let mut pairs = Vec::with_capacity(n);
                for _ in 0..n {
                    let k = self.value(levels + 1)?;
                    let v = self.value(levels + 1)?;
                    pairs.push((k, v));
                }
                Tree::Table(pairs)
            }
            FUNCTION => Tree::Marker(Marker::Function),
            USERDATA => Tree::Marker(Marker::Userdata),
            COROUTINE => Tree::Marker(Marker::Coroutine),
            CYCLE => Tree::Marker(Marker::Cycle),
            tag => return Err(DecodeError(format!("unknown tag {tag}"))),
        })
    }
}

/// Writes frozen-encoding values.
pub(crate) fn put_bytes(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> Tree {
        Tree::String(text.as_bytes().to_vec())
    }

    fn round_trip(tree: &Tree) -> Tree {
        decode(&encode(tree).unwrap()).unwrap()
    }

    #[test]
    fn values_round_trip() {
        let tree = Tree::Table(vec![
            (Tree::Number(1.0), s("sh")),
            (Tree::Number(2.5), Tree::Number(-0.25)),
            (s("name"), Tree::String(vec![0xff, 0, b'x'])),
            (s("on"), Tree::Bool(true)),
            (s("off"), Tree::Bool(false)),
            (s("none"), Tree::Nil),
            (Tree::Bool(true), Tree::Table(vec![])),
            (s("f"), Tree::Marker(Marker::Function)),
            (s("u"), Tree::Marker(Marker::Userdata)),
            (s("co"), Tree::Marker(Marker::Coroutine)),
            (s("me"), Tree::Marker(Marker::Cycle)),
        ]);
        assert_eq!(round_trip(&tree), tree);
    }

    /// The encoding is frozen: these bytes must always mean this tree.
    #[test]
    fn the_encoding_is_frozen() {
        let tree = Tree::Table(vec![
            (Tree::Number(1.0), s("a")),
            (s("k"), Tree::Bool(true)),
            (s("f"), Tree::Marker(Marker::Function)),
            (s("c"), Tree::Marker(Marker::Cycle)),
        ]);
        let bytes = [
            5, 4, 0, 0, 0, // a table of 4 pairs
            3, 0, 0, 0, 0, 0, 0, 0xf0, 0x3f, 4, 1, 0, 0, 0, b'a', // [1] = "a"
            4, 1, 0, 0, 0, b'k', 2, // k = true
            4, 1, 0, 0, 0, b'f', 6, // f = a function
            4, 1, 0, 0, 0, b'c', 9, // c = a cycle
        ];
        assert_eq!(encode(&tree).unwrap(), bytes);
        assert_eq!(decode(&bytes).unwrap(), tree);
    }

    fn nested(levels: usize) -> Tree {
        let mut tree = Tree::Table(vec![]);
        for _ in 1..levels {
            tree = Tree::Table(vec![(s("t"), tree)]);
        }
        tree
    }

    #[test]
    fn at_most_sixteen_levels() {
        assert_eq!(round_trip(&nested(16)), nested(16));
        assert_eq!(encode(&nested(17)), Err(Limit::TooDeep));
    }

    #[test]
    fn at_most_one_mib() {
        let fits = s(&"x".repeat(MAX_BYTES - 5));
        assert_eq!(round_trip(&fits), fits);
        assert_eq!(encode(&s(&"x".repeat(MAX_BYTES - 4))), Err(Limit::TooBig));
    }

    #[test]
    fn bad_bytes_are_refused() {
        assert!(decode(&[]).is_err());
        assert!(decode(&[42]).is_err());
        assert!(decode(&[4, 9, 0, 0, 0, b'x']).is_err());
        assert!(decode(&[5, 0xff, 0xff, 0xff, 0xff]).is_err());
        assert!(decode(&[0, 0]).is_err());
    }

    #[test]
    fn pairs_sort_numbers_then_strings() {
        let mut pairs = vec![
            (s("b"), Tree::Nil),
            (Tree::Bool(true), Tree::Nil),
            (Tree::Number(2.0), Tree::Nil),
            (s("a"), Tree::Nil),
            (Tree::Number(1.0), Tree::Nil),
        ];
        sort_pairs(&mut pairs);
        let keys: Vec<Tree> = pairs.into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            keys,
            [
                Tree::Number(1.0),
                Tree::Number(2.0),
                s("a"),
                s("b"),
                Tree::Bool(true)
            ]
        );
    }
}
