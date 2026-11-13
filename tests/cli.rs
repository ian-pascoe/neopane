//! The command line through the built binary: what goes to stdout and
//! stderr, and the exit statuses (#94 decisions 41-43).

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

/// The binary Cargo built for this test. Cargo names it, as Rust 1.100's
/// build directory no longer has it beside the test's own binary.
fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neopane"))
}

fn run_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(exe())
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn run(args: &[&str]) -> Output {
    run_in(&std::env::temp_dir(), args)
}

fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

#[test]
fn help_and_version_go_to_stdout() {
    for (args, want) in [
        (&["--help"][..], neopane::cli::help()),
        (&["help"], neopane::cli::help()),
        (&["list", "-h"], neopane::cli::help()),
        (&["help", "debug"], neopane::cli::debug_help()),
        (&["-v"], neopane::cli::version()),
        (&["version"], neopane::cli::version()),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert_eq!(text(&out.stdout), want, "{args:?}");
        assert_eq!(text(&out.stderr), "", "{args:?}");
    }
}

#[test]
fn the_first_line_of_the_version_is_the_build() {
    let out = Command::new("sh")
        .arg("-c")
        .arg("\"$0\" -v | head -1")
        .arg(exe())
        .output()
        .unwrap();
    assert_eq!(text(&out.stdout), format!("{}\n", neopane::attach::build()));
}

#[test]
fn a_broken_pipe_exits_quietly() {
    let mut fds = [0; 2];
    // SAFETY: pipe fills both fds, which are then owned here.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // nobody will read
    drop(read);
    let out = Command::new(exe())
        .arg("--help")
        .stdout(File::from(write))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn usage_errors_are_one_line_and_exit_2() {
    for (args, why) in [
        (
            &["file", "--name", "x"][..],
            format!("put --name before Neovim's arguments: {NAME} --name N [NVIM-ARGS]"),
        ),
        (
            &["--listen", "x"],
            "--listen is the daemon's own: it starts Neovim with --embed --headless --listen"
                .to_string(),
        ),
        (
            &["detach"],
            format!("detach isn't available yet; to open a file named detach, run {NAME} ./detach"),
        ),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&out.stderr),
            format!("{NAME}: {why} (see {NAME} --help)\n"),
            "{args:?}"
        );
        assert_eq!(text(&out.stdout), "", "{args:?}");
    }
}

#[test]
fn commands_built_later_exit_1() {
    for (args, what) in [
        (&["list"][..], "list"),
        (&["status"], "status"),
        (&["kill", "--all"], "kill"),
        (&["provider", "check"], "provider check"),
        (&["--detach"], "--detach"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            text(&out.stderr),
            format!("{NAME}: {what} isn't built yet\n"),
            "{args:?}"
        );
    }
}

#[test]
fn a_command_word_naming_a_file_says_so() {
    let dir = std::env::temp_dir().join(format!("{NAME}-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("version"), "").unwrap();
    let out = run_in(&dir, &["version"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), neopane::cli::version());
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: version is a command; to open the file, run {NAME} ./version\n")
    );
}

#[test]
fn starting_without_a_terminal_says_how() {
    // --provider does nothing yet, but is no usage error
    for args in [&["f.txt"][..], &["--provider", "p", "f.txt"]] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        assert_eq!(
            text(&out.stderr),
            format!(
                "{NAME}: stdin isn't a terminal; to start a daemon without one, use --detach\n"
            ),
            "{args:?}"
        );
    }
}

/// `daemon` with `args`, as the launcher starts it: its stdout a pipe, and
/// its files under `tmpdir`.
fn daemon(tmpdir: &Path, args: &[&str]) -> Output {
    Command::new(exe())
        .arg("daemon")
        .args(args)
        .env(neopane::daemon::dir::tmpdir_var(), tmpdir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// A new directory for one test, emptied of what an earlier run left.
fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{NAME}-cli-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).unwrap();
    dir
}

#[test]
fn the_daemon_says_why_it_didnt_start_on_its_ready_pipe() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch("ready");
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let base = tmp.join(format!("{NAME}-{uid}"));

    let out = daemon(&tmp, &["work", "--detach", "--"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "error --detach isn't built yet\n");

    // a base directory others can read
    std::fs::create_dir(&base).unwrap();
    std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = daemon(&tmp, &["work", "--"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        format!(
            "error {} isn't a private directory (owner {uid}, mode 0755); set {} to another \
             place\n",
            base.display(),
            neopane::daemon::dir::tmpdir_var()
        )
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// `daemon` with `args`, left running: it says `ok` once it listens, Rex or
/// not, then waits for the launcher's pane. With the line it said.
fn listening_daemon(tmpdir: &Path, args: &[&str]) -> (std::process::Child, String) {
    listening_daemon_with(tmpdir, args, &[])
}

/// `listening_daemon`, with `env` set too.
fn listening_daemon_with(
    tmpdir: &Path,
    args: &[&str],
    env: &[(&str, &std::ffi::OsStr)],
) -> (std::process::Child, String) {
    use std::io::BufRead;
    let mut child = Command::new(exe())
        .arg("daemon")
        .args(args)
        .envs(env.iter().copied())
        .env(neopane::daemon::dir::tmpdir_var(), tmpdir)
        .env_remove("REX_SERVER")
        .env_remove("REX_SESSION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (child, line)
}

/// A launcher's first attach to the daemon at `sock`: a self-attach,
/// relayed, from no terminal. Its connection.
fn first_attach(sock: &Path) -> std::os::unix::net::UnixStream {
    use neopane::attach::{Hello, Message, Purpose, Size, Terminal, Transport};
    use std::io::Write;
    let terminal = Terminal {
        transport: Transport::Relay,
        size: Size {
            cols: 80,
            rows: 24,
            px: None,
        },
        pid: std::process::id(),
        tty: Vec::new(),
        env: Vec::new(),
        cwd: b"/".to_vec(),
    };
    let purpose = Purpose::SelfAttach {
        terminal,
        provider: None,
        file: None,
        line: None,
    };
    let mut stream = std::os::unix::net::UnixStream::connect(sock).unwrap();
    let hello = Message::Hello(Hello::new(purpose)).encode().unwrap();
    stream.write_all(&hello).unwrap();
    stream
}

#[test]
fn a_start_that_fails_is_logged_and_the_next_claim_keeps_the_log_as_log_1() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch("failed");
    // SAFETY: getuid can't fail.
    let base = tmp.join(format!("{NAME}-{}", unsafe { libc::getuid() }));
    // a Neovim that can't start
    let bin = tmp.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let nvim = bin.join("nvim");
    std::fs::write(&nvim, "#!/bin/sh\necho 'nvim: it broke' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&nvim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut path = std::ffi::OsString::from(&bin);
    path.push(":");
    path.push(std::env::var_os("PATH").unwrap_or_default());

    let (mut first, said) = listening_daemon_with(&tmp, &["work", "--"], &[("PATH", &path)]);
    assert_eq!(said, "ok work\n");
    let attach = first_attach(&base.join("work/sock"));
    assert_eq!(first.wait().unwrap().code(), Some(1));
    drop(attach);
    let log = std::fs::read_to_string(base.join("work/log")).unwrap();
    assert!(
        log.contains("Neovim exited at once (status 1): it broke"),
        "{log:?}"
    );

    // the next claim keeps it as log.1
    let (mut next, said) = listening_daemon(&tmp, &["work", "--"]);
    assert_eq!(said, "ok work\n");
    assert_eq!(
        log,
        std::fs::read_to_string(base.join("work/log.1")).unwrap()
    );
    next.kill().unwrap();
    next.wait().unwrap();
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn a_daemon_says_ok_with_its_name_and_a_live_name_gets_a_number() {
    let tmp = scratch("ok");
    // SAFETY: getuid can't fail.
    let base = tmp.join(format!("{NAME}-{}", unsafe { libc::getuid() }));
    let (mut first, said) = listening_daemon(&tmp, &["work", "--dedup", "--"]);
    assert_eq!(said, "ok work\n");
    let (mut second, said) = listening_daemon(&tmp, &["work", "--dedup", "--"]);
    assert_eq!(said, "ok work-2\n");
    for live in ["work", "work-2"] {
        assert!(base.join(live).join("sock").exists(), "{live}");
    }
    // without --dedup, a live name is refused
    let out = daemon(&tmp, &["work", "--"]);
    assert_eq!(text(&out.stdout), "error work is already running\n");

    for d in [&mut first, &mut second] {
        d.kill().unwrap();
        d.wait().unwrap();
    }
    // the name is free again: its directory is reclaimed
    let (mut third, said) = listening_daemon(&tmp, &["work", "--dedup", "--"]);
    assert_eq!(said, "ok work\n");
    assert!(base.join("work/log.1").exists());
    third.kill().unwrap();
    third.wait().unwrap();
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn an_unknown_transport_is_warned_of_and_fd_used() {
    let var = format!("{}_TRANSPORT", NAME.to_ascii_uppercase());
    let out = Command::new(exe())
        .args([
            "attach",
            "--address",
            "/nonexistent/sock",
            "--pane",
            "1-abcdefghij",
        ])
        .env(&var, "carrier-pigeon")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    // stdin isn't a terminal: the attach fails after the warning
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!(
            "{NAME}: ignoring {var}=carrier-pigeon (use fd or relay)\n{NAME}: not a terminal\n"
        )
    );
    // `--transport` overrides it, unread
    let out = Command::new(exe())
        .args([
            "attach",
            "--address",
            "/x",
            "--pane",
            "1-abcdefghij",
            "--transport",
            "fd",
        ])
        .env(&var, "carrier-pigeon")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(text(&out.stderr), format!("{NAME}: not a terminal\n"));
}

/// `debug-relay {attach}`, the relay test tool: a requested pane relayed
/// over its stdin and stdout, against a fake daemon.
#[test]
fn the_relay_test_tool_relays_stdin_and_stdout() {
    use neopane::attach::{Accept, End, Ending, Frame, Message, Purpose, Transport};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    let sock = std::env::temp_dir().join(format!("{NAME}-relay-tool-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let server = UnixListener::bind(&sock).unwrap();
    let mut tool = Command::new(exe())
        .arg("debug-relay")
        .arg(exe())
        .args(["attach", "--address"])
        .arg(&sock)
        .args(["--pane", "1-abcdefghij", "--transport", "relay"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut s, _) = server.accept().unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 64 << 10];
    let mut next = |s: &mut std::os::unix::net::UnixStream| loop {
        if let Some((f, n)) = Frame::decode(&buf).unwrap() {
            buf.drain(..n);
            if f.kind == b'h' {
                return Message::Hello(Message::decode_hello(&f).unwrap());
            }
            return Message::decode(&f).unwrap().unwrap();
        }
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0, "the tool hung up");
        buf.extend_from_slice(&chunk[..n]);
    };
    let Message::Hello(h) = next(&mut s) else {
        panic!("no hello")
    };
    let Purpose::Requested { pane, terminal } = h.purpose else {
        panic!("{h:?}")
    };
    assert_eq!("1-abcdefghij", pane.to_string());
    assert_eq!(Transport::Relay, terminal.transport);
    let accept = Message::Accept(Accept {
        v: neopane::attach::ATTACH_PROTOCOL_VERSION,
        build: neopane::attach::build().to_string(),
        daemon: "d".into(),
        log: b"/l".to_vec(),
        features: vec![],
    });
    s.write_all(&accept.encode().unwrap()).unwrap();
    tool.stdin.as_mut().unwrap().write_all(b"typed").unwrap();
    let mut typed = Vec::new();
    while typed.len() < 5 {
        match next(&mut s) {
            Message::Input(i) => typed.extend_from_slice(&i),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(b"typed", &typed[..]);
    for m in [
        Message::Output(b"drawn".to_vec()),
        Message::End(End::ending(Ending::Closed, "closed", false)),
    ] {
        s.write_all(&m.encode().unwrap()).unwrap();
    }
    drop(s);
    let out = tool.wait_with_output().unwrap();
    let _ = std::fs::remove_file(&sock);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "drawn");
}

#[test]
fn provider_show_lists_the_builtins_and_refuses_rex() {
    let out = run(&["provider", "show"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stdout),
        "rex  return \"rex\" from provider() (built in; no Lua table)\n"
    );
    assert_eq!(text(&out.stderr), "");

    let out = run(&["provider", "show", "rex"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert_eq!(
        text(&out.stderr),
        format!(
            "{NAME}: rex is built into {NAME} and has no Lua table; return \"rex\" from \
             provider() to use it\n"
        )
    );

    let out = run(&["provider", "show", "foo"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no built-in pane provider named foo (built-ins: rex)\n")
    );

    let out = run(&["provider", "show", "a", "b"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn provider_types_writes_both_files_then_keeps_luarc() {
    /// Removed when dropped, even on failure.
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let home = std::env::temp_dir().join(format!("{NAME}-cli-types-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    // as the working directory reads it, symlinks resolved
    let tmp = Temp(home.canonicalize().unwrap());
    let home = &tmp.0;
    let config_var = format!("{}_CONFIG", NAME.to_ascii_uppercase());
    let types = |args: &[&str], config: Option<&str>| {
        let mut command = Command::new(exe());
        command
            .args(["provider", "types"])
            .args(args)
            .current_dir(home)
            .env("HOME", home)
            .env_remove("XDG_CONFIG_HOME")
            .stdin(Stdio::null());
        match config {
            Some(c) => command.env(&config_var, c),
            None => command.env_remove(&config_var),
        };
        command.output().unwrap()
    };
    let docs = format!("{}/blob/main/docs/config.md", neopane::cli::repo_url());

    // the XDG default, under HOME
    let out = types(&[], None);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "wrote ~/.config/{NAME}/types/{NAME}.lua\nwrote ~/.config/{NAME}/.luarc.json\n\
             no init.lua yet: see {docs}\n"
        )
    );
    let dir = home.join(".config").join(NAME);
    let meta = std::fs::read_to_string(dir.join(format!("types/{NAME}.lua"))).unwrap();
    assert!(
        meta.starts_with(&format!("---@meta\n-- {}\n", neopane::attach::build())),
        "{meta}"
    );
    let luarc = std::fs::read_to_string(dir.join(".luarc.json")).unwrap();
    assert!(
        luarc.contains("\"runtime.version\": \"Lua 5.1\""),
        "{luarc}"
    );

    // a second run keeps the user's .luarc.json
    std::fs::write(dir.join(".luarc.json"), "{}").unwrap();
    std::fs::write(dir.join("init.lua"), "return {}").unwrap();
    let out = types(&[], None);
    assert_eq!(
        text(&out.stdout),
        format!(
            "wrote ~/.config/{NAME}/types/{NAME}.lua\nkept ~/.config/{NAME}/.luarc.json \
             (already there)\n"
        )
    );
    assert_eq!(
        std::fs::read_to_string(dir.join(".luarc.json")).unwrap(),
        "{}"
    );

    // $<NAME>_CONFIG's directory, relative to here; --config wins over it
    let out = types(&[], Some("env/my.lua"));
    assert_eq!(
        text(&out.stdout),
        format!(
            "wrote ~/env/types/{NAME}.lua\nwrote ~/env/.luarc.json\nno my.lua yet: see {docs}\n"
        )
    );
    let out = types(&["--config", "flag/init.lua"], Some("env/my.lua"));
    assert!(
        text(&out.stdout).starts_with(&format!("wrote ~/flag/types/{NAME}.lua\n")),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn naming_a_daemon_badly_is_a_usage_error() {
    for (args, why) in [
        (
            &["--name", "a/b"][..],
            "--name: use 1-32 letters, digits, '.', '_' or '-'",
        ),
        (
            &["attach", "--name", ".."],
            "--name: use 1-32 letters, digits, '.', '_' or '-'",
        ),
        (
            &["--name", "a", "--address", "b"],
            "--name and --address both choose a daemon: give one",
        ),
        (
            &["attach", "--name", "a", "--address", "b"],
            "--name and --address both choose a daemon: give one",
        ),
        (&["attach", "a", "b"], "attach takes [+LINE] [FILE]"),
    ] {
        let out = run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(
            text(&out.stderr),
            format!("{NAME}: {why} (see {NAME} --help)\n"),
            "{args:?}"
        );
    }
}

#[test]
fn attach_with_no_daemon_running_says_how_to_start_one() {
    let tmp = scratch("none");
    let attach = |args: &[&str]| {
        Command::new(exe())
            .args(args)
            .env(neopane::daemon::dir::tmpdir_var(), &tmp)
            .env_remove(NAME.to_ascii_uppercase())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let out = attach(&["attach", "f.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no daemon is running; start one with {NAME} [FILE]\n")
    );
    let out = attach(&["attach", "--name", "work"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no daemon named work is running\n")
    );
    // the bare --address never starts one
    let out = attach(&["--address", "/nowhere/sock", "f.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no daemon answers at /nowhere/sock\n")
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn attach_among_several_real_daemons_needs_a_running_name() {
    let tmp = scratch("sev");
    let attach = |args: &[&str]| {
        Command::new(exe())
            .args(args)
            .env(neopane::daemon::dir::tmpdir_var(), &tmp)
            .env_remove(NAME.to_ascii_uppercase())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    // live and listening, each waiting for its launcher's attach
    let mut daemons: Vec<_> = ["notes", "scratch", "work"]
        .into_iter()
        .map(|name| {
            let (child, said) = listening_daemon(&tmp, &[name, "--"]);
            assert_eq!(said, format!("ok {name}\n"));
            child
        })
        .collect();
    let out = attach(&["attach", "f.txt"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: 3 daemons are running (notes, scratch, work); choose one with --name\n")
    );
    let mut work = daemons.pop().unwrap();
    work.kill().unwrap();
    work.wait().unwrap();
    let out = attach(&["attach", "--name", "work"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no daemon named work is running (running: notes, scratch)\n")
    );
    for mut d in daemons {
        d.kill().unwrap();
        d.wait().unwrap();
    }
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Whether `nvim` runs; with `CI` set, it must.
fn have_nvim() -> bool {
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
/// daemons under `tmpdir`, with no pane provider or config around it. What
/// it draws is read and dropped.
fn in_pty(tmpdir: &Path, cwd: &Path, args: &[&str]) -> std::process::Child {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
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
            &mut size,
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
        .env_remove(&upper)
        .env_remove(format!("{upper}_CONFIG"))
        .env_remove(format!("{upper}_PROVIDER"))
        .env_remove("REX_SERVER")
        .env_remove("REX_SESSION")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut master = File::from(master);
        let mut buf = [0u8; 4096];
        while matches!(master.read(&mut buf), Ok(n) if n > 0) {}
    });
    child
}

/// What Neovim at `sock` makes of `expr`, if it answers.
fn remote_expr(sock: &Path, expr: &str) -> Option<String> {
    let out = Command::new("nvim")
        .args(["--clean", "--headless", "--server"])
        .arg(sock)
        .args(["--remote-expr", expr])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| text(&out.stdout).trim().to_string())
}

/// Waits up to 20 s for Neovim at `sock` to make `want` of `expr`, as it
/// last did.
fn until_expr(sock: &Path, expr: &str, want: impl Fn(&str) -> bool) -> Option<String> {
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
struct Ending {
    sock: PathBuf,
    helpers: Vec<std::process::Child>,
    tmp: PathBuf,
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

#[test]
fn a_name_started_twice_attaches_and_attach_opens_a_file_at_its_line() {
    if !have_nvim() {
        return;
    }
    let tmp = scratch("at");
    let lines: String = (1..=20).map(|n| format!("{n}\n")).collect();
    std::fs::write(tmp.join("f.txt"), lines).unwrap();
    // SAFETY: getuid can't fail.
    let base = tmp.join(format!("{NAME}-{}", unsafe { libc::getuid() }));
    let sock = base.join("work/nvim.sock");
    let mut ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let start = ["--name", "work", "--provider", "none"];

    // started, named exactly work
    ending.helpers.push(in_pty(&tmp, &tmp, &start));
    let wins = until_expr(&sock, "len(nvim_list_wins())", |_| true);
    let wins: usize = wins.expect("work's Neovim never answered").parse().unwrap();

    // a second time, it attaches: a new window, and no work-2
    ending.helpers.push(in_pty(&tmp, &tmp, &start));
    let more = until_expr(&sock, "len(nvim_list_wins())", |n| n != wins.to_string());
    assert_eq!(more, Some((wins + 1).to_string()));
    assert!(!base.join("work-2").exists());
    assert!(ending.helpers[1].try_wait().unwrap().is_none(), "it ended");

    // attach finds the only daemon running, and opens the file at its line
    ending
        .helpers
        .push(in_pty(&tmp, &tmp, &["attach", "+12", "f.txt"]));
    let at = until_expr(&sock, "expand('%:t') .. ':' .. line('.')", |at| {
        at == "f.txt:12"
    });
    assert_eq!(at.as_deref(), Some("f.txt:12"));
    let file = remote_expr(&sock, "expand('%:p')").unwrap();
    assert_eq!(
        Path::new(&file).canonicalize().unwrap(),
        tmp.join("f.txt").canonicalize().unwrap()
    );
}
