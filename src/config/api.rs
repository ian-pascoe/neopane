//! What the config's Lua gets (#93 decisions 4 and 6): the libraries, set up
//! so `require` reads only the config's `lua/` directory and output goes to
//! the log, and the global `<name>` with `default`, `run` and `which`.

use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mlua::{Lua, Table, Value};

use super::wire::Request;
use crate::name::NAME;

/// What `run` keeps of each output.
const MAX_OUTPUT: usize = 1 << 20;
/// What `run` leaves the config to answer in.
const MARGIN: Duration = Duration::from_millis(250);
/// How long a timed-out command has between SIGTERM and SIGKILL.
const TERM_GRACE: Duration = Duration::from_millis(250);

/// Sets up `lua` for the config `request` names, which has until
/// `deadline`.
pub(super) fn install(lua: &Lua, request: &Request, deadline: Instant) -> mlua::Result<()> {
    let file = Path::new(OsStr::from_bytes(&request.file));
    let dir = file.parent().unwrap_or(Path::new("/"));
    let dir = lua.create_string(dir.as_os_str().as_bytes())?;
    let g = lua.globals();

    let package: Table = g.raw_get("package")?;
    let mut path = dir.as_bytes().to_vec();
    path.extend_from_slice(b"/lua/?.lua;");
    path.extend_from_slice(&dir.as_bytes());
    path.extend_from_slice(b"/lua/?/init.lua");
    package.raw_set("path", lua.create_string(&path)?)?;
    package.raw_set("cpath", "")?;
    package.raw_set("loadlib", Value::Nil)?;
    let loaders: Table = package.raw_get("loaders")?;
    loaders.raw_set(3, Value::Nil)?;
    loaders.raw_set(4, Value::Nil)?;

    // print, io.write, io.stdout and io.stderr write to stderr at once; the
    // daemon logs it line by line
    let out = lua.create_function(|_, s: mlua::LuaString| {
        let _ = std::io::stderr().write_all(&s.as_bytes());
        Ok(())
    })?;
    let api = lua.create_table()?;
    api.raw_set("os", std::env::consts::OS)?;
    api.raw_set("version", request.version.as_str())?;
    api.raw_set("config_dir", dir)?;
    api.raw_set("default", lua.create_function(default)?)?;
    api.raw_set("builtin", lua.create_table()?)?;
    api.raw_set("inside", lua.create_table()?)?;
    let time = request.time;
    let run = lua.create_function(move |lua, (argv, opts): (Value, Value)| {
        Ok(match run(lua, argv, opts, deadline, time)? {
            Ok(result) => (Value::Table(result), None),
            Err(why) => (Value::Nil, Some(format!("{NAME}.run: {why}"))),
        })
    })?;
    let which = lua.create_function(|lua, program: Value| {
        Ok(match which(lua, program)? {
            Ok(found) => (found, None),
            Err(why) => (Value::Nil, Some(format!("{NAME}.which: {why}"))),
        })
    })?;
    lua.load(PRELUDE)
        .set_name("=prelude")
        .call::<()>((out, &api, run, which))?;
    g.raw_set(NAME, &api)?;
    let loaded: Table = package.raw_get("loaded")?;
    loaded.raw_set(NAME, api)?;
    Ok(())
}

/// Sends the config's output to stderr, and wraps `run` and `which`.
const PRELUDE: &str = include_str!("prelude.lua");

/// `<name>.default(attach)`: never raises.
fn default(lua: &Lua, attach: Value) -> mlua::Result<Value> {
    let env = match attach {
        Value::Table(t) => t.raw_get::<Value>("env").ok(),
        _ => None,
    };
    let var = |name: &str| -> Option<Vec<u8>> {
        match &env {
            Some(Value::Table(env)) => match env.raw_get::<Value>(name) {
                Ok(Value::String(s)) => Some(s.as_bytes().to_vec()),
                _ => None,
            },
            _ => None,
        }
    };
    Ok(match super::default_name(|name| var(name)) {
        Some(name) => Value::String(lua.create_string(name)?),
        None => Value::Nil,
    })
}

/// Lua's name for `v`'s type.
fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Integer(_) | Value::Number(_) => "number",
        Value::Thread(_) => "coroutine",
        Value::UserData(_) | Value::LightUserData(_) => "userdata",
        other => other.type_name(),
    }
}

/// `<name>.which(program)`, or why its argument is bad.
fn which(lua: &Lua, program: Value) -> mlua::Result<Result<Value, String>> {
    let Value::String(program) = program else {
        return Ok(Err(format!(
            "expected a string, got {}",
            type_name(&program)
        )));
    };
    let program = program.as_bytes().to_vec();
    let found = find_program(
        &program,
        std::env::var_os("PATH").as_deref(),
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    );
    Ok(Ok(match found {
        Some(p) => Value::String(lua.create_string(p.as_os_str().as_bytes())?),
        None => Value::Nil,
    }))
}

/// The first executable `program` on `path`, made absolute from `cwd`; with
/// a `/`, `program` itself if it is executable.
pub(super) fn find_program(program: &[u8], path: Option<&OsStr>, cwd: &Path) -> Option<PathBuf> {
    if program.is_empty() || program.contains(&0) {
        return None;
    }
    let program = Path::new(OsStr::from_bytes(program));
    if program.as_os_str().as_bytes().contains(&b'/') {
        return executable(program).then(|| program.to_path_buf());
    }
    let path = path?;
    path.as_bytes()
        .split(|&b| b == b':')
        .map(|dir| match dir {
            b"" => cwd.to_path_buf(),
            dir => cwd.join(OsStr::from_bytes(dir)),
        })
        .map(|dir| dir.join(program))
        .find(|p| executable(p))
}

fn executable(path: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: c is a valid C string; access only checks permissions.
    let x = unsafe { libc::access(c.as_ptr(), libc::X_OK) } == 0;
    x && path.metadata().is_ok_and(|m| m.is_file())
}

/// `<name>.run`'s options.
#[derive(Default)]
struct RunOptions {
    timeout: Option<f64>,
    cwd: Option<Vec<u8>>,
    env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    stdin: Vec<u8>,
}

/// `argv` as a non-empty list of strings without NUL, or why not.
fn argv_of(argv: &Value) -> Result<Vec<Vec<u8>>, String> {
    let Value::Table(t) = argv else {
        return Err(format!(
            "expected a list of strings, got {}",
            type_name(argv)
        ));
    };
    let n = t.raw_len();
    let mut words = Vec::with_capacity(n);
    for i in 1..=n {
        match t.raw_get::<Value>(i) {
            Ok(Value::String(s)) if s.as_bytes().contains(&0) => {
                return Err(format!("argv[{i}] holds a NUL byte"));
            }
            Ok(Value::String(s)) => words.push(s.as_bytes().to_vec()),
            Ok(v) => return Err(format!("argv[{i}] is a {}, not a string", type_name(&v))),
            Err(e) => return Err(e.to_string()),
        }
    }
    let count = t.pairs::<Value, Value>().count();
    if count != n {
        return Err("argv must be a list, with keys 1 to n only".into());
    }
    match words.first() {
        None => Err("argv is empty".into()),
        Some(p) if p.is_empty() => Err("argv[1], the program, is empty".into()),
        Some(_) => Ok(words),
    }
}

fn options_of(opts: &Value) -> Result<RunOptions, String> {
    let mut o = RunOptions::default();
    let t = match opts {
        Value::Nil => return Ok(o),
        Value::Table(t) => t,
        other => return Err(format!("opts must be a table, got {}", type_name(other))),
    };
    for pair in t.pairs::<Value, Value>() {
        let (k, v) = pair.map_err(|e| e.to_string())?;
        let key = match &k {
            Value::String(s) => s.to_string_lossy(),
            other => return Err(format!("unknown option {other:?}")),
        };
        match (key.as_str(), v) {
            ("timeout", Value::Number(n)) if n > 0.0 && n.is_finite() => o.timeout = Some(n),
            ("timeout", Value::Integer(n)) if n > 0 => o.timeout = Some(n as f64),
            ("timeout", v) => {
                return Err(format!(
                    "opts.timeout must be a number of seconds over 0, got {}",
                    shown(&v)
                ));
            }
            ("cwd", Value::String(s)) if !s.as_bytes().contains(&0) => {
                o.cwd = Some(s.as_bytes().to_vec())
            }
            ("cwd", v) => return Err(format!("opts.cwd must be a path, got {}", shown(&v))),
            ("stdin", Value::String(s)) => o.stdin = s.as_bytes().to_vec(),
            ("stdin", v) => return Err(format!("opts.stdin must be a string, got {}", shown(&v))),
            ("env", Value::Table(env)) => {
                for pair in env.pairs::<Value, Value>() {
                    let (name, value) = pair.map_err(|e| e.to_string())?;
                    let name = match name {
                        Value::String(s)
                            if !s.as_bytes().is_empty()
                                && !s.as_bytes().contains(&b'=')
                                && !s.as_bytes().contains(&0) =>
                        {
                            s.as_bytes().to_vec()
                        }
                        other => {
                            return Err(format!(
                                "opts.env's keys must be variable names, got {}",
                                shown(&other)
                            ));
                        }
                    };
                    let value = match value {
                        Value::String(s) if !s.as_bytes().contains(&0) => {
                            Some(s.as_bytes().to_vec())
                        }
                        Value::Boolean(false) => None,
                        other => {
                            return Err(format!(
                                "opts.env.{} must be a string or false, got {}",
                                String::from_utf8_lossy(&name),
                                shown(&other)
                            ));
                        }
                    };
                    o.env.push((name, value));
                }
            }
            ("env", v) => return Err(format!("opts.env must be a table, got {}", shown(&v))),
            (other, _) => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

fn shown(v: &Value) -> String {
    match v {
        Value::Number(n) => format!("{n}"),
        Value::Integer(n) => format!("{n}"),
        other => type_name(other).to_string(),
    }
}

/// Seconds as Lua would show them, to two places at most: `1`, `0.75`.
pub(super) fn seconds(d: Duration) -> String {
    let s = format!("{:.2}", d.as_secs_f64());
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// What running a command came to.
struct RunResult {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    error: Option<String>,
}

/// `<name>.run(argv, opts)` (#93 decision 25), for a config that has
/// `time` in all, until `deadline`; or why its arguments are bad.
fn run(
    lua: &Lua,
    argv: Value,
    opts: Value,
    deadline: Instant,
    time: Duration,
) -> mlua::Result<Result<Table, String>> {
    let argv = match argv_of(&argv) {
        Ok(a) => a,
        Err(why) => return Ok(Err(why)),
    };
    let opts = match options_of(&opts) {
        Ok(o) => o,
        Err(why) => return Ok(Err(why)),
    };
    let left = deadline
        .saturating_duration_since(Instant::now())
        .saturating_sub(MARGIN);
    let result = if left.is_zero() {
        RunResult {
            code: None,
            stdout: vec![],
            stderr: vec![],
            error: Some(format!("no time left (the config has {} s)", seconds(time))),
        }
    } else {
        let asked = opts
            .timeout
            .map_or(Duration::from_secs(1), Duration::from_secs_f64);
        run_command(&argv, &opts, asked.min(left))
    };
    let t = lua.create_table()?;
    t.raw_set("ok", result.code == Some(0) && result.error.is_none())?;
    t.raw_set("code", result.code)?;
    t.raw_set("stdout", lua.create_string(&result.stdout)?)?;
    t.raw_set("stderr", lua.create_string(&result.stderr)?)?;
    t.raw_set("error", result.error)?;
    Ok(Ok(t))
}

fn run_command(argv: &[Vec<u8>], opts: &RunOptions, timeout: Duration) -> RunResult {
    let started = Instant::now();
    let mut command = Command::new(OsStr::from_bytes(&argv[0]));
    command
        .args(argv[1..].iter().map(|w| OsStr::from_bytes(w)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &opts.cwd {
        command.current_dir(OsStr::from_bytes(cwd));
    }
    for (name, value) in &opts.env {
        match value {
            Some(v) => command.env(OsStr::from_bytes(name), OsStr::from_bytes(v)),
            None => command.env_remove(OsStr::from_bytes(name)),
        };
    }
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            return RunResult {
                code: None,
                stdout: vec![],
                stderr: vec![],
                error: Some(format!(
                    "can't start: {}: {e}",
                    String::from_utf8_lossy(&argv[0])
                )),
            };
        }
    };
    let mut stdin = child.stdin.take();
    let input = opts.stdin.clone();
    if !input.is_empty() {
        let mut w = stdin.take();
        thread::spawn(move || {
            if let Some(w) = w.as_mut() {
                let _ = w.write_all(&input);
            }
        });
    }
    drop(stdin);
    let stdout = collect(child.stdout.take());
    let stderr = collect(child.stderr.take());

    let end = started + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= end => {
                timed_out = true;
                break stop(&mut child);
            }
            Ok(None) => thread::sleep(Duration::from_millis(2)),
            Err(_) => break None,
        }
    };
    // what the command wrote before it ended, unless something it started
    // keeps its pipes open past the timeout
    for reader in [&stdout, &stderr] {
        while !reader.1.lock().unwrap().1 && Instant::now() < end {
            thread::sleep(Duration::from_millis(2));
        }
    }
    let error = if timed_out {
        Some(format!("timed out after {} s", seconds(timeout)))
    } else {
        status
            .and_then(|s| s.signal())
            .map(|sig| format!("killed by signal {sig}"))
    };
    let code = if timed_out {
        None
    } else {
        status.and_then(|s| s.code())
    };
    let take = |r: &Collected| std::mem::take(&mut r.1.lock().unwrap().0);
    RunResult {
        code,
        stdout: take(&stdout),
        stderr: take(&stderr),
        error,
    }
}

/// A reader thread's bytes so far, and whether it reached the end.
type Collected = (Option<thread::JoinHandle<()>>, Arc<Mutex<(Vec<u8>, bool)>>);

/// Reads `pipe` on a thread, keeping the first [`MAX_OUTPUT`] bytes.
fn collect(pipe: Option<impl Read + Send + 'static>) -> Collected {
    let shared = Arc::new(Mutex::new((Vec::new(), pipe.is_none())));
    let Some(mut pipe) = pipe else {
        return (None, shared);
    };
    let to = Arc::clone(&shared);
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut got = to.lock().unwrap();
                    let room = MAX_OUTPUT - got.0.len().min(MAX_OUTPUT);
                    got.0.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        to.lock().unwrap().1 = true;
    });
    (Some(handle), shared)
}

/// SIGTERM, then SIGKILL [`TERM_GRACE`] later; reaps it.
fn stop(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let pid = child.id() as libc::pid_t;
    // SAFETY: signals our own child, not yet reaped.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let end = Instant::now() + TERM_GRACE;
    while Instant::now() < end {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s);
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = child.kill();
    child.wait().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_are_shown_short() {
        assert_eq!(seconds(Duration::from_secs(1)), "1");
        assert_eq!(seconds(Duration::from_millis(750)), "0.75");
        assert_eq!(seconds(Duration::from_millis(2500)), "2.5");
        assert_eq!(seconds(Duration::from_secs(3)), "3");
    }

    #[test]
    fn which_searches_path() {
        let dir = std::env::temp_dir().join(format!("which-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let plain = bin.join("plain");
        std::fs::write(&plain, "").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = OsStr::new("/nonexistent:bin");
        assert_eq!(find_program(b"tool", Some(path), &dir), Some(tool.clone()));
        assert_eq!(find_program(b"plain", Some(path), &dir), None);
        assert_eq!(find_program(b"nope", Some(path), &dir), None);
        assert_eq!(find_program(b"tool", None, &dir), None);
        assert_eq!(find_program(b"", Some(path), &dir), None);
        // a directory isn't a program
        assert_eq!(find_program(b"bin", Some(OsStr::new("")), &dir), None);
        // with a slash: that path only
        let named = tool.as_os_str().as_bytes();
        assert_eq!(find_program(named, None, &dir), Some(tool.clone()));
        assert_eq!(find_program(b"/nonexistent/tool", Some(path), &dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
