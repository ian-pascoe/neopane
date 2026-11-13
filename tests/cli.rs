//! The command line through the built binary: what goes to stdout and
//! stderr, and the exit statuses (#94 decisions 41-43).

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

mod support;
use support::*;

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

#[test]
fn the_daemon_says_why_it_didnt_start_on_its_ready_pipe() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = scratch("ready");
    // SAFETY: getuid can't fail.
    let uid = unsafe { libc::getuid() };
    let base = tmp.join(format!("{NAME}-{uid}"));

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

/// `$HOME`, as `list` shortens paths by it.
fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(std::ffi::OsString::into_encoded_bytes)
}

#[test]
fn list_with_none_running_prints_the_header_and_says_so() {
    let tmp = scratch("list0");
    let out = under(&tmp, &["list"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stdout),
        "NAME  PANES  UNSAVED  PROVIDER  DIRECTORY\n"
    );
    assert_eq!(text(&out.stderr), format!("{NAME}: no daemon is running\n"));
    let out = under(&tmp, &["list", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), "{\"daemons\":[]}\n");
    // kill: none to end
    let out = under(&tmp, &["kill"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stderr), format!("{NAME}: no daemon is running\n"));
    let out = under(&tmp, &["kill", "--all"]);
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    let out = under(&tmp, &["status"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: no daemon is running; start one with {NAME} [FILE]\n")
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn list_asks_every_daemon_skips_stale_ones_and_kill_all_ends_them() {
    let tmp = scratch("list2");
    // two live, each waiting for its launcher's attach, and one gone
    let mut daemons: Vec<_> = ["work", "notes", "gone"]
        .into_iter()
        .map(|name| {
            let (child, said) = listening_daemon(&tmp, &[name, "--"]);
            assert_eq!(said, format!("ok {name}\n"));
            child
        })
        .collect();
    let mut gone = daemons.pop().unwrap();
    gone.kill().unwrap();
    gone.wait().unwrap();
    // they start in this test's directory, as Neovim would
    let cwd = std::env::current_dir()
        .unwrap()
        .into_os_string()
        .into_encoded_bytes();
    let dir = neopane::daemon::control::shown_dir(&cwd, home().as_deref());
    let out = under(&tmp, &["list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "NAME   PANES  UNSAVED  PROVIDER  DIRECTORY\n\
             notes  0      0        none      {dir}\n\
             work   0      0        none      {dir}\n"
        )
    );
    // the JSON's shape: README.md documents it
    let out = under(&tmp, &["list", "--json"]);
    let listed = json(&out);
    let daemons_json = listed["daemons"].as_array().unwrap();
    assert_eq!(2, daemons_json.len());
    let work = &daemons_json[1];
    let keys: Vec<&str> = work
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "name",
            "address",
            "pid",
            "build",
            "v",
            "same_version",
            "dir",
            "panes",
            "unsaved",
            "provider",
            "capabilities",
            "unsupported",
            "kept",
            "log",
            "terminals"
        ]
    );
    let base = base_of(&tmp);
    assert_eq!(work["name"], "work");
    assert_eq!(work["address"], base.join("work/sock").to_str().unwrap());
    assert_eq!(work["pid"], daemons[0].id());
    assert_eq!(work["build"], neopane::attach::build());
    assert_eq!(work["v"], neopane::attach::ATTACH_PROTOCOL_VERSION);
    assert_eq!(work["same_version"], true);
    assert_eq!(work["dir"], String::from_utf8_lossy(&cwd).as_ref());
    assert_eq!(work["panes"], 0);
    assert_eq!(work["unsaved"], serde_json::json!([]));
    assert_eq!(work["provider"], "none");
    assert_eq!(work["kept"], false);
    assert_eq!(work["log"], base.join("work/log").to_str().unwrap());

    let out = under(&tmp, &["kill"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!(
            "{NAME}: 2 daemons are running (notes, work); choose one with --name, or use --all\n"
        )
    );
    let out = under(&tmp, &["kill", "--all"]);
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    for d in &mut daemons {
        // ended by request, never a signal
        let status = d.wait().unwrap();
        assert_eq!(None, std::os::unix::process::ExitStatusExt::signal(&status));
    }
    let out = under(&tmp, &["list", "--json"]);
    assert_eq!(text(&out.stdout), "{\"daemons\":[]}\n");
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// A daemon of attach protocol version 0, named `old`, faked: its
/// directory locked and its address answering every request with only the
/// frozen fields, for the rest of the test. With `refusal`, it refuses
/// everything with that message.
fn older_daemon(tmpdir: &Path, refusal: Option<&'static str>) {
    use neopane::attach::{Message, Reply};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::DirBuilderExt;
    let base = base_of(tmpdir);
    // the daemons started first made it
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&base);
    let dir = base.join("old");
    std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
    let lock = File::open(&dir).unwrap();
    // SAFETY: flock on an fd we own.
    assert_eq!(0, unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) });
    let server = std::os::unix::net::UnixListener::bind(dir.join("sock")).unwrap();
    std::thread::spawn(move || {
        let _lock = lock;
        while let Ok((mut s, _)) = server.accept() {
            let mut buf = [0u8; 4096];
            // finding it connects and hangs up
            if !matches!(s.read(&mut buf), Ok(1..)) {
                continue;
            }
            let reply = Message::Reply(Reply {
                ok: refusal.is_none(),
                message: refusal.map(String::from),
                v: 0,
                build: format!("{NAME} 0.0.9"),
                name: "old".into(),
                pid: 77,
                status: None,
                waited: None,
            });
            let _ = s.write_all(&reply.encode().unwrap());
            // open until the client hangs up, as a daemon is
            while matches!(s.read(&mut buf), Ok(1..)) {}
        }
    });
}

#[test]
fn a_daemon_of_another_version_shows_only_its_frozen_fields() {
    let tmp = scratch("older");
    older_daemon(&tmp, None);
    let out = under(&tmp, &["list"]);
    assert_eq!(
        text(&out.stdout),
        format!(
            "NAME  PANES  UNSAVED  PROVIDER  DIRECTORY\n\
             old   -      -        -         ({NAME} 0.0.9, attach protocol 0)\n"
        )
    );
    let out = under(&tmp, &["status", "--name", "old"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        text(&out.stdout),
        format!(
            "old\n  pid        77\n  build      {NAME} 0.0.9, attach protocol 0\n  \
             (another version: only its name, pid and build are known)\n"
        )
    );
    let out = under(&tmp, &["status", "--json"]);
    assert_eq!(
        json(&out),
        serde_json::json!({
            "name": "old", "address": base_of(&tmp).join("old/sock").to_str().unwrap(),
            "pid": 77, "build": format!("{NAME} 0.0.9"), "v": 0, "same_version": false,
        })
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn kill_all_ends_those_it_can_and_exits_1_naming_the_rest() {
    let tmp = scratch("killall");
    let (mut work, said) = listening_daemon(&tmp, &["work", "--"]);
    assert_eq!(said, "ok work\n");
    older_daemon(&tmp, Some("old has unsaved changes: x"));
    let out = under(&tmp, &["kill", "--all"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: old has unsaved changes: x\n")
    );
    let status = work.wait().unwrap();
    assert_eq!(None, std::os::unix::process::ExitStatusExt::signal(&status));
    std::fs::remove_dir_all(&tmp).unwrap();
}

#[test]
fn status_shows_unsaved_files_the_directory_and_terminals_and_kill_keeps_them_in_swap() {
    if !have_nvim() {
        return;
    }
    let tmp = scratch("kill");
    // as Neovim names it: macOS's /var is /private/var
    let real = tmp.canonicalize().unwrap();
    std::fs::write(tmp.join("f.txt"), "one\n").unwrap();
    std::fs::create_dir(tmp.join("sub")).unwrap();
    let sock = base_of(&tmp).join("work/nvim.sock");
    let mut ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    ending.helpers.push(in_pty(
        &tmp,
        &tmp,
        &["--name", "work", "--provider", "none", "f.txt"],
    ));
    until_expr(&sock, "1", |_| true).expect("work's Neovim never answered");
    let changed = "execute('cd sub | call setline(1, \"two\") | hide terminal sleep 30')";
    remote_expr(&sock, changed).expect("Neovim answers");
    let status = || under(&tmp, &["status", "--json"]);
    // told at its next SafeState
    let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let work = loop {
        let work = json(&status());
        let told = work["terminals"].as_array().is_some_and(|t| !t.is_empty())
            && work["unsaved"].as_array().is_some_and(|u| !u.is_empty())
            && work["dir"] == real.join("sub").to_str().unwrap();
        if told || std::time::Instant::now() > due {
            break work;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert_eq!(
        work["unsaved"],
        serde_json::json!([real.join("f.txt").to_str().unwrap()])
    );
    assert_eq!(work["dir"], real.join("sub").to_str().unwrap());
    let ptys = "join(map(filter(nvim_list_chans(), 'get(v:val, \"pty\", \"\") != \"\"'), \
                'v:val.pty'))";
    let pty = remote_expr(&sock, ptys);
    assert_eq!(work["terminals"], serde_json::json!([pty.unwrap()]));
    assert_eq!(work["panes"], 1);
    assert_eq!(work["provider"], "none (hint none)");
    let out = under(&tmp, &["status"]);
    let shown = text(&out.stdout);
    assert!(shown.starts_with("work\n  directory  "), "{shown}");
    let unsaved = format!("\n  unsaved    {}\n", real.join("f.txt").display());
    assert!(shown.contains(&unsaved), "{shown}");

    // refused while something is unsaved
    let out = under(&tmp, &["kill"]);
    assert_eq!(out.status.code(), Some(1));
    let refusal = text(&out.stderr);
    assert!(
        refusal.starts_with(&format!("{NAME}: work has unsaved changes: ")),
        "{refusal}"
    );
    assert!(
        refusal.ends_with(" (save them, or kill --force to keep them only in swap files)\n"),
        "{refusal}"
    );
    assert!(remote_expr(&sock, "1").is_some(), "it ended");

    // forced, it ends, and the changes are in a swap file
    let out = under(&tmp, &["kill", "--force", "--name", "work"]);
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    assert!(!base_of(&tmp).join("work/sock").exists());
    let swap = std::fs::read_dir(tmp.join("nvim/swap")).map(|d| {
        d.flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with("f.txt.swp"))
    });
    assert!(swap.unwrap_or(false), "no swap file for f.txt");
    // the attach ended `stopped`: 0
    let helper = ending.helpers.pop().unwrap();
    let status = helper.wait_with_output().unwrap().status;
    assert_eq!(status.code(), Some(0));
}

/// Sends Neovim at `sock` the keys `keys`, from Normal mode.
fn remote_send(sock: &Path, keys: &str) {
    let _ = Command::new("nvim")
        .args(["--clean", "--headless", "--server"])
        .arg(sock)
        .args(["--remote-send", &format!("<C-\\><C-N>{keys}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Waits up to 20 s for `file` to hold something, and gives it.
fn until_file(file: &Path) -> Option<String> {
    let due = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        match std::fs::read_to_string(file) {
            Ok(s) if !s.is_empty() => return Some(s.trim().to_string()),
            _ if std::time::Instant::now() > due => return None,
            _ => std::thread::sleep(std::time::Duration::from_millis(100)),
        }
    }
}

/// `git` with no config of the user's, in `repo`.
fn git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_string()
}

/// Whether `git` runs; with `CI` set, it must.
fn have_git() -> bool {
    let ran = Command::new("git").arg("--version").output().is_ok();
    assert!(
        ran || std::env::var_os("CI").is_none(),
        "git isn't installed"
    );
    ran
}

/// Scripts for Neovim's `:terminal`, in `tmp`, each run as `sh SCRIPT REPO
/// EXE OUT`. `commit.sh` runs `git commit` in REPO with EXE as its editor,
/// through `editor.sh`, which keeps EXE's status in `OUT.status` and its
/// stderr in `OUT.err`; git's own status goes to `OUT.git`. Hang-ups are
/// ignored, so they outlive Neovim. `attach.sh` and `no-wait.sh` open
/// `f.txt` in REPO, keeping status and stderr the same way.
fn terminal_scripts(tmp: &Path) {
    let editor = tmp.join("editor.sh");
    std::fs::write(
        &editor,
        "\"$ED_EXE\" \"$@\" 2> \"$ED_OUT.err\"; s=$?; echo $s > \"$ED_OUT.status\"; exit $s\n",
    )
    .unwrap();
    let commit = format!(
        "trap '' HUP\n\
         cd \"$1\" && export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 ED_EXE=\"$2\" \
         ED_OUT=\"$3\"\n\
         git -c user.name=t -c user.email=t@t -c commit.gpgsign=false \
         -c core.editor=\"sh {}\" commit -q; echo $? > \"$3.git\"\n",
        editor.display()
    );
    std::fs::write(tmp.join("commit.sh"), commit).unwrap();
    for (script, args) in [
        ("attach.sh", "attach --name work +2 f.txt"),
        ("no-wait.sh", "--no-wait f.txt"),
    ] {
        std::fs::write(
            tmp.join(script),
            format!("cd \"$1\" && \"$2\" {args} 2> \"$3.err\"; echo $? > \"$3.status\"\n"),
        )
        .unwrap();
    }
}

#[test]
fn as_git_s_editor_in_a_daemons_terminal_a_file_is_waited_for() {
    if !have_nvim() || !have_git() {
        return;
    }
    let tmp = scratch("waited");
    let repo = tmp.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    terminal_scripts(&tmp);
    let sock = base_of(&tmp).join("work/nvim.sock");
    let mut ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    ending.helpers.push(in_pty(
        &tmp,
        &repo,
        &["--name", "work", "--provider", "none"],
    ));
    until_expr(&sock, "1", |_| true).expect("work's Neovim never answered");
    let exe = exe();
    // runs `script` in a :terminal, its output named `out`
    let in_terminal = |script: &str, out: &str| -> PathBuf {
        let out = tmp.join(out);
        let cmd = format!(
            "execute('terminal sh {} {} {} {}')",
            tmp.join(script).display(),
            repo.display(),
            exe.display(),
            out.display()
        );
        remote_expr(&sock, &cmd).expect("Neovim answers");
        out
    };
    let with = |out: &Path, ext: &str| PathBuf::from(format!("{}.{ext}", out.display()));
    let editing = |what: &str| until_expr(&sock, "expand('%:t')", |f| f == what);
    let log = base_of(&tmp).join("work/log");
    let dones = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .matches(" is done")
            .count()
    };
    // Neovim has run `keys`, and the daemon has read what it noted meanwhile
    let barrier = |keys: &str| {
        remote_send(
            &sock,
            &format!("{keys}:let g:barrier = {}<CR>", dones() + 100),
        );
        let want = format!("{}", dones() + 100);
        until_expr(&sock, "get(g:, 'barrier', 0)", |b| b == want).expect("Neovim answers");
        let out = under(&tmp, &["status", "--name", "work"]);
        assert_eq!(out.status.code(), Some(0));
    };

    // :wq: done, 0, and committed
    let out = in_terminal("commit.sh", "wq");
    assert_eq!(editing("COMMIT_EDITMSG").as_deref(), Some("COMMIT_EDITMSG"));
    remote_send(&sock, ":call setline(1, 'first')<CR>:wq<CR>");
    assert_eq!(until_file(&with(&out, "status")).as_deref(), Some("0"));
    assert_eq!(until_file(&with(&out, "git")).as_deref(), Some("0"));
    assert_eq!(git(&repo, &["log", "--format=%s"]), "first");

    // :w alone isn't done; :bd is
    std::fs::write(repo.join("f.txt"), "1\n2\n3\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    let out = in_terminal("commit.sh", "bd");
    assert_eq!(editing("COMMIT_EDITMSG").as_deref(), Some("COMMIT_EDITMSG"));
    let before = dones();
    barrier(":call setline(1, 'second')<CR>:w<CR>");
    assert_eq!(before, dones(), "done at :w");
    assert!(!with(&out, "status").exists(), "done at :w");
    remote_send(&sock, ":bd<CR>");
    assert_eq!(until_file(&with(&out, "status")).as_deref(), Some("0"));
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]), "second");

    // attach naming the daemon it runs in opens its file there, waited
    // (#94 decision 24)
    let out = in_terminal("attach.sh", "attach");
    let at = until_expr(&sock, "expand('%:t') .. ':' .. line('.')", |a| {
        a == "f.txt:2"
    });
    assert_eq!(at.as_deref(), Some("f.txt:2"));
    let before = dones();
    barrier("");
    assert_eq!(before, dones());
    assert!(!with(&out, "status").exists(), "attach didn't wait");
    remote_send(&sock, ":q<CR>");
    assert_eq!(until_file(&with(&out, "status")).as_deref(), Some("0"));
    assert_eq!(std::fs::read_to_string(with(&out, "err")).unwrap(), "");

    // --no-wait returns once Neovim has the window
    let out = in_terminal("no-wait.sh", "no-wait");
    assert_eq!(until_file(&with(&out, "status")).as_deref(), Some("0"));
    let shown = remote_expr(&sock, "bufwinnr('f.txt') > 0");
    assert_eq!(shown.as_deref(), Some("1"));

    // $NEOPANE from a terminal not Neovim's, or naming no daemon: ignored,
    // with a warning, and a new daemon starts
    let work = base_of(&tmp).join("work/sock");
    let gone = tmp.join("gone.sock");
    let var = NAME.to_ascii_uppercase();
    for (dir, inside, why) in [
        (
            // short: a socket path has room for few bytes
            "e",
            work.to_str().unwrap(),
            format!("this terminal isn't one of work's (unset {var} to stop this warning)"),
        ),
        (
            "s",
            gone.to_str().unwrap(),
            format!("nothing answers at {}", gone.display()),
        ),
    ] {
        let cwd = tmp.join(dir);
        std::fs::create_dir(&cwd).unwrap();
        let env = [(var.as_str(), std::ffi::OsStr::new(inside))];
        let (helper, drawn) = in_pty_drawn(&tmp, &cwd, &[], &env, 80);
        let other = base_of(&tmp).join(dir).join("nvim.sock");
        let _other = Ending {
            sock: other.clone(),
            helpers: vec![helper],
            tmp: tmp.join("kept"),
        };
        let answered = until_expr(&other, "1", |_| true);
        let drawn = String::from_utf8_lossy(&drawn.lock().unwrap()).into_owned();
        let log = std::fs::read_to_string(base_of(&tmp).join(dir).join("log"));
        assert!(
            answered.is_some(),
            "no new daemon answered: {drawn:?} {log:?}"
        );
        let warning = format!("{NAME}: ignoring ${var}: {why}");
        assert!(drawn.contains(&warning), "{warning}: {drawn:?}");
    }

    // :cq quits Neovim and ends the daemon: the waited file is never done,
    // and git commits nothing
    std::fs::write(repo.join("f.txt"), "three\n").unwrap();
    git(&repo, &["add", "f.txt"]);
    let out = in_terminal("commit.sh", "cq");
    assert_eq!(editing("COMMIT_EDITMSG").as_deref(), Some("COMMIT_EDITMSG"));
    remote_send(&sock, ":call setline(1, 'third')<CR>:w<CR>:cq<CR>");
    let launcher = ending.helpers.pop().unwrap();
    let ended = launcher.wait_with_output().unwrap();
    assert_eq!(ended.status.code(), Some(1));
    assert_eq!(until_file(&with(&out, "status")).as_deref(), Some("1"));
    let err = std::fs::read_to_string(with(&out, "err")).unwrap();
    assert!(
        err.starts_with(&format!("{NAME}: work ended before ")) && err.ends_with(" was done\n"),
        "{err:?}"
    );
    assert_ne!(until_file(&with(&out, "git")).as_deref(), Some("0"));
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]), "second");
}

/// `neopane` with `args`, from no terminal, in `cwd`, its daemons under
/// `tmpdir` with nothing around them, as `in_pty` has them.
fn detached(tmpdir: &Path, cwd: &Path, args: &[&str]) -> Output {
    detached_command(tmpdir, cwd, args).output().unwrap()
}

/// The command `detached` runs.
fn detached_command(tmpdir: &Path, cwd: &Path, args: &[&str]) -> Command {
    let upper = NAME.to_ascii_uppercase();
    let mut command = Command::new(exe());
    command
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
        .stdin(Stdio::null());
    command
}

/// Waits up to 10 s for `done`.
fn within_10s(mut done: impl FnMut() -> bool) -> bool {
    let due = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < due {
        if done() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// What `drawn` shows so far, lossily.
fn shown(drawn: &Drawn) -> String {
    String::from_utf8_lossy(&drawn.lock().unwrap()).into_owned()
}

#[test]
fn detach_starts_a_kept_daemon_that_attach_shows_and_kill_ends() {
    if !have_nvim() {
        return;
    }
    let tmp = scratch("detach");
    std::fs::write(tmp.join("f.txt"), "detached text\n").unwrap();
    let cwd = tmp.join("work");
    std::fs::create_dir(&cwd).unwrap();
    let sock = base_of(&tmp).join("work/nvim.sock");
    let mut ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };

    // named after its directory, its name printed once Neovim answers
    let out = detached(&tmp, &cwd, &["--detach", "../f.txt"]);
    assert_eq!(
        (out.status.code(), text(&out.stdout), text(&out.stderr)),
        (Some(0), "work\n", "")
    );
    assert_eq!(
        remote_expr(&sock, "expand('%:t')").as_deref(),
        Some("f.txt")
    );
    let work = || json(&under(&tmp, &["status", "--json", "--name", "work"]));
    assert_eq!(
        (work()["panes"].clone(), work()["kept"].clone()),
        (0.into(), true.into())
    );
    let status = text(&under(&tmp, &["status"]).stdout).to_string();
    assert!(
        status.contains("\n  stays      until killed, started with --detach\n"),
        "{status}"
    );

    // a name running can't be started again
    let out = detached(&tmp, &cwd, &["--detach", "--name", "work"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: work is already running; attach with {NAME} attach --name work\n")
    );

    // attach shows its window
    let (helper, drawn, _keys) = in_pty_typed(&tmp, &tmp, &["attach"], &[], 80);
    ending.helpers.push(helper);
    assert!(
        within_10s(|| shown(&drawn).contains("detached text")),
        "{:?}",
        shown(&drawn)
    );
    assert!(within_10s(|| work()["panes"] == 1));

    // its last pane gone with nothing unsaved, it stays
    let mut helper = ending.helpers.pop().unwrap();
    helper.kill().unwrap();
    helper.wait().unwrap();
    assert!(within_10s(|| work()["panes"] == 0));
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(remote_expr(&sock, "1").as_deref(), Some("1"));
    let listed = text(&under(&tmp, &["list"]).stdout).to_string();
    assert!(listed.contains("\nwork  0 kept "), "{listed}");

    // kill ends it
    let out = under(&tmp, &["kill"]);
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    assert!(!base_of(&tmp).join("work/sock").exists());
}

#[test]
fn detach_that_fails_says_why_in_one_line() {
    let tmp = scratch("dfail");
    let out = detached_command(&tmp, &tmp, &["--detach", "--name", "work"])
        .env("PATH", tmp.join("nowhere"))
        .output()
        .unwrap();
    let log = base_of(&tmp).join("work/log");
    assert_eq!(
        (
            out.status.code(),
            text(&out.stdout),
            text(&out.stderr).to_string()
        ),
        (
            Some(1),
            "",
            format!(
                "{NAME}: work didn't start: can't run nvim: not found on PATH (log: {})\n",
                log.display()
            )
        )
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// Kills every daemon under its directory as it drops, unsaved changes
/// and all, and the helpers.
struct KillAll {
    tmp: PathBuf,
    helpers: Vec<std::process::Child>,
}

impl Drop for KillAll {
    fn drop(&mut self) {
        let _ = under(&self.tmp, &["kill", "--all", "--force"]);
        for h in &mut self.helpers {
            let _ = h.kill();
            let _ = h.wait();
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

#[test]
fn a_file_with_unsaved_changes_in_a_daemon_no_terminal_shows_is_asked_about() {
    use std::io::Write;
    if !have_nvim() {
        return;
    }
    let tmp = scratch("question");
    std::fs::write(tmp.join("notes.md"), "text\n").unwrap();
    let mut all = KillAll {
        tmp: tmp.clone(),
        helpers: Vec::new(),
    };
    let notes = base_of(&tmp).join("notes");
    let out = detached(
        &tmp,
        &tmp,
        &[
            "--detach",
            "--name",
            "notes",
            "notes.md",
            "-c",
            "normal! ichanged",
        ],
    );
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    assert!(
        within_10s(|| notes.join("unsaved").exists()),
        "never listed"
    );
    let question = format!(
        "{NAME}: ../notes.md has unsaved changes in notes, which no terminal shows. Attach to \
         it? [Y/n] "
    );
    let running = || base_of(&tmp).read_dir().unwrap().count();
    // a short name for the daemon `n` starts, as its socket's path is long
    let cwd = tmp.join("n");
    std::fs::create_dir(&cwd).unwrap();
    let asking = |all: &mut KillAll| {
        let (helper, drawn, keys) = in_pty_typed(&tmp, &cwd, &["../notes.md"], &[], 120);
        all.helpers.push(helper);
        let asked = within_10s(|| shown(&drawn).contains(&question));
        assert!(asked, "{:?}", shown(&drawn));
        (drawn, keys)
    };

    // Ctrl-D: exit 1, nothing started
    let (_, mut keys) = asking(&mut all);
    keys.write_all(b"\x04").unwrap();
    let mut helper = all.helpers.pop().unwrap();
    assert!(within_10s(|| helper.try_wait().unwrap().is_some()));
    assert_eq!(helper.wait().unwrap().code(), Some(1));
    assert_eq!(running(), 1);

    // anything else asks again; n starts a new daemon
    let (drawn, mut keys) = asking(&mut all);
    keys.write_all(b"what\n").unwrap();
    assert!(within_10s(|| shown(&drawn).matches(&question).count() == 2));
    keys.write_all(b"n\n").unwrap();
    assert!(within_10s(|| running() == 2), "{:?}", shown(&drawn));
    let out = under(&tmp, &["kill", "--force", "--name", "n"]);
    assert_eq!((out.status.code(), text(&out.stderr)), (Some(0), ""));
    let mut helper = all.helpers.pop().unwrap();
    let _ = helper.kill();
    let _ = helper.wait();

    // y attaches
    let (drawn, mut keys) = asking(&mut all);
    keys.write_all(b"y\n").unwrap();
    assert!(
        within_10s(|| shown(&drawn).contains("changedtext")),
        "{:?}",
        shown(&drawn)
    );
    let status = json(&under(&tmp, &["status", "--json", "--name", "notes"]));
    assert_eq!(status["panes"], 1);
}

#[test]
fn the_first_attach_to_a_kept_daemon_shows_its_current_tab_page() {
    if !have_nvim() {
        return;
    }
    let tmp = scratch("dtab");
    std::fs::write(tmp.join("a.txt"), "first tab\n").unwrap();
    std::fs::write(tmp.join("b.txt"), "second tab\n").unwrap();
    let mut ending = Ending {
        sock: base_of(&tmp).join("w/nvim.sock"),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let args = [
        "--detach", "--name", "w", "-p", "a.txt", "b.txt", "-c", "tabnext",
    ];
    let out = detached(&tmp, &tmp, &args);
    assert_eq!((out.status.code(), text(&out.stdout)), (Some(0), "w\n"));
    let (helper, drawn, _keys) = in_pty_typed(&tmp, &tmp, &["attach"], &[], 80);
    ending.helpers.push(helper);
    assert!(
        within_10s(|| shown(&drawn).contains("second tab")),
        "{:?}",
        shown(&drawn)
    );
    assert!(!shown(&drawn).contains("first tab"), "{:?}", shown(&drawn));
}

/// A kept daemon started with `args` in a scratch directory `test` holding
/// `a.txt`, `b.txt` and `c.txt`, then `attach c.txt`: the files its
/// external windows show, sorted, once `c.txt` shows, and what the attach
/// drew.
fn attached_c(test: &str, args: &[&str]) -> (Option<String>, String) {
    let tmp = scratch(test);
    for f in ["a", "b", "c"] {
        std::fs::write(tmp.join(format!("{f}.txt")), format!("file {f}\n")).unwrap();
    }
    let sock = base_of(&tmp).join("w/nvim.sock");
    let mut ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let mut start = vec!["--detach", "--name", "w"];
    start.extend(args);
    let out = detached(&tmp, &tmp, &start);
    assert_eq!((out.status.code(), text(&out.stdout)), (Some(0), "w\n"));
    let (helper, drawn, _keys) = in_pty_typed(&tmp, &tmp, &["attach", "c.txt"], &[], 120);
    ending.helpers.push(helper);
    within_10s(|| shown(&drawn).contains("file c"));
    let files = "join(sort(map(filter(nvim_list_wins(), 'nvim_win_get_config(v:val).external'), \
                 'fnamemodify(bufname(winbufnr(v:val)), \":t\")')))";
    let want = if args.is_empty() {
        "c.txt"
    } else {
        "a.txt b.txt c.txt"
    };
    let files = until_expr(&sock, files, |f| f == want);
    // drawn with the rest of the tab page, past the first redraws
    within_10s(|| args.is_empty() || shown(&drawn).contains("file a"));
    (files, shown(&drawn))
}

#[test]
fn a_file_attach_to_a_kept_daemon_keeps_the_windows_its_arguments_opened() {
    if !have_nvim() {
        return;
    }
    let (files, drawn) = attached_c("dkeep", &["-O", "a.txt", "b.txt"]);
    assert_eq!(files.as_deref(), Some("a.txt b.txt c.txt"));
    for f in ["file a", "file b", "file c"] {
        assert!(drawn.contains(f), "{f}: {drawn:?}");
    }
}

#[test]
fn a_file_attach_to_a_kept_daemon_started_on_no_file_closes_its_empty_window() {
    if !have_nvim() {
        return;
    }
    let (files, drawn) = attached_c("dempty", &[]);
    assert_eq!(files.as_deref(), Some("c.txt"));
    assert!(drawn.contains("file c"), "{drawn:?}");
}
