//! The command line through the built binary: what goes to stdout and
//! stderr, and the exit statuses (#94 decisions 41-43).

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use neorex::name::NAME;

/// The binary Cargo built beside this test.
fn exe() -> PathBuf {
    let test = std::env::current_exe().unwrap();
    test.parent().unwrap().parent().unwrap().join(NAME)
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
        (&["--help"][..], neorex::cli::help()),
        (&["help"], neorex::cli::help()),
        (&["list", "-h"], neorex::cli::help()),
        (&["help", "debug"], neorex::cli::debug_help()),
        (&["-v"], neorex::cli::version()),
        (&["version"], neorex::cli::version()),
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
    assert_eq!(text(&out.stdout), format!("{}\n", neorex::attach::build()));
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
        (&["attach", "f"], "attach"),
        (&["status"], "status"),
        (&["kill", "--all"], "kill"),
        (&["provider", "check"], "provider check"),
        (&["--name", "x", "f"], "--name"),
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
fn pane_points_at_attach() {
    let out = run(&["pane", "/tmp/x/sock", "3"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stderr),
        format!("{NAME}: pane was replaced by attach --pane (this came from an older daemon)\n")
    );
}

#[test]
fn a_command_word_naming_a_file_says_so() {
    let dir = std::env::temp_dir().join(format!("{NAME}-cli-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("version"), "").unwrap();
    let out = run_in(&dir, &["version"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out.stdout), neorex::cli::version());
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
        .env(neorex::daemon::dir::tmpdir_var(), tmpdir)
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
            neorex::daemon::dir::tmpdir_var()
        )
    );
    std::fs::remove_dir_all(&tmp).unwrap();
}

/// `daemon` with `args`, left running: it says `ok` once it listens, Rex or
/// not, then waits for the launcher's pane. With the line it said.
fn listening_daemon(tmpdir: &Path, args: &[&str]) -> (std::process::Child, String) {
    use std::io::BufRead;
    let mut child = Command::new(exe())
        .arg("daemon")
        .args(args)
        .env(neorex::daemon::dir::tmpdir_var(), tmpdir)
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
    use neorex::attach::{Accept, End, Ending, Frame, Message, Purpose, Transport};
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
        v: neorex::attach::ATTACH_PROTOCOL_VERSION,
        build: neorex::attach::build().to_string(),
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
    let docs = format!("{}/blob/main/docs/config.md", neorex::cli::repo_url());

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
        meta.starts_with(&format!("---@meta\n-- {}\n", neorex::attach::build())),
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
