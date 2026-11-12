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

use super::layout::MAX_REPORT;
use crate::daemon::event_loop::Loop;

/// The longest terminal id (#93 decision 19).
pub(crate) const MAX_ID: usize = 4 << 10;
/// The most of a run's stderr that is kept, for the log.
pub(crate) const MAX_STDERR: usize = 64 << 10;
/// The most of stdout's first line kept: past it, the id is too long
/// whatever its spaces. Also the longest `events` line (#93 decision 19).
pub(crate) const MAX_LINE: usize = 64 << 10;

/// What becomes of a command's stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stdout {
    /// `/dev/null`.
    Null,
    /// Its first line is kept, up to `MAX_LINE`: the opens'.
    FirstLine,
    /// All of it is kept, up to `MAX_REPORT`: `report_layout`'s.
    Whole,
    /// Each line is taken as it comes (`take_lines`); longer ones than
    /// `MAX_LINE` are dropped: `events`'.
    Lines,
}

/// What is kept of stdout so far, as its `Stdout` says. A part that went
/// past its limit is None.
enum Kept {
    Null,
    /// The first line so far, and whether it ended.
    FirstLine {
        line: Option<Vec<u8>>,
        ended: bool,
    },
    /// All of it so far.
    Whole(Option<Vec<u8>>),
    /// The line being read; the lines ended and not yet taken; and how many
    /// were dropped as too long, not yet told.
    Lines {
        line: Option<Vec<u8>>,
        ended: Vec<Vec<u8>>,
        dropped: usize,
    },
}

/// What a command is run with.
pub(crate) struct Start<'a> {
    pub argv: &'a [Vec<u8>],
    pub env: &'a [(Vec<u8>, Vec<u8>)],
    pub cwd: &'a Path,
    pub stdout: Stdout,
}

/// A command running, or ended and not yet finished with.
pub(crate) struct Run {
    child: Child,
    stdout: Option<PipeReader>,
    stderr: Option<PipeReader>,
    kept: Kept,
    /// Stderr's first `MAX_STDERR` bytes.
    stderr_kept: Vec<u8>,
    stderr_cut: bool,
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ended {
    pub status: ExitStatus,
    /// Stdout's first line, untrimmed, or all of it, as `Stdout` says; None
    /// if it was too long to keep.
    pub out: Option<Vec<u8>>,
    pub stderr: Vec<u8>,
    /// Whether stderr went past `MAX_STDERR`.
    pub stderr_cut: bool,
}

impl Run {
    /// Starts `start`, watching its pipes and its exit on `event_loop` under
    /// `token`. Fails if it can't start.
    pub(crate) fn start(start: &Start<'_>, event_loop: &Loop, token: usize) -> io::Result<Run> {
        let (program, args) = start
            .argv
            .split_first()
            .ok_or_else(|| io::Error::other("an empty argv"))?;
        let (stderr_r, stderr_w) = io::pipe()?;
        let (stdout_r, stdout) = if start.stdout == Stdout::Null {
            (None, Stdio::null())
        } else {
            let (r, w) = io::pipe()?;
            (Some(r), Stdio::from(w))
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
        // SAFETY: `default_signals` is async-signal-safe and allocates nothing.
        unsafe {
            command.pre_exec(|| crate::util::default_signals());
        }
        // the daemon's own fds are all close-on-exec: only stdio goes along
        let child = command.spawn()?;
        drop(command);
        for r in stdout_r.iter().chain([&stderr_r]) {
            set_nonblocking(r.as_raw_fd());
            let _ = event_loop.add_read(r.as_raw_fd(), token);
        }
        // one that ended before it was watched is found by `ended`
        let _ = event_loop.add_exit(child.id() as libc::pid_t, token);
        Ok(Run {
            child,
            stdout: stdout_r,
            stderr: Some(stderr_r),
            kept: match start.stdout {
                Stdout::Null => Kept::Null,
                Stdout::FirstLine => Kept::FirstLine {
                    line: Some(Vec::new()),
                    ended: false,
                },
                Stdout::Whole => Kept::Whole(Some(Vec::new())),
                Stdout::Lines => Kept::Lines {
                    line: Some(Vec::new()),
                    ended: Vec::new(),
                    dropped: 0,
                },
            },
            stderr_kept: Vec::new(),
            stderr_cut: false,
        })
    }

    /// Its process group's id: its own pid.
    pub(crate) fn group(&self) -> libc::pid_t {
        self.child.id() as libc::pid_t
    }

    /// Reads what its pipes have now. A pipe that ended leaves `event_loop`.
    pub(crate) fn read(&mut self, event_loop: &Loop) {
        let mut stdout = self.stdout.take();
        let open = stdout.is_some();
        drain(&mut stdout, event_loop, |bytes| self.keep(bytes));
        self.stdout = stdout;
        if open && self.stdout.is_none() {
            self.stdout_ended();
        }
        let Run {
            stderr,
            stderr_kept,
            stderr_cut,
            ..
        } = self;
        drain(stderr, event_loop, |bytes| {
            let room = MAX_STDERR - stderr_kept.len();
            if bytes.len() > room {
                *stderr_cut = true;
            }
            stderr_kept.extend_from_slice(&bytes[..bytes.len().min(room)]);
        });
    }

    /// Keeps what `bytes`, read from stdout, add, as its mode says.
    fn keep(&mut self, bytes: &[u8]) {
        match &mut self.kept {
            Kept::Null => {}
            Kept::FirstLine { line, ended } => {
                if *ended {
                    return;
                }
                let (part, end) = match bytes.iter().position(|&b| b == b'\n') {
                    Some(i) => (&bytes[..i], true),
                    None => (bytes, false),
                };
                append(line, part, MAX_LINE);
                *ended = end;
            }
            Kept::Whole(all) => append(all, bytes, MAX_REPORT),
            Kept::Lines {
                line,
                ended,
                dropped,
            } => {
                let mut rest = bytes;
                while let Some(i) = rest.iter().position(|&b| b == b'\n') {
                    append(line, &rest[..i], MAX_LINE);
                    end_line(line, ended, dropped);
                    rest = &rest[i + 1..];
                }
                append(line, rest, MAX_LINE);
            }
        }
    }

    /// Stdout closed: a last line with no end is a line all the same.
    fn stdout_ended(&mut self) {
        if let Kept::Lines {
            line,
            ended,
            dropped,
        } = &mut self.kept
            && line.as_ref().is_none_or(|l| !l.is_empty())
        {
            end_line(line, ended, dropped);
        }
    }

    /// The lines read since last taken, and how many were dropped as too
    /// long: `Stdout::Lines`'.
    pub(crate) fn take_lines(&mut self) -> (Vec<Vec<u8>>, usize) {
        match &mut self.kept {
            Kept::Lines { ended, dropped, .. } => (std::mem::take(ended), std::mem::take(dropped)),
            _ => (Vec::new(), 0),
        }
    }

    /// How it ended, if it has: what its pipes held by then is read, and
    /// they close, though something it started may still hold them.
    pub(crate) fn ended(&mut self, event_loop: &Loop) -> Option<Ended> {
        let status = self.child.try_wait().ok()??;
        self.read(event_loop);
        if self.stdout.is_some() {
            self.stdout_ended();
        }
        for pipe in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = event_loop.remove(pipe.as_raw_fd());
        }
        Some(Ended {
            status,
            out: match &mut self.kept {
                Kept::FirstLine { line, .. } => line.take(),
                Kept::Whole(all) => all.take(),
                Kept::Null | Kept::Lines { .. } => None,
            },
            stderr: std::mem::take(&mut self.stderr_kept),
            stderr_cut: self.stderr_cut,
        })
    }

    /// Ends it at once, with its group, and reaps it: the daemon is going.
    pub(crate) fn kill(mut self, event_loop: &Loop) {
        signal_group(self.group(), libc::SIGKILL);
        let _ = self.child.wait();
        for pipe in [self.stdout.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            let _ = event_loop.remove(pipe.as_raw_fd());
        }
    }
}

/// Adds `bytes` to `kept`, unless that takes it past `max`, which drops it.
fn append(kept: &mut Option<Vec<u8>>, bytes: &[u8], max: usize) {
    if kept.as_ref().is_some_and(|k| k.len() + bytes.len() > max) {
        *kept = None;
    }
    if let Some(k) = kept {
        k.extend_from_slice(bytes);
    }
}

/// The line being read ended: it is taken, or counted as dropped if too
/// long; the next starts empty.
fn end_line(line: &mut Option<Vec<u8>>, ended: &mut Vec<Vec<u8>>, dropped: &mut usize) {
    match line.replace(Vec::new()) {
        Some(l) => ended.push(l),
        None => *dropped += 1,
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
/// `event_loop` and drops it.
fn drain(pipe: &mut Option<PipeReader>, event_loop: &Loop, mut to: impl FnMut(&[u8])) {
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
        let _ = event_loop.remove(r.as_raw_fd());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_is_found_ended_without_its_exit_event() {
        let event_loop = Loop::init().unwrap();
        let argv = [b"sh".to_vec(), b"-c".to_vec(), b"echo id; exit 3".to_vec()];
        let env = [(b"PATH".to_vec(), b"/usr/bin:/bin".to_vec())];
        let start = Start {
            argv: &argv,
            env: &env,
            cwd: Path::new("/"),
            stdout: Stdout::FirstLine,
        };
        let mut run = Run::start(&start, &event_loop, 1).unwrap();
        // the loop is never waited on: `ended` reaps it by itself
        let started = std::time::Instant::now();
        let ended = loop {
            if let Some(ended) = run.ended(&event_loop) {
                break ended;
            }
            assert!(started.elapsed() < std::time::Duration::from_secs(5));
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(Some(3), ended.status.code());
        assert_eq!(Some(b"id".to_vec()), ended.out);
    }
}
