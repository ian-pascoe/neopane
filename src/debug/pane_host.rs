//! Panes for the debug daemons: takes terminals from helpers run as
//! `<name> attach --address A --pane ID`, with ids the host minted, and
//! shows their input and sizes. `q` in a pane closes it.

use std::io::Write;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::os::unix::net::{UnixListener, UnixStream};

use super::Result;
use crate::attach::{End, Ending, Message, PaneId, PaneIdRegistry, Purpose, Refusal, Size};
use crate::daemon::attach_conn::{self, HELLO_TIMEOUT, Pending, Step};
use crate::daemon::event_loop::Loop;
use crate::name::NAME;
use crate::term::input::{self, Event, Key};
use crate::term::terminal::Terminal;

pub(crate) const MAX_PANES: usize = 8;

/// Fixed slots. (Each `Terminal` owns its output buffer.)
pub(crate) struct PaneHost {
    pub panes: [Option<Pane>; MAX_PANES],
    /// Loop tokens from here up, two per slot.
    pub base: usize,
    /// Shown in each pane's greeting.
    pub name: String,
    /// The pane ids it minted, each good for one attach.
    pub ids: PaneIdRegistry,
}

pub(crate) struct Pane {
    pub id: PaneId,
    pub stream: UnixStream,
    /// What the helper sent that isn't a whole frame yet.
    pub from_helper: Vec<u8>,
    pub term: Terminal,
    /// As the helper last said.
    pub size: Size,
    pub parser: input::Parser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Socket = 0,
    Tty = 1,
}

impl PaneHost {
    /// A host whose tokens start at `base`, greeting its panes as driven by `name`.
    pub(crate) fn new(base: usize, name: &str) -> PaneHost {
        PaneHost {
            panes: [const { None }; MAX_PANES],
            base,
            name: name.to_string(),
            ids: PaneIdRegistry::new(),
        }
    }

    /// A pane id for a helper to attach with.
    pub(crate) fn mint(&mut self) -> PaneId {
        self.ids.mint()
    }

    /// Restores every terminal and hangs up, so the helpers exit. (Dropping
    /// the host does the same.)
    pub(crate) fn deinit(&mut self) {
        for slot in &mut self.panes {
            close(slot, None);
        }
    }

    /// Whether `token` is one of this host's.
    pub(crate) fn owns(&self, token: usize) -> bool {
        token >= self.base && token <= self.escape_timer()
    }

    /// The token of the timer for ESCs waiting in the panes.
    fn escape_timer(&self) -> usize {
        self.base + 2 * MAX_PANES
    }

    /// The escape timer fired: an ESC still waiting in a pane is its Escape key.
    fn flush_pending_escapes(&mut self) -> Result<()> {
        for p in self.panes.iter_mut().flatten() {
            if let Some(event) = p.parser.flush_pending() {
                write!(p.term.writer, "  {event}\r\n")?;
                p.term.flush()?;
            }
        }
        Ok(())
    }

    /// Handles an event for one of this host's tokens.
    pub(crate) fn handle(&mut self, event_loop: &Loop, token: usize) -> Result<()> {
        if token == self.escape_timer() {
            return self.flush_pending_escapes();
        }
        let i = (token - self.base) / 2;
        let source = if (token - self.base).is_multiple_of(2) {
            Source::Socket
        } else {
            Source::Tty
        };
        let timer = self.escape_timer();
        let slot = &mut self.panes[i];
        // null if closed earlier in this batch
        let Some(p) = slot.as_mut() else {
            return Ok(());
        };
        match source {
            Source::Socket => {
                if !on_message(p)? {
                    eprintln!("pane {}: helper gone", p.id);
                    close(slot, Some(event_loop));
                }
            }
            Source::Tty => {
                if !on_input(p)? {
                    eprintln!("pane {}: closed with q", p.id);
                    close(slot, Some(event_loop));
                } else if p.parser.flush_due() {
                    event_loop.add_timer(timer, input::ESCAPE_TIMEOUT_MS, timer)?;
                }
            }
        }
        Ok(())
    }

    /// Takes a new helper's terminal and greets it, or refuses it. Call
    /// between batches of events, so a stale event can't reach a reused
    /// slot. Waits for the hello: this is a debug daemon.
    pub(crate) fn accept(&mut self, event_loop: &Loop, server: &UnixListener) -> Result<()> {
        let (stream, _) = server.accept()?;
        let Some(pending) = Pending::new(stream, HELLO_TIMEOUT, attach_conn::euid())? else {
            eprintln!("refused a helper of another user");
            return Ok(());
        };
        let Step::Arrived(a) = pending.read_blocking() else {
            eprintln!("a helper went without its hello");
            return Ok(());
        };
        let refuse = |stream, z: End| {
            eprintln!("refused a helper: {}: {}", z.reason, z.message);
            attach_conn::refuse(stream, z);
            Ok(())
        };
        let hello = match a.hello {
            Ok(h) => h,
            Err(e) => return refuse(a.stream, attach_conn::refusal_of(&e)),
        };
        let Purpose::Requested { pane, terminal } = hello.purpose else {
            let m = format!("{} takes only requested panes", self.name);
            return refuse(a.stream, End::refusal(Refusal::Hello, m));
        };
        if let Err(z) = attach_conn::check_terminal(&terminal, a.tty.as_ref()) {
            return refuse(a.stream, z);
        }
        // before the id is taken, so it stays good for later
        let Some(i) = self.panes.iter().position(|p| p.is_none()) else {
            let m = format!("{} drives {MAX_PANES} panes at most", self.name);
            return refuse(a.stream, End::refusal(Refusal::Hello, m));
        };
        if let Err(reason) = self.ids.take(&pane) {
            let m = format!("pane {pane} can't be attached");
            return refuse(a.stream, End::refusal(reason, m));
        }
        let tty = a.tty.expect("checked").into_raw_fd();
        // its helper put it in raw mode
        let term = Terminal::new(tty);
        attach_conn::send_accept(&a.stream, &self.name, "")?;
        a.stream.set_nonblocking(true)?;
        self.panes[i] = Some(Pane {
            id: pane,
            stream: a.stream,
            from_helper: a.rest,
            term,
            size: terminal.size,
            parser: input::Parser::default(),
        });

        // (a failed slot is emptied again)
        match self.greet(i, event_loop) {
            Ok(()) => Ok(()),
            Err(e) => {
                let slot = &mut self.panes[i];
                if let Some(p) = slot.as_mut() {
                    // the terminal was never shown anything
                    p.term.writer.clear();
                }
                close(slot, Some(event_loop));
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
            "\x1b[H\x1b[2J{NAME} pane {}, driven by {}.\r\n",
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

/// Frames from the helper: its sizes. False when it hung up or broke the
/// protocol.
fn on_message(p: &mut Pane) -> Result<bool> {
    let mut buf = [0u8; 4096];
    let n = match std::io::Read::read(&mut p.stream, &mut buf) {
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(true),
        Err(_) => 0,
    };
    if n == 0 {
        return Ok(false);
    }
    p.from_helper.extend_from_slice(&buf[..n]);
    loop {
        match attach_conn::next_message(&mut p.from_helper) {
            Ok(Some(Message::Size(size))) => {
                p.size = size;
                print_size(p)?;
            }
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(_) => return Ok(false),
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
    write!(p.term.writer, "size {}x{}\r\n", p.size.cols, p.size.rows)?;
    Ok(())
}

/// Takes the slot's fds off `event_loop` (none when it is ending too),
/// and hangs up, so the helper restores the terminal and exits. Does nothing for
/// an empty slot.
fn close(slot: &mut Option<Pane>, event_loop: Option<&Loop>) {
    if let Some(mut p) = slot.take() {
        if let Some(event_loop) = event_loop {
            // (an fd that was never added is fine)
            let _ = event_loop.remove(p.stream.as_raw_fd());
            let _ = event_loop.remove(p.term.fd);
        }
        let _ = p.term.flush();
        close_fd(p.term.fd);
        attach_conn::send_end(
            &p.stream,
            End::ending(Ending::Closed, "the pane closed", false),
        );
        // the stream closes as `p` drops
    }
}

fn close_fd(fd: i32) {
    // SAFETY: closing an fd this module owns.
    unsafe { libc::close(fd) };
}
