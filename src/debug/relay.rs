//! The relay test tool: a requested pane's terminal attached over the relay
//! (ADR 0013) with no pty behind it. Tests drive a [`RelayPane`] in their
//! own process; scripts, such as a fake pane provider's `open`, run
//! `<name> debug-relay {attach}`, which copies stdin to the daemon in `i`
//! frames and its `o` frames to stdout.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use crate::attach::Transport;
use crate::attach::frame::encode_split;
use crate::attach::{Accept, End, Frame, Hello, Message, PaneId, Purpose, Size, Terminal};
use crate::daemon::dir::Address;
use crate::daemon::pane_helper::{self, Exit, Want};

/// One pane's terminal, relayed: what it has been sent, and what it sends.
#[derive(Debug)]
pub struct RelayPane {
    stream: UnixStream,
    /// Bytes not yet sent: the rest of the hello.
    out: Vec<u8>,
    /// Bytes read that aren't a whole frame yet.
    buf: Vec<u8>,
    /// Every `o` payload, joined.
    output: Vec<u8>,
    accepted: Option<Accept>,
    /// The latest undo bytes.
    undo: Vec<u8>,
    /// The `z`: a refusal, or the attach's ending.
    end: Option<End>,
    /// The daemon hung up.
    gone: bool,
}

impl RelayPane {
    /// Connects to the daemon at `address`, and sends the hello of pane
    /// `pane`'s terminal, of `size`.
    pub fn connect(address: &Address, pane: PaneId, size: Size) -> io::Result<RelayPane> {
        RelayPane::on(address.connect()?, pane, size)
    }

    /// Sends the hello of pane `pane`'s terminal, of `size`, on `stream`,
    /// connected to a daemon.
    pub fn on(stream: UnixStream, pane: PaneId, size: Size) -> io::Result<RelayPane> {
        let hello = Message::Hello(Hello::new(Purpose::Requested {
            pane,
            terminal: terminal(size),
        }));
        stream.set_nonblocking(true)?;
        let mut p = RelayPane {
            stream,
            out: hello.encode().map_err(io::Error::other)?,
            buf: Vec::new(),
            output: Vec::new(),
            accepted: None,
            undo: Vec::new(),
            end: None,
            gone: false,
        };
        p.flush()?;
        Ok(p)
    }

    /// Writes what waits to be sent, as far as the socket takes it now:
    /// whether it all went. A hello carries the whole environment, more
    /// than a socket may hold before the daemon reads.
    pub fn flush(&mut self) -> io::Result<bool> {
        while !self.out.is_empty() {
            match (&self.stream).write(&self.out) {
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }

    /// Types `bytes`, in `i` frames.
    pub fn type_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut frames = Vec::new();
        encode_split(b'i', bytes, &mut frames).map_err(io::Error::other)?;
        self.send(&frames)
    }

    /// Says the terminal is `size` now.
    pub fn resize(&mut self, size: Size) -> io::Result<()> {
        let frame = Message::Size(size).encode().map_err(io::Error::other)?;
        self.send(&frame)
    }

    /// Writes all of `bytes`, after what waits, waiting for room.
    fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.extend_from_slice(bytes);
        let out = std::mem::take(&mut self.out);
        self.stream.set_nonblocking(false)?;
        let sent = (&self.stream).write_all(&out);
        self.stream.set_nonblocking(true)?;
        sent
    }

    /// Reads what the daemon has sent, without waiting. Err when it broke
    /// the protocol.
    pub fn read(&mut self) -> io::Result<()> {
        self.flush()?;
        let mut chunk = [0u8; 64 << 10];
        while !self.gone {
            match (&self.stream).read(&mut chunk) {
                Ok(0) => self.gone = true,
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => self.gone = true,
            }
        }
        while let Some((frame, n)) = Frame::decode(&self.buf).map_err(io::Error::other)? {
            self.buf.drain(..n);
            match Message::decode(&frame).map_err(io::Error::other)? {
                Some(Message::Accept(a)) => self.accepted = Some(a),
                Some(Message::Undo(u)) => self.undo = u,
                Some(Message::Output(o)) => self.output.extend_from_slice(&o),
                Some(Message::End(z)) => self.end = Some(z),
                _ => {}
            }
        }
        Ok(())
    }

    /// Reads until `done` says so, or the daemon hangs up, for up to
    /// `within`: whether `done` said so.
    pub fn read_until(
        &mut self,
        within: Duration,
        done: impl Fn(&RelayPane) -> bool,
    ) -> io::Result<bool> {
        let deadline = Instant::now() + within;
        loop {
            self.read()?;
            if done(self) {
                return Ok(true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.gone || left.is_zero() {
                return Ok(false);
            }
            let out = if self.out.is_empty() {
                0
            } else {
                libc::POLLOUT
            };
            let mut fds = [libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN | out,
                revents: 0,
            }];
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        }
    }

    /// Everything the daemon drew, as `o` frames brought it.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    /// Takes what the daemon drew so far.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// The daemon's `a`, once it came.
    pub fn accepted(&self) -> Option<&Accept> {
        self.accepted.as_ref()
    }

    /// The latest undo bytes.
    pub fn undo(&self) -> &[u8] {
        &self.undo
    }

    /// The daemon's `z`, once it came: its refusal, or why it ended the
    /// attach.
    pub fn end(&self) -> Option<&End> {
        self.end.as_ref()
    }

    /// The daemon hung up.
    pub fn gone(&self) -> bool {
        self.gone
    }

    /// Bytes read after the last whole frame: a frame the daemon cut off.
    pub fn partial(&self) -> &[u8] {
        &self.buf
    }

    /// The socket, for a test that sets its options.
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }
}

/// What the hello says of a terminal of `size` with no pty: this process's
/// pid, environment and working directory, and no tty.
fn terminal(size: Size) -> Terminal {
    let mut env: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let k = k.as_bytes().to_vec();
        if !env.iter().any(|(seen, _)| *seen == k) {
            env.push((k, v.as_bytes().to_vec()));
        }
    }
    Terminal {
        transport: Transport::Relay,
        size,
        pid: std::process::id(),
        tty: Vec::new(),
        env,
        cwd: std::env::current_dir()
            .map(|d| d.as_os_str().as_bytes().to_vec())
            .unwrap_or_default(),
    }
}

/// The terminal size `debug-relay` says it has.
pub const SIZE: Size = Size {
    cols: 80,
    rows: 24,
    px: None,
};

/// `<name> debug-relay {attach}`: attaches pane `pane` at `address` over
/// the relay, at 80 x 24, copying stdin to the daemon and its output to
/// stdout until the attach ends. It exits as a requested pane's helper
/// does.
pub fn run(address: &Address, pane: PaneId) -> Exit {
    match relay(address, pane) {
        Ok(exit) => exit,
        Err(e) => Exit {
            status: 1,
            message: Some(e.to_string()),
            by_signal: false,
        },
    }
}

fn relay(address: &Address, pane: PaneId) -> io::Result<Exit> {
    let mut p = RelayPane::connect(address, pane, SIZE)?;
    let mut stdin_open = true;
    let mut stdout = io::stdout();
    loop {
        p.read()?;
        let out = p.take_output();
        if !out.is_empty() {
            stdout.write_all(&out)?;
            stdout.flush()?;
        }
        if let Some(z) = p.end() {
            let want = Want::Requested(pane);
            return Ok(match p.accepted() {
                Some(_) => pane_helper::ended(want, z),
                None => pane_helper::refused(want, z),
            });
        }
        if p.gone() {
            return Err(io::Error::other("the daemon hung up"));
        }
        let mut fds = [
            libc::pollfd {
                fd: p.stream.as_raw_fd(),
                events: libc::POLLIN | if p.out.is_empty() { 0 } else { libc::POLLOUT },
                revents: 0,
            },
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // what is typed before the answer waits for it
        let n = if stdin_open && p.accepted().is_some() {
            2
        } else {
            1
        };
        // SAFETY: the first `n` pollfds are valid.
        if unsafe { libc::poll(fds.as_mut_ptr(), n, -1) } < 0 {
            continue;
        }
        if stdin_open && fds[1].revents != 0 {
            let mut chunk = [0u8; 4096];
            match io::stdin().read(&mut chunk) {
                Ok(0) | Err(_) => stdin_open = false,
                Ok(n) => p.type_bytes(&chunk[..n])?,
            }
        }
    }
}
