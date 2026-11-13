//! `open_float`: a tmux floating pane over the origin's window, from
//! `new-pane -d`, running the attach command, its content on the float
//! group's cells. tmux 3.7 and newer; older ones have no floats, and it
//! exits 69.

use std::ffi::OsString;

use super::Failure;
use super::layout::{PANES, reported_corner};
use super::panes::{attach_words, pane_id, printed_pane};
use super::server::{Server, older_than};
use crate::providers::builtin::Given;

/// The first tmux with floating panes, `new-pane`.
pub(super) const FIRST_WITH_FLOATS: (u32, u32) = (3, 7);

/// The first tmux whose `new-pane -x -y -X -Y` give the frame.
const FIRST_WITH_FRAMED_FLAGS: (u32, u32) = (3, 8);

/// The new pane's id, or why there's none.
pub(super) fn open_float(given: &Given) -> Result<String, Failure> {
    let server = Server::of(given)?;
    let version = Server::version()?;
    let Some(geometry) = Geometry::of(&version) else {
        let (major, minor) = FIRST_WITH_FLOATS;
        return Err(Failure::Unsupported(format!(
            "tmux {version} has no floating panes: they need {major}.{minor} or newer"
        )));
    };
    let origin = pane_id(given, "origin")?;
    let words = attach_words(given)?;
    let at = [number(given, "x")?, number(given, "y")?];
    let size = [cells(given, "cols")?, cells(given, "rows")?];
    let shown = server.run(&["display-message", "-p", "-t", &origin, PANES])?;
    let [x, y, ox, oy] = placement(geometry, at, size, &shown)?;
    let mut args: Vec<OsString> = vec!["new-pane".into(), "-d".into()];
    for (flag, n) in [("-x", x), ("-y", y), ("-X", ox), ("-Y", oy)] {
        args.extend([flag.into(), n.to_string().into()]);
    }
    args.extend(["-t".into(), origin.into()]);
    args.extend(["-P".into(), "-F".into(), "#{pane_id}".into()]);
    args.extend(words);
    Ok(printed_pane(&server.run(&args)?, "new-pane")?)
}

/// `new-pane`'s `-x -y -X -Y` for a float group of `size` cells at `at`
/// cells from the top left of the origin's rect, as `report_layout` gives
/// it, the origin being `shown`, its line of `PANES`; or why it can't
/// read it.
fn placement(
    geometry: Geometry,
    at: [i64; 2],
    size: [u32; 2],
    shown: &[u8],
) -> Result<[i64; 4], String> {
    let (corner, window) = reported_corner(shown).ok_or_else(|| {
        format!(
            "tmux gave the origin's place as {:?}",
            String::from_utf8_lossy(shown).trim()
        )
    })?;
    Ok(flags(geometry, at, size, corner, window))
}

/// Input `name`, an integer, or why not.
fn number(given: &Given, name: &str) -> Result<i64, String> {
    let n = given.var(name).unwrap_or_default();
    std::str::from_utf8(&n)
        .ok()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            format!(
                "{} {:?} isn't an integer",
                name.to_ascii_uppercase(),
                String::from_utf8_lossy(&n)
            )
        })
}

/// Input `name`, a count of cells, one at least, or why not.
fn cells(given: &Given, name: &str) -> Result<u32, String> {
    number(given, name)?
        .try_into()
        .ok()
        .filter(|&n: &u32| n >= 1)
        .ok_or_else(|| format!("{} isn't a count of cells", name.to_ascii_uppercase()))
}

/// What `new-pane`'s `-x -y -X -Y` give, which depends on tmux's version.
/// Either way tmux draws a frame one cell outside the content: kept, as a
/// float with none merges into the pane beneath.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Geometry {
    /// 3.7: the content's size and top left. Its size must be under the
    /// window's.
    Content,
    /// 3.8 and newer: the frame's.
    Frame,
}

impl Geometry {
    /// How tmux `version`, as `tmux -V` gives it after `tmux `, reads the
    /// flags; None below 3.7. A version it can't read, such as `master`,
    /// is taken for the newest.
    fn of(version: &str) -> Option<Geometry> {
        if older_than(version, FIRST_WITH_FLOATS) {
            None
        } else if older_than(version, FIRST_WITH_FRAMED_FLAGS) {
            Some(Geometry::Content)
        } else {
            Some(Geometry::Frame)
        }
    }
}

/// `new-pane`'s `-x -y -X -Y` for a float group of `size` cells, cols and
/// rows, at `at` cells from the origin's top left, the origin's rect
/// being at `origin` in its window of `window` cells: the content lands
/// on the group, but for 3.7's, which can't be as big as the window.
fn flags(
    geometry: Geometry,
    at: [i64; 2],
    size: [u32; 2],
    origin: [i64; 2],
    window: [u32; 2],
) -> [i64; 4] {
    let left = origin[0] + at[0];
    let top = origin[1] + at[1];
    let [cols, rows] = size.map(i64::from);
    match geometry {
        Geometry::Content => {
            let [most_cols, most_rows] = window.map(|n| (i64::from(n) - 1).max(1));
            [cols.min(most_cols), rows.min(most_rows), left, top]
        }
        Geometry::Frame => [cols + 2, rows + 2, left - 1, top - 1],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_need_3_7_whose_flags_give_the_content_and_later_ones_the_frame() {
        for old in ["3.2a", "3.6", "3.6b", "2.9"] {
            assert_eq!(Geometry::of(old), None, "{old}");
        }
        assert_eq!(Geometry::of("3.7"), Some(Geometry::Content));
        assert_eq!(Geometry::of("3.7c"), Some(Geometry::Content));
        for new in ["3.8", "3.8a", "3.10", "4.0", "next-3.9", "master"] {
            assert_eq!(Geometry::of(new), Some(Geometry::Frame), "{new}");
        }
    }

    /// The origin `%1` is the right half of an 80x24 window, at 41,0; the
    /// group 20x6 at 10,4 in it. Measured on tmux 3.7c and 3.8: 3.7's
    /// `-x 20 -y 6 -X 51 -Y 4` and 3.8's `-x 22 -y 8 -X 50 -Y 3` both put
    /// the content at 51,4, 20x6.
    #[test]
    fn the_flags_put_the_content_on_the_group_in_window_cells() {
        let flags = |g| flags(g, [10, 4], [20, 6], [41, 0], [80, 24]);
        assert_eq!(flags(Geometry::Content), [20, 6, 51, 4]);
        assert_eq!(flags(Geometry::Frame), [22, 8, 50, 3]);
    }

    /// Recorded from tmux 3.8, `pane-border-status top`: `%0` is the left
    /// half of an 80x24 window, its content from row 1, the status line
    /// on row 0. `report_layout` gives its rect from row 0, and the group's
    /// `X`,`Y` are from that rect's top left: the content lands at 10,3,
    /// not a row lower.
    #[test]
    fn under_a_top_status_line_the_group_is_placed_from_the_reported_rect() {
        let line = b"@0 80 24 %0 0 1 40 23 1 top 0\n";
        let at = |g| placement(g, [10, 3], [20, 5], line).unwrap();
        assert_eq!(at(Geometry::Content), [20, 5, 10, 3]);
        assert_eq!(at(Geometry::Frame), [22, 7, 9, 2]);
        // without the status line, the content's top left is the rect's
        let line = b"@0 80 24 %1 41 0 39 24 1 off 0\n";
        let at = |g| placement(g, [10, 4], [20, 6], line).unwrap();
        assert_eq!(at(Geometry::Content), [20, 6, 51, 4]);
        assert!(placement(Geometry::Frame, [0, 0], [1, 1], b"%0 0 1\n").is_err());
    }

    /// A group may start left of or above its origin, and its frame off
    /// the window: tmux takes both.
    #[test]
    fn a_group_left_of_and_above_its_origin_and_a_frame_off_the_window() {
        let flags = |g| flags(g, [-3, -2], [5, 3], [3, 2], [80, 24]);
        assert_eq!(flags(Geometry::Content), [5, 3, 0, 0]);
        assert_eq!(flags(Geometry::Frame), [7, 5, -1, -1]);
    }

    /// 3.7 refuses a float as big as its window ("size or position too
    /// large"): its content is a cell short of the window's, the group's
    /// floats fitting the size it got. 3.8 takes a frame bigger than the
    /// window, its content measured at 0,0, 80x24.
    #[test]
    fn a_group_the_size_of_the_window_is_a_cell_short_on_3_7() {
        let flags = |g| flags(g, [0, 0], [80, 24], [0, 0], [80, 24]);
        assert_eq!(flags(Geometry::Content), [79, 23, 0, 0]);
        assert_eq!(flags(Geometry::Frame), [82, 26, -1, -1]);
    }
}
