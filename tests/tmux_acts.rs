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

/// The window and pane each attached client shows.
fn shown(tmux: &TmuxServer) -> String {
    tmux.run(&["list-clients", "-F", "#{window_id} #{pane_id}"])
}

#[test]
fn focus_selects_the_pane_and_its_window_and_unzooms_to_reach_it() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-focus");
    let one = split(&tmux, "%0", &["-h"]);
    let other = tmux.run(&["new-window", "-d", "-P", "-F", "#{pane_id}"]);
    let client = tmux.attached();
    assert_eq!(shown(&tmux), "@0 %0");
    let focus = |t: &str| done(&tmux, "focus", &[("TERMINAL", t)]);
    focus(&other);
    assert_eq!(shown(&tmux), format!("@1 {other}"));
    focus(&one);
    assert_eq!(shown(&tmux), format!("@0 {one}"));
    // %0 zoomed hides `one`: focusing it unzooms, as select-pane does
    tmux.run(&["resize-pane", "-Z", "-t", "%0"]);
    assert_eq!(of(&tmux, "%0", "#{window_zoomed_flag}"), "1");
    focus(&one);
    assert_eq!(shown(&tmux), format!("@0 {one}"));
    assert_eq!(of(&tmux, "%0", "#{window_zoomed_flag}"), "0");
    // a pane that isn't one
    let out = act(&tmux, "focus", &[("TERMINAL", "@1")]);
    assert_eq!(out.status.code(), Some(1));
    drop(client);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn focus_direction_steps_in_the_shown_window_and_stops_at_its_edge() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-focus-direction");
    // %0 | B | C, and another window shown by no client
    let b = split(&tmux, "%0", &["-h"]);
    let c = split(&tmux, &b, &["-h"]);
    tmux.run(&["new-window", "-d"]);
    let client = tmux.attached();
    let step = |dir: &str, count: &str| {
        done(
            &tmux,
            "focus_direction",
            &[("DIRECTION", dir), ("COUNT", count)],
        )
    };
    step("right", "1");
    assert_eq!(shown(&tmux), format!("@0 {b}"));
    // five steps right go as far as C, and no further round
    step("right", "5");
    assert_eq!(shown(&tmux), format!("@0 {c}"));
    step("right", "1");
    assert_eq!(shown(&tmux), format!("@0 {c}"));
    step("up", "1");
    assert_eq!(shown(&tmux), format!("@0 {c}"));
    step("left", "2");
    assert_eq!(shown(&tmux), "@0 %0");
    step("left", "1");
    assert_eq!(shown(&tmux), "@0 %0");
    // with no client, in tmux's own current window
    drop(client);
    step("right", "1");
    assert_eq!(of(&tmux, &b, "#{pane_active}"), "1");
    for (dir, count) in [("sideways", "1"), ("left", "0"), ("left", "21")] {
        let out = act(
            &tmux,
            "focus_direction",
            &[("DIRECTION", dir), ("COUNT", count)],
        );
        assert_eq!(out.status.code(), Some(1), "{dir} {count}");
    }
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn open_tab_page_opens_a_window_after_the_origins_running_the_attach() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-tab-page");
    split(&tmux, "%0", &["-h"]);
    tmux.run(&["new-window", "-d"]);
    let client = tmux.attached();
    tmux.run(&["resize-pane", "-Z", "-t", "%0"]);
    let pane = done(
        &tmux,
        "open_tab_page",
        &[
            ("ORIGIN", "%0"),
            ("ATTACH", "sleep 300"),
            ("ATTACH_JSON", r#"["sleep","300"]"#),
            ("COLS", "80"),
            ("ROWS", "24"),
        ],
    );
    assert!(pane.starts_with('%'), "{pane}");
    // after the origin's window, the session still on the origin's
    let windows = tmux.run(&["list-windows", "-F", "#{window_index} #{pane_id}"]);
    let second = windows.lines().nth(1).unwrap();
    assert_eq!(second, format!("1 {pane}"), "{windows}");
    assert_eq!(shown(&tmux), "@0 %0");
    assert_eq!(of(&tmux, "%0", "#{window_zoomed_flag}"), "1");
    assert_eq!(of(&tmux, &pane, "#{pane_start_command}"), "sleep 300");
    // a one-word attach would go through a shell
    let out = act(
        &tmux,
        "open_tab_page",
        &[("ORIGIN", "%0"), ("ATTACH", "sleep")],
    );
    assert_eq!(out.status.code(), Some(1));
    drop(client);
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn report_layout_puts_the_panes_a_zoom_hides_in_an_area_of_their_own() {
    if !have_tmux() {
        return;
    }
    let (tmux, dir) = server("tmux-zoomed");
    let right = split(&tmux, "%0", &["-h"]);
    tmux.run(&["resize-pane", "-Z", "-t", "%0"]);
    let report: serde_json::Value =
        serde_json::from_str(&done(&tmux, "report_layout", &[])).unwrap();
    let areas: Vec<(String, Vec<String>)> = report["areas"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            let ids = a["terminals"].as_array().unwrap();
            let ids = ids.iter().map(|t| t["terminal"].as_str().unwrap().into());
            (a["id"].as_str().unwrap().into(), ids.collect())
        })
        .collect();
    assert_eq!(
        areas,
        [
            ("@0".to_string(), vec!["%0".to_string()]),
            ("@0~hidden".to_string(), vec![right]),
        ]
    );
    assert_eq!(
        report["areas"][0]["terminals"][0]["rect"],
        serde_json::json!([0.0, 0.0, 1.0, 1.0])
    );
    drop(tmux);
    let _ = std::fs::remove_dir_all(&dir);
}
