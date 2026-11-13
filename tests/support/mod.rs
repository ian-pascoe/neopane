//! What the tests that run the built binary share.
#![allow(dead_code)]

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

/// The binary Cargo built for this test. Cargo names it, as Rust 1.100's
/// build directory no longer has it beside the test's own binary.
pub fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neopane"))
}

pub fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

/// A new directory for one test, emptied of what an earlier run left.
pub fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{NAME}-cli-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    dir
}

/// Whether `nvim` runs; with `CI` set, it must.
pub fn have_nvim() -> bool {
    let ran = Command::new("nvim")
        .arg("--version")
        .stdout(Stdio::null())
        .status()
        .is_ok();
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "nvim isn't installed"
    );
    ran
}

/// `neopane` with `args` in a pseudo-terminal of its own, in `cwd`, its
/// daemons under `tmpdir`, with no pane provider or config around it, and
/// Neovim's swap files under `tmpdir` too. What it draws is read and
/// dropped.
pub fn in_pty(tmpdir: &Path, cwd: &Path, args: &[&str]) -> std::process::Child {
    in_pty_with(tmpdir, cwd, args, &[], 80)
}

/// `in_pty`, with `env` set too, `cols` wide.
pub fn in_pty_with(
    tmpdir: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    cols: u16,
) -> std::process::Child {
    in_pty_drawn(tmpdir, cwd, args, env, cols).0
}

/// What a terminal was sent, as it comes.
pub type Drawn = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// `in_pty_with`, keeping what it draws.
pub fn in_pty_drawn(
    tmpdir: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    cols: u16,
) -> (std::process::Child, Drawn) {
    let (child, drawn, _keys) = in_pty_typed(tmpdir, cwd, args, env, cols);
    (child, drawn)
}

/// `in_pty_drawn`, with the terminal's end to type into.
pub fn in_pty_typed(
    tmpdir: &Path,
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
    cols: u16,
) -> (std::process::Child, Drawn, File) {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: valid out-pointers; no name or termios.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(0, rc);
    // SAFETY: openpty made both, and they are ours alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let upper = NAME.to_ascii_uppercase();
    let child = Command::new(exe())
        .args(args)
        .current_dir(cwd)
        .env(neopane::daemon::dir::tmpdir_var(), tmpdir)
        .env("XDG_CONFIG_HOME", tmpdir)
        .env("XDG_STATE_HOME", tmpdir)
        .env_remove(&upper)
        .env_remove(format!("{upper}_CONFIG"))
        .env_remove(format!("{upper}_PROVIDER"))
        .env_remove("REX_SERVER")
        .env_remove("REX_SESSION")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM_PROGRAM")
        .envs(env.iter().copied())
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
    let keys = File::from(master.try_clone().unwrap());
    let drawn = Drawn::default();
    let kept = drawn.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut master = File::from(master);
        let mut buf = [0u8; 4096];
        while let Ok(n @ 1..) = master.read(&mut buf) {
            kept.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    (child, drawn, keys)
}

/// What Neovim at `sock` makes of `expr`, if it answers.
pub fn remote_expr(sock: &Path, expr: &str) -> Option<String> {
    let mut child = Command::new("nvim")
        .args(["--clean", "--headless", "--server"])
        .arg(sock)
        .args(["--remote-expr", expr])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // a Neovim held at a prompt never answers
    let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().ok()?.is_none() {
        if std::time::Instant::now() > due {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().ok()?;
    out.status
        .success()
        .then(|| text(&out.stdout).trim().to_string())
}

/// Waits up to 20 s for Neovim at `sock` to make `want` of `expr`, as it
/// last did.
pub fn until_expr(sock: &Path, expr: &str, want: impl Fn(&str) -> bool) -> Option<String> {
    let due = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let got = remote_expr(sock, expr);
        if got.as_deref().is_some_and(&want) || std::time::Instant::now() > due {
            return got;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Ends, as it drops, the daemon whose Neovim is at `sock`, by quitting its
/// Neovim, and the helpers attached to it.
pub struct Ending {
    pub sock: PathBuf,
    pub helpers: Vec<std::process::Child>,
    pub tmp: PathBuf,
}

impl Drop for Ending {
    fn drop(&mut self) {
        let _ = Command::new("nvim")
            .args(["--clean", "--headless", "--server"])
            .arg(&self.sock)
            .args(["--remote-send", "<C-\\><C-N>:qa!<CR>"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
        for h in &mut self.helpers {
            while h.try_wait().ok().flatten().is_none() && std::time::Instant::now() < due {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = h.kill();
            let _ = h.wait();
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

/// `neopane` with `args`, its daemons under `tmpdir`, from no daemon's
/// Neovim.
pub fn under(tmpdir: &Path, args: &[&str]) -> Output {
    Command::new(exe())
        .args(args)
        .env(neopane::daemon::dir::tmpdir_var(), tmpdir)
        .env_remove(NAME.to_ascii_uppercase())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// The base directory under `tmpdir`.
pub fn base_of(tmpdir: &Path) -> PathBuf {
    // SAFETY: getuid can't fail.
    tmpdir.join(format!("{NAME}-{}", unsafe { libc::getuid() }))
}

pub fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&out.stdout)))
}

/// Whether `tmux` runs; with `CI` set, it must.
pub fn have_tmux() -> bool {
    let ran = Command::new("tmux")
        .arg("-V")
        .stdout(Stdio::null())
        .status()
        .is_ok();
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "tmux isn't installed"
    );
    ran
}

/// A private tmux server, on a socket of its own under `dir` and no
/// config, started without the user's `TMUX` or `TMUX_PANE`, or any
/// variable of `<name>`'s or Rex's but those in `env`: one detached
/// session, 80x24, running `command` in its pane, or a shell. Killed as it
/// drops.
pub struct TmuxServer {
    pub socket: PathBuf,
    /// Its pid, killed as it drops, should its socket be gone by then.
    pid: Option<i32>,
}

impl TmuxServer {
    pub fn start(dir: &Path, env: &[(&str, &std::ffi::OsStr)], command: &[&str]) -> TmuxServer {
        let socket = dir.join("sock");
        let mut server = TmuxServer { socket, pid: None };
        let upper = NAME.to_ascii_uppercase();
        let mut cmd = server.command();
        cmd.args(["new-session", "-d", "-x", "80", "-y", "24"])
            .args(command)
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .envs(env.iter().copied());
        let status = cmd.status().unwrap();
        assert!(status.success(), "tmux didn't start");
        server.pid = server
            .run(&["display-message", "-p", "-t", "%0", "#{pid}"])
            .parse()
            .ok();
        server
    }

    /// `tmux` on this server, with `TMUX` and `TMUX_PANE` cleared.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null());
        cmd
    }

    /// What `tmux args…` prints, trimmed; panics if it fails.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self.command().args(args).output().unwrap();
        assert!(out.status.success(), "tmux {args:?}: {}", text(&out.stderr));
        text(&out.stdout).trim().to_string()
    }

    /// `TMUX` as a pane of this server has it: its socket, its pid and
    /// session 0.
    pub fn tmux_var(&self) -> String {
        format!("{},{},0", self.socket.display(), self.pid.unwrap())
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let killed = self
            .command()
            .arg("kill-server")
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if let Some(pid) = self.pid.filter(|_| !killed) {
            // SAFETY: a plain syscall, to the server this test started.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}
