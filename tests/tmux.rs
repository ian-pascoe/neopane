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

/// The installed tmux's major and minor version, from `tmux -V`, such as
/// `(3, 7)` for `tmux 3.7c`; None if it can't be read.
fn tmux_version() -> Option<(u32, u32)> {
    let out = std::process::Command::new("tmux").arg("-V").output().ok()?;
    let shown = String::from_utf8_lossy(&out.stdout);
    let digits = shown.trim_start_matches(|c: char| !c.is_ascii_digit());
    let (major, rest) = digits.split_once('.')?;
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

#[test]
fn in_a_tmux_pane_a_float_opens_a_tmux_floating_pane_on_its_group() {
    if !have_nvim() || !have_tmux() {
        return;
    }
    // floating panes came in 3.7; below it open_float exits 69
    if tmux_version().is_some_and(|v| v < (3, 7)) {
        return;
    }
    let tmp = scratch("tmux-fl");
    let exe = exe();
    let tmux = TmuxServer::start(
        &tmp,
        &[
            (&neopane::daemon::dir::tmpdir_var(), tmp.as_os_str()),
            ("XDG_CONFIG_HOME", tmp.as_os_str()),
            ("XDG_STATE_HOME", tmp.as_os_str()),
        ],
        &[exe.to_str().unwrap(), "--name", "fl"],
    );
    let sock = base_of(&tmp).join("fl/nvim.sock");
    let ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("fl/log")).unwrap_or_default();
    let floats = || {
        tmux.run(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_id} #{?pane_floating_flag,1,0} #{pane_left} #{pane_top} #{pane_width} #{pane_height}",
        ])
        .lines()
        .filter(|l| l.split(' ').nth(1) == Some("1"))
        .map(String::from)
        .collect::<Vec<_>>()
    };
    let status = || json(&under(&tmp, &["status", "--json", "--name", "fl"]));

    until_expr(&sock, "1", |_| true).expect("fl's Neovim never answered");
    let s = until(|s: &serde_json::Value| s["provider"] == "tmux", status);
    assert_eq!(s["provider"], "tmux", "{}", log());

    // a float 20x5 entered, as a picker is, at row 3, column 10 of the
    // screen, which is %0's
    let win = remote_expr(
        &sock,
        "luaeval('vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, \
         {relative = \"editor\", row = 3, col = 10, width = 20, height = 5})')",
    )
    .unwrap();
    let now = until(|f: &Vec<String>| f.len() == 1, floats);
    assert_eq!(now.len(), 1, "no float opened: {}", log());
    let float = now[0].split(' ').collect::<Vec<_>>();
    // its content lands on the group: %0 is at the window's top left
    assert_eq!(float[2..], ["10", "3", "20", "5"], "{now:?}");
    let started = tmux.run(&[
        "display-message",
        "-p",
        "-t",
        float[0],
        "#{pane_start_command}",
    ]);
    assert!(started.contains(" attach --address "), "{started}");
    let s = until(|s: &serde_json::Value| s["panes"] == 2, status);
    assert_eq!(s["panes"], 2, "its attach never came: {}", log());

    // closing the float closes its pane
    remote_expr(
        &sock,
        &format!("luaeval('vim.api.nvim_win_close({win}, true)')"),
    )
    .unwrap();
    let now = until(|f: &Vec<String>| f.is_empty(), floats);
    assert!(now.is_empty(), "{now:?}: {}", log());

    drop(ending);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}
