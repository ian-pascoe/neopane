//! A daemon's life after its last pane (ADR 0003): it ends, unless Neovim
//! has unsaved changes. Then it stays, unattached, keeping one window for
//! the next attach, until an attach comes, the changes are gone, or a
//! signal ends it.
//!
//! "The last pane goes" when no pane shows a window and none is on its way:
//! a float group pane doesn't count, nor a requested pane whose request
//! failed. Going unattached, the panes left go, so a terminal still on its
//! way for one is refused, and opens not yet asked of the pane provider are
//! dropped; Neovim is sent FocusLost, so autosave can run, and asked to tell
//! its unsaved changes at its next SafeState. The daemon decides from that
//! note, or from the last one if none comes within `UNATTACH_WAIT_MS`, never
//! from a request, which would wait behind a hit-enter prompt.

use super::*;

/// How long going unattached waits for Neovim's next SafeState.
const UNATTACH_WAIT_MS: u32 = 1000;

/// Where a daemon is in its life after its panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// A pane shows a window, or one is on its way.
    Attached,
    /// The last pane went: Neovim's answer `ask` about its unsaved changes
    /// is awaited, until the unattach timer fires.
    Deciding { ask: u32 },
    /// Unattached, staying for unsaved changes, listed in the `unsaved`
    /// file.
    StayingForUnsaved,
}

/// A daemon's life after its panes, and what it goes by.
#[derive(Debug)]
pub(crate) struct Lifetime {
    pub(crate) stage: Stage,
    /// The unsaved changes, as Neovim last told.
    unsaved: Vec<Vec<u8>>,
    /// The last answer about them asked of Neovim.
    ask: u32,
    /// Where the unsaved changes are listed while staying for them: None
    /// without a daemon directory, as in tests.
    pub(crate) unsaved_file: Option<crate::daemon::dir::Unsaved>,
    /// The window an unattached daemon keeps, unshown, for the next attach.
    pub(super) kept: Option<Kept>,
}

/// A window kept for the next attach, and the tab page its pane held it in,
/// as split notes name it: the next pane to take it shows that tab page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Kept {
    pub(crate) win: i64,
    pub(crate) tab: i64,
}

impl Default for Lifetime {
    fn default() -> Lifetime {
        Lifetime {
            stage: Stage::Attached,
            unsaved: Vec::new(),
            ask: 0,
            unsaved_file: None,
            kept: None,
        }
    }
}

impl Lifetime {
    pub(crate) fn is_attached(&self) -> bool {
        self.stage == Stage::Attached
    }

    /// The unsaved changes, as Neovim last told.
    pub(crate) fn unsaved(&self) -> &[Vec<u8>] {
        &self.unsaved
    }
}

impl Daemon {
    /// Whether a pane other than `but` shows a window or has one on its way.
    pub(super) fn panes_count_but(&self, but: Option<u32>) -> bool {
        self.panes.list.iter().any(|p| {
            Some(p.id) != but && p.layer.is_none() && p.closing != Some(Close::RequestFailed)
        })
    }

    /// The last pane went: pane `last`, if it's still there, its windows
    /// closing but the one kept, the last current one it showed. A window
    /// kept before, as the daemon went unattached earlier, stays kept if
    /// none is. Every pane left goes, opens not yet asked are dropped, and
    /// Neovim is asked about its unsaved changes.
    pub(super) fn go_unattached(&mut self, last: Option<u32>) -> Result<()> {
        let before = self.lifetime.kept.take().filter(|k| {
            self.grids.grid_of(k.win).is_some()
                && !self.changes.is_closing(k.win)
                && self.panes.by_win(k.win).is_none()
        });
        let mut kept = None;
        if let Some(i) = last.and_then(|id| self.panes.index_of(id)) {
            let p = &self.panes.list[i];
            let wins = p.windows();
            let current = self.nvim.current_win;
            let win = if wins.contains(&current) {
                Some(current)
            } else {
                p.neovim_window_handle.filter(|w| wins.contains(w))
            };
            kept = win.map(|win| Kept {
                win,
                tab: p.tab_of(win).unwrap_or(0),
            });
            self.panes.remove(i, &self.event_loop);
            for &win in wins.iter().filter(|&&w| Some(w) != kept.map(|k| k.win)) {
                self.changes.mark_closing(win);
                windows::close(&mut self.nvim.client, win);
            }
        }
        // one window kept: an older one goes
        match (kept, before) {
            (Some(_), Some(old)) => {
                self.changes.mark_closing(old.win);
                windows::close(&mut self.nvim.client, old.win);
            }
            (None, old) => kept = old,
            _ => {}
        }
        self.lifetime.kept = kept;
        // float group panes, and requests that failed: their ids expire
        while let Some(p) = self.panes.list.last() {
            let close = self.close_id(p);
            let p = self.panes.list.last_mut().unwrap();
            let failed = p.closing == Some(Close::RequestFailed);
            if p.closing.is_none() {
                p.closing = Some(Close::Shutdown);
            }
            if !p.is_self_attached()
                && !failed
                && let Some(t) = close
            {
                self.act(asks::close(t));
            }
            let i = self.panes.list.len() - 1;
            self.panes.remove(i, &self.event_loop);
        }
        let dropped = self.asks.drop_waiting_opens();
        if dropped > 0 {
            log_info!("{NAME} daemon: dropped {dropped} open(s) not yet asked");
        }
        // FocusLost, so autosave can run first
        self.nvim.ui_focused = false;
        let _ = self.nvim.client.request("nvim_ui_set_focus", 1);
        msgpack::write_bool(self.nvim.client.writer(), false);
        self.lifetime.ask += 1;
        let ask = self.lifetime.ask;
        windows::unsaved_at_safe_state(&mut self.nvim.client, ask);
        self.lifetime.stage = Stage::Deciding { ask };
        self.add_timer(Token::UnattachTimer, UNATTACH_WAIT_MS)?;
        log_info!("{NAME} daemon: the last pane went; asking Neovim about unsaved changes");
        Ok(self.nvim.client.flush()?)
    }

    /// Neovim told its unsaved changes, `files`, as answer `ask`, or 0
    /// unasked.
    pub(super) fn on_unsaved(&mut self, files: Vec<Vec<u8>>, ask: u32) -> Result<()> {
        self.lifetime.unsaved = files;
        match self.lifetime.stage {
            Stage::Deciding { ask: asked } if ask == asked => self.decide(),
            Stage::StayingForUnsaved if self.lifetime.unsaved.is_empty() => {
                log_info!("{NAME} daemon: no unsaved changes are left");
                self.stop(StopReason::PaneGone);
            }
            Stage::StayingForUnsaved => self.write_unsaved(),
            _ => {}
        }
        Ok(())
    }

    /// Neovim's next SafeState didn't come in time: what it last said
    /// decides.
    pub(super) fn unattach_wait_over(&mut self) -> Result<()> {
        if matches!(self.lifetime.stage, Stage::Deciding { .. }) {
            log_info!("{NAME} daemon: Neovim didn't answer in time; going by what it last said");
            self.decide();
        }
        Ok(())
    }

    /// Ends, with nothing unsaved; else stays unattached, listing them.
    fn decide(&mut self) {
        if self.lifetime.unsaved.is_empty() {
            log_info!("{NAME} daemon: nothing unsaved");
            self.stop(StopReason::PaneGone);
            return;
        }
        self.lifetime.stage = Stage::StayingForUnsaved;
        log_info!(
            "{NAME} daemon: staying unattached for unsaved changes in {}",
            names(&self.lifetime.unsaved)
        );
        self.write_unsaved();
    }

    /// Lists the unsaved changes in the `unsaved` file.
    fn write_unsaved(&self) {
        if let Some(f) = &self.lifetime.unsaved_file
            && let Err(e) = f.write(&self.lifetime.unsaved)
        {
            log_warn!("{NAME} daemon: couldn't write the unsaved file: {e}");
        }
    }

    /// A pane attached: an unattached daemon is attached again, and stays.
    pub(super) fn attached_again(&mut self) {
        if self.lifetime.is_attached() {
            return;
        }
        self.lifetime.stage = Stage::Attached;
        if let Some(f) = &self.lifetime.unsaved_file {
            f.remove();
        }
        log_info!("{NAME} daemon: attached again");
    }

    /// Forgets the kept window once it's gone.
    pub(super) fn forget_kept_if_gone(&mut self) {
        if self.lifetime.kept.is_some_and(|k| {
            self.grids.grid_of(k.win).is_none() && !self.changes.awaits_drawing(k.win)
        }) {
            self.lifetime.kept = None;
        }
    }

    /// Pane `id` shows window `win`: kept, it's kept no longer.
    pub(super) fn kept_taken(&mut self, win: i64) {
        if self.lifetime.kept.is_some_and(|k| k.win == win) {
            self.lifetime.kept = None;
        }
    }

    /// A pane shows a file it opened: the kept window, unless a pane took
    /// it, closes.
    pub(super) fn close_kept(&mut self) {
        let Some(Kept { win, .. }) = self.lifetime.kept.take() else {
            return;
        };
        if self.panes.by_win(win).is_some() {
            return;
        }
        self.changes.mark_closing(win);
        windows::close(&mut self.nvim.client, win);
    }
}

/// `files`, for the log: an unnamed buffer as `[No Name]`, as Neovim says.
fn names(files: &[Vec<u8>]) -> String {
    files
        .iter()
        .map(|f| match f.is_empty() {
            true => "[No Name]".into(),
            false => String::from_utf8_lossy(f),
        })
        .collect::<Vec<_>>()
        .join(", ")
}
