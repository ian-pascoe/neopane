//! `focus` and `focus_direction`.
//!
//! zellij acts for the client that last pressed a key, wherever it is:
//! before any client has, a focus action moves nobody, and `focus-pane-id`
//! still exits 0; with no client attached, `move-focus` fails.
//!
//! - `focus`: `focus-pane-id`, which goes to the pane's tab. A pane already
//!   focused exits 2, `Pane Terminal(N) is already focused`: done. Focusing
//!   a floating pane shows its tab's floats, and a tiled one hides them.
//! - `focus_direction`: `move-focus` `COUNT` times, one call each. It never
//!   wraps: at the tab's edge it moves nowhere, and exits 0.

use super::session::{Ran, Session};
use super::terminal_of;
use crate::providers::builtin::Given;

/// Focuses the terminal, or says why not.
pub(super) fn focus(given: &Given) -> Result<(), String> {
    focused(&Session::of(given)?, terminal_of(given, "terminal")?)
}

/// Focuses terminal pane `pane` of `session`, or says why not.
fn focused(session: &Session, pane: u32) -> Result<(), String> {
    match session.run(&["focus-pane-id", &format!("terminal_{pane}")]) {
        Ok(_) => Ok(()),
        Err(Ran::Failed(why))
            if why.ends_with(&format!(": Pane Terminal({pane}) is already focused")) =>
        {
            Ok(())
        }
        Err(why) => Err(why.into()),
    }
}

/// Moves focus `COUNT` panes `DIRECTION`, or says why not.
pub(super) fn focus_direction(given: &Given) -> Result<(), String> {
    let session = Session::of(given)?;
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
    moved(&session, direction, count)
}

/// Moves focus `count` panes `direction` in `session`, or says why not.
fn moved(session: &Session, direction: &str, count: u32) -> Result<(), String> {
    for _ in 0..count {
        session.run(&["move-focus", direction])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::fake::Fake;
    use super::*;

    #[test]
    fn focus_on_the_pane_already_focused_is_done() {
        let already = "echo 'Pane Terminal(3) is already focused' >&2; exit 2";
        let fake = Fake::new("focus-already", &[("focus-pane-id", already)]);
        assert_eq!(focused(&fake.session(), 3), Ok(()));
        assert_eq!(fake.calls(), ["focus-pane-id terminal_3"]);
        // another pane's, or another failure, isn't
        let other = Fake::new("focus-other", &[("focus-pane-id", already)]);
        assert!(focused(&other.session(), 4).is_err());
        let gone = "echo 'Pane with id Terminal(3) not found' >&2; exit 2";
        let gone = Fake::new("focus-gone", &[("focus-pane-id", gone)]);
        let why = focused(&gone.session(), 3).unwrap_err();
        assert!(why.ends_with("Pane with id Terminal(3) not found"), "{why}");
        let done = Fake::new("focus-done", &[("focus-pane-id", "true")]);
        assert_eq!(focused(&done.session(), 3), Ok(()));
    }

    #[test]
    fn focus_direction_moves_focus_count_times_and_stops_at_a_failure() {
        let fake = Fake::new("move-3", &[("move-focus", "true")]);
        assert_eq!(moved(&fake.session(), "left", 3), Ok(()));
        assert_eq!(fake.calls(), ["move-focus left"; 3]);
        let none = "echo 'No connected clients to move focus for' >&2; exit 2";
        let fake = Fake::new("move-none", &[("move-focus", none)]);
        let why = moved(&fake.session(), "down", 3).unwrap_err();
        assert!(
            why.ends_with("No connected clients to move focus for"),
            "{why}"
        );
        assert_eq!(fake.calls(), ["move-focus down"]);
    }
}
