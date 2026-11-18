//! `focus` and `focus_direction`. herdr has one focus per server, not per
//! client: moving it moves every client attached, as herdr's own keys do.
//!
//! - `focus`: `pane.focus` on the terminal, which goes to its tab and
//!   workspace; then, for a pane alone in its tab, the scrollbar's column
//!   given back if herdr kept it from the pty (`fit`, SPI-1207).
//! - `focus_direction`: `COUNT` steps of `pane.focus_direction`, the first
//!   from the server's focused pane, as `session.snapshot` has it, and
//!   each after from where the last landed. herdr's own stop at the tab's
//!   edge, answering `no_neighbor`: there it stops, done. Any other step
//!   that moves nothing fails, with herdr's reason.

use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

use serde_json::json;

use super::client::{Herdr, UNREADABLE};
use crate::providers::builtin::Given;
use crate::providers::tty;

/// What `focus` and `focus_direction` have, of their 2 s.
const FOCUS_TIME: Duration = Duration::from_millis(1500);

/// What `fit` has, after the focus: herdr gives the column back as it
/// next lays the tab out once it has read the pane's switch to the
/// alternate screen, a few ms after the switch is written.
const FIT_TIME: Duration = Duration::from_millis(300);

/// Between a `pane.resize` that lays the tab out and the next look.
const FIT_LOOK: Duration = Duration::from_millis(10);

/// Focuses the terminal, or says why not.
pub(super) fn focus(given: &Given) -> Result<(), String> {
    focus_by(given, columns_of_process)
}

/// [`focus`], reading the columns of a process's terminal through
/// `columns_of`.
fn focus_by(given: &Given, columns_of: impl Fn(u32) -> Option<u16>) -> Result<(), String> {
    let pane = given.text("terminal")?;
    let herdr = Herdr::of(given, FOCUS_TIME)?;
    herdr.call("pane.focus", json!({ "pane_id": pane }))?;
    let due = Instant::now() + FIT_TIME;
    fit(&herdr.again(FIT_TIME), &pane, columns_of, || {
        wait_for_a_look(due)
    });
    Ok(())
}

/// Gives `pane`'s pty, if one column narrower than the pane, alone in its
/// tab, the scrollbar's column (SPI-1207); anything it can't read leaves
/// it as it is, the focus done.
///
/// herdr starts a pane's pty a column narrow, for a scrollbar on the
/// primary screen, and sizes a tab's ptys only as a request lays the tab
/// out, such as `pane.focus` going to it, or as it draws a pane it drew
/// before on the other screen. A pane whose switch to the alternate
/// screen herdr reads after the focus has laid its tab out, but before it
/// first draws the tab, is drawn as it is from then on: on the alternate
/// screen, its pty still a column narrow. A tab of one pane is never laid
/// out again, so the pane and Neovim's screen in it stay a column short.
///
/// `pane.resize` lays out the tabs clients show, as the focus did,
/// and by `amount` 0 moves no split; so while the pty is a column narrow
/// it is asked, and the pty looked at again, until it fits or
/// `look_again` says there's no time for another look, waiting for it
/// if there is: a pane herdr hasn't yet seen switch fits once it has, and
/// one on the primary screen for good never does.
fn fit(
    herdr: &Herdr,
    pane: &str,
    columns_of: impl Fn(u32) -> Option<u16>,
    mut look_again: impl FnMut() -> bool,
) {
    while one_column_short(herdr, pane, &columns_of) == Some(true) {
        let resize = json!({ "pane_id": pane, "direction": "right", "amount": 0 });
        if herdr.call("pane.resize", resize).is_err() || !look_again() {
            return;
        }
    }
}

/// Waits [`FIT_LOOK`] for `fit`'s next look if there's time for it before
/// `due`: whether there was.
fn wait_for_a_look(due: Instant) -> bool {
    if Instant::now() + FIT_LOOK > due {
        return false;
    }
    std::thread::sleep(FIT_LOOK);
    true
}

/// Whether `pane`, alone in its unzoomed tab, has a pty one column
/// narrower than its rect, which has no frame; None if it isn't alone or
/// either can't be read.
fn one_column_short(
    herdr: &Herdr,
    pane: &str,
    columns_of: impl Fn(u32) -> Option<u16>,
) -> Option<bool> {
    let layout = herdr.call("pane.layout", json!({ "pane_id": pane })).ok()?;
    let layout = &layout["layout"];
    let [alone] = layout["panes"].as_array()?.as_slice() else {
        return None;
    };
    if layout["zoomed"] != false || alone["pane_id"] != pane {
        return None;
    }
    let width = alone["rect"]["width"].as_u64()?;
    let info = herdr
        .call("pane.process_info", json!({ "pane_id": pane }))
        .ok()?;
    let pid = info["process_info"]["shell_pid"].as_u64()?;
    let columns = columns_of(u32::try_from(pid).ok()?)?;
    Some(u64::from(columns) + 1 == width)
}

/// The columns of process `pid`'s controlling terminal, as `TIOCGWINSZ`
/// has them.
fn columns_of_process(pid: u32) -> Option<u16> {
    let path = tty::path_of_device(tty::of_pid(pid)?)?;
    let tty = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    // SAFETY: a zeroed winsize is valid for TIOCGWINSZ to fill.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes a winsize through the pointer.
    let r = unsafe { libc::ioctl(tty.as_raw_fd(), libc::TIOCGWINSZ, &mut size) };
    (r == 0).then_some(size.ws_col)
}

/// Moves focus `COUNT` panes `DIRECTION`, or says why not.
pub(super) fn focus_direction(given: &Given) -> Result<(), String> {
    let direction = given.var("direction").unwrap_or_default();
    let direction = match direction.as_slice() {
        b"left" => "left",
        b"right" => "right",
        b"up" => "up",
        b"down" => "down",
        other => {
            return Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let count = given.var("count").unwrap_or_default();
    let count = std::str::from_utf8(&count)
        .ok()
        .and_then(|c| c.parse::<u32>().ok())
        .filter(|c| (1..=20).contains(c))
        .ok_or_else(|| format!("COUNT {:?} isn't 1 to 20", String::from_utf8_lossy(&count)))?;
    let herdr = Herdr::of(given, FOCUS_TIME)?;
    let snapshot = herdr.call("session.snapshot", json!({}))?;
    let mut from = snapshot["snapshot"]["focused_pane_id"]
        .as_str()
        .ok_or("herdr has no focused pane")?
        .to_string();
    for _ in 0..count {
        let stepped = herdr.call(
            "pane.focus_direction",
            json!({ "pane_id": from, "direction": direction }),
        )?;
        let stepped = &stepped["focus"];
        if stepped["changed"] != true {
            return match stepped["reason"].as_str() {
                // at the tab's edge: done
                Some("no_neighbor") => Ok(()),
                Some(reason) => Err(format!(
                    "herdr didn't move focus {direction} from {from}: {reason}"
                )),
                None => Err(UNREADABLE.into()),
            };
        }
        from = stepped["focused_pane_id"]
            .as_str()
            .ok_or(UNREADABLE)?
            .to_string();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::fake::{Fake, Reply, given, recorded};
    use super::*;

    #[test]
    fn focus_focuses_the_pane_by_its_id() {
        // w1:p1 shares its tab: no pty is read
        let fake = Fake::serve(vec![
            ("pane.focus", Reply::Line(recorded("open-focus"))),
            ("pane.layout", Reply::Line(recorded("resize-layout"))),
        ]);
        let read = |_| panic!("no pty is read");
        let given = given(&fake, &[("TERMINAL", "w1:p1")]);
        assert_eq!(focus_by(&given, read), Ok(()));
        assert_eq!(fake.methods(), ["pane.focus", "pane.layout"]);
        assert_eq!(fake.asked()[0]["params"], json!({ "pane_id": "w1:p1" }));
        assert_eq!(fake.asked()[1]["params"], json!({ "pane_id": "w1:p1" }));
    }

    /// A fake herdr whose `w1:p2`, 74 columns wide, is alone in its tab.
    fn alone() -> Fake {
        Fake::serve(vec![
            ("pane.focus", Reply::Line(recorded("tab-page-focus"))),
            ("pane.layout", Reply::Line(recorded("tab-page-layout"))),
            (
                "pane.process_info",
                Reply::Line(recorded("tab-page-process-info")),
            ),
            ("pane.resize", Reply::Line(recorded("tab-page-resize"))),
        ])
    }

    #[test]
    fn a_pane_alone_whose_pty_fits_is_left_as_it_is() {
        let fake = alone();
        let given = given(&fake, &[("TERMINAL", "w1:p2")]);
        let read = |pid| {
            assert_eq!(pid, 20004);
            Some(74)
        };
        assert_eq!(focus_by(&given, read), Ok(()));
        assert_eq!(
            fake.methods(),
            ["pane.focus", "pane.layout", "pane.process_info"]
        );
    }

    /// The fake herdr's server, its requests having `FOCUS_TIME`, which
    /// only stops a test that would hang: when `fit` gives up is the
    /// test's to say.
    fn herdr(fake: &Fake) -> Herdr {
        Herdr::at(&fake.path(), FOCUS_TIME)
    }

    #[test]
    fn a_pane_alone_whose_pty_is_a_column_short_has_its_tab_laid_out_until_it_fits() {
        // herdr reads the switch to the alternate screen late: the first
        // lay-out still leaves the scrollbar's column
        let fake = alone();
        let looks = std::cell::Cell::new(0);
        let read = |_| {
            looks.set(looks.get() + 1);
            Some(if looks.get() < 3 { 73 } else { 74 })
        };
        fit(&herdr(&fake), "w1:p2", read, || true);
        assert_eq!(
            fake.methods(),
            [
                "pane.layout",
                "pane.process_info",
                "pane.resize",
                "pane.layout",
                "pane.process_info",
                "pane.resize",
                "pane.layout",
                "pane.process_info",
            ]
        );
        // no split moves, no focus moves
        assert_eq!(
            fake.asked()[2]["params"],
            json!({ "pane_id": "w1:p2", "direction": "right", "amount": 0 })
        );
    }

    #[test]
    fn a_pty_short_for_good_has_its_tab_laid_out_at_every_look_there_is_time_for() {
        let fake = alone();
        let mut more = 2;
        let look_again = || {
            more -= 1;
            more >= 0
        };
        fit(&herdr(&fake), "w1:p2", |_| Some(73), look_again);
        let look = ["pane.layout", "pane.process_info", "pane.resize"];
        assert_eq!(fake.methods(), look.repeat(3));
    }

    #[test]
    fn a_pty_short_for_good_is_given_up_on_and_the_focus_done() {
        let fake = alone();
        let given = given(&fake, &[("TERMINAL", "w1:p2")]);
        let started = Instant::now();
        assert_eq!(focus_by(&given, |_| Some(73)), Ok(()));
        // given up once its time is up, not before
        let took = started.elapsed();
        assert!(took >= FIT_TIME - FIT_LOOK, "{took:?}");
        assert_eq!(fake.methods()[0], "pane.focus");
    }

    #[test]
    fn a_look_is_waited_for_only_if_there_is_time_for_it() {
        let started = Instant::now();
        assert!(!wait_for_a_look(started + FIT_LOOK / 2));
        assert!(!wait_for_a_look(started));
        assert!(wait_for_a_look(started + Duration::from_secs(60)));
        assert!(started.elapsed() >= FIT_LOOK);
    }

    #[test]
    fn a_pty_narrower_by_more_or_unread_is_left_as_it_is() {
        // a frame's columns, say, or a pane with no process
        for read in [Some(72), Some(80), None] {
            let fake = alone();
            let given = given(&fake, &[("TERMINAL", "w1:p2")]);
            assert_eq!(focus_by(&given, |_| read), Ok(()));
            assert_eq!(
                fake.methods(),
                ["pane.focus", "pane.layout", "pane.process_info"],
                "{read:?}"
            );
        }
    }

    #[test]
    fn a_zoomed_tab_or_a_layout_herdr_wont_give_reads_no_pty() {
        let zoomed = recorded("tab-page-layout").replace(r#""zoomed":false"#, r#""zoomed":true"#);
        for layout in [zoomed, recorded("pane-not-found")] {
            let fake = Fake::serve(vec![
                ("pane.focus", Reply::Line(recorded("tab-page-focus"))),
                ("pane.layout", Reply::Line(layout)),
            ]);
            let given = given(&fake, &[("TERMINAL", "w1:p2")]);
            assert_eq!(focus_by(&given, |_| panic!("no pty is read")), Ok(()));
            assert_eq!(fake.methods(), ["pane.focus", "pane.layout"]);
        }
    }

    #[test]
    fn the_columns_of_a_process_are_its_controlling_terminals() {
        use crate::providers::tty::tests::{end, pty, sleeper};
        let (master, slave, _) = pty();
        let size = libc::winsize {
            ws_row: 29,
            ws_col: 57,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCSWINSZ reads a winsize through the pointer.
        assert_eq!(0, unsafe {
            libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size)
        });
        let child = sleeper(Some(&slave));
        let columns = columns_of_process(child.id());
        end(child);
        assert_eq!(columns, Some(57));
        // no terminal, or no process
        let child = sleeper(None);
        let columns = columns_of_process(child.id());
        end(child);
        assert_eq!(columns, None);
        assert_eq!(columns_of_process(u32::MAX), None);
    }

    #[test]
    fn focus_on_a_pane_herdr_doesnt_have_fails() {
        let fake = Fake::serve(vec![(
            "pane.focus",
            Reply::Line(recorded("pane-not-found")),
        )]);
        assert_eq!(
            focus(&given(&fake, &[("TERMINAL", "w1:p9")])),
            Err("herdr: pane not found".into())
        );
        let fake = Fake::serve(vec![]);
        assert_eq!(focus(&given(&fake, &[])), Err("TERMINAL isn't set".into()));
        assert!(fake.asked().is_empty());
    }

    /// The params of each `pane.focus_direction` `fake` was asked.
    fn steps(fake: &Fake) -> Vec<Value> {
        fake.asked()
            .into_iter()
            .filter(|r| r["method"] == "pane.focus_direction")
            .map(|r| r["params"].clone())
            .collect()
    }

    #[test]
    fn focus_direction_steps_from_the_servers_focus_then_from_where_it_landed() {
        // the server's focus is w1:p2; the first step lands on w1:p6
        let fake = Fake::serve(vec![
            (
                "session.snapshot",
                Reply::Line(recorded("session-snapshot")),
            ),
            (
                "pane.focus_direction",
                Reply::Line(recorded("focus-direction")),
            ),
        ]);
        let given = given(&fake, &[("DIRECTION", "right"), ("COUNT", "2")]);
        assert_eq!(focus_direction(&given), Ok(()));
        assert_eq!(
            steps(&fake),
            [
                json!({ "pane_id": "w1:p2", "direction": "right" }),
                json!({ "pane_id": "w1:p6", "direction": "right" }),
            ]
        );
    }

    #[test]
    fn focus_direction_stops_at_the_edge_and_is_done() {
        let fake = Fake::serve(vec![
            (
                "session.snapshot",
                Reply::Line(recorded("session-snapshot")),
            ),
            (
                "pane.focus_direction",
                Reply::Line(recorded("focus-direction-edge")),
            ),
        ]);
        let given = given(&fake, &[("DIRECTION", "left"), ("COUNT", "5")]);
        assert_eq!(focus_direction(&given), Ok(()));
        assert_eq!(
            steps(&fake),
            [json!({ "pane_id": "w1:p2", "direction": "left" })]
        );
    }

    #[test]
    fn a_step_that_moves_nothing_for_another_reason_fails_with_it() {
        let edge = recorded("focus-direction-edge");
        for (reply, why) in [
            (
                edge.replace("no_neighbor", "zoomed"),
                "herdr didn't move focus left from w1:p2: zoomed",
            ),
            (edge.replace(r#""reason":"no_neighbor","#, ""), UNREADABLE),
            (recorded("pane-not-found"), "herdr: pane not found"),
        ] {
            let fake = Fake::serve(vec![
                (
                    "session.snapshot",
                    Reply::Line(recorded("session-snapshot")),
                ),
                ("pane.focus_direction", Reply::Line(reply)),
            ]);
            let given = given(&fake, &[("DIRECTION", "left"), ("COUNT", "3")]);
            assert_eq!(focus_direction(&given), Err(why.into()));
            assert_eq!(steps(&fake).len(), 1);
        }
    }

    #[test]
    fn focus_direction_with_bad_inputs_asks_herdr_nothing() {
        let fake = Fake::serve(vec![]);
        for (vars, why) in [
            (
                vec![("DIRECTION", "sideways"), ("COUNT", "1")],
                "DIRECTION \"sideways\" isn't left, right, up or down",
            ),
            (
                vec![("DIRECTION", "up"), ("COUNT", "0")],
                "COUNT \"0\" isn't 1 to 20",
            ),
            (
                vec![("DIRECTION", "up"), ("COUNT", "21")],
                "COUNT \"21\" isn't 1 to 20",
            ),
            (vec![("DIRECTION", "up")], "COUNT \"\" isn't 1 to 20"),
        ] {
            assert_eq!(focus_direction(&given(&fake, &vars)), Err(why.into()));
        }
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn focus_direction_with_no_focused_pane_fails() {
        let empty = r#"{"id":"x","result":{"type":"session_snapshot","snapshot":{"focused_pane_id":null,"layouts":[]}}}"#;
        let fake = Fake::serve(vec![("session.snapshot", Reply::Line(empty.into()))]);
        let given = given(&fake, &[("DIRECTION", "up"), ("COUNT", "1")]);
        assert_eq!(
            focus_direction(&given),
            Err("herdr has no focused pane".into())
        );
        assert_eq!(fake.methods(), ["session.snapshot"]);
    }
}
