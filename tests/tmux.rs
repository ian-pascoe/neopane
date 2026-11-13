//! The built-in tmux pane provider, live (ADR 0016): a daemon started in
//! a pane of a private tmux server, with no config, gives each Neovim
//! window a tmux pane of its own.

use std::time::{Duration, Instant};

mod support;
use support::*;

/// Waits up to 20 s for `f` to make `want` true; what it last made.
fn until<T>(want: impl Fn(&T) -> bool, f: impl Fn() -> T) -> T {
    let due = Instant::now() + Duration::from_secs(20);
    loop {
        let got = f();
        if want(&got) || Instant::now() > due {
            return got;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn in_a_tmux_pane_a_split_opens_a_tmux_pane_and_close_removes_it() {
    if !have_nvim() || !have_tmux() {
        return;
    }
    let tmp = scratch("tmux-live");
    let exe = exe();
    let tmux = TmuxServer::start(
        &tmp,
        &[
            (&neopane::daemon::dir::tmpdir_var(), tmp.as_os_str()),
            ("XDG_CONFIG_HOME", tmp.as_os_str()),
            ("XDG_STATE_HOME", tmp.as_os_str()),
        ],
        &[exe.to_str().unwrap(), "--name", "work"],
    );
    let sock = base_of(&tmp).join("work/nvim.sock");
    let ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("work/log")).unwrap_or_default();
    let panes = || {
        tmux.run(&["list-panes", "-a", "-F", "#{pane_id}"])
            .lines()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let status = || json(&under(&tmp, &["status", "--json", "--name", "work"]));

    until_expr(&sock, "1", |_| true).expect("work's Neovim never answered");
    let s = until(|s: &serde_json::Value| s["provider"] == "tmux", status);
    assert_eq!(s["provider"], "tmux", "{}", log());
    assert_eq!(panes(), ["%0"]);

    // :vsplit opens a pane beside it, running the attach
    remote_expr(&sock, "execute('vsplit')").unwrap();
    let now = until(|p: &Vec<String>| p.len() == 2, panes);
    assert_eq!(now.len(), 2, "no pane opened: {}", log());
    let new = now.iter().find(|p| *p != "%0").unwrap();
    let started = tmux.run(&["display-message", "-p", "-t", new, "#{pane_start_command}"]);
    assert!(
        started.contains(" attach --address "),
        "{new} runs {started}"
    );
    let s = until(|s: &serde_json::Value| s["panes"] == 2, status);
    assert_eq!(s["panes"], 2, "its attach never came: {}", log());
    // beside: the window is split left and right
    let lefts = tmux.run(&["list-panes", "-t", "%0", "-F", "#{pane_left}"]);
    assert_eq!(lefts.lines().filter(|l| *l != "0").count(), 1, "{lefts}");

    // :close removes it
    remote_expr(&sock, "execute('close')").unwrap();
    let now = until(|p: &Vec<String>| p.len() == 1, panes);
    assert_eq!(now, ["%0"], "{}", log());

    drop(ending);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}
