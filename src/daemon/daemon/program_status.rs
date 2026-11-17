//! Program status (ADR 0021): each Neovim pane's terminal holds the status
//! records of the terminal buffers whose windows it holds or draws, as
//! `program_status` makes them, written after each batch of events.

use std::collections::HashMap;

use super::*;
use crate::nvim::windows::BufWin;

impl Daemon {
    /// Writes each pane's terminal the reports that bring the program
    /// status it holds to what it is to hold, if that changed. A terminal
    /// that had frames dropped waits until it has caught up, and is then
    /// written in full. A terminal that can't take them is gone, which its
    /// hang-up tells.
    pub(super) fn relay_program_status(&mut self) {
        let shown = self.program_status_shown();
        for p in self.panes.list.iter_mut() {
            if p.closing.is_some() {
                continue;
            }
            let id = p.id;
            let token = p.token(PaneFd::Terminal);
            let Some(s) = p.terminal_surface.as_deref_mut() else {
                continue;
            };
            if s.backlog.dropped() {
                continue;
            }
            let bufs = shown.get(&id).map(Vec::as_slice).unwrap_or_default();
            let wanted = self.program_status.wanted(bufs);
            let watched = match s.write_program_status(&wanted) {
                Ok(Some(Sent::Waiting)) => self
                    .event_loop
                    .add_write(s.output_fd(), token)
                    .map_err(|e| e.to_string()),
                Ok(_) => Ok(()),
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = watched {
                log_info!("{NAME} daemon: pane {id}: can't write its program status: {e}");
            }
        }
    }

    /// The terminal buffers with status records each pane shows the
    /// status of, by pane id: those with a window it holds, in the tab page
    /// it shows or another; a float's, in its float group pane, and in the
    /// panes its last frame drew it in, entered or not.
    fn program_status_shown(&self) -> HashMap<u32, Vec<i64>> {
        let mut out: HashMap<u32, Vec<i64>> = HashMap::new();
        let mut add = |id: u32, buf: i64| {
            let bufs = out.entry(id).or_default();
            if !bufs.contains(&buf) {
                bufs.push(buf);
            }
        };
        for BufWin { buf, win } in self.program_status.shown() {
            for p in self.panes.list.iter().filter(|p| p.holds(win)) {
                add(p.id, buf);
            }
            let Some(grid) = self.grids.grid_of(win) else {
                continue;
            };
            if self.grids.wins.get(&grid).map(|w| w.kind) != Some(WinKind::Float) {
                continue;
            }
            if let Some(p) = floating_panes::layer_of_win(&self.grids, &self.panes, win) {
                add(p.id, buf);
            }
            for p in &self.panes.list {
                let drew = p
                    .terminal_surface
                    .as_ref()
                    .is_some_and(|s| s.view.drawn_floats().iter().any(|r| r.grid == grid));
                if drew {
                    add(p.id, buf);
                }
            }
        }
        out
    }
}
