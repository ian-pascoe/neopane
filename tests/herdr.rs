//! The built-in herdr pane provider, live (ADR 0016, ADR 0017): a daemon
//! started in a pane of a private `herdr server`, with no config, gives
//! each Neovim window a herdr pane of its own. A herdr client in a pty of
//! fixed size gives the server its size, as a user's would.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use neopane::name::NAME;
use serde_json::{Value, json};

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

/// The variables a daemon started in `tmp` keeps its files by.
fn daemon_env(tmp: &std::path::Path) -> Vec<(String, std::ffi::OsString)> {
    vec![
        (neopane::daemon::dir::tmpdir_var(), tmp.into()),
        ("XDG_CONFIG_HOME".into(), tmp.into()),
        ("XDG_STATE_HOME".into(), tmp.into()),
    ]
}

/// A workspace of `herdr`'s, its one tab's one pane running `command`, or
/// a shell; that pane's id.
fn workspace(herdr: &HerdrServer, cwd: &std::path::Path, command: Option<&[&str]>) -> String {
    let made = herdr.call("workspace.create", json!({ "cwd": cwd, "focus": true }));
    let shell = made["root_pane"]["pane_id"].as_str().unwrap().to_string();
    let Some(command) = command else {
        return shell;
    };
    let workspace = made["workspace"]["workspace_id"].as_str().unwrap();
    let applied = herdr.call(
        "layout.apply",
        json!({
            "workspace_id": workspace,
            "focus": true,
            "root": { "type": "pane", "command": command },
        }),
    );
    // its tab goes with it
    herdr.call("pane.close", json!({ "pane_id": shell }));
    applied["layout"]["root"]["pane_id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// The layout of the tab pane `pane` is in.
fn tab_of(herdr: &HerdrServer, pane: &str) -> Value {
    herdr.call("pane.layout", json!({ "pane_id": pane }))["layout"].clone()
}

/// Each pane of `layout`, left to right then top to bottom: its id, and
/// the pty size its rect gives an alternate-screen program, rows and
/// columns, in a tab with a split.
fn framed(layout: &Value) -> Vec<(String, (u16, u16))> {
    let cells = |p: &Value, k: &str| p["rect"][k].as_u64().unwrap();
    let mut panes = layout["panes"].as_array().unwrap().clone();
    panes.sort_by_key(|p| (cells(p, "x"), cells(p, "y")));
    panes
        .iter()
        .map(|p| {
            let rows = (cells(p, "height") - 2) as u16;
            let cols = (cells(p, "width") - 2) as u16;
            (p["pane_id"].as_str().unwrap().to_string(), (rows, cols))
        })
        .collect()
}

#[test]
fn in_a_herdr_pane_a_split_opens_a_herdr_pane_beside_it_at_its_size_and_close_removes_it() {
    if !have_nvim() || !have_herdr() {
        return;
    }
    let tmp = scratch("hd-live");
    let exe = exe();
    let env = daemon_env(&tmp);
    let env: Vec<(&str, &std::ffi::OsStr)> = env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect();
    let herdr = HerdrServer::start(&tmp, &env);
    let origin = workspace(
        &herdr,
        &tmp,
        Some(&[exe.to_str().unwrap(), "--name", "work"]),
    );
    // a client shows the focused tab, the daemon's, and gives every tab
    // its size, a moment after it attaches; herdr draws, and so lays out
    // as the screen a program is on says, only the tab a client shows
    let mut client = herdr.attach(100, 30);
    let area = || tab_of(&herdr, &origin)["area"].clone();
    let sized = until(|a: &Value| a["width"] != 120, area);
    assert_ne!(sized["width"], 120, "the client never gave its size");
    let sock = base_of(&tmp).join("work/nvim.sock");
    let ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("work/log")).unwrap_or_default();
    let status = || json(&under(&tmp, &["status", "--json", "--name", "work"]));
    let panes = || {
        tab_of(&herdr, &origin)["panes"]
            .as_array()
            .map_or(0, Vec::len)
    };

    until_expr(&sock, "1", |_| true).expect("work's Neovim never answered");
    let s = until(|s: &Value| s["provider"] == "herdr", status);
    assert_eq!(s["provider"], "herdr", "{}", log());
    assert_eq!(panes(), 1);
    let tabs = || {
        herdr.call("tab.list", json!({}))["tabs"]
            .as_array()
            .unwrap()
            .len()
    };
    assert_eq!(tabs(), 1);

    // :vsplit opens a pane on its left, a swap away; :belowright split
    // one below, none: each running the attach, in its tab, both at their
    // sizes, and :close removes it
    for (split, new_at) in [("vsplit", 0), ("belowright split", 1)] {
        // the daemon asks for half its origin, along the split, as a share
        let (rows, cols) = herdr.pty_size(&origin).expect("the origin's size");
        let (along, axis, extent) = match new_at {
            0 => (cols, "x", "width"),
            _ => (rows, "y", "height"),
        };
        let asked = f64::from(along / 2) / f64::from(along);
        remote_expr(&sock, &format!("execute('{split}')")).unwrap();
        assert_eq!(until(|n| *n == 2, panes), 2, "{split}: {}", log());
        let layout = tab_of(&herdr, &origin);
        let share = share_of(&layout, new_at, axis, extent);
        assert!(
            (share - asked).abs() <= cell(&layout, extent),
            "{split}: a share of {share}, asked {asked}: {layout}"
        );
        let sizes = framed(&layout);
        assert_eq!(sizes[1 - new_at].0, origin, "{split}: {layout}");
        let new = sizes[new_at].0.clone();
        let info = herdr.call("pane.process_info", json!({ "pane_id": new }));
        let argv = info["process_info"]["foreground_processes"][0]["argv"].to_string();
        assert!(argv.contains("\"attach\""), "{new} runs {argv}");
        let s = until(|s: &Value| s["panes"] == 2, status);
        assert_eq!(s["panes"], 2, "{split}: its attach never came: {}", log());
        // no tab left behind
        assert_eq!(tabs(), 1);
        // both ptys at their panes' sizes: the tab was laid out again
        for (pane, want) in sizes {
            let got = until(|got| *got == Some(want), || herdr.pty_size(&pane));
            let alive = client.alive();
            assert_eq!(
                got,
                Some(want),
                "{split}: {pane} in {layout} (client alive: {alive}): {}",
                log()
            );
        }
        remote_expr(&sock, "execute('close')").unwrap();
        assert_eq!(until(|n| *n == 1, panes), 1, "{split}: {}", log());
        let s = until(|s: &Value| s["panes"] == 1, status);
        assert_eq!(s["panes"], 1, "{split}: {}", log());
    }

    drop(ending);
    drop(herdr);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// The share of `layout`'s area along `axis` (`x` or `y`, `extent` being
/// `width` or `height`) that its pane at index `at` takes, panes in order
/// along that axis.
fn share_of(layout: &Value, at: usize, axis: &str, extent: &str) -> f64 {
    let mut panes = layout["panes"].as_array().unwrap().clone();
    panes.sort_by_key(|p| p["rect"][axis].as_u64().unwrap());
    let cells = panes[at]["rect"][extent].as_f64().unwrap();
    cells / layout["area"][extent].as_f64().unwrap()
}

/// One cell of `layout`'s area along `extent`, as a share.
fn cell(layout: &Value, extent: &str) -> f64 {
    1.0 / layout["area"][extent].as_f64().unwrap()
}

#[test]
fn open_beside_gives_the_new_pane_its_ratio_on_every_side() {
    if !have_herdr() {
        return;
    }
    let tmp = scratch("hd-ratio");
    let herdr = HerdrServer::start(&tmp, &[]);
    let origin = workspace(&herdr, &tmp, None);
    let up = NAME.to_ascii_uppercase();
    // the new pane first along the axis, or second
    for (direction, at, axis, extent) in [
        ("left", 0, "x", "width"),
        ("right", 1, "x", "width"),
        ("up", 0, "y", "height"),
        ("down", 1, "y", "height"),
    ] {
        let out = Command::new(exe())
            .args(["provider", "builtin", "herdr", "open_beside"])
            .env(format!("{up}_PROTOCOL"), "1")
            .env(format!("{up}_CAPABILITY"), "open_beside")
            .env(format!("{up}_ORIGIN"), &origin)
            .env(format!("{up}_DIRECTION"), direction)
            .env(format!("{up}_RATIO"), "0.3")
            .env(format!("{up}_ATTACH"), "/bin/sleep 100")
            .env(format!("{up}_TITLE"), format!("t-{direction}"))
            .env("HERDR_SOCKET_PATH", &herdr.socket)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{direction}: {}",
            text(&out.stderr)
        );
        let new = text(&out.stdout).trim().to_string();
        let layout = tab_of(&herdr, &origin);
        let mut panes = layout["panes"].as_array().unwrap().clone();
        panes.sort_by_key(|p| p["rect"][axis].as_u64().unwrap());
        assert_eq!(panes[at]["pane_id"], new.as_str(), "{direction}: {layout}");
        // RATIO is the new pane's share, on either side of a swap
        let share = share_of(&layout, at, axis, extent);
        assert!(
            (share - 0.3).abs() <= cell(&layout, extent),
            "{direction}: a share of {share}: {layout}"
        );
        herdr.call("pane.close", json!({ "pane_id": new }));
    }

    drop(herdr);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn a_terminal_herdr_doesnt_run_with_a_panes_variables_copied_in_is_refused() {
    if !have_nvim() || !have_herdr() {
        return;
    }
    let tmp = scratch("hd-leak");
    let herdr = HerdrServer::start(&tmp, &[]);
    let pane = workspace(&herdr, &tmp, None);
    let helper = in_pty_with(
        &tmp,
        &tmp,
        &["--name", "work"],
        &[
            ("TERM_PROGRAM", std::ffi::OsStr::new("herdr")),
            ("HERDR_ENV", std::ffi::OsStr::new("1")),
            ("HERDR_PANE_ID", std::ffi::OsStr::new(&pane)),
            ("HERDR_SOCKET_PATH", herdr.socket.as_os_str()),
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
    let s = until(|s: &Value| s["provider"] != "(choosing)", status);
    assert_eq!(s["provider"], "none (find refused)", "{}", log());
    let logged = until(|l: &String| l.contains("herdr find: this terminal"), log);
    assert!(
        logged.contains(&format!("isn't herdr pane {pane}'s (/dev/")),
        "{logged}"
    );

    drop(ending);
    drop(herdr);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn open_beside_a_zoomed_pane_exits_1_and_leaves_no_tab_behind() {
    if !have_herdr() {
        return;
    }
    let tmp = scratch("hd-zoom");
    let herdr = HerdrServer::start(&tmp, &[]);
    let origin = workspace(&herdr, &tmp, None);
    herdr.call(
        "pane.split",
        json!({ "target_pane_id": origin, "direction": "right" }),
    );
    let zoomed = herdr.call("pane.zoom", json!({ "pane_id": origin, "mode": "on" }));
    assert_eq!(zoomed["zoom"]["zoomed"], true, "{zoomed}");
    let tab = tab_of(&herdr, &origin)["tab_id"].clone();
    let snapshot = || herdr.call("session.snapshot", json!({}))["snapshot"].clone();
    let before = snapshot();

    let up = NAME.to_ascii_uppercase();
    let out = Command::new(exe())
        .args(["provider", "builtin", "herdr", "open_beside"])
        .env(format!("{up}_PROTOCOL"), "1")
        .env(format!("{up}_CAPABILITY"), "open_beside")
        .env(format!("{up}_ORIGIN"), &origin)
        .env(format!("{up}_DIRECTION"), "left")
        .env(format!("{up}_RATIO"), "0.5")
        .env(format!("{up}_ATTACH"), "/bin/sleep 100")
        .env("HERDR_SOCKET_PATH", &herdr.socket)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty());
    let err = text(&out.stderr);
    assert!(
        err.contains(&format!("herdr tab {} is zoomed", tab.as_str().unwrap())),
        "{err}"
    );
    let after = snapshot();
    assert_eq!(after["tabs"], before["tabs"]);
    assert_eq!(
        after["panes"].as_array().unwrap().len(),
        before["panes"].as_array().unwrap().len()
    );

    drop(herdr);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A daemon in a pane of a private herdr server, with no config, and a
/// user's client attached, 100x30: everything ends as it drops, the
/// server before its directory goes, as it may be another process after a
/// live handoff, stopped only through its socket.
struct Live {
    client: Option<HerdrClient>,
    herdr: HerdrServer,
    ending: Ending,
    /// The daemon's own pane.
    origin: String,
    tmp: std::path::PathBuf,
}

impl Live {
    /// None without `nvim` or `herdr`.
    fn start(test: &str) -> Option<Live> {
        if !have_nvim() || !have_herdr() {
            return None;
        }
        let tmp = scratch(test);
        let exe = exe();
        let env = daemon_env(&tmp);
        let env: Vec<(&str, &std::ffi::OsStr)> = env
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_os_str()))
            .collect();
        let herdr = HerdrServer::start(&tmp, &env);
        let origin = workspace(
            &herdr,
            &tmp,
            Some(&[exe.to_str().unwrap(), "--name", "work"]),
        );
        let client = herdr.attach(100, 30);
        let area = || tab_of(&herdr, &origin)["area"].clone();
        let sized = until(|a: &Value| a["width"] != 120, area);
        assert_ne!(sized["width"], 120, "the client never gave its size");
        let sock = base_of(&tmp).join("work/nvim.sock");
        let live = Live {
            client: Some(client),
            herdr,
            ending: Ending {
                sock,
                helpers: Vec::new(),
                tmp: tmp.clone(),
            },
            origin,
            tmp,
        };
        let answered = until_expr(&live.ending.sock, "1", |_| true);
        assert!(
            answered.is_some(),
            "work's Neovim never answered: {}",
            live.log()
        );
        let s = until(|s: &Value| s["provider"] == "herdr", || live.status());
        assert_eq!(s["provider"], "herdr", "{}", live.log());
        Some(live)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(base_of(&self.tmp).join("work/log")).unwrap_or_default()
    }

    fn status(&self) -> Value {
        json(&under(&self.tmp, &["status", "--json", "--name", "work"]))
    }

    /// Runs `cmd` in Neovim, as `:execute` does.
    fn ex(&self, cmd: &str) {
        remote_expr(&self.ending.sock, &format!("execute('{cmd}')")).unwrap();
    }

    /// Types `keys` in Neovim's Normal mode, mappings and all, `\<C-w>`
    /// as Vim's double-quoted strings read it.
    fn normal(&self, keys: &str) {
        remote_expr(&self.ending.sock, &format!("execute(\"normal {keys}\")")).unwrap();
    }

    /// Neovim's current window.
    fn win(&self) -> String {
        remote_expr(&self.ending.sock, "win_getid()").unwrap()
    }

    /// Waits for Neovim's current window to be `win`; what it last was.
    fn until_win(&self, win: &str) -> Option<String> {
        until_expr(&self.ending.sock, "win_getid()", |w| w == win)
    }

    /// The herdr server's focused pane.
    fn focused(&self) -> String {
        let snapshot = self.herdr.call("session.snapshot", json!({}));
        snapshot["snapshot"]["focused_pane_id"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// Waits for `pane` to be herdr's focus; what it last was.
    fn until_focused(&self, pane: &str) -> String {
        until(|f: &String| f == pane, || self.focused())
    }

    /// Waits for Neovim's tab page `n` and herdr's focus on `pane`, then
    /// lets the reports after the focus act come back: a report read
    /// before the act landed mustn't send either back.
    fn stays_on(&self, n: u32, pane: &str) {
        let want = (n.to_string(), pane.to_string());
        let now = || {
            let tab = remote_expr(&self.ending.sock, "tabpagenr()").unwrap_or_default();
            (tab, self.focused())
        };
        assert_eq!(until(|got| *got == want, now), want, "{}", self.log());
        std::thread::sleep(Duration::from_millis(1000));
        assert_eq!(now(), want, "it went back: {}", self.log());
    }

    /// `:vsplit`, once its pane has attached and been placed, and the
    /// daemon's focus of it has landed: the new window's pane, left of the
    /// origin. A move made before then is undone by that focus, asked as
    /// the window opened and run once the pane is there.
    fn vsplit(&self) -> String {
        self.ex("vsplit");
        let s = until(|s: &Value| s["panes"] == 2, || self.status());
        assert_eq!(s["panes"], 2, "its attach never came: {}", self.log());
        // it may attach before it is moved beside the origin, then
        // swapped to its left
        let tab = || tab_of(&self.herdr, &self.origin);
        let placed = |l: &Value| {
            let sizes = framed(l);
            sizes.len() == 2 && sizes[1].0 == self.origin
        };
        let layout = until(placed, tab);
        let sizes = framed(&layout);
        assert_eq!(sizes.len(), 2, "{layout}");
        assert_eq!(sizes[1].0, self.origin, "{layout}");
        let left = sizes[0].0.clone();
        assert_eq!(self.until_focused(&left), left, "{}", self.log());
        std::thread::sleep(Duration::from_millis(500));
        left
    }

    /// Presses herdr's prefix, then `key`, in the user's client.
    fn herdr_keys(&mut self, key: &[u8]) {
        use std::io::Write;
        let client = self.client.as_mut().expect("a client");
        client.keys.write_all(b"\x02").unwrap();
        std::thread::sleep(Duration::from_millis(100));
        client.keys.write_all(key).unwrap();
    }
}

impl Drop for Live {
    /// Quits Neovim and stops the server, waiting for each to go, before
    /// their directory does: Neovim writes its shada as it quits, and
    /// after a live handoff the server is a process the test didn't start,
    /// which writes its session as it stops.
    fn drop(&mut self) {
        let sock = &self.ending.sock;
        let _ = remote_expr(sock, "execute('qa!')");
        let left = until(|there: &bool| !there, || sock.exists());
        if left {
            eprintln!("Neovim at {} didn't quit", sock.display());
        }
        let _ = self.herdr.try_call("server.stop", json!({}));
        let left = until(|there: &bool| !there, || self.herdr.socket.exists());
        if left {
            eprintln!("herdr at {} didn't stop", self.herdr.socket.display());
        }
    }
}

/// With herdr's own keys, focus moves from pane to pane, and Neovim's
/// current window with it, through `events`.
#[test]
fn focusing_a_pane_with_herdrs_keys_moves_neovims_current_window() {
    let Some(mut live) = Live::start("hd-keys") else {
        return;
    };
    let first = live.win();
    let left = live.vsplit();
    let second = live.win();
    assert_ne!(first, second);
    let origin = live.origin.clone();
    for (key, pane, win) in [
        (b"l", &origin, &first),
        (b"h", &left, &second),
        (b"l", &origin, &first),
    ] {
        live.herdr_keys(key);
        assert_eq!(live.until_focused(pane), *pane, "{}", live.log());
        let now = live.until_win(win);
        assert_eq!(now.as_deref(), Some(win.as_str()), "{pane}: {}", live.log());
    }
}

#[test]
fn ctrl_w_l_moves_herdr_focus_right_and_not_past_the_edge() {
    let Some(live) = Live::start("hd-cw") else {
        return;
    };
    let first = live.win();
    let left = live.vsplit();
    live.normal("\\<C-w>h");
    assert_eq!(live.until_focused(&left), left, "{}", live.log());
    live.normal("\\<C-w>l");
    assert_eq!(
        live.until_focused(&live.origin),
        live.origin,
        "{}",
        live.log()
    );
    // Neovim's current window follows: the first, right of the new one
    assert_eq!(
        live.until_win(&first),
        Some(first.clone()),
        "{}",
        live.log()
    );
    // at the right edge it goes nowhere, not round to the left
    live.normal("\\<C-w>l");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(live.focused(), live.origin, "{}", live.log());
    assert_eq!(live.win(), first);
}

#[test]
fn tabnew_opens_a_herdr_tab_and_gt_focuses_it() {
    let Some(live) = Live::start("hd-tab") else {
        return;
    };
    let tabs = || {
        live.herdr.call("tab.list", json!({}))["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["tab_id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    let origin_tab = tab_of(&live.herdr, &live.origin)["tab_id"].clone();
    live.ex("tabnew");
    let now = until(|t: &Vec<String>| t.len() == 2, tabs);
    assert_eq!(now.len(), 2, "no herdr tab opened: {}", live.log());
    let tab = now.iter().find(|t| **t != origin_tab).unwrap().clone();
    let s = until(|s: &Value| s["panes"] == 2, || live.status());
    assert_eq!(s["panes"], 2, "its attach never came: {}", live.log());
    let snapshot = live.herdr.call("session.snapshot", json!({}));
    let pane = snapshot["snapshot"]["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["tab_id"] == tab.as_str())
        .unwrap()["pane_id"]
        .as_str()
        .unwrap()
        .to_string();
    let info = live
        .herdr
        .call("pane.process_info", json!({ "pane_id": pane }));
    let argv = info["process_info"]["foreground_processes"][0]["argv"].to_string();
    assert!(argv.contains("\"attach\""), "{pane} runs {argv}");
    // :tabnew focuses its tab, and stays there
    live.stays_on(2, &pane);
    live.ex("tabnext 1");
    live.stays_on(1, &live.origin);
    live.normal("gt");
    live.stays_on(2, &pane);
    // back and forth at once: each focus act runs beside the report the
    // one before it ended with, read before it landed
    for _ in 0..3 {
        live.ex("tabnext 1");
        assert_eq!(
            live.until_focused(&live.origin),
            live.origin,
            "{}",
            live.log()
        );
        live.normal("gt");
        live.stays_on(2, &pane);
    }
    // :tabclose closes its tab
    live.ex("tabclose");
    let now = until(|t: &Vec<String>| t.len() == 1, tabs);
    assert_eq!(now.len(), 1, "{}", live.log());
}

/// A live handoff ends every connection to herdr, `events`' too: it
/// starts again on the new server, and Neovim follows focus as before.
#[test]
fn events_comes_back_after_a_live_handoff() {
    let Some(mut live) = Live::start("hd-ho") else {
        return;
    };
    let first = live.win();
    let left = live.vsplit();
    let second = live.win();

    live.herdr.call("server.live_handoff", json!({}));
    // the client goes with the old server
    live.client = None;
    let handed = until(
        |r: &Result<Value, String>| r.is_ok(),
        || live.herdr.try_call("session.snapshot", json!({})),
    );
    assert!(handed.is_ok(), "no server after the handoff: {handed:?}");
    let ended = until(|l: &String| l.contains("events ended"), || live.log());
    assert!(ended.contains("events ended"), "{ended}");

    // focus moved in herdr, Neovim following: `events` is back
    for (pane, win) in [(&live.origin.clone(), &first), (&left, &second)] {
        live.herdr.call("pane.focus", json!({ "pane_id": pane }));
        let now = live.until_win(win);
        assert_eq!(now.as_deref(), Some(win.as_str()), "{pane}: {}", live.log());
    }
}
