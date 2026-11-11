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
