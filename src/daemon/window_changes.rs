//! Neovim windows partway through a change: asked to become external,
//! closing, reopening, or just split. `Daemon.reconcile` asks it what to do.

use indexmap::{IndexMap, IndexSet};

use crate::nvim::windows;
use crate::screen::grids::Grids;

#[derive(Debug, Clone, Default)]
pub(crate) struct WindowChanges {
    /// Windows asked to become external and not seen external since.
    pub asked: IndexSet<i64>,
    /// Windows we asked Neovim to close because their pane went away.
    pub closing: IndexSet<i64>,
    /// Windows closing to reopen (`:close` on the last one, or `:tabclose`
    /// taking windows that only followed into its tab page): their panes wait
    /// for the new window instead of hanging up. Kept until it opens.
    pub reopening: IndexSet<i64>,
    /// New windows that reopened closing ones, not drawn yet. A slow command,
    /// such as an autocommand that reads the clipboard, can flush a redraw
    /// before the new window is drawn, so their panes wait for it.
    pub reopened: IndexSet<i64>,
    /// New windows without panes yet, and where Neovim put them.
    pub splits: IndexMap<i64, PendingSplit>,
}

/// A slow command, such as a plugin's first load, can flush a redraw
/// between making a window and drawing it, so a split is only forgotten
/// once its window was seen and is gone.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingSplit {
    pub split: windows::Split,
    pub seen: bool,
}

impl WindowChanges {
    /// Forgets windows Neovim no longer has.
    pub(crate) fn forget_gone(&mut self, grids: &Grids) {
        // drawn: an ordinary window from here on
        self.reopened.retain(|&win| grids.grid_of(win).is_none());
        for set in [&mut self.asked, &mut self.closing] {
            let mut j = set.len();
            while j > 0 {
                j -= 1;
                if grids.grid_of(*set.get_index(j).unwrap()).is_none() {
                    set.swap_remove_index(j);
                }
            }
        }
        let mut j = self.splits.len();
        while j > 0 {
            j -= 1;
            let (&win, pending) = self.splits.get_index_mut(j).unwrap();
            if grids.grid_of(win).is_some() {
                pending.seen = true;
            } else if pending.seen {
                self.splits.swap_remove_index(j);
            }
        }
    }

    /// True the first time for `win`: ask Neovim to make it external.
    pub(crate) fn should_adopt(&mut self, win: i64) -> bool {
        self.asked.insert(win)
    }

    /// `win` is external now, so it's no longer waiting on our request.
    pub(crate) fn became_external(&mut self, win: i64) {
        self.asked.swap_remove(&win);
    }

    pub(crate) fn mark_closing(&mut self, win: i64) {
        self.closing.insert(win);
    }

    pub(crate) fn is_closing(&self, win: i64) -> bool {
        self.closing.contains(&win)
    }

    pub(crate) fn mark_reopening(&mut self, win: i64) {
        self.reopening.insert(win);
    }

    pub(crate) fn is_reopening(&self, win: i64) -> bool {
        self.reopening.contains(&win)
    }

    /// True if `win` was closing to reopen; forgets it.
    pub(crate) fn take_reopening(&mut self, win: i64) -> bool {
        self.reopening.swap_remove(&win)
    }

    /// `win` reopened a closing window, and isn't drawn yet.
    pub(crate) fn mark_reopened(&mut self, win: i64) {
        self.reopened.insert(win);
    }

    /// True while `win`'s pane waits for it: closing to reopen, or the new
    /// window not drawn yet.
    pub(crate) fn awaits_reopen(&self, win: i64) -> bool {
        self.reopening.contains(&win) || self.reopened.contains(&win)
    }

    pub(crate) fn note_split(&mut self, sp: windows::Split) {
        self.splits.insert(
            sp.win,
            PendingSplit {
                split: sp,
                seen: false,
            },
        );
    }

    /// Where Neovim put `win`; beside the current window if it didn't say.
    pub(crate) fn split_of(&self, win: i64) -> windows::Split {
        if let Some(pending) = self.splits.get(&win) {
            return pending.split;
        }
        windows::Split {
            win,
            side_by_side: true,
            before: false,
            from: 0,
            tab: 0,
        }
    }

    pub(crate) fn forget_split(&mut self, win: i64) {
        self.splits.swap_remove(&win);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::grids::{Float, Win, WinKind};

    #[test]
    fn asks_to_adopt_a_window_once_until_its_external() {
        let mut changes = WindowChanges::default();
        assert!(changes.should_adopt(1000));
        assert!(!changes.should_adopt(1000));
        changes.became_external(1000);
        assert!(changes.should_adopt(1000));
    }

    #[test]
    fn a_reopening_window_is_taken_once() {
        let mut changes = WindowChanges::default();
        changes.mark_reopening(1000);
        assert!(changes.take_reopening(1000));
        assert!(!changes.take_reopening(1000));
    }

    #[test]
    fn split_defaults_to_beside_the_current_window() {
        let mut changes = WindowChanges::default();
        assert_eq!(0, changes.split_of(1000).from);
        changes.note_split(windows::Split {
            win: 1000,
            side_by_side: false,
            before: true,
            from: 999,
            tab: 0,
        });
        assert_eq!(999, changes.split_of(1000).from);
        changes.forget_split(1000);
        assert_eq!(0, changes.split_of(1000).from);
    }

    #[test]
    fn forgets_windows_neovim_no_longer_has() {
        let mut grids = Grids::default();
        grids.wins.insert(
            2,
            Win {
                win: 1000,
                kind: WinKind::External,
                float: Float::default(),
            },
        );
        let mut changes = WindowChanges::default();
        for win in [1000, 1001] {
            changes.should_adopt(win);
            changes.mark_closing(win);
            changes.mark_reopening(win);
            changes.note_split(windows::Split {
                win,
                side_by_side: true,
                before: false,
                from: 0,
                tab: 0,
            });
        }
        changes.forget_gone(&grids);
        assert!(changes.is_closing(1000));
        assert!(!changes.is_closing(1001));
        assert!(!changes.should_adopt(1000));
        // reopening windows wait for their replacement, gone or not
        assert!(changes.take_reopening(1000));
        assert!(changes.take_reopening(1001));
        // a split not drawn yet is kept until it's been seen
        assert_eq!(2, changes.splits.len());
        grids.wins.swap_remove(&2);
        changes.forget_gone(&grids);
        assert_eq!(1, changes.splits.len());
        assert!(changes.splits.contains_key(&1001));
    }
}
