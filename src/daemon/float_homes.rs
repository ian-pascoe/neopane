//! Float groups with no float group pane (ADR 0010): each is drawn in the
//! window pane that was current when one of its floats last became current,
//! and stays there when another pane becomes current. A group that met no
//! `open_float` to ask goes without a float group pane for life, as one
//! whose `open_float` failed does.

use super::panes::Panes;
use crate::screen::float_groups::{self, Group};
use crate::screen::grids::Grids;

/// How many floats that became current are remembered, with their pane.
const MAX_REMEMBERED: usize = 32;

#[derive(Debug, Default)]
pub(crate) struct FloatHomes {
    /// Floats that became current, each with the window pane current then,
    /// newest last; the oldest drop off.
    became_current: Vec<(i64, u32)>,
    /// The groups, by key, drawn in a pane for life for want of an
    /// `open_float` to ask.
    pane_for_life: Vec<i64>,
}

impl FloatHomes {
    /// Float `win` became current while pane `pane` was the current pane.
    pub(crate) fn became_current(&mut self, win: i64, pane: u32) {
        self.became_current.retain(|&(w, _)| w != win);
        if self.became_current.len() == MAX_REMEMBERED {
            self.became_current.remove(0);
        }
        self.became_current.push((win, pane));
    }

    /// Group `key` met no `open_float`: it's drawn in a pane for life.
    pub(crate) fn keep_in_pane(&mut self, key: i64) {
        if !self.pane_for_life.contains(&key) {
            self.pane_for_life.push(key);
        }
    }

    /// Whether group `key` met no `open_float`.
    pub(crate) fn in_pane_for_life(&self, key: i64) -> bool {
        self.pane_for_life.contains(&key)
    }

    /// Forgets the groups whose window named by their key closed.
    pub(crate) fn forget_closed(&mut self, grids: &Grids) {
        self.pane_for_life.retain(|&k| grids.grid_of(k).is_some());
    }

    /// The pane that was current when one of group `g`'s floats last became
    /// current, if one did.
    fn last_current_in(&self, grids: &Grids, g: &Group) -> Option<u32> {
        self.became_current
            .iter()
            .rev()
            .find(|&&(w, _)| float_groups::has_win(grids, g, w))
            .map(|&(_, p)| p)
    }

    /// The entered groups (`entered` as for `float_groups::find`) drawn in
    /// window panes, each with its pane: those with no float group pane,
    /// ready or on its way. One whose pane is gone, or whose floats were
    /// never current or are forgotten, takes `current`, the current pane, as
    /// its pane from now on.
    pub(crate) fn in_panes(
        &mut self,
        grids: &Grids,
        panes: &Panes,
        entered: &[i64],
        current: Option<u32>,
    ) -> Vec<(u32, Group)> {
        let mut out = Vec::new();
        for g in float_groups::find(grids, entered) {
            if panes.by_layer_key(g.key).is_some_and(|p| p.layer_hides()) {
                continue;
            }
            let home = self.last_current_in(grids, &g).filter(|&id| {
                panes
                    .by_id(id)
                    .is_some_and(|p| p.layer.is_none() && p.terminal_surface.is_some())
            });
            let home = match (home, current) {
                (Some(h), _) => h,
                (None, Some(c)) => {
                    self.became_current(g.key, c);
                    c
                }
                (None, None) => continue,
            };
            out.push((home, g));
        }
        out
    }
}
