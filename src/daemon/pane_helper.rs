//! The helper (ADR 0013): `<name> attach --address A --pane ID`, which each
//! requested pane's terminal runs, and the launcher's own attach. It checks
//! the daemon runs as its user, puts its terminal in raw mode, sends its
//! hello with the terminal riding on the first byte, and waits for the
//! answer. Once accepted it sends the terminal's size as it changes, and
//! keeps the latest undo bytes, until the attach ends; then it undoes what
//! the daemon wrote and restores its terminal, however the attach ended.
//! `docs/attach-protocol.md` is the reference.

use std::io::{self, Read, Write};
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::dir::Address;
use super::fdpass;
use crate::attach::frame;
use crate::attach::{
    End, Ending, Frame, Hello, Message, PaneId, Purpose, Refusal, Size, Terminal, Transport,
};
use crate::name::NAME;
use crate::term::raw::Saved;
use crate::term::signals::{self, Signals};

/// How long the helper waits for the daemon's answer.
pub(crate) const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// The helper never exits sooner after it starts, except by a signal: a
/// terminal that closes as its command exits would take a failure's line
/// with it unread.
pub(crate) const MIN_LIFE: Duration = Duration::from_millis(300);

/// Written before the undo bytes when the daemon cut a frame short, or went
/// without a word: CAN ends a half CSI, and `ESC \` a string.
pub(crate) const CANCEL: &[u8] = b"\x18\x1b\\";

/// The signals that stop the helper. SIGHUP keeps its default action: the
/// terminal is gone.
const STOPPING: [i32; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGQUIT];

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
    /// Stopped by a signal: at once, quietly.
    pub(crate) by_signal: bool,
}

impl Exit {
    fn ok() -> Exit {
        Exit {
            status: 0,
            message: None,
            by_signal: false,
        }
    }

    fn fail(message: impl Into<String>) -> Exit {
        Exit {
            status: 1,
            message: Some(message.into()),
            by_signal: false,
        }
    }

    fn signalled() -> Exit {
        Exit {
            status: 1,
            message: None,
            by_signal: true,
        }
    }
}

/// Attaches stdin's terminal to the daemon at `address`, and waits until
/// the attach ends. A failed self-attach then waits for a key, unless a
/// shell started it as a job.
pub fn run(address: &Address, want: Want, transport: Transport) -> Exit {
    let how = Attach::new(want, transport);
    let exit = attach_stdin(address, &how);
    if !exit.by_signal {
        wait_out(how.started + how.min_life);
    }
    exit
}

fn attach_stdin(address: &Address, how: &Attach) -> Exit {
    let tty = libc::STDIN_FILENO;
    // SAFETY: `isatty` only inspects the fd.
    if unsafe { libc::isatty(tty) } == 0 {
        return Exit::fail("not a terminal");
    }
    // before the hello, so a resize in between isn't missed
    let mut watched = vec![libc::SIGWINCH];
    watched.extend(STOPPING);
    let mut signals = match Signals::init(&watched) {
        Ok(s) => s,
        Err(e) => return Exit::fail(format!("can't watch for signals: {e}")),
    };
    let exit = match address.connect() {
        Ok(stream) => attach_on(stream, tty, how, Some(&mut signals)),
        Err(e) => Exit::fail(format!("can't connect to {address}: {e}")),
    };
    let exit = match how.want {
        Want::SelfAttach => {
            let mut stderr = io::stderr();
            let due = how.started + how.min_life;
            hold_failure(exit, tty, shell_job(), due, &mut stderr, Some(&mut signals))
        }
        Want::Requested(_) => exit,
    };
    signals.deinit();
    exit
}

/// How the helper attaches.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Attach {
    pub(crate) want: Want,
    pub(crate) transport: Transport,
    /// How long the daemon has to answer.
    pub(crate) answer_within: Duration,
    /// The user the daemon must run as: ours.
    pub(crate) uid: u32,
    /// When the helper started, and how long it lives at least.
    pub(crate) started: Instant,
    pub(crate) min_life: Duration,
}

impl Attach {
    pub(crate) fn new(want: Want, transport: Transport) -> Attach {
        Attach {
            want,
            transport,
            answer_within: ANSWER_TIMEOUT,
            // SAFETY: `geteuid` has no preconditions.
            uid: unsafe { libc::geteuid() },
            started: Instant::now(),
            min_life: MIN_LIFE,
        }
    }
}

/// The helper's work on a connected `stream`, for terminal `tty`, as `how`
/// says; `signals` reports SIGWINCH and the stopping signals, if given. The
/// daemon is checked to run as `how.uid` before the hello. Unless a signal
/// stopped it, it returns no sooner than `how.min_life` after
/// `how.started`.
pub(crate) fn attach_on(
    stream: UnixStream,
    tty: RawFd,
    how: &Attach,
    signals: Option<&mut Signals>,
) -> Exit {
    let exit = attach_with(stream, tty, how, signals);
    if !exit.by_signal {
        wait_out(how.started + how.min_life);
    }
    exit
}

fn attach_with(
    mut stream: UnixStream,
    tty: RawFd,
    how: &Attach,
    mut signals: Option<&mut Signals>,
) -> Exit {
    let Attach {
        want,
        transport,
        answer_within,
        uid,
        ..
    } = *how;
    match peer_uid(&stream) {
        Ok(peer) if check_peer(peer, uid) => {}
        Ok(_) => return Exit::fail("the daemon runs as another user"),
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
            provider: provider_hint(),
            file: None,
            line: None,
        },
    };
    let hello = match Message::Hello(Hello::new(purpose)).encode() {
        Ok(b) => b,
        Err(_) => return Exit::fail("environment too large"),
    };
    // raw before the hello, keeping what was typed for the daemon
    let saved = match Saved::raw(tty) {
        Ok(s) => s,
        Err(e) => return Exit::fail(format!("can't set the terminal's mode: {e}")),
    };
    let mut restore = Restore {
        tty,
        saved,
        undo: Vec::new(),
        due: how.started + how.min_life,
    };
    let sent = match transport {
        Transport::Fd => fdpass::send(&stream, &hello, tty),
        Transport::Relay => stream.write_all(&hello),
    };
    if let Err(e) = sent {
        restore.end(false);
        return Exit::fail(format!("can't send the hello: {e}"));
    }

    let mut reader = Reader::default();
    let log = match reader.answer(&mut stream, answer_within, signals.as_deref_mut()) {
        Answer::Accepted(log) => log,
        Answer::Refused(z) => {
            restore.end(false);
            return refused(want, &z);
        }
        Answer::Failed(message) => {
            restore.end(false);
            return Exit::fail(message);
        }
        Answer::Signalled => {
            // `a` and `u` may have come all the same
            if let Some(undo) = reader.latest_undo(&mut stream) {
                restore.undo = undo;
            }
            drop(stream);
            restore.stopped();
            return Exit::signalled();
        }
    };
    let gone = |restore: &Restore| {
        restore.end(true);
        Exit::fail(format!(
            "the daemon went away; see {}",
            String::from_utf8_lossy(&log)
        ))
    };

    // a resize between the hello and now
    if let Some(now) = changed_size(tty, &mut size)
        && send_size(&mut stream, now).is_err()
    {
        return gone(&restore);
    }
    loop {
        // what came with the answer, or in the last read
        loop {
            match reader.decoded() {
                Ok(Some(Message::Undo(undo))) => restore.undo = undo,
                Ok(Some(Message::Output(o))) => write_blocking(tty, &o),
                Ok(Some(Message::End(z))) => {
                    restore.end(z.cut);
                    return ended(want, &z);
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(()) => return gone(&restore),
            }
        }
        // the socket, the signals, and under `relay` the terminal
        let mut fds = [libc::pollfd {
            fd: -1,
            events: libc::POLLIN,
            revents: 0,
        }; 3];
        fds[0].fd = stream.as_raw_fd();
        if let Some(s) = &signals {
            fds[1].fd = s.read_fd;
        }
        if transport == Transport::Relay {
            fds[2].fd = tty;
        }
        // SAFETY: valid pollfds; those of fd -1 are ignored.
        if unsafe { libc::poll(fds.as_mut_ptr(), 3, -1) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return gone(&restore);
        }
        if fds[2].revents != 0 {
            match relay_input(tty, &mut stream) {
                Relayed::Sent => {}
                // the daemon closes the pane; what's left of the terminal
                // is restored as on any other way out
                Relayed::TerminalGone => {
                    drop(stream);
                    restore.end(false);
                    return Exit::ok();
                }
                Relayed::ReadFailed(e) => {
                    drop(stream);
                    restore.end(false);
                    return Exit::fail(format!("can't read the terminal: {e}"));
                }
                Relayed::DaemonGone => return gone(&restore),
            }
        }
        if fds[1].revents != 0
            && let Some(s) = signals.as_deref_mut()
        {
            let fired = s.drain().unwrap_or_default();
            if stopping(fired) {
                if let Some(undo) = reader.latest_undo(&mut stream) {
                    restore.undo = undo;
                }
                drop(stream);
                restore.stopped();
                return Exit::signalled();
            }
            if fired.contains(libc::SIGWINCH)
                && let Some(now) = changed_size(tty, &mut size)
                && send_size(&mut stream, now).is_err()
            {
                return gone(&restore);
            }
        }
        if fds[0].revents != 0 && reader.read_more(&mut stream).is_err() {
            // the daemon's last frames, if it said any before it went
            while let Ok(Some(m)) = reader.decoded() {
                match m {
                    Message::Undo(undo) => restore.undo = undo,
                    Message::Output(o) => write_blocking(tty, &o),
                    Message::End(z) => {
                        restore.end(z.cut);
                        return ended(want, &z);
                    }
                    _ => {}
                }
            }
            return gone(&restore);
        }
    }
}

/// What relaying the terminal's input did.
enum Relayed {
    Sent,
    /// The terminal hung up: a read of nothing, or EIO, as Linux has a
    /// pty's reads once its master closes.
    TerminalGone,
    /// Reading the terminal failed otherwise.
    ReadFailed(io::Error),
    /// The daemon's socket failed.
    DaemonGone,
}

/// Sends what was typed on terminal `tty`, which has some, in `i` frames,
/// waiting for room.
fn relay_input(tty: RawFd, stream: &mut UnixStream) -> Relayed {
    let mut buf = [0u8; 16 << 10];
    let n = loop {
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::read(tty, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            break n as usize;
        }
        if n == 0 {
            return Relayed::TerminalGone;
        }
        let e = io::Error::last_os_error();
        match e.kind() {
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => return Relayed::Sent,
            _ if e.raw_os_error() == Some(libc::EIO) => return Relayed::TerminalGone,
            _ => return Relayed::ReadFailed(e),
        }
    };
    let mut frames = Vec::new();
    frame::encode_split(b'i', &buf[..n], &mut frames).expect("split to fit");
    match stream.write_all(&frames) {
        Ok(()) => Relayed::Sent,
        Err(_) => Relayed::DaemonGone,
    }
}

/// Whether `fired` holds a signal that stops the helper.
fn stopping(fired: signals::Set) -> bool {
    STOPPING.iter().any(|&sig| fired.contains(sig))
}

/// The helper's terminal as it was, and what undoes the daemon's output.
struct Restore {
    tty: RawFd,
    saved: Saved,
    /// The latest `u`.
    undo: Vec<u8>,
    /// Not restored before then: the helper's minimum life.
    due: Instant,
}

impl Restore {
    /// Writes CAN and `ESC \` if `cut`, and the undo bytes, then restores
    /// the terminal once its minimum life is up, dropping what was typed:
    /// a reply to a query the daemon wrote has had time to arrive by then.
    fn end(&self, cut: bool) {
        self.undo(cut);
        wait_out(self.due);
        self.saved.restore();
    }

    /// `end` for a signal: at once, after CAN and `ESC \`, since the daemon
    /// may be mid-frame.
    fn stopped(&self) {
        self.undo(true);
        self.saved.restore();
    }

    fn undo(&self, cut: bool) {
        let mut out = Vec::with_capacity(CANCEL.len() + self.undo.len());
        if cut {
            out.extend_from_slice(CANCEL);
        }
        out.extend_from_slice(&self.undo);
        write_blocking(self.tty, &out);
    }
}

/// Writes all of `bytes` to `fd`, waiting for room as long as it takes,
/// though a daemon left its open file non-blocking. Errors end it: the
/// terminal is gone.
fn write_blocking(fd: RawFd, bytes: &[u8]) {
    let mut done = 0;
    while done < bytes.len() {
        let rest = &bytes[done..];
        // SAFETY: `rest` is valid for `rest.len()` bytes.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n > 0 {
            done += n as usize;
            continue;
        }
        match io::Error::last_os_error().kind() {
            _ if n == 0 => return,
            io::ErrorKind::Interrupted => {}
            io::ErrorKind::WouldBlock => {
                let mut fds = [libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                }];
                // SAFETY: one valid pollfd.
                unsafe { libc::poll(fds.as_mut_ptr(), 1, -1) };
            }
            _ => return,
        }
    }
}

/// Sleeps until `due`, if it's still ahead.
fn wait_out(due: Instant) {
    let left = due.saturating_duration_since(Instant::now());
    if !left.is_zero() {
        std::thread::sleep(left);
    }
}

/// Waits for a key on terminal `tty`, or a stopping signal, then puts its
/// settings back and drops what else was typed.
pub(crate) fn wait_for_key(tty: RawFd, mut signals: Option<&mut Signals>) {
    let Ok(saved) = Saved::save(tty) else {
        return;
    };
    // blocking, so the read below waits
    saved.restore_flags();
    if saved.make_keyed().is_err() {
        return;
    }
    loop {
        let mut fds = [libc::pollfd {
            fd: tty,
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
            break;
        }
        if fds[0].revents != 0 {
            break;
        }
        if let Some(s) = signals.as_deref_mut()
            && stopping(s.drain().unwrap_or_default())
        {
            break;
        }
    }
    saved.restore();
}

/// The launcher's failure before it could attach, such as a daemon that
/// didn't start, held as a failed self-attach is: see `hold_failure`.
/// `started` is when the launcher started.
pub fn launcher_failed(message: String, started: Instant) -> Exit {
    let exit = Exit::fail(message);
    let tty = libc::STDIN_FILENO;
    // a key wait ended by a stopping signal still restores the terminal
    let Ok(mut signals) = Signals::init(&STOPPING) else {
        return exit;
    };
    let job = shell_job();
    let exit = hold_failure(
        exit,
        tty,
        job,
        started + MIN_LIFE,
        &mut io::stderr(),
        Some(&mut signals),
    );
    signals.deinit();
    exit
}

/// A failed self-attach's exit, once its line is shown: on terminal `tty`,
/// unless a shell started it as a `job`, it writes the line to `out` with
/// ` (press a key)` no sooner than `due`, and waits for a key, so the line
/// stays on a terminal that closes as it exits. The exit then has no line
/// left to print. Any other exit is left as it is.
pub(crate) fn hold_failure(
    exit: Exit,
    tty: RawFd,
    job: bool,
    due: Instant,
    out: &mut dyn Write,
    signals: Option<&mut Signals>,
) -> Exit {
    // SAFETY: `isatty` only inspects the fd.
    let terminal = unsafe { libc::isatty(tty) } == 1;
    let Some(m) = exit.message.as_ref().filter(|_| exit.status != 0) else {
        return exit;
    };
    if job || !terminal {
        return exit;
    }
    wait_out(due);
    let _ = write!(out, "{NAME}: {m} (press a key)");
    let _ = out.flush();
    wait_for_key(tty, signals);
    let _ = writeln!(out);
    Exit {
        message: None,
        ..exit
    }
}

/// A process's group and session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Ids {
    pub(crate) pgid: libc::pid_t,
    pub(crate) sid: libc::pid_t,
}

/// Whether a shell started the process of `ours` as a job: its parent is in
/// the same session, in another process group. A failed self-attach waits
/// for a key only when it isn't, so the line stays on a terminal that
/// closes as it exits.
pub(crate) fn is_shell_job(ours: Ids, parent: Option<Ids>) -> bool {
    parent.is_some_and(|p| p.sid == ours.sid && p.pgid != ours.pgid)
}

/// `is_shell_job` for this process.
fn shell_job() -> bool {
    // SAFETY: plain process-id queries.
    unsafe {
        let ours = Ids {
            pgid: libc::getpgrp(),
            sid: libc::getsid(0),
        };
        let ppid = libc::getppid();
        let parent = Ids {
            pgid: libc::getpgid(ppid),
            sid: libc::getsid(ppid),
        };
        let known = parent.pgid >= 0 && parent.sid >= 0;
        is_shell_job(ours, known.then_some(parent))
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
            ..Exit::ok()
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
    /// A stopping signal came first.
    Signalled,
}

/// Frames read from the daemon.
#[derive(Default)]
struct Reader {
    buf: Vec<u8>,
}

impl Reader {
    /// Reads until the answer, for up to `within`, or a stopping signal.
    fn answer(
        &mut self,
        stream: &mut UnixStream,
        within: Duration,
        mut signals: Option<&mut Signals>,
    ) -> Answer {
        let deadline = Instant::now() + within;
        loop {
            match self.decoded() {
                Ok(Some(Message::Accept(a))) => return Answer::Accepted(a.log),
                Ok(Some(Message::End(z))) => return Answer::Refused(z),
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(()) => return Answer::Failed("the daemon broke the protocol".into()),
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                let secs = within.as_secs_f64();
                return Answer::Failed(format!("no answer from the daemon in {secs} s"));
            }
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
            let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
            // SAFETY: the first `n` pollfds are valid.
            if unsafe { libc::poll(fds.as_mut_ptr(), n as libc::nfds_t, ms) } <= 0 {
                continue;
            }
            // a resize is sent once accepted
            if n == 2
                && fds[1].revents != 0
                && let Some(s) = signals.as_deref_mut()
                && stopping(s.drain().unwrap_or_default())
            {
                return Answer::Signalled;
            }
            if fds[0].revents != 0 && self.read_more(stream).is_err() {
                return match self.decoded() {
                    Ok(Some(Message::Accept(a))) => Answer::Accepted(a.log),
                    Ok(Some(Message::End(z))) => Answer::Refused(z),
                    _ => Answer::Failed("the daemon hung up".into()),
                };
            }
        }
    }

    /// One read into the buffer. Err when the daemon hung up, or the read
    /// failed.
    fn read_more(&mut self, stream: &mut UnixStream) -> Result<(), ()> {
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
        Ok(())
    }

    /// The latest `u` among what the daemon has sent, read without
    /// waiting; None if it sent none.
    fn latest_undo(&mut self, stream: &mut UnixStream) -> Option<Vec<u8>> {
        let _ = stream.set_nonblocking(true);
        while self.read_more(stream).is_ok() {}
        let mut latest = None;
        while let Ok(Some(m)) = self.decoded() {
            if let Message::Undo(undo) = m {
                latest = Some(undo);
            }
        }
        latest
    }

    /// The next whole message in the buffer, if any. Unknown kinds are
    /// skipped. Err when the daemon broke the protocol.
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

/// The pane provider a self-attach hints at: `$<NAME>_PROVIDER`, when set
/// and not empty.
fn provider_hint() -> Option<String> {
    let hint = std::env::var_os(format!("{}_PROVIDER", crate::name::upper()))?;
    Some(hint.to_string_lossy().into_owned()).filter(|h| !h.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::{Accept, Message};
    use crate::daemon::surface::open_pty;
    use crate::term::raw;
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
    }

    fn pane() -> PaneId {
        PaneId::parse("3-abcdefghij").unwrap()
    }

    /// A fake daemon's side of an attach: its socket, the hello, the
    /// terminal handed over, and the pty's master, which plays the terminal
    /// emulator.
    struct Fake {
        s: UnixStream,
        hello: Hello,
        /// None under the relay.
        tty: Option<OwnedFd>,
        master: RawFd,
    }

    impl Fake {
        /// The terminal handed over.
        fn tty(&self) -> RawFd {
            self.tty.as_ref().expect("the terminal came").as_raw_fd()
        }
    }

    /// What a helper run against a fake daemon did.
    struct Run {
        exit: Exit,
        /// What the helper wrote to its terminal, and the daemon's leftovers.
        shown: Vec<u8>,
        /// The settings are as before, and the file is blocking.
        restored: bool,
        blocking: bool,
        /// Since `how.started`, as the minimum life counts.
        took: Duration,
        /// Input left for whatever reads the terminal next, read raw.
        left: Vec<u8>,
    }

    /// Takes the hello and the terminal, if handed over, from `theirs`.
    fn take_hello(theirs: &UnixStream) -> (Hello, Option<OwnedFd>) {
        let mut buf = vec![0u8; 1 << 20];
        let mut got = Vec::new();
        let mut tty = None;
        loop {
            let r = fdpass::recv(theirs, &mut buf).unwrap();
            if let Some(fd) = r.fd {
                tty = Some(unsafe { OwnedFd::from_raw_fd(fd) });
            }
            assert!(r.len > 0, "the helper hung up");
            got.extend_from_slice(&buf[..r.len]);
            if let Some((f, _)) = Frame::decode(&got).unwrap() {
                let hello = Message::decode_hello(&f).unwrap();
                return (hello, tty);
            }
        }
    }

    /// Everything the pty's master has to read now.
    fn drain(master: RawFd) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        unsafe {
            let fl = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, fl | libc::O_NONBLOCK);
            loop {
                let n = libc::read(master, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
        }
        out
    }

    /// What terminal `fd` has to read now, read raw so a part line counts;
    /// its settings and flags are put back after.
    fn unread(fd: RawFd) -> Vec<u8> {
        let saved = Saved::raw(fd).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK);
            loop {
                let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    break;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
        }
        saved.restore();
        out
    }

    fn is_blocking(fd: RawFd) -> bool {
        unsafe { libc::fcntl(fd, libc::F_GETFL) & libc::O_NONBLOCK == 0 }
    }

    /// Runs the helper as `how` says over a pty of 80 x 24 against
    /// `daemon`.
    fn against_with(
        how: Attach,
        signals: Option<&mut Signals>,
        daemon: impl FnOnce(Fake) + Send + 'static,
    ) -> Run {
        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            let (hello, tty) = take_hello(&theirs);
            daemon(Fake {
                s: theirs,
                hello,
                tty,
                master,
            })
        });
        let exit = attach_on(ours, slave, &how, signals);
        // from the start the minimum life counts from
        let took = how.started.elapsed();
        fake.join().unwrap();
        let restored = raw::same(&before, &raw::settings(slave));
        let blocking = is_blocking(slave);
        // before the master closes: on Linux the slave is hung up then
        let run = Run {
            exit,
            shown: drain(master),
            restored,
            blocking,
            took,
            left: unread(slave),
        };
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        run
    }

    fn against(want: Want, daemon: impl FnOnce(Fake) + Send + 'static) -> Run {
        against_with(Attach::new(want, Transport::Fd), None, daemon)
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

    fn undo(bytes: &[u8]) -> Message {
        Message::Undo(bytes.to_vec())
    }

    fn end(ending: Ending, message: &str, cut: bool) -> Message {
        Message::End(End::ending(ending, message, cut))
    }

    #[test]
    fn the_hello_carries_the_pane_and_the_terminal_already_raw() {
        let run = against(Want::Requested(pane()), |f| {
            let Purpose::Requested { pane: p, terminal } = &f.hello.purpose else {
                panic!("{:?}", f.hello)
            };
            assert_eq!(pane(), *p);
            assert_eq!(Transport::Fd, terminal.transport);
            assert_eq!((80, 24), (terminal.size.cols, terminal.size.rows));
            assert_eq!(std::process::id(), terminal.pid);
            assert!(terminal.tty.starts_with(b"/dev/"), "{:?}", terminal.tty);
            assert!(terminal.env.iter().any(|(k, _)| k == b"PATH"));
            assert_eq!(1, unsafe { libc::isatty(f.tty()) });
            let now = raw::settings(f.tty());
            assert_eq!(0, now.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG));
            send(&f.s, accept());
            send(&f.s, end(Ending::Closed, "closed", false));
        });
        assert_eq!(Exit::ok(), run.exit);
        assert!(run.restored && run.blocking);
    }

    #[test]
    fn what_was_typed_before_the_answer_reaches_the_daemon() {
        let run = against(Want::Requested(pane()), |f| {
            // typed as the pane opens, before the daemon accepts
            assert_eq!(3, unsafe {
                libc::write(f.master, b"abc".as_ptr().cast(), 3)
            });
            send(&f.s, accept());
            let mut buf = [0u8; 8];
            let n = unsafe { libc::read(f.tty(), buf.as_mut_ptr().cast(), 8) };
            assert_eq!(b"abc", &buf[..n as usize]);
            send(&f.s, end(Ending::Closed, "", false));
        });
        assert_eq!(Exit::ok(), run.exit);
        // unechoed: raw mode came first
        assert!(run.shown.is_empty(), "{:?}", run.shown);
    }

    #[test]
    fn at_the_end_the_latest_undo_is_written_after_cancel_only_if_cut() {
        for cut in [false, true] {
            let run = against(Want::Requested(pane()), move |f| {
                send(&f.s, accept());
                send(&f.s, undo(b"<first>"));
                send(&f.s, undo(b"<latest>"));
                send(&f.s, end(Ending::Closed, "", cut));
            });
            assert_eq!(Exit::ok(), run.exit);
            let want: &[u8] = if cut {
                b"\x18\x1b\\<latest>"
            } else {
                b"<latest>"
            };
            assert_eq!(want, &run.shown[..], "{cut}");
            assert!(run.restored && run.blocking);
        }
    }

    #[test]
    fn a_daemon_gone_without_a_word_gets_cancel_and_the_latest_undo() {
        let run = against(Want::Requested(pane()), |f| {
            send(&f.s, accept());
            send(&f.s, undo(b"<undo>"));
            // as killed mid-write: half a sequence, the file left non-blocking
            unsafe {
                let fd = f.tty();
                libc::fcntl(
                    fd,
                    libc::F_SETFL,
                    libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
                );
                libc::write(fd, b"\x1b[38;2;".as_ptr().cast(), 7);
            }
        });
        assert_eq!(Exit::fail("the daemon went away; see /tmp/log"), run.exit);
        assert_eq!(b"\x1b[38;2;\x18\x1b\\<undo>", &run.shown[..]);
        assert!(run.restored && run.blocking);
    }

    /// The environment variable that makes `killable_daemon` run.
    fn killable_var() -> String {
        format!("{}_TEST_KILLABLE", crate::name::upper())
    }

    /// For `a_sigkilled_daemon_leaves_the_terminal_restored`, in a process
    /// of its own: a fake daemon on fd 3 that accepts, sends its undo bytes,
    /// leaves the terminal non-blocking mid-sequence, says `ready`, and
    /// waits to be killed.
    #[test]
    #[ignore = "a fake daemon process, run by another test"]
    fn killable_daemon() {
        if std::env::var_os(killable_var()).is_none() {
            return;
        }
        let s = unsafe { UnixStream::from_raw_fd(3) };
        let tty = take_hello(&s).1.expect("the terminal came");
        send(&s, accept());
        send(&s, undo(b"<undo>"));
        unsafe {
            let fd = tty.as_raw_fd();
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            );
            libc::write(fd, b"\x1b[38;2;".as_ptr().cast(), 7);
        }
        println!("ready");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn a_sigkilled_daemon_leaves_the_terminal_restored() {
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fd = theirs.as_raw_fd();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "daemon::pane_helper::tests::killable_daemon",
                "--ignored",
                "--nocapture",
            ])
            .env(killable_var(), "1")
            .stdout(Stdio::piped());
        // SAFETY: dup2 and fcntl are async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                if fd == 3 {
                    libc::fcntl(3, libc::F_SETFD, 0);
                } else if libc::dup2(fd, 3) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut daemon = command.spawn().unwrap();
        drop(theirs);
        let helper = std::thread::spawn(move || {
            let how = Attach::new(Want::Requested(pane()), Transport::Fd);
            attach_on(ours, slave, &how, None)
        });
        let stdout = daemon.stdout.take().unwrap();
        let mut ready = false;
        for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
            // the test harness may have begun the line
            if line.unwrap().contains("ready") {
                ready = true;
                break;
            }
        }
        daemon.kill().unwrap();
        daemon.wait().unwrap();
        assert!(ready, "the fake daemon never got ready");
        let exit = helper.join().unwrap();
        assert_eq!(Exit::fail("the daemon went away; see /tmp/log"), exit);
        assert!(raw::same(&before, &raw::settings(slave)));
        assert!(is_blocking(slave));
        assert_eq!(b"\x1b[38;2;\x18\x1b\\<undo>", &drain(master)[..]);
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn sigterm_hangs_up_undoes_and_restores_at_once() {
        let _one = crate::term::signals::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut watched = vec![libc::SIGWINCH];
        watched.extend(STOPPING);
        let mut signals = Signals::init(&watched).unwrap();
        let how = Attach {
            min_life: Duration::from_secs(30),
            ..Attach::new(Want::Requested(pane()), Transport::Fd)
        };
        let run = against_with(how, Some(&mut signals), |f| {
            send(&f.s, accept());
            send(&f.s, undo(b"<undo>"));
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
            // the helper hangs up
            let mut rest = Vec::new();
            (&f.s).read_to_end(&mut rest).unwrap();
        });
        signals.deinit();
        assert_eq!(Exit::signalled(), run.exit);
        assert_eq!(b"\x18\x1b\\<undo>", &run.shown[..]);
        assert!(run.restored && run.blocking);
        assert!(run.took < Duration::from_secs(30));
    }

    /// A signal while the answer is awaited: `a` and `u` came, unread, and
    /// the undo bytes are written all the same.
    #[test]
    fn a_signal_during_the_answer_wait_writes_the_undo_already_sent() {
        let _one = crate::term::signals::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut signals = Signals::init(&STOPPING).unwrap();
        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        let (ours, theirs) = UnixStream::pair().unwrap();
        // sent before the hello, and the signal before the wait: the wait
        // finds both, and the signal goes first
        send(&theirs, accept());
        send(&theirs, undo(b"<undo>"));
        unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        let sink = std::thread::spawn(move || {
            let _ = std::io::copy(&mut &theirs, &mut std::io::sink());
        });
        let how = Attach::new(Want::Requested(pane()), Transport::Fd);
        let exit = attach_on(ours, slave, &how, Some(&mut signals));
        signals.deinit();
        sink.join().unwrap();
        assert_eq!(Exit::signalled(), exit);
        assert_eq!(b"\x18\x1b\\<undo>", &drain(master)[..]);
        assert!(raw::same(&before, &raw::settings(slave)));
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn a_failure_waits_for_a_key_unless_a_shell_job_or_no_terminal() {
        let (master, slave) = open_pty(80, 24);
        let due = Instant::now() + Duration::from_millis(150);
        let typist = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(1, unsafe { libc::write(master, b"x".as_ptr().cast(), 1) });
            master
        });
        let mut out = Vec::new();
        let exit = hold_failure(Exit::fail("boom"), slave, false, due, &mut out, None);
        assert!(Instant::now() >= due + Duration::from_millis(100));
        let master = typist.join().unwrap();
        // shown, so not printed again
        assert_eq!((1, None), (exit.status, exit.message));
        let shown = format!("{NAME}: boom (press a key)\n");
        assert_eq!(shown.as_bytes(), &out[..]);

        // a shell's job: left to print its line, at once
        let mut out = Vec::new();
        let late = Instant::now() + Duration::from_secs(30);
        let exit = hold_failure(Exit::fail("boom"), slave, true, late, &mut out, None);
        assert_eq!(Exit::fail("boom"), exit);
        assert!(out.is_empty());
        // no terminal
        let mut fds = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        let exit = hold_failure(Exit::fail("boom"), fds[0], false, late, &mut out, None);
        assert_eq!(Exit::fail("boom"), exit);
        // a success
        assert_eq!(
            Exit::ok(),
            hold_failure(Exit::ok(), slave, false, late, &mut out, None)
        );
        assert!(out.is_empty());
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn the_helper_lives_300_ms_at_least() {
        let run = against(Want::Requested(pane()), |f| {
            send(&f.s, Message::End(End::refusal(Refusal::PaneTaken, "no")));
        });
        assert_eq!(Exit::fail("no"), run.exit);
        assert!(run.took >= MIN_LIFE, "{:?}", run.took);
        assert!(run.restored);
    }

    /// The reply to a query the daemon wrote, arriving after the attach
    /// ended, isn't left for the shell (SPI-970).
    #[test]
    fn a_late_reply_is_dropped_with_the_restore() {
        let how = Attach {
            min_life: Duration::from_secs(1),
            ..Attach::new(Want::SelfAttach, Transport::Fd)
        };
        let run = against_with(how, None, |f| {
            send(&f.s, accept());
            send(&f.s, end(Ending::Failed, "it broke", false));
            std::thread::sleep(Duration::from_millis(200));
            let reply = b"\x1b[?5u";
            assert_eq!(5, unsafe {
                libc::write(f.master, reply.as_ptr().cast(), 5)
            });
        });
        assert_eq!(Exit::fail("it broke"), run.exit);
        assert!(run.restored);
        // nothing is left to read, raw or not
        assert!(run.left.is_empty(), "{:?}", run.left);
    }

    #[test]
    fn only_a_shell_job_is_left_without_a_key_wait() {
        let ids = |pgid, sid| Ids { pgid, sid };
        // typed in an interactive shell: its own group, the shell's session
        assert!(is_shell_job(ids(200, 100), Some(ids(150, 100))));
        // `sh -c`, without job control: the parent's group
        assert!(!is_shell_job(ids(150, 100), Some(ids(150, 100))));
        // the pane's own command: its parent is in another session
        assert!(!is_shell_job(ids(200, 200), Some(ids(1, 1))));
        // its parent unknown
        assert!(!is_shell_job(ids(200, 100), None));
    }

    #[test]
    fn a_key_wait_ends_on_a_key_and_puts_the_settings_back() {
        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        let typist = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            assert_eq!(1, unsafe { libc::write(master, b"x".as_ptr().cast(), 1) });
        });
        let started = Instant::now();
        wait_for_key(slave, None);
        assert!(started.elapsed() >= Duration::from_millis(100));
        typist.join().unwrap();
        assert!(raw::same(&before, &raw::settings(slave)));
        // the key isn't left for the shell, nor echoed
        assert!(drain(master).is_empty());
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn a_pane_no_longer_wanted_exits_0_quietly() {
        for reason in [Refusal::PaneExpired, Refusal::Stopping] {
            let run = against(Want::Requested(pane()), move |f| {
                send(&f.s, Message::End(End::refusal(reason, "gone")));
            });
            assert_eq!(Exit::ok(), run.exit, "{reason:?}");
            assert!(run.restored);
        }
    }

    #[test]
    fn any_other_refusal_exits_1_with_its_message() {
        for reason in [Refusal::PaneTaken, Refusal::PaneUnknown, Refusal::Version] {
            let run = against(Want::Requested(pane()), move |f| {
                send(&f.s, Message::End(End::refusal(reason, "no thanks")));
            });
            assert_eq!(Exit::fail("no thanks"), run.exit, "{reason:?}");
        }
        // a launcher's attach refused as stopping isn't quiet
        let run = against(Want::SelfAttach, |f| {
            send(
                &f.s,
                Message::End(End::refusal(Refusal::Stopping, "stopping")),
            );
        });
        assert_eq!(1, run.exit.status);
    }

    #[test]
    fn a_failed_attach_exits_1_and_a_daemon_gone_names_its_log() {
        let run = against(Want::Requested(pane()), |f| {
            send(&f.s, accept());
            send(&f.s, end(Ending::Failed, "it broke", false));
        });
        assert_eq!(Exit::fail("it broke"), run.exit);
        let run = against(Want::Requested(pane()), |f| {
            send(&f.s, accept());
            drop(f.s);
        });
        assert_eq!(Exit::fail("the daemon went away; see /tmp/log"), run.exit);
        // the daemon wrote nothing before its answer: no cancel
        let run = against(Want::Requested(pane()), |f| drop(f.s));
        assert_eq!(Exit::fail("the daemon hung up"), run.exit);
        assert!(run.shown.is_empty() && run.restored);
    }

    #[test]
    fn quit_ends_a_self_attach_with_neovims_status() {
        let quit = |exit| {
            let mut z = End::ending(Ending::Quit, "quit", false);
            z.exit = exit;
            Message::End(z)
        };
        let run = against(Want::SelfAttach, move |f| {
            send(&f.s, accept());
            send(&f.s, quit(Some(3)));
        });
        assert_eq!(3, run.exit.status);
        let run = against(Want::Requested(pane()), move |f| {
            send(&f.s, accept());
            send(&f.s, quit(Some(3)));
        });
        assert_eq!(Exit::ok(), run.exit);
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
        let how = Attach {
            answer_within: Duration::from_millis(200),
            ..Attach::new(Want::Requested(pane()), Transport::Fd)
        };
        let exit = attach_on(ours, slave, &how, None);
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(Exit::fail("no answer from the daemon in 0.2 s"), exit);
        reader.join().unwrap();
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn a_daemon_of_another_user_is_left_before_the_hello() {
        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        let (ours, daemon_end) = UnixStream::pair().unwrap();
        let how = Attach {
            uid: unsafe { libc::geteuid() } + 1,
            ..Attach::new(Want::Requested(pane()), Transport::Fd)
        };
        let exit = attach_on(ours, slave, &how, None);
        assert_eq!(Exit::fail("the daemon runs as another user"), exit);
        // nothing was sent: the hello carries the environment
        let mut sent = Vec::new();
        (&daemon_end).read_to_end(&mut sent).unwrap();
        assert!(sent.is_empty(), "{sent:?}");
        // nor was the terminal touched
        assert!(raw::same(&before, &raw::settings(slave)));
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    /// Reads frames from `theirs` until a size.
    fn next_size(theirs: &UnixStream) -> Size {
        let mut buf = vec![0u8; 1 << 16];
        let mut got = Vec::new();
        loop {
            if let Some((f, n)) = Frame::decode(&got).unwrap() {
                got.drain(..n);
                if let Some(Message::Size(s)) = Message::decode(&f).unwrap() {
                    return s;
                }
                continue;
            }
            let r = fdpass::recv(theirs, &mut buf).unwrap();
            assert!(r.len > 0, "hung up");
            got.extend_from_slice(&buf[..r.len]);
        }
    }

    fn resize(master: RawFd, cols: u16, rows: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        assert_eq!(0, unsafe { libc::ioctl(master, libc::TIOCSWINSZ, &ws) });
    }

    /// A resize once attached: SIGWINCH, then the new size in an `s` frame.
    #[test]
    fn a_resize_signalled_once_attached_is_sent() {
        let _one = crate::term::signals::TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut signals = Signals::init(&[libc::SIGWINCH]).unwrap();
        let how = Attach::new(Want::Requested(pane()), Transport::Fd);
        let run = against_with(how, Some(&mut signals), |f| {
            send(&f.s, accept());
            // the helper waits on its socket and the signal by now
            std::thread::sleep(Duration::from_millis(200));
            resize(f.master, 120, 40);
            unsafe { libc::raise(libc::SIGWINCH) };
            let s = next_size(&f.s);
            assert_eq!((120, 40), (s.cols, s.rows));
            send(&f.s, end(Ending::Closed, "", false));
        });
        signals.deinit();
        assert_eq!(Exit::ok(), run.exit);
    }

    #[test]
    fn unknown_frames_are_skipped() {
        let run = against(Want::Requested(pane()), |mut f| {
            let mut odd = Vec::new();
            Frame::encode_into(b'q', b"??", &mut odd).unwrap();
            f.s.write_all(&odd).unwrap();
            send(&f.s, accept());
            f.s.write_all(&odd).unwrap();
            send(&f.s, end(Ending::Stopped, "stopped", false));
        });
        assert_eq!(Exit::ok(), run.exit);
    }

    #[test]
    fn a_resize_since_the_hello_is_sent() {
        let run = against(Want::Requested(pane()), |f| {
            // resized before the answer
            resize(f.master, 100, 30);
            send(&f.s, accept());
            let s = next_size(&f.s);
            assert_eq!((100, 30), (s.cols, s.rows));
            send(&f.s, end(Ending::Closed, "", false));
        });
        assert_eq!(Exit::ok(), run.exit);
    }

    fn relayed(want: Want, daemon: impl FnOnce(Fake) + Send + 'static) -> Run {
        against_with(Attach::new(want, Transport::Relay), None, daemon)
    }

    /// What the helper relays in `i` frames on `theirs`, until `n` bytes.
    fn next_input(theirs: &UnixStream, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; 1 << 16];
        let mut got = Vec::new();
        let mut typed = Vec::new();
        while typed.len() < n {
            if let Some((f, used)) = Frame::decode(&got).unwrap() {
                got.drain(..used);
                if let Some(Message::Input(i)) = Message::decode(&f).unwrap() {
                    typed.extend_from_slice(&i);
                }
                continue;
            }
            let r = fdpass::recv(theirs, &mut buf).unwrap();
            assert!(r.len > 0, "hung up");
            got.extend_from_slice(&buf[..r.len]);
        }
        typed
    }

    #[test]
    fn relayed_typing_goes_in_i_frames_and_output_comes_in_o_frames() {
        let run = relayed(Want::Requested(pane()), |f| {
            assert!(f.tty.is_none(), "no terminal is handed over");
            let Purpose::Requested { terminal, .. } = &f.hello.purpose else {
                panic!("{:?}", f.hello)
            };
            assert_eq!(Transport::Relay, terminal.transport);
            // typed before the answer, and after
            assert_eq!(1, unsafe { libc::write(f.master, b"a".as_ptr().cast(), 1) });
            send(&f.s, accept());
            send(&f.s, undo(b"<undo>"));
            assert_eq!(2, unsafe {
                libc::write(f.master, b"bc".as_ptr().cast(), 2)
            });
            assert_eq!(b"abc".to_vec(), next_input(&f.s, 3));
            send(&f.s, Message::Output(b"drawn".to_vec()));
            send(&f.s, Message::Output(b" more".to_vec()));
            send(&f.s, end(Ending::Closed, "", false));
        });
        assert_eq!(Exit::ok(), run.exit);
        // the output, then the undo bytes; nothing echoed
        assert_eq!(b"drawn more<undo>", &run.shown[..]);
        assert!(run.restored && run.blocking);
    }

    #[test]
    fn a_relaying_daemon_gone_has_its_last_output_shown_before_cancel() {
        let run = relayed(Want::Requested(pane()), |f| {
            send(&f.s, accept());
            send(&f.s, undo(b"<undo>"));
            send(&f.s, Message::Output(b"\x1b[38;2;".to_vec()));
            // and half an `o`
            (&f.s).write_all(b"o\0\0\0\x09\x1b[").unwrap();
        });
        assert_eq!(Exit::fail("the daemon went away; see /tmp/log"), run.exit);
        assert_eq!(b"\x1b[38;2;\x18\x1b\\<undo>", &run.shown[..]);
        assert!(run.restored && run.blocking);
    }

    /// A relayed attach whose terminal hangs up ends quietly, still
    /// living out its minimum life and hanging up on the daemon.
    #[test]
    fn a_relayed_terminal_that_hangs_up_ends_the_attach_quietly() {
        let (master, slave) = open_pty(80, 24);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            take_hello(&theirs);
            send(&theirs, accept());
            send(&theirs, undo(b"<undo>"));
            // the terminal emulator goes
            unsafe { libc::close(master) };
            let mut rest = Vec::new();
            (&theirs).read_to_end(&mut rest).unwrap();
        });
        let how = Attach::new(Want::Requested(pane()), Transport::Relay);
        let exit = attach_on(ours, slave, &how, None);
        fake.join().unwrap();
        assert_eq!(Exit::ok(), exit);
        assert!(how.started.elapsed() >= MIN_LIFE);
        unsafe { libc::close(slave) };
    }

    /// A relayed terminal that can't be read is a failure, and still
    /// undone and restored.
    #[test]
    fn a_relayed_terminal_that_cant_be_read_fails_and_is_restored() {
        let (master, slave) = open_pty(80, 24);
        let before = raw::settings(slave);
        // the same terminal, open for writing only: its reads fail
        let path = std::ffi::CString::new(tty_name(slave)).unwrap();
        let tty = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NOCTTY) };
        assert!(tty >= 0);
        let (ours, theirs) = UnixStream::pair().unwrap();
        let fake = std::thread::spawn(move || {
            take_hello(&theirs);
            send(&theirs, accept());
            send(&theirs, undo(b"<undo>"));
            // typed: the terminal has input to read
            assert_eq!(1, unsafe { libc::write(master, b"x".as_ptr().cast(), 1) });
            let mut rest = Vec::new();
            (&theirs).read_to_end(&mut rest).unwrap();
        });
        let how = Attach::new(Want::Requested(pane()), Transport::Relay);
        let exit = attach_on(ours, tty, &how, None);
        fake.join().unwrap();
        assert_eq!(1, exit.status);
        let message = exit.message.unwrap_or_default();
        assert!(
            message.starts_with("can't read the terminal: "),
            "{message}"
        );
        assert_eq!(b"<undo>", &drain(master)[..]);
        assert!(raw::same(&before, &raw::settings(slave)));
        unsafe {
            libc::close(tty);
            libc::close(master);
            libc::close(slave);
        }
    }
}
