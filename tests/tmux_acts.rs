//! The built-in tmux's acts (ADR 0016), each run as `provider builtin tmux
//! CAPABILITY` against a private tmux server: what it does to the server.

use std::process::{Command, Output, Stdio};

use neopane::name::NAME;

mod support;
use support::*;

/// `provider builtin tmux cap` on `tmux`'s server, with `vars` (`<NAME>_`
/// left out) as its only `<NAME>_*` variables besides the protocol's.
fn act(tmux: &TmuxServer, cap: &str, vars: &[(&str, &str)]) -> Output {
    let upper = NAME.to_ascii_uppercase();
    let mut cmd = Command::new(exe());
    cmd.args(["provider", "builtin", "tmux", cap]);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with(&format!("{upper}_")) {
            cmd.env_remove(k);
        }
    }
    cmd.env(format!("{upper}_PROTOCOL"), "1")
        .env(format!("{upper}_CAPABILITY"), cap)
        .env("TMUX", tmux.tmux_var())
        .env_remove("TMUX_PANE");
    for (k, v) in vars {
        cmd.env(format!("{upper}_{k}"), v);
    }
    cmd.stdin(Stdio::null()).output().unwrap()
}

/// `act`, which must be done.
fn done(tmux: &TmuxServer, cap: &str, vars: &[(&str, &str)]) -> String {
    let out = act(tmux, cap, vars);
    assert_eq!(out.status.code(), Some(0), "{cap}: {}", text(&out.stderr));
    text(&out.stdout).trim().to_string()
}

/// Pane `p`'s `#{format}`.
fn of(tmux: &TmuxServer, p: &str, format: &str) -> String {
    tmux.run(&["display-message", "-p", "-t", p, format])
}

/// A new pane split off `target` with `args`: its id.
fn split(tmux: &TmuxServer, target: &str, args: &[&str]) -> String {
    let mut all = vec!["split-window", "-d", "-P", "-F", "#{pane_id}", "-t", target];
    all.extend(args);
    tmux.run(&all)
}

/// A private server, its window 80x24: `%0`.
fn server(name: &str) -> (TmuxServer, std::path::PathBuf) {
    let dir = scratch(name);
    let tmux = TmuxServer::start(&dir, &[], &[]);
    (tmux, dir)
}

#[test]
fn resize_through_the_last_pane_where_every_pane_before_has_its_own_split() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-resize");
    // ((P | A) / (X | Y)) | C
    let p = "%0";
    let c = split(&tmux, p, &["-h"]);
    let x = split(&tmux, p, &["-v"]);
    split(&tmux, p, &["-h"]);
    let y = split(&tmux, &x, &["-h"]);
    let resize = |terminal: &str, origin: &str, dir: &str, size: &str| {
        act(
            &tmux,
            "resize",
            &[
                ("TERMINAL", terminal),
                ("ORIGIN", origin),
                ("DIRECTION", dir),
                ("SIZE", size),
                ("RATIO", "0.5"),
            ],
        )
    };
    let out = resize(&c, p, "right", "30");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(of(&tmux, &c, "#{pane_width}"), "30");
    // the column, left of C, gets 50 of the 79 cells
    let out = resize(&x, &c, "left", "50");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(of(&tmux, &c, "#{pane_width}"), "29");
    let width = |p: &str| of(&tmux, p, "#{pane_width}").parse::<u32>().unwrap();
    assert_eq!(width(&x) + 1 + width(&y), 50);

    // the column | C border, with D after C: no pane moves it
    let d = split(&tmux, &c, &["-h"]);
    let out = resize(&c, p, "right", "20");
    assert_eq!(out.status.code(), Some(1));
    let err = text(&out.stderr);
    assert!(err.starts_with("tmux resize: "), "{err}");
    // C | D, through C
    let out = resize(&d, &c, "right", "10");
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(of(&tmux, &d, "#{pane_width}"), "10");
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn place_beside_moves_the_pane_at_its_size_and_leaves_focus_alone() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-place");
    // %0 | B | C, then C to the left of %0, 20 wide
    let b = split(&tmux, "%0", &["-h"]);
    let c = split(&tmux, &b, &["-h"]);
    let place = |terminal: &str, origin: &str, dir: &str, size: &str| {
        done(
            &tmux,
            "place_beside",
            &[
                ("TERMINAL", terminal),
                ("ORIGIN", origin),
                ("DIRECTION", dir),
                ("SIZE", size),
                ("RATIO", "0.25"),
            ],
        )
    };
    place(&c, "%0", "left", "20");
    assert_eq!(of(&tmux, &c, "#{pane_left} #{pane_width}"), "0 20");
    assert_eq!(of(&tmux, "%0", "#{pane_left}"), "21");
    assert_eq!(of(&tmux, "%0", "#{pane_active}"), "1");
    // below B, from another window: B's window keeps focus
    let other = tmux.run(&["new-window", "-d", "-P", "-F", "#{pane_id}"]);
    place(&other, &b, "down", "8");
    assert_eq!(
        of(&tmux, &other, "#{window_id} #{pane_height}"),
        of(&tmux, &b, "#{window_id} 8")
    );
    assert_eq!(
        tmux.run(&["list-windows", "-F", "#{window_id}"])
            .lines()
            .count(),
        1
    );
    assert_eq!(of(&tmux, "%0", "#{pane_active}"), "1");
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}
