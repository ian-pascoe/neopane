//! `open_tab_page`: a herdr tab is a tab page area. `layout.apply` makes
//! a tab in `ORIGIN`'s workspace, `focus:false`, its one pane running the
//! attach command, and leaves the server's focus where it is: the daemon
//! focuses the new pane when Neovim goes to its tab page.
//!
//! The pane goes to the alternate screen before the attach starts
//! ([`ON_THE_ALTERNATE_SCREEN`]), not as the daemon draws its first frame:
//! herdr, which keeps a scrollbar's column of a pane on the primary
//! screen, sizes the tab's ptys as it answers `pane.focus`, and a pane
//! whose switch it reads between that and its next frame keeps the column
//! short until the tab is next laid out, which a tab of one pane, with no
//! split to set, never is (SPI-1207). That narrows the race, but a focus
//! can still come before herdr has read the switch; `focus` gives the
//! column back (`focus::fit`).
//!
//! A `layout.apply` with no answer may have made its tab all the same:
//! the daemon gives up a pane whose open failed, refusing its attach, or
//! ending it as closed if it came first, and the attach ends; herdr closes
//! a pane whose command ended, and a tab with its last pane.

use std::time::Duration;

use serde_json::json;

use super::client::{Herdr, text};
use crate::providers::builtin::Given;

/// What `open_tab_page` has, of its 5 s.
const OPEN_TIME: Duration = Duration::from_millis(4000);

/// Runs the words after it, the attach command, on the alternate screen:
/// `sh` enters it and becomes the attach, which is the pane's command
/// from then on. The daemon enters it again as it draws, changing nothing.
const ON_THE_ALTERNATE_SCREEN: [&str; 4] =
    ["/bin/sh", "-c", r#"printf '\033[?1049h'; exec "$@""#, "sh"];

/// The new tab's pane, or why there's none.
pub(super) fn open_tab_page(given: &Given) -> Result<String, String> {
    let origin = given.text("origin")?;
    let mut words: Vec<String> = ON_THE_ALTERNATE_SCREEN.map(String::from).into();
    words.extend(given.attach_strings("herdr")?);
    let herdr = Herdr::of(given, OPEN_TIME)?;
    let pane = herdr.call("pane.get", json!({ "pane_id": origin }))?;
    let workspace = text(&pane["pane"], "workspace_id")?;
    let applied = herdr.call(
        "layout.apply",
        json!({
            "workspace_id": workspace,
            "focus": false,
            "root": { "type": "pane", "command": words },
        }),
    )?;
    Ok(text(&applied["layout"]["root"], "pane_id")?.to_string())
}

#[cfg(test)]
mod tests {
    use super::super::fake::{ATTACH, Fake, Reply, given, recorded};
    use super::*;

    #[test]
    fn it_runs_the_attach_in_a_new_unfocused_tab_of_the_origins_workspace() {
        // w1:p6 is in w1; herdr makes tab w1:t5, its pane w1:p8
        let fake = Fake::serve(vec![
            ("pane.get", Reply::Line(recorded("pane-get"))),
            ("layout.apply", Reply::Line(recorded("tab-page-apply"))),
        ]);
        let given = given(
            &fake,
            &[
                ("ORIGIN", "w1:p6"),
                ("ATTACH", ATTACH),
                ("TITLE", "np-2-k3j9x2qpa8"),
            ],
        );
        assert_eq!(open_tab_page(&given), Ok("w1:p8".into()));
        assert_eq!(fake.methods(), ["pane.get", "layout.apply"]);
        let asked = fake.asked();
        assert_eq!(asked[0]["params"], json!({ "pane_id": "w1:p6" }));
        assert_eq!(
            asked[1]["params"],
            json!({
                "workspace_id": "w1",
                "focus": false,
                "root": { "type": "pane", "command": [
                    "/bin/sh", "-c", r#"printf '\033[?1049h'; exec "$@""#, "sh",
                    "/bin/np", "attach", "--address", "/tmp/a b",
                ] },
            })
        );
    }

    #[test]
    fn an_origin_herdr_doesnt_have_makes_nothing() {
        let fake = Fake::serve(vec![("pane.get", Reply::Line(recorded("pane-not-found")))]);
        let given = given(&fake, &[("ORIGIN", "w1:p9"), ("ATTACH", ATTACH)]);
        assert_eq!(open_tab_page(&given), Err("herdr: pane not found".into()));
        assert_eq!(fake.methods(), ["pane.get"]);
    }

    #[test]
    fn bad_inputs_ask_herdr_nothing() {
        let fake = Fake::serve(vec![]);
        for (vars, why) in [
            (vec![("ATTACH", ATTACH)], "ORIGIN isn't set"),
            (vec![("ORIGIN", "w1:p6")], "ATTACH is empty"),
        ] {
            assert_eq!(open_tab_page(&given(&fake, &vars)), Err(why.into()));
        }
        assert!(fake.asked().is_empty());
    }
}
