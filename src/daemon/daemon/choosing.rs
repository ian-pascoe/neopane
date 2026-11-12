//! Choosing the pane provider at a self-attach, and reach (ADRs 0006 and
//! 0007, #92 decisions 24–28).
//!
//! A self-attach chooses when the daemon has no pane provider: the
//! built-in default, from its hello (`providers::choose`). The chosen pane
//! provider is held at once, unconfirmed, and its `find` for the choosing
//! pane confirms it: in reach keeps it, anything else drops it with a
//! warning. Nothing waits for that but what needs the pane provider: the
//! windows Neovim has with no pane of their own wait unshown, then open as
//! new windows do. A later self-attach is in reach if its context variables
//! are the choosing attach's, and its `find` agrees; a window opened from a
//! pane out of reach is drawn in that pane.

use super::*;
use crate::daemon::pane::{AttachInfo, Reach};
use crate::daemon::provider::Ask;
use crate::providers::choose::{self, Chosen};

impl Daemon {
    /// Self-attached pane `id` arrived: it chooses the pane provider if
    /// there is none, else its reach is checked.
    pub(super) fn choose_or_reach(&mut self, id: u32) -> Result<()> {
        let Some(info) = self.panes.by_id(id).and_then(|p| p.info.as_deref()) else {
            return Ok(());
        };
        if self.provider.is_some() {
            return self.check_reach(id);
        }
        let chosen = choose::choose(&info.env, info.hint.as_deref(), &mut self.connect);
        self.take_choice(id, chosen)
    }

    /// What self-attached pane `id` chose: a pane provider, which its
    /// `find` confirms, or none. Its windows wait for that.
    pub(super) fn take_choice(&mut self, id: u32, chosen: Chosen) -> Result<()> {
        let held = match chosen {
            Chosen::Provider(held) => held,
            Chosen::None { why, warn: false } => {
                log_info!("{NAME} daemon: no pane provider for pane {id}: {why}");
                return Ok(());
            }
            Chosen::None { why, warn: true } => {
                let line = format!("{why}; no pane provider");
                log_warn!("{NAME} daemon: {line}");
                self.warn_when_ready(format!("{NAME}: {line}"));
                return Ok(());
            }
        };
        let Some(info) = self.panes.by_id(id).and_then(|p| p.info.as_deref()) else {
            return Ok(());
        };
        let p = held.seam_ref();
        log_info!("{NAME} daemon: pane {id} chose {}", p.name());
        self.context = p
            .context_vars()
            .iter()
            .map(|v| info.var(v).map(<[u8]>::to_vec))
            .collect();
        if let Some(fd) = held.fd() {
            self.event_loop.add_read(fd, Token::Provider as usize)?;
        }
        self.provider = Some(held);
        // panes that came before have no verdict: their windows stay theirs
        for q in self.panes.list.iter_mut() {
            if q.is_self_attached() && q.id != id {
                q.reach = Reach::Out;
            }
        }
        self.choosing = Some(id);
        self.find(id);
        Ok(())
    }

    /// Self-attached pane `id` came with a pane provider there: out of reach
    /// if its context variables aren't the choosing attach's, else as its
    /// `find` says.
    fn check_reach(&mut self, id: u32) -> Result<()> {
        let p = self.provider.as_ref().map(Held::seam_ref).unwrap();
        let vars = p.context_vars();
        let info = self.panes.by_id(id).unwrap().info.as_deref().unwrap();
        let same = vars
            .iter()
            .zip(&self.context)
            .all(|(v, chose)| info.var(v) == chose.as_deref());
        if !same {
            log_info!(
                "{NAME} daemon: pane {id} is out of reach: its {} differ",
                vars.join(", ")
            );
            self.panes.by_id_mut(id).unwrap().reach = Reach::Out;
            return Ok(());
        }
        self.find(id);
        Ok(())
    }

    /// Asks the pane provider to `find` pane `id`'s terminal; without
    /// `find`, it is in reach.
    fn find(&mut self, id: u32) {
        let p = self.provider.as_ref().map(Held::seam_ref);
        let finds = self.asks.may_ask(p, Capability::Find);
        let find_vars: Vec<String> = p
            .map(|p| p.find_vars().iter().map(|v| v.to_string()).collect())
            .unwrap_or_default();
        let Some(q) = self.panes.by_id_mut(id) else {
            return;
        };
        if !finds {
            q.reach = Reach::In;
            self.confirm(id);
            return;
        }
        q.reach = Reach::Finding;
        let attach_id = q.attach_id.expect("a self-attached pane has an id");
        let info: &AttachInfo = q.info.as_deref().expect("a pane that finds has a hello");
        let env = info
            .env
            .iter()
            .filter(|(k, _)| find_vars.iter().any(|v| v.as_bytes() == k.as_slice()))
            .cloned()
            .collect();
        let ask = Ask::Find {
            pane: attach_id,
            pid: info.pid,
            tty: String::from_utf8_lossy(&info.tty).into_owned(),
            device: info.device,
            title: pane::title(&attach_id),
            env,
        };
        // L7 is timed from here
        log::trace(Point::Accept, u64::from(id));
        self.act(ask);
    }

    /// Pane `id` is in reach: if it chose, the choice is confirmed.
    fn confirm(&mut self, id: u32) {
        if self.choosing == Some(id) {
            self.choosing = None;
            let name = self
                .provider
                .as_ref()
                .map(|p| p.seam_ref().name().to_string());
            log_info!(
                "{NAME} daemon: pane {id} confirmed {}",
                name.unwrap_or_default()
            );
        }
    }

    /// The pane provider found the terminal of the pane `find` asked for,
    /// `answer` its id if it gave one: it is in reach. Windows waiting for
    /// that open.
    pub(super) fn found(&mut self, ask: &Ask, answer: Answer) -> Result<()> {
        let Ask::Find { pane, .. } = ask else {
            return Ok(());
        };
        let Some(p) = self.panes.by_attach_id_mut(pane) else {
            return Ok(());
        };
        let id = p.id;
        p.reach = Reach::In;
        if let Answer::Terminal(t) = answer {
            p.terminal = Some(t);
        }
        log::trace(Point::Found, u64::from(id));
        self.confirm(id);
        // a live layout that came first places it now
        if let Some(l) = self.live_layout.clone()
            && self.apply_live_layout(&l)
        {
            self.refit_to_areas()?;
        }
        self.reach_decided()
    }

    /// `find` didn't find the terminal of the pane it was asked for: it is
    /// out of reach, for `why`. Refusing the choosing attach drops the pane
    /// provider, with a warning.
    pub(super) fn not_found(&mut self, ask: &Ask, why: &str) -> Result<()> {
        let Ask::Find { pane, .. } = ask else {
            return Ok(());
        };
        let Some(p) = self.panes.by_attach_id_mut(pane) else {
            return Ok(());
        };
        let id = p.id;
        p.reach = Reach::Out;
        if self.choosing != Some(id) {
            log_info!("{NAME} daemon: pane {id} is out of reach: {why}");
            return self.reach_decided();
        }
        self.choosing = None;
        let name = self
            .provider
            .as_ref()
            .map(|p| p.seam_ref().name().to_string());
        let line = format!(
            "{}: find didn't confirm this terminal: {why}; no pane provider",
            name.unwrap_or_default()
        );
        self.warn_when_ready(format!("{NAME}: {line}"));
        self.provider_gone(&line)?;
        self.reach_decided()
    }

    /// A `find` fell back, unasked or unanswered: in reach without an id if
    /// the pane provider found it unsupported, else, with the pane provider
    /// gone, out of reach.
    pub(super) fn find_fell_back(&mut self, ask: &Ask) -> Result<()> {
        let Ask::Find { pane, .. } = ask else {
            return Ok(());
        };
        let kept = self.provider.is_some() && self.asks.unsupported(Capability::Find);
        let Some(p) = self.panes.by_attach_id_mut(pane) else {
            return Ok(());
        };
        let id = p.id;
        p.reach = if kept { Reach::In } else { Reach::Out };
        if kept {
            self.confirm(id);
        }
        self.reach_decided()
    }

    /// A pane's reach, or the choice, was decided: the windows that waited
    /// for it get panes, or are drawn in theirs.
    fn reach_decided(&mut self) -> Result<()> {
        self.reconcile()?;
        self.draw()
    }

    /// Whether a window split off as `sp` waits for a choice to be confirmed
    /// (#92 decision 28), or for its origin pane's `find`.
    pub(super) fn waits_for_reach(&self, sp: windows::Split) -> bool {
        self.choosing.is_some()
            || self
                .split_from(sp)
                .is_some_and(|f| f.reach == Reach::Finding)
    }

    /// Shows `message` in Neovim as a warning once it is set up: before, at
    /// a hit-enter prompt, it would hide the first window until a key.
    pub(super) fn warn_when_ready(&mut self, message: String) {
        if self.nvim.ready {
            self.warn_in_nvim(&message);
        } else {
            self.held_warnings.push(message);
        }
    }

    /// Shows the warnings held until Neovim was set up.
    pub(super) fn warn_held(&mut self) {
        for w in std::mem::take(&mut self.held_warnings) {
            self.warn_in_nvim(&w);
        }
    }

    /// Keeps the loop watching the pane provider's fd for room to write
    /// while it has bytes waiting for it.
    pub(super) fn watch_provider_writes(&mut self) -> Result<()> {
        let (fd, wants) = match &self.provider {
            Some(p) => (p.fd(), p.wants_write()),
            None => (None, false),
        };
        let Some(fd) = fd else {
            self.provider_writes = false;
            return Ok(());
        };
        if wants != self.provider_writes {
            if wants {
                self.event_loop.add_write(fd, Token::Provider as usize)?;
            } else {
                self.event_loop.remove_write(fd)?;
            }
            self.provider_writes = wants;
        }
        Ok(())
    }
}
