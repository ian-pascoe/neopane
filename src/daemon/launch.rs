//! `neorex [nvim args]`: opens Neovim in this pane. Starts the daemon in the
//! background, then hands it this terminal and waits as its pane helper.

use std::ffi::OsStr;
use std::io::{self, Read};
use std::process::{Command, Stdio};

use super::pane_helper;
use super::serve;
use crate::attach::Transport;
use crate::log::{self, LogLevel, Point, Traces};

/// The launcher is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the launcher gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    NotATerminal,
    NotInRex,
    /// The daemon didn't start; its log is at this path.
    DaemonFailed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotATerminal => write!(
                f,
                "stdin isn't a terminal; to start a daemon without one, use --detach"
            ),
            Error::NotInRex => write!(f, "run it in a Rex pane"),
            Error::DaemonFailed(log) => write!(f, "the daemon didn't start; see {log}"),
        }
    }
}

impl std::error::Error for Error {}

/// Starts the daemon and attaches this terminal to it: how the attach
/// ended, for the exit status.
pub fn run<S: AsRef<OsStr>>(nvim_args: &[S]) -> Result<pane_helper::Exit> {
    // L6 is timed from here
    let started = log::now_ns();
    // SAFETY: isatty only inspects the fd.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return Err(Error::NotATerminal.into());
    }
    // each window becomes a Rex pane
    if std::env::var_os("REX_SERVER").is_none() || std::env::var_os("REX_SESSION").is_none() {
        return Err(Error::NotInRex.into());
    }

    let exe = std::env::current_exe()?.canonicalize()?;
    let dir = serve::private_dir()?;
    let sock = serve::path(&dir, "sock");
    let log_path = serve::path(&dir, "log");
    let mut log = serve::create_log(&log_path)?;
    // the daemon warns of a level it doesn't know
    if log::from_env().0 == LogLevel::Debug {
        let mut traces = Traces::new();
        traces.set_on(true);
        traces.record(Point::Exec, u64::from(std::process::id()), started, None);
        traces.write_into(&mut log)?;
    }
    // (the log is closed here once the command, which holds it, is dropped)
    let mut daemon = Command::new(&exe)
        .arg("daemon")
        .arg(&dir)
        .args(nvim_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()?;

    // one byte once it's listening; nothing if it failed
    let mut ready = [0u8; 1];
    let mut stdout = daemon.stdout.take().unwrap();
    let n = loop {
        match stdout.read(&mut ready) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break 0,
            Ok(n) => break n,
        }
    };
    drop(stdout);
    if n != 1 {
        return Err(Error::DaemonFailed(log_path.to_string()).into());
    }
    // the daemon outlives us; it's never waited for here
    Ok(pane_helper::run(
        sock.as_ref(),
        pane_helper::Want::SelfAttach,
        Transport::Fd,
    ))
}
