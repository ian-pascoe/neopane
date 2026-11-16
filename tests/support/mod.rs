//! What the tests that run the built binary share.
#![allow(dead_code)]

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

/// Every variable a built-in pane provider's choosing reads, Rex's, tmux's,
/// herdr's, zellij's and the Herdr app's: a test's neopane, or a
/// multiplexer whose panes run one, must not inherit them from the
/// developer's own terminal, or it reaches their own pane provider.
pub const PROVIDER_VARS: &[&str] = &[
    "REX_SERVER",
    "REX_SESSION",
    "REX_BLOCK",
    "TMUX",
    "TMUX_PANE",
    "TERM_PROGRAM",
    "HERDR_ENV",
    "HERDR_PANE_ID",
    "HERDR_SOCKET_PATH",
    "HERDR_TAB_ID",
    "HERDR_WORKSPACE_ID",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_SOCKET_DIR",
    "HC_TERMINAL_ID",
    "HC_TERMINAL_SOCKET",
    "HERDR_APP_HOME",
];

/// `cmd` without any of [`PROVIDER_VARS`].
pub fn without_provider_vars(cmd: &mut Command) -> &mut Command {
    for var in PROVIDER_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// `neopane provider builtin WORDS…`, as a pane provider table runs it,
/// with none of [`PROVIDER_VARS`] and no `<NAME>_*` variable but those the
/// test sets after.
pub fn provider_builtin(words: &[&str]) -> Command {
    let mut cmd = Command::new(exe());
    cmd.args(["provider", "builtin"]).args(words);
    let own = format!("{}_", NAME.to_ascii_uppercase());
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with(&own) {
            cmd.env_remove(k);
        }
    }
    without_provider_vars(&mut cmd);
    cmd
}

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
    let mut cmd = Command::new(exe());
    let child = without_provider_vars(&mut cmd)
        .args(args)
        .current_dir(cwd)
        .env(neopane::daemon::dir::tmpdir_var(), tmpdir)
        .env("XDG_CONFIG_HOME", tmpdir)
        .env("XDG_STATE_HOME", tmpdir)
        .env_remove(&upper)
        .env_remove(format!("{upper}_CONFIG"))
        .env_remove(format!("{upper}_PROVIDER"))
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
    without_provider_vars(&mut Command::new(exe()))
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
            .envs(env.iter().copied());
        let status = cmd.status().unwrap();
        assert!(status.success(), "tmux didn't start");
        server.pid = server
            .run(&["display-message", "-p", "-t", "%0", "#{pid}"])
            .parse()
            .ok();
        server
    }

    /// `tmux` on this server, with none of [`PROVIDER_VARS`].
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("tmux");
        without_provider_vars(&mut cmd)
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null"])
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
    /// user's pane provider variables or `<NAME>*` ones.
    pub fn command(&self) -> Command {
        let upper = NAME.to_ascii_uppercase();
        let mut cmd = Command::new("zellij");
        without_provider_vars(&mut cmd)
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
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
    /// is the server's, not the caller's: `env` sets the rest.
    pub fn open(&self, env: &[(&str, &std::ffi::OsStr)], command: &[&str]) -> String {
        self.ready();
        let set: Vec<String> = env
            .iter()
            .map(|(k, v)| format!("{k}={}", v.to_str().unwrap()))
            .collect();
        let mut args = vec!["new-pane", "--", "env"];
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

    /// Points `cmd` at a server under `dir`, and at no other herdr: with
    /// none of [`PROVIDER_VARS`] but the socket, set after.
    fn isolate(cmd: &mut Command, dir: &Path) {
        without_provider_vars(cmd)
            .env("HOME", dir.join("home"))
            .env("XDG_CONFIG_HOME", dir.join("home/.config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("HERDR_SOCKET_PATH", dir.join("h.sock"));
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

/// Whether `hc`, the hc core's command, runs; with `CI` set, it must.
pub fn have_hc() -> bool {
    let ran = Command::new("hc")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "hc isn't installed"
    );
    ran
}

/// A private hc core, the Herdr app's: its own `HC_SESSION`, and its state,
/// config and home under a directory of its own, so neither the user's
/// core nor their config is touched. Started without the user's pane
/// provider variables or any `<NAME>*` one: its terminals get its
/// environment, and what [`HcCore::run`] adds. Stopped, its directory
/// removed, as it drops.
pub struct HcCore {
    /// Its JSON-RPC socket, as its terminals' `HC_TERMINAL_SOCKET` has it.
    pub socket: PathBuf,
    /// Under `/tmp`: a socket's path must stay short.
    pub dir: PathBuf,
    session: String,
    pid: Option<i32>,
}

impl HcCore {
    /// Starts one for test `test`, a name of at most 10 letters, and waits
    /// for it to answer.
    pub fn start(test: &str) -> HcCore {
        // a socket's path is at most 104 bytes on macOS: $TMPDIR is long
        let dir = PathBuf::from(format!("/tmp/{NAME}-hc-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["s", "config", "home"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        // the core's sockets are under its state, named for its session,
        // twice, and must fit 103 bytes: its state alone keeps it apart
        assert!(test.len() <= 10, "{test} is too long for a session name");
        let session = test.to_string();
        let socket = dir.join(format!("s/herdr-core/{session}/{session}.sock"));
        let mut core = HcCore {
            socket,
            dir,
            session,
            pid: None,
        };
        let out = core.command().args(["server", "start"]).output().unwrap();
        assert!(
            out.status.success(),
            "hc server start: {}",
            text(&out.stderr)
        );
        let due = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let status = loop {
            match core.try_call("server.status", serde_json::json!({})) {
                Ok(status) => break status,
                Err(e) => assert!(
                    std::time::Instant::now() < due,
                    "the hc core never answered: {e}: {}",
                    core.log()
                ),
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        core.pid = status["pid"].as_i64().and_then(|p| i32::try_from(p).ok());
        core
    }

    /// `hc` on this core, with none of [`PROVIDER_VARS`], `HC_*` or
    /// `<NAME>*` variables but its own.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new("hc");
        let upper = NAME.to_ascii_uppercase();
        without_provider_vars(&mut cmd)
            .env_remove("HC_SCOPE")
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .envs(self.env())
            .stdin(Stdio::null());
        cmd
    }

    /// The variables that point `hc` at this core alone.
    pub fn env(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        vec![
            ("HC_SESSION", self.session.clone().into()),
            ("XDG_STATE_HOME", self.dir.join("s").into()),
            ("XDG_CONFIG_HOME", self.dir.join("config").into()),
            ("HOME", self.dir.join("home").into()),
        ]
    }

    /// The core's log, for a failure's message.
    pub fn log(&self) -> String {
        let at = self.dir.join(format!("s/herdr-core/{}", self.session));
        std::fs::read_to_string(at.join("core.log")).unwrap_or_default()
    }

    /// The `result` of request `method` with `params`, or the reply that
    /// has none, on a connection of its own.
    pub fn try_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        hc_call(&self.socket, method, params)
    }

    /// [`try_call`](Self::try_call)'s result; panics without one.
    pub fn call(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.try_call(method, params)
            .unwrap_or_else(|e| panic!("hc core {method}: {e}"))
    }

    /// A new terminal running `argv` in `cwd`, with `env` over the core's
    /// own: its address.
    pub fn run(&self, argv: &[&str], cwd: &Path, env: &[(&str, &std::ffi::OsStr)]) -> String {
        let env: serde_json::Map<String, serde_json::Value> = env
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.to_str().unwrap().into()))
            .collect();
        let made = self.call(
            "terminal.run",
            serde_json::json!({ "argv": argv, "cwd": cwd, "env": env, "no_parent": true }),
        );
        made["address"].as_str().unwrap().to_string()
    }

    /// Terminal `terminal`'s record, live or finished.
    pub fn terminal(&self, terminal: &str) -> serde_json::Value {
        self.call("terminal.get", serde_json::json!({ "terminal": terminal }))
    }

    /// The path of the tty terminal `terminal`'s process runs on, such as
    /// `/dev/ttys004`; None if it can't be read.
    pub fn tty_of(&self, terminal: &str) -> Option<String> {
        let pid = self.terminal(terminal)["pid"].as_u64()?;
        let ps = Command::new("ps")
            .args(["-o", "tty=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let tty = text(&ps.stdout).trim().to_string();
        (!tty.is_empty() && !tty.starts_with('?')).then(|| format!("/dev/{tty}"))
    }

    /// Its Herdr app doc, and the doc's version.
    pub fn doc(&self) -> (serde_json::Value, u64) {
        let got = self.call("doc.get", serde_json::json!({ "name": "herdr-app" }));
        (got["value"].clone(), got["version"].as_u64().unwrap())
    }

    /// Writes its Herdr app doc, as the app does, over version `version`.
    pub fn set_doc(&self, doc: &serde_json::Value, version: u64) {
        self.call(
            "doc.set",
            serde_json::json!({ "name": "herdr-app", "value": doc, "version": version }),
        );
    }
}

impl Drop for HcCore {
    fn drop(&mut self) {
        let _ = self.try_call("server.stop", serde_json::json!({}));
        if let Some(pid) = self.pid {
            // SAFETY: plain syscalls, to the core this test started.
            let alive = || unsafe { libc::kill(pid, 0) } == 0;
            let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while alive() && std::time::Instant::now() < due {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if alive() {
                // SAFETY: as above.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The `result` of request `method` with `params` to the hc core at
/// `socket`, or the reply that has none, on a connection of its own.
pub fn hc_call(
    socket: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    use std::io::{BufRead, Write};
    let mut conn = std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    conn.set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let request =
        serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    conn.write_all(format!("{request}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    // the core holds the connection open: its reply is one line
    let mut lines = std::io::BufReader::new(conn).lines();
    loop {
        let line = lines
            .next()
            .ok_or("the hc core closed the connection")?
            .map_err(|e| e.to_string())?;
        let v: serde_json::Value =
            serde_json::from_str(&line).map_err(|e| format!("{e}: {line}"))?;
        if v["id"] == 1 {
            return v.get("result").cloned().ok_or(line);
        }
    }
}
