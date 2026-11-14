//! Opening and closing panes.
//!
//! `open_beside` runs `new-pane --no-focus --close-on-exit` issued from the
//! origin (`ZELLIJ_PANE_ID` the origin's number), which splits the origin
//! wherever it is, moving no client's focus, and runs the attach command
//! without a shell. zellij splits only to the right or below, halving the
//! origin: for left or up the new pane then swaps places with the origin,
//! its one neighbour on that side, with `move-pane`; a swap that fails
//! leaves it right or below, said on stderr, and the open still done.
//! zellij takes no size: `SIZE` and `RATIO` are ignored.
//!
//! A split fails while the origin's tab shows its floating panes, and on
//! Linux may still start its command, which attaches only to be refused:
//! so the tab is checked first, and if it shows them, they are hidden
//! around the split and its swap, and shown again.
//!
//! `close` runs `close-pane`, which is done for a pane already gone.

use std::ffi::OsString;

use serde_json::Value;

use super::session::Session;
use super::{terminal_number, terminal_of};
use crate::name::NAME;
use crate::providers::builtin::Given;

/// The new pane's id, `terminal_N`, or why there's none.
pub(super) fn open_beside(given: &Given) -> Result<String, String> {
    let session = Session::of(given)?;
    let origin = terminal_of(given, "origin")?;
    let words = given.attach_words()?;
    let direction = given.var("direction").unwrap_or_default();
    let (split, swap) = match direction.as_slice() {
        b"left" => ("right", Some("left")),
        b"right" => ("right", None),
        b"up" => ("down", Some("up")),
        b"down" => ("down", None),
        other => {
            return Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let mut args: Vec<OsString> = [
        "new-pane",
        "--no-focus",
        "--close-on-exit",
        "--name",
        NAME,
        "--direction",
        split,
        "--",
    ]
    .map(OsString::from)
    .to_vec();
    args.extend(words);
    let open = || {
        let new = printed_terminal(&session.run_from(origin, &args)?)?;
        if let Some(swap) = swap
            && let Err(why) = session.run(&["move-pane", "--pane-id", &new, swap])
        {
            eprintln!(
                "zellij open_beside: {new} stays {split} of terminal_{origin}, not {swap}: {why}"
            );
        }
        Ok(new)
    };
    // checked first: a split that fails may still start its command, which
    // attaches, to be refused
    let panes = session.run(&["list-panes", "--json"])?;
    let tabs = session.run(&["list-tabs", "--json", "--all"])?;
    match floats_shown(&panes, &tabs, origin) {
        Some(tab) => with_floats_hidden(&session, tab, open),
        None => open(),
    }
}

/// What `f` comes to, run with tab `tab`'s floating panes hidden, then
/// shown again.
fn with_floats_hidden<T>(session: &Session, tab: u64, f: impl FnOnce() -> T) -> T {
    let tab = tab.to_string();
    // hiding moves no focus; done or already hidden, `f` may go
    let hid = session
        .run(&["hide-floating-panes", "--tab-id", &tab])
        .is_ok();
    let done = f();
    if hid && let Err(why) = session.run(&["show-floating-panes", "--tab-id", &tab]) {
        eprintln!("zellij open_beside: tab {tab}'s floating panes stay hidden: {why}");
    }
    done
}

/// The tab of terminal pane `origin`, if it shows its floating panes, as
/// `list-panes --json`, `panes`, and `list-tabs --json --all`, `tabs`,
/// say.
fn floats_shown(panes: &[u8], tabs: &[u8], origin: u32) -> Option<u64> {
    let panes: Value = serde_json::from_slice(panes).ok()?;
    let tab = panes.as_array()?.iter().find_map(|p| {
        (p["id"].as_u64() == Some(u64::from(origin)) && p["is_plugin"] == false)
            .then(|| p["tab_id"].as_u64())?
    })?;
    let tabs: Value = serde_json::from_slice(tabs).ok()?;
    let shown = tabs
        .as_array()?
        .iter()
        .any(|t| t["tab_id"].as_u64() == Some(tab) && t["are_floating_panes_visible"] == true);
    shown.then_some(tab)
}

/// The pane id `new-pane` printed, `out`, or why it isn't a terminal's.
fn printed_terminal(out: &[u8]) -> Result<String, String> {
    let id = String::from_utf8_lossy(out);
    let id = id.lines().next().unwrap_or_default().trim();
    if terminal_number(id.as_bytes()).is_none() {
        return Err(format!("new-pane printed {id:?}, not a terminal pane's id"));
    }
    Ok(id.to_string())
}

/// Closes the pane, or says why it's still there.
pub(super) fn close(given: &Given) -> Result<(), String> {
    let session = Session::of(given)?;
    let pane = terminal_of(given, "terminal")?;
    let id = format!("terminal_{pane}");
    let Err(why) = session.run(&["close-pane", "--pane-id", &id]) else {
        return Ok(());
    };
    // gone already, or its session with it
    let listed = session.run(&["list-panes", "--json"]).ok();
    let there = listed
        .and_then(|out| serde_json::from_slice::<Value>(&out).ok())
        .and_then(|v| {
            v.as_array().map(|panes| {
                panes
                    .iter()
                    .any(|p| p["id"].as_u64() == Some(u64::from(pane)) && p["is_plugin"] == false)
            })
        });
    match there {
        Some(true) => Err(why),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_id_is_taken_from_new_pane() {
        assert_eq!(Ok("terminal_7".into()), printed_terminal(b"terminal_7\n"));
        assert!(printed_terminal(b"plugin_7\n").is_err());
        assert!(printed_terminal(b"").is_err());
        assert!(printed_terminal(b"Cannot split\n").is_err());
    }

    #[test]
    fn floats_are_hidden_only_in_a_tab_that_shows_them() {
        let panes = include_bytes!("fixtures/0.46-panes.json");
        let tabs = include_bytes!("fixtures/0.46-tabs.json");
        // tab 0 shows terminal_3, floating; tab 1 has none
        assert_eq!(Some(0), floats_shown(panes, tabs, 0));
        assert_eq!(Some(0), floats_shown(panes, tabs, 2));
        assert_eq!(None, floats_shown(panes, tabs, 5));
        assert_eq!(None, floats_shown(panes, tabs, 99));
        assert_eq!(None, floats_shown(b"nonsense", tabs, 0));
    }
}
