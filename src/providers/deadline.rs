//! Waiting with a deadline, for the built-in commands: on a socket, until
//! it has something to read, and on a command, until it exits or is
//! killed. Neither names a pane provider.

use std::io::{ErrorKind, Read};
use std::os::fd::AsRawFd;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How often a command's exit is polled for.
const POLL: Duration = Duration::from_millis(2);

/// Whether `conn` has something to read, or has ended, before `due`. It
/// waits with `poll`, and sets no socket option: a server that closes the
/// connection once it has replied leaves macOS refusing any (`EINVAL`).
pub(crate) fn readable_by(conn: &impl AsRawFd, due: Instant) -> std::io::Result<bool> {
    loop {
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let mut fds = [libc::pollfd {
            fd: conn.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // rounded up, so it never wakes early and spins
        let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as libc::c_int;
        // SAFETY: one valid pollfd, for the call only.
        match unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) } {
            n if n > 0 => return Ok(true),
            0 => {}
            _ => {
                let e = std::io::Error::last_os_error();
                if e.kind() != ErrorKind::Interrupted {
                    return Err(e);
                }
            }
        }
    }
}

/// What a command that ran to its end gave.
#[derive(Debug)]
pub(crate) struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Why a command gave no [`Output`].
#[derive(Debug)]
pub(crate) enum Unfinished {
    /// It couldn't start.
    CantStart(std::io::Error),
    /// It didn't exit by its deadline, and was killed.
    TimedOut,
}

/// Runs `cmd` with stdin `/dev/null`, reading its stdout and stderr: its
/// output, or killed if it hasn't exited by `due`, if given.
pub(crate) fn output_by(mut cmd: Command, due: Option<Instant>) -> Result<Output, Unfinished> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(Unfinished::CantStart)?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if due.is_none_or(|due| Instant::now() < due) => std::thread::sleep(POLL),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Unfinished::TimedOut);
            }
        }
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn a_command_gives_its_output_or_is_killed_at_its_deadline() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let out = output_by(cmd, None).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out\n");
        assert_eq!(out.stderr, b"err\n");

        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("5");
        let started = Instant::now();
        let ran = output_by(cmd, Some(Instant::now() + Duration::from_millis(100)));
        assert!(matches!(ran, Err(Unfinished::TimedOut)), "{ran:?}");
        assert!(started.elapsed() < Duration::from_secs(2));

        let ran = output_by(Command::new("/nowhere/x"), None);
        assert!(matches!(ran, Err(Unfinished::CantStart(_))), "{ran:?}");
    }

    #[test]
    fn a_socket_is_readable_once_written_or_closed_else_not_by_its_deadline() {
        let soon = || Instant::now() + Duration::from_millis(50);
        let (ours, theirs) = UnixStream::pair().unwrap();
        assert!(!readable_by(&ours, soon()).unwrap());
        std::io::Write::write_all(&mut &theirs, b"x").unwrap();
        assert!(readable_by(&ours, soon()).unwrap());
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        assert!(readable_by(&ours, soon()).unwrap());
    }
}
