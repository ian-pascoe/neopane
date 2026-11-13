//! Float groups with no float group pane (ADR 0010): each is drawn in the
//! window pane that was current when one of its floats last became current,
//! and stays there when another pane becomes current. A group that once went
//! without a float group pane, past the cap or with no `open_float` to ask,
//! goes without one for life, as one whose `open_float` failed does.

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
    /// The groups, by key, that are drawn in a pane for life without having
    /// asked for a float group pane.
    pub in_pane: Vec<i64>,
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

    /// The pane that was current when one of group `g`'s floats last became
    /// current, if one did.
    fn last_current_in(&self, grids: &Grids, g: &Group) -> Option<u32> {
        self.became_current
            .iter()
            .rev()
            .find(|&&(w, _)| float_groups::has_win(grids, g, w))
            .map(|&(_, p)| p)
    }
}

/// The entered groups (`entered` as for `float_groups::find`) drawn in
/// window panes, each with its pane: those with no float group pane, ready
/// or on its way. One whose pane is gone, or that was never current in one,
/// is drawn in `current`, the current pane.
pub(crate) fn in_panes(
    grids: &Grids,
    panes: &Panes,
    homes: &FloatHomes,
    entered: &[i64],
    current: Option<u32>,
) -> Vec<(u32, Group)> {
    let mut out = Vec::new();
    for g in float_groups::find(grids, entered) {
        if panes.by_layer_key(g.key).is_some_and(|p| p.layer_hides()) {
            continue;
        }
        let home = homes
            .last_current_in(grids, &g)
            .filter(|&id| {
                panes
                    .by_id(id)
                    .is_some_and(|p| p.layer.is_none() && p.terminal_surface.is_some())
            })
            .or(current);
        if let Some(h) = home {
            out.push((h, g));
        }
    }
    out
}
