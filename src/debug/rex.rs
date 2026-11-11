//! `neorex debug-rex SOCKET`: talks to Rex directly. Attaches to this
//! session, opens a split running `neorex pane SOCKET`, takes that pane's
//! terminal over as debug-panes does, and prints Rex's events. Ctrl+C quits.

use std::fs;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use serde_json::{Map, Value, json};

use super::Result;
use super::pane_host::PaneHost;
use super::panes as debug_panes;
use crate::attach::{Transport, attach_command};
use crate::daemon::event_loop::{Event, Loop};
use crate::name::NAME;
use crate::rex::client::{self, Client, Message, ResponseResult};

const TOKEN_SIGNAL: usize = 0;
const TOKEN_SERVER: usize = 1;
const TOKEN_REX: usize = 2;
const TOKEN_PANES: usize = 3;

/// What `run` refuses to start without.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    SocketPathNotAbsolute,
    NoRexServer,
    NoRexSession,
    RexRequestFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub fn run(socket_path: impl AsRef<Path>) -> Result<()> {
    let socket_path = socket_path.as_ref();
    // the pane starts elsewhere, so the path must work from any directory
    if !socket_path.is_absolute() {
        return Err(Error::SocketPathNotAbsolute.into());
    }
    let server_url = std::env::var("REX_SERVER").map_err(|_| Error::NoRexServer)?;
    let session_id = std::env::var("REX_SESSION").map_err(|_| Error::NoRexSession)?;
    let anchor_id = std::env::var("REX_BLOCK").ok();
    let exe = std::env::current_exe()?;

    let mut event_loop = Loop::init()?;
    event_loop.add_signal(libc::SIGINT, TOKEN_SIGNAL)?;
    event_loop.add_signal(libc::SIGTERM, TOKEN_SIGNAL)?;

    let server = debug_panes::listen(socket_path)?;
    let result = serve(
        &mut event_loop,
        &server,
        socket_path,
        &server_url,
        &session_id,
        anchor_id.as_deref(),
        &exe,
    );
    let _ = fs::remove_file(socket_path);
    result
}

fn serve(
    event_loop: &mut Loop,
    server: &std::os::unix::net::UnixListener,
    socket_path: &Path,
    server_url: &str,
    session_id: &str,
    anchor_id: Option<&str>,
    exe: &Path,
) -> Result<()> {
    event_loop.add_read(server.as_raw_fd(), TOKEN_SERVER)?;

    let mut rex = Client::connect(server_url, NAME)?;
    event_loop.add_read(rex.fd(), TOKEN_REX)?;
    eprintln!("debug-rex: connected to {server_url}. Ctrl+C quits.");

    let mut host = PaneHost::new(TOKEN_PANES, "debug-rex");

    let attach_id = rex.request("session.attach", Some(&json!({ "session_id": session_id })))?;
    let mut split_id: Option<u64> = None;

    let mut events = [Event::default(); 16];
    loop {
        let mut pending_accept = false;
        for e in event_loop.wait(&mut events)? {
            match e.token {
                TOKEN_SIGNAL => {
                    eprintln!("closing all panes");
                    return Ok(());
                }
                TOKEN_SERVER => pending_accept = true,
                TOKEN_REX => {
                    if !rex.read()? {
                        eprintln!("rex: server hung up");
                        return Ok(());
                    }
                    while let Some(m) = rex.next()? {
                        match m {
                            Message::Response(r) => {
                                match &r.result {
                                    ResponseResult::Ok(v) => {
                                        eprintln!("rex: response {}: {v}", r.id)
                                    }
                                    ResponseResult::Err(err) => eprintln!(
                                        "rex: response {} failed: {} ({}): {}",
                                        r.id, err.code, err.status, err.message
                                    ),
                                }
                                let failed = matches!(r.result, ResponseResult::Err(_));
                                if failed && (r.id == attach_id || Some(r.id) == split_id) {
                                    return Err(Error::RexRequestFailed.into());
                                }
                                // creating made us the owner, which sizes it; let the app size it instead
                                if Some(r.id) == split_id
                                    && let ResponseResult::Ok(v) = &r.result
                                {
                                    rex.request(
                                        "session.disown_block",
                                        Some(&json!({
                                            "session_id": session_id,
                                            "block_ids": client::get(v, "block_ids")
                                                .cloned()
                                                .unwrap_or(Value::Null),
                                        })),
                                    )?;
                                }
                                // the split names the session, so it waits for the attach
                                if r.id == attach_id {
                                    let mut payload = Map::new();
                                    payload.insert("session_id".into(), json!(session_id));
                                    // a missing anchor is left out, not sent as null
                                    if let Some(anchor_id) = anchor_id {
                                        payload.insert("anchor_id".into(), json!(anchor_id));
                                    }
                                    payload.insert("direction".into(), json!("horizontal"));
                                    payload.insert("side".into(), json!("after"));
                                    payload.insert(
                                        "layout".into(),
                                        json!({ "block": {
                                            "flavor": "com.superlogical.terminal.shell",
                                            "label": NAME,
                                            "options": {
                                                "shell": "none",
                                                "command": attach_command(
                                                    exe.as_ref(),
                                                    socket_path.as_ref(),
                                                    &host.mint(),
                                                    Transport::Fd,
                                                )
                                                .iter()
                                                .map(|w| w.to_string_lossy())
                                                .collect::<Vec<_>>(),
                                            },
                                        } }),
                                    );
                                    split_id = Some(rex.request(
                                        "session.new_split",
                                        Some(&Value::Object(payload)),
                                    )?);
                                }
                            }
                            Message::Event(ev) => {
                                eprintln!("rex: event {} {}: {}", ev.seq, ev.name, ev.payload)
                            }
                            Message::Closed(c) => {
                                match c.code {
                                    Some(code) => eprint!("rex: closed {code} "),
                                    None => eprint!("rex: closed null "),
                                }
                                eprintln!("{}", String::from_utf8_lossy(&c.reason));
                                return Ok(());
                            }
                        }
                    }
                }
                token => {
                    if host.owns(token) {
                        host.handle(event_loop, token)?;
                    }
                }
            }
        }
        if pending_accept {
            host.accept(event_loop, server)?;
        }
    }
}
