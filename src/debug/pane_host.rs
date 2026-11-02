//! Panes for the debug daemons: takes terminals from `neorex pane` helpers,
//! shows their input and sizes. `q` in a pane closes it.

use std::fmt;
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};

use super::Result;
use crate::daemon::event_loop::Loop;
use crate::daemon::fdpass;
use crate::daemon::pane_helper::{self, Message};
use crate::term::input::{self, Event, Key};
use crate::term::terminal::Terminal;

pub const MAX_PANES: usize = 8;

/// Fixed slots. (The Zig kept each Terminal's output buffer in a caller-owned
/// `OutBufs` array; a `Terminal` owns its buffer here, so there is none.)
pub struct PaneHost {
    pub panes: [Option<Pane>; MAX_PANES],
    /// Loop tokens from here up, two per slot.
    pub base: usize,
    /// Shown in each pane's greeting.
    pub name: String,
    pub next_id: u32,
}

pub struct Pane {
    pub id: u32,
    pub stream: UnixStream,
    pub term: Terminal,
    pub parser: input::Parser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Socket = 0,
    Tty = 1,
}

/// What `PaneHost::accept` can fail with besides I/O: `pane_helper::Error`'s
/// `NoTerminal` and `BadHello` too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    TooManyPanes,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::TooManyPanes => f.write_str("TooManyPanes"),
        }
    }
}

impl std::error::Error for Error {}

impl PaneHost {
    /// A host whose tokens start at `base`, greeting its panes as driven by `name`.
    pub fn new(base: usize, name: &str) -> PaneHost {
        PaneHost {
            panes: [const { None }; MAX_PANES],
            base,
            name: name.to_string(),
            next_id: 1,
        }
    }

    /// Restores every terminal and hangs up, so the helpers exit. (Dropping
    /// the host does the same.)
    pub fn deinit(&mut self) {
        for slot in &mut self.panes {
            close(slot);
        }
    }

    /// Whether `token` is one of this host's.
    pub fn owns(&self, token: usize) -> bool {
        token >= self.base && token < self.base + 2 * MAX_PANES
    }

    /// Handles an event for one of this host's tokens.
    pub fn handle(&mut self, token: usize) -> Result<()> {
        let i = (token - self.base) / 2;
        let source = if (token - self.base).is_multiple_of(2) {
            Source::Socket
        } else {
            Source::Tty
        };
        let slot = &mut self.panes[i];
        // null if closed earlier in this batch
        let Some(p) = slot.as_mut() else {
            return Ok(());
        };
        match source {
            Source::Socket => {
                if !on_message(p)? {
                    eprintln!("pane {}: helper gone", p.id);
                    close(slot);
                }
            }
            Source::Tty => {
                if !on_input(p)? {
                    eprintln!("pane {}: closed with q", p.id);
                    close(slot);
                }
            }
        }
        Ok(())
    }

    /// Takes a new helper's terminal and greets it. Call between batches of
    /// events, so a stale event can't reach a reused slot.
    pub fn accept(&mut self, event_loop: &Loop, server: &UnixListener) -> Result<()> {
        let (stream, _) = server.accept()?;
        let mut buf = [0u8; 1];
        let hello = fdpass::recv(&stream, &mut buf)?;
        let tty = hello.fd.ok_or(pane_helper::Error::NoTerminal)?;
        if hello.len != 1 || buf[0] != Message::Hello.to_byte() {
            close_fd(tty);
            return Err(pane_helper::Error::BadHello.into());
        }

        let Some(i) = self.panes.iter().position(|p| p.is_none()) else {
            close_fd(tty);
            return Err(Error::TooManyPanes.into());
        };
        let term = match Terminal::init(tty) {
            Ok(term) => term,
            Err(e) => {
                close_fd(tty);
                return Err(e.into());
            }
        };
        self.panes[i] = Some(Pane {
            id: self.next_id,
            stream,
            term,
            parser: input::Parser::default(),
        });
        self.next_id += 1;

        // (the Zig left a failed slot half-closed; here it's emptied again)
        match self.greet(i, event_loop) {
            Ok(()) => Ok(()),
            Err(e) => {
                let slot = &mut self.panes[i];
                if let Some(p) = slot.as_mut() {
                    // the terminal was never shown anything
                    p.term.writer.clear();
                }
                close(slot);
                Err(e)
            }
        }
    }

    /// Watches slot `i`'s socket and terminal, and writes its greeting.
    fn greet(&mut self, i: usize, event_loop: &Loop) -> Result<()> {
        let p = self.panes[i].as_mut().expect("the slot was just filled");
        event_loop.add_read(
            p.stream.as_raw_fd(),
            self.base + 2 * i + Source::Socket as usize,
        )?;
        event_loop.add_read(p.term.fd, self.base + 2 * i + Source::Tty as usize)?;
        eprintln!("pane {}: attached", p.id);
        write!(
            p.term.writer,
            "\x1b[H\x1b[2Jneorex pane {}, driven by {}.\r\n",
            p.id, self.name
        )?;
        print_size(p)?;
        p.term
            .writer
            .extend_from_slice(b"Type to see events. q closes this pane.\r\n");
        p.term.flush()?;
        Ok(())
    }
}

impl Drop for PaneHost {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// A message from the helper. False when it hung up.
fn on_message(p: &mut Pane) -> Result<bool> {
    let mut buf = [0u8; 64];
    let r = fdpass::recv(&p.stream, &mut buf)?;
    if let Some(fd) = r.fd {
        close_fd(fd);
    }
    if r.len == 0 {
        return Ok(false);
    }
    for &b in &buf[..r.len] {
        if Message::from_byte(b) == Message::Resized {
            print_size(p)?;
        }
    }
    p.term.flush()?;
    Ok(true)
}

/// Keys typed in the pane. False on q.
fn on_input(p: &mut Pane) -> Result<bool> {
    let mut buf = [0u8; 4096];
    // SAFETY: `buf` is valid for `buf.len()` bytes.
    let n = unsafe { libc::read(p.term.fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return Ok(false);
    }
    p.parser.feed(&buf[..n as usize]);
    while let Some(event) = p.parser.next() {
        if let Event::Key(k) = event
            && k.key == Key::Codepoint('q' as u32)
            && k.mods.is_empty()
        {
            return Ok(false);
        }
        write!(p.term.writer, "  {event}\r\n")?;
    }
    p.term.flush()?;
    Ok(true)
}

fn print_size(p: &mut Pane) -> Result<()> {
    let ws = p.term.size()?;
    write!(p.term.writer, "size {}x{}\r\n", ws.ws_col, ws.ws_row)?;
    Ok(())
}

/// Restores the terminal and hangs up, so the helper exits. Does nothing for
/// an empty slot.
fn close(slot: &mut Option<Pane>) {
    if let Some(mut p) = slot.take() {
        p.term.deinit();
        close_fd(p.term.fd);
        // the stream closes as `p` drops
    }
}

fn close_fd(fd: i32) {
    // SAFETY: closing an fd this module owns.
    unsafe { libc::close(fd) };
}
