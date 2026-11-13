//! How a pane's terminal bytes travel (ADR 0013): the daemon reads and
//! writes the terminal its helper handed over (`fd`), or the helper relays
//! them in `i` and `o` frames over its socket (`relay`). The hello, the
//! answer, sizes, the end and the restore don't depend on which.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::attach_conn;
use super::backlog::Backlog;
use crate::attach::frame::{self, HEADER_LEN};
use crate::attach::{End, Message, Transport};

/// How long a relayed attach's end waits for the `o` in flight to go.
pub(crate) const END_FLUSH: Duration = Duration::from_secs(1);

/// One pane's terminal bytes, as its transport carries them.
pub(crate) trait TerminalIo {
    fn transport(&self) -> Transport;

    /// The terminal, read for input under `fd`. None under `relay`, whose
    /// input comes in `i` frames on the helper socket.
    fn input_fd(&self) -> Option<RawFd>;

    /// Where output is written: the terminal, or the helper socket `helper`.
    fn output_fd(&self, helper: RawFd) -> RawFd;

    /// Makes `frame`, drawn by the daemon, what the output fd is sent.
    fn wrap(&self, frame: &mut Vec<u8>);

    /// Sends the helper `u`, a `u` frame with new undo bytes: on `helper`
    /// at once, or, where output travels on it too, into `out`, the bytes
    /// the output fd is sent next, so it goes between whole frames.
    fn send_undo(&self, helper: &UnixStream, u: &[u8], out: &mut Vec<u8>);

    /// Ends the attach on `helper`, telling it `end`, once what must go of
    /// `backlog` has. Nothing is written after.
    fn end(&self, helper: &UnixStream, backlog: &Backlog, end: End);
}

/// The terminal handed over: the daemon reads and writes it itself.
pub(crate) struct FdIo {
    /// Closed as it drops.
    pub(crate) tty: OwnedFd,
}

impl TerminalIo for FdIo {
    fn transport(&self) -> Transport {
        Transport::Fd
    }

    fn input_fd(&self) -> Option<RawFd> {
        Some(self.tty.as_raw_fd())
    }

    fn output_fd(&self, _helper: RawFd) -> RawFd {
        self.tty.as_raw_fd()
    }

    fn wrap(&self, _frame: &mut Vec<u8>) {}

    /// Only the daemon's frames go on the helper socket, and few: there is
    /// room for it.
    fn send_undo(&self, helper: &UnixStream, u: &[u8], _out: &mut Vec<u8>) {
        write_within(helper, u, Instant::now() + END_FLUSH);
    }

    /// What waits is dropped, and the `z` says whether that cut a frame
    /// short.
    fn end(&self, helper: &UnixStream, backlog: &Backlog, mut end: End) {
        end.cut = backlog.cut();
        attach_conn::send_end(helper, end);
    }
}

/// The terminal relayed by the helper, in `i` and `o` frames.
pub(crate) struct RelayIo;

impl TerminalIo for RelayIo {
    fn transport(&self) -> Transport {
        Transport::Relay
    }

    fn input_fd(&self) -> Option<RawFd> {
        None
    }

    fn output_fd(&self, helper: RawFd) -> RawFd {
        helper
    }

    fn wrap(&self, frame: &mut Vec<u8>) {
        if frame.is_empty() {
            return;
        }
        let bytes = std::mem::take(frame);
        frame::encode_split(b'o', &bytes, frame).expect("split to fit");
    }

    fn send_undo(&self, _helper: &UnixStream, u: &[u8], out: &mut Vec<u8>) {
        out.extend_from_slice(u);
    }

    /// The `o` in flight is finished, so the frames stay whole, and the
    /// rest dropped; the `z` says whether that cut a drawn frame short. If
    /// the socket doesn't take that and the `z` within `END_FLUSH`, the
    /// attach ends without a `z`, and the helper takes it for a crash.
    fn end(&self, helper: &UnixStream, backlog: &Backlog, mut end: End) {
        let mut bytes = Vec::new();
        if let Some((drawn, written)) = backlog.in_flight() {
            let stop = end_of_frame_at(drawn, written);
            bytes.extend_from_slice(&drawn[written..stop]);
            end.cut = stop < drawn.len();
        }
        let Ok(z) = Message::End(end).encode() else {
            return;
        };
        bytes.extend_from_slice(&z);
        write_within(helper, &bytes, Instant::now() + END_FLUSH);
    }
}

/// Where the frame that byte `at` of `frames`, whole frames back to back,
/// falls in ends: `at` itself if a frame starts there.
fn end_of_frame_at(frames: &[u8], at: usize) -> usize {
    let mut pos = 0;
    while pos < at {
        let Some(h) = frames.get(pos..pos + HEADER_LEN) else {
            return frames.len();
        };
        let len = u32::from_be_bytes([h[1], h[2], h[3], h[4]]) as usize;
        pos += HEADER_LEN + len;
    }
    pos.min(frames.len())
}

/// Writes `bytes` to the non-blocking `stream`, waiting for room until
/// `deadline`: whether it all went.
fn write_within(stream: &UnixStream, bytes: &[u8], deadline: Instant) -> bool {
    let mut done = 0;
    while done < bytes.len() {
        match (&*stream).write(&bytes[done..]) {
            Ok(0) => return false,
            Ok(n) => done += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return false;
                }
                let mut fds = [libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                }];
                let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
            }
            Err(_) => return false,
        }
    }
    true
}
