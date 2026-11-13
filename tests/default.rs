//! The built-in default where multiplexers nest (SPI-1135): a pane of
//! one inherits the variables of whatever started its server, so the
//! default proves which it is in by its terminal, and quietly falls through
//! when it guessed wrong. Through `provider check`, which chooses as the
//! daemon does: with no config, and with a config returning
//! `<name>.default(attach)`, which must answer the same.

mod support;

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use neopane::name::NAME;
use serde_json::Value;
use support::{TmuxServer, exe, have_tmux, scratch, text};

/// A config that prints what `<name>.inside.tmux` says, then returns
/// `<name>.default(attach)`.
const BY_DEFAULT: &str = "return { provider = function(attach) \
     print({name}.inside.tmux(attach)) \
     return {name}.default(attach) end }";

/// A directory for one test, with `BY_DEFAULT` at `config.lua`.
struct Dir {
    path: PathBuf,
}

impl Dir {
    fn new(test: &str) -> Dir {
        let path = scratch(&format!("default-{test}"));
        std::fs::write(path.join("config.lua"), BY_DEFAULT.replace("{name}", NAME)).unwrap();
        Dir { path }
    }

    fn config(&self) -> PathBuf {
        self.path.join("config.lua")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `provider check --json` with `args`, in `dir`, with only `env` of the
/// pane provider and config variables set (`<NAME>_CONFIG` naming the
/// config, if any), no config at the default places.
fn check(dir: &Path, args: &[&str], env: &[(&str, &OsStr)]) -> Command {
    let upper = NAME.to_ascii_uppercase();
    let mut c = Command::new(exe());
    c.args(["provider", "check", "--json"])
        .args(args)
        .current_dir(dir)
        .env("HOME", dir)
        .env("XDG_CONFIG_HOME", dir)
        .env_remove(&upper)
        .env_remove(format!("{upper}_CONFIG"))
        .env_remove(format!("{upper}_PROVIDER"))
        .env_remove("REX_SERVER")
        .env_remove("REX_SESSION")
        .env_remove("REX_BLOCK")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TERM_PROGRAM")
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    c
}

/// `<NAME>_CONFIG` naming `file`.
fn config_var(file: &Path) -> (String, &OsStr) {
    (
        format!("{}_CONFIG", NAME.to_ascii_uppercase()),
        file.as_os_str(),
    )
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}{}", text(&out.stdout), text(&out.stderr)))
}

/// A pseudo-terminal of no tmux server: its slave's end and path.
fn pty() -> (OwnedFd, OwnedFd, String) {
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
    // SAFETY: openpty made both, and they are ours alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut name = [0u8; 256];
    // SAFETY: `name` is valid for its length.
    assert_eq!(0, unsafe {
        libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr().cast(), name.len())
    });
    let end = name.iter().position(|&b| b == 0).unwrap();
    let path = String::from_utf8(name[..end].to_vec()).unwrap();
    (master, slave, path)
}

/// What `command` prints, its stdin `slave`, a terminal.
fn in_terminal(mut command: Command, slave: &OwnedFd) -> Output {
    command.stdin(slave.try_clone().unwrap()).output().unwrap()
}

/// The `inside.tmux` line `BY_DEFAULT` printed: `ok\twhy`.
fn printed(v: &Value) -> &str {
    v["ran"]["printed"][0].as_str().unwrap_or_default()
}

#[test]
fn tmuxs_variables_leaked_into_a_terminal_it_doesnt_run_fall_through_to_rex() {
    if !have_tmux() {
        return;
    }
    let dir = Dir::new("leaked");
    let tmux = TmuxServer::start(&dir.path, &[], &[]);
    let (_master, slave, tty) = pty();
    let tmux_var = tmux.tmux_var();
    let env = [
        ("TERM_PROGRAM", OsStr::new("tmux")),
        ("TMUX", OsStr::new(&tmux_var)),
        ("REX_SERVER", OsStr::new("unix:///nowhere/rex.sock")),
        ("REX_SESSION", OsStr::new("session:s")),
    ];
    let why = format!(
        "this terminal ({tty}) isn't a pane of the tmux server at {}",
        tmux.socket.display()
    );

    let none = json(&in_terminal(check(&dir.path, &["--no-find"], &env), &slave));
    assert_eq!(none["chose"]["how"], "default");
    assert_eq!(none["chose"]["name"], "rex", "{none}");
    assert_eq!(
        none["default"],
        serde_json::json!({
            "named": ["tmux", "rex"],
            "inside": [{ "name": "tmux", "ok": false, "why": why }],
        })
    );

    let config = dir.config();
    let (k, v) = config_var(&config);
    let with_config = [&env[..], &[(k.as_str(), v)]].concat();
    let by_config = json(&in_terminal(
        check(&dir.path, &["--no-find"], &with_config),
        &slave,
    ));
    assert_eq!(by_config["chose"]["how"], "provider");
    assert_eq!(by_config["chose"]["name"], "rex", "{by_config}");
    assert_eq!(printed(&by_config), format!("false\t{why}"));
}

/// Waits up to 20 s for `file` to be there.
fn until_there(file: &Path) -> Option<String> {
    let due = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(s) = std::fs::read_to_string(file) {
            return Some(s);
        }
        if Instant::now() > due {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_pane_of_tmux_with_rexs_variables_leaked_into_it_is_tmuxs() {
    if !have_tmux() {
        return;
    }
    let dir = Dir::new("nested");
    // both checks run in the server's one pane, %0, which then waits
    let checks = format!(
        "{exe} provider check --json --no-find > none.tmp; mv none.tmp none.json; \
         {var}={config} {exe} provider check --json --no-find > config.tmp; \
         mv config.tmp config.json; exec sleep 60",
        exe = exe().display(),
        var = config_var(Path::new("")).0,
        config = dir.config().display(),
    );
    let path = dir.path.clone();
    let tmux = TmuxServer::start(
        &dir.path,
        &[
            ("HOME", path.as_os_str()),
            ("XDG_CONFIG_HOME", path.as_os_str()),
            ("REX_SERVER", OsStr::new("unix:///nowhere/rex.sock")),
            ("REX_SESSION", OsStr::new("session:s")),
        ],
        &["sh", "-c", &format!("cd {} && {checks}", path.display())],
    );
    let read = |file: &str| -> Value {
        let got = until_there(&path.join(file)).unwrap_or_else(|| panic!("no {file}"));
        serde_json::from_str(&got).unwrap_or_else(|e| panic!("{e}: {got}"))
    };

    let none = read("none.json");
    assert_eq!(none["chose"]["how"], "default");
    assert_eq!(none["chose"]["name"], "tmux", "{none}");
    assert_eq!(
        none["default"],
        serde_json::json!({
            "named": ["tmux", "rex"],
            "inside": [{ "name": "tmux", "ok": true, "why": "tmux pane %0" }],
        })
    );
    let by_config = read("config.json");
    assert_eq!(by_config["chose"]["how"], "provider");
    assert_eq!(by_config["chose"]["name"], "tmux", "{by_config}");
    assert_eq!(printed(&by_config), "true\ttmux pane %0");
    drop(tmux);
}

/// What `inside.tmux` says for a terminal with `env`, its stdin `slave`
/// if given, else not a terminal.
fn inside(dir: &Dir, env: &[(&str, &OsStr)], slave: Option<&OwnedFd>) -> String {
    let config = dir.config();
    let (k, v) = config_var(&config);
    let env = [env, &[(k.as_str(), v)]].concat();
    let command = check(&dir.path, &["--no-find"], &env);
    let out = match slave {
        Some(slave) => in_terminal(command, slave),
        None => {
            let mut command = command;
            command.output().unwrap()
        }
    };
    printed(&json(&out)).to_string()
}

/// A directory holding only a `tmux` that runs `script`.
fn fake_tmux(dir: &Dir, script: &str) -> PathBuf {
    let bin = dir.path.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tmux = bin.join("tmux");
    std::fs::write(&tmux, format!("#!/bin/sh\n{script}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

#[test]
fn inside_tmux_says_why_it_cant_prove_a_pane() {
    let dir = Dir::new("why");
    let (_master, slave, _tty) = pty();
    let tmux = ("TMUX", OsStr::new("/tmp/nowhere/sock,1,0"));
    assert_eq!(inside(&dir, &[], Some(&slave)), "false\tTMUX isn't set");
    assert_eq!(inside(&dir, &[tmux], None), "false\tthe attach has no tty");

    let empty = dir.path.join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let cant = inside(&dir, &[tmux, ("PATH", empty.as_os_str())], Some(&slave));
    assert!(cant.starts_with("false\tcan't run tmux: "), "{cant}");

    let failing = fake_tmux(
        &dir,
        "echo 'no server running on /tmp/nowhere/sock' >&2\nexit 1",
    );
    assert_eq!(
        inside(&dir, &[tmux, ("PATH", failing.as_os_str())], Some(&slave)),
        "false\ttmux: no server running on /tmp/nowhere/sock"
    );

    let hanging = fake_tmux(&dir, "exec /bin/sleep 5");
    let started = Instant::now();
    assert_eq!(
        inside(&dir, &[tmux, ("PATH", hanging.as_os_str())], Some(&slave)),
        "false\ttmux didn't answer in 0.5 s"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_refused_guess_is_none_quietly_but_a_configs_choice_refused_quotes_finds_stderr() {
    if !have_tmux() {
        return;
    }
    let dir = Dir::new("refused");
    let tmux = TmuxServer::start(&dir.path, &[], &[]);
    let (_master, slave, tty) = pty();
    let tmux_var = tmux.tmux_var();
    // TMUX and TERM_PROGRAM leaked: tmux alone is named, and chosen
    let env = [
        ("TERM_PROGRAM", OsStr::new("tmux")),
        ("TMUX", OsStr::new(&tmux_var)),
    ];
    let refusal = format!(
        "{NAME}: tmux: find didn't confirm this terminal (exit 1: tmux find: this terminal \
         ({tty}) isn't a pane of the tmux server at {}); no pane provider",
        tmux.socket.display()
    );

    let out = in_terminal(check(&dir.path, &[], &env), &slave);
    assert_eq!(Some(0), out.status.code(), "{}", text(&out.stdout));
    let v = json(&out);
    assert_eq!(v["chose"]["name"], "tmux");
    assert_eq!(
        v["result"],
        serde_json::json!({ "state": "none", "message": null, "refused": refusal })
    );

    let builtin = dir.path.join("builtin.lua");
    std::fs::write(
        &builtin,
        format!("return {{ provider = function() return {NAME}.builtin.tmux() end }}"),
    )
    .unwrap();
    let (k, v) = config_var(&builtin);
    let out = in_terminal(
        check(&dir.path, &[], &[&env[..], &[(k.as_str(), v)]].concat()),
        &slave,
    );
    assert_eq!(Some(1), out.status.code(), "{}", text(&out.stdout));
    assert_eq!(
        json(&out)["result"],
        serde_json::json!({ "state": "error", "message": refusal })
    );
}
