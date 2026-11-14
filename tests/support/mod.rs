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
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("ZELLIJ_SESSION_NAME")
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

/// The installed tmux's major and minor version, from `tmux -V`, such as
/// `(3, 7)` for `tmux 3.7c`; None if it can't be read.
pub fn tmux_version() -> Option<(u32, u32)> {
    let out = std::process::Command::new("tmux").arg("-V").output().ok()?;
    let shown = String::from_utf8_lossy(&out.stdout);
    let digits = shown.trim_start_matches(|c: char| !c.is_ascii_digit());
    let (major, rest) = digits.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Whether the installed tmux has `wait-for -E`, 3.8, for `events`. Under
/// CI on macOS, where Homebrew's is 3.8, it must; elsewhere an older one
/// skips.
pub fn have_tmux_events() -> bool {
    let new_enough = tmux_version().is_some_and(|v| v >= (3, 8));
    assert!(
        new_enough || std::env::var_os("CI").is_none() || !cfg!(target_os = "macos"),
        "tmux is older than 3.8"
    );
    new_enough
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
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_PANE_ID")
            .env_remove("ZELLIJ_SESSION_NAME")
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

    /// A user's client, `tmux attach`, in a pty of its own, 80x24: what it
    /// draws is read and dropped. Detached as it drops.
    pub fn attach(&self) -> TmuxClient {
        let mut cmd = self.command();
        cmd.args(["attach", "-t", "%0"])
            .env("TERM", "xterm-256color");
        let (child, keys) = client_in_pty(cmd, [80, 24]);
        TmuxClient { child, keys }
    }

    /// [`attach`](Self::attach), once tmux lists the client.
    pub fn attached(&self) -> TmuxClient {
        let client = self.attach();
        let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.run(&["list-clients"]).is_empty() {
            assert!(
                std::time::Instant::now() < due,
                "tmux attach never attached"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        client
    }
}

/// A user's `tmux attach` in a pty, from [`TmuxServer::attach`].
pub struct TmuxClient {
    child: std::process::Child,
    /// The pty's master: what's written is what the user's terminal sends.
    pub keys: File,
}

impl Drop for TmuxClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

/// A user's client, `cmd`, started in a pty of its own of `size` cells,
/// cols and rows, as its controlling terminal: what it draws is read and
/// dropped. The client, and the pty's master: what's written to it is what
/// the user's terminal sends.
fn client_in_pty(mut cmd: Command, size: [u16; 2]) -> (std::process::Child, File) {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: size[1],
        ws_col: size[0],
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
    cmd.stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    let keys = File::from(master.try_clone().unwrap());
    std::thread::spawn(move || {
        use std::io::Read;
        let mut master = File::from(master);
        let mut buf = [0u8; 4096];
        while let Ok(1..) = master.read(&mut buf) {}
    });
    (child, keys)
}

/// The oldest zellij the built-in works with.
const ZELLIJ_OLDEST: (u32, u32, u32) = (0, 46, 0);

/// Whether a zellij 0.46.0 or newer runs; with `CI` set, it must.
pub fn have_zellij() -> bool {
    let version = Command::new("zellij")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|out| {
            let shown = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let mut parts = shown.strip_prefix("zellij ")?.split(['.', '-']);
            let mut next = || parts.next()?.parse::<u32>().ok();
            Some((next()?, next()?, next()?))
        });
    let ran = version.is_some_and(|v| v >= ZELLIJ_OLDEST);
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "zellij 0.46.0 or newer isn't installed: {version:?}"
    );
    ran
}

/// A private zellij session, started in the background with no client:
/// its own socket directory, `HOME` and config directory, so neither the
/// user's sessions nor their config or cache are touched, nor a first-run
/// float shown. Killed, its files removed, as it drops.
pub struct ZellijSession {
    pub name: String,
    /// `HOME`, `ZELLIJ_SOCKET_DIR` and `ZELLIJ_CONFIG_DIR`, and the rest
    /// every zellij call gets.
    pub env: Vec<(String, std::ffi::OsString)>,
    /// Under `/tmp`: a socket's path must stay short.
    dir: PathBuf,
}

impl ZellijSession {
    /// The private directories for session `name`, with nothing started.
    pub fn new(name: &str) -> ZellijSession {
        // a socket's path is at most 104 bytes on macOS: $TMPDIR is long
        let dir = PathBuf::from(format!("/tmp/{NAME}-zj-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (home, sockets, config) = (dir.join("home"), dir.join("s"), dir.join("config"));
        for d in [&home, &sockets, &config] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(
            config.join("config.kdl"),
            "show_startup_tips false\nshow_release_notes false\n",
        )
        .unwrap();
        // the setup screen, a float, dismissed for good, as its "Don't Show
        // Again" leaves it, where macOS and Linux keep zellij's data
        let dismissed = r#"{"flags":["WizardDismissed","SetupNeverShow"]}"#;
        for data in [
            home.join("Library/Application Support/org.Zellij-Contributors.Zellij"),
            home.join(".local/share/zellij"),
        ] {
            std::fs::create_dir_all(&data).unwrap();
            std::fs::write(data.join("desktop_integration.json"), dismissed).unwrap();
        }
        let env = vec![
            ("HOME".into(), home.clone().into()),
            ("ZELLIJ_SOCKET_DIR".into(), sockets.into()),
            ("ZELLIJ_CONFIG_DIR".into(), config.into()),
            ("XDG_DATA_HOME".into(), home.join(".local/share").into()),
            ("XDG_CACHE_HOME".into(), home.join(".cache").into()),
            ("XDG_CONFIG_HOME".into(), home.join(".config").into()),
            ("SHELL".into(), "/bin/sh".into()),
            ("TERM".into(), "xterm-256color".into()),
        ];
        ZellijSession {
            name: name.into(),
            env,
            dir,
        }
    }

    /// `zellij` with this session's private environment, and without the
    /// user's `ZELLIJ*`, `TMUX*` or `<NAME>*` variables, or Rex's.
    pub fn command(&self) -> Command {
        let upper = NAME.to_ascii_uppercase();
        let mut cmd = Command::new("zellij");
        cmd.env_remove("ZELLIJ")
            .env_remove("ZELLIJ_PANE_ID")
            .env_remove("ZELLIJ_SESSION_NAME")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TERM_PROGRAM")
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .envs(self.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        cmd
    }

    /// Starts it in the background, its first pane running a shell.
    pub fn start(&self) {
        let out = self
            .command()
            .args(["attach", "--create-background", &self.name])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "zellij didn't start: {}",
            text(&out.stderr)
        );
    }

    /// Waits until it lists a terminal pane with room: a background
    /// session's first pane has none at first.
    pub fn ready(&self) {
        let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let out = self
                .command()
                .args(["--session", &self.name, "action", "list-panes", "--json"])
                .output()
                .unwrap();
            let panes: Vec<serde_json::Value> =
                serde_json::from_slice(&out.stdout).unwrap_or_default();
            let roomy = |p: &serde_json::Value| {
                p["is_plugin"] == false && p["pane_columns"].as_u64().is_some_and(|c| c > 0)
            };
            if panes.iter().any(roomy) {
                return;
            }
            assert!(std::time::Instant::now() < due, "zellij never got ready");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    /// Opens a pane running `command` with `env` set, once it is ready,
    /// where zellij finds room: its id, `terminal_N`. A pane's environment
    /// is the server's, not the caller's: `env` sets the rest. On Linux, a
    /// pane's command may start before its terminal has a size: it waits
    /// for one, up to 5 s.
    pub fn open(&self, env: &[(&str, &std::ffi::OsStr)], command: &[&str]) -> String {
        self.ready();
        let set: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{k}={}", v.to_str().unwrap()))
            .collect();
        let sized = "i=0; while [ \"$(stty size 2>/dev/null)\" = '0 0' ] && [ $i -lt 100 ]; do \
                     sleep 0.05; i=$((i + 1)); done; exec \"$@\"";
        let mut args = vec!["new-pane", "--", "sh", "-c", sized, "sh", "env"];
        args.extend(set.iter().map(String::as_str));
        args.extend(command);
        let id = self.run(&args);
        assert!(id.starts_with("terminal_"), "new-pane printed {id:?}");
        id
    }

    /// What `zellij --session NAME action args…` prints; panics if it
    /// fails.
    pub fn run(&self, args: &[&str]) -> String {
        let out = self
            .command()
            .args(["--session", &self.name, "action"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "zellij action {args:?}: {}",
            text(&out.stderr)
        );
        text(&out.stdout).trim().to_string()
    }

    /// Its panes, as `list-panes --json` lists them. zellij sometimes
    /// prints nothing, and exits 0, while it changes its panes: it asks
    /// again, for up to 5 s.
    pub fn panes(&self) -> Vec<serde_json::Value> {
        let due = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let listed = self.run(&["list-panes", "--json"]);
            if !listed.is_empty() || std::time::Instant::now() > due {
                return serde_json::from_str(&listed)
                    .unwrap_or_else(|e| panic!("list-panes printed {listed:?}: {e}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    /// A user's client, `zellij attach`, in a pty of its own, 120x40, once
    /// zellij lists it and acts for it: zellij acts for the client that
    /// last typed, and before any has, its focus actions move nobody. It
    /// types `Ctrl-U`, which no zellij mode binds, until `focus-pane-id`
    /// moves it to a pane opened for that, then closed: a key typed while
    /// the client starts may be lost. Detached as it drops.
    pub fn attached(&self) -> ZellijClient {
        let mut cmd = self.command();
        cmd.args(["attach", &self.name]);
        let (child, keys) = client_in_pty(cmd, [120, 40]);
        let mut client = ZellijClient { child, keys };
        let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.clients().is_empty() {
            assert!(
                std::time::Instant::now() < due,
                "zellij attach never attached"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let probe = self.run(&["new-pane", "--no-focus", "--", "sleep", "100000"]);
        loop {
            client.typed(b"\x15");
            let _ = self
                .command()
                .args(["--session", &self.name, "action", "focus-pane-id", &probe])
                .output();
            if self.clients() == [probe.as_str()] {
                break;
            }
            assert!(
                std::time::Instant::now() < due,
                "zellij never acted for the client that typed"
            );
        }
        self.run(&["close-pane", "--pane-id", &probe]);
        client
    }

    /// The pane each client focuses, as `list-clients` lists them.
    pub fn clients(&self) -> Vec<String> {
        self.run(&["list-clients"])
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().nth(1).map(String::from))
            .collect()
    }

    /// Its terminal panes, as `terminal_N`.
    pub fn terminals(&self) -> Vec<String> {
        self.panes()
            .iter()
            .filter(|p| p["is_plugin"] == false)
            .map(|p| format!("terminal_{}", p["id"]))
            .collect()
    }
}

/// A user's `zellij attach` in a pty, from [`ZellijSession::attached`].
pub struct ZellijClient {
    child: std::process::Child,
    /// The pty's master: what's written is what the user's terminal sends.
    keys: File,
}

impl ZellijClient {
    /// Types `keys`, then lets zellij read them.
    pub fn typed(&mut self, keys: &[u8]) {
        use std::io::Write;
        self.keys.write_all(keys).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

impl Drop for ZellijClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = self
            .command()
            .args(["kill-session", &self.name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // the server ends after it answers, its socket last, and may still
        // write its session index: gone once its directory stays gone
        let sockets = self.dir.join("s/contract_version_1");
        let due = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let live = || std::fs::read_dir(&sockets).is_ok_and(|mut d| d.next().is_some());
        while live() && std::time::Instant::now() < due {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        while std::time::Instant::now() < due {
            let _ = std::fs::remove_dir_all(&self.dir);
            std::thread::sleep(std::time::Duration::from_millis(200));
            if !self.dir.exists() {
                break;
            }
        }
    }
}

/// Whether `herdr` runs; with `CI` set, it must.
pub fn have_herdr() -> bool {
    let ran = Command::new("herdr")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "herdr isn't installed"
    );
    ran
}

/// A private `herdr server`, its home, config and state under `dir` and
/// its API socket `dir/h.sock`, started without the user's herdr, tmux or
/// Rex variables, or any of `<name>`'s but those in `env`, which its panes
/// get. Its config turns off the first-run screen. Stopped as it drops.
pub struct HerdrServer {
    pub socket: PathBuf,
    dir: PathBuf,
    child: std::process::Child,
}

impl HerdrServer {
    pub fn start(dir: &Path, env: &[(&str, &std::ffi::OsStr)]) -> HerdrServer {
        let config = dir.join("home/.config/herdr");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("config.toml"), "onboarding = false\n").unwrap();
        let socket = dir.join("h.sock");
        let mut cmd = Command::new("herdr");
        cmd.arg("server");
        HerdrServer::isolate(&mut cmd, dir);
        let upper = NAME.to_ascii_uppercase();
        let log = File::create(dir.join("herdr.log")).unwrap();
        let child = cmd
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("REX_BLOCK")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("SHELL", "/bin/sh")
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let server = HerdrServer {
            socket,
            dir: dir.to_path_buf(),
            child,
        };
        let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while server.try_call("ping", serde_json::json!({})).is_err() {
            assert!(
                std::time::Instant::now() < due,
                "herdr server never answered: {}",
                std::fs::read_to_string(dir.join("herdr.log")).unwrap_or_default()
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        server
    }

    /// Points `cmd` at a server under `dir`, and at no other herdr.
    fn isolate(cmd: &mut Command, dir: &Path) {
        cmd.env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("home/.config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("HERDR_SOCKET_PATH", dir.join("h.sock"))
            .env_remove("HERDR_ENV")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_WORKSPACE_ID");
    }

    /// The `result` of request `method` with `params`, or the reply that
    /// has none.
    pub fn try_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use std::io::{Read, Write};
        let mut conn =
            std::os::unix::net::UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        conn.set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let request = serde_json::json!({ "id": "t", "method": method, "params": params });
        conn.write_all(format!("{request}\n").as_bytes())
            .map_err(|e| e.to_string())?;
        let mut reply = String::new();
        conn.read_to_string(&mut reply).map_err(|e| e.to_string())?;
        let v: serde_json::Value =
            serde_json::from_str(&reply).map_err(|e| format!("{e}: {reply}"))?;
        v.get("result").cloned().ok_or(reply)
    }

    /// [`try_call`](Self::try_call)'s result; panics without one.
    pub fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("herdr {method}: {e}"))
    }

    /// A user's client, `herdr`, in a pty of its own, `cols` x `rows`:
    /// what it draws is read and dropped. Killed as it drops.
    pub fn attach(&self, cols: u16, rows: u16) -> HerdrClient {
        let mut cmd = Command::new("herdr");
        HerdrServer::isolate(&mut cmd, &self.dir);
        cmd.env("TERM", "xterm-256color");
        let (child, keys) = client_in_pty(cmd, [cols, rows]);
        HerdrClient { child, keys }
    }

    /// The rows and columns of pane `pane`'s pty, as its process's
    /// terminal has them; None if it can't be read.
    pub fn pty_size(&self, pane: &str) -> Option<(u16, u16)> {
        let info = self
            .try_call("pane.process_info", serde_json::json!({ "pane_id": pane }))
            .ok()?;
        let pid = info["process_info"]["shell_pid"].as_u64()?;
        let ps = Command::new("ps")
            .args(["-o", "tty=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let tty = text(&ps.stdout).trim().to_string();
        if tty.is_empty() || tty.starts_with('?') {
            return None;
        }
        let path = std::ffi::CString::new(format!("/dev/{tty}")).ok()?;
        // SAFETY: a NUL-terminated path; the fd is ours, closed below.
        let fd = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_NOCTTY | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return None;
        }
        // SAFETY: fd is ours from here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        // SAFETY: TIOCGWINSZ fills a winsize.
        let r = unsafe {
            libc::ioctl(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                libc::TIOCGWINSZ,
                &mut size,
            )
        };
        (r == 0).then_some((size.ws_row, size.ws_col))
    }
}

/// A user's `herdr` client in a pty, from [`HerdrServer::attach`].
pub struct HerdrClient {
    child: std::process::Child,
    /// The pty's master: what's written is what the user's terminal sends.
    pub keys: File,
}

impl HerdrClient {
    /// Whether it still runs.
    pub fn alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }
}

impl Drop for HerdrClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for HerdrServer {
    fn drop(&mut self) {
        let _ = self.try_call("server.stop", serde_json::json!({}));
        let due = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while self.child.try_wait().ok().flatten().is_none() && std::time::Instant::now() < due {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
