//! What the daemon and the config child send each other, in frozen
//! encodings: the daemon may be older or newer than the binary at its path.
//!
//! The daemon writes a [`Request`] to the child's fd 3 and closes it; the
//! child answers on fd 4 with an [`Answer`], prefixed by its length as a
//! `u32`. Both start with a version byte, 1. Strings are a `u32` length and
//! bytes; an optional string is a byte 0 or 1, then the string if 1.

use std::time::Duration;

use super::tree::{DecodeError, Limit, Reader, Tree, put_bytes};

const VERSION: u8 = 1;

/// The most bytes an answer may take: a tree of 1 MiB, the file's table and
/// the build, with room to spare.
pub const MAX_ANSWER: usize = 3 << 20;

/// The choosing attach, as the config's `provider` sees it (#93 decision 7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attach {
    /// The hello's environment, raw bytes, each name once.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    /// Absolute.
    pub cwd: Vec<u8>,
    /// Empty when there's none.
    pub tty: Vec<u8>,
    /// The attaching helper's pid.
    pub pid: u32,
    /// The daemon's name.
    pub daemon: String,
    /// The `--provider` hint; never `none` or empty.
    pub provider: Option<String>,
}

/// What the child runs, and with what.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The config file, absolute.
    pub file: Vec<u8>,
    /// The daemon's version, as `<name>.version`.
    pub version: String,
    pub attach: Attach,
    /// The time the config has, counted from its start.
    pub time: Duration,
}

/// What the child answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// The child's build.
    pub build: String,
    pub ran: Ran,
}

/// How running the file went.
#[derive(Debug, Clone, PartialEq)]
pub enum Ran {
    /// The file couldn't be read.
    CantRead(String),
    /// It didn't compile.
    LoadFailed(String),
    /// Running it raised.
    Failed(String),
    /// The file's own table was over the limits, its keys too many or too
    /// long. [`Answer::encode`] sends a `Returned` whose `file` is over them
    /// as this.
    FileOver(Limit),
    /// It returned `file`: its first value, with each table in a table
    /// sent empty, as only the top level is read. `provider` is what came
    /// of calling its `provider`.
    Returned { file: Tree, provider: Called },
}

/// What came of calling `provider(attach)`.
#[derive(Debug, Clone, PartialEq)]
pub enum Called {
    /// There was no `provider` function to call.
    NotCalled,
    /// Its first value.
    Returned(Tree),
    Raised(String),
    /// Its first value couldn't be sent.
    Over(Limit),
}

impl Request {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![VERSION];
        put_bytes(&mut out, &self.file);
        put_bytes(&mut out, self.version.as_bytes());
        let a = &self.attach;
        out.extend_from_slice(&(a.env.len() as u32).to_le_bytes());
        for (k, v) in &a.env {
            put_bytes(&mut out, k);
            put_bytes(&mut out, v);
        }
        put_bytes(&mut out, &a.cwd);
        put_bytes(&mut out, &a.tty);
        out.extend_from_slice(&a.pid.to_le_bytes());
        put_bytes(&mut out, a.daemon.as_bytes());
        put_option(&mut out, a.provider.as_deref());
        let ms = u32::try_from(self.time.as_millis()).unwrap_or(u32::MAX);
        out.extend_from_slice(&ms.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Request, DecodeError> {
        let mut r = Reader::new(bytes);
        version(&mut r)?;
        let file = r.bytes()?;
        let version = string(&mut r)?;
        let n = r.u32()? as usize;
        let mut env = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            env.push((r.bytes()?, r.bytes()?));
        }
        let attach = Attach {
            env,
            cwd: r.bytes()?,
            tty: r.bytes()?,
            pid: r.u32()?,
            daemon: string(&mut r)?,
            provider: option(&mut r)?,
        };
        let time = Duration::from_millis(r.u32()?.into());
        Ok(Request {
            file,
            version,
            attach,
            time,
        })
    }
}

const CANT_READ: u8 = 0;
const LOAD_FAILED: u8 = 1;
const FAILED: u8 = 2;
const RETURNED: u8 = 3;
const FILE_OVER: u8 = 4;

const NOT_CALLED: u8 = 0;
const CALLED_RETURNED: u8 = 1;
const RAISED: u8 = 2;
const TOO_BIG: u8 = 3;
const TOO_DEEP: u8 = 4;

impl Answer {
    /// The answer, without its length. A tree over the limits goes as
    /// [`Ran::FileOver`] or [`Called::Over`].
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![VERSION];
        put_bytes(&mut out, self.build.as_bytes());
        match &self.ran {
            Ran::CantRead(m) => message(&mut out, CANT_READ, m),
            Ran::LoadFailed(m) => message(&mut out, LOAD_FAILED, m),
            Ran::Failed(m) => message(&mut out, FAILED, m),
            Ran::FileOver(limit) => {
                out.push(FILE_OVER);
                out.push(limit_tag(*limit));
            }
            Ran::Returned { file, provider } => {
                let file = match super::tree::encode(file) {
                    Ok(bytes) => bytes,
                    Err(limit) => {
                        out.push(FILE_OVER);
                        out.push(limit_tag(limit));
                        return out;
                    }
                };
                out.push(RETURNED);
                out.extend(file);
                let called = match provider {
                    Called::Returned(tree) => super::tree::encode(tree).map_err(Called::Over),
                    other => Err(other.clone()),
                };
                match called {
                    Ok(bytes) => {
                        out.push(CALLED_RETURNED);
                        out.extend(bytes);
                    }
                    Err(Called::Raised(m)) => message(&mut out, RAISED, &m),
                    Err(Called::Over(limit)) => out.push(limit_tag(limit)),
                    Err(_) => out.push(NOT_CALLED),
                }
            }
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Answer, DecodeError> {
        let mut r = Reader::new(bytes);
        version(&mut r)?;
        let build = string(&mut r)?;
        let ran = match r.u8()? {
            CANT_READ => Ran::CantRead(string(&mut r)?),
            LOAD_FAILED => Ran::LoadFailed(string(&mut r)?),
            FAILED => Ran::Failed(string(&mut r)?),
            RETURNED => {
                let file = r.tree()?;
                let provider = match r.u8()? {
                    NOT_CALLED => Called::NotCalled,
                    CALLED_RETURNED => Called::Returned(r.tree()?),
                    RAISED => Called::Raised(string(&mut r)?),
                    tag @ (TOO_BIG | TOO_DEEP) => Called::Over(limit_of(tag)?),
                    tag => return Err(DecodeError(format!("unknown call tag {tag}"))),
                };
                Ran::Returned { file, provider }
            }
            FILE_OVER => Ran::FileOver(limit_of(r.u8()?)?),
            tag => return Err(DecodeError(format!("unknown outcome tag {tag}"))),
        };
        if r.at != bytes.len() {
            return Err(DecodeError("bytes after the answer".into()));
        }
        Ok(Answer { build, ran })
    }
}

fn limit_tag(limit: Limit) -> u8 {
    match limit {
        Limit::TooBig => TOO_BIG,
        Limit::TooDeep => TOO_DEEP,
    }
}

fn limit_of(tag: u8) -> Result<Limit, DecodeError> {
    match tag {
        TOO_BIG => Ok(Limit::TooBig),
        TOO_DEEP => Ok(Limit::TooDeep),
        tag => Err(DecodeError(format!("unknown limit tag {tag}"))),
    }
}

fn message(out: &mut Vec<u8>, tag: u8, m: &str) {
    out.push(tag);
    put_bytes(out, m.as_bytes());
}

fn put_option(out: &mut Vec<u8>, s: Option<&str>) {
    match s {
        None => out.push(0),
        Some(s) => {
            out.push(1);
            put_bytes(out, s.as_bytes());
        }
    }
}

fn version(r: &mut Reader<'_>) -> Result<(), DecodeError> {
    match r.u8()? {
        VERSION => Ok(()),
        v => Err(DecodeError(format!("version {v}, not {VERSION}"))),
    }
}

/// A string, its bad UTF-8 replaced.
fn string(r: &mut Reader<'_>) -> Result<String, DecodeError> {
    Ok(String::from_utf8_lossy(&r.bytes()?).into_owned())
}

fn option(r: &mut Reader<'_>) -> Result<Option<String>, DecodeError> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(string(r)?)),
        b => Err(DecodeError(format!("bad option byte {b}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tree::Marker;

    #[test]
    fn a_request_round_trips() {
        let request = Request {
            file: b"/home/u/.config/x/init.lua".to_vec(),
            version: "0.1.0".into(),
            attach: Attach {
                env: vec![(b"A".to_vec(), vec![0xff, b'='])],
                cwd: b"/tmp".to_vec(),
                tty: vec![],
                pid: 42,
                daemon: "work".into(),
                provider: Some("tmux".into()),
            },
            time: Duration::from_millis(3000),
        };
        assert_eq!(Request::decode(&request.encode()), Ok(request));
    }

    #[test]
    fn answers_round_trip() {
        let file = Tree::Table(vec![(
            Tree::String(b"provider".to_vec()),
            Tree::Marker(Marker::Function),
        )]);
        for ran in [
            Ran::CantRead("no such file".into()),
            Ran::LoadFailed("x".into()),
            Ran::Failed("y".into()),
            Ran::FileOver(Limit::TooBig),
            Ran::Returned {
                file: file.clone(),
                provider: Called::NotCalled,
            },
            Ran::Returned {
                file: file.clone(),
                provider: Called::Returned(Tree::String(b"rex".to_vec())),
            },
            Ran::Returned {
                file: file.clone(),
                provider: Called::Raised("boom".into()),
            },
            Ran::Returned {
                file: file.clone(),
                provider: Called::Over(Limit::TooDeep),
            },
        ] {
            let answer = Answer {
                build: "b".into(),
                ran,
            };
            assert_eq!(Answer::decode(&answer.encode()), Ok(answer));
        }
    }
}
