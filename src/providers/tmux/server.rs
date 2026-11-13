//! The tmux server the choosing attach runs in, and running `tmux` on it.
//! Every call passes its socket with `-S`, from `TMUX`'s first field, as
//! tmux finds its own; `TMUX_PANE` is taken from the call's environment, so
//! a command without `-t`, such as a listing of the whole server, can't be
//! sent somewhere by one that leaked.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

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
        let tmux = given.env("TMUX").unwrap_or_default();
        let socket = tmux.split(|&b| b == b',').next().unwrap_or_default();
        if socket.is_empty() {
            return Err("TMUX isn't set: the choosing attach isn't in tmux".into());
        }
        Ok(Server {
            socket: socket.to_vec(),
        })
    }

    /// `tmux -S SOCKET args…`'s stdout, or why it failed: its first stderr
    /// line.
    pub(super) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, String> {
        let out = Command::new("tmux")
            .arg("-S")
            .arg(OsStr::from_bytes(&self.socket))
            .args(args)
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
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

    /// The installed tmux's version, as `tmux -V` prints it, such as
    /// `3.2a`.
    pub(super) fn version() -> Result<String, String> {
        let out = Command::new("tmux")
            .arg("-V")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("can't run tmux: {e}"))?;
        let shown = String::from_utf8_lossy(&out.stdout);
        let shown = shown.trim();
        Ok(shown.strip_prefix("tmux ").unwrap_or(shown).to_string())
    }
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
