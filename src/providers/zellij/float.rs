//! `open_float`: a zellij floating pane over the origin's tab, running the
//! attach command, its content on the float group's cells.
//!
//! `new-pane --floating --no-focus`, issued from the origin
//! (`ZELLIJ_PANE_ID`), opens it in the origin's tab, moving no client's
//! focus. zellij shows a tab's floats only while one of them has focus, so
//! it is pinned (`--pinned true`), shown whichever pane has focus, as a
//! Neovim float is whichever window is current.
//!
//! zellij's `-x -y --width --height` are the float's frame, in cells of
//! the screen, as `list-panes` gives every rect: its content is a cell
//! inside on each side. The frame asked for is the origin's content's top
//! left moved by `X`,`Y`, less a cell, and `COLS`x`ROWS` and a cell on each
//! side, no left or top under 0; zellij keeps it inside the tab, and the
//! next `report_layout` says where it went. `AREA_BOUNDS` is ignored.

use std::ffi::{OsStr, OsString};

use super::session::Session;
use super::{pane_rows, printed_terminal, terminal_of, terminal_row};
use crate::name::NAME;
use crate::providers::builtin::Given;

/// The new pane's id, `terminal_N`, or why there's none.
pub(super) fn open_float(given: &Given) -> Result<String, String> {
    let session = Session::of(given)?;
    let float = Float {
        origin: terminal_of(given, "origin")?,
        words: given.attach_words()?,
        at: [given.integer("x")?, given.integer("y")?],
        size: [given.cells("cols")?, given.cells("rows")?],
    };
    float.open(&session)
}

/// A float group of `size` cells, cols and rows, at `at` cells from the
/// top left of terminal pane `origin`'s content, running the attach's
/// `words`.
struct Float {
    origin: u32,
    words: Vec<OsString>,
    at: [i64; 2],
    size: [u32; 2],
}

impl Float {
    /// The new pane's id, or why there's none.
    fn open(&self, session: &Session) -> Result<String, String> {
        let panes = session.run(&["list-panes", "--json"])?;
        let corner = content_corner(&panes, self.origin)?;
        let mut args: Vec<OsString> = [
            "new-pane",
            "--floating",
            "--pinned",
            "true",
            "--no-focus",
            "--close-on-exit",
            "--name",
            NAME,
        ]
        .map(OsString::from)
        .to_vec();
        for (flag, n) in ["-x", "-y", "--width", "--height"]
            .into_iter()
            .zip(frame(self.at, self.size, corner))
        {
            args.extend([flag.into(), n.to_string().into()]);
        }
        args.push("--".into());
        args.extend(self.words.iter().cloned());
        let args: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
        printed_terminal(&session.run_from(self.origin, &args)?, "new-pane")
    }
}

/// The top left of terminal pane `origin`'s content, in cells of the
/// screen, as `list-panes --json`, `panes`, gives it; or why not.
fn content_corner(panes: &[u8], origin: u32) -> Result<[i64; 2], String> {
    let rows = pane_rows(panes)?;
    let row = terminal_row(&rows, origin)
        .ok_or_else(|| format!("list-panes doesn't list terminal_{origin}"))?;
    let n = |k: &str| row[k].as_i64();
    match (n("pane_content_x"), n("pane_content_y")) {
        (Some(x), Some(y)) => Ok([x, y]),
        _ => Err(format!("list-panes gave terminal_{origin} as {row}")),
    }
}

/// `new-pane`'s `-x -y --width --height` for a float group of `size`
/// cells, cols and rows, at `at` cells from the top left of the origin's
/// content, which is at `corner` on the screen: the frame a cell outside
/// the group on each side, its left and top no less than 0.
fn frame(at: [i64; 2], size: [u32; 2], corner: [i64; 2]) -> [i64; 4] {
    let [cols, rows] = size.map(i64::from);
    [
        (corner[0] + at[0] - 1).max(0),
        (corner[1] + at[1] - 1).max(0),
        cols + 2,
        rows + 2,
    ]
}

#[cfg(test)]
mod tests {
    use super::super::fake::Fake;
    use super::*;

    /// terminal_1, listed with its content at 60,2.
    const LISTED: &str =
        r#"echo '[{"id": 1, "is_plugin": false, "pane_content_x": 60, "pane_content_y": 2}]'"#;

    fn group() -> Float {
        Float {
            origin: 1,
            words: vec!["/bin/neo pane".into(), "attach".into()],
            at: [3, 4],
            size: [20, 6],
        }
    }

    #[test]
    fn a_float_opens_pinned_from_its_origin_moving_no_focus() {
        let fake = Fake::new(
            "float",
            &[("list-panes", LISTED), ("new-pane", "echo terminal_9")],
        );
        assert_eq!(group().open(&fake.session()), Ok("terminal_9".into()));
        assert_eq!(
            fake.calls(),
            [
                "list-panes --json".to_string(),
                format!(
                    "ZELLIJ_PANE_ID=1 new-pane --floating --pinned true --no-focus \
                     --close-on-exit --name {NAME} -x 62 -y 5 --width 22 --height 8 \
                     -- /bin/neo pane attach"
                ),
            ]
        );
    }

    #[test]
    fn a_float_whose_origin_isnt_listed_opens_nothing() {
        let fake = Fake::new("float-unlisted", &[("list-panes", "echo '[]'")]);
        let why = group().open(&fake.session()).unwrap_err();
        assert_eq!(why, "list-panes doesn't list terminal_1");
        assert_eq!(fake.asked(), ["list-panes"]);
        // nor one zellij gives no id for
        let fake = Fake::new(
            "float-no-id",
            &[("list-panes", LISTED), ("new-pane", "echo plugin_2")],
        );
        assert!(group().open(&fake.session()).is_err());
    }

    /// Measured on zellij 0.46.0: `-x 10 -y 5 --width 22 --height 8` gave a
    /// float whose frame is at 10,5, 22x8, and its content at 11,6, 20x6.
    /// The origin's content at 0,2, under the tab bar and its title row,
    /// and the group 20x6 at 11,4 from it: the content lands on it.
    #[test]
    fn the_frame_is_a_cell_outside_the_group_on_each_side() {
        assert_eq!(frame([11, 4], [20, 6], [0, 2]), [10, 5, 22, 8]);
        // from a floating origin's content, or a tiled one's further right
        assert_eq!(frame([2, 3], [5, 2], [71, 6]), [72, 8, 7, 4]);
    }

    /// A group at the origin's content's top left, or left of or above it,
    /// asks no frame off the screen's top left: zellij keeps a float in its
    /// tab anyway.
    #[test]
    fn the_frame_starts_no_further_left_or_up_than_the_screens_corner() {
        assert_eq!(frame([0, 0], [80, 24], [0, 0]), [0, 0, 82, 26]);
        assert_eq!(frame([-3, -2], [5, 3], [1, 1]), [0, 0, 7, 5]);
        assert_eq!(frame([0, 0], [5, 3], [1, 2]), [0, 1, 7, 5]);
    }

    #[test]
    fn the_origins_content_corner_is_read_from_its_row() {
        let panes = include_bytes!("fixtures/0.46-panes.json");
        // tiled terminal_1 at 60,1, its title row above its content
        assert_eq!(content_corner(panes, 1), Ok([60, 2]));
        // float terminal_3 at 10,5, framed
        assert_eq!(content_corner(panes, 3), Ok([11, 6]));
        assert_eq!(
            content_corner(panes, 99),
            Err("list-panes doesn't list terminal_99".into())
        );
        assert!(content_corner(b"nonsense", 1).is_err());
        assert!(content_corner(br#"[{"id": 1, "is_plugin": false}]"#, 1).is_err());
    }
}
