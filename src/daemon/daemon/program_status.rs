//! Program status (ADR 0021): each Neovim pane's terminal holds the status
//! records of the terminal buffers whose windows it holds or draws, as
//! `program_status` makes them, written after each batch of events.

use std::collections::HashMap;

use super::*;

impl Daemon {
    /// Writes each pane's terminal the reports that bring the program
    /// status it holds to what it is to hold, if that changed. A terminal
    /// that had frames dropped waits until it has caught up, and is then
    /// written in full. A terminal that can't take them is gone, which its
    /// hang-up tells.
    pub(super) fn relay_status(&mut self) {
        let shown = self.status_shown();
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
            let desired = self.program_status.desired(bufs);
            let watched = match s.write_status(&desired) {
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

    /// The status buffers with records each pane shows the status of, by
    /// pane id: those with a window it holds, in the tab page it shows or
    /// another; a float's, in its float group pane, or else in the window
    /// pane its group is drawn in.
    fn status_shown(&mut self) -> HashMap<u32, Vec<i64>> {
        let mut out: HashMap<u32, Vec<i64>> = HashMap::new();
        let mut add = |id: u32, buf: i64| {
            let bufs = out.entry(id).or_default();
            if !bufs.contains(&buf) {
                bufs.push(buf);
            }
        };
        let current = self.current_pane_id();
        let mut homed: Option<Vec<(u32, float_groups::Group)>> = None;
        let wins: Vec<(i64, i64)> = self.program_status.shown().collect();
        for (buf, win) in wins {
            let mut held = false;
            for p in self.panes.list.iter().filter(|p| p.holds(win)) {
                add(p.id, buf);
                held = true;
            }
            if held {
                continue;
            }
            if let Some(p) = floating_panes::layer_of_win(&self.grids, &self.panes, win) {
                add(p.id, buf);
                continue;
            }
            let homed = homed.get_or_insert_with(|| {
                self.float_homes.in_panes(
                    &self.grids,
                    &self.panes,
                    self.nvim.entered_floats(),
                    current,
                )
            });
            for (home, g) in homed.iter() {
                if float_groups::has_win(&self.grids, g, win) {
                    add(*home, buf);
                }
            }
        }
        out
    }
}
