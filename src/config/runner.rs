//! Runs the config child and waits for its answer: the blocking runner
//! (#93 decision 3). The child runs in its own process group, killed once
//! it has answered or its time is up, so nothing it started outlives the
//! choice.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::api::seconds;
use super::child::{ANSWER_FD, REQUEST_FD};
use super::wire::{Answer, MAX_ANSWER, Request};
use crate::log::log_info;

/// The most output the config's run logs: 64 KiB.
pub const MAX_PRINTED: usize = 64 << 10;

/// How to start the child.
#[derive(Debug, Clone, Copy)]
pub struct Spawn<'a> {
    /// The daemon's own executable path.
    pub exe: &'a Path,
    /// The commands' environment, exactly.
    pub env: &'a [(OsString, OsString)],
}

/// What running the config came to.
#[derive(Debug)]
pub struct Outcome {
    /// What the config printed, line by line, without the `config: `
    /// prefix the log gives them; at most [`MAX_PRINTED`] bytes.
    pub printed: Vec<String>,
    /// Whether output past [`MAX_PRINTED`] was dropped.
    pub printed_cut: bool,
    pub answer: Result<Answer, Failure>,
    /// How long it took, to the answer.
    pub took: Duration,
}

impl Outcome {
    /// Logs what the config printed, line by line, as `config: <line>`.
    pub fn log(&self) {
        for line in &self.printed {
            log_info!("config: {line}");
        }
        if self.printed_cut {
            log_info!("config: (output past {} KiB dropped)", MAX_PRINTED >> 10);
        }
    }
}

/// Why the child gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// It couldn't be started.
    CantStart(String),
    /// It exited first.
    Exited(i32),
    /// A signal ended it first.
    Killed(i32),
    /// Its time ran out.
    TimedOut(Duration),
    /// What it sent isn't an answer.
    BadAnswer(String),
}

impl Failure {
    /// The message, for the file named `file`, such as `init.lua`.
    pub fn message(&self, file: &str) -> String {
        match self {
            Failure::CantStart(e) => format!("can't run {file}: {e}"),
            Failure::Exited(code) => {
                format!("{file} ended before answering (exit status {code}; did it call os.exit?)")
            }
            Failure::Killed(sig) => format!("{file} was killed by signal {sig} before answering"),
            Failure::TimedOut(t) => format!("{file} didn't answer within {} s", seconds(*t)),
            Failure::BadAnswer(e) => format!("{file} answered in a way this build can't read: {e}"),
        }
    }
}

/// Runs the config `request` names in a child started as `spawn` says, and
/// waits for its answer, at most `request.time`.
pub fn run(spawn: Spawn<'_>, request: &Request) -> Outcome {
    let started = Instant::now();
    let deadline = started + request.time;
    let mut printed = Printed::default();
    let answer = match start(spawn, request) {
        Ok(running) => running.wait(deadline, request.time, &mut printed),
        Err(e) => Err(Failure::CantStart(e.to_string())),
    };
    Outcome {
        printed: printed.lines,
        printed_cut: printed.dropped,
        answer,
        took: started.elapsed(),
    }
}

struct Running {
    child: Child,
    answer: io::PipeReader,
    output: io::PipeReader,
}

fn start(spawn: Spawn<'_>, request: &Request) -> io::Result<Running> {
    let (request_r, mut request_w) = io::pipe()?;
    let (answer_r, answer_w) = io::pipe()?;
    let (output_r, output_w) = io::pipe()?;
    let cwd = Path::new(std::ffi::OsStr::from_bytes(&request.attach.cwd));
    let cwd = if cwd.is_absolute() && cwd.is_dir() {
        cwd
    } else {
        Path::new("/")
    };
    let mut command = Command::new(spawn.exe);
    command
        .arg("config-child")
        .env_clear()
        .envs(spawn.env.iter().map(|(k, v)| (k, v)))
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(output_w.try_clone()?)
        .stderr(output_w)
        .process_group(0);
    let fds = [
        (OwnedFd::from(request_r), REQUEST_FD),
        (OwnedFd::from(answer_w), ANSWER_FD),
    ];
    let raw: [(RawFd, RawFd); 2] = [
        (fds[0].0.as_raw_fd(), fds[0].1),
        (fds[1].0.as_raw_fd(), fds[1].1),
    ];
    // SAFETY: between fork and exec this only calls fcntl, dup2 and close,
    // which are async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            // out of the way of 3 and 4 first, so neither dup2 clobbers the other
            let mut high = [0; 2];
            for (i, (fd, _)) in raw.iter().enumerate() {
                high[i] = libc::fcntl(*fd, libc::F_DUPFD, 10);
                if high[i] < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            for (i, (_, to)) in raw.iter().enumerate() {
                if libc::dup2(high[i], *to) < 0 {
                    return Err(io::Error::last_os_error());
                }
                libc::close(high[i]);
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(command);
    drop(fds);
    // on a thread: a child that doesn't read mustn't hold up the timeout
    let bytes = request.encode();
    std::thread::spawn(move || {
        let _ = request_w.write_all(&bytes);
    });
    Ok(Running {
        child,
        answer: answer_r,
        output: output_r,
    })
}

impl Running {
    fn wait(
        mut self,
        deadline: Instant,
        time: Duration,
        printed: &mut Printed,
    ) -> Result<Answer, Failure> {
        for fd in [self.answer.as_raw_fd(), self.output.as_raw_fd()] {
            set_nonblocking(fd);
        }
        let mut answer = Vec::new();
        let mut answer_open = true;
        let mut output_open = true;
        let got = loop {
            if let Some(body) = complete(&answer) {
                break Ok(body);
            }
            if !answer_open {
                break Err(None);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Some(Failure::TimedOut(time)));
            }
            let mut fds = vec![poll_fd(self.answer.as_raw_fd())];
            if output_open {
                fds.push(poll_fd(self.output.as_raw_fd()));
            }
            let ms = left.as_millis().clamp(1, 1000) as libc::c_int;
            // SAFETY: fds is a valid array of pollfd for its length.
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
            answer_open = drain(&mut self.answer, |b| answer.extend_from_slice(b));
            if answer.len() > MAX_ANSWER + 4 {
                break Err(Some(Failure::BadAnswer("too long".into())));
            }
            if output_open {
                output_open = drain(&mut self.output, |b| printed.add(b));
            }
        };
        let status = match &got {
            // answered, or out of time: nothing it started may outlive it
            Ok(_) | Err(Some(_)) => self.kill_group(),
            // it exited, or closed fd 4 before exiting
            Err(None) => {
                let status = wait_until(&mut self.child, deadline);
                self.kill_group().or(status)
            }
        };
        drain(&mut self.output, |b| printed.add(b));
        printed.finish();
        match got {
            Ok(body) => Answer::decode(body).map_err(|e| Failure::BadAnswer(e.to_string())),
            Err(Some(failure)) => Err(failure),
            Err(None) => Err(match status {
                Some(s) if s.signal().is_some() => Failure::Killed(s.signal().unwrap()),
                Some(s) => Failure::Exited(s.code().unwrap_or(-1)),
                None => Failure::TimedOut(time),
            }),
        }
    }

    /// Kills the child's process group, then reaps the child.
    fn kill_group(&mut self) -> Option<ExitStatus> {
        let pgid = self.child.id() as libc::pid_t;
        // SAFETY: the child isn't reaped yet, so its group id is still
        // ours to signal.
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
        self.child.wait().ok()
    }
}

/// The answer's body, once all of it has come.
fn complete(bytes: &[u8]) -> Option<&[u8]> {
    let len = u32::from_le_bytes(bytes.get(..4)?.try_into().unwrap()) as usize;
    bytes.get(4..4 + len)
}

fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        if let Ok(Some(s)) = child.try_wait() {
            return Some(s);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn poll_fd(fd: RawFd) -> libc::pollfd {
    libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }
}

fn set_nonblocking(fd: RawFd) {
    // SAFETY: fcntl on an fd we own.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

/// Reads what `pipe` has now into `to`: false once it has ended.
fn drain(pipe: &mut io::PipeReader, mut to: impl FnMut(&[u8])) -> bool {
    let mut buf = [0u8; 16 << 10];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => to(&buf[..n]),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
}

/// The config's output, split into lines, up to [`MAX_PRINTED`] bytes.
#[derive(Default)]
struct Printed {
    lines: Vec<String>,
    partial: Vec<u8>,
    kept: usize,
    dropped: bool,
}

impl Printed {
    fn add(&mut self, bytes: &[u8]) {
        let room = MAX_PRINTED - self.kept;
        if bytes.len() > room {
            self.dropped = true;
        }
        let bytes = &bytes[..bytes.len().min(room)];
        self.kept += bytes.len();
        for &b in bytes {
            if b == b'\n' {
                self.end_line();
            } else {
                self.partial.push(b);
            }
        }
    }

    fn end_line(&mut self) {
        let line = String::from_utf8_lossy(&self.partial).into_owned();
        self.lines.push(line);
        self.partial.clear();
    }

    fn finish(&mut self) {
        if !self.partial.is_empty() {
            self.end_line();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn why_there_was_no_answer() {
        let file = "init.lua";
        assert_eq!(
            Failure::Exited(0).message(file),
            "init.lua ended before answering (exit status 0; did it call os.exit?)"
        );
        assert_eq!(
            Failure::Killed(11).message(file),
            "init.lua was killed by signal 11 before answering"
        );
        assert_eq!(
            Failure::TimedOut(super::super::TIME).message(file),
            "init.lua didn't answer within 3 s"
        );
    }

    #[test]
    fn output_splits_into_lines_and_stops_at_64_kib() {
        let mut p = Printed::default();
        p.add(b"a\nb");
        p.add(b"c\n\nd");
        p.finish();
        assert_eq!(p.lines, ["a", "bc", "", "d"]);
        assert!(!p.dropped);
        let mut p = Printed::default();
        p.add(&vec![b'x'; MAX_PRINTED + 1]);
        p.finish();
        assert_eq!(p.lines, ["x".repeat(MAX_PRINTED)]);
        assert!(p.dropped);
    }
}
