//! `neorex pane SOCKET [ID]`: the helper each Rex pane runs. It hands its
//! terminal to the daemon, then waits. When the daemon hangs up it exits,
//! and Rex closes the pane. The ID says which window the daemon made the
//! pane for; the launcher's own pane has none.

use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::time::Duration;

use super::fdpass;
use crate::term::signals::Signals;

/// What can go wrong in the helper or in the daemon's reading of its hello.
#[derive(Debug)]
pub enum Error {
    NotATerminal,
    /// The hello came without a terminal fd.
    NoTerminal,
    BadHello,
    /// `hello_bytes`'s buffer is too small.
    NoSpaceLeft,
    /// The peer runs as another user.
    WrongUser,
    Io(io::Error),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotATerminal => write!(f, "NotATerminal"),
            Error::NoTerminal => write!(f, "NoTerminal"),
            Error::BadHello => write!(f, "BadHello"),
            Error::NoSpaceLeft => write!(f, "NoSpaceLeft"),
            Error::WrongUser => write!(f, "WrongUser"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// What a helper sends the daemon, one byte each. Any other byte is
/// `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Message {
    /// First, with the terminal's fd attached, then the ID in decimal if any.
    Hello,
    /// The terminal was resized. Only the helper gets SIGWINCH: it's in
    /// the terminal's foreground process group, the daemon isn't.
    Resized,
    Other(u8),
}

impl Message {
    pub(crate) fn from_byte(byte: u8) -> Message {
        match byte {
            b'p' => Message::Hello,
            b'w' => Message::Resized,
            other => Message::Other(other),
        }
    }

    /// The byte sent for this message.
    pub(crate) fn to_byte(self) -> u8 {
        match self {
            Message::Hello => b'p',
            Message::Resized => b'w',
            Message::Other(byte) => byte,
        }
    }
}

pub fn run(socket_path: impl AsRef<Path>, id: Option<u32>) -> Result<()> {
    let tty = libc::STDIN_FILENO;
    // SAFETY: `isatty` only inspects the fd.
    if unsafe { libc::isatty(tty) } == 0 {
        return Err(Error::NotATerminal);
    }

    // before the hello, so a resize in between isn't missed
    let mut signals = Signals::init(&[libc::SIGWINCH])?;

    let mut stream = UnixStream::connect(socket_path)?;
    let mut hello_buf = [0u8; 16];
    fdpass::send(&stream, hello_bytes(&mut hello_buf, id)?, tty)?;

    loop {
        let mut fds = [
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: signals.read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of two pollfds.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) } < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }
        if fds[1].revents != 0 && signals.drain()?.contains(libc::SIGWINCH) {
            stream.write_all(&[Message::Resized.to_byte()])?;
            stream.flush()?;
        }
        if fds[0].revents != 0 {
            let mut input = [0u8; 64];
            // nothing is sent this way yet; hang-up is the only message
            let n = loop {
                match stream.read(&mut input) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break 0,
                    Ok(n) => break n,
                }
            };
            if n == 0 {
                return Ok(());
            }
        }
    }
}

/// The hello message: `p`, then the ID if there is one.
pub(crate) fn hello_bytes(buf: &mut [u8], id: Option<u32>) -> Result<&[u8]> {
    let hello = Message::Hello.to_byte();
    let Some(n) = id else {
        *buf.first_mut().ok_or(Error::NoSpaceLeft)? = hello;
        return Ok(&buf[..1]);
    };
    let text = format!("{}{n}", hello as char);
    let out = buf.get_mut(..text.len()).ok_or(Error::NoSpaceLeft)?;
    out.copy_from_slice(text.as_bytes());
    Ok(out)
}

/// A helper's connection and the terminal it handed over.
#[derive(Debug)]
pub(crate) struct Hello {
    pub(crate) stream: UnixStream,
    pub(crate) tty: RawFd,
    /// None for the launcher's pane.
    pub(crate) id: Option<u32>,
}

impl Hello {
    pub(crate) fn close(self) {
        // SAFETY: `tty` is the fd this hello owns; the stream closes on drop.
        unsafe { libc::close(self.tty) };
    }
}

/// How long a connected peer has to send its hello.
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// The user id of the process on the other end of `stream`.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
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

/// Only the daemon's own user may hand it a terminal.
fn check_peer(peer: u32, ours: u32) -> Result<()> {
    if peer == ours {
        Ok(())
    } else {
        Err(Error::WrongUser)
    }
}

/// The daemon's side: takes the next helper and its hello. Closes both on
/// failure. A peer of another user, or one that sends no hello within
/// `HELLO_TIMEOUT`, is dropped.
pub(crate) fn accept_hello(server: &UnixListener) -> Result<Hello> {
    let (stream, _) = server.accept()?;
    accept_hello_on(stream, HELLO_TIMEOUT)
}

fn accept_hello_on(stream: UnixStream, timeout: Duration) -> Result<Hello> {
    // SAFETY: `geteuid` has no preconditions.
    check_peer(peer_uid(&stream)?, unsafe { libc::geteuid() })?;
    stream.set_read_timeout(Some(timeout))?;
    let mut buf = [0u8; 16];
    let r = fdpass::recv(&stream, &mut buf)?;
    stream.set_read_timeout(None)?;
    let tty = r.fd.ok_or(Error::NoTerminal)?;
    match parse_hello(&buf[..r.len]) {
        Ok(id) => Ok(Hello { stream, tty, id }),
        Err(e) => {
            // SAFETY: `tty` was passed to us and nothing else holds it.
            unsafe { libc::close(tty) };
            Err(e)
        }
    }
}

/// The ID in a hello: None for the launcher's pane.
pub(crate) fn parse_hello(bytes: &[u8]) -> Result<Option<u32>> {
    if bytes.first() != Some(&Message::Hello.to_byte()) {
        return Err(Error::BadHello);
    }
    if bytes.len() == 1 {
        return Ok(None);
    }
    std::str::from_utf8(&bytes[1..])
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .map(Some)
        .ok_or(Error::BadHello)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_carries_the_id() {
        let mut buf = [0u8; 16];
        assert!(matches!(
            parse_hello(hello_bytes(&mut buf, None).unwrap()),
            Ok(None)
        ));
        assert!(matches!(
            parse_hello(hello_bytes(&mut buf, Some(42)).unwrap()),
            Ok(Some(42))
        ));
        assert!(matches!(parse_hello(b"w"), Err(Error::BadHello)));
        assert!(matches!(parse_hello(b"px"), Err(Error::BadHello)));
    }

    #[test]
    fn only_our_own_user_is_let_in() {
        assert!(check_peer(501, 501).is_ok());
        assert!(matches!(check_peer(0, 501), Err(Error::WrongUser)));
    }

    #[test]
    fn a_peer_is_our_user() {
        let (a, _b) = UnixStream::pair().unwrap();
        assert_eq!(unsafe { libc::geteuid() }, peer_uid(&a).unwrap());
    }

    #[test]
    fn a_silent_peer_times_out() {
        let (ours, _silent) = UnixStream::pair().unwrap();
        let started = std::time::Instant::now();
        let r = accept_hello_on(ours, Duration::from_millis(100));
        assert!(matches!(r, Err(Error::Io(_))));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    /// A hello over a real socket, with a terminal fd.
    #[test]
    fn accept_hello_takes_the_terminal_and_the_id() {
        let path = std::env::temp_dir().join(format!("neorex-ph-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let server = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).unwrap();
        let mut pipe: [RawFd; 2] = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(pipe.as_mut_ptr()) });
        let mut buf = [0u8; 16];
        fdpass::send(&client, hello_bytes(&mut buf, Some(7)).unwrap(), pipe[1]).unwrap();

        let hello = accept_hello(&server).unwrap();
        assert_eq!(Some(7), hello.id);
        assert!(hello.tty >= 0);
        hello.close();
        unsafe {
            libc::close(pipe[0]);
            libc::close(pipe[1]);
        }
        let _ = std::fs::remove_file(&path);
    }
}
