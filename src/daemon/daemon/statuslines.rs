//! The statuslines neopane draws windows that Neovim draws none for, at
//! `'laststatus'` 0 (ADR 0009): their text and highlights, as Neovim would
//! draw them, asked of Neovim after each redraw and whenever windows.lua says
//! they may have changed without one.

use super::*;

/// An ask that runs one at a time: one in flight, and one more due if it was
/// wanted meanwhile.
#[derive(Debug, Default)]
pub(crate) struct OneAsk {
    in_flight: Option<u32>,
    again: bool,
}

impl OneAsk {
    /// Whether an ask may go now. If one is in flight, one more is due once
    /// it's answered.
    fn may_ask(&mut self) -> bool {
        if self.in_flight.is_some() {
            self.again = true;
            return false;
        }
        true
    }

    fn asked(&mut self, id: u32) {
        self.in_flight = Some(id);
    }

    /// Request `id` was answered: whether one more is due, or None if `id`
    /// wasn't this ask.
    fn answered(&mut self, id: u32) -> Option<bool> {
        if self.in_flight != Some(id) {
            return None;
        }
        self.in_flight = None;
        Some(std::mem::take(&mut self.again))
    }

    /// The ask in flight.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> Option<u32> {
        self.in_flight
    }
}

impl Daemon {
    /// The windows of every pane's layout that neopane draws a statusline for.
    fn substitute_wins(&self) -> Vec<i64> {
        let mut wins = Vec::new();
        for p in &self.panes.list {
            let (laid, _) = self.laid_out_wins(p);
            wins.extend(laid.iter().filter(|(_, ours)| *ours).map(|(w, _)| w.win));
        }
        wins
    }

    /// Forgets the statuslines of the windows that no longer have one of
    /// ours: one they get again is blank until Neovim evaluates it.
    pub(super) fn forget_unneeded_statuslines(&mut self) {
        if self.grids.statuslines.is_empty() {
            return;
        }
        let grids: Vec<u32> = self
            .substitute_wins()
            .iter()
            .filter_map(|&w| self.grids.grid_of(w))
            .collect();
        self.grids.keep_statuslines(&grids);
    }

    /// Asks Neovim for the statuslines it would draw the windows neopane draws
    /// statuslines for, if any: after each redraw, and when windows.lua says
    /// they may have changed without one. One ask at a time.
    pub(super) fn ask_statuslines(&mut self) -> Result<()> {
        if self.split_options.statuslines || !self.statuslines_ask.may_ask() {
            return Ok(());
        }
        let wins = self.substitute_wins();
        if wins.is_empty() {
            return Ok(());
        }
        let id = windows::statuslines(&mut self.nvim.client, &wins);
        self.statuslines_ask.asked(id);
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Whether `id` answered `ask_statuslines`. The panes showing a
    /// statusline that changed draw it.
    pub(super) fn on_statuslines_answer(
        &mut self,
        id: u32,
        err: Option<&[u8]>,
        mut result: Decoder<'_>,
    ) -> Result<bool> {
        let Some(again) = self.statuslines_ask.answered(id) else {
            return Ok(false);
        };
        if let Some(e) = err {
            log_warn!(
                "neovim error evaluating statuslines: {}",
                String::from_utf8_lossy(e)
            );
        } else {
            let answer = windows::parse_statuslines(&mut result)?;
            let lines = answer
                .lines
                .into_iter()
                .filter_map(|(win, cells)| Some((self.grids.grid_of(win)?, cells)))
                .collect();
            if self.grids.set_statuslines(answer.attrs, lines) {
                self.draw()?;
            }
        }
        if again {
            self.ask_statuslines()?;
        }
        Ok(true)
    }
}
