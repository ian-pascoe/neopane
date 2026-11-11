//! The helper (ADR 0013): `<name> attach --address A --pane ID`, which each
//! requested pane's terminal runs, and the launcher's own attach. It checks
//! the daemon runs as its user, sends its hello with the terminal riding on
//! the first byte, and waits for the answer. Once accepted it sends the
//! terminal's size as it changes, until the daemon ends the attach.
//! `docs/attach-protocol.md` is the reference.

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::fdpass;
use crate::attach::{
    End, Ending, Frame, Hello, Message, PaneId, Purpose, Refusal, Size, Terminal, Transport,
};
use crate::term::signals::Signals;

/// How long the helper waits for the daemon's answer.
pub(crate) const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// What the helper attaches as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Want {
    /// A requested pane's terminal, with its pane id.
    Requested(PaneId),
    /// The launcher's terminal.
    SelfAttach,
}

/// How the helper ends: its exit status, and the line it prints on stderr
/// after `<name>: `, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    pub status: u8,
    pub message: Option<String>,
}

impl Exit {
    fn ok() -> Exit {
        Exit {
            status: 0,
            message: None,
        }
    }

    fn fail(message: impl Into<String>) -> Exit {
        Exit {
            status: 1,
            message: Some(message.into()),
        }
    }
}

/// Attaches stdin's terminal to the daemon at `address`, and waits until
/// the attach ends.
pub fn run(address: &OsStr, want: Want, transport: Transport) -> Exit {
    let tty = libc::STDIN_FILENO;
    // SAFETY: `isatty` only inspects the fd.
    if unsafe { libc::isatty(tty) } == 0 {
        return Exit::fail("not a terminal");
    }
    // before the hello, so a resize in between isn't missed
    let mut signals = match Signals::init(&[libc::SIGWINCH]) {
        Ok(s) => s,
        Err(e) => return Exit::fail(format!("can't watch for resizes: {e}")),
    };
    let stream = match UnixStream::connect(address) {
        Ok(s) => s,
        Err(e) => {
            return Exit::fail(format!(
                "can't connect to {}: {e}",
                address.to_string_lossy()
            ));
        }
    };
    let exit = attach_on(
        stream,
        tty,
        want,
        transport,
        Some(&mut signals),
        ANSWER_TIMEOUT,
    );
    signals.deinit();
    exit
}

/// The helper's work on a connected `stream`, for terminal `tty`; `signals`
/// reports SIGWINCH, if given. The daemon has `answer_within` to answer.
pub(crate) fn attach_on(
    mut stream: UnixStream,
    tty: RawFd,
    want: Want,
    transport: Transport,
    mut signals: Option<&mut Signals>,
    answer_within: Duration,
) -> Exit {
    match same_user(&stream) {
        Ok(true) => {}
        Ok(false) => return Exit::fail("the daemon runs as another user"),
        Err(e) => return Exit::fail(format!("can't check the daemon's user: {e}")),
    }
    let terminal = match terminal_of(tty, transport) {
        Ok(t) => t,
        Err(e) => return Exit::fail(format!("can't read the terminal: {e}")),
    };
    let mut size = terminal.size;
    let purpose = match want {
        Want::Requested(pane) => Purpose::Requested { pane, terminal },
        Want::SelfAttach => Purpose::SelfAttach {
            terminal,
            provider: None,
            file: None,
            line: None,
        },
    };
    let hello = match Message::Hello(Hello::new(purpose)).encode() {
        Ok(b) => b,
        Err(_) => return Exit::fail("environment too large"),
    };
    let sent = match transport {
        Transport::Fd => fdpass::send(&stream, &hello, tty),
        Transport::Relay => stream.write_all(&hello),
    };
    if let Err(e) = sent {
        return Exit::fail(format!("can't send the hello: {e}"));
    }

    let mut reader = Reader::default();
    let log = match reader.answer(&mut stream, answer_within) {
        Answer::Accepted(log) => log,
        Answer::Refused(z) => return refused(want, &z),
        Answer::Failed(message) => return Exit::fail(message),
    };
    let gone = || {
        Exit::fail(format!(
            "the daemon went away; see {}",
            String::from_utf8_lossy(&log)
        ))
    };

    // a resize between the hello and now
    if let Some(now) = changed_size(tty, &mut size)
        && send_size(&mut stream, now).is_err()
    {
        return gone();
    }
    loop {
        let mut fds = [libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }; 2];
        let mut n = 1;
        if let Some(s) = &signals {
            fds[1].fd = s.read_fd;
            n = 2;
        }
        // SAFETY: the first `n` pollfds are valid.
        if unsafe { libc::poll(fds.as_mut_ptr(), n as libc::nfds_t, -1) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return gone();
        }
        if n == 2
            && fds[1].revents != 0
            && let Some(s) = signals.as_deref_mut()
            && s.drain().is_ok_and(|set| set.contains(libc::SIGWINCH))
            && let Some(now) = changed_size(tty, &mut size)
            && send_size(&mut stream, now).is_err()
        {
            return gone();
        }
        if fds[0].revents != 0 {
            match reader.next(&mut stream) {
                Ok(Some(Message::End(z))) => return ended(want, &z),
                Ok(Some(_)) => {}
                Ok(None) => {}
                Err(()) => return gone(),
            }
        }
    }
}

/// The exit for a refusal: a requested pane that is no longer wanted, or a
/// daemon stopping, exits quietly.
pub(crate) fn refused(want: Want, z: &End) -> Exit {
    match (want, z.as_refusal()) {
        (Want::Requested(_), Some(Refusal::PaneExpired | Refusal::Stopping)) => Exit::ok(),
        _ => Exit::fail(z.message.clone()),
    }
}

/// The exit for an attach the daemon ended.
pub(crate) fn ended(want: Want, z: &End) -> Exit {
    match z.as_ending() {
        Ending::Closed | Ending::Stopped | Ending::Detached => Exit::ok(),
        Ending::Quit => Exit {
            status: match want {
                Want::SelfAttach => z.exit.unwrap_or(0),
                Want::Requested(_) => 0,
            },
            message: None,
        },
        Ending::Failed => Exit::fail(z.message.clone()),
    }
}

/// What the daemon answered a hello.
enum Answer {
    /// `a`, with the daemon's log's path.
    Accepted(Vec<u8>),
    Refused(End),
    Failed(String),
}

/// Frames read from the daemon.
#[derive(Default)]
struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    /// Reads until the answer, for up to `within`.
    fn answer(&mut self, stream: &mut UnixStream, within: Duration) -> Answer {
        let deadline = Instant::now() + within;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let secs = within.as_secs_f64();
                return Answer::Failed(format!("no answer from the daemon in {secs} s"));
            }
            let mut fds = [libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) } <= 0 {
                continue;
            }
            match self.next(stream) {
                Ok(Some(Message::Accept(a))) => return Answer::Accepted(a.log),
                Ok(Some(Message::End(z))) => return Answer::Refused(z),
                Ok(_) => {}
                Err(()) => return Answer::Failed("the daemon hung up".into()),
            }
        }
    }

    /// One read, and the next whole message, if any. Unknown kinds are
    /// skipped. Err when the daemon hung up or broke the protocol.
    fn next(&mut self, stream: &mut UnixStream) -> Result<Option<Message>, ()> {
        if let Some(m) = self.decoded()? {
            return Ok(Some(m));
        }
        let mut chunk = [0u8; 4096];
        let n = loop {
            match stream.read(&mut chunk) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(()),
                Ok(n) => break n,
            }
        };
        if n == 0 {
            return Err(());
        }
        self.buf.extend_from_slice(&chunk[..n]);
        self.decoded()
    }

    fn decoded(&mut self) -> Result<Option<Message>, ()> {
        while let Some((frame, n)) = Frame::decode(&self.buf).map_err(|_| ())? {
            self.buf.drain(..n);
            if let Some(m) = Message::decode(&frame).map_err(|_| ())? {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }
}

fn send_size(stream: &mut UnixStream, size: Size) -> io::Result<()> {
    let bytes = Message::Size(size).encode().map_err(io::Error::other)?;
    stream.write_all(&bytes)
}

/// The terminal's size now, if it differs from `last`, which it updates.
fn changed_size(tty: RawFd, last: &mut Size) -> Option<Size> {
    let now = size_of(tty).ok()?;
    if now == *last {
        return None;
    }
    *last = now;
    Some(now)
}

/// The terminal's size, as `TIOCGWINSZ` has it.
pub(crate) fn size_of(tty: RawFd) -> io::Result<Size> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCGWINSZ writes a winsize through the pointer.
    if unsafe { libc::ioctl(tty, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let px = (ws.ws_xpixel != 0 && ws.ws_ypixel != 0)
        .then_some((u32::from(ws.ws_xpixel), u32::from(ws.ws_ypixel)));
    Ok(Size {
        cols: ws.ws_col,
        rows: ws.ws_row,
        px,
    })
}

/// What the hello says of terminal `tty`: its size and device, and this
/// process's pid, environment and working directory.
fn terminal_of(tty: RawFd, transport: Transport) -> io::Result<Terminal> {
    let mut env: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for (k, v) in std::env::vars_os() {
        let k = k.as_bytes().to_vec();
        if !env.iter().any(|(seen, _)| *seen == k) {
            env.push((k, v.as_bytes().to_vec()));
        }
    }
    Ok(Terminal {
        transport,
        size: size_of(tty)?,
        pid: std::process::id(),
        tty: tty_name(tty),
        env,
        cwd: std::env::current_dir()
            .map(|d| d.as_os_str().as_bytes().to_vec())
            .unwrap_or_default(),
    })
}

/// The terminal's device path; empty if it has none.
fn tty_name(tty: RawFd) -> Vec<u8> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for its length.
    let r = unsafe { libc::ttyname_r(tty, buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return Vec::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    buf[..end].to_vec()
}

/// The user id of the process on the other end of `stream`.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        // SAFETY: `cred` and `len` are valid for the call.
        let r = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(cred.uid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let (mut uid, mut gid) = (0, 0);
        // SAFETY: both out-pointers are valid.
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

/// Only the same user is let in, at either end.
fn check_peer(peer: u32, ours: u32) -> bool {
    peer == ours
}

/// Whether the process on the other end of `stream` runs as our user: the
/// peer check both ends make before the hello.
pub(crate) fn same_user(stream: &UnixStream) -> io::Result<bool> {
    // SAFETY: `geteuid` has no preconditions.
    Ok(check_peer(peer_uid(stream)?, unsafe { libc::geteuid() }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{Accept, Message};
    use crate::daemon::surface::open_pty;
    use std::os::fd::{FromRawFd, OwnedFd};

    #[test]
    fn only_our_own_user_is_let_in() {
        assert!(check_peer(501, 501));
        assert!(!check_peer(0, 501));
    }

    #[test]
    fn a_peer_is_our_user() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(unsafe { libc::geteuid() }, peer_uid(&a).unwrap());
        assert!(same_user(&a).unwrap());
    }

    fn pane() -> PaneId {
        PaneId::parse("3-abcdefghij").unwrap()
    }

    /// Runs the helper over a pty of 80 x 24 against `daemon`, a fake
    /// daemon's side of the socket given the hello and the terminal; returns
    /// the helper's exit.
    fn against(
        want: Want,
        daemon: impl FnOnce(UnixStream, Hello, OwnedFd) + Send + 'static,
    ) -> Exit {
        let (master, slave) = open_pty(80, 24);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 20];
            let mut got = Vec::new();
            let mut tty = None;
            loop {
                let r = fdpass::recv(&theirs, &mut buf).unwrap();
                if let Some(fd) = r.fd {
                    tty = Some(unsafe { OwnedFd::from_raw_fd(fd) });
                }
                got.extend_from_slice(&buf[..r.len]);
                if let Some((f, _)) = Frame::decode(&got).unwrap() {
                    let hello = Message::decode_hello(&f).unwrap();
                    return daemon(theirs, hello, tty.expect("the terminal came"));
                }
            }
        });
        let exit = attach_on(ours, slave, want, Transport::Fd, None, ANSWER_TIMEOUT);
        fake.join().unwrap();
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        exit
    }

    fn send(mut s: &UnixStream, m: Message) {
        s.write_all(&m.encode().unwrap()).unwrap();
    }

    fn accept() -> Message {
        Message::Accept(Accept {
            v: 1,
            build: "x".into(),
            daemon: "d".into(),
            log: b"/tmp/log".to_vec(),
            features: vec![],
        })
    }

    #[test]
    fn the_hello_carries_the_pane_and_the_terminal() {
        let exit = against(Want::Requested(pane()), |s, hello, tty| {
            let Purpose::Requested { pane: p, terminal } = hello.purpose else {
                panic!("{hello:?}")
            };
            assert_eq!(pane(), p);
            assert_eq!(Transport::Fd, terminal.transport);
            assert_eq!((80, 24), (terminal.size.cols, terminal.size.rows));
            assert_eq!(std::process::id(), terminal.pid);
            assert!(terminal.tty.starts_with(b"/dev/"), "{:?}", terminal.tty);
            assert!(terminal.env.iter().any(|(k, _)| k == b"PATH"));
            assert_eq!(1, unsafe {
                libc::isatty(std::os::fd::AsRawFd::as_raw_fd(&tty))
            });
            send(&s, accept());
            send(
                &s,
                Message::End(End::ending(Ending::Closed, "closed", false)),
            );
        });
        assert_eq!(Exit::ok(), exit);
    }

    #[test]
    fn a_pane_no_longer_wanted_exits_0_quietly() {
        for reason in [Refusal::PaneExpired, Refusal::Stopping] {
            let exit = against(Want::Requested(pane()), move |s, _, _| {
                send(&s, Message::End(End::refusal(reason, "gone")));
            });
            assert_eq!(Exit::ok(), exit, "{reason:?}");
        }
    }

    #[test]
    fn any_other_refusal_exits_1_with_its_message() {
        for reason in [Refusal::PaneTaken, Refusal::PaneUnknown, Refusal::Version] {
            let exit = against(Want::Requested(pane()), move |s, _, _| {
                send(&s, Message::End(End::refusal(reason, "no thanks")));
            });
            assert_eq!(Exit::fail("no thanks"), exit, "{reason:?}");
        }
        // a launcher's attach refused as stopping isn't quiet
        let exit = against(Want::SelfAttach, |s, _, _| {
            send(
                &s,
                Message::End(End::refusal(Refusal::Stopping, "stopping")),
            );
        });
        assert_eq!(1, exit.status);
    }

    #[test]
    fn a_failed_attach_exits_1_and_a_daemon_gone_names_its_log() {
        let exit = against(Want::Requested(pane()), |s, _, _| {
            send(&s, accept());
            send(
                &s,
                Message::End(End::ending(Ending::Failed, "it broke", false)),
            );
        });
        assert_eq!(Exit::fail("it broke"), exit);
        let exit = against(Want::Requested(pane()), |s, _, _| {
            send(&s, accept());
            drop(s);
        });
        assert_eq!(Exit::fail("the daemon went away; see /tmp/log"), exit);
        let exit = against(Want::Requested(pane()), |s, _, _| drop(s));
        assert_eq!(Exit::fail("the daemon hung up"), exit);
    }

    #[test]
    fn quit_ends_a_self_attach_with_neovims_status() {
        let quit = |exit| {
            let mut z = End::ending(Ending::Quit, "quit", false);
            z.exit = exit;
            Message::End(z)
        };
        let exit = against(Want::SelfAttach, move |s, _, _| {
            send(&s, accept());
            send(&s, quit(Some(3)));
        });
        assert_eq!(3, exit.status);
        let exit = against(Want::Requested(pane()), move |s, _, _| {
            send(&s, accept());
            send(&s, quit(Some(3)));
        });
        assert_eq!(Exit::ok(), exit);
    }

    #[test]
    fn no_answer_in_time_exits_1() {
        let (master, slave) = open_pty(80, 24);
        let (ours, silent) = UnixStream::pair().unwrap();
        // takes the hello, and never answers
        let reader = std::thread::spawn(move || {
            let _ = std::io::copy(&mut &silent, &mut std::io::sink());
        });
        let started = Instant::now();
        let exit = attach_on(
            ours,
            slave,
            Want::Requested(pane()),
            Transport::Fd,
            None,
            Duration::from_millis(200),
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(Exit::fail("no answer from the daemon in 0.2 s"), exit);
        reader.join().unwrap();
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn unknown_frames_are_skipped() {
        let exit = against(Want::Requested(pane()), |mut s, _, _| {
            let mut odd = Vec::new();
            Frame::encode_into(b'q', b"??", &mut odd).unwrap();
            s.write_all(&odd).unwrap();
            send(&s, accept());
            s.write_all(&odd).unwrap();
            send(
                &s,
                Message::End(End::ending(Ending::Stopped, "stopped", false)),
            );
        });
        assert_eq!(Exit::ok(), exit);
    }

    #[test]
    fn a_resize_since_the_hello_is_sent() {
        let (master, slave) = open_pty(80, 24);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 20];
            let mut got = Vec::new();
            loop {
                let r = fdpass::recv(&theirs, &mut buf).unwrap();
                if let Some(fd) = r.fd {
                    drop(unsafe { OwnedFd::from_raw_fd(fd) });
                }
                got.extend_from_slice(&buf[..r.len]);
                if let Some((_, n)) = Frame::decode(&got).unwrap() {
                    got.drain(..n);
                    break;
                }
            }
            // resized before the answer
            let ws = libc::winsize {
                ws_row: 30,
                ws_col: 100,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            assert_eq!(0, unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &ws) });
            send(&theirs, accept());
            loop {
                if let Some((f, _)) = Frame::decode(&got).unwrap() {
                    let Some(Message::Size(s)) = Message::decode(&f).unwrap() else {
                        panic!("{f:?}")
                    };
                    assert_eq!((100, 30), (s.cols, s.rows));
                    break;
                }
                let r = fdpass::recv(&theirs, &mut buf).unwrap();
                got.extend_from_slice(&buf[..r.len]);
            }
            send(
                &theirs,
                Message::End(End::ending(Ending::Closed, "", false)),
            );
            master
        });
        let exit = attach_on(
            ours,
            slave,
            Want::Requested(pane()),
            Transport::Fd,
            None,
            ANSWER_TIMEOUT,
        );
        let master = fake.join().unwrap();
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        assert_eq!(Exit::ok(), exit);
    }
}
