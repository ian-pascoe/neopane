//! The config child: `<name> config-child`, which the daemon starts to run
//! the user's `init.lua` (#93 decisions 3, 4, 6 and 7). It reads a
//! [`Request`] on fd 3, runs the file and its `provider`, and answers on
//! fd 4. Its stdout and stderr are the log's `config: ` lines.

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use mlua::{Lua, LuaOptions, MultiValue, StdLib, Table, Value};

use super::api;
use super::tree::{Limit, MAX_BYTES, MAX_LEVELS, Marker, Tree, sort_pairs};
use super::wire::{Answer, Attach, Called, Ran, Request};
use crate::attach::build;
use crate::name::NAME;

/// The fd the request comes in on.
pub const REQUEST_FD: i32 = 3;
/// The fd the answer goes out on.
pub const ANSWER_FD: i32 = 4;

/// Runs the child: reads the request, runs the file, answers. An error
/// means there was no request to answer.
pub fn main() -> Result<(), String> {
    let started = Instant::now();
    let not_ours = || format!("config-child is started by {NAME} itself");
    // SAFETY: fcntl only inspects the fds.
    let open = |fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1;
    if !open(REQUEST_FD) || !open(ANSWER_FD) {
        return Err(not_ours());
    }
    // SAFETY: both fds are open and nothing else here owns them.
    let (mut request_in, mut answer_out) =
        unsafe { (File::from_raw_fd(REQUEST_FD), File::from_raw_fd(ANSWER_FD)) };
    // what the config starts mustn't hold the answer's pipe open
    // SAFETY: sets a flag on an fd we own.
    unsafe { libc::fcntl(ANSWER_FD, libc::F_SETFD, libc::FD_CLOEXEC) };
    let mut bytes = Vec::new();
    request_in
        .read_to_end(&mut bytes)
        .map_err(|e| format!("can't read the request: {e}"))?;
    drop(request_in);
    let request = Request::decode(&bytes).map_err(|e| format!("bad request: {e}"))?;
    let answer = Answer {
        build: build().to_string(),
        ran: run_file(&request, started),
    };
    let body = answer.encode();
    let mut framed = (body.len() as u32).to_le_bytes().to_vec();
    framed.extend(body);
    answer_out
        .write_all(&framed)
        .map_err(|e| format!("can't answer: {e}"))
}

/// Runs the file and calls its `provider`.
fn run_file(request: &Request, started: Instant) -> Ran {
    let path = Path::new(OsStr::from_bytes(&request.file));
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => return Ran::CantRead(e.to_string()),
    };
    // every library, debug too: no sandbox (ADR 0007); api::install keeps
    // require to the config's lua/ directory and refuses C modules
    // SAFETY: mlua's "unsafe" is only that C modules could load, and
    // api::install removes every way to load one before any config runs.
    let lua = unsafe { Lua::unsafe_new_with(StdLib::ALL, LuaOptions::default()) };
    let deadline = started + request.time;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Err(e) = api::install(&lua, request, deadline) {
        return Ran::Failed(message(&e));
    }
    let chunk = match lua
        .load(&source)
        .set_name(format!("@{file_name}"))
        .into_function()
    {
        Ok(f) => f,
        Err(e) => return Ran::LoadFailed(message(&e)),
    };
    let returned: MultiValue = match chunk.call(()) {
        Ok(v) => v,
        Err(e) => return Ran::Failed(message(&e)),
    };
    let first = returned.into_iter().next().unwrap_or(Value::Nil);
    // the keys together can be over the limits too: Answer::encode sends
    // that as Ran::FileOver
    let file = match top_level(&first) {
        Ok(file) => file,
        Err(limit) => return Ran::FileOver(limit),
    };
    let provider = match &first {
        Value::Table(t) => match t.raw_get::<Value>("provider") {
            Ok(Value::Function(f)) => call_provider(&lua, &f, &request.attach),
            _ => Called::NotCalled,
        },
        _ => Called::NotCalled,
    };
    Ran::Returned { file, provider }
}

fn call_provider(lua: &Lua, provider: &mlua::Function, attach: &Attach) -> Called {
    let attach = match attach_table(lua, attach) {
        Ok(t) => t,
        Err(e) => return Called::Raised(message(&e)),
    };
    match provider.call::<MultiValue>(attach) {
        Ok(values) => {
            let first = values.into_iter().next().unwrap_or(Value::Nil);
            match to_tree(&first) {
                Ok(tree) => Called::Returned(tree),
                Err(limit) => Called::Over(limit),
            }
        }
        Err(e) => Called::Raised(message(&e)),
    }
}

/// The `attach` table (#93 decision 7).
pub(super) fn attach_table(lua: &Lua, attach: &Attach) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    for (name, value) in &attach.env {
        // the first occurrence wins
        if env.raw_get::<Value>(lua.create_string(name)?)?.is_nil() {
            env.raw_set(lua.create_string(name)?, lua.create_string(value)?)?;
        }
    }
    let t = lua.create_table()?;
    t.raw_set("env", env)?;
    t.raw_set("cwd", lua.create_string(&attach.cwd)?)?;
    t.raw_set("tty", lua.create_string(&attach.tty)?)?;
    t.raw_set("pid", attach.pid)?;
    t.raw_set("daemon", attach.daemon.as_str())?;
    t.raw_set("provider", attach.provider.as_deref())?;
    Ok(t)
}

/// A Lua error's message, without mlua's wrapping or a traceback.
pub(super) fn message(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::CallbackError { cause, .. } => return message(cause),
        mlua::Error::WithContext { cause, .. } => return message(cause),
        other => other.to_string(),
    };
    match text.find("\nstack traceback:") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// The file's first value, one level deep: the checker reads only its keys
/// and the types of their values, so tables in it go empty and string
/// values short. Its keys can still be over the limits.
fn top_level(value: &Value) -> Result<Tree, Limit> {
    let Value::Table(t) = value else {
        return leaf(value, true);
    };
    let mut pairs = Vec::new();
    for (k, v) in t.pairs::<Value, Value>().filter_map(Result::ok) {
        pairs.push((leaf(&k, false)?, leaf(&v, true)?));
    }
    sort_pairs(&mut pairs);
    Ok(Tree::Table(pairs))
}

/// A value without what it holds: a table goes empty; with `short`, a
/// string too.
fn leaf(value: &Value, short: bool) -> Result<Tree, Limit> {
    match value {
        Value::Table(_) => Ok(Tree::Table(vec![])),
        Value::String(_) if short => Ok(Tree::String(vec![])),
        other => to_tree(other),
    }
}

/// A Lua value as a plain tree, within the limits: read with raw `pairs`,
/// a table inside itself marked as a cycle.
pub(super) fn to_tree(value: &Value) -> Result<Tree, Limit> {
    let mut walk = Walk {
        path: Vec::new(),
        size: 0,
    };
    walk.tree(value)
}

struct Walk {
    /// The tables from the root down to here.
    path: Vec<*const c_void>,
    size: usize,
}

impl Walk {
    fn tree(&mut self, value: &Value) -> Result<Tree, Limit> {
        let tree = match value {
            Value::Nil => Tree::Nil,
            Value::Boolean(b) => Tree::Bool(*b),
            Value::Integer(i) => Tree::Number(*i as f64),
            Value::Number(n) => Tree::Number(*n),
            Value::String(s) => Tree::String(s.as_bytes().to_vec()),
            Value::Table(t) => return self.table(t),
            Value::Function(_) => Tree::Marker(Marker::Function),
            Value::Thread(_) => Tree::Marker(Marker::Coroutine),
            _ => Tree::Marker(Marker::Userdata),
        };
        self.add(tree.own_size())?;
        Ok(tree)
    }

    fn add(&mut self, n: usize) -> Result<(), Limit> {
        self.size += n;
        if self.size > MAX_BYTES {
            return Err(Limit::TooBig);
        }
        Ok(())
    }

    fn table(&mut self, t: &Table) -> Result<Tree, Limit> {
        let at = t.to_pointer();
        if self.path.contains(&at) {
            self.add(1)?;
            return Ok(Tree::Marker(Marker::Cycle));
        }
        if self.path.len() == MAX_LEVELS {
            return Err(Limit::TooDeep);
        }
        self.add(5)?;
        self.path.push(at);
        let mut pairs = Vec::new();
        for pair in t.pairs::<Value, Value>() {
            let Ok((k, v)) = pair else { continue };
            pairs.push((self.tree(&k)?, self.tree(&v)?));
        }
        self.path.pop();
        sort_pairs(&mut pairs);
        Ok(Tree::Table(pairs))
    }
}
