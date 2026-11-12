//! The command runner: a pane provider whose capabilities are commands the
//! user's config gives (ADR 0006, #86 decision D2, #93 decisions 10, 13–15,
//! 19, 21 and 22). `docs/capabilities.md` is its contract.
//!
//! - Each ask runs its capability's argv once, without a shell, its
//!   placeholders replaced (`inputs.rs`), in the choosing attach's
//!   environment, stripped of `<NAME>` and the per-act variables, plus the
//!   act's own; in the attach's working directory, else `/`. stdin is
//!   `/dev/null`, and it runs in its own process group (`run.rs`).
//! - Nothing waits: its pipes, its exit and its deadlines are watched on a
//!   loop of its own, which the daemon watches under `Token::Provider`
//!   (`fd`).
//! - Exit 0 is done, and an open's first stdout line, trimmed, the terminal
//!   id: at most 4 KiB, no NUL. Exit 69, or a command that can't start, is
//!   unsupported; anything else fails. The first 64 KiB of stderr goes to
//!   the log.
//! - At its timeout its group gets SIGTERM, and SIGKILL 1 s later. The act
//!   itself is timed out by `asks.rs`, at the same deadline: a run past it
//!   answers nothing.
//!
//! `find`, `report_layout` and `events` are #126's and #127's: a spec's are
//! kept out of its capabilities.

mod inputs;
mod run;

pub(crate) use inputs::commands_env;

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::check;
use crate::daemon::event_loop::{Event, Loop};
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, CapabilitySet, Outcome, PaneProvider, ProviderEvent, TerminalId,
};
use crate::log::{log_info, log_warn};
use crate::name::NAME;
use inputs::Inputs;
use run::{Ended, MAX_ID, Run, Start, signal_group, status_text};

/// The exit status that says a capability is unsupported (EX_UNAVAILABLE).
const UNSUPPORTED: i32 = 69;

/// How long after SIGTERM a timed-out run's group gets SIGKILL.
const KILL_AFTER: Duration = Duration::from_secs(1);

/// The capabilities this runner runs, so far.
const RUNS: [Capability; 9] = [
    Capability::Open,
    Capability::OpenBeside,
    Capability::OpenFloat,
    Capability::OpenTabPage,
    Capability::Close,
    Capability::PlaceBeside,
    Capability::Resize,
    Capability::Focus,
    Capability::FocusDirection,
];

/// The inner loop's one timer, and the tokens on it.
const TIMER: usize = 0;
const TIMER_TOKEN: usize = 0;
const RUN_TOKEN: usize = 1;

/// A pane provider as the runner takes it: each capability's command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spec {
    pub name: String,
    pub commands: Vec<Template>,
}

/// One capability's command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Template {
    pub cap: Capability,
    /// As written, placeholders and all.
    pub argv: Vec<Vec<u8>>,
    pub timeout: Duration,
    /// `<NAME>_PROTOCOL`: the number its table agreed.
    pub protocol: i64,
}

impl Spec {
    /// A pane provider table returned whole.
    pub(crate) fn whole(p: &check::Provider) -> Spec {
        Spec {
            name: p.name.clone(),
            commands: templates(p, |_| true),
        }
    }

    /// A terminal provider and a layout provider, or one alone: `open` and
    /// `close` from the first, the layout capabilities from the second.
    pub(crate) fn pair(p: &check::Pair) -> Spec {
        let mut commands = Vec::new();
        if let Some(t) = &p.terminal {
            commands.extend(templates(t, check::Capability::is_terminal));
        }
        if let Some(l) = &p.layout {
            commands.extend(templates(l, check::Capability::is_layout));
        }
        Spec {
            name: p.name.clone(),
            commands,
        }
    }
}

/// The commands of `p` whose capability `keep` says.
fn templates(p: &check::Provider, keep: impl Fn(check::Capability) -> bool) -> Vec<Template> {
    p.capabilities
        .iter()
        .filter(|(c, _)| keep(*c))
        .filter_map(|(c, cmd)| {
            let cap = RUNS.into_iter().find(|r| r.name() == c.name())?;
            let default = Duration::from_millis(u64::from(cap.timeout_ms()));
            Some(Template {
                cap,
                argv: cmd.argv.clone(),
                timeout: cmd.timeout.unwrap_or(default),
                protocol: p.protocol,
            })
        })
        .collect()
}

/// A command running for an ask.
struct Running {
    id: AskId,
    cap: Capability,
    run: Run,
    due: Instant,
    /// Past its deadline: its group was sent SIGTERM, and it answers
    /// nothing.
    timed_out: bool,
}

pub(crate) struct CommandProvider {
    name: String,
    commands: Vec<Template>,
    /// The choosing attach's environment, stripped.
    env: Vec<(Vec<u8>, Vec<u8>)>,
    /// The choosing attach's working directory.
    cwd: PathBuf,
    /// Its own loop: its pipes, its children's exits and its timer.
    lp: Loop,
    running: Vec<Running>,
    /// Timed-out runs' groups, and when they get SIGKILL.
    kills: Vec<(libc::pid_t, Instant)>,
    events: VecDeque<ProviderEvent>,
    /// Answers known at the ask, given at the next `on_ready`: an ask is
    /// never answered inline.
    soon: Vec<ProviderEvent>,
}

impl CommandProvider {
    /// The runner for `spec`, for an attach with environment `env` and
    /// working directory `cwd`.
    pub(crate) fn new(
        spec: Spec,
        env: &[(Vec<u8>, Vec<u8>)],
        cwd: &[u8],
    ) -> std::io::Result<CommandProvider> {
        let lp = Loop::init().map_err(std::io::Error::other)?;
        Ok(CommandProvider {
            name: spec.name,
            commands: spec.commands,
            env: commands_env(env),
            cwd: PathBuf::from(OsStr::from_bytes(cwd)),
            lp,
            running: Vec::new(),
            kills: Vec::new(),
            events: VecDeque::new(),
            soon: Vec::new(),
        })
    }

    /// The fd the daemon watches under `Token::Provider`.
    pub(crate) fn fd(&self) -> RawFd {
        self.lp.fd()
    }

    fn template(&self, cap: Capability) -> Option<&Template> {
        self.commands.iter().find(|t| t.cap == cap)
    }

    /// Answers `id` at the next `on_ready`, which it makes due now.
    fn answer_soon(&mut self, id: AskId, outcome: Outcome) {
        self.soon.push(ProviderEvent::Answered { id, outcome });
        self.arm_timer(Instant::now());
    }

    /// The outcome of a run of `cap` that ended as `ended`.
    fn outcome(&self, cap: Capability, ended: &Ended) -> Outcome {
        self.log_stderr(cap, ended);
        match ended.status.code() {
            Some(0) if is_open(cap) => match &ended.line {
                Some(line) => terminal_id(line),
                None => Outcome::Failed(too_long()),
            },
            Some(0) => Outcome::Done(Answer::Nothing),
            Some(UNSUPPORTED) => Outcome::Unsupported,
            _ => {
                let first = ended.stderr.split(|&b| b == b'\n').next().unwrap_or(&[]);
                let first = String::from_utf8_lossy(first);
                let status = status_text(ended.status);
                Outcome::Failed(if first.trim().is_empty() {
                    status
                } else {
                    format!("{status}: {}", first.trim_end())
                })
            }
        }
    }

    /// Logs what a run of `cap` wrote to stderr, line by line.
    fn log_stderr(&self, cap: Capability, ended: &Ended) {
        let text = String::from_utf8_lossy(&ended.stderr);
        for line in text.lines() {
            log_info!("{NAME} daemon: {}: {cap}: {line}", self.name);
        }
        if ended.stderr_cut {
            log_info!(
                "{NAME} daemon: {}: {cap}: (stderr past {} KiB dropped)",
                self.name,
                run::MAX_STDERR >> 10
            );
        }
    }

    /// Reads the runs' pipes, finishes the runs that ended, and ends those
    /// past their time.
    fn tend(&mut self) {
        let now = Instant::now();
        let mut i = 0;
        while i < self.running.len() {
            let r = &mut self.running[i];
            r.run.read(&self.lp);
            let Some(ended) = r.run.ended(&self.lp) else {
                if !r.timed_out && r.due <= now {
                    r.timed_out = true;
                    signal_group(r.run.group(), libc::SIGTERM);
                    self.kills.push((r.run.group(), now + KILL_AFTER));
                    log_warn!(
                        "{NAME} daemon: {}: {} ran out of time: SIGTERM",
                        self.name,
                        r.cap
                    );
                }
                i += 1;
                continue;
            };
            let r = self.running.remove(i);
            let outcome = self.outcome(r.cap, &ended);
            if !r.timed_out {
                self.events
                    .push_back(ProviderEvent::Answered { id: r.id, outcome });
            }
        }
        self.kills.retain(|&(group, at)| {
            if at > now {
                return true;
            }
            signal_group(group, libc::SIGKILL);
            false
        });
        self.arm_timer(now);
    }

    /// Sets the timer for the soonest deadline or kill, or for now while an
    /// answer waits to be given.
    fn arm_timer(&self, now: Instant) {
        let soonest = self
            .running
            .iter()
            .filter(|r| !r.timed_out)
            .map(|r| r.due)
            .chain(self.kills.iter().map(|&(_, at)| at))
            .chain((!self.soon.is_empty()).then_some(now))
            .min();
        if let Some(at) = soonest {
            let ms = at.saturating_duration_since(now).as_micros().div_ceil(1000);
            let _ = self
                .lp
                .add_timer(TIMER, u32::try_from(ms).unwrap_or(u32::MAX), TIMER_TOKEN);
        }
    }
}

impl PaneProvider for CommandProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::of(&self.commands.iter().map(|t| t.cap).collect::<Vec<_>>())
    }

    fn timeout(&self, cap: Capability) -> Duration {
        self.template(cap)
            .map(|t| t.timeout)
            .unwrap_or(Duration::from_millis(u64::from(cap.timeout_ms())))
    }

    fn ask(&mut self, id: AskId, ask: Ask) {
        let cap = ask.capability();
        let Some(t) = self.template(cap) else {
            return self.answer_soon(id, Outcome::Unsupported);
        };
        let inputs = Inputs::of(&ask, t.protocol);
        let argv = inputs.argv(&t.argv);
        let env = inputs.env(&self.env);
        let timeout = t.timeout;
        let cwd = if self.cwd.is_absolute() && self.cwd.is_dir() {
            self.cwd.as_path()
        } else {
            Path::new("/")
        };
        let start = Start {
            argv: &argv,
            env: &env,
            cwd,
            reads_id: is_open(cap),
        };
        match Run::start(&start, &self.lp, RUN_TOKEN) {
            Ok(run) => {
                let now = Instant::now();
                self.running.push(Running {
                    id,
                    cap,
                    run,
                    due: now + timeout,
                    timed_out: false,
                });
                self.arm_timer(now);
            }
            Err(e) => {
                let program = String::from_utf8_lossy(&argv[0]).into_owned();
                log_warn!(
                    "{NAME} daemon: {}: {cap}: can't run {program}: {e}",
                    self.name
                );
                self.answer_soon(id, Outcome::Unsupported);
            }
        }
    }

    fn on_ready(&mut self, _token: usize) {
        // Polled once: a pipe at its end stays ready until `tend` reads it
        // and takes it off. What is still ready keeps the loop's fd
        // readable, and the daemon calls again.
        let mut buf = [Event::default(); 16];
        let _ = self.lp.poll(&mut buf);
        self.events.extend(self.soon.drain(..));
        self.tend();
    }

    fn next(&mut self) -> Option<ProviderEvent> {
        self.events.pop_front()
    }

    fn unlaid_size(&self) -> Option<[u32; 2]> {
        None
    }

    fn context_vars(&self) -> &[&str] {
        // reach is #127's
        &[]
    }
}

impl Drop for CommandProvider {
    /// Nothing it started outlives it, but what its commands left behind
    /// on purpose: their groups are killed.
    fn drop(&mut self) {
        for r in self.running.drain(..) {
            r.run.kill(&self.lp);
        }
        for (group, _) in self.kills.drain(..) {
            signal_group(group, libc::SIGKILL);
        }
    }
}

fn is_open(cap: Capability) -> bool {
    matches!(
        cap,
        Capability::Open | Capability::OpenBeside | Capability::OpenFloat | Capability::OpenTabPage
    )
}

/// An open's answer from its stdout's first `line`.
fn terminal_id(line: &[u8]) -> Outcome {
    let id = line.trim_ascii();
    if id.is_empty() {
        return Outcome::Done(Answer::Nothing);
    }
    if id.len() > MAX_ID {
        return Outcome::Failed(too_long());
    }
    if id.contains(&0) {
        return Outcome::Failed("its terminal id holds a NUL".into());
    }
    match std::str::from_utf8(id) {
        Ok(id) => Outcome::Done(Answer::Terminal(TerminalId::from(id))),
        Err(_) => Outcome::Failed("its terminal id isn't UTF-8".into()),
    }
}

fn too_long() -> String {
    format!("its terminal id is over {} KiB", MAX_ID >> 10)
}

#[cfg(test)]
mod tests;
