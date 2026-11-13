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

#[test]
fn in_a_tmux_pane_a_float_opens_a_tmux_floating_pane_on_its_group() {
    a_float_lands_on_its_group("off");
}

/// With the status line on top, `%0`'s rect starts on the window's top
/// row, above its content, and the group is placed from it.
#[test]
fn under_a_top_status_line_a_float_still_lands_on_its_group() {
    a_float_lands_on_its_group("top");
}

/// A float opened in a daemon in a tmux window whose
/// `pane-border-status` is `border_status` gets a tmux floating pane on
/// its group, and Neovim leaves the float where it opened it.
fn a_float_lands_on_its_group(border_status: &str) {
    if !have_nvim() || !have_tmux() {
        return;
    }
    // floating panes came in 3.7; below it open_float exits 69
    if tmux_version().is_some_and(|v| v < (3, 7)) {
        return;
    }
    let tmp = scratch(&format!("tmux-fl-{border_status}"));
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
    tmux.run(&["set", "-g", "pane-border-status", border_status]);
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
    // its content lands on the group: %0's rect is at the window's top left
    assert_eq!(float[2..], ["10", "3", "20", "5"], "{now:?}: {}", log());
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
    // and Neovim's float stays where it was opened: its group followed no move
    std::thread::sleep(Duration::from_millis(500));
    let at = remote_expr(
        &sock,
        &format!("luaeval('vim.api.nvim_win_get_config({win}).row')"),
    );
    assert_eq!(at.as_deref(), Some("3"), "{}", log());

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

#[test]
fn in_a_tmux_pane_with_rexs_variables_leaked_into_it_the_default_proves_tmux() {
    if !have_nvim() || !have_tmux() {
        return;
    }
    let tmp = scratch("tmux-nest");
    let exe = exe();
    // a tmux run in a Rex pane, its server holding Rex's variables
    let tmux = TmuxServer::start(
        &tmp,
        &[
            (&neopane::daemon::dir::tmpdir_var(), tmp.as_os_str()),
            ("XDG_CONFIG_HOME", tmp.as_os_str()),
            ("XDG_STATE_HOME", tmp.as_os_str()),
            (
                "REX_SERVER",
                std::ffi::OsStr::new("unix:///nowhere/rex.sock"),
            ),
            ("REX_SESSION", std::ffi::OsStr::new("session:s")),
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
    let status = || json(&under(&tmp, &["status", "--json", "--name", "work"]));

    assert!(
        until_expr(&sock, "1", |_| true).is_some(),
        "work's Neovim never answered: {}",
        log()
    );
    let s = until(|s: &serde_json::Value| s["provider"] == "tmux", status);
    assert_eq!(s["provider"], "tmux", "{}", log());
    assert!(
        log().contains("inside.tmux: yes, tmux pane %0"),
        "{}",
        log()
    );

    drop(ending);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn with_no_config_a_guess_find_refuses_is_logged_and_never_warned_of() {
    if !have_nvim() || !have_tmux() {
        return;
    }
    let tmp = scratch("tmux-leak");
    let tmux = TmuxServer::start(&tmp, &[], &[]);
    let tmux_var = tmux.tmux_var();
    // a terminal tmux doesn't run, with tmux's variables leaked into it
    let helper = in_pty_with(
        &tmp,
        &tmp,
        &["--name", "work"],
        &[
            ("TERM_PROGRAM", std::ffi::OsStr::new("tmux")),
            ("TMUX", std::ffi::OsStr::new(&tmux_var)),
        ],
        160,
    );
    let sock = base_of(&tmp).join("work/nvim.sock");
    let ending = Ending {
        sock: sock.clone(),
        helpers: vec![helper],
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("work/log")).unwrap_or_default();
    let status = || json(&under(&tmp, &["status", "--json", "--name", "work"]));

    assert!(
        until_expr(&sock, "1", |_| true).is_some(),
        "work's Neovim never answered: {}",
        log()
    );
    let s = until(
        |s: &serde_json::Value| s["provider"] != "(choosing)",
        status,
    );
    assert_eq!(s["provider"], "none (find refused)", "{}", log());
    let logged = until(|l: &String| l.contains("not warned of"), log);
    assert!(
        logged
            .contains("tmux: find didn't confirm this terminal (exit 1: tmux find: this terminal"),
        "{logged}"
    );
    // give a warning time to show, then look
    std::thread::sleep(Duration::from_millis(500));
    let messages = remote_expr(&sock, "execute('messages')").unwrap_or_default();
    assert!(!messages.contains("no pane provider"), "{messages}");

    drop(ending);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// With a user's client attached, `select-pane` in tmux moves Neovim's
/// current window to that pane's window, through `events`; its waiters
/// are no client, and a pane still hears focus-out.
#[test]
fn in_tmux_3_8_select_pane_moves_neovims_current_window_and_no_client_is_added() {
    if !have_nvim() || !have_tmux() || !have_tmux_events() {
        return;
    }
    let tmp = scratch("tmux-events");
    let exe = exe();
    let tmux = TmuxServer::start(
        &tmp,
        &[
            (&neopane::daemon::dir::tmpdir_var(), tmp.as_os_str()),
            ("XDG_CONFIG_HOME", tmp.as_os_str()),
            ("XDG_STATE_HOME", tmp.as_os_str()),
        ],
        &[exe.to_str().unwrap(), "--name", "ev"],
    );
    tmux.run(&["set", "-g", "focus-events", "on"]);
    let mut client = tmux.attach();
    let clients = || tmux.run(&["list-clients", "-F", "#{client_name}"]);
    let attached = || tmux.run(&["display-message", "-p", "-t", "%0", "#{session_attached}"]);
    assert_eq!(
        until(|c: &String| c.lines().count() == 1, clients)
            .lines()
            .count(),
        1
    );
    let sock = base_of(&tmp).join("ev/nvim.sock");
    let ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("ev/log")).unwrap_or_default();
    let status = || json(&under(&tmp, &["status", "--json", "--name", "ev"]));
    let waiters = || tmux.run(&["wait-for", "-E", "-l", "window-pane-changed"]);

    until_expr(&sock, "1", |_| true).expect("ev's Neovim never answered");
    let s = until(|s: &serde_json::Value| s["provider"] == "tmux", status);
    assert_eq!(s["provider"], "tmux", "{}", log());
    let w = until(|w: &String| !w.is_empty(), waiters);
    assert!(!w.is_empty(), "events never waited: {}", log());
    let first = remote_expr(&sock, "win_getid()").unwrap();

    // :vsplit's window is current; its pane isn't tmux's active one
    remote_expr(&sock, "execute('vsplit')").unwrap();
    let s = until(|s: &serde_json::Value| s["panes"] == 2, status);
    assert_eq!(s["panes"], 2, "its attach never came: {}", log());
    let second = remote_expr(&sock, "win_getid()").unwrap();
    assert_ne!(first, second);
    let panes = tmux.run(&["list-panes", "-t", "%0", "-F", "#{pane_id}"]);
    let new = panes.lines().find(|p| *p != "%0").unwrap().to_string();

    // back in Neovim to the first, which is tmux's active pane; then to the
    // new pane and back in tmux, Neovim following each move
    remote_expr(&sock, "execute('wincmd p')").unwrap();
    assert_eq!(remote_expr(&sock, "win_getid()"), Some(first.clone()));
    for (pane, win) in [(new.as_str(), &second), ("%0", &first), (&new, &second)] {
        tmux.run(&["select-pane", "-t", pane]);
        let now = until_expr(&sock, "win_getid()", |w| w == win);
        assert_eq!(now.as_deref(), Some(win.as_str()), "{pane}: {}", log());
    }

    // the waiters are no client
    assert!(!waiters().is_empty(), "events stopped: {}", log());
    assert_eq!(clients().lines().count(), 1, "{}", clients());
    assert_eq!(attached(), "1");

    // a pane that asked for focus reports still hears focus-out
    let heard = tmp.join("heard");
    let recorder = format!(
        "printf '\\033[?1004h'; stty raw -echo; exec cat > {}",
        heard.display()
    );
    let session = tmux.run(&["display-message", "-p", "-t", "%0", "#{session_id}:"]);
    let pane = tmux.run(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &session,
        "sh",
        "-c",
        &recorder,
    ]);
    std::thread::sleep(Duration::from_millis(500));
    {
        use std::io::Write;
        client.keys.write_all(b"\x1b[O").unwrap();
    }
    let got = until(
        |h: &Vec<u8>| h.windows(3).any(|w| w == b"\x1b[O"),
        || std::fs::read(&heard).unwrap_or_default(),
    );
    assert!(
        got.windows(3).any(|w| w == b"\x1b[O"),
        "{pane} heard {got:?}: {}",
        log()
    );

    drop(client);
    drop(ending);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}
