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
//! around the split and its swap, and shown again. Hiding them failing
//! fails the open, with no split.
//!
//! The command runner kills `open_beside` at 5 s. So that the floats are
//! always shown again, every step but that has a deadline, [`WORK`] from
//! the start, and showing them has [`RESTORE`] more: a step that runs past
//! the deadline is killed, and any step left is skipped, the swap, say.
//!
//! `close` runs `close-pane`, which is done for a pane already gone.

use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::session::{Ran, Session};
use super::{pane_rows, terminal_number, terminal_of, terminal_row};
use crate::name::NAME;
use crate::providers::builtin::Given;

/// The time `open_beside` has for all but showing the floats again.
const WORK: Duration = Duration::from_secs(3);
/// The time it has after that to show them again, well inside the
/// runner's 5 s.
const RESTORE: Duration = Duration::from_secs(1);

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
    let split = Split {
        origin,
        direction: split,
        swap,
        words,
    };
    let start = Instant::now();
    split.open(&session, start + WORK, start + WORK + RESTORE)
}

/// A split of terminal pane `origin` to `direction`, right or down, then
/// a swap to `swap`, left or up, if any, running the attach's `words`.
struct Split {
    origin: u32,
    direction: &'static str,
    swap: Option<&'static str>,
    words: Vec<OsString>,
}

impl Split {
    /// The new pane's id, or why there's none: every step done by `due`,
    /// but showing the floats again, by `restored`.
    fn open(&self, session: &Session, due: Instant, restored: Instant) -> Result<String, String> {
        // checked first: a split that fails may still start its command,
        // which attaches, to be refused
        let panes = session.run_until(None, &["list-panes", "--json"], due)?;
        let tabs = session.run_until(None, &["list-tabs", "--json", "--all"], due)?;
        let Some(tab) = floats_shown(&panes, &tabs, self.origin) else {
            return self.split(session, due);
        };
        let tab = tab.to_string();
        let show = || {
            let shown =
                session.run_until(None, &["show-floating-panes", "--tab-id", &tab], restored);
            if let Err(why) = shown {
                eprintln!("zellij open_beside: tab {tab}'s floating panes stay hidden: {why}");
            }
        };
        match session.run_until(None, &["hide-floating-panes", "--tab-id", &tab], due) {
            Ok(_) => {}
            Err(why) => {
                // killed, it may have hidden them
                if matches!(why, Ran::TimedOut(_)) {
                    show();
                }
                return Err(format!(
                    "can't hide tab {tab}'s floating panes, which a split can't open under: {why}"
                ));
            }
        }
        let opened = self.split(session, due);
        show();
        opened
    }

    /// The split and the swap, by `due`.
    fn split(&self, session: &Session, due: Instant) -> Result<String, String> {
        let mut args: Vec<&OsStr> = [
            "new-pane",
            "--no-focus",
            "--close-on-exit",
            "--name",
            NAME,
            "--direction",
            self.direction,
            "--",
        ]
        .map(OsStr::new)
        .to_vec();
        args.extend(self.words.iter().map(OsString::as_os_str));
        let new = printed_terminal(&session.run_until(Some(self.origin), &args, due)?)?;
        if let Some(swap) = self.swap
            && let Err(why) = session.run_until(None, &["move-pane", "--pane-id", &new, swap], due)
        {
            eprintln!(
                "zellij open_beside: {new} stays {} of terminal_{}, not {swap}: {why}",
                self.direction, self.origin
            );
        }
        Ok(new)
    }
}

/// The tab of terminal pane `origin`, if it shows its floating panes, as
/// `list-panes --json`, `panes`, and `list-tabs --json --all`, `tabs`,
/// say.
fn floats_shown(panes: &[u8], tabs: &[u8], origin: u32) -> Option<u64> {
    let rows = pane_rows(panes).ok()?;
    let tab = terminal_row(&rows, origin)?["tab_id"].as_u64()?;
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

/// Closes the pane, or says why it may still be there.
pub(super) fn close(given: &Given) -> Result<(), String> {
    closed(&Session::of(given)?, terminal_of(given, "terminal")?)
}

/// Closes terminal pane `pane` of `session`, or says why it may still be
/// there.
fn closed(session: &Session, pane: u32) -> Result<(), String> {
    let id = format!("terminal_{pane}");
    let why = match session.run(&["close-pane", "--pane-id", &id]) {
        Ok(_) | Err(Ran::NoSession(_)) => return Ok(()),
        Err(why) => why,
    };
    // done only if the session's listing proves it gone, or that the
    // session is
    let listed = session.run(&["list-panes", "--json"]);
    match listed.map(|out| pane_rows(&out)) {
        Ok(Ok(rows)) if terminal_row(&rows, pane).is_none() => Ok(()),
        Ok(Ok(_)) => Err(why.to_string()),
        Err(Ran::NoSession(_)) => Ok(()),
        Ok(Err(unlisted)) => Err(format!("{why}; and can't tell it's gone: {unlisted}")),
        Err(unlisted) => Err(format!("{why}; and can't tell it's gone: {unlisted}")),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A fake zellij in a directory of its own: it logs each action's
    /// name, one a line, and answers it as `answers` say, a shell snippet
    /// per action. Removed as it drops.
    struct Fake {
        dir: PathBuf,
    }

    impl Fake {
        fn new(test: &str, answers: &[(&str, &str)]) -> Fake {
            let dir = std::env::temp_dir().join(format!("zj-fake-{test}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let log = dir.join("log");
            let mut script = format!(
                "#!/bin/sh\nshift 3\necho \"$1\" >> '{}'\ncase \"$1\" in\n",
                log.display()
            );
            for (action, answer) in answers {
                script.push_str(&format!("{action}) {answer} ;;\n"));
            }
            script.push_str("esac\n");
            let program = dir.join("zellij");
            std::fs::write(&program, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            Fake { dir }
        }

        fn session(&self) -> Session {
            Session::faked(&self.dir.join("zellij"), "work")
        }

        /// The actions it was asked, in order.
        fn asked(&self) -> Vec<String> {
            let log = std::fs::read_to_string(self.dir.join("log")).unwrap_or_default();
            log.lines().map(String::from).collect()
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// terminal_0 in tab 0, which shows its floats if `floats`.
    fn listed(floats: bool) -> [(&'static str, String); 2] {
        [
            (
                "list-panes",
                r#"echo '[{"id": 0, "is_plugin": false, "tab_id": 0}]'"#.into(),
            ),
            (
                "list-tabs",
                format!(r#"echo '[{{"tab_id": 0, "are_floating_panes_visible": {floats}}}]'"#),
            ),
        ]
    }

    fn fake(test: &str, floats: bool, answers: &[(&str, &str)]) -> Fake {
        let listed = listed(floats);
        let mut all: Vec<(&str, &str)> = listed.iter().map(|(a, s)| (*a, s.as_str())).collect();
        all.extend(answers);
        Fake::new(test, &all)
    }

    fn left_of_0() -> Split {
        Split {
            origin: 0,
            direction: "right",
            swap: Some("left"),
            words: vec!["attach".into()],
        }
    }

    /// A deadline far past what the steps before the one that blocks take,
    /// however loaded the machine: only that one, which sleeps a minute
    /// unless killed, runs into it.
    const DUE: Duration = Duration::from_secs(2);

    /// A step that blocks until it is killed.
    const BLOCKS: &str = "exec sleep 60";

    /// What `open` comes to on `fake`, its steps due in [`DUE`], showing
    /// the floats in as long again; and that it came to it by then, after
    /// the step that blocks, if `blocks`, was killed at its deadline.
    fn open_on(fake: &Fake, blocks: bool) -> Result<String, String> {
        let start = Instant::now();
        let due = start + DUE;
        let opened = left_of_0().open(&fake.session(), due, due + DUE);
        let took = start.elapsed();
        assert!(!blocks || took >= DUE, "{took:?}");
        assert!(took < 2 * DUE, "{took:?}");
        opened
    }

    #[test]
    fn a_split_past_its_deadline_is_killed_and_the_floats_shown_again() {
        let fake = fake("split-late", true, &[("new-pane", BLOCKS)]);
        assert!(open_on(&fake, true).unwrap_err().contains("out of time"));
        assert_eq!(
            fake.asked(),
            [
                "list-panes",
                "list-tabs",
                "hide-floating-panes",
                "new-pane",
                "show-floating-panes"
            ]
        );
    }

    #[test]
    fn a_swap_past_the_deadline_is_cut_the_pane_kept_and_the_floats_shown_again() {
        let fake = fake(
            "swap-late",
            true,
            &[("new-pane", "echo terminal_5"), ("move-pane", BLOCKS)],
        );
        assert_eq!(open_on(&fake, true), Ok("terminal_5".into()));
        assert_eq!(
            fake.asked(),
            [
                "list-panes",
                "list-tabs",
                "hide-floating-panes",
                "new-pane",
                "move-pane",
                "show-floating-panes"
            ]
        );
    }

    #[test]
    fn a_hide_killed_at_the_deadline_fails_the_open_and_shows_the_floats_again() {
        // killed, it may have hidden them
        let fake = fake("hide-late", true, &[("hide-floating-panes", BLOCKS)]);
        let why = open_on(&fake, true).unwrap_err();
        assert!(
            why.starts_with("can't hide tab 0's floating panes"),
            "{why}"
        );
        assert_eq!(
            fake.asked(),
            [
                "list-panes",
                "list-tabs",
                "hide-floating-panes",
                "show-floating-panes"
            ]
        );
    }

    #[test]
    fn a_step_due_already_isnt_run() {
        let fake = Fake::new("past", &[]);
        let ran = fake
            .session()
            .run_until(None, &["move-pane"], Instant::now());
        assert!(matches!(ran, Err(Ran::TimedOut(_))), "{ran:?}");
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn hiding_the_floats_failing_fails_the_open_with_no_split() {
        let fake = fake(
            "hide-fails",
            true,
            &[("hide-floating-panes", "echo 'Tab not found' >&2; exit 1")],
        );
        let why = open_on(&fake, false).unwrap_err();
        assert!(
            why.starts_with("can't hide tab 0's floating panes"),
            "{why}"
        );
        assert!(why.ends_with("Tab not found"), "{why}");
        assert_eq!(
            fake.asked(),
            ["list-panes", "list-tabs", "hide-floating-panes"]
        );
    }

    #[test]
    fn with_no_floats_shown_the_split_and_swap_alone_run() {
        let fake = fake("no-floats", false, &[("new-pane", "echo terminal_5")]);
        assert_eq!(open_on(&fake, false), Ok("terminal_5".into()));
        assert_eq!(
            fake.asked(),
            ["list-panes", "list-tabs", "new-pane", "move-pane"]
        );
    }

    /// Fails as 0.46.0 does for a session that isn't there, `line` its
    /// first line on stderr.
    fn no_session(line: &str) -> String {
        format!("echo \"{line}\" >&2; exit 1")
    }

    #[test]
    fn close_is_done_only_when_the_listing_proves_the_pane_gone() {
        let failing = "echo 'Pane with id Terminal(3) not found' >&2; exit 2";
        let gone = fake("close-gone", false, &[("close-pane", failing)]);
        assert_eq!(closed(&gone.session(), 3), Ok(()));
        let there = fake("close-there", false, &[("close-pane", failing)]);
        assert!(closed(&there.session(), 0).is_err());
        let unknown = Fake::new(
            "close-unknown",
            &[("close-pane", failing), ("list-panes", "exit 1")],
        );
        let why = closed(&unknown.session(), 3).unwrap_err();
        assert!(why.contains("can't tell it's gone"), "{why}");
        let done = Fake::new("close-done", &[("close-pane", "true")]);
        assert_eq!(closed(&done.session(), 3), Ok(()));
    }

    #[test]
    fn close_is_done_in_a_session_that_has_ended() {
        // no session at all; others, but not this one
        for (i, line) in [
            "There is no active session!",
            "Session 'work' not found. The following sessions are active:",
        ]
        .into_iter()
        .enumerate()
        {
            let ended = no_session(line);
            let both = Fake::new(
                &format!("close-ended-{i}"),
                &[("close-pane", &ended), ("list-panes", &ended)],
            );
            assert_eq!(closed(&both.session(), 3), Ok(()), "{line}");
            assert_eq!(both.asked(), ["close-pane"], "{line}");
            // ended between the two
            let between = Fake::new(
                &format!("close-between-{i}"),
                &[("close-pane", "exit 1"), ("list-panes", &ended)],
            );
            assert_eq!(closed(&between.session(), 3), Ok(()), "{line}");
        }
        // another session's name is no proof
        let other = no_session("Session 'other' not found.");
        let other = Fake::new(
            "close-other",
            &[("close-pane", &other), ("list-panes", &other)],
        );
        assert!(closed(&other.session(), 3).is_err());
    }

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
