//! Self-attached panes getting their windows (#105): a new window on the
//! file the hello names, else an unshown window, else a new window, as
//! `:split` opens one. Each becomes current, with its home tab page.

use super::*;
use crate::daemon::pane::Taking;
use crate::daemon::unshown::pick_unshown_window;

impl Daemon {
    /// Asks Neovim for a window for each self-attached pane still without
    /// one, in the order they attached: a new one on a file asked for at
    /// once; for the rest, the windows they could take first. Not before
    /// setup, which gives the first its window.
    pub(super) fn give_windows(&mut self) -> Result<()> {
        if !self.nvim.ready {
            return Ok(());
        }
        let waiting: Vec<(u32, Option<Taking>)> = self
            .panes
            .self_attached_awaiting()
            .map(|p| (p.id, p.taking.clone()))
            .collect();
        for (id, taking) in waiting {
            match taking {
                Some(Taking::File(file, line)) => self.open_window(id, Some(&file), line),
                Some(Taking::Asked { .. }) => {}
                None if self.windows_req == 0 => {
                    self.windows_req = windows::list_windows(&mut self.nvim.client);
                }
                None => {}
            }
        }
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Asks Neovim for a new window for pane `id`, on `file` if given.
    fn open_window(&mut self, id: u32, file: Option<&[u8]>, line: Option<u32>) {
        let size = self.geometry().window_size(self.panes.by_id(id).unwrap());
        let req = windows::open(&mut self.nvim.client, file, line, size[0], size[1]);
        self.panes.by_id_mut(id).unwrap().taking = Some(Taking::Asked { req, win: None });
    }

    /// Neovim answered request `id` if it was one of ours for self-attached
    /// panes: true then.
    pub(super) fn on_taking_answer(
        &mut self,
        id: u32,
        err: Option<&[u8]>,
        mut result: Decoder<'_>,
    ) -> Result<bool> {
        if id == 0 {
            return Ok(false);
        }
        if id == self.windows_req {
            self.windows_req = 0;
            match err {
                Some(e) => log_warn!(
                    "{NAME} daemon: couldn't list the windows: {}",
                    String::from_utf8_lossy(e)
                ),
                None => {
                    let (current, wins) = windows::parse_windows(&mut result)?;
                    self.take_unshown(current, &wins)?;
                }
            }
            return Ok(true);
        }
        let Some(pane) = self
            .panes
            .list
            .iter()
            .find(|p| matches!(p.taking, Some(Taking::Asked { req, .. }) if req == id))
            .map(|p| p.id)
        else {
            return Ok(false);
        };
        let p = self.panes.by_id_mut(pane).unwrap();
        p.taking = None;
        let win = match err {
            Some(e) => {
                log_warn!(
                    "{NAME} daemon: pane {pane} got no window: {}",
                    String::from_utf8_lossy(e)
                );
                0
            }
            None => rpc::expect_int(&mut result)?,
        };
        if win == 0 {
            // gone before it was shown: it takes another
            return self.give_windows().map(|()| true);
        }
        p.neovim_window_handle = Some(win);
        self.changes.mark_taken(win);
        self.unshown_awaiting_origin.retain(|&(w, _)| w != win);
        self.wait_for_report(pane, Instant::now())?;
        self.size_window(pane);
        self.nvim.client.flush()?;
        Ok(true)
    }

    /// Neovim's current window is `current` and `wins` the windows a pane
    /// could show: each self-attached pane waiting for a window takes an
    /// unshown one, in the order they attached, or a new one if none is.
    fn take_unshown(&mut self, current: i64, wins: &[i64]) -> Result<()> {
        let waiting: Vec<u32> = self
            .panes
            .self_attached_awaiting()
            .filter(|p| p.taking.is_none())
            .map(|p| p.id)
            .collect();
        for id in waiting {
            let held = |w: i64| {
                self.panes.by_win(w).is_some()
                    || self.changes.is_closing(w)
                    || self.changes.is_reopening(w)
                    || self.panes.list.iter().any(
                        |p| matches!(p.taking, Some(Taking::Asked { win: Some(t), .. }) if t == w),
                    )
            };
            match pick_unshown_window(current, wins, held) {
                Some(win) => {
                    let size = self.geometry().window_size(self.panes.by_id(id).unwrap());
                    let req = windows::show(&mut self.nvim.client, win, size[0], size[1]);
                    self.panes.by_id_mut(id).unwrap().taking = Some(Taking::Asked {
                        req,
                        win: Some(win),
                    });
                }
                None => self.open_window(id, None, None),
            }
        }
        self.nvim.client.flush()?;
        Ok(())
    }
}
