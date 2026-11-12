//! Self-attached panes getting their windows (#105): a new window on the
//! file the hello names, else an unshown window, else a new window, as
//! `:split` opens one. Each becomes current, with its home tab page.
//!
//! One pane is served at a time, in the order they attached, so each gets
//! its window, and is made current, in that order.

use super::*;
use crate::daemon::unshown::pick_unshown_window;

/// What a self-attached pane asked of Neovim for its window, unanswered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowAsk {
    /// The daemon's id for the pane.
    pub(super) pane: u32,
    /// Neovim's request.
    pub(super) req: u32,
    pub(super) kind: AskKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AskKind {
    /// The windows it could take.
    List,
    /// Unshown window `win`, which its pane holds already.
    Show(i64),
    /// A new window: it says which as it makes it (`Note::Opened`).
    Open,
}

impl Daemon {
    /// Serves the first self-attached pane still without a window, unless
    /// one is being served: a new window on its file, or else the windows
    /// it could take, listed. Not before setup, which gives the first its
    /// window.
    pub(super) fn give_windows(&mut self) -> Result<()> {
        if !self.nvim.ready || self.window_ask.is_some() {
            return Ok(());
        }
        let Some(p) = self.panes.self_attached_awaiting_window() else {
            return Ok(());
        };
        let pane = p.id;
        let (req, kind) = match p.opening.clone() {
            Some(f) => {
                let [cols, rows] = self.window_size_of(pane);
                let req = windows::open(&mut self.nvim.client, Some((&f.file, f.line)), cols, rows);
                (req, AskKind::Open)
            }
            None => (windows::list_windows(&mut self.nvim.client), AskKind::List),
        };
        self.window_ask = Some(WindowAsk { pane, req, kind });
        self.nvim.client.flush()?;
        Ok(())
    }

    /// The size pane `id`'s window is asked to be.
    fn window_size_of(&self, id: u32) -> [u32; 2] {
        self.geometry().window_size(self.panes.by_id(id).unwrap())
    }

    /// Neovim made window `win` for the pane being served a new one: the
    /// pane holds it from now on, before any flush shows it.
    pub(super) fn on_window_opened(&mut self, win: i64) -> Result<()> {
        let Some(WindowAsk {
            pane,
            kind: AskKind::Open,
            ..
        }) = self.window_ask
        else {
            return Ok(());
        };
        match self.panes.by_id_mut(pane) {
            Some(p) => {
                p.neovim_window_handle = Some(win);
                p.opening = None;
                self.changes.mark_taken(win);
            }
            // gone while it was asked: nothing will show it, and no other
            // pane takes it as it closes
            None => {
                self.changes.mark_closing(win);
                windows::close(&mut self.nvim.client, win);
                self.nvim.client.flush()?;
            }
        }
        Ok(())
    }

    /// Neovim answered request `id`: true if it was the pane being served's.
    pub(super) fn on_window_answer(
        &mut self,
        id: u32,
        err: Option<&[u8]>,
        mut result: Decoder<'_>,
    ) -> Result<bool> {
        let Some(ask) = self.window_ask.filter(|a| a.req == id) else {
            return Ok(false);
        };
        self.window_ask = None;
        if let Some(e) = err {
            let e = String::from_utf8_lossy(e);
            log_warn!("{NAME} daemon: pane {} got no window: {e}", ask.pane);
            if let AskKind::Show(win) = ask.kind {
                self.untake(ask.pane, win);
            }
            // its attach ends rather than keep the rest waiting
            self.end_unserved(ask.pane)?;
        } else {
            match ask.kind {
                AskKind::List => {
                    let (current, wins) = windows::parse_windows(&mut result)?;
                    self.take_unshown(ask.pane, current, &wins);
                }
                AskKind::Show(win) => {
                    if rpc::expect_int(&mut result)? == 0 {
                        // gone before it was shown: it takes another
                        self.untake(ask.pane, win);
                    } else {
                        self.window_taken(ask.pane)?;
                    }
                }
                AskKind::Open => {
                    self.window_taken(ask.pane)?;
                    self.close_kept();
                }
            }
        }
        self.give_windows()?;
        self.nvim.client.flush()?;
        Ok(true)
    }

    /// Pane `id` couldn't be given its window: its attach ends, as its pane
    /// closes, and any window it took, a new one opened before the failure,
    /// closes with it. The last pane gone, the daemon goes unattached, as
    /// `pane_gone` does.
    fn end_unserved(&mut self, id: u32) -> Result<()> {
        let Some(i) = self.panes.index_of(id) else {
            return Ok(());
        };
        let others = self.panes_count_but(Some(id));
        let wins = self.panes.list[i].windows();
        self.panes.list[i].closing = Some(Close::WindowClosed);
        self.panes.remove(i, &self.event_loop);
        for win in wins {
            self.changes.untake(win);
            self.changes.mark_closing(win);
            windows::close(&mut self.nvim.client, win);
        }
        if !others && self.lifetime.is_attached() {
            return self.go_unattached(None);
        }
        Ok(())
    }

    /// Pane `id` gave window `win` back, unshown: it gets another.
    fn untake(&mut self, id: u32, win: i64) {
        self.changes.untake(win);
        if let Some(p) = self.panes.by_id_mut(id) {
            p.neovim_window_handle = None;
        }
    }

    /// Pane `id` has its window: it's sized to it, and placed.
    fn window_taken(&mut self, id: u32) -> Result<()> {
        let Some(win) = self.panes.by_id(id).and_then(|p| p.neovim_window_handle) else {
            return Ok(());
        };
        self.unshown_awaiting_origin.retain(|&(w, _)| w != win);
        self.wait_for_report(id, Instant::now())?;
        self.size_window(id);
        Ok(())
    }

    /// Neovim's current window is `current` and `wins` the windows a pane
    /// could show: pane `id` takes an unshown one, which it holds from now
    /// on, or a new one if none is.
    pub(super) fn take_unshown(&mut self, id: u32, current: i64, wins: &[i64]) {
        if self.panes.by_id(id).is_none() {
            return;
        }
        // the window an unattached daemon kept comes first
        let first = self.kept.filter(|w| wins.contains(w)).unwrap_or(current);
        let picked = pick_unshown_window(first, wins, |w| self.held(w));
        if picked.is_some() && picked == self.kept {
            self.kept = None;
        }
        let [cols, rows] = self.window_size_of(id);
        let (req, kind) = match picked {
            Some(win) => {
                self.panes.by_id_mut(id).unwrap().neovim_window_handle = Some(win);
                self.changes.mark_taken(win);
                let req = windows::show(&mut self.nvim.client, win, cols, rows);
                (req, AskKind::Show(win))
            }
            None => {
                let req = windows::open(&mut self.nvim.client, None, cols, rows);
                (req, AskKind::Open)
            }
        };
        self.window_ask = Some(WindowAsk {
            pane: id,
            req,
            kind,
        });
    }

    /// Whether window `w` is shown, or on its way to a pane, so no
    /// self-attach takes it: a pane holds it, it's closing or reopening, its
    /// requested pane waits for something first (its tab page's floating
    /// pane, an origin), or it waits for a pane that can draw it.
    fn held(&self, w: i64) -> bool {
        self.panes.by_win(w).is_some()
            || self.changes.is_closing(w)
            || self.changes.is_reopening(w)
            || self.unshown_awaiting_origin.iter().any(|&(a, _)| a == w)
            || (self.provider.is_some() && self.changes.splits.contains_key(&w))
    }
}
