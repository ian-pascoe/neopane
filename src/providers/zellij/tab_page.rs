//! `open_tab_page`: a zellij tab is a tab page area. `new-tab
//! --close-on-exit --no-focus` makes one after the last, running the
//! attach command, and moves no client: the daemon focuses its pane when
//! Neovim goes to its tab page.
//!
//! `new-tab` prints the tab's id, not its pane's, and with no client
//! attached prints one and makes nothing: so its pane is found in
//! `list-panes`. The tab is made from the user's tab template, whose other
//! panes may run other commands (`new_tab_template`, say, with the attach
//! in its bare pane): its pane is the one whose `terminal_command`, the
//! attach's words joined by spaces, is the attach's; else its only
//! terminal pane. With several and none running the attach, or none at
//! all, the open fails: a pane of the user's, taken for it, would be
//! closed with the window.

use std::ffi::{OsStr, OsString};

use serde_json::Value;

use super::session::Session;
use super::{first_line, pane_rows};
use crate::providers::builtin::Given;

/// The new tab's pane, `terminal_N`, or why there's none.
pub(super) fn open_tab_page(given: &Given) -> Result<String, String> {
    opened(&Session::of(given)?, &given.attach_words()?)
}

/// The pane of a new tab of `session` running `words`, or why there's
/// none.
fn opened(session: &Session, words: &[OsString]) -> Result<String, String> {
    let mut args: Vec<&OsStr> = ["new-tab", "--close-on-exit", "--no-focus", "--"]
        .map(OsStr::new)
        .to_vec();
    args.extend(words.iter().map(OsString::as_os_str));
    let printed = first_line(&session.run(&args)?);
    let tab: u64 = printed
        .parse()
        .map_err(|_| format!("new-tab printed {printed:?}, not a tab's id"))?;
    let rows = pane_rows(&session.run(&["list-panes", "--json"])?)?;
    attach_pane(&rows, tab, words)
}

/// The pane of tab `tab` in `rows`, as `list-panes --json` printed them,
/// that runs `words`, or its only terminal pane; or why there's none.
fn attach_pane(rows: &[Value], tab: u64, words: &[OsString]) -> Result<String, String> {
    let command: Vec<_> = words.iter().map(|w| w.to_string_lossy()).collect();
    let command = command.join(" ");
    let terminals: Vec<&Value> = rows
        .iter()
        .filter(|p| p["tab_id"].as_u64() == Some(tab) && p["is_plugin"] == false)
        .collect();
    let pane = match terminals[..] {
        [] => return Err(format!("zellij made no tab (id {tab})")),
        [only] => only,
        _ => terminals
            .iter()
            .find(|p| p["terminal_command"].as_str() == Some(command.as_str()))
            .ok_or_else(|| {
                format!(
                    "tab {tab} has {} terminal panes, none running the attach",
                    terminals.len()
                )
            })?,
    };
    pane["id"]
        .as_u64()
        .map(|n| format!("terminal_{n}"))
        .ok_or_else(|| format!("list-panes gave tab {tab}'s pane as {pane}"))
}

#[cfg(test)]
mod tests {
    use super::super::fake::Fake;
    use super::*;

    fn attach() -> Vec<OsString> {
        vec!["/bin/x".into(), "attach".into()]
    }

    /// terminal_0 in tab 0, and the bars and terminal_4 in tab 2.
    const LISTED: &str = r#"echo '[
        {"id": 0, "is_plugin": false, "tab_id": 0},
        {"id": 1, "is_plugin": true, "tab_id": 2},
        {"id": 4, "is_plugin": false, "tab_id": 2}]'"#;

    #[test]
    fn a_tab_page_is_a_new_tab_its_pane_found_in_the_listing() {
        let fake = Fake::new("tab", &[("new-tab", "echo 2"), ("list-panes", LISTED)]);
        assert_eq!(opened(&fake.session(), &attach()), Ok("terminal_4".into()));
        assert_eq!(
            fake.calls(),
            [
                "new-tab --close-on-exit --no-focus -- /bin/x attach",
                "list-panes --json"
            ]
        );
    }

    /// With no client attached, zellij 0.46 prints a tab's id and makes
    /// no tab.
    #[test]
    fn a_tab_zellij_didnt_make_fails_the_open() {
        let fake = Fake::new("tab-none", &[("new-tab", "echo 3"), ("list-panes", LISTED)]);
        assert_eq!(
            opened(&fake.session(), &attach()),
            Err("zellij made no tab (id 3)".into())
        );
        let fake = Fake::new("tab-no-id", &[("new-tab", "echo nope")]);
        assert_eq!(
            opened(&fake.session(), &attach()),
            Err("new-tab printed \"nope\", not a tab's id".into())
        );
        assert_eq!(fake.asked(), ["new-tab"]);
    }

    /// Recorded from zellij 0.46.0, its `new_tab_template` three panes
    /// side by side, `sleep 100002`, a bare pane and `sleep 100003`:
    /// `new-tab -- sh -c 'sleep 997' attach --address /tmp/sock` made tab
    /// 1, the command in the bare pane, `terminal_10`, between
    /// `terminal_9` and `terminal_11`, each running the template's.
    #[test]
    fn zellij_0_46_in_a_tab_template_of_several_panes_its_the_one_running_the_attach() {
        let rows = pane_rows(include_bytes!("fixtures/0.46-tab-template-panes.json")).unwrap();
        let words: Vec<OsString> = ["sh", "-c", "sleep 997", "attach", "--address", "/tmp/sock"]
            .map(OsString::from)
            .to_vec();
        assert_eq!(attach_pane(&rows, 1, &words), Ok("terminal_10".into()));
        // none running it: none taken, not one of the user's
        assert_eq!(
            attach_pane(&rows, 1, &attach()),
            Err("tab 1 has 3 terminal panes, none running the attach".into())
        );
        // a tab not listed
        assert!(attach_pane(&rows, 7, &words).is_err());
    }
}
