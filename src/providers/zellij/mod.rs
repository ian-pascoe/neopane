//! The built-in zellij pane provider's command (ADR 0016): `provider
//! builtin zellij CAPABILITY`, which its table runs for each capability.
//! It acts on the zellij session the choosing attach runs in, as
//! `ZELLIJ_SESSION_NAME` names it, found through the attach's
//! `ZELLIJ_SOCKET_DIR` and `HOME` (`session.rs`).
//!
//! - Every zellij call is `zellij --session NAME action …`, without the
//!   choosing attach's `ZELLIJ_PANE_ID`, which zellij takes as the pane
//!   issuing it; only a split or a float names one, its origin's.
//! - `find` (`find.rs`): the inside proof (`inside.rs`), then zellij 0.46.0
//!   at least, and the pane listed and not exited; prints `terminal_N`.
//! - `open_beside` and `close` (`panes.rs`): `new-pane --no-focus` right of
//!   or below the origin, then a `move-pane` swap for left or up, the
//!   tab's floats hidden around the split if they show; `close-pane`.
//! - `open_float` (`float.rs`): `new-pane --floating --pinned true
//!   --no-focus` over the origin's tab, its content on the float group.
//! - `open_tab_page` (`tab_page.rs`): `new-tab --no-focus`, its pane, the
//!   one running the attach, found in `list-panes`: a zellij tab is a tab
//!   page area.
//! - `focus` and `focus_direction` (`focus.rs`): `focus-pane-id` and
//!   `move-focus`, for the client that last typed.
//! - `report_layout` (`layout.rs`): one area per tab, from `list-panes`
//!   and `list-tabs`, a full screen's hidden panes in `tab_N~hidden`; the
//!   focus from `list-clients`, with one client.
//! - `inside.zellij` (`inside.rs`), no capability: the attach's process
//!   table proves its terminal a pane of a zellij server, running no
//!   zellij command; `find` runs the same proof.
//!
//! Each capability gives the line it prints, or why it failed; `run`
//! prints it, or the failure as one `zellij CAPABILITY: why` line on
//! stderr, and gives the exit status. Every terminal id read or given is
//! `terminal_` and digits.

#[cfg(test)]
mod fake;
mod find;
mod float;
mod focus;
mod inside;
mod layout;
mod panes;
mod session;
mod tab_page;

pub(crate) use inside::inside;

use serde_json::Value;

use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `zellij CAPABILITY: why` line on stderr, and gives its
/// exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    let done = match cap {
        Capability::Find => find::find(given).map(Some),
        Capability::OpenBeside => panes::open_beside(given).map(Some),
        Capability::Close => panes::close(given).map(|()| None),
        Capability::ReportLayout => layout::report_layout(given).map(Some),
        Capability::OpenFloat => float::open_float(given).map(Some),
        Capability::OpenTabPage => tab_page::open_tab_page(given).map(Some),
        Capability::Focus => focus::focus(given).map(|()| None),
        Capability::FocusDirection => focus::focus_direction(given).map(|()| None),
        _ => return UNSUPPORTED,
    };
    match done {
        Ok(Some(line)) => print_line(line.as_bytes()),
        Ok(None) => DONE,
        Err(why) => {
            eprintln!("zellij {}: {why}", cap.name());
            FAILED
        }
    }
}

/// The number of terminal pane `terminal_N`, if `id` is one: what
/// zellij's `-p` takes, and `ZELLIJ_PANE_ID` holds.
fn terminal_number(id: &[u8]) -> Option<u32> {
    let n = id.strip_prefix(b"terminal_")?;
    if n.is_empty() || !n.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(n).ok()?.parse().ok()
}

/// Input `name`'s terminal pane, `terminal_N`: its number, or why not.
fn terminal_of(given: &Given, name: &str) -> Result<u32, String> {
    let id = given.var(name).unwrap_or_default();
    terminal_number(&id).ok_or_else(|| {
        format!(
            "{} {:?} isn't a zellij terminal pane's id",
            name.to_ascii_uppercase(),
            String::from_utf8_lossy(&id)
        )
    })
}

/// The first line of what zellij printed, `out`, trimmed.
fn first_line(out: &[u8]) -> String {
    let out = String::from_utf8_lossy(out);
    out.lines().next().unwrap_or_default().trim().to_string()
}

/// The pane id an open printed, `out`, `what` it ran, or why it isn't a
/// terminal's.
fn printed_terminal(out: &[u8], what: &str) -> Result<String, String> {
    let id = first_line(out);
    if terminal_number(id.as_bytes()).is_none() {
        return Err(format!("{what} printed {id:?}, not a terminal pane's id"));
    }
    Ok(id)
}

/// The rows `list-panes --json` printed, `out`, one per pane, or why
/// there are none.
fn pane_rows(out: &[u8]) -> Result<Vec<Value>, String> {
    let rows: Value =
        serde_json::from_slice(out).map_err(|e| format!("list-panes printed no JSON: {e}"))?;
    match rows {
        Value::Array(rows) => Ok(rows),
        _ => Err("list-panes printed no list".into()),
    }
}

/// Terminal pane `terminal_N`'s row in `rows`, if listed: plugin panes
/// number apart from terminal panes.
fn terminal_row(rows: &[Value], n: u32) -> Option<&Value> {
    rows.iter()
        .find(|p| p["id"].as_u64() == Some(u64::from(n)) && p["is_plugin"] == false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_id_is_terminal_and_digits() {
        assert_eq!(Some(3), terminal_number(b"terminal_3"));
        assert_eq!(Some(120), terminal_number(b"terminal_120"));
        for not in [
            &b"3"[..],
            b"plugin_3",
            b"terminal_",
            b"terminal_3x",
            b"%3",
            b"",
        ] {
            assert_eq!(None, terminal_number(not), "{not:?}");
        }
    }

    #[test]
    fn only_a_terminal_id_is_taken_from_an_open() {
        let printed = |out: &[u8]| printed_terminal(out, "new-pane");
        assert_eq!(Ok("terminal_7".into()), printed(b"terminal_7\n"));
        assert!(printed(b"plugin_7\n").is_err());
        assert!(printed(b"").is_err());
        assert_eq!(
            printed(b"Cannot split\n"),
            Err("new-pane printed \"Cannot split\", not a terminal pane's id".into())
        );
    }
}
