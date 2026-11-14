//! The built-in zellij pane provider, live (ADR 0016): a daemon started in
//! a pane of a private zellij session, with no config, gives each Neovim
//! window a zellij pane of its own. Each session has its own socket
//! directory, `HOME` and config directory (`ZellijSession`), and runs in
//! the background: with no client, or, where focus is tested (`Live`), a
//! client in a pty that has typed a key, as zellij acts for the client
//! that last typed.
//!
//! They skip without zellij 0.46.0 or newer; `have_zellij` panics instead
//! when `CI` is set.

use std::ffi::OsStr;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod support;
use neopane::name::NAME;
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

/// The daemon's variables, under `tmp`, with no config.
fn daemon_env(tmp: &std::path::Path) -> Vec<(String, &OsStr)> {
    vec![
        (neopane::daemon::dir::tmpdir_var(), tmp.as_os_str()),
        ("XDG_CONFIG_HOME".into(), tmp.as_os_str()),
        ("XDG_STATE_HOME".into(), tmp.as_os_str()),
    ]
}

/// The pane whose id is `terminal_N`, `id`.
fn pane<'a>(panes: &'a [serde_json::Value], id: &str) -> &'a serde_json::Value {
    find_terminal(panes, id).unwrap_or_else(|| panic!("no {id} in {panes:?}"))
}

/// Terminal pane `id` in `panes`, if it is listed.
fn find_terminal<'a>(panes: &'a [serde_json::Value], id: &str) -> Option<&'a serde_json::Value> {
    panes
        .iter()
        .find(|p| p["is_plugin"] == false && format!("terminal_{}", p["id"]) == id)
}

/// Where terminal pane `id`'s left edge is in `panes`, if it is listed.
fn x_of(panes: &[serde_json::Value], id: &str) -> Option<u64> {
    edge_of(panes, id, "pane_x")
}

/// Where terminal pane `id`'s top edge is in `panes`, if it is listed.
fn y_of(panes: &[serde_json::Value], id: &str) -> Option<u64> {
    edge_of(panes, id, "pane_y")
}

/// Whether an edge is listed before another, both listed.
fn before_in(a: Option<u64>, b: Option<u64>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a < b)
}

/// Terminal pane `id`'s `edge` in `panes`, if it is listed.
fn edge_of(panes: &[serde_json::Value], id: &str, edge: &str) -> Option<u64> {
    find_terminal(panes, id)?[edge].as_u64()
}

/// The terminal pane in `now` that `before` hasn't.
fn new_in(before: &[String], now: &[String]) -> String {
    let mut new = now.iter().filter(|t| !before.contains(t));
    match (new.next(), new.next()) {
        (Some(t), None) => t.clone(),
        _ => panic!("not one new pane: {before:?}, then {now:?}"),
    }
}

#[test]
fn in_a_zellij_pane_splits_open_panes_on_the_left_and_above_and_close_removes_them() {
    if !have_nvim() || !have_zellij() {
        return;
    }
    let tmp = scratch("zj-live");
    let exe = exe();
    let zellij = ZellijSession::new("live");
    zellij.start();
    let env = daemon_env(&tmp);
    let env: Vec<(&str, &OsStr)> = env.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let origin = zellij.open(&env, &[exe.to_str().unwrap(), "--name", "work"]);
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
        "work's Neovim never answered: {}\n{origin} shows:\n{}",
        log(),
        zellij.run(&["dump-screen", "--pane-id", &origin])
    );
    let s = until(|s: &serde_json::Value| s["provider"] == "zellij", status);
    assert_eq!(s["provider"], "zellij", "{}", log());
    let before = zellij.terminals();

    // :vsplit opens a pane on the left, split then swapped, running the
    // attach
    remote_expr(&sock, "execute('vsplit')").unwrap();
    let now = until(
        |t: &Vec<String>| t.len() > before.len(),
        || zellij.terminals(),
    );
    assert!(now.len() > before.len(), "no pane opened: {}", log());
    let new = new_in(&before, &now);
    // opened beside the origin, then swapped to its left: wait for that
    let panes = until(
        |p: &Vec<_>| before_in(x_of(p, &new), x_of(p, &origin)),
        || zellij.panes(),
    );
    let (ours, theirs) = (pane(&panes, &new), pane(&panes, &origin));
    let command = ours["terminal_command"].as_str().unwrap_or_default();
    assert!(
        command.contains(" attach --address "),
        "{new} runs {command}"
    );
    assert_eq!(ours["title"], NAME, "{ours}");
    let s = until(|s: &serde_json::Value| s["panes"] == 2, status);
    assert_eq!(s["panes"], 2, "its attach never came: {}", log());
    // left of its origin, the two side by side
    assert_eq!(ours["pane_y"], theirs["pane_y"], "{panes:?}");
    assert!(
        ours["pane_x"].as_u64() < theirs["pane_x"].as_u64(),
        "{new} isn't left of {origin}: {panes:?}"
    );

    // :close removes it
    remote_expr(&sock, "execute('close')").unwrap();
    let now = until(|t: &Vec<String>| *t == before, || zellij.terminals());
    assert_eq!(now, before, "{}", log());

    // with the tab's floats shown, which a split can't open under, a split
    // opens a pane above, the floats hidden around it and shown again; one
    // not entered, as focusing a tiled pane hides the tab's floats
    zellij.run(&["new-pane", "--floating", "--", "sleep", "100000"]);
    let floats_shown = || {
        let tabs: serde_json::Value =
            serde_json::from_str(&zellij.run(&["list-tabs", "--json", "--all"])).unwrap();
        tabs[0]["are_floating_panes_visible"] == true
    };
    assert!(floats_shown());
    let before = zellij.terminals();
    remote_expr(
        &sock,
        "luaeval('vim.api.nvim_open_win(0, false, {split = \"above\"})')",
    )
    .unwrap();
    let now = until(
        |t: &Vec<String>| t.len() > before.len(),
        || zellij.terminals(),
    );
    assert!(now.len() > before.len(), "no pane opened: {}", log());
    let new = new_in(&before, &now);
    // opened below the origin, then swapped above it: wait for that
    let panes = until(
        |p: &Vec<_>| before_in(y_of(p, &new), y_of(p, &origin)),
        || zellij.panes(),
    );
    let (ours, theirs) = (pane(&panes, &new), pane(&panes, &origin));
    assert_eq!(ours["pane_x"], theirs["pane_x"], "{panes:?}");
    assert!(
        ours["pane_y"].as_u64() < theirs["pane_y"].as_u64(),
        "{new} isn't above {origin}: {panes:?}"
    );
    assert!(floats_shown(), "the floats stayed hidden");

    drop(ending);
    drop(zellij);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A config that prints what `<name>.inside.zellij` says, then returns
/// `<name>.default(attach)`.
const BY_DEFAULT: &str = "return { provider = function(attach) \
     print({name}.inside.zellij(attach)) \
     return {name}.default(attach) end }";

#[test]
fn a_terminal_of_no_pane_with_a_panes_variables_copied_in_is_disproved() {
    if !have_zellij() {
        return;
    }
    let tmp = scratch("zj-copied");
    let zellij = ZellijSession::new("copied");
    zellij.start();
    let config = tmp.join("config.lua");
    std::fs::write(&config, BY_DEFAULT.replace("{name}", NAME)).unwrap();
    // a pane's own: what terminal_0 runs has them
    let copied: Vec<(String, std::ffi::OsString)> = zellij
        .env
        .iter()
        .cloned()
        .chain([
            ("ZELLIJ".into(), "0".into()),
            ("ZELLIJ_PANE_ID".into(), "0".into()),
            ("ZELLIJ_SESSION_NAME".into(), zellij.name.clone().into()),
        ])
        .collect();
    let (_master, slave, tty) = pty();
    let check = |config: Option<&std::path::Path>| {
        let upper = NAME.to_ascii_uppercase();
        let mut c = Command::new(exe());
        c.args(["provider", "check", "--json"])
            .current_dir(&tmp)
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TERM_PROGRAM")
            .envs(copied.iter().map(|(k, v)| (k, v)))
            .env("XDG_CONFIG_HOME", &tmp)
            .stdin(slave.try_clone().unwrap())
            .stderr(Stdio::piped());
        if let Some(config) = config {
            c.env(format!("{upper}_CONFIG"), config);
        }
        json(&c.output().unwrap())
    };
    let not_a_pane = format!("this terminal ({tty}) isn't a zellij pane's: its session leader");

    // with no config, the default names zellij alone, and its find refuses
    let none = check(None);
    assert_eq!(none["chose"]["name"], "zellij", "{none}");
    assert_eq!(none["find"]["verdict"], "refused", "{none}");
    let refused = none["result"]["refused"].as_str().unwrap_or_default();
    assert!(refused.contains(&not_a_pane), "{none}");
    // and inside.zellij says why
    let by_config = check(Some(&config));
    let printed = by_config["ran"]["printed"][0].as_str().unwrap_or_default();
    assert!(
        printed.starts_with(&format!("false\t{not_a_pane}")),
        "{by_config}"
    );

    drop(zellij);
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn zellij_started_from_a_tmux_shell_is_chosen_by_the_defaults_proofs() {
    if !have_nvim() || !have_tmux() || !have_zellij() {
        return;
    }
    let tmp = scratch("zj-tmux");
    let exe = exe();
    let zellij = ZellijSession::new("intmux");
    // a tmux pane runs a zellij client, which starts its server there:
    // tmux's TERM_PROGRAM and TMUX leak into every zellij pane
    let mut client = zellij.command();
    client.args(["attach", "--create", &zellij.name]);
    let words: Vec<String> = std::iter::once("env".to_string())
        .chain(
            client
                .get_envs()
                .filter_map(|(k, v)| Some(format!("{}={}", k.to_str()?, v?.to_str()?))),
        )
        .chain(["zellij", "attach", "--create", &zellij.name].map(String::from))
        .collect();
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let tmux = TmuxServer::start(&tmp, &[], &words);
    let env = daemon_env(&tmp);
    let env: Vec<(&str, &OsStr)> = env.iter().map(|(k, v)| (k.as_str(), *v)).collect();
    let opened = zellij.open(&env, &[exe.to_str().unwrap(), "--name", "work"]);
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
        "work's Neovim never answered: {}\n{opened} shows:\n{}\ntmux shows:\n{}",
        log(),
        zellij.run(&["dump-screen", "--pane-id", &opened]),
        tmux.run(&["capture-pane", "-p", "-t", "%0"])
    );
    let s = until(|s: &serde_json::Value| s["provider"] == "zellij", status);
    assert_eq!(s["provider"], "zellij", "{}", log());
    let logged = log();
    assert!(logged.contains("inside.tmux: no, "), "{logged}");
    assert!(
        logged.contains(&format!("inside.zellij: yes, zellij pane {opened}")),
        "{logged}"
    );

    drop(ending);
    drop(zellij);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// A pseudo-terminal of no pane: its master, its slave and the slave's
/// path.
fn pty() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd, String) {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let (mut master, mut slave) = (-1, -1);
    // SAFETY: valid out-pointers; no name, termios or size.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(0, rc);
    // SAFETY: openpty made both, and they are ours alone.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut name = [0u8; 256];
    // SAFETY: `name` is valid for its length.
    assert_eq!(0, unsafe {
        libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr().cast(), name.len())
    });
    let end = name.iter().position(|&b| b == 0).unwrap();
    (
        master,
        slave,
        String::from_utf8(name[..end].to_vec()).unwrap(),
    )
}

/// A daemon in a pane of a private zellij session, with no config, and a
/// client attached to it that has typed a key, which zellij's focus
/// actions move: everything ends as it drops.
struct Live {
    _client: ZellijClient,
    ending: Ending,
    zellij: ZellijSession,
    /// The daemon's pane.
    origin: String,
    tmp: std::path::PathBuf,
}

impl Live {
    /// None without `nvim` or zellij.
    fn start(test: &str) -> Option<Live> {
        if !have_nvim() || !have_zellij() {
            return None;
        }
        let tmp = scratch(&format!("zj-{test}"));
        let exe = exe();
        let zellij = ZellijSession::new(test);
        zellij.start();
        zellij.ready();
        let client = zellij.attached();
        let env = daemon_env(&tmp);
        let env: Vec<(&str, &OsStr)> = env.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        // a plain new-pane: the client that typed is moved to it; and the
        // pane the session started with closed, so it is alone in its tab
        let origin = zellij.open(&env, &[exe.to_str().unwrap(), "--name", "work"]);
        zellij.run(&["close-pane", "--pane-id", "terminal_0"]);
        let live = Live {
            _client: client,
            ending: Ending {
                sock: base_of(&tmp).join("work/nvim.sock"),
                helpers: Vec::new(),
                tmp: tmp.clone(),
            },
            zellij,
            origin,
            tmp,
        };
        let answered = until_expr(&live.ending.sock, "1", |_| true);
        assert!(
            answered.is_some(),
            "work's Neovim never answered: {}\n{} shows:\n{}",
            live.log(),
            live.origin,
            live.zellij.run(&["dump-screen", "--pane-id", &live.origin])
        );
        let s = until(
            |s: &serde_json::Value| s["provider"] == "zellij",
            || live.status(),
        );
        assert_eq!(s["provider"], "zellij", "{}", live.log());
        let origin = live.until_focused(&live.origin);
        assert_eq!(origin, [live.origin.as_str()], "{}", live.log());
        // zellij tells a pane it has focus as the client lands on it, and
        // may tell it again once its program reads focus reports: one read
        // after a test's first act would take Neovim back to the origin's
        // window, as a user going there would
        std::thread::sleep(Duration::from_millis(1000));
        Some(live)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(base_of(&self.tmp).join("work/log")).unwrap_or_default()
    }

    fn status(&self) -> serde_json::Value {
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

    /// Waits for the client to focus `pane`; the panes the clients last
    /// focused.
    fn until_focused(&self, pane: &str) -> Vec<String> {
        until(|c: &Vec<String>| c == &[pane], || self.zellij.clients())
    }

    /// Waits for Neovim's tab page `n` and the client's `pane`, then lets
    /// the reports after the focus act come back: a report read before the
    /// act landed mustn't send either back.
    fn stays_on(&self, n: u32, pane: &str) {
        let want = (n.to_string(), vec![pane.to_string()]);
        let now = || {
            let tab = remote_expr(&self.ending.sock, "tabpagenr()").unwrap_or_default();
            (tab, self.zellij.clients())
        };
        assert_eq!(until(|got| *got == want, now), want, "{}", self.log());
        std::thread::sleep(Duration::from_millis(1000));
        assert_eq!(now(), want, "it went back: {}", self.log());
    }
}

#[test]
fn ctrl_w_l_moves_zellij_focus_right_and_not_past_the_edge() {
    let Some(live) = Live::start("cw") else {
        return;
    };
    let first = remote_expr(&live.ending.sock, "win_getid()").unwrap();
    let before = live.zellij.terminals();
    live.ex("vsplit");
    let s = until(|s: &serde_json::Value| s["panes"] == 2, || live.status());
    assert_eq!(s["panes"], 2, "its attach never came: {}", live.log());
    let left = new_in(&before, &live.zellij.terminals());
    live.normal("\\<C-w>h");
    assert_eq!(live.until_focused(&left), [left.as_str()], "{}", live.log());
    live.normal("\\<C-w>l");
    let origin = [live.origin.as_str()];
    assert_eq!(live.until_focused(origin[0]), origin, "{}", live.log());
    // Neovim's current window follows: the first, right of the new one
    let now = until_expr(&live.ending.sock, "win_getid()", |w| w == first);
    assert_eq!(now, Some(first));
    // at the right edge it goes nowhere, not round to the left
    live.normal("\\<C-w>l");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(live.zellij.clients(), origin, "{}", live.log());
}

#[test]
fn tabnew_opens_a_zellij_tab_and_gt_focuses_it() {
    let Some(live) = Live::start("tl") else {
        return;
    };
    let tabs = || {
        let tabs: Vec<serde_json::Value> =
            serde_json::from_str(&live.zellij.run(&["list-tabs", "--json"])).unwrap();
        tabs.iter()
            .filter_map(|t| t["tab_id"].as_u64())
            .collect::<Vec<_>>()
    };
    live.ex("tabnew");
    let now = until(|t: &Vec<u64>| t.len() == 2, tabs);
    assert_eq!(now.len(), 2, "no zellij tab opened: {}", live.log());
    let s = until(|s: &serde_json::Value| s["panes"] == 2, || live.status());
    assert_eq!(s["panes"], 2, "its attach never came: {}", live.log());
    let panes = live.zellij.panes();
    let tab = panes
        .iter()
        .find(|p| p["is_plugin"] == false && p["tab_id"].as_u64() == Some(now[1]))
        .map(|p| format!("terminal_{}", p["id"]))
        .unwrap_or_else(|| panic!("no pane in tab {}: {panes:?}", now[1]));
    let command = pane(&panes, &tab)["terminal_command"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        command.contains(" attach --address "),
        "{tab} runs {command}"
    );
    // :tabnew focuses its tab, and stays there
    live.stays_on(2, &tab);
    live.ex("tabnext 1");
    live.stays_on(1, &live.origin);
    live.normal("gt");
    live.stays_on(2, &tab);
}

#[test]
fn a_float_opens_a_zellij_floating_pane_on_its_group() {
    let Some(live) = Live::start("fl") else {
        return;
    };
    let floats = || {
        live.zellij
            .panes()
            .into_iter()
            .filter(|p| p["is_plugin"] == false && p["is_floating"] == true)
            .collect::<Vec<_>>()
    };
    // a float 20x5 entered, as a picker is, at row 3, column 10 of the
    // screen, which spans the origin's tab
    let win = remote_expr(
        &live.ending.sock,
        "luaeval('vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, \
         {relative = \"editor\", row = 3, col = 10, width = 20, height = 5})')",
    )
    .unwrap();
    let now = until(|f: &Vec<serde_json::Value>| f.len() == 1, floats);
    assert_eq!(now.len(), 1, "no float opened: {}", live.log());
    let float = &now[0];
    let id = format!("terminal_{}", float["id"]);
    // its content lands on the group: the tab's tiled panes start at the
    // screen's top left
    let panes = live.zellij.panes();
    let tiled = panes
        .iter()
        .filter(|p| p["is_floating"] == false && p["is_selectable"] == true)
        .filter(|p| p["tab_id"] == float["tab_id"]);
    let x0 = tiled
        .clone()
        .filter_map(|p| p["pane_x"].as_u64())
        .min()
        .unwrap();
    let y0 = tiled.filter_map(|p| p["pane_y"].as_u64()).min().unwrap();
    let content = [
        "pane_content_x",
        "pane_content_y",
        "pane_content_columns",
        "pane_content_rows",
    ]
    .map(|k| float[k].as_u64().unwrap());
    assert_eq!(content, [x0 + 10, y0 + 3, 20, 5], "{float}: {}", live.log());
    let command = float["terminal_command"].as_str().unwrap_or_default();
    assert!(
        command.contains(" attach --address "),
        "{id} runs {command}"
    );
    assert_eq!(float["title"], NAME, "{float}");
    let s = until(|s: &serde_json::Value| s["panes"] == 2, || live.status());
    assert_eq!(s["panes"], 2, "its attach never came: {}", live.log());
    // entered, it has the client's focus, and Neovim's float stays where it
    // was opened: its group followed no move
    assert_eq!(live.until_focused(&id), [id.as_str()], "{}", live.log());
    std::thread::sleep(Duration::from_millis(500));
    let at = remote_expr(
        &live.ending.sock,
        &format!("luaeval('vim.api.nvim_win_get_config({win}).row')"),
    );
    assert_eq!(at.as_deref(), Some("3"), "{}", live.log());
    // left, focus goes back to the origin, and the float's pane stays
    live.normal("\\<C-w>p");
    let origin = [live.origin.as_str()];
    assert_eq!(live.until_focused(origin[0]), origin, "{}", live.log());
    assert_eq!(floats().len(), 1, "{}", live.log());

    // closing the float closes its pane
    remote_expr(
        &live.ending.sock,
        &format!("luaeval('vim.api.nvim_win_close({win}, true)')"),
    )
    .unwrap();
    let now = until(|f: &Vec<serde_json::Value>| f.is_empty(), floats);
    assert!(now.is_empty(), "{now:?}: {}", live.log());
}
