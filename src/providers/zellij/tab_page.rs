//! `open_tab_page`: a zellij tab is a tab page area. `new-tab
//! --close-on-exit --no-focus` makes one after the last, running the
//! attach command, and moves no client: the daemon focuses its pane when
//! Neovim goes to its tab page.
//!
//! `new-tab` prints the tab's id, not its pane's, and with no client
//! attached prints one and makes nothing: so its pane is the tab's
//! terminal pane `list-panes` lists, and with none, the open fails.

use std::ffi::{OsStr, OsString};

use super::pane_rows;
use super::session::Session;
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
    let printed = session.run(&args)?;
    let printed = String::from_utf8_lossy(&printed);
    let printed = printed.lines().next().unwrap_or_default().trim();
    let tab: u64 = printed
        .parse()
        .map_err(|_| format!("new-tab printed {printed:?}, not a tab's id"))?;
    let rows = pane_rows(&session.run(&["list-panes", "--json"])?)?;
    rows.iter()
        .filter(|p| p["tab_id"].as_u64() == Some(tab) && p["is_plugin"] == false)
        .find_map(|p| p["id"].as_u64())
        .map(|n| format!("terminal_{n}"))
        .ok_or_else(|| format!("zellij made no tab (id {tab})"))
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
}
