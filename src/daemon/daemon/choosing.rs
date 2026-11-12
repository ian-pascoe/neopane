//! Choosing the pane provider at a self-attach, and reach (ADRs 0006 and
//! 0007, #92 decisions 24–28).
//!
//! A self-attach chooses when the daemon has no pane provider: the
//! built-in default, from its hello (`providers::choose`). The chosen pane
//! provider is held at once, unconfirmed, and its `find` for the choosing
//! pane confirms it: in reach keeps it; anything else, or the choice's 3 s
//! running out, drops it with a warning. Nothing waits for that but what
//! needs the pane provider: the windows Neovim has with no pane of their own
//! wait unshown, then open as new windows do, and self-attaches that arrive
//! meanwhile wait their turn, until one gets a pane provider. A later
//! self-attach is in reach if its context variables are the choosing
//! attach's, and its `find` agrees; a window opened from a pane out of reach
//! is drawn in that pane.

use super::*;
use crate::attach::{PaneId, env_value};
use crate::daemon::pane::{Reach, SelfHello};
use crate::daemon::provider::Ask;
use crate::providers::choose::{self, Chosen};

/// The choice running: the choosing pane, the pane id its `find` carries,
/// and when the choice runs out of time.
pub(crate) struct Choosing {
    pub(super) id: u32,
    attach: PaneId,
    due: Instant,
}

impl Daemon {
    /// Self-attached pane `id` arrived: it chooses the pane provider if
    /// there is none, else its reach is checked. While a choice runs, it
    /// waits its turn.
    pub(super) fn choose_or_reach(&mut self, id: u32) -> Result<()> {
        let Some(hello) = self.panes.by_id(id).and_then(|p| p.hello.as_deref()) else {
            return Ok(());
        };
        if self.choosing.is_some() {
            log_info!("{NAME} daemon: pane {id} waits for the choice running");
            self.panes.by_id_mut(id).unwrap().reach = Reach::Finding;
            self.waiting_choice.push(id);
            return Ok(());
        }
        if self.provider.is_some() {
            return self.check_reach(id);
        }
        #[cfg(test)]
        if let Some(choose) = &mut self.test_choose {
            let chosen = choose(hello);
            return self.take_choice(id, chosen);
        }
        let chosen = choose::choose(&hello.env, hello.hint.as_deref(), &mut choose::connect());
        self.take_choice(id, chosen)
    }

    /// What self-attached pane `id` chose: a pane provider, which its
    /// `find` confirms within the choice's time, or none. Its windows wait
    /// for that.
    pub(super) fn take_choice(&mut self, id: u32, chosen: Chosen) -> Result<()> {
        let (held, spent) = match chosen {
            Chosen::Provider { held, spent } => (held, spent),
            Chosen::None { why, warn } => {
                if warn {
                    let line = format!("{why}; no pane provider");
                    log_warn!("{NAME} daemon: {line}");
                    self.warn_when_ready(format!("{NAME}: {line}"));
                } else {
                    log_info!("{NAME} daemon: no pane provider for pane {id}: {why}");
                }
                if let Some(p) = self.panes.by_id_mut(id) {
                    p.reach = Reach::Out;
                }
                return Ok(());
            }
        };
        let Some(p) = self.panes.by_id(id) else {
            return Ok(());
        };
        let hello = p.hello.as_deref().expect("a choosing pane has a hello");
        let attach = p.attach_id.expect("a self-attached pane has an id");
        log_info!("{NAME} daemon: pane {id} chose {}", held.name());
        self.context = held
            .context_vars()
            .iter()
            .map(|v| env_value(&hello.env, v).map(<[u8]>::to_vec))
            .collect();
        if let Some(fd) = held.fd() {
            self.event_loop.add_read(fd, Token::Provider as usize)?;
        }
        self.provider = Some(held);
        // the panes attached before are out of reach, their windows theirs,
        // until the choice is confirmed: then they rejoin as `find` says
        for q in self.panes.list.iter_mut() {
            // those waiting their turn are checked as it comes
            if q.id != id && !self.waiting_choice.contains(&q.id) {
                q.reach = Reach::Out;
            }
        }
        // the whole choice has the time: connecting, before, counts
        let due = Instant::now() + crate::config::TIME.saturating_sub(spent);
        self.choosing = Some(Choosing { id, attach, due });
        self.arm_choice_timer(due)?;
        self.find(id)
    }

    /// Starts the choice timer for `due`.
    fn arm_choice_timer(&self, due: Instant) -> Result<()> {
        let ms = due
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        self.add_timer(Token::ChoiceTimer, u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// The choice timer fired: a choice still unconfirmed at `now` is
    /// refused.
    pub(super) fn time_out_choice(&mut self, now: Instant) -> Result<()> {
        let Some(c) = &self.choosing else {
            return Ok(());
        };
        if c.due > now {
            return self.arm_choice_timer(c.due);
        }
        if let Some(p) = self.panes.by_id_mut(c.id) {
            p.reach = Reach::Out;
        }
        // stalled anywhere: connecting, `session.attach` or `find`
        let secs = crate::util::seconds(crate::config::TIME);
        let what = format!("no answer within {secs} s");
        self.refuse_choice(&what)
    }

    /// Self-attached pane `id` came with a pane provider there: out of reach
    /// if its context variables aren't the choosing attach's, else as its
    /// `find` says.
    fn check_reach(&mut self, id: u32) -> Result<()> {
        if !self.shares_context(id) {
            return self.reach_decided();
        }
        self.find(id)
    }

    /// Whether pane `id`'s hello has the choosing attach's values of the
    /// pane provider's context variables; if not, it is out of reach.
    fn shares_context(&mut self, id: u32) -> bool {
        let vars = self.provider.as_deref().unwrap().context_vars();
        let p = self.panes.by_id(id).unwrap();
        let same = p.hello.as_deref().is_some_and(|hello| {
            vars.iter()
                .zip(&self.context)
                .all(|(v, chose)| env_value(&hello.env, v) == chose.as_deref())
        });
        if !same {
            log_info!(
                "{NAME} daemon: pane {id} is out of reach: its {} differ",
                vars.join(", ")
            );
            self.panes.by_id_mut(id).unwrap().reach = Reach::Out;
        }
        same
    }

    /// Asks the pane provider to `find` pane `id`'s terminal; without
    /// `find`, it is in reach.
    fn find(&mut self, id: u32) -> Result<()> {
        let p = self.provider.as_deref();
        let finds = self.asks.may_ask(p, Capability::Find);
        let find_vars: Vec<String> = p
            .map(|p| p.find_vars().iter().map(|v| v.to_string()).collect())
            .unwrap_or_default();
        let Some(q) = self.panes.by_id_mut(id) else {
            return Ok(());
        };
        let attach_id = q.attach_id.expect("a self-attached pane has an id");
        if !finds {
            q.reach = Reach::In;
            self.confirm(&attach_id)?;
            return self.reach_decided();
        }
        q.reach = Reach::Finding;
        let hello: &SelfHello = q.hello.as_deref().expect("a pane that finds has a hello");
        let env = hello
            .env
            .iter()
            .filter(|(k, _)| find_vars.iter().any(|v| v.as_bytes() == k.as_slice()))
            .cloned()
            .collect();
        let ask = Ask::Find {
            pane: attach_id,
            pid: hello.pid,
            tty: String::from_utf8_lossy(&hello.tty).into_owned(),
            device: hello.device,
            title: pane::title(&attach_id),
            env,
        };
        // L7 is timed from here
        log::trace(Point::Accept, u64::from(id));
        self.act(ask);
        Ok(())
    }

    /// The pane provider's name, for the log and warnings.
    pub(super) fn provider_name(&self) -> String {
        self.provider
            .as_ref()
            .map(|p| p.name().to_string())
            .unwrap_or_default()
    }

    /// Whether the pane whose pane id is `attach` is choosing.
    fn is_choosing(&self, attach: &PaneId) -> bool {
        self.choosing.as_ref().is_some_and(|c| c.attach == *attach)
    }

    /// The pane with pane id `attach` is in reach: if it chose, the choice
    /// is confirmed, and the panes attached before it rejoin.
    fn confirm(&mut self, attach: &PaneId) -> Result<()> {
        if !self.is_choosing(attach) {
            return Ok(());
        }
        let c = self.choosing.take().unwrap();
        log_info!(
            "{NAME} daemon: pane {} confirmed {}",
            c.id,
            self.provider_name()
        );
        self.rejoin(c.id)
    }

    /// A newly chosen pane provider, confirmed by pane `chose`, checks the
    /// reach of every pane attached before it (ADR 0007): a pane in reach
    /// rejoins, as Rex's do when Rex is chosen again. Those waiting their
    /// turn are checked as it comes.
    fn rejoin(&mut self, chose: u32) -> Result<()> {
        let before: Vec<u32> = self
            .panes
            .list
            .iter()
            .filter(|p| {
                p.id != chose
                    && p.reach == Reach::Out
                    && p.closing.is_none()
                    && p.hello.is_some()
                    && !self.waiting_choice.contains(&p.id)
            })
            .map(|p| p.id)
            .collect();
        for id in before {
            if self.provider.is_some() && self.shares_context(id) {
                self.find(id)?;
            }
        }
        Ok(())
    }

    /// The choice running is refused, for `why`: the pane provider is
    /// dropped, with a warning.
    fn refuse_choice(&mut self, why: &str) -> Result<()> {
        self.choosing = None;
        self.provider_gone(why)?;
        self.reach_decided()
    }

    /// The pane provider found the terminal of the pane `find` asked for,
    /// `answer` its id if it gave one: it is in reach. Windows waiting for
    /// that open.
    pub(super) fn found(&mut self, ask: &Ask, answer: Answer) -> Result<()> {
        let Ask::Find { pane, .. } = ask else {
            return Ok(());
        };
        if let Some(p) = self.panes.by_attach_id_mut(pane) {
            p.reach = Reach::In;
            if let Answer::Terminal(t) = answer {
                p.terminal = Some(t);
            }
            log::trace(Point::Found, u64::from(p.id));
        }
        self.confirm(pane)?;
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
        if let Some(p) = self.panes.by_attach_id_mut(pane) {
            p.reach = Reach::Out;
            log_info!("{NAME} daemon: pane {} is out of reach: {why}", p.id);
        }
        if self.is_choosing(pane) {
            return self.refuse_choice(&format!("find didn't confirm this terminal: {why}"));
        }
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
        if let Some(p) = self.panes.by_attach_id_mut(pane) {
            p.reach = if kept { Reach::In } else { Reach::Out };
        }
        if kept {
            self.confirm(pane)?;
        }
        self.reach_decided()
    }

    /// A pane's reach, or the choice, was decided: once no choice runs, the
    /// self-attaches that waited for it take their turns, in order; the
    /// windows that waited get panes, or are drawn in theirs.
    fn reach_decided(&mut self) -> Result<()> {
        while self.choosing.is_none() && !self.waiting_choice.is_empty() {
            let id = self.waiting_choice.remove(0);
            if self.panes.by_id(id).is_some() {
                self.choose_or_reach(id)?;
            }
        }
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

    /// Shows `message` in Neovim as a warning once it is set up and has
    /// drawn: before, it would wait at a hit-enter prompt with the start's
    /// own messages, hiding the first window until a key.
    pub(super) fn warn_when_ready(&mut self, message: String) {
        if self.nvim.ready {
            self.warn_in_nvim(&message);
        } else {
            self.held_warnings.push(message);
        }
    }

    /// Shows the warnings held until Neovim was set up and drew.
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
