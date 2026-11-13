//! The hidden `provider builtin NAME CAPABILITY` command (ADR 0016), as a
//! process: its `<NAME>_*` variables in, its stdout and exit status out.

use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

mod support;
use support::*;

/// `<NAME>_<var>`.
fn var(name: &str) -> String {
    format!("{}_{name}", NAME.to_ascii_uppercase())
}

/// `provider builtin` and `words`, with `vars` (`<NAME>_` left out) as
/// the only `<NAME>_*` variables.
fn builtin(words: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(exe());
    cmd.args(["provider", "builtin"]).args(words);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with(&var("")) {
            cmd.env_remove(k);
        }
    }
    for (k, v) in vars {
        cmd.env(var(k), v);
    }
    cmd.stdin(Stdio::null()).output().unwrap()
}

/// What a pane provider table gives every capability.
const RUN_BY_A_TABLE: &[(&str, &str)] = &[("PROTOCOL", "1"), ("CAPABILITY", "find")];

#[test]
fn an_unknown_name_capability_or_extra_word_is_unsupported() {
    for words in [
        &[][..],
        &["tmux"],
        &["zellij", "find"],
        &["tmux", "open"],
        &["tmux", "nope"],
        &["tmux", "find", "extra"],
    ] {
        let out = builtin(words, RUN_BY_A_TABLE);
        assert_eq!(out.status.code(), Some(69), "{words:?}");
        assert!(out.stdout.is_empty(), "{words:?}");
        assert!(!out.stderr.is_empty(), "{words:?}");
    }
}

#[test]
fn run_by_hand_without_the_variables_it_is_a_usage_error() {
    let out = builtin(&["tmux", "find"], &[]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.contains("pane provider table"), "{err}");
    assert!(err.contains(&format!("{NAME} provider show tmux")), "{err}");
}

#[test]
fn it_is_hidden_from_help() {
    for args in [
        &["--help"][..],
        &["help"],
        &["help", "debug"],
        &["provider", "--help"],
        &["provider", "-h"],
        &["provider", "check", "--help"],
        &["provider", "show", "--help"],
        &["provider", "types", "--help"],
        // the usage error that lists provider's words
        &["provider"],
    ] {
        let out = Command::new(exe()).args(args).output().unwrap();
        let shown = format!("{}{}", text(&out.stdout), text(&out.stderr));
        assert!(!shown.is_empty(), "{args:?}");
        assert!(!shown.contains("builtin"), "{args:?}: {shown}");
    }
}

/// A new pty's slave's path: a tty of no tmux server. The pty lives as
/// long as the fds returned.
fn other_tty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd, String) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: both fds are written; the rest may be null.
    let r = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(0, r);
    // SAFETY: both are ours, owned from here.
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

/// `provider builtin tmux find` for an attach with tty `tty`, `TMUX`
/// being `tmux` and `path` the `PATH`.
fn find(tty: &str, tmux: &str, path: &std::ffi::OsStr) -> Output {
    let mut cmd = Command::new(exe());
    cmd.args(["provider", "builtin", "tmux", "find"])
        .env(var("PROTOCOL"), "1")
        .env(var("CAPABILITY"), "find")
        .env(var("TTY"), tty)
        .env("TMUX", tmux)
        .env("PATH", path)
        .env_remove("TMUX_PANE");
    cmd.stdin(Stdio::null()).output().unwrap()
}

#[test]
fn find_accepts_a_pane_of_the_server_and_refuses_a_tty_that_isnt_one() {
    if !have_tmux() {
        return;
    }
    let dir = scratch("tmux-find");
    let tmux = TmuxServer::start(&dir, &[], &[]);
    tmux.run(&["split-window", "-d", "-t", "%0"]);
    let tty = tmux.run(&["display-message", "-p", "-t", "%1", "#{pane_tty}"]);
    let path = std::env::var_os("PATH").unwrap();
    let out = find(&tty, &tmux.tmux_var(), &path);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), "%1\n");

    let (_master, _slave, other) = other_tty();
    let out = find(&other, &tmux.tmux_var(), &path);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    // no tty at all
    let out = find("", &tmux.tmux_var(), &path);
    assert_eq!(out.status.code(), Some(1));
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn find_refuses_a_tmux_older_than_3_2_naming_its_version() {
    let dir = scratch("tmux-old");
    let fake = dir.join("tmux");
    std::fs::write(&fake, "#!/bin/sh\necho 'tmux 3.1c'\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (_master, _slave, tty) = other_tty();
    let out = find(&tty, "/tmp/nowhere,1,0", dir.as_os_str());
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.contains("tmux 3.1c is too old"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_float_is_unsupported_below_tmux_3_7_naming_its_version() {
    let dir = scratch("tmux-nofloat");
    let fake = dir.join("tmux");
    // a tmux 3.6 that fails anything but -V, so only the version is asked
    std::fs::write(
        &fake,
        "#!/bin/sh\n[ \"$1\" = -V ] && echo 'tmux 3.6' && exit 0\nexit 1\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(exe())
        .args(["provider", "builtin", "tmux", "open_float"])
        .env(var("PROTOCOL"), "1")
        .env(var("CAPABILITY"), "open_float")
        .env(var("ATTACH"), "/bin/x attach")
        .env(var("ORIGIN"), "%0")
        .env(var("X"), "1")
        .env(var("Y"), "1")
        .env(var("COLS"), "20")
        .env(var("ROWS"), "5")
        .env("TMUX", "/tmp/nowhere,1,0")
        .env("PATH", &dir)
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(69), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(err.contains("tmux 3.6 has no floating panes"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
