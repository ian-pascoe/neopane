//! One run of a capability's command: started without a shell in its own
//! process group, its pipes read as they fill, never waited on (#86
//! decision D2).

use std::ffi::OsStr;
use std::io::{self, PipeReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};

use crate::daemon::event_loop::Loop;

/// The longest terminal id (#93 decision 19).
pub(crate) const MAX_ID: usize = 4 << 10;
/// The most of a run's stderr that is kept, for the log.
pub(crate) const MAX_STDERR: usize = 64 << 10;
/// The most of stdout's first line kept: past it, the id is too long
/// whatever its spaces.
const MAX_LINE: usize = 64 << 10;

/// What a command is run with.
pub(crate) struct Start<'a> {
    pub argv: &'a [Vec<u8>],
    pub env: &'a [(Vec<u8>, Vec<u8>)],
    pub cwd: &'a Path,
    /// Whether its stdout's first line is read: the opens'.
    pub reads_id: bool,
}

/// A command running, or ended and not yet finished with.
pub(crate) struct Run {
    child: Child,
    stdout: Option<PipeReader>,
    stderr: Option<PipeReader>,
    /// Stdout's first line so far, up to `MAX_LINE`.
    line: Vec<u8>,
    line_ended: bool,
    line_over: bool,
    /// Stderr's first `MAX_STDERR` bytes.
    stderr_kept: Vec<u8>,
    stderr_cut: bool,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub status: ExitStatus,
    /// Stdout's first line, untrimmed, or None if it was too long to keep.
    pub line: Option<Vec<u8>>,
    pub stderr: Vec<u8>,
    /// Whether stderr went past `MAX_STDERR`.
    pub stderr_cut: bool,
}

impl Run {
    /// Starts `start`, watching its pipes and its exit on `lp` under
    /// `token`. Fails if it can't start.
    pub(crate) fn start(start: &Start<'_>, lp: &Loop, token: usize) -> io::Result<Run> {
        let (program, args) = start
            .argv
            .split_first()
            .ok_or_else(|| io::Error::other("an empty argv"))?;
        let (stderr_r, stderr_w) = io::pipe()?;
        let (stdout_r, stdout) = if start.reads_id {
            let (r, w) = io::pipe()?;
            (Some(r), Stdio::from(w))
        } else {
            (None, Stdio::null())
        };
        let mut command = Command::new(OsStr::from_bytes(program));
        command
            .args(args.iter().map(|a| OsStr::from_bytes(a)))
            .env_clear()
            .envs(
                start
                    .env
                    .iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            )
            .current_dir(start.cwd)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr_w)
            .process_group(0);
        // the daemon's own fds are all close-on-exec: only stdio goes along
        let child = command.spawn()?;
        drop(command);
        for r in stdout_r.iter().chain([&stderr_r]) {
            set_nonblocking(r.as_raw_fd());
            let _ = lp.add_read(r.as_raw_fd(), token);
        }
        // one that ended before it was watched is found by `ended`
        let _ = lp.add_exit(child.id() as libc::pid_t, token);
        Ok(Run {
            child,
            stdout: stdout_r,
            stderr: Some(stderr_r),
            line: Vec::new(),
            line_ended: false,
            line_over: false,
            stderr_kept: Vec::new(),
            stderr_cut: false,
        })
    }

    /// Its process group's id: its own pid.
    pub(crate) fn group(&self) -> libc::pid_t {
        self.child.id() as libc::pid_t
    }

    /// Reads what its pipes have now. A pipe that ended leaves `lp`.
    pub(crate) fn read(&mut self, lp: &Loop) {
        let Run {
            stdout,
            stderr,
            line,
            line_ended,
            line_over,
            stderr_kept,
            stderr_cut,
            ..
        } = self;
        drain(stdout, lp, |bytes| {
            if *line_ended || *line_over {
                return;
            }
            let (part, ended) = match bytes.iter().position(|&b| b == b'\n') {
                Some(i) => (&bytes[..i], true),
                None => (bytes, false),
            };
            line.extend_from_slice(part);
            *line_ended = ended;
            if line.len() > MAX_LINE {
                *line_over = true;
                line.clear();
            }
        });
        drain(stderr, lp, |bytes| {
            let room = MAX_STDERR - stderr_kept.len();
            if bytes.len() > room {
                *stderr_cut = true;
            }
            stderr_kept.extend_from_slice(&bytes[..bytes.len().min(room)]);
        });
    }

    /// How it ended, if it has: what its pipes held by then is read, and
    /// they close, though something it started may still hold them.
    pub(crate) fn ended(&mut self, lp: &Loop) -> Option<Ended> {
        let status = self.child.try_wait().ok()??;
        self.read(lp);
        for pipe in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = lp.remove(pipe.as_raw_fd());
        }
        Some(Ended {
            status,
            line: (!self.line_over).then(|| std::mem::take(&mut self.line)),
            stderr: std::mem::take(&mut self.stderr_kept),
            stderr_cut: self.stderr_cut,
        })
    }

    /// Ends it at once, with its group, and reaps it: the daemon is going.
    pub(crate) fn kill(mut self, lp: &Loop) {
        signal_group(self.group(), libc::SIGKILL);
        let _ = self.child.wait();
        for pipe in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = lp.remove(pipe.as_raw_fd());
        }
    }
}

/// Signals process group `group`.
pub(crate) fn signal_group(group: libc::pid_t, sig: libc::c_int) {
    // SAFETY: a plain syscall; a group that has gone is ESRCH, ignored.
    unsafe { libc::kill(-group, sig) };
}

/// Why an exit status isn't 0, for the log and the warning.
pub(crate) fn status_text(status: ExitStatus) -> String {
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit {code}"),
        (None, Some(sig)) => format!("killed by signal {sig}"),
        (None, None) => format!("{status}"),
    }
}

/// Reads what `pipe` has now into `to`; once it has ended, takes it off
/// `lp` and drops it.
fn drain(pipe: &mut Option<PipeReader>, lp: &Loop, mut to: impl FnMut(&[u8])) {
    let Some(r) = pipe else {
        return;
    };
    let mut buf = [0u8; 16 << 10];
    let open = loop {
        match r.read(&mut buf) {
            Ok(0) => break false,
            Ok(n) => to(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break true,
            Err(_) => break false,
        }
    };
    if !open {
        let _ = lp.remove(r.as_raw_fd());
        *pipe = None;
    }
}

fn set_nonblocking(fd: std::os::fd::RawFd) {
    // SAFETY: fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}
