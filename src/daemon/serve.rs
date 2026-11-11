//! `neorex daemon DIR [nvim args]`: the background half of `neorex`. Takes
//! the launching pane's terminal, runs Neovim with `--listen`, and shows
//! each of its windows in a Rex pane (see `Daemon`). Exits when Neovim does
//! or the last pane goes away.
//!
//! Teardown is by values whose `Drop` does it, declared in the order they
//! must end: the daemon (its panes, then the loop, Rex and Neovim), Neovim's
//! process, the launcher's pane if the daemon never took it, the socket files
//! and the daemon's directory, the helpers' socket.
//!
//! Every file of a daemon is in its own directory (`private_dir`), which the
//! launcher makes and passes as DIR.

use std::ffi::{CString, OsStr};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::daemon::{Daemon, StopReason, Token};
use super::event_loop::Loop;
use super::nvim_session::NvimSession;
use super::pane::{self, Pane, RexBlock};
use super::pane_helper;
use super::surface::Surface;
use crate::log::{self, log_info, log_warn};
use crate::name::NAME;
use crate::nvim::nvim::{self, Nvim};
use crate::rex::client::Client;
use crate::rex::session::Session as RexSession;

/// `serve` is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the daemon couldn't start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NameTooLong,
    /// Something answers on one of the daemon's sockets: another daemon, or
    /// its Neovim.
    AlreadyRunning,
    NotInRex,
    LauncherGone,
    NoPane,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for Error {}

/// Longest path a Unix socket takes, less one for the terminator: 103 on macOS,
/// 107 on Linux.
pub(crate) const MAX_PATH: usize = {
    // SAFETY: an all-zero sockaddr_un is valid, and only its size is read.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    std::mem::size_of_val(&addr.sun_path) - 1
};

/// The longest name of a file in a daemon's directory: Neovim's socket.
const LONGEST_NAME: &str = "nvim.sock";

/// A new directory for one daemon's files, which only this user can enter:
/// `neorex-PID.XXXXXX` in the runtime directory (see `parent_dir`). Made
/// with `mkdtemp`, so it's never another daemon's, even when PIDs repeat.
pub(crate) fn private_dir() -> Result<String> {
    let xdg = env("XDG_RUNTIME_DIR");
    let tmpdir = env("TMPDIR");
    private_dir_in(parent_dir(xdg.as_deref(), tmpdir.as_deref()))
}

/// Where daemons' directories go: `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else
/// `/tmp`, each only when set to an absolute path.
fn parent_dir<'a>(xdg_runtime_dir: Option<&'a str>, tmpdir: Option<&'a str>) -> &'a str {
    [xdg_runtime_dir, tmpdir]
        .into_iter()
        .flatten()
        .find(|dir| dir.starts_with('/'))
        .unwrap_or("/tmp")
}

/// `private_dir`, in `parent`. Refuses a `parent` so deep that a socket in the
/// new directory couldn't be bound, before making anything.
fn private_dir_in(parent: &str) -> Result<String> {
    let parent = parent.trim_end_matches('/');
    // SAFETY: getpid can't fail.
    let template = format!("{parent}/{NAME}-{}.XXXXXX", unsafe { libc::getpid() });
    if path(&template, LONGEST_NAME).len() > MAX_PATH {
        return Err(Error::NameTooLong.into());
    }
    let mut template = CString::new(template)?.into_bytes_with_nul();
    // SAFETY: `template` is a writable, NUL-terminated buffer that mkdtemp
    // fills in; it makes the directory with mode 0700.
    if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    template.pop();
    Ok(String::from_utf8(template)?)
}

/// Makes the log at `path`, for the daemon's stderr: a new file only this user
/// can read, never one that's there already or a link.
pub(crate) fn create_log(path: &str) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Makes the socket at `path` this user's alone: mode 0600. Until then, its
/// directory, 0700, keeps others out.
fn owner_only(path: &str) -> std::io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

/// The file `name` in the daemon's directory `dir`.
pub(crate) fn path(dir: &str, name: &str) -> String {
    format!("{dir}/{name}")
}

/// What `serve` runs against. `run` uses the defaults (`Config::new`); tests
/// swap the seams.
pub(crate) struct Config<'a> {
    pub(crate) rex_url: &'a str,
    pub(crate) session_id: &'a str,
    /// Starts Neovim: its arguments follow.
    pub(crate) nvim_command: &'a [&'a str],
    /// Connects to Neovim's socket, waiting for it to appear.
    pub(crate) connect: fn(&str) -> nvim::Result<Nvim>,
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
            connect: connect_nvim,
            ready: Box::new(tell_launcher),
        }
    }
}

fn connect_nvim(nvim_sock: &str) -> nvim::Result<Nvim> {
    Nvim::connect_retry(nvim_sock)
}

pub fn run<S: AsRef<OsStr>>(dir: &str, nvim_args: &[S]) -> Result<()> {
    // out of the pane's session, so closing the pane doesn't signal us
    // SAFETY: setsid has no memory effects.
    unsafe { libc::setsid() };
    log::init_daemon();
    let rex_url = env("REX_SERVER").ok_or(Error::NotInRex)?;
    let session_id = env("REX_SESSION").ok_or(Error::NotInRex)?;
    serve(dir, nvim_args, Config::new(&rex_url, &session_id))
}

/// One byte on stdout says we're listening; then stdout goes nowhere.
fn tell_launcher() -> Result<()> {
    // SAFETY: writing one byte from a static buffer.
    if unsafe { libc::write(libc::STDOUT_FILENO, b"r".as_ptr().cast(), 1) } != 1 {
        return Err(Error::LauncherGone.into());
    }
    // SAFETY: plain fd syscalls on a path literal.
    unsafe {
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::close(devnull);
        }
    }
    Ok(())
}

/// Removes the socket files left over, in order, as it drops; then their
/// directory, unless a log is still in it.
struct RemoveFiles {
    files: [String; 2],
    dir: String,
}

impl Drop for RemoveFiles {
    fn drop(&mut self) {
        for f in &self.files {
            let _ = fs::remove_file(f);
        }
        let _ = fs::remove_dir(&self.dir);
    }
}

/// Neovim's process. As it drops, after the terminals are restored: SIGHUP
/// makes Neovim save swap files and exit, and it's waited for. Not one that
/// has already ended, such as one that quit by itself and ended within the
/// grace [`NvimChild::wait_for_exit`] gives it.
///
/// A Neovim that doesn't end within `grace` of the SIGHUP is killed: one
/// caught in a `system()` call, such as a clipboard read from an autocommand,
/// can hang on its way out, and would keep the daemon waiting for good.
struct NvimChild {
    child: Child,
    grace: Duration,
}

/// How long Neovim has to exit after SIGHUP before it's killed.
const NVIM_EXIT_GRACE: Duration = Duration::from_secs(3);

impl NvimChild {
    fn new(child: Child) -> NvimChild {
        NvimChild {
            child,
            grace: NVIM_EXIT_GRACE,
        }
    }

    /// Waits up to the grace for Neovim to end; whether it did. One that
    /// closed its socket is exiting by itself: it closes the socket before it
    /// deletes its swap files, so a SIGHUP in between keeps them.
    fn wait_for_exit(&mut self) -> bool {
        let deadline = Instant::now() + self.grace;
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
}

impl Drop for NvimChild {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: signalling our own child, which hasn't been reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGHUP) };
        if self.wait_for_exit() {
            return;
        }
        log_warn!("{NAME} daemon: Neovim didn't exit after SIGHUP; killing it");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(crate) fn serve<S: AsRef<OsStr>>(
    dir: &str,
    nvim_args: &[S],
    mut config: Config<'_>,
) -> Result<()> {
    let sock = path(dir, "sock");
    let nvim_sock = path(dir, "nvim.sock");
    let log = path(dir, "log");
    // resolved, symlinks and all: the `nrex` link shouldn't end up in every
    // pane's command
    let exe = std::env::current_exe()?
        .canonicalize()?
        .to_string_lossy()
        .into_owned();

    // another daemon's, or its Neovim's, never to be taken over
    for live in [&sock, &nvim_sock] {
        if UnixStream::connect(live).is_ok() {
            return Err(Error::AlreadyRunning.into());
        }
    }
    // open until we exit: every new pane's helper connects here. Never over a
    // file that's there: the directory is new
    let server = UnixListener::bind(&sock)?;
    owner_only(&sock)?;
    // the socket's file, then, last of all, after Neovim has had its chance to
    // remove its own: a Neovim that was stuck or killed leaves it behind
    let _files = RemoveFiles {
        files: [nvim_sock.clone(), sock.clone()],
        dir: dir.to_string(),
    };

    (config.ready)()?;

    let rex_block: Option<RexBlock> = match env("REX_BLOCK") {
        Some(b) => match pane::rex_block(b.as_bytes()) {
            Ok(block) => Some(block),
            Err(_) => {
                log_warn!("{NAME} daemon: REX_BLOCK isn't a block id: {b}");
                None
            }
        },
        None => None,
    };
    let mut startup = Startup {
        log: &log,
        launch: Some(Pane::startup(rex_block, take_pane(&server)?)),
    };

    // before the user's config, so it can tell it runs under neorex
    let spawned = Command::new(config.nvim_command[0])
        .args(&config.nvim_command[1..])
        .args([
            "--headless",
            "--listen",
            &nvim_sock,
            "--cmd",
            &format!("let g:{NAME} = 1"),
        ])
        .args(nvim_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // its stderr is ours: the log
        .spawn();
    let mut child = match spawned {
        Ok(child) => NvimChild::new(child),
        Err(err) => return Err(startup.give_up(StartStep::StartNvim, Box::new(err), None)),
    };

    // Neovim makes its socket under its umask, which its files need
    let connected = (config.connect)(&nvim_sock).and_then(|nvim| {
        owner_only(&nvim_sock)?;
        Ok(nvim)
    });
    let nvim = match connected {
        Ok(nvim) => nvim,
        Err(err) => {
            return Err(startup.give_up(
                StartStep::ConnectNvim,
                Box::new(err),
                Some(&mut child.child),
            ));
        }
    };
    let rex = match Client::connect(config.rex_url, NAME) {
        Ok(rex) => rex,
        Err(err) => {
            return Err(startup.give_up(
                StartStep::ConnectRex,
                Box::new(err),
                Some(&mut child.child),
            ));
        }
    };

    let mut event_loop = Loop::init()?;
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        event_loop.add_signal(sig, Token::Signal as usize)?;
    }
    event_loop.add_read(nvim.read_fd(), Token::NvimSocket as usize)?;
    event_loop.add_read(server.as_raw_fd(), Token::HelperListener as usize)?;
    event_loop.add_read(rex.fd(), Token::RexSocket as usize)?;

    // dropped first: terminals restored and panes hung up before anything else ends
    let mut d = Daemon::new(
        event_loop,
        NvimSession::new(nvim),
        RexSession::new(rex, config.session_id.to_string()),
        exe,
        sock.clone(),
    );
    d.start(startup.release())?;

    let stop_reason = d.run(&server)?;
    log_info!("{NAME} daemon: {}", stop_reason.tag_name());
    if stop_reason == StopReason::NvimExited {
        // panes first: Neovim may take a while to finish exiting
        drop(d);
        // one still running is sent SIGHUP as it drops
        child.wait_for_exit();
    }
    // a clean end needs no log, unless it was asked for in full
    if !log::tracing() {
        let _ = fs::remove_file(&log);
    }
    Ok(())
}

/// What the daemon was doing when it gave up before showing any window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartStep {
    StartNvim,
    ConnectNvim,
    ConnectRex,
}

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
            launch.deinit();
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

    /// Restores the pane and tells it why we're stopping, then returns
    /// `err`, which `main` prints to the log. `nvim_child` is Neovim, once
    /// it's started.
    fn give_up(
        &mut self,
        step: StartStep,
        err: Box<dyn std::error::Error>,
        nvim_child: Option<&mut Child>,
    ) -> Box<dyn std::error::Error> {
        let message = failure_message(step, err.as_ref(), nvim_exit(nvim_child), self.log);
        let launch = self.release();
        launch.deinit_with_message(&message);
        err
    }
}

/// How Neovim ended, if it has: a Neovim that exited never made its
/// socket. Doesn't wait. A Neovim found ended is reaped.
fn nvim_exit(child: Option<&mut Child>) -> Option<NvimExit> {
    let status = child?.try_wait().ok()??;
    if let Some(code) = status.code() {
        return Some(NvimExit::Status(code as u8));
    }
    status.signal().map(|sig| NvimExit::Signal(sig as u32))
}

/// A line for the user: what failed, and where the log is. At most 256 bytes
/// plus a path.
fn failure_message(
    step: StartStep,
    err: &(dyn std::error::Error + 'static),
    nvim_exit: Option<NvimExit>,
    log: &str,
) -> Vec<u8> {
    let mut w = format!("{NAME}: ");
    match step {
        StartStep::StartNvim => {
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                w.push_str("nvim not found");
            } else {
                w.push_str(&format!("couldn't start nvim: {err}"));
            }
        }
        StartStep::ConnectNvim => match nvim_exit {
            Some(NvimExit::Status(n)) => {
                w.push_str(&format!("Neovim exited (status {n}) before it was ready"))
            }
            Some(NvimExit::Signal(n)) => w.push_str(&format!(
                "Neovim was killed (signal {n}) before it was ready"
            )),
            None => {
                if matches!(
                    err.downcast_ref::<nvim::Error>(),
                    Some(nvim::Error::Timeout)
                ) {
                    w.push_str("Neovim didn't start in time");
                } else {
                    w.push_str(&format!("couldn't connect to Neovim: {err}"));
                }
            }
        },
        StartStep::ConnectRex => w.push_str(&format!("couldn't connect to Rex: {err}")),
    }
    w.push_str(&format!("; see {log}\r\n"));
    let mut bytes = w.into_bytes();
    bytes.truncate(256 + MAX_PATH);
    bytes
}

/// The environment variable `name`, if set (and Unicode).
fn env(name: &str) -> Option<String> {
    std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
}

/// Waits up to 5 s for the launcher's helper and takes its terminal.
fn take_pane(server: &UnixListener) -> Result<Box<Surface>> {
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
    let hello = pane_helper::accept_hello(server)?;
    // (a failing `Surface::create` closes the hello's socket and terminal
    // itself)
    Ok(Surface::create(hello.stream, hello.tty)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::fdpass;
    use std::os::fd::RawFd;

    #[test]
    fn a_neovim_that_wont_exit_on_sighup_is_killed_after_the_grace() {
        // ignores SIGHUP, as a Neovim stuck on its way out does
        let child = Command::new("sh")
            .args(["-c", "trap '' HUP; echo ready; while :; do sleep 0.1; done"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let mut child = NvimChild {
            child,
            grace: Duration::from_millis(200),
        };
        // the trap is set once it says so
        let mut ready = [0u8; 6];
        std::io::Read::read_exact(child.child.stdout.as_mut().unwrap(), &mut ready).unwrap();
        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(5));
        // reaped: no such process
        assert_eq!(-1, unsafe { libc::kill(pid, 0) });
    }

    #[test]
    fn a_neovim_exiting_by_itself_is_waited_for_not_sent_sighup() {
        // Neovim closes its socket before it deletes its swap files: a SIGHUP
        // in between keeps them. This one notes a SIGHUP, then ends itself.
        let marker = std::env::temp_dir().join(format!("{NAME}-hup-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let script = format!(
            "trap 'echo hup > {}; exit 1' HUP; echo ready; sleep 0.3",
            marker.display()
        );
        let child = Command::new("sh")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = NvimChild {
            child,
            grace: Duration::from_secs(3),
        };
        let mut ready = [0u8; 6];
        std::io::Read::read_exact(child.child.stdout.as_mut().unwrap(), &mut ready).unwrap();
        child.wait_for_exit();
        drop(child);
        let sent_sighup = marker.exists();
        let _ = fs::remove_file(&marker);
        assert!(
            !sent_sighup,
            "Neovim was sent SIGHUP while exiting by itself"
        );
    }

    #[test]
    fn a_neovim_that_exits_on_sighup_is_waited_for_not_killed() {
        let child = Command::new("sleep").arg("60").spawn().unwrap();
        let child = NvimChild {
            child,
            grace: Duration::from_secs(30),
        };
        let pid = child.child.id() as libc::pid_t;
        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(-1, unsafe { libc::kill(pid, 0) });
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

    fn mode(file: &str) -> u32 {
        fs::symlink_metadata(file).unwrap().permissions().mode() & 0o7777
    }

    #[test]
    fn each_daemon_gets_a_new_directory_only_its_user_can_enter() {
        let parent = std::env::temp_dir();
        let parent = parent.to_str().unwrap();
        let a = private_dir_in(parent).unwrap();
        let b = private_dir_in(parent).unwrap();
        assert_ne!(a, b);
        for dir in [&a, &b] {
            assert!(dir.starts_with(parent.trim_end_matches('/')));
            assert!(fs::symlink_metadata(dir).unwrap().is_dir());
            assert_eq!(0o700, mode(dir));
            assert!(path(dir, "nvim.sock").len() <= MAX_PATH);
            fs::remove_dir(dir).unwrap();
        }
    }

    #[test]
    fn the_log_is_made_new_and_only_its_user_can_read_it() {
        let dir = private_dir().unwrap();
        let log = path(&dir, "log");
        create_log(&log).unwrap();
        assert_eq!(0o600, mode(&log));
        // never one that's there already
        let err = create_log(&log).unwrap_err();
        assert_eq!(std::io::ErrorKind::AlreadyExists, err.kind());
        fs::remove_file(&log).unwrap();

        // nor a link: its target is left alone
        let target = path(&dir, "target");
        fs::write(&target, "kept").unwrap();
        std::os::unix::fs::symlink(&target, &log).unwrap();
        assert!(create_log(&log).is_err());
        assert_eq!("kept", fs::read_to_string(&target).unwrap());
        fs::remove_file(&log).unwrap();
        fs::remove_file(&target).unwrap();
        fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn a_directory_too_deep_for_a_socket_is_refused_before_it_is_made() {
        let parent = format!("/tmp/{}", "x".repeat(MAX_PATH));
        let err = private_dir_in(&parent).unwrap_err();
        assert_eq!(Some(&Error::NameTooLong), err.downcast_ref::<Error>());
    }

    #[test]
    fn the_runtime_directory_is_preferred_then_tmpdir_then_tmp() {
        assert_eq!(
            "/run/user/1",
            parent_dir(Some("/run/user/1"), Some("/var/t"))
        );
        assert_eq!("/var/t", parent_dir(None, Some("/var/t")));
        assert_eq!("/var/t", parent_dir(Some(""), Some("/var/t")));
        assert_eq!("/var/t", parent_dir(Some("run"), Some("/var/t")));
        assert_eq!("/tmp", parent_dir(None, None));
        assert_eq!("/tmp", parent_dir(None, Some("")));
        assert_eq!("/tmp", parent_dir(None, Some("tmp")));
    }

    #[test]
    fn a_daemon_wont_start_over_a_socket_that_answers() {
        for name in ["sock", "nvim.sock"] {
            let dir = private_dir().unwrap();
            let live = path(&dir, name);
            let other = UnixListener::bind(&live).unwrap();
            let mut ready = false;

            let err = serve(
                &dir,
                &[] as &[&str],
                Config {
                    ready: Box::new(|| {
                        ready = true;
                        Ok(())
                    }),
                    ..Config::new("ws://unused", "unused")
                },
            )
            .unwrap_err();
            assert_eq!(
                Some(&Error::AlreadyRunning),
                err.downcast_ref::<Error>(),
                "{name}: {err}"
            );
            assert!(!ready, "{name}");
            // the other one's socket is left to it, still answering
            UnixStream::connect(&live).unwrap();
            assert!(other.accept().is_ok(), "{name}");

            drop(other);
            fs::remove_file(&live).unwrap();
            fs::remove_dir(&dir).unwrap();
        }
    }

    /// The launcher, for a test: connects to the daemon's socket as soon as it
    /// listens, and hands over a pseudo-terminal's slave as its pane.
    fn fake_launcher_ready(sock: &str, slave: RawFd) -> Result<UnixStream> {
        let stream = UnixStream::connect(sock)?;
        let mut hello = [0u8; 16];
        fdpass::send(&stream, pane_helper::hello_bytes(&mut hello, None)?, slave)?;
        // the daemon has its own copy now
        // SAFETY: closing our copy of the slave.
        unsafe { libc::close(slave) };
        Ok(stream)
    }

    /// A pseudo-terminal's output, read as it comes until it closes or 1024
    /// bytes: restoring a terminal waits for its output to be taken.
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

    /// Neovim that never answers, leaving its socket file behind. Takes a moment,
    /// as the real one does: enough for a Neovim that is quitting to quit.
    fn connect_never(nvim_sock: &str) -> nvim::Result<Nvim> {
        std::thread::sleep(Duration::from_millis(300));
        fs::File::create(nvim_sock)?;
        Err(nvim::Error::Timeout)
    }

    fn exists(file: &str) -> bool {
        fs::symlink_metadata(file).is_ok()
    }

    /// How a test's start goes: `nvim_command` and `connect` for Neovim, Rex
    /// at `rex_url`. `on_ready` runs once the helpers' socket listens.
    struct TestStart<'a> {
        nvim_command: &'a [&'a str],
        connect: fn(&str) -> nvim::Result<Nvim>,
        rex_url: &'a str,
        on_ready: &'a mut dyn FnMut(),
    }

    /// Runs a start in `dir` that must fail, from a launcher in a
    /// pseudo-terminal. Checks the daemon's files and directory are gone and
    /// the launcher's helper was hung up on; returns the error and what the
    /// pane showed.
    fn failed_start(dir: &str, start: TestStart<'_>) -> (Box<dyn std::error::Error>, String) {
        let sock = path(dir, "sock");
        let nvim_sock = path(dir, "nvim.sock");

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
                connect: start.connect,
                ready: Box::new(|| {
                    on_ready();
                    launcher = Some(fake_launcher_ready(&sock, slave)?);
                    Ok(())
                }),
                ..Config::new(start.rex_url, "unused")
            },
        )
        .unwrap_err();
        assert!(!exists(&sock));
        assert!(!exists(&nvim_sock));
        assert!(!exists(dir));

        let shown = reader.join().unwrap();
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        let launcher = launcher.unwrap();
        let mut hangup = [0u8; 1];
        // SAFETY: `hangup` is valid for one byte.
        let n = unsafe { libc::read(launcher.as_raw_fd(), hangup.as_mut_ptr().cast(), 1) };
        assert_eq!(0, n);
        (err, String::from_utf8_lossy(&shown).into_owned())
    }

    /// Runs a start whose Neovim connect fails, with `nvim_command` standing in
    /// for Neovim, and checks the pane shows `reason`.
    fn expect_failed_start(nvim_command: &[&str], reason: &str) {
        let dir = private_dir().unwrap();
        let (err, shown) = failed_start(
            &dir,
            TestStart {
                nvim_command,
                connect: connect_never,
                rex_url: "ws://unused",
                on_ready: &mut || {},
            },
        );
        assert!(
            matches!(
                err.downcast_ref::<nvim::Error>(),
                Some(nvim::Error::Timeout)
            ),
            "{err}"
        );
        assert!(shown.contains(reason), "{shown:?}");
    }

    #[test]
    fn a_daemons_sockets_are_only_its_users() {
        let Some(_nvim) = crate::nvim::nvim::spawn_for_test() else {
            return;
        };
        let dir = private_dir().unwrap();
        let sock = path(&dir, "sock");
        let nvim_sock = path(&dir, "nvim.sock");

        // Rex, for this test: notes Neovim's socket's mode once the daemon is
        // connected to Neovim and calls, then hangs up
        let rex_dir = private_dir().unwrap();
        let rex_sock = path(&rex_dir, "rex.sock");
        let rex = UnixListener::bind(&rex_sock).unwrap();
        let rex = std::thread::spawn(move || {
            let (_hung_up, _) = rex.accept().unwrap();
            mode(&nvim_sock)
        });

        let mut sock_mode = 0;
        let rex_url = format!("unix://{rex_sock}");
        let (err, shown) = failed_start(
            &dir,
            TestStart {
                nvim_command: &["nvim", "--clean"],
                connect: connect_nvim,
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

        fs::remove_file(&rex_sock).unwrap();
        fs::remove_dir(&rex_dir).unwrap();
    }

    #[test]
    fn a_start_whose_neovim_is_still_running_says_it_didnt_start_in_time() {
        expect_failed_start(
            &["sh", "-c", "exec sleep 30", "--"],
            "Neovim didn't start in time",
        );
    }

    #[test]
    fn a_start_whose_neovim_exited_says_so_with_its_status() {
        expect_failed_start(
            &["sh", "-c", "exit 3", "--"],
            "Neovim exited (status 3) before it was ready",
        );
    }
}
