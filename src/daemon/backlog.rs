//! A pane terminal's output that the terminal hasn't taken yet.
//!
//! The daemon has one thread, so a write that waits for a terminal Rex isn't
//! reading (a hidden or throttled pane) would stop everything else. Writes
//! here never wait: what the terminal can't take now waits here, a frame at a
//! time, until the event loop says it has room. Past `cap`, the waiting frames
//! are dropped, all but the one being written, which is finished so the
//! terminal never sees half an escape sequence; the pane then needs a full
//! repaint once the rest is written.

use std::collections::VecDeque;
use std::io;
use std::os::fd::RawFd;
use std::time::{Duration, Instant};

/// Most bytes a pane keeps waiting: a few full frames of a big terminal.
pub const CAP: usize = 1 << 20;

#[derive(Debug)]
pub struct Backlog {
    /// Frames waiting, oldest first. The first may be partly written.
    frames: VecDeque<Vec<u8>>,
    /// How much of the first frame is written.
    written: usize,
    /// Bytes waiting, over every frame.
    len: usize,
    /// Most bytes kept waiting. A field so a test can make it small.
    pub cap: usize,
    /// Frames were dropped since the terminal was last caught up.
    dropped: bool,
}

impl Default for Backlog {
    fn default() -> Backlog {
        Backlog {
            frames: VecDeque::new(),
            written: 0,
            len: 0,
            cap: CAP,
            dropped: false,
        }
    }
}

impl Backlog {
    /// Nothing waits.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Frames were dropped: the pane is drawn again in full, not frame by
    /// frame, once what's waiting is written.
    pub fn dropped(&self) -> bool {
        self.dropped
    }

    /// Sends `frame` to `fd` after what waits, taking its bytes; what `fd`
    /// can't take now waits. An error means the terminal is gone.
    pub fn send(&mut self, fd: RawFd, frame: &mut Vec<u8>) -> io::Result<()> {
        if frame.is_empty() {
            return Ok(());
        }
        if self.is_empty() {
            let n = write_some(fd, frame)?;
            if n == frame.len() {
                frame.clear();
                return Ok(());
            }
            frame.drain(..n);
        }
        self.len += frame.len();
        self.frames.push_back(std::mem::take(frame));
        if self.len > self.cap && self.frames.len() > 1 {
            self.frames.truncate(1);
            self.len = self.frames[0].len() - self.written;
            self.dropped = true;
        }
        self.drain(fd)
    }

    /// Writes what waits, as far as `fd` takes it now. An error means the
    /// terminal is gone.
    pub fn drain(&mut self, fd: RawFd) -> io::Result<()> {
        while let Some(first) = self.frames.front() {
            let n = write_some(fd, &first[self.written..])?;
            self.written += n;
            self.len -= n;
            if self.written < first.len() {
                return Ok(());
            }
            self.frames.pop_front();
            self.written = 0;
        }
        Ok(())
    }

    /// True once, when frames were dropped and nothing waits any more: time
    /// for the full repaint.
    pub fn take_dropped(&mut self) -> bool {
        let repaint = self.dropped && self.is_empty();
        if repaint {
            self.dropped = false;
        }
        repaint
    }

    /// The rest of the frame being written, for the terminal to get before
    /// it's restored; the frames after it are forgotten.
    pub fn take_unfinished(&mut self) -> Vec<u8> {
        let rest = match self.frames.pop_front() {
            Some(first) => first[self.written..].to_vec(),
            None => Vec::new(),
        };
        *self = Backlog {
            cap: self.cap,
            ..Backlog::default()
        };
        rest
    }
}

/// Writes `bytes` to `fd`, waiting at most `wait` for room; what doesn't fit
/// by then is dropped. For a terminal being restored as its pane goes: a
/// terminal nobody reads mustn't hold the daemon up.
pub fn write_within(fd: RawFd, bytes: &[u8], wait: Duration) {
    let deadline = Instant::now() + wait;
    let mut done = 0;
    loop {
        match write_some(fd, &bytes[done..]) {
            Ok(n) => done += n,
            Err(_) => return,
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if done == bytes.len() || left.is_zero() {
            return;
        }
        let mut fds = [libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        }];
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) } < 0
            && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
        {
            return;
        }
    }
}

/// Writes as much of `bytes` to `fd` as it takes without waiting; how much.
///
/// The terminal's open file is shared with the pane's helper and shell, so
/// it's only non-blocking for the write, and back as it was after: a shell
/// left with a non-blocking terminal can fail its reads.
fn write_some(fd: RawFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: F_GETFL only reads the file's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let blocking = flags & libc::O_NONBLOCK == 0;
    // SAFETY: F_SETFL with the file's own flags, plus O_NONBLOCK.
    if blocking && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut done = 0;
    let result = loop {
        if done == bytes.len() {
            break Ok(done);
        }
        let rest = &bytes[done..];
        // SAFETY: `rest` is valid for `rest.len()` bytes.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n > 0 {
            done += n as usize;
            continue;
        }
        if n == 0 {
            break Ok(done);
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => break Ok(done),
            _ => break Err(err),
        }
    };
    if blocking {
        // SAFETY: puts back the flags read above.
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
    }
    result
}
