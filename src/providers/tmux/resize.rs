//! `resize` and `place_beside`: sizing a terminal against the terminal
//! across its split, from tmux's layout tree (`window_layout.rs`).
//!
//! `resize-pane -x/-y` on a pane resizes the cell holding it in its
//! nearest split of that kind, against the next cell, or the one before if
//! it is the last. So to move the border between two cells of a split,
//! resize a pane of the first one that has no split of that kind between
//! it and the split, to the first cell's new size; failing that, one of
//! the second, if it is the last, to its own. Where neither has such a
//! pane, no single `resize-pane` moves that border: `resize` fails. A
//! `select-layout` of the whole window is the way past it, not taken yet.
//!
//! - `resize`: the terminal's cell in the deepest split holding it and
//!   `ORIGIN` gets `SIZE` cells, by the border it shares with the cell
//!   toward `ORIGIN`: the one `ORIGIN` is in, when they are next to each
//!   other.
//! - `place_beside`: `join-pane -d`, `-h` or `-v` and `-b` from
//!   `DIRECTION`, `-l SIZE`, the terminal from where it is to beside
//!   `ORIGIN`, without focusing it; then the `resize`, since within one
//!   window the room the terminal leaves goes to its neighbour, which may
//!   be itself.
//!
//! Both unzoom the window, as tmux's own commands do; the layout read is
//! the unzoomed one either way.

use super::panes::pane_id;
use super::server::Server;
use super::window_layout::{Cell, Kind, Split, parse};
use crate::providers::builtin::Given;

/// Sizes the terminal, or says why it can't.
pub(super) fn resize(given: &Given) -> Result<(), String> {
    let server = Server::of(given)?;
    let ask = Asked::of(given)?;
    ask.resize(&server)
}

/// Moves the terminal beside the origin and sizes it, or says why not.
pub(super) fn place_beside(given: &Given) -> Result<(), String> {
    let server = Server::of(given)?;
    let ask = Asked::of(given)?;
    let axis = match ask.split {
        Split::SideBySide => "-h",
        Split::Stacked => "-v",
    };
    let size = ask.size.to_string();
    let mut args = vec!["join-pane", "-d", axis];
    if ask.before {
        args.push("-b");
    }
    args.extend(["-l", &size, "-s", &ask.terminal, "-t", &ask.origin]);
    server.run(&args)?;
    ask.resize(&server)
}

/// What `resize` and `place_beside` are given.
struct Asked {
    terminal: String,
    origin: String,
    split: Split,
    /// The terminal goes left of or above `origin`.
    before: bool,
    size: u32,
}

impl Asked {
    fn of(given: &Given) -> Result<Asked, String> {
        let terminal = pane_id(given, "terminal")?;
        let origin = pane_id(given, "origin")?;
        let direction = given.var("direction").unwrap_or_default();
        let (split, before) = match direction.as_slice() {
            b"left" => (Split::SideBySide, true),
            b"right" => (Split::SideBySide, false),
            b"up" => (Split::Stacked, true),
            b"down" => (Split::Stacked, false),
            other => {
                return Err(format!(
                    "DIRECTION {:?} isn't left, right, up or down",
                    String::from_utf8_lossy(other)
                ));
            }
        };
        let size = given.var("size").unwrap_or_default();
        let size = std::str::from_utf8(&size)
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("SIZE {:?} isn't cells", String::from_utf8_lossy(&size)))?;
        Ok(Asked {
            terminal,
            origin,
            split,
            before,
            size,
        })
    }

    fn resize(&self, server: &Server) -> Result<(), String> {
        let layout = server.run(&[
            "display-message",
            "-p",
            "-t",
            &self.terminal,
            "#{window_layout}",
        ])?;
        let tree = parse(&String::from_utf8_lossy(&layout))?;
        let step = plan(&tree, &self.terminal, &self.origin, self.split, self.size)?;
        let flag = match self.split {
            Split::SideBySide => "-x",
            Split::Stacked => "-y",
        };
        server.run(&[
            "resize-pane",
            "-t",
            &step.pane,
            flag,
            &step.cells.to_string(),
        ])?;
        Ok(())
    }
}

/// The `resize-pane` that sizes a terminal: pane `pane` to `cells` along
/// the split.
#[derive(Debug, PartialEq)]
struct Step {
    pane: String,
    cells: u32,
}

/// The step that gives `terminal`'s cell `size` cells along `split`, in
/// the deepest split of `root` holding it and `origin`, by the border
/// toward `origin`; or why there's none.
fn plan(
    root: &Cell,
    terminal: &str,
    origin: &str,
    split: Split,
    size: u32,
) -> Result<Step, String> {
    let (Some(to_terminal), Some(to_origin)) = (path(root, terminal), path(root, origin)) else {
        return Err(format!("{terminal} and {origin} aren't in one window"));
    };
    let depth = to_terminal
        .iter()
        .zip(&to_origin)
        .take_while(|(a, b)| a == b)
        .count();
    if depth == to_terminal.len() || depth == to_origin.len() {
        return Err(format!("{terminal} is {origin}"));
    }
    let mut shared = root;
    for &i in &to_terminal[..depth] {
        let Kind::Split(_, cells) = &shared.kind else {
            unreachable!("a path goes through splits")
        };
        shared = &cells[i];
    }
    let Kind::Split(kind, cells) = &shared.kind else {
        unreachable!("two panes share a split")
    };
    if *kind != split {
        let way = match split {
            Split::SideBySide => "side by side",
            Split::Stacked => "stacked",
        };
        return Err(format!("{terminal} and {origin} aren't split {way}"));
    }
    let tiled: Vec<&Cell> = cells.iter().filter(|c| !c.floating).collect();
    let at = |i: usize| tiled.iter().position(|c| std::ptr::eq(*c, &cells[i]));
    let (Some(t), Some(o)) = (at(to_terminal[depth]), at(to_origin[depth])) else {
        return Err(format!("{terminal} or {origin} floats"));
    };
    // the border between `before` and the cell after it
    let (before, terminal_first) = if o < t { (t - 1, false) } else { (t, true) };
    let (first, second) = (tiled[before], tiled[before + 1]);
    let total = first.size(split) + second.size(split);
    if total < 2 {
        return Err(format!("{terminal} and {origin} have no room to share"));
    }
    let size = size.clamp(1, total - 1);
    let (first_size, second_size) = if terminal_first {
        (size, total - size)
    } else {
        (total - size, size)
    };
    if let Some(pane) = reachable(first, split) {
        return Ok(Step {
            pane: pane.into(),
            cells: first_size,
        });
    }
    let last = before + 2 == tiled.len();
    if let Some(pane) = reachable(second, split).filter(|_| last) {
        return Ok(Step {
            pane: pane.into(),
            cells: second_size,
        });
    }
    Err(format!(
        "no pane's resize moves the border between {terminal} and {origin}: \
         each has a split of the same kind of its own"
    ))
}

/// The indices from `cell` down to pane `id`.
fn path(cell: &Cell, id: &str) -> Option<Vec<usize>> {
    match &cell.kind {
        Kind::Pane(p) => (p == id).then(Vec::new),
        Kind::Split(_, cells) => cells.iter().enumerate().find_map(|(i, c)| {
            let mut rest = path(c, id)?;
            rest.insert(0, i);
            Some(rest)
        }),
    }
}

/// A tiled pane of `cell` with no split of kind `split` between it and
/// `cell`'s parent, so its `resize-pane` resizes `cell`.
fn reachable(cell: &Cell, split: Split) -> Option<&str> {
    match &cell.kind {
        Kind::Pane(id) => Some(id),
        Kind::Split(kind, _) if *kind == split => None,
        Kind::Split(_, cells) => cells
            .iter()
            .filter(|c| !c.floating)
            .find_map(|c| reachable(c, split)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::window_layout::tests::{pane, split};
    use super::*;
    use Split::{SideBySide as H, Stacked as V};

    fn step(pane: &str, cells: u32) -> Result<Step, String> {
        Ok(Step {
            pane: pane.into(),
            cells,
        })
    }

    /// `((%0 | %3) / %2) | %1`, 100x40, from tmux 3.8.
    fn pa_q_c() -> Cell {
        parse(include_str!("fixtures/3.8-layout-pa-q-c.txt")).unwrap()
    }

    /// `((P | A) / (X | Y)) | C`, 100x40, from tmux 3.8: P %4, A %7, X %6,
    /// Y %8, C %5.
    fn pa_xy_c() -> Cell {
        parse(include_str!("fixtures/3.8-layout-pa-xy-c.txt")).unwrap()
    }

    #[test]
    fn next_to_each_other_the_pane_before_the_border_takes_the_rest() {
        // A, right of P: P gets the 49 cells they share less A's 30
        assert_eq!(plan(&pa_q_c(), "%3", "%0", H, 30), step("%0", 19));
        // P, left of A: P gets its own 30
        assert_eq!(plan(&pa_q_c(), "%0", "%3", H, 30), step("%0", 30));
        // Q, below P | A: through P, which no stacked split holds apart
        assert_eq!(plan(&pa_q_c(), "%2", "%0", V, 10), step("%0", 29));
        // C, right of the column: through Q, the column's only pane with
        // no side by side split of its own
        assert_eq!(plan(&pa_q_c(), "%1", "%0", H, 20), step("%2", 79));
    }

    #[test]
    fn where_every_pane_before_has_its_own_split_it_resizes_through_the_last() {
        // C, right of the column: every pane of it has a side by side split
        assert_eq!(plan(&pa_xy_c(), "%5", "%4", H, 30), step("%5", 30));
        // X, left of C: the column gets 60 of the 99 cells, C the rest
        assert_eq!(plan(&pa_xy_c(), "%6", "%5", H, 60), step("%5", 39));
    }

    #[test]
    fn a_shape_no_pane_can_resize_fails() {
        // the column, C and D: the column | C border, C not the last
        let column = match pa_xy_c().kind {
            Kind::Split(_, cells) => cells[0].clone(),
            _ => unreachable!(),
        };
        let root = split(
            H,
            [0, 0, 100, 40],
            vec![
                column,
                pane("%5", [51, 0, 24, 40]),
                pane("%9", [76, 0, 24, 40]),
            ],
        );
        let err = plan(&root, "%5", "%4", H, 30).unwrap_err();
        assert!(err.contains("no pane's resize"), "{err}");
        // C | D still moves, through C
        assert_eq!(plan(&root, "%9", "%5", H, 20), step("%5", 28));
    }

    #[test]
    fn apart_it_moves_the_border_toward_the_origin() {
        // K0 | K1 | K2: K2's left border, through K1
        let root = split(
            H,
            [0, 0, 80, 24],
            vec![
                pane("%0", [0, 0, 26, 24]),
                pane("%1", [27, 0, 26, 24]),
                pane("%2", [54, 0, 26, 24]),
            ],
        );
        assert_eq!(plan(&root, "%2", "%0", H, 20), step("%1", 32));
        // K0's right border, through K0
        assert_eq!(plan(&root, "%0", "%2", H, 20), step("%0", 20));
    }

    #[test]
    fn panes_not_split_that_way_or_not_both_there_fail() {
        assert!(plan(&pa_q_c(), "%3", "%0", V, 10).is_err());
        assert!(plan(&pa_q_c(), "%3", "%9", H, 10).is_err());
        assert!(plan(&pa_q_c(), "%3", "%3", H, 10).is_err());
    }

    #[test]
    fn a_size_past_the_room_is_held_inside_it() {
        assert_eq!(plan(&pa_q_c(), "%3", "%0", H, 500), step("%0", 1));
    }
}
