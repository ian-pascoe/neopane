//! `neorex daemon BASE [nvim args]`: the background half of `neorex`. Takes
//! the launching pane's terminal, runs Neovim with `--listen`, and shows
//! each of its windows in a Rex pane (see `Daemon`). Exits when Neovim does
//! or the last pane goes away.
//!
//! Teardown is by values whose `Drop` does it, declared in the order they
//! must end: the daemon (its panes, then the loop, Rex and Neovim), Neovim's
//! process, the launcher's pane if the daemon never took it, the socket files,
//! the helpers' socket.

use std::ffi::OsStr;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use super::daemon::{Daemon, Token};
use super::event_loop::Loop;
use super::nvim_session::NvimSession;
use super::pane::{self, Pane, RexBlock};
use super::pane_helper;
use super::surface::Surface;
use crate::nvim::nvim::{self, Nvim};
use crate::rex::client::Client;
use crate::rex::session::Session as RexSession;

/// `serve` is a top layer: any error will do.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the daemon couldn't start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NameTooLong,
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

/// Longest path a Unix socket takes on macOS, less one for the terminator.
pub const MAX_PATH: usize = 103;

/// `$TMPDIR/neorex-PID`, which every daemon file is named from.
pub fn base_path() -> Result<String> {
    let tmp = env("TMPDIR").unwrap_or_else(|| "/tmp".to_string());
    let dir = tmp.trim_end_matches('/');
    // SAFETY: getpid can't fail.
    let base = format!("{dir}/neorex-{}", unsafe { libc::getpid() });
    // room for the longest suffix
    if base.len() + ".nvim.sock".len() > MAX_PATH {
        return Err(Error::NameTooLong.into());
    }
    Ok(base)
}

/// `base` plus `suffix`, such as `.sock`.
pub fn path(base: &str, suffix: &str) -> String {
    format!("{base}{suffix}")
}

/// What `serve` runs against. `run` uses the defaults (`Config::new`); tests
/// swap the seams.
pub struct Config<'a> {
    pub rex_url: &'a str,
    pub session_id: &'a str,
    /// Starts Neovim: its arguments follow.
    pub nvim_command: &'a [&'a str],
    /// Connects to Neovim's socket, waiting for it to appear.
    pub connect: fn(&str) -> nvim::Result<Nvim>,
    /// Called once the helpers' socket listens: the launcher may connect.
    pub ready: Box<dyn FnMut() -> Result<()> + 'a>,
}

impl<'a> Config<'a> {
    /// The config `run` uses, for Rex at `rex_url` and session `session_id`.
    pub fn new(rex_url: &'a str, session_id: &'a str) -> Config<'a> {
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

pub fn run<S: AsRef<OsStr>>(base: &str, nvim_args: &[S]) -> Result<()> {
    // out of the pane's session, so closing the pane doesn't signal us
    // SAFETY: setsid has no memory effects.
    unsafe { libc::setsid() };
    let rex_url = env("REX_SERVER").ok_or(Error::NotInRex)?;
    let session_id = env("REX_SESSION").ok_or(Error::NotInRex)?;
    serve(base, nvim_args, Config::new(&rex_url, &session_id))
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

/// Removes the socket files left over, in order, as it drops.
struct RemoveFiles([String; 2]);

impl Drop for RemoveFiles {
    fn drop(&mut self) {
        for f in &self.0 {
            let _ = fs::remove_file(f);
        }
    }
}

/// Neovim's process. As it drops, after the terminals are restored: SIGHUP
/// makes Neovim save swap files and exit, and it's waited for. Not one that
/// has already ended.
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
}

impl Drop for NvimChild {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: signalling our own child, which hasn't been reaped.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGHUP) };
        let deadline = Instant::now() + self.grace;
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("neorex daemon: Neovim didn't exit after SIGHUP; killing it");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn serve<S: AsRef<OsStr>>(base: &str, nvim_args: &[S], mut config: Config<'_>) -> Result<()> {
    let sock = path(base, ".sock");
    let nvim_sock = path(base, ".nvim.sock");
    let log = path(base, ".log");
    // resolved, symlinks and all: the `nrex` link shouldn't end up in every
    // pane's command
    let exe = std::env::current_exe()?
        .canonicalize()?
        .to_string_lossy()
        .into_owned();

    let _ = fs::remove_file(&sock);
    // open until we exit: every new pane's helper connects here
    let server = UnixListener::bind(&sock)?;
    // the socket's file, then, last of all, after Neovim has had its chance to
    // remove its own: a Neovim that was stuck or killed leaves it behind
    let _files = RemoveFiles([nvim_sock.clone(), sock.clone()]);

    (config.ready)()?;

    let rex_block: Option<RexBlock> = match env("REX_BLOCK") {
        Some(b) => match pane::rex_block(b.as_bytes()) {
            Ok(block) => Some(block),
            Err(_) => {
                eprintln!("neorex daemon: REX_BLOCK isn't a block id: {b}");
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
            "let g:neorex = 1",
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

    let nvim = match (config.connect)(&nvim_sock) {
        Ok(nvim) => nvim,
        Err(err) => {
            return Err(startup.give_up(
                StartStep::ConnectNvim,
                Box::new(err),
                Some(&mut child.child),
            ));
        }
    };
    let rex = match Client::connect(config.rex_url) {
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
    eprintln!("neorex daemon: {}", stop_reason.tag_name());
    // a clean end needs no log
    let _ = fs::remove_file(&log);
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
    let mut w = String::from("neorex: ");
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
    use std::os::unix::net::UnixStream;

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
    fn base_path_fits_a_socket() {
        let base = base_path().unwrap();
        assert!(base.contains("/neorex-"));
        assert!(path(&base, ".nvim.sock").len() <= MAX_PATH);
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

    /// Runs a start whose Neovim connect fails, with `nvim_command` standing in
    /// for Neovim, and checks no sockets remain and the pane shows `reason`.
    fn expect_failed_start(nvim_command: &[&str], reason: &str) {
        let base = base_path().unwrap();
        let sock = path(&base, ".sock");
        let nvim_sock = path(&base, ".nvim.sock");

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

        let err = serve(
            &base,
            &[] as &[&str],
            Config {
                nvim_command,
                connect: connect_never,
                ready: Box::new(|| {
                    launcher = Some(fake_launcher_ready(&sock, slave)?);
                    Ok(())
                }),
                ..Config::new("ws://unused", "unused")
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err.downcast_ref::<nvim::Error>(),
                Some(nvim::Error::Timeout)
            ),
            "{err}"
        );
        assert!(!exists(&sock));
        assert!(!exists(&nvim_sock));

        // the pane shows the reason; the launcher's helper was hung up on
        let shown = reader.join().unwrap();
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        let shown = String::from_utf8_lossy(&shown);
        assert!(shown.contains(reason), "{shown:?}");
        let launcher = launcher.unwrap();
        let mut hangup = [0u8; 1];
        // SAFETY: `hangup` is valid for one byte.
        let n = unsafe { libc::read(launcher.as_raw_fd(), hangup.as_mut_ptr().cast(), 1) };
        assert_eq!(0, n);
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
