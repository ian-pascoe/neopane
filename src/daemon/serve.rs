//! `neorex daemon NAME [--dedup] -- [nvim args]`: the background half of
//! `neorex`. Claims its directory (`dir`), then takes
//! the launching pane's terminal, runs Neovim under `--embed`, speaking RPC
//! over its stdin and stdout, and shows each of its windows in a Rex pane (see
//! `Daemon`). Exits when Neovim does or the last pane goes away.
//!
//! Teardown is by values whose `Drop` does it, declared in the order they
//! must end: the daemon (its panes, then the loop, Rex and Neovim), Neovim's
//! process, the launcher's pane if the daemon never took it, the socket files,
//! the helpers' socket, and last the lock on the daemon's directory. The log
//! stays.
//!
//! Every file of a daemon is in its own directory, `<base>/NAME/` (`dir`),
//! which the daemon claims and holds locked for its whole life.

use std::cell::Cell;
use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, IntoRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::attach_conn::{self, Pending, Step, euid};
use super::daemon::{Daemon, StopReason, Token};
use super::dir::{self, Claimed, MAX_PATH};
use super::event_loop::Loop;
use super::nvim_session::NvimSession;
use super::pane::Pane;
use super::provider::TerminalId;
use super::surface::Surface;
use crate::attach::{End, Ending, Purpose, Refusal, Transport};
use crate::log::{self, Point, log_info, log_warn};
use crate::name::{self, NAME};
use crate::nvim::nvim::{Frame, Nvim};
use crate::nvim::{msgpack, rpc};
use crate::providers::Held;
use crate::providers::rex::RexProvider;
use crate::rex::client::Client;
use crate::rex::ids;
use crate::rex::session::Session as RexSession;

/// `serve` is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the daemon couldn't start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// `--detach`, which comes with #124.
    DetachNotBuilt,
    NotInRex,
    LauncherGone,
    NoPane,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::DetachNotBuilt => write!(f, "--detach isn't built yet"),
            Error::NotInRex => write!(f, "run it in a Rex pane"),
            Error::LauncherGone => write!(f, "the launcher went away"),
            Error::NoPane => write!(f, "the launcher's pane didn't attach"),
        }
    }
}

impl std::error::Error for Error {}

/// Makes the log at `path`, for the daemon's stderr: a new file only this user
/// can read, never one that's there already or a link. Appended to, as the
/// launcher adds its trace.
pub(crate) fn create_log(path: &str) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .append(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// What `serve` runs against. `run` uses the defaults (`Config::new`); tests
/// swap the seams.
pub(crate) struct Config<'a> {
    pub(crate) rex_url: &'a str,
    pub(crate) session_id: &'a str,
    /// Starts Neovim: its arguments follow.
    pub(crate) nvim_command: &'a [&'a str],
    /// How long Neovim has to answer its first request.
    pub(crate) nvim_timeout: Duration,
    /// Called once the helpers' socket listens: the launcher may connect.
    pub(crate) ready: Box<dyn FnMut() -> Result<()> + 'a>,
}

impl<'a> Config<'a> {
    /// The config `run` uses, for Rex at `rex_url` and session `session_id`.
    pub(crate) fn new(rex_url: &'a str, session_id: &'a str) -> Config<'a> {
        Config {
            rex_url,
            session_id,
            nvim_command: &["nvim"],
            nvim_timeout: NVIM_START_TIMEOUT,
            ready: Box::new(|| Ok(())),
        }
    }
}

/// How long Neovim has to answer its first request. It answers once it
/// waits for input, after the user's config has run.
const NVIM_START_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs the daemon `name`, or with `dedup` the first of `name-2`, `name-3`, …
/// that isn't live. The launcher, which started it in a session of its own,
/// is told on stdout, in one line, `ok <name>` once it may attach, or
/// `error <reason>`.
pub fn run<S: AsRef<OsStr>>(name: &str, dedup: bool, detach: bool, nvim_args: &[S]) -> Result<()> {
    // what the daemon was started with, such as a pipe its shell had, never
    // reaches Neovim
    for fd in inherited_fds() {
        close_on_exec(fd);
    }
    let told = Cell::new(false);
    let logging = Cell::new(None);
    let result = start(name, dedup, detach, nvim_args, &told, &logging);
    if let Err(err) = &result
        && !told.get()
    {
        tell_launcher(&format!("error {}", start_error(err, logging.take())));
    }
    result
}

/// What the launcher is told of `err`, which stopped the start: with the
/// daemon's name and log, `(name, log)`, once there is a log.
fn start_error(err: &dyn std::fmt::Display, logging: Option<(String, String)>) -> String {
    match logging {
        Some((name, log)) => format!("{name} didn't start: {err} (log: {log})"),
        None => err.to_string(),
    }
}

/// Claims the directory, makes the log our stderr, and serves.
fn start<S: AsRef<OsStr>>(
    name: &str,
    dedup: bool,
    detach: bool,
    nvim_args: &[S],
    told: &Cell<bool>,
    logging: &Cell<Option<(String, String)>>,
) -> Result<()> {
    if detach {
        return Err(Error::DetachNotBuilt.into());
    }
    let base = dir::base()?;
    let claimed = dir::claim_name(&base, name, dedup)?;
    let log = create_log(&claimed.log())?;
    // SAFETY: dup2 onto stderr from an fd we own, closed after.
    if unsafe { libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    drop(log);
    logging.set(Some((claimed.name().to_string(), claimed.log())));
    log::init_daemon();
    let rex_url = env("REX_SERVER").ok_or(Error::NotInRex)?;
    let session_id = env("REX_SESSION").ok_or(Error::NotInRex)?;
    let ready = || {
        if !tell_launcher(&format!("ok {}", claimed.name())) {
            return Err(Error::LauncherGone.into());
        }
        told.set(true);
        Ok(())
    };
    let config = Config {
        ready: Box::new(ready),
        ..Config::new(&rex_url, &session_id)
    };
    serve(&claimed, nvim_args, config)
}

/// The open fds above stderr.
fn inherited_fds() -> Vec<RawFd> {
    let Ok(dir) = fs::read_dir("/dev/fd") else {
        return Vec::new();
    };
    // (the directory's own fd is among them, closed by the time they're used)
    dir.flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter(|&fd| fd > libc::STDERR_FILENO)
        .collect()
}

/// Makes `fd` close-on-exec, if it's open.
fn close_on_exec(fd: RawFd) {
    // SAFETY: fcntl on an fd number, open or not.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 && flags & libc::FD_CLOEXEC == 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

/// Tells the launcher `line`, its one line on stdout; then stdout goes
/// nowhere. Whether the launcher got it.
fn tell_launcher(line: &str) -> bool {
    let line = format!("{}\n", line.replace('\n', " "));
    let told = std::io::stdout().lock().write_all(line.as_bytes()).is_ok();
    // SAFETY: plain fd syscalls on a path literal.
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::close(devnull);
        }
    }
    told
}

/// Neovim's process, and the thread copying its stderr into the log.
///
/// The daemon ends Neovim by closing its `--embed` channel, dropping its
/// `Nvim` before this: Neovim then exits through `preserve_exit()`, running
/// VimLeave, keeping the swap files of changed buffers and deleting the rest.
/// As this drops, after the panes are hung up, Neovim is waited for, and
/// killed if it hasn't ended within `grace`: one caught in a `system()`
/// call, such as a clipboard read from an autocommand, can hang on its way
/// out, and would keep the daemon waiting for good. Never `:qa`, which can
/// wait at a prompt nobody sees.
struct NvimChild {
    child: Child,
    grace: Duration,
    /// How long a Neovim that closed the channel has to exit by itself before
    /// it's sent SIGHUP.
    exiting: Duration,
    /// Copies Neovim's stderr into ours, the log, until Neovim closes it;
    /// then returns the first bytes.
    stderr: Option<JoinHandle<Vec<u8>>>,
}

/// How long Neovim has to exit after its channel closes before it's killed.
const NVIM_EXIT_GRACE: Duration = Duration::from_secs(3);

/// How long a Neovim that closed the channel itself has to exit before it's
/// sent SIGHUP. One that's exiting ends within milliseconds of closing it.
const NVIM_EXITING: Duration = Duration::from_millis(500);

/// How much of Neovim's stderr is kept for the reason a start failed.
const STDERR_HEAD: usize = 4096;

impl NvimChild {
    fn new(mut child: Child) -> NvimChild {
        let stderr = child
            .stderr
            .take()
            .map(|from| std::thread::spawn(move || drain_stderr(from)));
        NvimChild {
            child,
            grace: NVIM_EXIT_GRACE,
            exiting: NVIM_EXITING,
            stderr,
        }
    }

    /// Waits up to `limit` for Neovim to end; whether it did.
    fn wait_for_exit(&mut self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        loop {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Neovim closed the channel: it's exiting, or a plugin's `:detach` left
    /// it running with no UI. One still running after `exiting` is sent
    /// SIGHUP, which makes it keep its swap files and exit; dropping waits
    /// for it. One that's exiting isn't: a SIGHUP while it deletes its swap
    /// files would keep them.
    fn closed_by_nvim(&mut self) {
        if self.wait_for_exit(self.exiting) {
            return;
        }
        // SAFETY: signalling our own child, which hasn't been reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGHUP) };
    }

    /// Why a Neovim that closed the channel before it answered failed to
    /// start: how it ended, and the first line it wrote to stderr.
    fn exited_at_once(&mut self) -> StartFailure {
        let exit = if self.wait_for_exit(self.grace) {
            nvim_exit(&mut self.child)
        } else {
            None
        };
        let head = self.stderr_head(Duration::from_secs(1));
        StartFailure::Exited {
            exit,
            message: start_reason(&head),
        }
    }

    /// The first bytes Neovim wrote to stderr, once it has closed it, waiting
    /// up to `limit` for that. Empty if it hasn't.
    fn stderr_head(&mut self, limit: Duration) -> Vec<u8> {
        let deadline = Instant::now() + limit;
        while self.stderr.as_ref().is_some_and(|t| !t.is_finished()) {
            if Instant::now() >= deadline {
                return Vec::new();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.stderr
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

impl Drop for NvimChild {
    fn drop(&mut self) {
        if self.wait_for_exit(self.grace) {
            return;
        }
        log_warn!(
            "{NAME} daemon: Neovim didn't exit within {:?} of its channel closing; killing it",
            self.grace
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Copies Neovim's stderr, `from`, into ours, the log, until Neovim closes
/// it. Returns its first `STDERR_HEAD` bytes.
fn drain_stderr(mut from: ChildStderr) -> Vec<u8> {
    let mut head = Vec::new();
    let mut buf = [0u8; 4096];
    let mut log = std::io::stderr();
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return head,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return head,
        };
        let keep = n.min(STDERR_HEAD - head.len());
        head.extend_from_slice(&buf[..keep]);
        let _ = log.write_all(&buf[..n]);
    }
}

/// The reason a start failed, from what Neovim wrote to stderr: its first
/// line, without `nvim: `, passing over `More info with "nvim -h"`.
fn start_reason(stderr: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && *line != r#"More info with "nvim -h""#)
        .map(|line| line.strip_prefix("nvim: ").unwrap_or(line).to_string())
}

/// The oldest Neovim the daemon runs.
const MIN_VERSION: Version = Version {
    major: 0,
    minor: 12,
    patch: 0,
};

/// A Neovim version, from `nvim_get_api_info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: i64,
    minor: i64,
    patch: i64,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The version in `frame`, Neovim's answer to `nvim_get_api_info`:
/// `[channel, {version = {major, minor, patch, ...}, ...}]`.
fn api_info_version(frame: &Frame) -> std::result::Result<Version, String> {
    let rpc::Message::Response {
        err, mut result, ..
    } = frame.message().map_err(|e| e.to_string())?
    else {
        return Err("not an answer".to_string());
    };
    if let Some(err) = err {
        return Err(String::from_utf8_lossy(err).into_owned());
    }
    match parse_api_info(&mut result) {
        Ok(Some(version)) => Ok(version),
        Ok(None) => Err("no version in nvim_get_api_info".to_string()),
        Err(e) => Err(format!("nvim_get_api_info: {e}")),
    }
}

/// The version in `d`, at the result of `nvim_get_api_info`.
fn parse_api_info(d: &mut msgpack::Decoder<'_>) -> rpc::Result<Option<Version>> {
    rpc::expect_array(d)?;
    rpc::skip(d)?;
    for _ in 0..rpc::expect_map(d)? {
        if rpc::expect_str(d)? != b"version" {
            rpc::skip(d)?;
            continue;
        }
        let mut v = [None; 3];
        for _ in 0..rpc::expect_map(d)? {
            let slot = match rpc::expect_str(d)? {
                b"major" => &mut v[0],
                b"minor" => &mut v[1],
                b"patch" => &mut v[2],
                _ => {
                    rpc::skip(d)?;
                    continue;
                }
            };
            *slot = Some(rpc::expect_int(d)?);
        }
        let [Some(major), Some(minor), Some(patch)] = v else {
            return Ok(None);
        };
        return Ok(Some(Version {
            major,
            minor,
            patch,
        }));
    }
    Ok(None)
}

/// Whether Neovim's answer to `nvim_get_api_info` says it's new enough.
fn check_version(frame: &Frame) -> std::result::Result<(), StartFailure> {
    let version = api_info_version(frame).map_err(StartFailure::Nvim)?;
    if version < MIN_VERSION {
        return Err(StartFailure::TooOld(version));
    }
    Ok(())
}

/// Starts Neovim for the daemon in `dir`: `command`, then the daemon's
/// arguments, then `nvim_args`, in the launch directory, its RPC on its stdin
/// and stdout and `$<NAME>` the daemon's address. Waits up to `timeout` for
/// it to answer `nvim_get_api_info`, and checks its version.
///
/// Drop the `Nvim` before the `NvimChild`: closing the channel ends Neovim.
fn start_nvim<S: AsRef<OsStr>>(
    command: &[&str],
    dir: &Claimed,
    nvim_args: &[S],
    timeout: Duration,
) -> std::result::Result<(NvimChild, Nvim), StartFailure> {
    // before the user's config, so it can tell it runs under neorex
    let mut child = Command::new(command[0])
        .args(&command[1..])
        .args(["--embed", "--headless", "--listen"])
        .arg(dir.nvim_sock())
        .args(["--cmd", &format!("let g:{NAME} = 1")])
        .args(nvim_args)
        // Neovim's alone: nothing else the daemon runs gets it
        .env(name::upper(), dir.address().as_arg())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(StartFailure::CantRun)?;
    let (stdin, stdout) = (child.stdin.take(), child.stdout.take());
    // declared in this order so that, on a return, the channel closes before
    // Neovim is waited for
    let mut child = NvimChild::new(child);
    let mut nvim = Nvim::from_pipes(stdin.unwrap(), stdout.unwrap());

    match first_answer(&mut nvim, timeout) {
        Ok(frame) => check_version(&frame)?,
        Err(NoAnswer::Closed) => return Err(child.exited_at_once()),
        Err(NoAnswer::Failed(failure)) => return Err(failure),
    }
    // Neovim makes its socket under its umask, which its files need. It's
    // there once Neovim answers.
    dir.own_nvim_sock()
        .map_err(|err| StartFailure::Nvim(format!("{}: {err}", dir.nvim_sock().display())))?;
    Ok((child, nvim))
}

/// Why Neovim's first answer didn't come.
enum NoAnswer {
    /// Neovim closed the channel first.
    Closed,
    Failed(StartFailure),
}

/// Asks Neovim for `nvim_get_api_info` and waits up to `timeout` for the
/// answer.
fn first_answer(nvim: &mut Nvim, timeout: Duration) -> std::result::Result<Frame, NoAnswer> {
    let id = nvim.request("nvim_get_api_info", 0);
    // only a Neovim that has closed its stdin refuses it
    nvim.flush().map_err(|_| NoAnswer::Closed)?;
    let deadline = Instant::now() + timeout;
    loop {
        while let Some(frame) = nvim
            .next()
            .map_err(|e| NoAnswer::Failed(StartFailure::Nvim(e.to_string())))?
        {
            if matches!(frame.message(), Ok(rpc::Message::Response { id: got, .. }) if got == id) {
                return Ok(frame);
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fds = [libc::pollfd {
            fd: nvim.read_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one valid pollfd.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, left.as_millis() as libc::c_int) };
        if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if n <= 0 {
            return Err(NoAnswer::Failed(StartFailure::NoAnswer));
        }
        match nvim.read() {
            Ok(true) => {}
            Ok(false) => return Err(NoAnswer::Closed),
            Err(e) => return Err(NoAnswer::Failed(StartFailure::Nvim(e.to_string()))),
        }
    }
}

pub(crate) fn serve<S: AsRef<OsStr>>(
    dir: &Claimed,
    nvim_args: &[S],
    mut config: Config<'_>,
) -> Result<()> {
    let log = dir.log();
    // resolved, symlinks and all: the `nrex` link shouldn't end up in every
    // pane's command
    let exe = std::env::current_exe()?
        .canonicalize()?
        .to_string_lossy()
        .into_owned();

    // open until we exit: every new pane's helper connects here
    let server = dir.bind()?;
    // the socket files, last of all, after Neovim has had its chance to
    // remove its own
    let _files = dir.remove_sockets_on_drop();

    (config.ready)()?;

    let rex_block: Option<TerminalId> = match env("REX_BLOCK") {
        Some(b) => match ids::block(b.as_bytes()) {
            Ok(_) => Some(TerminalId::from(b.as_str())),
            Err(_) => {
                log_warn!("{NAME} daemon: REX_BLOCK isn't a block id: {b}");
                None
            }
        },
        None => None,
    };
    let name = dir.name().to_string();
    let mut startup = Startup {
        log: &log,
        launch: Some(Pane::startup(rex_block, take_pane(&server, &name, &log)?)),
    };

    // Rex is connected to while Neovim starts. Declared in this order so
    // that, on a return, the channel closes before Neovim is waited for.
    let mut child;
    let nvim;
    let (started, rex) = std::thread::scope(|s| {
        let rex = s.spawn(|| Client::connect(config.rex_url, NAME));
        let started = start_nvim(config.nvim_command, dir, nvim_args, config.nvim_timeout);
        let rex = rex
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        (started, rex)
    });
    match started {
        Ok(started) => (child, nvim) = started,
        Err(failure) => return Err(startup.give_up(failure)),
    }
    let rex = match rex {
        Ok(rex) => rex,
        Err(err) => return Err(startup.give_up(StartFailure::Rex(err.to_string()))),
    };

    let mut event_loop = Loop::init()?;
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        event_loop.add_signal(sig, Token::Signal as usize)?;
    }
    event_loop.add_read(nvim.read_fd(), Token::NvimChannel as usize)?;
    event_loop.add_read(server.as_raw_fd(), Token::HelperListener as usize)?;
    event_loop.add_read(rex.fd(), Token::Provider as usize)?;

    // dropped first: panes hung up before anything else ends
    let mut d = Daemon::new(
        event_loop,
        NvimSession::new(nvim),
        Some(Held::Rex(Box::new(RexProvider::new(RexSession::new(
            rex,
            config.session_id.to_string(),
        ))))),
        exe,
        dir.address(),
    );
    d.name = name;
    d.log = log.clone();
    d.start(startup.release())?;

    let stop_reason = d.run(&server)?;
    log_info!("{NAME} daemon: {}", stop_reason.tag_name());
    if stop_reason == StopReason::NvimExited {
        // panes first: Neovim may take a while to finish exiting
        drop(d);
        child.closed_by_nvim();
    }
    Ok(())
}

/// Why the daemon gave up before showing any window, for the user.
#[derive(Debug)]
enum StartFailure {
    /// `nvim` couldn't be run.
    CantRun(std::io::Error),
    /// Neovim closed its channel before it answered: how it ended, if it
    /// has, and the reason it gave (`start_reason`).
    Exited {
        exit: Option<NvimExit>,
        message: Option<String>,
    },
    TooOld(Version),
    /// Neovim didn't answer in time.
    NoAnswer,
    /// Talking to Neovim went wrong.
    Nvim(String),
    Rex(String),
}

impl std::fmt::Display for StartFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartFailure::CantRun(e) if e.kind() == std::io::ErrorKind::NotFound => {
                write!(f, "can't run nvim: not found on PATH")
            }
            StartFailure::CantRun(e) => write!(f, "can't run nvim: {e}"),
            StartFailure::Exited { exit, message } => {
                match exit {
                    Some(NvimExit::Status(n)) => write!(f, "Neovim exited at once (status {n})")?,
                    Some(NvimExit::Signal(n)) => {
                        write!(f, "Neovim was killed at once (signal {n})")?
                    }
                    None => write!(f, "Neovim closed its channel before it was ready")?,
                }
                match message {
                    Some(m) => write!(f, ": {m}"),
                    None if exit.is_some() => write!(f, " with no message"),
                    None => Ok(()),
                }
            }
            StartFailure::TooOld(v) => {
                write!(
                    f,
                    "Neovim {}.{} or later is needed; nvim is {v}",
                    MIN_VERSION.major, MIN_VERSION.minor
                )
            }
            StartFailure::NoAnswer => write!(f, "Neovim didn't start in time"),
            StartFailure::Nvim(e) => write!(f, "couldn't talk to Neovim: {e}"),
            StartFailure::Rex(e) => write!(f, "couldn't connect to Rex: {e}"),
        }
    }
}

impl std::error::Error for StartFailure {}

/// How Neovim ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NvimExit {
    Status(u8),
    Signal(u32),
}

/// The launcher's pane, owned until the daemon takes it over, and what's
/// needed to tell its user why a start failed.
struct Startup<'a> {
    log: &'a str,
    launch: Option<Pane>,
}

impl Drop for Startup<'_> {
    fn drop(&mut self) {
        if let Some(launch) = self.launch.take() {
            fail(
                launch,
                &format!("the daemon didn't start; see {}", self.log),
            );
        }
    }
}

impl Startup<'_> {
    /// The pane, for the daemon to own.
    fn release(&mut self) -> Pane {
        self.launch
            .take()
            .expect("the launcher's pane was released twice")
    }

    /// Tells the pane why we're stopping, then returns `failure`, which
    /// `main` prints to the log.
    fn give_up(&mut self, failure: StartFailure) -> Box<dyn std::error::Error> {
        let message = failure_message(&failure, self.log);
        fail(self.release(), &message);
        Box::new(failure)
    }
}

/// Ends `launch`'s attach as failed with `message`, which its helper prints.
fn fail(launch: Pane, message: &str) {
    launch.end(Ending::Failed, message);
}

/// How `child`, which has ended, ended. Reaps it.
fn nvim_exit(child: &mut Child) -> Option<NvimExit> {
    let status = child.try_wait().ok()??;
    if let Some(code) = status.code() {
        return Some(NvimExit::Status(code as u8));
    }
    status.signal().map(|sig| NvimExit::Signal(sig as u32))
}

/// A line for the user, which the launcher prints after `<name>: `: why
/// the start failed, and where the log is. At most 256 bytes plus a path.
fn failure_message(failure: &StartFailure, log: &str) -> String {
    let mut w = format!("{failure}; see {log}");
    let mut end = w.len().min(256 + MAX_PATH);
    while !w.is_char_boundary(end) {
        end -= 1;
    }
    w.truncate(end);
    w
}

/// The environment variable `name`, if set (and Unicode).
fn env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

/// Waits up to 5 s for the launcher's helper, and takes its terminal: the
/// one attach of its own accord this daemon takes (#105 opens it up). Its
/// hello gets `HELLO_TIMEOUT`, blocking: the loop isn't running yet.
fn take_pane(server: &UnixListener, name: &str, log: &str) -> Result<Box<Surface>> {
    let mut fds = [libc::pollfd {
        fd: server.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one valid pollfd.
    let ready = loop {
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 1, 5000) };
        if n >= 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break n;
        }
    };
    if ready <= 0 {
        return Err(Error::NoPane.into());
    }
    let (stream, _) = server.accept()?;
    let Some(pending) = Pending::new(stream, attach_conn::HELLO_TIMEOUT, euid())? else {
        return Err(Error::NoPane.into());
    };
    let Step::Arrived(a) = pending.read_blocking() else {
        return Err(Error::NoPane.into());
    };
    let hello = match a.hello {
        Ok(h) => h,
        Err(e) => {
            attach_conn::refuse(a.stream, attach_conn::refusal_of(&e));
            return Err(e.into());
        }
    };
    let Purpose::SelfAttach { terminal, .. } = hello.purpose else {
        let message = format!("the launcher's attach can't be {}", hello.purpose.name());
        attach_conn::refuse(a.stream, End::refusal(Refusal::Hello, message));
        return Err(Error::NoPane.into());
    };
    log_info!(
        "{NAME} daemon: hello: {}",
        attach_conn::describe("self", None, &terminal, &hello.build)
    );
    if let Err(z) = attach_conn::check_terminal(&terminal, a.tty.as_ref()) {
        attach_conn::refuse(a.stream, z);
        return Err(Error::NoPane.into());
    }
    let mut surface = match (terminal.transport, a.tty) {
        (Transport::Fd, Some(tty)) => Surface::create(a.stream, tty.into_raw_fd(), terminal.size),
        _ => Surface::relayed(a.stream, terminal.size),
    };
    attach_conn::send_accept(&surface.helper_socket, name, log)?;
    log::trace_at(Point::Hello, 0, a.at);
    log::trace(Point::Accepted, 0);
    surface.helper_in = a.rest;
    Ok(surface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::Message;
    use crate::daemon::dir::Address;
    use crate::daemon::dir::testing::Scratch;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixStream;

    /// The file `name` in the directory `dir`.
    fn path(dir: &str, name: &str) -> String {
        format!("{dir}/{name}")
    }

    use crate::daemon::fdpass;
    use std::os::fd::RawFd;

    /// `script` run by `sh` as Neovim, its stdin a pipe like Neovim's
    /// channel, with `grace`. Returns once it has written its first line.
    fn sh_child(script: &str, grace: Duration) -> (NvimChild, std::process::ChildStdin) {
        let mut child = Command::new("sh")
            .args(["-c", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut child = NvimChild::new(child);
        child.grace = grace;
        // its traps are set once it says so
        let mut ready = [0u8; 6];
        std::io::Read::read_exact(child.child.stdout.as_mut().unwrap(), &mut ready).unwrap();
        (child, stdin)
    }

    /// A file in the temporary directory for test `name`, not there yet.
    fn temp_file(name: &str) -> std::path::PathBuf {
        let file = std::env::temp_dir().join(format!("{NAME}-{name}-{}", std::process::id()));
        let _ = fs::remove_file(&file);
        file
    }

    #[test]
    fn a_neovim_that_wont_exit_when_its_channel_closes_is_killed_after_the_grace() {
        // ignores its stdin closing and SIGHUP, as a Neovim stuck on its way
        // out does
        let (child, stdin) = sh_child(
            "trap '' HUP; echo ready; while :; do sleep 0.1; done",
            Duration::from_millis(200),
        );
        let pid = child.child.id() as libc::pid_t;
        let started = Instant::now();
        drop(stdin);
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(5));
        // reaped: no such process
        assert_eq!(-1, unsafe { libc::kill(pid, 0) });
    }

    #[test]
    fn a_neovim_whose_channel_closed_is_waited_for_not_signalled() {
        // exits once its stdin closes, noting whether it got that far
        let done = temp_file("closed");
        let script = format!("echo ready; cat >/dev/null; touch {}", done.display());
        let (child, stdin) = sh_child(&script, Duration::from_secs(30));
        let started = Instant::now();
        drop(stdin);
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(5));
        let exited_itself = done.exists();
        let _ = fs::remove_file(&done);
        assert!(exited_itself, "Neovim was killed");
    }

    #[test]
    fn a_neovim_exiting_by_itself_is_waited_for_not_sent_sighup() {
        // Neovim closes its channel a moment before it exits: a SIGHUP in
        // between could keep swap files it's deleting. This one notes a
        // SIGHUP, then ends itself.
        let marker = temp_file("hup");
        let script = format!(
            "trap 'echo hup > {}; exit 1' HUP; echo ready; sleep 0.1",
            marker.display()
        );
        let (mut child, _stdin) = sh_child(&script, Duration::from_secs(3));
        // far longer than it takes: a loaded machine mustn't make it SIGHUP
        child.exiting = Duration::from_secs(3);
        child.closed_by_nvim();
        drop(child);
        let sent_sighup = marker.exists();
        let _ = fs::remove_file(&marker);
        assert!(
            !sent_sighup,
            "Neovim was sent SIGHUP while exiting by itself"
        );
    }

    #[test]
    fn a_neovim_still_running_after_closing_its_channel_is_sent_sighup() {
        // as after a plugin's `:detach`
        let marker = temp_file("detached");
        let script = format!(
            "trap 'echo hup > {}; exit 1' HUP; echo ready; while :; do sleep 0.1; done",
            marker.display()
        );
        let (mut child, _stdin) = sh_child(&script, Duration::from_secs(30));
        child.exiting = Duration::from_millis(100);
        let started = Instant::now();
        child.closed_by_nvim();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(5));
        let sent_sighup = marker.exists();
        let _ = fs::remove_file(&marker);
        assert!(sent_sighup);
    }

    #[test]
    fn an_inherited_fd_is_made_close_on_exec() {
        // a copy of stderr, not close-on-exec: one that reaches another test's
        // child in the moment before it's fixed holds nothing open
        // SAFETY: dup of an open fd.
        let fd = unsafe { libc::dup(libc::STDERR_FILENO) };
        assert!(fd > libc::STDERR_FILENO);
        let inherited = inherited_fds();
        close_on_exec(fd);
        // SAFETY: reading the flags of our fd, then closing it.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        unsafe { libc::close(fd) };
        assert!(inherited.contains(&fd), "{fd} in {inherited:?}");
        assert_ne!(0, flags & libc::FD_CLOEXEC);
        assert!(!inherited.contains(&libc::STDERR_FILENO));
    }

    #[test]
    fn the_longest_socket_path_is_what_the_platform_binds() {
        // the sizes from the platforms' headers: 104 bytes of path on the BSDs,
        // 108 on Linux, each less the terminator
        let expected = if cfg!(target_os = "linux") { 107 } else { 103 };
        assert_eq!(expected, MAX_PATH);

        let padded = |len: usize| {
            let stem = format!("/tmp/{NAME}-maxpath-{}-", std::process::id());
            format!("{stem}{}", "x".repeat(len - stem.len()))
        };
        let longest = padded(MAX_PATH);
        let listener = UnixListener::bind(&longest).unwrap();
        drop(listener);
        fs::remove_file(&longest).unwrap();
        assert!(UnixListener::bind(padded(MAX_PATH + 1)).is_err());
    }

    fn mode(file: impl AsRef<std::path::Path>) -> u32 {
        fs::symlink_metadata(file).unwrap().permissions().mode() & 0o7777
    }

    /// A daemon's directory, `d`, claimed in a scratch base directory.
    fn test_dir() -> (Scratch, Claimed) {
        let base = Scratch::new();
        let claimed = dir::claim(&base.base(), "d").unwrap();
        (base, claimed)
    }

    #[test]
    fn the_log_is_made_new_and_only_its_user_can_read_it() {
        let dir = Scratch::new();
        let log = dir.file("log");
        create_log(&log).unwrap();
        assert_eq!(0o600, mode(&log));
        // never one that's there already
        let err = create_log(&log).unwrap_err();
        assert_eq!(std::io::ErrorKind::AlreadyExists, err.kind());
        fs::remove_file(&log).unwrap();

        // nor a link: its target is left alone
        let target = dir.file("target");
        fs::write(&target, "kept").unwrap();
        std::os::unix::fs::symlink(&target, &log).unwrap();
        assert!(create_log(&log).is_err());
        assert_eq!("kept", fs::read_to_string(&target).unwrap());
    }

    /// The launcher, for a test: connects to the daemon's socket as soon as it
    /// listens, and hands over a pseudo-terminal's slave as its pane.
    fn fake_launcher_ready(address: &Address, slave: RawFd) -> Result<UnixStream> {
        let stream = address.connect()?;
        let hello = attach_conn::self_hello(attach_conn::test_terminal(80, 24));
        fdpass::send(&stream, &hello, slave)?;
        // the daemon has its own copy now
        // SAFETY: closing our copy of the slave.
        unsafe { libc::close(slave) };
        Ok(stream)
    }

    /// A pseudo-terminal's output, read as it comes until it closes or 1024
    /// bytes.
    fn read_screen(master: RawFd) -> Vec<u8> {
        let mut text = vec![0u8; 1024];
        let mut len = 0;
        while len < text.len() {
            // SAFETY: the rest of `text` is valid for writing.
            let n =
                unsafe { libc::read(master, text[len..].as_mut_ptr().cast(), text.len() - len) };
            if n <= 0 {
                break;
            }
            len += n as usize;
        }
        text.truncate(len);
        text
    }

    fn exists(file: impl AsRef<std::path::Path>) -> bool {
        fs::symlink_metadata(file).is_ok()
    }

    /// How a test's start goes: `nvim_command` for Neovim, answering within
    /// `nvim_timeout`, and Rex at `rex_url`. `on_ready` runs once the helpers'
    /// socket listens.
    struct TestStart<'a> {
        nvim_command: &'a [&'a str],
        nvim_timeout: Duration,
        rex_url: &'a str,
        on_ready: &'a mut dyn FnMut(),
    }

    /// Runs a start in `dir` that must fail, from a launcher in a
    /// pseudo-terminal. Checks the daemon's sockets are gone, its directory
    /// kept, and the launcher's attach was accepted, then failed; returns the
    /// error and the line the launcher was told, which it prints in the pane.
    fn failed_start(dir: &Claimed, start: TestStart<'_>) -> (Box<dyn std::error::Error>, String) {
        let address = dir.address();
        let sock = dir.sock_path();
        let nvim_sock = dir.nvim_sock();

        // the pane neorex was run in
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: valid out-pointers; no name, termios or size.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(0, rc);
        let reader = std::thread::spawn(move || read_screen(master));
        let mut launcher: Option<UnixStream> = None;
        let on_ready = start.on_ready;

        let err = serve(
            dir,
            &[] as &[&str],
            Config {
                nvim_command: start.nvim_command,
                nvim_timeout: start.nvim_timeout,
                ready: Box::new(|| {
                    on_ready();
                    launcher = Some(fake_launcher_ready(&address, slave)?);
                    Ok(())
                }),
                ..Config::new(start.rex_url, "unused")
            },
        )
        .unwrap_err();
        assert!(!exists(&sock));
        assert!(!exists(&nvim_sock));

        // nothing was drawn, so nothing was written: no mode was set, and no
        // query asked whose reply could reach the shell (SPI-970)
        let shown = reader.join().unwrap();
        assert!(shown.is_empty(), "{shown:?}");
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        let said = attach_conn::read_all_frames(&launcher.unwrap());
        let [Message::Accept(_), Message::Undo(undo), Message::End(z)] = &said[..] else {
            panic!("{said:?}")
        };
        assert_eq!(&crate::term::modes::undo(), undo);
        assert_eq!(Ending::Failed, z.as_ending());
        assert!(!z.cut);
        (err, z.message.clone())
    }

    /// Runs a start whose Neovim fails, with `nvim_command` standing in for
    /// Neovim, and checks the pane shows `reason`.
    fn expect_failed_start(nvim_command: &[&str], nvim_timeout: Duration, reason: &str) {
        let (_base, dir) = test_dir();
        let (err, shown) = failed_start(
            &dir,
            TestStart {
                nvim_command,
                nvim_timeout,
                rex_url: "ws://unused",
                on_ready: &mut || {},
            },
        );
        assert!(err.downcast_ref::<StartFailure>().is_some(), "{err}");
        assert!(shown.contains(reason), "{shown:?}");
    }

    /// Whether `nvim` is installed. Without it, a test skips (a panic under
    /// `CI`).
    fn have_nvim() -> bool {
        match Command::new("nvim")
            .arg("--version")
            .stdout(Stdio::null())
            .status()
        {
            Ok(_) => true,
            Err(_) => crate::nvim::nvim::nvim_missing::<()>().is_some(),
        }
    }

    #[test]
    fn a_daemons_sockets_are_only_its_users() {
        let Some(_nvim) = crate::nvim::nvim::spawn_for_test() else {
            return;
        };
        let (_base, dir) = test_dir();
        let sock = dir.sock_path();
        let nvim_sock = dir.nvim_sock();

        // Rex, for this test: once the daemon calls, which it does as Neovim
        // starts, waits up to 5 s for Neovim's socket to be made 0600, notes
        // its mode, then hangs up
        let rex_dir = Scratch::new();
        let rex_sock = rex_dir.file("rex.sock");
        let rex = UnixListener::bind(&rex_sock).unwrap();
        let rex = std::thread::spawn(move || {
            let (_hung_up, _) = rex.accept().unwrap();
            eventually(|| exists(&nvim_sock) && mode(&nvim_sock) == 0o600);
            mode(&nvim_sock)
        });

        let mut sock_mode = 0;
        let rex_url = format!("unix://{rex_sock}");
        let (err, shown) = failed_start(
            &dir,
            TestStart {
                nvim_command: &["nvim", "--clean"],
                nvim_timeout: NVIM_START_TIMEOUT,
                rex_url: &rex_url,
                on_ready: &mut || sock_mode = mode(&sock),
            },
        );
        assert!(
            shown.contains("couldn't connect to Rex"),
            "{err}: {shown:?}"
        );
        assert_eq!(0o600, sock_mode);
        assert_eq!(0o600, rex.join().unwrap());
    }

    #[test]
    fn a_start_whose_neovim_doesnt_answer_says_it_didnt_start_in_time() {
        expect_failed_start(
            &["sh", "-c", "exec sleep 30", "--"],
            Duration::from_secs(1),
            "Neovim didn't start in time",
        );
    }

    #[test]
    fn a_start_whose_neovim_exited_silently_says_so_with_its_status() {
        expect_failed_start(
            &["sh", "-c", "exit 3", "--"],
            Duration::from_secs(10),
            "Neovim exited at once (status 3) with no message",
        );
    }

    #[test]
    fn a_start_with_an_unknown_option_gives_neovims_reason() {
        if !have_nvim() {
            return;
        }
        expect_failed_start(
            &["nvim", "--fo"],
            Duration::from_secs(10),
            r#"Neovim exited at once (status 1): Unknown option argument: "--fo"; see "#,
        );
    }

    #[test]
    fn a_start_without_nvim_on_path_says_so() {
        let missing = format!("{NAME}-no-such-nvim");
        expect_failed_start(
            &[&missing],
            Duration::from_secs(10),
            "can't run nvim: not found on PATH",
        );
    }

    /// A clean Neovim started as the daemon starts it, in a new daemon
    /// directory, with its swap files in `<dir>/swap` and `args` after.
    struct TestNvim {
        /// Holds the daemon's directory, `d`, and `swap`.
        dir: String,
        claimed: Claimed,
        nvim: Option<Nvim>,
        child: Option<NvimChild>,
    }

    impl TestNvim {
        /// None (the test skips) without `nvim`.
        fn start(args: &[&str]) -> Option<TestNvim> {
            let dir = Scratch::new().keep();
            let swap = path(&dir, "swap");
            fs::create_dir(&swap).unwrap();
            let directory = format!("set directory={swap}//");
            let mut all = vec!["--cmd", &directory];
            all.extend_from_slice(args);
            let claimed = dir::claim(&dir::Base::at(dir.as_ref()), "d").unwrap();
            let started = start_nvim(&["nvim", "--clean"], &claimed, &all, NVIM_START_TIMEOUT);
            match started {
                Ok((child, mut nvim)) => {
                    // it answered nvim_get_api_info as soon as it could; this
                    // waits for the main loop, its files read
                    let id = nvim.request("nvim_eval", 1);
                    msgpack::write_str(nvim.writer(), "1");
                    nvim.flush().unwrap();
                    until_readable(&nvim);
                    crate::nvim::nvim::wait_response(&mut nvim, id);
                    Some(TestNvim {
                        dir,
                        claimed,
                        nvim: Some(nvim),
                        child: Some(child),
                    })
                }
                Err(StartFailure::CantRun(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    fs::remove_dir_all(&dir).unwrap();
                    crate::nvim::nvim::nvim_missing()
                }
                Err(e) => panic!("{e}"),
            }
        }

        fn nvim(&mut self) -> &mut Nvim {
            self.nvim.as_mut().unwrap()
        }

        /// How many swap files there are.
        fn swap_files(&self) -> usize {
            fs::read_dir(path(&self.dir, "swap")).unwrap().count()
        }

        /// Closes the channel and waits for Neovim to end.
        fn close(&mut self) {
            self.nvim = None;
            self.child = None;
        }

        /// Reads until Neovim closes the channel.
        fn until_closed(&mut self) {
            let nvim = self.nvim();
            loop {
                until_readable(nvim);
                if !nvim.read().unwrap() {
                    return;
                }
                while nvim.next().unwrap().is_some() {}
            }
        }
    }

    /// Waits up to 10 s for Neovim to say something, or panics.
    fn until_readable(nvim: &Nvim) {
        let mut fds = [libc::pollfd {
            fd: nvim.read_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: one valid pollfd.
        assert!(
            unsafe { libc::poll(fds.as_mut_ptr(), 1, 10_000) } > 0,
            "TestTimedOut"
        );
    }

    impl Drop for TestNvim {
        fn drop(&mut self) {
            self.close();
            if !self.dir.is_empty() {
                let _ = fs::remove_dir_all(&self.dir);
            }
        }
    }

    /// Waits up to 5 s for `done`.
    fn eventually(done: impl Fn() -> bool) -> bool {
        for _ in 0..500 {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Rex, for a test: greets the client at `rex_sock`, then reads all it's
    /// sent and answers nothing.
    fn silent_rex(rex_sock: &str) -> std::thread::JoinHandle<()> {
        use crate::rex::{client::PROTOCOL, web_socket};
        let listener = UnixListener::bind(rex_sock).unwrap();
        std::thread::spawn(move || {
            let (mut rex, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while web_socket::head_end(&head).is_none() {
                rex.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap();
            let key = head
                .lines()
                .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
                .unwrap();
            let accept = web_socket::accept_for(key.as_bytes().try_into().unwrap());
            let accept = std::str::from_utf8(&accept).unwrap();
            write!(
                rex,
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
                 Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\
                 Sec-WebSocket-Protocol: {PROTOCOL}\r\n\r\n"
            )
            .unwrap();
            let welcome = format!(
                r#"{{"type":"welcome","protocol":"{PROTOCOL}","limits":{{"max_message_bytes":65536}}}}"#
            );
            // a server frame is unmasked
            let mut frame = vec![0x81, 126];
            frame.extend_from_slice(&(welcome.len() as u16).to_be_bytes());
            frame.extend_from_slice(welcome.as_bytes());
            rex.write_all(&frame).unwrap();
            let mut buf = [0u8; 4096];
            while matches!(rex.read(&mut buf), Ok(n) if n > 0) {}
        })
    }

    #[test]
    fn a_plugins_detach_ends_the_daemon_as_neovim_exiting_keeping_its_log() {
        if !have_nvim() {
            return;
        }
        let (_base, dir) = test_dir();
        let address = dir.address();
        let sock = dir.sock_path();
        // as `run` makes it
        fs::write(dir.log(), "logged\n").unwrap();
        let rex_dir = Scratch::new();
        let rex_sock = rex_dir.file("rex.sock");
        let rex = silent_rex(&rex_sock);
        let swap = Scratch::new().keep();
        let file = temp_file("plugin-detach");

        // the pane neorex was run in, read until it closes
        let (master, slave) = crate::daemon::surface::open_pty(80, 24);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // SAFETY: `buf` is valid for its length.
            while unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
            // SAFETY: closing the pty's master.
            unsafe { libc::close(master) };
        });
        let mut launcher: Option<UnixStream> = None;

        // a plugin that detaches once windows.lua runs and the daemon is
        // attached: past windows.lua's refusal of a typed :detach
        let plugin = format!(
            "lua local t = vim.uv.new_timer() \
             t:start(50, 50, vim.schedule_wrap(function() \
               if _G.{NAME} and #vim.api.nvim_list_uis() > 0 then \
                 t:stop() vim.cmd('detach') \
               end \
             end))"
        );
        let directory = format!("set directory={swap}//");
        let args = [
            "--cmd",
            &directory,
            file.to_str().unwrap(),
            "-c",
            "normal! ichanged",
            "-c",
            &plugin,
        ];
        let rex_url = format!("unix://{rex_sock}");
        let started = Instant::now();
        let result = serve(
            &dir,
            &args,
            Config {
                nvim_command: &["nvim", "--clean"],
                ready: Box::new(|| {
                    launcher = Some(fake_launcher_ready(&address, slave)?);
                    Ok(())
                }),
                ..Config::new(&rex_url, "session:s")
            },
        );
        let took = started.elapsed();
        drop(launcher);
        reader.join().unwrap();
        rex.join().unwrap();
        // Neovim was sent SIGHUP: its changed buffer's swap is kept
        let swap_files = fs::read_dir(&swap).unwrap().count();
        let _ = fs::remove_dir_all(&swap);
        let _ = fs::remove_file(&file);
        assert!(result.is_ok(), "{result:?}");
        assert!(took < Duration::from_secs(10), "{took:?}");
        assert!(!exists(&sock), "the daemon's socket is left");
        // a clean end keeps the log
        assert_eq!("logged\n", fs::read_to_string(dir.log()).unwrap());
        assert_eq!(1, swap_files);
    }

    #[test]
    fn a_config_slower_than_a_moment_still_starts() {
        // Neovim answers only once its config has run
        let Some(mut nvim) = TestNvim::start(&["--cmd", "lua vim.uv.sleep(1500)"]) else {
            return;
        };
        let id = nvim.nvim().request("nvim_eval", 1);
        msgpack::write_str(nvim.nvim().writer(), "1+2");
        nvim.nvim().flush().unwrap();
        let frame = crate::nvim::nvim::wait_response(nvim.nvim(), id);
        let rpc::Message::Response {
            err, mut result, ..
        } = frame.message().unwrap()
        else {
            unreachable!()
        };
        assert_eq!(None, err);
        assert_eq!(Ok(3), rpc::expect_int(&mut result));
    }

    #[test]
    fn closing_the_channel_runs_vimleave_and_removes_an_unchanged_buffers_swap() {
        let marker = temp_file("vimleave");
        let file = temp_file("unchanged");
        fs::write(&file, "text\n").unwrap();
        let vimleave = format!(
            "autocmd VimLeave * call writefile(['left'], '{}')",
            marker.display()
        );
        let Some(mut nvim) = TestNvim::start(&["--cmd", &vimleave, file.to_str().unwrap()]) else {
            return;
        };
        assert_eq!(1, nvim.swap_files());
        nvim.close();
        let (left, swap) = (marker.exists(), nvim.swap_files());
        let _ = fs::remove_file(&marker);
        let _ = fs::remove_file(&file);
        assert!(left, "no VimLeave");
        assert_eq!(0, swap);
    }

    #[test]
    fn a_plugins_detach_ends_neovim_keeping_a_changed_buffers_swap() {
        let file = temp_file("detached");
        let Some(mut nvim) = TestNvim::start(&[file.to_str().unwrap(), "-c", "normal! ichanged"])
        else {
            return;
        };
        // `:detach` leaves a UI
        nvim.nvim()
            .ui_attach(40, 10, crate::nvim::nvim::UiOptions::default())
            .unwrap();
        nvim.nvim().exec_lua("vim.cmd('detach')", 0);
        nvim.nvim().flush().unwrap();
        nvim.until_closed();
        // as the daemon does once Neovim has closed the channel
        let started = Instant::now();
        let mut child = nvim.child.take().unwrap();
        child.closed_by_nvim();
        // ended by the SIGHUP, not killed after the grace
        assert!(child.wait_for_exit(Duration::from_secs(2)), "still running");
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(child);
        let _ = fs::remove_file(&file);
        assert_eq!(1, nvim.swap_files());
    }

    #[test]
    fn neovims_terminals_see_the_daemons_address() {
        let Some(mut nvim) = TestNvim::start(&[]) else {
            return;
        };
        let out = path(&nvim.dir, "out");
        let cmd = format!("terminal echo \"${}\" > {out}", name::upper());
        let id = nvim.nvim().request("nvim_command", 1);
        msgpack::write_str(nvim.nvim().writer(), &cmd);
        nvim.nvim().flush().unwrap();
        let frame = crate::nvim::nvim::wait_response(nvim.nvim(), id);
        let rpc::Message::Response { err, .. } = frame.message().unwrap() else {
            unreachable!()
        };
        assert_eq!(None, err.map(String::from_utf8_lossy));
        assert!(eventually(
            || fs::read_to_string(&out).is_ok_and(|s| s.ends_with('\n'))
        ));
        let seen = fs::read_to_string(&out).unwrap();
        assert_eq!(nvim.claimed.address().to_string(), seen.trim_end());
    }

    /// The environment variable that makes `sigkill_helper` run, naming the
    /// file it edits.
    fn sigkill_helper_var() -> String {
        format!("{}_TEST_SIGKILL", name::upper())
    }

    /// For `a_sigkilled_daemons_neovim_exits_keeping_a_changed_buffers_swap`,
    /// in a process of its own: starts Neovim with a changed buffer, says
    /// `ready <Neovim's pid> <swap directory>`, and waits to be killed.
    #[test]
    #[ignore = "a helper process, run by another test"]
    fn sigkill_helper() {
        let Some(file) = std::env::var_os(sigkill_helper_var()) else {
            return;
        };
        let file = file.to_str().unwrap();
        let mut nvim = TestNvim::start(&[file, "-c", "normal! ichanged"]).unwrap();
        let pid = nvim.child.as_ref().unwrap().child.id();
        println!("ready {pid} {}", path(&nvim.dir, "swap"));
        std::io::stdout().flush().unwrap();
        // the directory is the test's to remove
        nvim.dir = String::new();
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    #[test]
    fn a_sigkilled_daemons_neovim_exits_keeping_a_changed_buffers_swap() {
        if !have_nvim() {
            return;
        }
        let file = temp_file("killed");
        let mut daemon = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon::serve::tests::sigkill_helper",
                "--ignored",
                "--nocapture",
            ])
            .env(sigkill_helper_var(), &file)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = daemon.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        // what it said before `ready`, for a failure
        let mut said = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let ready = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                // the test harness may have begun the line
                Ok(line) => match line.split_once("ready ") {
                    Some((_, ready)) => break ready.to_string(),
                    None => said.push(line),
                },
                Err(e) => {
                    let status = daemon.try_wait();
                    let _ = daemon.kill();
                    let _ = daemon.wait();
                    panic!("the helper's Neovim didn't start ({e}, {status:?}): {said:?}");
                }
            }
        };
        let (pid, swap) = ready.split_once(' ').unwrap();
        let pid: libc::pid_t = pid.parse().unwrap();
        daemon.kill().unwrap();
        daemon.wait().unwrap();

        // SAFETY: probing a pid.
        let gone = eventually(|| unsafe { libc::kill(pid, 0) } == -1);
        if !gone {
            // SAFETY: killing the Neovim the helper started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let swap_files = fs::read_dir(swap).unwrap().count();
        let dir = std::path::Path::new(swap).parent().unwrap();
        fs::remove_dir_all(dir).unwrap();
        let _ = fs::remove_file(&file);
        assert!(gone, "Neovim outlived its daemon");
        assert_eq!(1, swap_files);
    }

    /// Neovim's answer to `nvim_get_api_info`, as recorded from `version`.
    fn api_info(version: &str) -> Frame {
        let bytes: &[u8] = match version {
            "0.11.2" => include_bytes!("../nvim/testdata/nvim_get_api_info_0.11.2.mpack"),
            "0.12.6" => include_bytes!("../nvim/testdata/nvim_get_api_info_0.12.6.mpack"),
            "0.13.0" => include_bytes!("../nvim/testdata/nvim_get_api_info.mpack"),
            _ => unreachable!(),
        };
        Frame(bytes.to_vec())
    }

    fn reason(failure: StartFailure) -> String {
        failure_message(&failure, "/tmp/x/log")
    }

    #[test]
    fn a_neovim_older_than_0_12_is_refused_with_its_version() {
        let failure = check_version(&api_info("0.11.2")).unwrap_err();
        assert_eq!(
            "Neovim 0.12 or later is needed; nvim is 0.11.2; see /tmp/x/log",
            reason(failure)
        );
        for newer in ["0.12.6", "0.13.0"] {
            assert!(check_version(&api_info(newer)).is_ok(), "{newer}");
        }
        assert_eq!(
            Ok(Version {
                major: 0,
                minor: 12,
                patch: 6
            }),
            api_info_version(&api_info("0.12.6"))
        );
    }

    #[test]
    fn the_launcher_is_told_where_the_log_is_once_there_is_one() {
        assert_eq!("run it in a Rex pane", start_error(&Error::NotInRex, None));
        assert_eq!(
            "work-2 didn't start: run it in a Rex pane (log: /tmp/x/work-2/log)",
            start_error(
                &Error::NotInRex,
                Some(("work-2".into(), "/tmp/x/work-2/log".into()))
            )
        );
    }

    #[test]
    fn failed_start_reasons() {
        // as `nvim --fo` writes it
        let stderr = b"nvim: Unknown option argument: \"--fo\"\nMore info with \"nvim -h\"\n";
        let cases = [
            (
                StartFailure::Exited {
                    exit: Some(NvimExit::Status(1)),
                    message: start_reason(stderr),
                },
                r#"Neovim exited at once (status 1): Unknown option argument: "--fo""#,
            ),
            (
                StartFailure::Exited {
                    exit: Some(NvimExit::Status(1)),
                    message: start_reason(b""),
                },
                "Neovim exited at once (status 1) with no message",
            ),
            (
                StartFailure::CantRun(std::io::ErrorKind::NotFound.into()),
                "can't run nvim: not found on PATH",
            ),
        ];
        for (failure, why) in cases {
            assert_eq!(format!("{why}; see /tmp/x/log"), reason(failure));
        }
        // the hint alone is no reason
        assert_eq!(None, start_reason(b"More info with \"nvim -h\"\n"));
    }
}
