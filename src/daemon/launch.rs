//! `neorex [nvim args]`: opens Neovim in this pane. Starts the daemon in the
//! background, then hands it this terminal and waits as its pane helper.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use super::dir;
use super::pane_helper;
use crate::attach::Transport;
use crate::log::{self, LogLevel, Point, Traces};
use crate::name::NAME;

/// The launcher is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the launcher gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    NotATerminal,
    NotInRex,
    /// The daemon said why it didn't start.
    Refused(String),
    /// The daemon ended without a word: the first line it wrote to stderr,
    /// if any, and the base directory its log would be under.
    DaemonFailed(Option<String>, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotATerminal => write!(
                f,
                "stdin isn't a terminal; to start a daemon without one, use --detach"
            ),
            Error::NotInRex => write!(f, "run it in a Rex pane"),
            Error::Refused(why) => write!(f, "{why}"),
            Error::DaemonFailed(said, base) => {
                let said = said.as_deref().unwrap_or("it exited with no message");
                write!(f, "the daemon didn't start: {said} (logs: {base})")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The daemon's one line on the ready pipe.
#[derive(Debug, PartialEq, Eq)]
enum Ready {
    /// `ok <name>`: it listens, under this name.
    Ok(String),
    /// `error <reason>`.
    Error(String),
    /// Nothing, or nothing it says.
    Nothing,
}

fn ready(line: &str) -> Ready {
    let line = line.strip_suffix('\n').unwrap_or(line);
    if let Some(name) = line.strip_prefix("ok ")
        && dir::valid_name(OsStr::new(name))
    {
        return Ready::Ok(name.to_string());
    }
    match line.strip_prefix("error ") {
        Some(reason) => Ready::Error(reason.to_string()),
        None => Ready::Nothing,
    }
}

/// Starts a daemon and attaches this terminal to it: how the attach ended,
/// for the exit status. The daemon is named after this directory,
/// de-duplicated. A failure before the attach is held as a failed attach
/// is, with a key wait unless a shell started it as a job.
pub fn run<S: AsRef<OsStr>>(nvim_args: &[S]) -> pane_helper::Exit {
    let started = std::time::Instant::now();
    match start(nvim_args) {
        Ok(exit) => exit,
        Err(e) => pane_helper::launcher_failed(e.to_string(), started),
    }
}

fn start<S: AsRef<OsStr>>(nvim_args: &[S]) -> Result<pane_helper::Exit> {
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
    // warned of before the daemon starts, which warns in its log
    let transport = Transport::from_env(|w| eprintln!("{NAME}: {w}"));
    let base = dir::base()?;
    let name = default_name(&std::env::current_dir().unwrap_or_default());

    let exe = std::env::current_exe()?.canonicalize()?;
    let mut command = Command::new(&exe);
    // `ps` shows `<name> daemon NAME`
    command
        .arg0(NAME)
        .args(["daemon", &name, "--dedup", "--"])
        .args(nvim_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // out of the pane's session, so closing the pane doesn't signal it
    // SAFETY: setsid is async-signal-safe, and touches no memory.
    unsafe {
        command.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let mut daemon = command.spawn()?;

    let mut line = String::new();
    let stdout = daemon.stdout.take().expect("piped");
    let _ = BufReader::new(stdout).read_line(&mut line);
    let name = match ready(&line) {
        Ready::Ok(name) => name,
        Ready::Error(why) => {
            gone(&mut daemon);
            return Err(Error::Refused(why).into());
        }
        Ready::Nothing => {
            let said = gone(&mut daemon);
            return Err(Error::DaemonFailed(said, base.to_string()).into());
        }
    };
    // (from here it writes to its log, not to this pipe)
    drop(daemon.stderr.take());
    // the daemon warns of a level it doesn't know
    if log::from_env().0 == LogLevel::Debug {
        let mut traces = Traces::new();
        traces.set_on(true);
        traces.record(Point::Exec, u64::from(std::process::id()), started, None);
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(base.log(&name))?;
        traces.write_into(&mut log)?;
    }
    // the daemon outlives us; it's never waited for here
    Ok(pane_helper::run(
        &base.address(&name),
        pane_helper::Want::SelfAttach,
        transport,
    ))
}

/// The name of a daemon started in `cwd`: from its last component.
fn default_name(cwd: &std::path::Path) -> String {
    dir::clean_name(cwd.file_name().unwrap_or_default())
}

/// Waits for a daemon that failed to start to end: the first line it wrote
/// to stderr, if any.
fn gone(daemon: &mut Child) -> Option<String> {
    let mut said = Vec::new();
    if let Some(mut stderr) = daemon.stderr.take() {
        let _ = stderr.read_to_end(&mut said);
    }
    let _ = daemon.wait();
    String::from_utf8_lossy(&said)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{Ready, default_name, ready};
    use std::path::Path;

    #[test]
    fn a_daemon_is_named_after_the_directory_it_started_in() {
        assert_eq!("neox.1", default_name(Path::new("/home/me/src/neo x.1")));
        assert_eq!("default", default_name(Path::new("/")));
        assert_eq!("default", default_name(Path::new("/home/\u{e9}")));
    }

    #[test]
    fn the_ready_line_names_the_daemon_or_says_why_not() {
        assert_eq!(Ready::Ok("work-2".into()), ready("ok work-2\n"));
        assert_eq!(
            Ready::Error("work is already running".into()),
            ready("error work is already running\n")
        );
        for nothing in ["", "\n", "ok \n", "ok a/b\n", "r", "okwork\n"] {
            assert_eq!(Ready::Nothing, ready(nothing), "{nothing:?}");
        }
    }
}
