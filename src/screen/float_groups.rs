//! Floats to show in a Rex floating pane. A group is an editor
//! float with every float hanging from it, as a picker builds its box,
//! input, list and preview; focusable groups that touch, as Telescope's
//! do, are one. It goes native once something in it has taken focus:
//! being focusable isn't enough, since completion menus are and are never
//! entered. Those stay drawn in their pane, beside what they complete.

use super::grids::{Grids, MAX_FLOATS, Placed, WinKind, keep_top_floats};

pub const MAX_GROUPS: usize = 4;
const MAX_TOPS: usize = MAX_FLOATS;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Group {
    /// The editor floats in it, by grid.
    pub tops: [u32; MAX_TOPS],
    pub tops_len: u8,
    /// Names the group while it's open: its lowest window handle.
    pub key: i64,
    /// Its highest float's (zindex, compindex): where the group stacks.
    pub z: (u32, u32),
    /// Where it sits on grid 1.
    pub row: i32,
    pub col: i32,
    pub width: u32,
    pub height: u32,
    pub focusable: bool,
    /// A float in it has been Neovim's current window.
    pub entered: bool,
}

impl Group {
    pub fn top_grids(&self) -> &[u32] {
        &self.tops[0..self.tops_len as usize]
    }

    pub fn has_top(&self, top: u32) -> bool {
        self.top_grids().contains(&top)
    }

    fn bottom(&self) -> i64 {
        self.row as i64 + self.height as i64
    }

    fn right(&self) -> i64 {
        self.col as i64 + self.width as i64
    }

    /// Overlapping or a cell apart.
    fn touches(&self, b: &Group) -> bool {
        self.row as i64 <= b.bottom() + 1
            && b.row as i64 <= self.bottom() + 1
            && self.col as i64 <= b.right() + 1
            && b.col as i64 <= self.right() + 1
    }

    fn merge(&mut self, b: Group) {
        self.unite(b.row, b.col, b.width, b.height);
        self.focusable = self.focusable || b.focusable;
        self.entered = self.entered || b.entered;
        self.key = self.key.min(b.key);
        self.z = self.z.max(b.z);
        for &t in b.top_grids() {
            if (self.tops_len as usize) < MAX_TOPS {
                self.tops[self.tops_len as usize] = t;
                self.tops_len += 1;
            }
        }
    }

    /// Grows to cover the rect too.
    fn unite(&mut self, row: i32, col: i32, width: u32, height: u32) {
        if self.width == 0 {
            *self = Group {
                tops: self.tops,
                tops_len: self.tops_len,
                key: self.key,
                z: self.z,
                focusable: self.focusable,
                entered: self.entered,
                row,
                col,
                width,
                height,
            };
            return;
        }
        let top = self.row.min(row);
        let left = self.col.min(col);
        self.height = (self.bottom().max(row as i64 + height as i64) - top as i64) as u32;
        self.width = (self.right().max(col as i64 + width as i64) - left as i64) as u32;
        self.row = top;
        self.col = left;
    }
}

/// The groups to show natively: those with a float in `entered`, the
/// float windows that have been current. At most `MAX_GROUPS`: past that the
/// top-most groups (by their highest float) stay. Lowest first, ties in grid order.
pub fn find(grids: &Grids, entered: &[i64]) -> Vec<Group> {
    let mut all: Vec<Group> = Vec::new();
    // each editor float, with the floats hanging from it
    for (&id, w) in &grids.wins {
        if w.kind != WinKind::Float || w.win < 0 {
            continue;
        }
        let Some(p) = grids.place(id) else { continue };
        if p.top != id {
            continue;
        }
        let mut g = Group {
            key: w.win,
            z: (w.float.zindex, w.float.compindex),
            ..Group::default()
        };
        g.tops[0] = id;
        g.tops_len = 1;
        for (&mid, m) in &grids.wins {
            if m.kind != WinKind::Float {
                continue;
            }
            let Some(mp) = grids.place(mid) else { continue };
            if mp.top != id {
                continue;
            }
            let Some(grid) = grids.get(mid) else { continue };
            if grid.width == 0 || grid.height == 0 {
                continue;
            }
            g.unite(mp.row, mp.col, grid.width, grid.height);
            g.focusable = g.focusable || m.float.focusable;
            g.entered = g.entered || entered.contains(&m.win);
        }
        if g.width == 0 {
            continue;
        }
        all.push(g);
    }
    // lowest first, so a merge keeps the lower group's tops first
    all.sort_by_key(|g| g.z);
    // focusable groups that touch become one
    let mut merged = true;
    while merged {
        merged = false;
        let n = all.len();
        'outer: for i in 0..n {
            for j in i + 1..n {
                if !all[i].focusable || !all[j].focusable || !all[i].touches(&all[j]) {
                    continue;
                }
                let b = all[j];
                all[i].merge(b);
                all.remove(j);
                merged = true;
                break 'outer;
            }
        }
    }
    all.retain(|g| g.entered);
    all.sort_by_key(|g| g.z);
    let extra = all.len().saturating_sub(MAX_GROUPS);
    all.drain(..extra);
    all
}

/// The floats in group `g`, lowest first. At most `MAX_FLOATS`, the top-most.
pub fn members(grids: &Grids, g: &Group) -> Vec<Placed> {
    let mut out: Vec<Placed> = Vec::new();
    for (&id, w) in &grids.wins {
        if w.kind != WinKind::Float {
            continue;
        }
        let Some(p) = grids.place(id) else { continue };
        if p.top == 0 || !g.has_top(p.top) {
            continue;
        }
        out.push(Placed {
            grid: id,
            row: p.row,
            col: p.col,
            zindex: w.float.zindex,
            compindex: w.float.compindex,
            on_screen: true,
            top: p.top,
            ..Placed::default()
        });
    }
    keep_top_floats(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::grid::Grid;
    use crate::screen::grids::{Float, Win};

    fn add_float(gs: &mut Grids, grid: u32, win: i64, w: u32, h: u32, f: Float) {
        gs.grids.insert(grid, Grid::default());
        gs.grids.get_mut(&grid).unwrap().resize(w, h);
        gs.wins.insert(
            grid,
            Win {
                win,
                kind: WinKind::Float,
                float: f,
            },
        );
    }

    #[test]
    fn a_pickers_floats_are_one_group_ones_never_entered_arent() {
        let mut gs = Grids::default();
        // a picker as snacks makes it: an unfocusable box, input and list on it
        add_float(
            &mut gs,
            3,
            1002,
            60,
            20,
            Float {
                focusable: false,
                screen: Some([4, 10]),
                ..Float::default()
            },
        );
        add_float(
            &mut gs,
            4,
            1004,
            58,
            1,
            Float {
                anchor_grid: 3,
                row: 1.0,
                col: 1.0,
                ..Float::default()
            },
        );
        add_float(
            &mut gs,
            5,
            1005,
            58,
            16,
            Float {
                anchor_grid: 3,
                row: 3.0,
                col: 1.0,
                ..Float::default()
            },
        );
        // a notification: unfocusable
        add_float(
            &mut gs,
            6,
            1006,
            30,
            6,
            Float {
                focusable: false,
                screen: Some([0, 100]),
                ..Float::default()
            },
        );
        // a small focusable input: its own group
        add_float(
            &mut gs,
            7,
            1007,
            30,
            1,
            Float {
                screen: Some([40, 0]),
                ..Float::default()
            },
        );
        // the popup menu
        add_float(
            &mut gs,
            8,
            -1,
            30,
            10,
            Float {
                screen: Some([40, 40]),
                ..Float::default()
            },
        );
        // a focusable float nobody went into
        add_float(
            &mut gs,
            9,
            1009,
            20,
            4,
            Float {
                screen: Some([30, 100]),
                ..Float::default()
            },
        );

        let groups = find(&gs, &[1004, 1007]);
        assert_eq!(2, groups.len());
        assert_eq!(&[7][..], groups[1].top_grids());
        let g = groups[0];
        assert_eq!(&[3][..], g.top_grids());
        assert_eq!(1002, g.key);
        assert_eq!(4, g.row);
        assert_eq!(10, g.col);
        assert_eq!(60, g.width);
        assert_eq!(20, g.height);

        let ms = members(&gs, &g);
        assert_eq!(3, ms.len());
    }

    #[test]
    fn editor_floats_side_by_side_as_telescopes_are_one_group() {
        let mut gs = Grids::default();
        // prompt over results, preview to the right with a gap of a cell
        add_float(
            &mut gs,
            3,
            1010,
            40,
            3,
            Float {
                screen: Some([2, 2]),
                ..Float::default()
            },
        );
        add_float(
            &mut gs,
            4,
            1011,
            40,
            20,
            Float {
                screen: Some([5, 2]),
                ..Float::default()
            },
        );
        add_float(
            &mut gs,
            5,
            1012,
            40,
            23,
            Float {
                screen: Some([2, 43]),
                ..Float::default()
            },
        );
        // only the prompt takes focus
        let groups = find(&gs, &[1010]);
        assert_eq!(1, groups.len());
        assert_eq!(3, groups[0].tops_len);
        assert_eq!(1010, groups[0].key);
        assert_eq!(81, groups[0].width);
        assert_eq!(23, groups[0].height);
    }

    #[test]
    fn a_completion_menu_under_the_command_line_stays_in_the_pane() {
        let mut gs = Grids::default();
        // the command line popup, as noice makes it
        add_float(
            &mut gs,
            3,
            1020,
            62,
            3,
            Float {
                focusable: false,
                screen: Some([4, 40]),
                ..Float::default()
            },
        );
        // blink's menu right under it: focusable, never entered
        add_float(
            &mut gs,
            4,
            1021,
            30,
            8,
            Float {
                screen: Some([7, 41]),
                ..Float::default()
            },
        );
        // and its scrollbar, on the menu
        add_float(
            &mut gs,
            5,
            1022,
            1,
            3,
            Float {
                anchor_grid: 4,
                col: 29.0,
                ..Float::default()
            },
        );
        assert_eq!(0, find(&gs, &[]).len());
        // a float elsewhere was entered: still not these
        assert_eq!(0, find(&gs, &[1000]).len());
    }

    #[test]
    fn past_the_cap_the_top_most_floats_are_kept() {
        let mut gs = Grids::default();
        // 20 floats hanging from one editor float, the last made the top-most
        add_float(
            &mut gs,
            3,
            1000,
            10,
            10,
            Float {
                screen: Some([0, 0]),
                ..Float::default()
            },
        );
        for i in 0..19u32 {
            add_float(
                &mut gs,
                4 + i,
                1001 + i as i64,
                2,
                2,
                Float {
                    anchor_grid: 3,
                    zindex: 50 + i,
                    ..Float::default()
                },
            );
        }
        let g = find(&gs, &[1000])[0];
        let ms = members(&gs, &g);
        assert_eq!(MAX_FLOATS, ms.len());
        // lowest first, and the newest (top-most) is last
        assert_eq!(3 + 19, ms.last().unwrap().grid);
        assert_eq!(3 + 19 - (MAX_FLOATS as u32 - 1), ms[0].grid);
        assert!(ms.windows(2).all(|w| w[0].zindex <= w[1].zindex));
    }

    #[test]
    fn higher_floats_nobody_entered_dont_push_out_an_entered_group() {
        let mut gs = Grids::default();
        // the entered one, lowest
        add_float(
            &mut gs,
            3,
            1000,
            4,
            4,
            Float {
                screen: Some([0, 0]),
                zindex: 10,
                ..Float::default()
            },
        );
        for i in 0..20u32 {
            add_float(
                &mut gs,
                4 + i,
                1001 + i as i64,
                2,
                2,
                Float {
                    screen: Some([10, 10 * i as i32]),
                    zindex: 50 + i,
                    ..Float::default()
                },
            );
        }
        let groups = find(&gs, &[1000]);
        assert_eq!(1, groups.len());
        assert_eq!(&[3][..], groups[0].top_grids());
    }

    #[test]
    fn past_max_groups_the_top_most_entered_groups_stay_lowest_first() {
        let mut gs = Grids::default();
        for i in 0..20u32 {
            add_float(
                &mut gs,
                3 + i,
                1000 + i as i64,
                2,
                2,
                Float {
                    screen: Some([0, 10 * i as i32]),
                    zindex: 50 + i,
                    ..Float::default()
                },
            );
        }
        let entered: Vec<i64> = (1000..1020).collect();
        let groups = find(&gs, &entered);
        let first = 3 + 20 - MAX_GROUPS as u32;
        let grids: Vec<u32> = groups.iter().map(|g| g.tops[0]).collect();
        assert_eq!(
            (first..first + MAX_GROUPS as u32).collect::<Vec<_>>(),
            grids
        );
    }
}
