//! `neorex [nvim args]`: opens Neovim in this pane. Starts the daemon in the
//! background, then hands it this terminal and waits as its pane helper.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read};
use std::process::{Command, Stdio};

use super::pane_helper;
use super::serve;

/// The launcher is a top layer: any error will do.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the launcher gave up, with the Zig's error names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotATerminal,
    NotInRex,
    DaemonFailed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

pub fn run<S: AsRef<OsStr>>(nvim_args: &[S]) -> Result<()> {
    // SAFETY: isatty only inspects the fd.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return Err(Error::NotATerminal.into());
    }
    // each window becomes a Rex pane
    if std::env::var_os("REX_SERVER").is_none() || std::env::var_os("REX_SESSION").is_none() {
        eprintln!("neorex: run it in a Rex pane");
        return Err(Error::NotInRex.into());
    }

    let base = serve::base_path()?;
    let sock = serve::path(&base, ".sock");
    let log_path = serve::path(&base, ".log");

    let exe = std::env::current_exe()?.canonicalize()?;
    let log = File::create(&log_path)?;
    // (the log is closed here once the command, which holds it, is dropped)
    let mut daemon = Command::new(&exe)
        .arg("daemon")
        .arg(&base)
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
        eprintln!("neorex: the daemon didn't start; see {log_path}");
        return Err(Error::DaemonFailed.into());
    }
    // the daemon outlives us; it's never waited for here
    pane_helper::run(&sock, None)?;
    Ok(())
}
