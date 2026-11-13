//! Choosing the pane provider at a self-attach, and reach (ADRs 0006 and
//! 0007, #92 decisions 24–28).
//!
//! A self-attach chooses when the daemon has no pane provider: the user's
//! config, run in the config child while the loop goes on, or with no
//! file the built-in default, from its hello (`providers::choose`). A
//! config error means none for that choice, with one warning. The chosen
//! pane provider is held at once, unconfirmed, and its `find` for the
//! choosing attach's pane confirms it: in reach keeps it; anything else, or
//! the choice's time running out (3 s, or `find`'s timeout if longer),
//! drops it with a warning. Nothing waits for that but what needs the pane
//! provider: the windows Neovim has with no pane of their own wait
//! unshown, then open as new windows do, and self-attaches that arrive
//! meanwhile wait their turn, until one gets a pane provider. A later
//! self-attach is in reach if its context variables are the choosing
//! attach's, and its `find` agrees; a window opened from a pane out of reach
//! is drawn in that pane. A terminal the pane provider opened is in its
//! reach whatever `find` says, which runs for it only for its id: in a
//! pair, or when its open printed none (#93 decision 17).
//!
//! Before the first `find` for an attach, a pane provider whose `find`
//! matches the pane title, unlike Rex's, has the daemon write it into the
//! terminal, and a new `u` tells the helper how to put the old one back.

use std::path::Path;

use super::control::NoProvider;
use super::*;
use crate::attach::{PaneId, env_value};
use crate::daemon::pane::{AttachHello, Reach};
use crate::daemon::provider::Ask;
use crate::providers::choose::{self, ByDefault, ChooseBy, Chosen, ConfigRun, DefaultRun, Env};

/// The choice running: the choosing attach's pane, the pane id its `find`
/// carries, when the choice runs out of time, and the time it had.
pub(crate) struct Choosing {
    pub(super) id: u32,
    attach: PaneId,
    due: Instant,
    time: Duration,
    /// The built-in default's guess, with no config: refusing it only logs.
    quiet: bool,
}

impl Daemon {
    /// Self-attached pane `id` arrived: it chooses the pane provider if
    /// there is none, else its reach is checked. While a choice runs, it
    /// waits its turn.
    pub(super) fn choose_or_reach(&mut self, id: u32) -> Result<()> {
        if self
            .panes
            .by_id(id)
            .and_then(|p| p.hello.as_deref())
            .is_none()
        {
            return Ok(());
        }
        if self.a_choice_runs() {
            log_info!("{NAME} daemon: pane {id} waits for the choice running");
            self.panes.by_id_mut(id).unwrap().reach = Reach::Finding;
            self.waiting_choice.push(id);
            return Ok(());
        }
        // a pane provider with no terminal left in its reach, only asks
        // pending, is on its way out: this attach chooses anew
        if self.provider.is_some() && !self.reaches(Some(id)) {
            log_info!(
                "{NAME} daemon: dropping {} for pane {id}'s choice: no terminal in its reach",
                self.provider_name()
            );
            self.drop_provider()?;
            self.run_steps()?;
        }
        if self.provider.is_some() {
            // only a choosing attach's hint matters (#94 decision 37)
            let hint = self
                .panes
                .by_id(id)
                .and_then(|p| p.hello.as_deref())
                .and_then(|h| h.hint.as_deref());
            if let Some(hint) = hint {
                log_info!(
                    "{NAME} daemon: hint {hint} ignored: {} keeps {}",
                    self.name,
                    self.provider_name()
                );
            }
            return self.check_reach(id);
        }
        let hello = self
            .panes
            .by_id(id)
            .and_then(|p| p.hello.as_deref())
            .unwrap();
        #[cfg(test)]
        if let Some(choose) = &mut self.test_choose {
            let chosen = choose(hello);
            return self.take_choice(id, chosen);
        }
        match choose::choose_by(self.config_file.as_ref(), hello.hint.as_deref()) {
            ChooseBy::Config(file) => {
                let file = file.to_path_buf();
                self.run_config(id, &file)
            }
            ChooseBy::Missing(file) => {
                let line = choose::missing_line(file);
                self.config_failed(id, line)
            }
            ChooseBy::Default => {
                let attach = hello.for_config(&self.name);
                let by = choose::choose(
                    &attach,
                    hello.hint.as_deref(),
                    &self.exe,
                    &mut choose::connect(),
                    &self.prove,
                );
                match by {
                    ByDefault::Now(chosen) => self.take_default(id, chosen),
                    ByDefault::Proving(run) => self.wait_for_default(id, run),
                }
            }
        }
    }

    /// Self-attached pane `id`'s config failed before it could run, as
    /// `line` says: no pane provider, with that one warning.
    fn config_failed(&mut self, id: u32, line: String) -> Result<()> {
        log_warn!("{NAME} daemon: {line}");
        self.warn_when_ready(format!("{NAME}: {line}"));
        self.take_choice(id, Chosen::ConfigError)
    }

    /// Whether a choice runs: the config, or a chosen pane provider's
    /// `find` that hasn't confirmed it yet.
    pub(super) fn a_choice_runs(&self) -> bool {
        self.choosing.is_some() || self.config_run.is_some() || self.default_run.is_some()
    }

    /// Self-attached pane `id` chooses with config `file`, which runs in
    /// the config child while the loop goes on: what needs the pane
    /// provider waits for its answer.
    fn run_config(&mut self, id: u32, file: &Path) -> Result<()> {
        let hello = self.panes.by_id(id).and_then(|p| p.hello.as_deref());
        let attach = hello.unwrap().for_config(&self.name);
        match ConfigRun::start(&self.exe, file, attach) {
            Ok(run) => self.wait_for_config(id, run),
            Err(e) => {
                let line = format!("config: can't run {}: {e}", file.display());
                self.config_failed(id, line)
            }
        }
    }

    /// Self-attached pane `id` chooses with the config `run`, started:
    /// its answer is watched for on the loop.
    pub(super) fn wait_for_config(&mut self, id: u32, run: ConfigRun) -> Result<()> {
        log_info!("{NAME} daemon: pane {id} runs {}", run.file.display());
        self.event_loop.add_read(run.fd(), Token::Config as usize)?;
        if let Some(p) = self.panes.by_id_mut(id) {
            p.reach = Reach::Finding;
        }
        self.config_run = Some((id, run));
        Ok(())
    }

    /// The config's run for a choice is done: what it chose is taken, as
    /// the in-process default's is.
    pub(super) fn on_config(&mut self) -> Result<()> {
        let Some(outcome) = self.config_run.as_ref().and_then(|(_, run)| run.outcome()) else {
            return Ok(());
        };
        let (id, run) = self.config_run.take().unwrap();
        let _ = self.event_loop.remove(run.fd());
        let Some(hello) = self.panes.by_id(id).and_then(|p| p.hello.as_deref()) else {
            log_info!("{NAME} daemon: pane {id} went before its config answered");
            return self.reach_decided();
        };
        let (chosen, warning) = choose::by_config(
            &outcome,
            &run.file,
            hello,
            &self.exe,
            &mut choose::connect(),
        );
        if let Some(warning) = warning {
            self.warn_when_ready(warning);
        }
        // a pane provider's `find` decides, else this does
        let decided = !matches!(chosen, Chosen::Provider { .. });
        self.take_choice(id, chosen)?;
        if decided {
            self.reach_decided()?;
        }
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Self-attached pane `id` chooses with the built-in default, whose
    /// proofs `run` runs off the loop: what needs the pane provider waits
    /// for them, as for a config.
    pub(super) fn wait_for_default(&mut self, id: u32, run: DefaultRun) -> Result<()> {
        log_info!("{NAME} daemon: pane {id} proves which multiplexer it is in");
        self.event_loop
            .add_read(run.fd(), Token::Default as usize)?;
        if let Some(p) = self.panes.by_id_mut(id) {
            p.reach = Reach::Finding;
        }
        self.default_run = Some((id, run));
        Ok(())
    }

    /// The default's proofs for a choice are done: what it chose is taken.
    pub(super) fn on_default(&mut self) -> Result<()> {
        let Some(outcome) = self.default_run.as_ref().and_then(|(_, run)| run.outcome()) else {
            return Ok(());
        };
        let (id, run) = self.default_run.take().unwrap();
        let _ = self.event_loop.remove(run.fd());
        let Some(hello) = self.panes.by_id(id).and_then(|p| p.hello.as_deref()) else {
            log_info!("{NAME} daemon: pane {id} went before the default's proofs were done");
            return self.reach_decided();
        };
        #[cfg(test)]
        let chosen = match &mut self.test_of_default {
            Some(of) => of(&outcome.choice()),
            None => choose::by_proofs(
                &outcome,
                &hello.env,
                &hello.cwd,
                &self.exe,
                &mut choose::connect(),
            ),
        };
        #[cfg(not(test))]
        let chosen = choose::by_proofs(
            &outcome,
            &hello.env,
            &hello.cwd,
            &self.exe,
            &mut choose::connect(),
        );
        let decided = !matches!(chosen, Chosen::Provider { .. });
        self.take_default(id, chosen)?;
        if decided {
            self.reach_decided()?;
        }
        self.nvim.client.flush()?;
        Ok(())
    }

    /// What self-attached pane `id` chose: a pane provider, which its
    /// `find` confirms within the choice's time, or none. Its windows wait
    /// for that. A refusal warns.
    pub(super) fn take_choice(&mut self, id: u32, chosen: Chosen) -> Result<()> {
        self.take_chosen(id, chosen, false)
    }

    /// What the built-in default chose for self-attached pane `id`, with
    /// no config: as `take_choice`, but its `find` refusing only logs, the
    /// default having guessed from variables that may have leaked.
    pub(super) fn take_default(&mut self, id: u32, chosen: Chosen) -> Result<()> {
        self.take_chosen(id, chosen, true)
    }

    fn take_chosen(&mut self, id: u32, chosen: Chosen, quiet: bool) -> Result<()> {
        let (held, spent) = match chosen {
            Chosen::Provider { held, spent } => (held, spent),
            Chosen::ConfigError => {
                self.no_provider = NoProvider::ConfigError;
                log_info!("{NAME} daemon: no pane provider for pane {id}: the config failed");
                if let Some(p) = self.panes.by_id_mut(id) {
                    p.reach = Reach::Out;
                }
                return Ok(());
            }
            Chosen::None { why, warn } => {
                let hint = self
                    .panes
                    .by_id(id)
                    .and_then(|p| p.hello.as_deref())
                    .and_then(|h| h.hint.as_deref());
                self.no_provider = match hint {
                    Some(choose::HINT_NONE) => NoProvider::HintNone,
                    _ => NoProvider::None,
                };
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
        let hello = p.hello.as_deref().expect("a choosing attach has a hello");
        let attach = p.attach_id.expect("a self-attached pane has an id");
        log_info!("{NAME} daemon: pane {id} chose {}", held.name());
        let held_find_time = held.timeout(Capability::Find);
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
        self.out_of_reach_but(Some(id));
        // the choice has 3 s, or `find`'s time if longer: connecting,
        // before, counts; a config had 3 s of its own before
        let time = crate::config::TIME.max(held_find_time);
        let due = Instant::now() + time.saturating_sub(spent);
        self.choosing = Some(Choosing {
            id,
            attach,
            due,
            time,
            quiet,
        });
        self.arm_choice_timer(due)?;
        self.find(id)
    }

    /// Whether a pane but `but`, not closing, may be in the pane provider's
    /// reach: in it, or finding out.
    pub(super) fn reaches(&self, but: Option<u32>) -> bool {
        self.panes
            .list
            .iter()
            .any(|p| Some(p.id) != but && p.reach != Reach::Out && p.closing.is_none())
    }

    /// Puts every pane but `but` out of reach, except those waiting their
    /// turn to choose, which are checked as it comes.
    pub(super) fn out_of_reach_but(&mut self, but: Option<u32>) {
        for q in self.panes.list.iter_mut() {
            if Some(q.id) != but && !self.waiting_choice.contains(&q.id) {
                q.reach = Reach::Out;
            }
        }
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
        let secs = crate::util::seconds(c.time);
        let what = format!("no answer within {secs} s");
        self.refuse_choice(&what)
    }

    /// Self-attached pane `id` came with a pane provider there: out of reach
    /// if its context variables aren't the choosing attach's, else as its
    /// `find` says.
    fn check_reach(&mut self, id: u32) -> Result<()> {
        if !self.check_context_reach(id) {
            return self.reach_decided();
        }
        self.find(id)
    }

    /// Checks pane `id`'s hello has the choosing attach's values of the
    /// pane provider's context variables, whether it does; if not, the
    /// pane is out of reach.
    fn check_context_reach(&mut self, id: u32) -> bool {
        let vars = self.provider.as_deref().unwrap().context_vars();
        let p = self.panes.by_id(id).unwrap();
        let same = p
            .hello
            .as_deref()
            .is_some_and(|hello| in_context(&vars, &self.context, &hello.env));
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
    /// `find`, it is in reach, which is decided now.
    fn find(&mut self, id: u32) -> Result<()> {
        if !self.ask_find(id) {
            return Ok(());
        }
        let attach_id = self.panes.by_id(id).and_then(|p| p.attach_id);
        if let Some(a) = attach_id {
            self.confirm(&a)?;
        }
        self.reach_decided()
    }

    /// Asks the pane provider to `find` pane `id`'s terminal. Without `find`
    /// it is in reach at once: true, and the caller decides what follows.
    fn ask_find(&mut self, id: u32) -> bool {
        let p = self.provider.as_deref();
        let finds = self.asks.may_ask(p, Capability::Find);
        let find_vars: Vec<String> = p
            .map(|p| p.find_vars().iter().map(|v| v.to_string()).collect())
            .unwrap_or_default();
        let wants_title = p.is_some_and(|p| p.wants_title());
        let Some(q) = self.panes.by_id_mut(id) else {
            return false;
        };
        let attach_id = q.attach_id.expect("an attached pane has an id");
        if !finds {
            q.reach = Reach::In;
            return true;
        }
        // a terminal this pane provider opened stays in its reach whatever
        // `find` says: only its id is to find (#93 decision 17's (c))
        if !q.is_provider_made() {
            q.reach = Reach::Finding;
        }
        let title = pane::title(&attach_id);
        if wants_title && !q.titled {
            q.titled = true;
            self.write_title(id, &title);
        }
        let q = self.panes.by_id(id).unwrap();
        let hello: &AttachHello = q.hello.as_deref().expect("a pane that finds has a hello");
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
            title,
            env,
        };
        // L7 is timed from here
        log::trace(Point::Accept, u64::from(id));
        self.act(ask);
        false
    }

    /// Writes pane title `title` into pane `id`'s terminal, for its
    /// `find`. A terminal that can't take it is gone, which its hang-up
    /// tells.
    fn write_title(&mut self, id: u32, title: &str) {
        let Some(p) = self.panes.by_id_mut(id) else {
            return;
        };
        let token = p.token(PaneFd::Terminal);
        let Some(s) = p.terminal_surface.as_deref_mut() else {
            return;
        };
        let watched = match s.write_title(title) {
            Ok(Sent::Waiting) => self
                .event_loop
                .add_write(s.output_fd(), token)
                .map_err(|e| e.to_string()),
            Ok(Sent::All) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = watched {
            log_info!("{NAME} daemon: pane {id}: can't write its title: {e}");
        }
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
    /// rejoins. Those waiting their turn are checked as it comes.
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
        // the caller decides what follows, once, for those in reach at once
        for id in before {
            if self.provider.is_some() && self.check_context_reach(id) {
                self.ask_find(id);
            }
        }
        Ok(())
    }

    /// Requested pane `id`'s terminal attached, its hello with it: if it
    /// was out of reach, opened by a pane provider since dropped, the one
    /// chosen since checks its reach, as `rejoin` does.
    pub(super) fn rejoin_requested(&mut self, id: u32) -> Result<()> {
        let out = self.panes.by_id(id).is_some_and(|p| p.reach == Reach::Out);
        if !out || self.provider.is_none() || self.a_choice_runs() {
            return Ok(());
        }
        if self.check_context_reach(id) && self.ask_find(id) {
            return self.reach_decided();
        }
        Ok(())
    }

    /// Requested pane `id`'s terminal attached, or its open answered: the
    /// layout provider's `find` is asked for its id, if it has none from
    /// its open, as in a pair, and it attached (ADR 0006). It stays in
    /// reach whatever `find` says.
    pub(super) fn find_requested(&mut self, id: u32) -> Result<()> {
        let Some(p) = self.panes.by_id(id) else {
            return Ok(());
        };
        let asks = p.is_provider_made()
            && p.closing.is_none()
            && p.terminal.is_none()
            && p.hello.is_some()
            && p.terminal_surface.is_some();
        if asks && self.ask_find(id) {
            // no `find`: no id
            self.no_terminal_id(id)?;
        }
        Ok(())
    }

    /// The pane provider's `find` gave requested pane `id` its terminal's
    /// id `t`, which places it, as an open's id would.
    fn found_opened(&mut self, id: u32, t: &TerminalId) -> Result<()> {
        self.place_opened(id);
        let handle = self.panes.by_id(id).and_then(|p| p.neovim_window_handle);
        if handle.is_some() && handle == Some(self.nvim.current_win) {
            self.focus_terminal(Some(t));
        }
        self.wait_for_report(id, Instant::now())
    }

    /// Requested pane `id` has no terminal id, nor will have: nothing
    /// places it, and it is a layout area of its own.
    fn no_terminal_id(&mut self, id: u32) -> Result<()> {
        let Some(p) = self.panes.by_id_mut(id) else {
            return Ok(());
        };
        if p.area != Area::Unreported {
            return Ok(());
        }
        p.area = Area::Own;
        p.report_due = None;
        if !self.nvim.ready {
            return Ok(());
        }
        self.refit_to_areas()
    }

    /// The choice running is refused, for `why`: the pane provider is
    /// dropped, with a warning, or for the default's guess, only a line in
    /// the log.
    fn refuse_choice(&mut self, why: &str) -> Result<()> {
        let quiet = self.choosing.take().is_some_and(|c| c.quiet);
        if quiet {
            log_info!(
                "{NAME} daemon: {}: {why}; no pane provider (the default's guess: not warned of)",
                self.provider_name()
            );
            self.drop_provider()?;
        } else {
            self.provider_gone(why)?;
        }
        self.no_provider = NoProvider::FindRefused;
        self.reach_decided()
    }

    /// The pane provider found the terminal of the pane `find` asked for,
    /// `answer` its id if it gave one: it is in reach. Windows waiting for
    /// that open.
    pub(super) fn found(&mut self, ask: &Ask, answer: Answer) -> Result<()> {
        let Ask::Find { pane, .. } = ask else {
            return Ok(());
        };
        let mut made = None;
        if let Some(p) = self.panes.by_attach_id_mut(pane) {
            // (c): opened by this pane provider, only its id was to find
            if p.is_provider_made() {
                made = Some(p.id);
            }
            p.reach = Reach::In;
            if let Answer::Terminal(t) = &answer {
                p.terminal = Some(t.clone());
            }
            log::trace(Point::Found, u64::from(p.id));
        }
        self.confirm(pane)?;
        if let Some(id) = made {
            match answer {
                Answer::Terminal(t) => self.found_opened(id, &t)?,
                _ => self.no_terminal_id(id)?,
            }
        }
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
            if p.reach == Reach::Finding {
                p.reach = Reach::Out;
                log_info!("{NAME} daemon: pane {} is out of reach: {why}", p.id);
            } else {
                log_info!("{NAME} daemon: pane {} has no terminal id: {why}", p.id);
                let id = p.id;
                self.no_terminal_id(id)?;
            }
        }
        if self.is_choosing(pane) {
            return self.refuse_choice(&format!("find didn't confirm this terminal ({why})"));
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
            let made = p.is_provider_made();
            p.reach = if kept { Reach::In } else { Reach::Out };
            let id = p.id;
            if kept && made {
                self.no_terminal_id(id)?;
            }
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
        while !self.a_choice_runs() && !self.waiting_choice.is_empty() {
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
        self.a_choice_runs()
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

/// Whether an attach whose environment is `env` is in the context the
/// choosing attach was, its values of context variables `vars` being
/// `chose`: each the same bytes, unset matching unset (ADR 0006). With no
/// context variables, every attach is.
pub(crate) fn in_context(vars: &[&str], chose: &[Option<Vec<u8>>], env: &Env) -> bool {
    vars.iter()
        .zip(chose)
        .all(|(v, chose)| env_value(env, v) == chose.as_deref())
}
