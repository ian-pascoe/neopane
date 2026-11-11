//! The daemon's side of a new attach (ADR 0013): a connection waiting for
//! its hello, read without blocking, and the frames that answer it.
//! `docs/attach-protocol.md` is the reference.

use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::fdpass;
use super::pane_helper;
use crate::attach::{self, ATTACH_PROTOCOL_VERSION, Accept, End, Frame, Hello, Message, Refusal};
use crate::attach::{PaneId, Terminal, Transport};
use crate::log;

/// How long a connected peer has to send its hello.
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// Bytes read from a pending connection at a time.
const READ_SIZE: usize = 64 << 10;

/// A connection whose hello hasn't all arrived. Its socket is non-blocking.
#[derive(Debug)]
pub(crate) struct Pending {
    pub(crate) stream: UnixStream,
    buf: Vec<u8>,
    /// The terminal that rode on the hello's first byte, under `fd`.
    tty: Option<OwnedFd>,
    /// When it's refused if its hello still hasn't come.
    pub(crate) deadline: Instant,
}

/// A connection's hello, read whole or failed, and what came with it.
#[derive(Debug)]
pub(crate) struct Arrived {
    pub(crate) stream: UnixStream,
    pub(crate) tty: Option<OwnedFd>,
    pub(crate) hello: Result<Hello, attach::Error>,
    /// Whether a hello that can't be read still says it is a requested
    /// pane's, and the pane it names, if that can still be told.
    pub(crate) requested: bool,
    pub(crate) pane: Option<PaneId>,
    /// When its last byte was read, as `log::trace_now` has it: L9 is
    /// timed from here.
    pub(crate) at: u64,
    /// Frames sent after the hello, such as a size.
    pub(crate) rest: Vec<u8>,
}

/// What a read of a pending connection found.
#[derive(Debug)]
pub(crate) enum Step {
    /// Its hello isn't all here yet.
    Waiting(Pending),
    Arrived(Arrived),
    /// It hung up, or its socket failed, before its hello. Its socket is
    /// still open, for the caller to stop watching before it closes.
    Gone(UnixStream),
}

impl Pending {
    /// A connection the daemon just accepted, its hello due within
    /// `timeout`. Refused `user`, and None, when its peer runs as another
    /// user than `uid`, the daemon's.
    pub(crate) fn new(
        stream: UnixStream,
        timeout: Duration,
        uid: u32,
    ) -> io::Result<Option<Pending>> {
        if pane_helper::peer_uid(&stream)? != uid {
            refuse(
                stream,
                End::refusal(Refusal::User, "you run as another user than the daemon"),
            );
            return Ok(None);
        }
        stream.set_nonblocking(true)?;
        Ok(Some(Pending {
            stream,
            buf: Vec::new(),
            tty: None,
            deadline: Instant::now() + timeout,
        }))
    }

    /// Reads what has come, without blocking.
    pub(crate) fn read(mut self) -> Step {
        let mut chunk = vec![0u8; READ_SIZE];
        loop {
            let r = match fdpass::recv(&self.stream, &mut chunk) {
                Ok(r) => r,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Step::Waiting(self),
                Err(_) => return Step::Gone(self.stream),
            };
            if let Some(fd) = r.fd {
                // SAFETY: a passed fd is ours alone; a second one is closed.
                let fd = unsafe { OwnedFd::from_raw_fd(fd) };
                self.tty.get_or_insert(fd);
            }
            if r.len == 0 {
                return Step::Gone(self.stream);
            }
            self.buf.extend_from_slice(&chunk[..r.len]);
            match Frame::decode(&self.buf) {
                Ok(None) => continue,
                Ok(Some((frame, n))) => {
                    let at = log::trace_now();
                    let rest = self.buf.split_off(n);
                    let hello = Message::decode_hello(&frame);
                    let unreadable = hello.is_err() && frame.kind == b'h';
                    let requested = unreadable && Hello::reads_requested(&frame.payload);
                    let pane = if requested {
                        Hello::requested_pane(&frame.payload)
                    } else {
                        None
                    };
                    return Step::Arrived(Arrived {
                        stream: self.stream,
                        tty: self.tty,
                        hello,
                        requested,
                        pane,
                        at,
                        rest,
                    });
                }
                Err(e) => {
                    return Step::Arrived(Arrived {
                        stream: self.stream,
                        tty: self.tty,
                        hello: Err(e),
                        requested: false,
                        pane: None,
                        at: log::trace_now(),
                        rest: Vec::new(),
                    });
                }
            }
        }
    }

    /// Waits up to its deadline for the whole hello, blocking: for the
    /// launcher's attach before the loop runs, and the debug daemons.
    pub(crate) fn read_blocking(mut self) -> Step {
        loop {
            let left = self.deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                send_end(&self.stream, no_hello());
                return Step::Gone(self.stream);
            }
            let mut fds = [libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
            match self.read() {
                Step::Waiting(p) => self = p,
                done => return done,
            }
        }
    }

    /// Its deadline passed with no hello: refused, and closed.
    pub(crate) fn time_out(self) {
        refuse(self.stream, no_hello());
    }
}

/// The refusal of a connection whose hello didn't come in time.
fn no_hello() -> End {
    let secs = HELLO_TIMEOUT.as_secs();
    End::refusal(Refusal::Hello, format!("no hello within {secs} s"))
}

/// This process's effective user id: the user a daemon lets attach.
pub(crate) fn euid() -> u32 {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() }
}

/// Sends `end`, without waiting for room, and hangs up.
pub(crate) fn refuse(stream: UnixStream, end: End) {
    send_end(&stream, end);
}

/// Sends `end`, the last frame, without waiting for room: a connection
/// whose socket is full is hung up on without it.
pub(crate) fn send_end(stream: &UnixStream, end: End) {
    let _ = stream.set_nonblocking(true);
    if let Ok(bytes) = Message::End(end).encode() {
        let _ = (&*stream).write(&bytes);
    }
}

/// The `a` frame accepting an attach: this build, the daemon's name and its
/// log's path.
pub(crate) fn accept_frame(daemon: &str, log: &str) -> Vec<u8> {
    Message::Accept(Accept {
        v: ATTACH_PROTOCOL_VERSION,
        build: attach::build().to_string(),
        daemon: daemon.to_string(),
        log: log.as_bytes().to_vec(),
        features: Vec::new(),
    })
    .encode()
    .expect("an accept fits a frame")
}

/// Sends the `a` frame, waiting for room: the first thing on a new
/// connection, which has room for it.
pub(crate) fn send_accept(stream: &UnixStream, daemon: &str, log: &str) -> io::Result<()> {
    (&*stream).write_all(&accept_frame(daemon, log))
}

/// Whether a requested or self-attached terminal can be taken: under `fd`,
/// with a terminal handed over. The relay comes with #103.
pub(crate) fn check_terminal(t: &Terminal, tty: Option<&OwnedFd>) -> Result<(), End> {
    if t.transport == Transport::Relay {
        return Err(End::refusal(
            Refusal::Transport,
            "this daemon doesn't take the relay transport yet",
        ));
    }
    let Some(tty) = tty else {
        return Err(End::refusal(
            Refusal::Terminal,
            "no terminal came with the hello",
        ));
    };
    // SAFETY: `isatty` only inspects the fd.
    if unsafe { libc::isatty(tty.as_raw_fd()) } == 0 {
        return Err(End::refusal(
            Refusal::Terminal,
            "the handed-over file isn't a terminal",
        ));
    }
    Ok(())
}

/// The refusal for a hello that can't be read: its reason, and a message
/// naming the field, or both builds and versions.
pub(crate) fn refusal_of(e: &attach::Error) -> End {
    End::refusal(e.refusal(), e.to_string())
}

/// The fields of a terminal's attach worth logging: never its environment.
pub(crate) fn describe(purpose: &str, pane: Option<&str>, t: &Terminal, build: &str) -> String {
    let s = t.size;
    format!(
        "{purpose} pane {} transport {} size {}x{} pid {} tty {} build {build}",
        pane.unwrap_or("-"),
        t.transport.name(),
        s.cols,
        s.rows,
        t.pid,
        String::from_utf8_lossy(&t.tty),
    )
}

/// A frame from an attached helper's socket, read into `buf`: its sizes.
/// Unknown kinds are skipped.
pub(crate) fn next_message(buf: &mut Vec<u8>) -> Result<Option<Message>, attach::Error> {
    loop {
        let Some((frame, n)) = Frame::decode(buf)? else {
            return Ok(None);
        };
        buf.drain(..n);
        if let Some(m) = Message::decode(&frame)? {
            return Ok(Some(m));
        }
    }
}

/// A terminal as a test's helper says it is: `cols` x `rows`, under `fd`.
#[cfg(test)]
pub(crate) fn test_terminal(cols: u16, rows: u16) -> Terminal {
    Terminal {
        transport: Transport::Fd,
        size: crate::attach::Size {
            cols,
            rows,
            px: None,
        },
        pid: std::process::id(),
        tty: Vec::new(),
        env: Vec::new(),
        cwd: b"/".to_vec(),
    }
}

/// A requested pane's hello, as a frame.
#[cfg(test)]
pub(crate) fn requested_hello(pane: crate::attach::PaneId, t: Terminal) -> Vec<u8> {
    let purpose = crate::attach::Purpose::Requested { pane, terminal: t };
    Message::Hello(Hello::new(purpose)).encode().unwrap()
}

/// The launcher's hello, as a frame.
#[cfg(test)]
pub(crate) fn self_hello(t: Terminal) -> Vec<u8> {
    let purpose = crate::attach::Purpose::SelfAttach {
        terminal: t,
        provider: None,
        file: None,
        line: None,
    };
    Message::Hello(Hello::new(purpose)).encode().unwrap()
}

/// Every frame `stream` sends until it hangs up.
#[cfg(test)]
pub(crate) fn read_all_frames(stream: &UnixStream) -> Vec<Message> {
    let _ = stream.set_nonblocking(false);
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &*stream, &mut bytes).unwrap();
    let mut out = Vec::new();
    while let Some(m) = next_message(&mut bytes).unwrap() {
        out.push(m);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{PaneId, Purpose, Size};

    #[test]
    fn a_peer_of_another_user_is_refused_before_its_hello() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        assert!(
            Pending::new(ours, HELLO_TIMEOUT, euid() + 1)
                .unwrap()
                .is_none()
        );
        let said = read_all_frames(&theirs);
        let [Message::End(z)] = &said[..] else {
            panic!("{said:?}")
        };
        assert_eq!(Some(Refusal::User), z.as_refusal());
    }

    fn hello_bytes(h: &Hello) -> Vec<u8> {
        Message::Hello(h.clone()).encode().unwrap()
    }

    fn requested() -> Hello {
        Hello::new(Purpose::Requested {
            pane: PaneId::parse("1-aaaaaaaaaa").unwrap(),
            terminal: Terminal {
                transport: Transport::Fd,
                size: Size {
                    cols: 80,
                    rows: 24,
                    px: None,
                },
                pid: 1,
                tty: b"/dev/ttys001".to_vec(),
                env: vec![(b"TERM".to_vec(), b"xterm".to_vec())],
                cwd: b"/".to_vec(),
            },
        })
    }

    /// A hello split across reads arrives once whole, with what followed it.
    #[test]
    fn a_hello_in_pieces_arrives_whole() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut p = Pending::new(ours, HELLO_TIMEOUT, euid()).unwrap().unwrap();
        let mut bytes = hello_bytes(&requested());
        let size = Message::Size(Size {
            cols: 1,
            rows: 2,
            px: None,
        })
        .encode()
        .unwrap();
        bytes.extend_from_slice(&size);
        let (a, b) = bytes.split_at(7);
        (&theirs).write_all(a).unwrap();
        p = match p.read() {
            Step::Waiting(p) => p,
            other => panic!("{other:?}"),
        };
        (&theirs).write_all(b).unwrap();
        let Step::Arrived(a) = p.read() else {
            panic!("no hello")
        };
        assert_eq!(requested(), a.hello.unwrap());
        assert_eq!(size, a.rest);
        assert!(a.tty.is_none());
    }

    #[test]
    fn a_peer_that_hangs_up_is_gone() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let p = Pending::new(ours, HELLO_TIMEOUT, euid()).unwrap().unwrap();
        drop(theirs);
        assert!(matches!(p.read(), Step::Gone(_)));
    }

    #[test]
    fn a_frame_too_long_for_a_hello_fails_at_its_header() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let p = Pending::new(ours, HELLO_TIMEOUT, euid()).unwrap().unwrap();
        (&theirs).write_all(b"h\xff\xff\xff\xff").unwrap();
        let Step::Arrived(a) = p.read() else {
            panic!("no hello")
        };
        assert_eq!(Refusal::Hello, a.hello.unwrap_err().refusal());
    }

    #[test]
    fn a_silent_peer_times_out_blocking() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let started = Instant::now();
        let p = Pending::new(ours, Duration::from_millis(100), euid())
            .unwrap()
            .unwrap();
        assert!(matches!(p.read_blocking(), Step::Gone(_)));
        assert!(started.elapsed() < Duration::from_secs(2));
        // it was told why
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut &theirs, &mut buf).unwrap();
        let (frame, _) = Frame::decode(&buf).unwrap().unwrap();
        let Some(Message::End(z)) = Message::decode(&frame).unwrap() else {
            panic!("no end")
        };
        assert_eq!(Some(Refusal::Hello), z.as_refusal());
    }

    #[test]
    fn only_a_terminal_under_fd_is_taken() {
        let mut t = match requested().purpose {
            Purpose::Requested { terminal, .. } => terminal,
            _ => unreachable!(),
        };
        let reason = |r: Result<(), End>| r.unwrap_err().as_refusal().unwrap();
        assert_eq!(Refusal::Terminal, reason(check_terminal(&t, None)));
        let mut fds = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        assert_eq!(Refusal::Terminal, reason(check_terminal(&t, Some(&r))));
        drop(w);
        let (master, slave) = super::super::surface::open_pty(10, 10);
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        assert!(check_terminal(&t, Some(&slave)).is_ok());
        t.transport = Transport::Relay;
        assert_eq!(Refusal::Transport, reason(check_terminal(&t, Some(&slave))));
        drop(master);
    }

    #[test]
    fn the_log_line_has_no_environment() {
        let t = match requested().purpose {
            Purpose::Requested { terminal, .. } => terminal,
            _ => unreachable!(),
        };
        let line = describe("requested", Some("1-aaaaaaaaaa"), &t, "x 1");
        assert_eq!(
            "requested pane 1-aaaaaaaaaa transport fd size 80x24 pid 1 tty /dev/ttys001 build x 1",
            line
        );
        assert!(!line.contains("xterm"));
    }
}
