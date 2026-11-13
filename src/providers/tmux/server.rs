//! The tmux server the choosing attach runs in, and running `tmux` on it.
//! Every call passes its socket with `-S`, from `TMUX`'s first field, as
//! tmux finds its own; `TMUX_PANE` is taken from the call's environment, so
//! a command without `-t`, such as a listing of the whole server, can't be
//! sent somewhere by one that leaked.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::providers::builtin::Given;

/// The oldest tmux the built-in works with: `list-panes -a` with
/// `pane_tty`, `split-window -l` and `-P -F`, and `TERM_PROGRAM` in panes.
pub(super) const OLDEST: (u32, u32) = (3, 2);

/// A tmux server, by its socket.
pub(super) struct Server {
    socket: Vec<u8>,
}

impl Server {
    /// The server `TMUX` names, or why there's none.
    pub(super) fn of(given: &Given) -> Result<Server, String> {
        Server::named_by(&given.env("TMUX").unwrap_or_default())
            .ok_or_else(|| "TMUX isn't set: the choosing attach isn't in tmux".into())
    }

    /// The server a `TMUX` value names, by its first field; None if that's
    /// empty.
    pub(super) fn named_by(tmux: &[u8]) -> Option<Server> {
        let socket = tmux.split(|&b| b == b',').next().unwrap_or_default();
        (!socket.is_empty()).then(|| Server {
            socket: socket.to_vec(),
        })
    }

    /// Its socket's path, for messages.
    pub(super) fn socket(&self) -> String {
        String::from_utf8_lossy(&self.socket).into_owned()
    }

    /// `tmux -S SOCKET args…`'s stdout, run in environment `env` alone
    /// (less `TMUX_PANE`), `tmux` found on its `PATH`; killed if it hasn't
    /// exited within `time`.
    pub(super) fn run_within<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        env: &[(Vec<u8>, Vec<u8>)],
        time: Duration,
    ) -> Result<Vec<u8>, Ran> {
        let mut child = Command::new("tmux")
            .arg("-S")
            .arg(OsStr::from_bytes(&self.socket))
            .args(args)
            .env_clear()
            .envs(
                env.iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            )
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Ran::CantRun(e.to_string()))?;
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
        let due = Instant::now() + time;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < due => std::thread::sleep(Duration::from_millis(5)),
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Ran::TimedOut);
                }
            }
        };
        let (out, err) = (
            stdout.join().unwrap_or_default(),
            stderr.join().unwrap_or_default(),
        );
        if status.success() {
            return Ok(out);
        }
        let err = String::from_utf8_lossy(&err);
        Err(Ran::Failed(
            err.lines().next().unwrap_or_default().to_string(),
        ))
    }

    /// `tmux -S SOCKET args…`'s stdout, or why it failed: its first stderr
    /// line.
    pub(super) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, String> {
        let out = self
            .command(args)
            .output()
            .map_err(|e| format!("can't run tmux: {e}"))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or_default();
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        Err(format!(
            "tmux {} failed ({}): {first}",
            shown.join(" "),
            out.status
        ))
    }

    /// `tmux -S SOCKET args…`, to run, its stdin `/dev/null`.
    pub(super) fn command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.arg("-S")
            .arg(OsStr::from_bytes(&self.socket))
            .args(args)
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null());
        cmd
    }

    /// The installed tmux's version, as `tmux -V` prints it, such as
    /// `3.2a`.
    pub(super) fn version() -> Result<String, String> {
        let out = Command::new("tmux")
            .arg("-V")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("can't run tmux: {e}"))?;
        Ok(version_of(&out.stdout))
    }
}

/// The version `tmux -V` printed as `out`, such as `3.2a`.
pub(super) fn version_of(out: &[u8]) -> String {
    let shown = String::from_utf8_lossy(out);
    let shown = shown.trim();
    shown.strip_prefix("tmux ").unwrap_or(shown).to_string()
}

/// How a timed run of tmux failed.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Ran {
    /// It couldn't start, and why.
    CantRun(String),
    /// It exited unsuccessfully: its first stderr line.
    Failed(String),
    /// It didn't exit in time, and was killed.
    TimedOut,
}

/// Whether tmux `version`, as `tmux -V` gives it after `tmux `, is older
/// than [`OLDEST`]: `3.1c`, `2.9` are; `3.2a`, `3.10`, `next-3.9` and a
/// version it can't read, such as `master`, aren't.
pub(super) fn too_old(version: &str) -> bool {
    older_than(version, OLDEST)
}

/// Whether tmux `version`, as `tmux -V` gives it after `tmux `, is older
/// than `major`.`minor`; a version it can't read isn't.
pub(super) fn older_than(version: &str, (major_floor, minor_floor): (u32, u32)) -> bool {
    let digits = version.trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = digits.split('.');
    let major = parts.next().and_then(|m| m.parse::<u32>().ok());
    let minor = parts.next().map(|m| {
        m.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
    });
    match (major, minor) {
        (Some(major), Some(Ok(minor))) => (major, minor) < (major_floor, minor_floor),
        _ => false,
    }
}
