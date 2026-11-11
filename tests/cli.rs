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
        .env_remove("REX_SERVER")
        .env_remove("REX_SESSION")
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

    // its directory claimed and its log made, before it finds no Rex
    let out = daemon(&tmp, &["work", "--dedup", "--"]);
    assert_eq!(out.status.code(), Some(1));
    let log = std::fs::read_to_string(base.join("work/log")).unwrap();
    assert!(log.contains("run it in a Rex pane"), "{log:?}");
    assert_eq!(
        text(&out.stdout),
        format!(
            "error work didn't start: run it in a Rex pane (log: {})\n",
            base.join("work/log").display()
        )
    );
    // the next claim keeps it as log.1, and says where its own is
    let out = daemon(&tmp, &["work", "--"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        text(&out.stdout),
        format!(
            "error work didn't start: run it in a Rex pane (log: {})\n",
            base.join("work/log").display()
        )
    );
    assert_eq!(
        log,
        std::fs::read_to_string(base.join("work/log.1")).unwrap()
    );

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

/// `daemon` with `args` and Rex's variables set, left running: it says `ok`
/// once it listens, then waits for the launcher's pane. With the line it
/// said.
fn listening_daemon(tmpdir: &Path, args: &[&str]) -> (std::process::Child, String) {
    use std::io::BufRead;
    let mut child = Command::new(exe())
        .arg("daemon")
        .args(args)
        .env(neorex::daemon::dir::tmpdir_var(), tmpdir)
        .env("REX_SERVER", "ws://unused")
        .env("REX_SESSION", "unused")
        .env_remove("REX_BLOCK")
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
