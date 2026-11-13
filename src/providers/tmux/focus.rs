//! `focus` and `focus_direction`, each one tmux call once it knows its
//! window.
//!
//! - `focus`: `select-window` and `select-pane` on the terminal, which
//!   move the session it is in: every client on that session. Never
//!   `switch-client`, which without `-c` moves the most recently active
//!   client, even one on another session. Selecting a pane hidden by a
//!   zoomed one unzooms its window, as tmux's own `select-pane` does.
//! - `focus_direction`: `COUNT` steps in the window the most recently
//!   active client shows, else in tmux's own current window, each a
//!   `select-pane -L/-R/-U/-D` guarded by `#{pane_at_left}` and the like,
//!   so it stops at the window's edge: tmux's own wraps around.

use super::panes::pane_id;
use super::server::Server;
use crate::providers::builtin::Given;

/// Focuses the terminal, or says why not.
pub(super) fn focus(given: &Given) -> Result<(), String> {
    let server = Server::of(given)?;
    let pane = pane_id(given, "terminal")?;
    server.run(&[
        "select-window",
        "-t",
        &pane,
        ";",
        "select-pane",
        "-t",
        &pane,
    ])?;
    Ok(())
}

/// Moves focus `COUNT` panes `DIRECTION`, or says why not.
pub(super) fn focus_direction(given: &Given) -> Result<(), String> {
    let server = Server::of(given)?;
    let direction = given.var("direction").unwrap_or_default();
    let count = given.var("count").unwrap_or_default();
    let count = std::str::from_utf8(&count)
        .ok()
        .and_then(|c| c.parse::<u32>().ok())
        .filter(|c| (1..=20).contains(c))
        .ok_or_else(|| format!("COUNT {:?} isn't 1 to 20", String::from_utf8_lossy(&count)))?;
    let clients = server.run(&["list-clients", "-F", "#{client_activity} #{window_id}"])?;
    let window = match latest_window(&clients) {
        Some(window) => window,
        None => {
            let current = server.run(&["display-message", "-p", "#{window_id}"])?;
            let current = String::from_utf8_lossy(&current).trim().to_string();
            if !is_window_id(&current) {
                return Err(format!("display-message printed {current:?}, not a window"));
            }
            current
        }
    };
    let args = steps(&direction, count, &window).ok_or_else(|| {
        format!(
            "DIRECTION {:?} isn't left, right, up or down",
            String::from_utf8_lossy(&direction)
        )
    })?;
    server.run(&args)?;
    Ok(())
}

/// The tmux command of `count` steps `direction` in window `window`, each
/// only if the active pane isn't at that edge; None for a direction it
/// doesn't know.
fn steps(direction: &[u8], count: u32, window: &str) -> Option<Vec<String>> {
    let (edge, flag) = match direction {
        b"left" => ("left", "-L"),
        b"right" => ("right", "-R"),
        b"up" => ("top", "-U"),
        b"down" => ("bottom", "-D"),
        _ => return None,
    };
    let mut args = Vec::new();
    for i in 0..count {
        if i > 0 {
            args.push(";".to_string());
        }
        args.extend([
            "if-shell".to_string(),
            "-F".into(),
            "-t".into(),
            window.into(),
            format!("#{{pane_at_{edge}}}"),
            String::new(),
            format!("select-pane -t {window} {flag}"),
        ]);
    }
    Some(args)
}

/// The window the most recently active client shows, from `list-clients
/// -F '#{client_activity} #{window_id}'`: the first of the latest.
fn latest_window(clients: &[u8]) -> Option<String> {
    let clients = std::str::from_utf8(clients).ok()?;
    let mut latest: Option<(u64, &str)> = None;
    for line in clients.lines() {
        let Some((Ok(activity), window)) = line
            .split_once(' ')
            .map(|(a, w)| (a.parse::<u64>(), w.trim()))
        else {
            continue;
        };
        if is_window_id(window) && latest.is_none_or(|(a, _)| activity > a) {
            latest = Some((activity, window));
        }
    }
    latest.map(|(_, window)| window.to_string())
}

/// Whether `id` is a tmux window id, `@` and digits.
fn is_window_id(id: &str) -> bool {
    id.strip_prefix('@')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
