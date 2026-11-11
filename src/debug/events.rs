//! `neorex debug-events`: attaches to Neovim as a UI and prints every redraw
//! event it sends. Keys and pastes go to Neovim. Ctrl+Q or :q quits.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};

use super::Result;
use crate::name::NAME;
use crate::nvim::forwarder::Forwarder;
use crate::nvim::msgpack::{Decoder, Token};
use crate::nvim::nvim::{Nvim, UiOptions};
use crate::nvim::rpc;
use crate::term::input;
use crate::term::raw::Saved;
use crate::term::signals::Signals;
use crate::term::terminal::Terminal;

// no alternate screen, so the events stay in scrollback
const MODES_ON: &str = concat!("\x1b[?2004h\x1b[?1004h", input::kitty_push_literal!());
const MODES_OFF: &str = concat!(input::kitty_pop_literal!(), "\x1b[?1004l\x1b[?2004l");

/// Runs `nvim --embed` plus `nvim_args`.
pub fn run<S: AsRef<OsStr>>(nvim_args: &[S]) -> Result<()> {
    let saved = Saved::raw(libc::STDIN_FILENO)?;
    let mut term = Terminal::new(libc::STDIN_FILENO);
    let result = run_in(&mut term, nvim_args);
    let _ = term.flush();
    saved.restore();
    result
}

fn run_in<S: AsRef<OsStr>>(term: &mut Terminal, nvim_args: &[S]) -> Result<()> {
    let mut signals = Signals::init(&[libc::SIGWINCH])?;

    term.writer.extend_from_slice(MODES_ON.as_bytes());
    let result = drive(term, &mut signals, nvim_args);
    term.writer.extend_from_slice(MODES_OFF.as_bytes());
    result
}

fn drive<S: AsRef<OsStr>>(
    term: &mut Terminal,
    signals: &mut Signals,
    nvim_args: &[S],
) -> Result<()> {
    let mut argv: Vec<OsString> = vec!["nvim".into(), "--embed".into()];
    argv.extend(nvim_args.iter().map(|a| a.as_ref().to_os_string()));

    let mut nvim = Nvim::spawn(&argv)?;

    let ws = term.size()?;
    nvim.ui_attach(ws.ws_col as u32, ws.ws_row as u32, UiOptions::default())?;
    write!(
        term.writer,
        "debug-events: attached at {}x{}. Ctrl+Q or :q quits.\r\n",
        ws.ws_col, ws.ws_row
    )?;
    term.flush()?;

    let mut forwarder = Forwarder::default();
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: term.fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: signals.read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: nvim.read_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of three pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::other("PollFailed").into());
        }

        if fds[1].revents != 0 {
            let fired = signals.drain()?;
            if fired.contains(libc::SIGWINCH) {
                let size = term.size()?;
                nvim.ui_try_resize(size.ws_col as u32, size.ws_row as u32)?;
            }
        }

        if fds[2].revents != 0 {
            if !nvim.read()? {
                term.writer.extend_from_slice(b"neovim exited\r\n");
                return Ok(());
            }
            while let Some(frame) = nvim.next()? {
                print_message(&mut nvim, &mut term.writer, &frame.message()?)?;
            }
            term.flush()?;
        }

        if fds[0].revents != 0 {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::read(term.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::other("ReadFailed").into());
            }
            if n == 0 {
                return Ok(());
            }
            if !forwarder.feed(&mut nvim, &buf[..n as usize])?.more {
                return Ok(());
            }
        }
    }
}

/// One line per redraw event; errors and other messages in brief.
fn print_message(nvim: &mut Nvim, out: &mut Vec<u8>, msg: &rpc::Message) -> Result<()> {
    match msg {
        rpc::Message::Response { err, .. } => {
            if let Some(e) = err {
                out.extend_from_slice(b"error: ");
                write_escaped(out, e);
                out.extend_from_slice(b"\r\n");
            }
        }
        rpc::Message::Request { id, method, .. } => {
            // nothing here serves requests; answer so Neovim doesn't wait
            out.extend_from_slice(b"request ");
            out.extend_from_slice(method);
            out.extend_from_slice(b" (refused)\r\n");
            nvim.respond_error(*id, &format!("{NAME} debug-events serves no requests"))?;
        }
        rpc::Message::Notification { method, params } => {
            if *method != b"redraw" {
                out.extend_from_slice(b"notification ");
                out.extend_from_slice(method);
                out.extend_from_slice(b"\r\n");
                return Ok(());
            }
            // params: [[name, args, args, ...], ...]
            let mut d = *params;
            let events = rpc::expect_array(&mut d)?;
            for _ in 0..events {
                let len = rpc::expect_array(&mut d)?;
                if len == 0 {
                    return Err(rpc::Error::BadMessage.into());
                }
                let name = rpc::expect_str(&mut d)?;
                for _ in 1..len {
                    out.extend_from_slice(name);
                    out.push(b' ');
                    print_value(&mut d, out)?;
                    out.extend_from_slice(b"\r\n");
                }
            }
        }
    }
    Ok(())
}

/// Prints one value compactly, as in `[1, "a", {k: true}]`.
fn print_value(d: &mut Decoder, out: &mut Vec<u8>) -> rpc::Result<()> {
    match d.next()?.ok_or(rpc::Error::BadMessage)? {
        Token::Nil => out.extend_from_slice(b"nil"),
        Token::Bool(b) => write!(out, "{b}").expect("writing to a Vec"),
        Token::Int(n) => write!(out, "{n}").expect("writing to a Vec"),
        Token::Uint(n) => write!(out, "{n}").expect("writing to a Vec"),
        Token::Float(f) => {
            // non-finite floats print as nan, inf and -inf
            if f.is_nan() {
                out.extend_from_slice(b"nan");
            } else {
                write!(out, "{f}").expect("writing to a Vec");
            }
        }
        Token::Array(n) => {
            out.push(b'[');
            for i in 0..n {
                if i > 0 {
                    out.extend_from_slice(b", ");
                }
                print_value(d, out)?;
            }
            out.push(b']');
        }
        Token::Map(n) => {
            out.push(b'{');
            for i in 0..n {
                if i > 0 {
                    out.extend_from_slice(b", ");
                }
                print_value(d, out)?;
                out.extend_from_slice(b": ");
                print_value(d, out)?;
            }
            out.push(b'}');
        }
        Token::Str(len) => {
            out.push(b'"');
            write_escaped(out, rpc::expect_bytes(d, len)?);
            out.push(b'"');
        }
        Token::Bin(len) => {
            write!(out, "bin({})", rpc::expect_bytes(d, len)?.len()).expect("writing to a Vec");
        }
        // buffer, window and tabpage handles: an int packed inside
        Token::Ext(e) => {
            let payload = rpc::expect_bytes(d, e.len)?;
            let mut inner = Decoder::of(payload);
            let names = ["buf", "win", "tab"];
            let name = usize::try_from(e.r#type)
                .ok()
                .and_then(|i| names.get(i))
                .copied()
                .unwrap_or("ext");
            write!(out, "{name}:").expect("writing to a Vec");
            print_value(&mut inner, out)?;
        }
        Token::Chunk(_) => return Err(rpc::Error::BadMessage),
    }
    Ok(())
}

/// Writes `s` with control bytes, quotes and backslashes escaped, so it
/// can't change the terminal's state.
fn write_escaped(out: &mut Vec<u8>, s: &[u8]) {
    for &c in s {
        match c {
            b'"' | b'\\' => {
                out.push(b'\\');
                out.push(c);
            }
            0..=0x1f | 0x7f => write!(out, "\\x{c:02x}").expect("writing to a Vec"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_value_() {
        // [1, "a\x1b", {"k": true}, ext buffer 5, -2.5]
        let mut d = Decoder::of(
            b"\x95\x01\xa2a\x1b\x81\xa1k\xc3\xd4\x00\x05\xcb\xc0\x04\x00\x00\x00\x00\x00\x00",
        );
        let mut out = Vec::new();
        print_value(&mut d, &mut out).unwrap();
        assert_eq!(
            "[1, \"a\\x1b\", {\"k\": true}, buf:5, -2.5]",
            String::from_utf8(out).unwrap()
        );
    }
}
