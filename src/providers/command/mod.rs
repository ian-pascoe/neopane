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
//!   id: at most 4 KiB, no NUL; `report_layout`'s stdout, at most 1 MiB,
//!   the live layout (`layout.rs`). Exit 69, or a command that can't start,
//!   is unsupported; anything else fails. The first 64 KiB of stderr goes
//!   to the log.
//! - At its timeout its group gets SIGTERM, and SIGKILL 1 s later. The act
//!   itself is timed out by `asks.rs`, at the same deadline: a run past it
//!   answers nothing, but says the layout may have changed once it ends.
//! - `events` runs from the start, for as long as the runner lives (#86
//!   decision D7, #93 decision 18, ADR 0006 as #126 amends it). Each line
//!   it prints is told as `ProviderEvent::LayoutChanged`, for the daemon to
//!   ask `report_layout` through its layout queue; a JSON object line's
//!   `focus` and `closed` are told as `Focused` and `Closed` before it.
//!   Lines over 64 KiB are dropped. Once it ends, or a restart can't start,
//!   it starts again after 1 s, then 2 s, doubling up to 60 s; one that ran
//!   60 s waits 1 s again. Exit 69, or a first start that fails, ends it for
//!   good.
//!
//! `find` is #127's: a spec's is kept out of its capabilities. Building a
//! spec from the config's choice is #129's.

mod inputs;
mod layout;
mod run;

pub(crate) use inputs::commands_env;

use std::collections::VecDeque;
use std::ffi::OsStr;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config;
use crate::daemon::event_loop::{Event, Loop};
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, CapabilitySet, Outcome, PaneProvider, ProviderEvent, TerminalId,
};
use crate::log::{log_info, log_warn};
use crate::name::NAME;
use inputs::Inputs;
use layout::{MAX_REPORT, parse_report_layout};
use run::{Ended, MAX_ID, Run, Start, Stdout, signal_group, status_text};

/// The exit status that says a capability is unsupported (EX_UNAVAILABLE).
const UNSUPPORTED: i32 = 69;

/// How long after SIGTERM a timed-out run's group gets SIGKILL.
const KILL_AFTER: Duration = Duration::from_secs(1);

/// How long `events` waits to start again after it ended at first, and at
/// most; and how long it must have run for the wait to be the first again.
const EVENTS_FIRST_WAIT: Duration = Duration::from_secs(1);
const EVENTS_MOST_WAIT: Duration = Duration::from_secs(60);
const EVENTS_STEADY: Duration = Duration::from_secs(60);

/// The capabilities this runner runs, so far.
const RUNS: [Capability; 11] = [
    Capability::Open,
    Capability::OpenBeside,
    Capability::OpenFloat,
    Capability::OpenTabPage,
    Capability::Close,
    Capability::PlaceBeside,
    Capability::Resize,
    Capability::Focus,
    Capability::FocusDirection,
    Capability::ReportLayout,
    Capability::Events,
];

/// The id of the inner loop's one timer, set for the soonest deadline, kill
/// or answer due; and the tokens on the inner loop: that timer's, and every
/// run's pipes and exit.
const DUE_TIMER_ID: usize = 0;
const DUE_TOKEN: usize = 0;
const RUN_TOKEN: usize = 1;

/// A pane provider as the runner takes it: each capability's command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spec {
    pub name: String,
    pub commands: Vec<CapCommand>,
    /// Its terminal half's attach timeout (#88 decision 15).
    pub attach_timeout: Duration,
}

/// The attach timeout of the pane provider the config chose, for its
/// `Spec`: its terminal half's, or a whole table's; the default without one
/// (#88 decision 15).
pub(crate) fn attach_timeout_of(choice: &config::Choice) -> Duration {
    let terminal = match choice {
        config::Choice::Whole(p) => Some(p),
        config::Choice::Pair(pair) => pair.terminal.as_ref(),
        config::Choice::None | config::Choice::Builtin(_) => None,
    };
    terminal.map_or(config::check::ATTACH_TIMEOUT, |t| t.attach_timeout)
}

/// One capability's command.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CapCommand {
    pub cap: Capability,
    /// As written, placeholders and all.
    pub argv: Vec<Vec<u8>>,
    pub timeout: Duration,
    /// `<NAME>_PROTOCOL`: the number its table agreed.
    pub protocol: i64,
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

/// `events`, while the runner has it.
enum Events {
    Running {
        run: Run,
        since: Instant,
    },
    /// Ended: it starts again at `until`.
    Waiting {
        until: Instant,
    },
}

/// The time, as the runner sees it: `Instant::now`, but in tests.
type Clock = Box<dyn Fn() -> Instant>;

pub(crate) struct CommandProvider {
    name: String,
    attach_timeout: Duration,
    commands: Vec<CapCommand>,
    /// `events`, and how long it waits to start again the next time it
    /// ends; None once it has ended for good, or without one.
    events_run: Option<Events>,
    events_wait: Duration,
    clock: Clock,
    /// The choosing attach's environment, stripped.
    env: Vec<(Vec<u8>, Vec<u8>)>,
    /// The choosing attach's working directory.
    cwd: PathBuf,
    /// Its own loop: its pipes, its children's exits and its timer.
    inner_loop: Loop,
    running: Vec<Running>,
    /// Timed-out runs' groups, and when they get SIGKILL.
    kills: Vec<(libc::pid_t, Instant)>,
    events: VecDeque<ProviderEvent>,
    /// Answers known at the ask, given at the next `on_ready`: an ask is
    /// never answered inline.
    answers_due: Vec<ProviderEvent>,
}

impl CommandProvider {
    /// The runner for `spec`, for an attach with environment `env` and
    /// working directory `cwd`.
    pub(crate) fn new(
        spec: Spec,
        env: &[(Vec<u8>, Vec<u8>)],
        cwd: &[u8],
    ) -> std::io::Result<CommandProvider> {
        CommandProvider::with_clock(spec, env, cwd, Box::new(Instant::now))
    }

    /// The same, its time told by `clock`. `events` starts at once.
    fn with_clock(
        spec: Spec,
        env: &[(Vec<u8>, Vec<u8>)],
        cwd: &[u8],
        clock: Clock,
    ) -> std::io::Result<CommandProvider> {
        let inner_loop = Loop::init().map_err(std::io::Error::other)?;
        let mut p = CommandProvider {
            name: spec.name,
            attach_timeout: spec.attach_timeout,
            // `find` isn't run yet
            commands: spec
                .commands
                .into_iter()
                .filter(|c| RUNS.contains(&c.cap))
                .collect(),
            events_run: None,
            events_wait: EVENTS_FIRST_WAIT,
            clock,
            env: commands_env(env),
            cwd: PathBuf::from(OsStr::from_bytes(cwd)),
            inner_loop,
            running: Vec::new(),
            kills: Vec::new(),
            events: VecDeque::new(),
            answers_due: Vec::new(),
        };
        if p.command(Capability::Events).is_some() {
            p.start_events(true);
        }
        Ok(p)
    }

    fn command(&self, cap: Capability) -> Option<&CapCommand> {
        self.commands.iter().find(|t| t.cap == cap)
    }

    /// Answers `id` at the next `on_ready`, which it makes due now.
    fn answer_at_next_ready(&mut self, id: AskId, outcome: Outcome) {
        self.answers_due
            .push(ProviderEvent::Answered { id, outcome });
        self.arm_timer((self.clock)());
    }

    /// The outcome of a run of `cap` that ended as `ended`.
    fn outcome(&self, cap: Capability, ended: &Ended) -> Outcome {
        self.log_stderr(cap, ended);
        match ended.status.code() {
            Some(0) => match (stdout_of(cap), &ended.out) {
                (Stdout::FirstLine, Some(line)) => terminal_id(line),
                (Stdout::FirstLine, None) => Outcome::Failed(too_long()),
                (Stdout::Whole, Some(out)) => match parse_report_layout(out) {
                    Ok(l) => Outcome::Done(Answer::Layout(l)),
                    Err(why) => Outcome::Failed(why),
                },
                (Stdout::Whole, None) => {
                    Outcome::Failed(format!("its report is over {} KiB", MAX_REPORT >> 10))
                }
                (Stdout::Null | Stdout::Lines, _) => Outcome::Done(Answer::Nothing),
            },
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

    /// Starts `cap`'s command with `inputs`, watched on the inner loop.
    /// Fails if it can't start, which is logged.
    fn spawn(&self, cap: Capability, inputs: &Inputs) -> Option<Run> {
        let t = self.command(cap).expect("a capability it has");
        let argv = inputs.argv(&t.argv);
        let env = inputs.env(&self.env);
        let start = Start {
            argv: &argv,
            env: &env,
            cwd: self.cwd(),
            stdout: stdout_of(cap),
        };
        match Run::start(&start, &self.inner_loop, RUN_TOKEN) {
            Ok(run) => Some(run),
            Err(e) => {
                let program = String::from_utf8_lossy(&argv[0]).into_owned();
                log_warn!(
                    "{NAME} daemon: {}: {cap}: can't run {program}: {e}",
                    self.name
                );
                None
            }
        }
    }

    /// The attach's working directory, if it's still there, else `/`.
    fn cwd(&self) -> &Path {
        if self.cwd.is_absolute() && self.cwd.is_dir() {
            self.cwd.as_path()
        } else {
            Path::new("/")
        }
    }

    /// `cap` turned out unsupported: it's run no more.
    fn drop_command(&mut self, cap: Capability) {
        log_info!("{NAME} daemon: {}: {cap} is unsupported", self.name);
        self.commands.retain(|c| c.cap != cap);
    }

    /// Starts `events`, its `first` start or a restart, and tells the
    /// layout changed: what it would have told while it wasn't running is
    /// missed. A first start that fails ends it; a restart that fails
    /// waits to try again.
    fn start_events(&mut self, first: bool) {
        let protocol = self.command(Capability::Events).expect("events").protocol;
        let inputs = Inputs::bare(Capability::Events, protocol);
        let now = (self.clock)();
        match self.spawn(Capability::Events, &inputs) {
            Some(run) => {
                self.events_run = Some(Events::Running { run, since: now });
                self.events.push_back(ProviderEvent::LayoutChanged);
            }
            None if first => {
                self.events_run = None;
                self.drop_command(Capability::Events);
            }
            None => self.wait_to_restart(now),
        }
    }

    /// `events` starts again after its wait from `now`, which doubles.
    fn wait_to_restart(&mut self, now: Instant) {
        let wait = self.events_wait;
        log_warn!(
            "{NAME} daemon: {}: starting events again in {} s",
            self.name,
            wait.as_secs()
        );
        self.events_run = Some(Events::Waiting { until: now + wait });
        self.events_wait = (wait * 2).min(EVENTS_MOST_WAIT);
    }

    /// Reads what `events` printed, and starts it again once its wait after
    /// it ended is over, at `now`.
    fn tend_events(&mut self, now: Instant) {
        match &mut self.events_run {
            None => {}
            Some(Events::Waiting { until }) => {
                if *until <= now {
                    self.start_events(false);
                }
            }
            Some(Events::Running { run, since }) => {
                let since = *since;
                run.read(&self.inner_loop);
                let ended = run.ended(&self.inner_loop);
                let (lines, dropped) = run.take_lines();
                if dropped > 0 {
                    log_warn!(
                        "{NAME} daemon: {}: events: {dropped} line(s) over {} KiB dropped",
                        self.name,
                        run::MAX_LINE >> 10
                    );
                }
                for line in &lines {
                    self.on_events_line(line);
                }
                if let Some(ended) = ended {
                    self.events_ended(&ended, now.saturating_duration_since(since), now);
                }
            }
        }
    }

    /// One line `events` printed: a JSON object's `focus` and `closed` are
    /// told, then that the layout changed.
    fn on_events_line(&mut self, line: &[u8]) {
        let Ok(serde_json::Value::Object(o)) = serde_json::from_slice(line.trim_ascii()) else {
            self.events.push_back(ProviderEvent::LayoutChanged);
            return;
        };
        let id = |key: &str| {
            o.get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(TerminalId::from)
        };
        if let Some(t) = id("focus") {
            self.events.push_back(ProviderEvent::Focused(t));
        }
        if let Some(t) = id("closed") {
            self.events.push_back(ProviderEvent::Closed(t));
        }
        self.events.push_back(ProviderEvent::LayoutChanged);
    }

    /// `events` ended as `ended`, at `now`, having run for `ran`: it starts
    /// again after its wait, unless it's unsupported.
    fn events_ended(&mut self, ended: &Ended, ran: Duration, now: Instant) {
        self.log_stderr(Capability::Events, ended);
        if ended.status.code() == Some(UNSUPPORTED) {
            self.events_run = None;
            self.drop_command(Capability::Events);
            return;
        }
        if ran >= EVENTS_STEADY {
            self.events_wait = EVENTS_FIRST_WAIT;
        }
        log_warn!(
            "{NAME} daemon: {}: events ended ({})",
            self.name,
            status_text(ended.status)
        );
        self.wait_to_restart(now);
    }

    /// Reads the runs' pipes, finishes the runs that ended, and ends those
    /// past their time; and tends `events`.
    fn tend(&mut self) {
        let now = (self.clock)();
        self.tend_events(now);
        let mut i = 0;
        while i < self.running.len() {
            let r = &mut self.running[i];
            r.run.read(&self.inner_loop);
            let Some(ended) = r.run.ended(&self.inner_loop) else {
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
            } else if r.cap != Capability::ReportLayout {
                // killed, it may have moved terminals: read once it's gone,
                // not while it may still be moving them
                self.events.push_back(ProviderEvent::LayoutChanged);
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

    /// Sets the timer for the soonest deadline, kill or start of `events`,
    /// or for now while an answer waits to be given.
    fn arm_timer(&self, now: Instant) {
        let restart = match &self.events_run {
            Some(Events::Waiting { until }) => Some(*until),
            _ => None,
        };
        let soonest = self
            .running
            .iter()
            .filter(|r| !r.timed_out)
            .map(|r| r.due)
            .chain(self.kills.iter().map(|&(_, at)| at))
            .chain(restart)
            .chain((!self.answers_due.is_empty()).then_some(now))
            .min();
        if let Some(at) = soonest {
            let ms = at.saturating_duration_since(now).as_micros().div_ceil(1000);
            let _ = self.inner_loop.add_timer(
                DUE_TIMER_ID,
                u32::try_from(ms).unwrap_or(u32::MAX),
                DUE_TOKEN,
            );
        }
    }

    /// Whether `events` is running, not waiting to start again.
    #[cfg(test)]
    fn events_running(&self) -> bool {
        matches!(self.events_run, Some(Events::Running { .. }))
    }
}

impl PaneProvider for CommandProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn fd(&self) -> Option<RawFd> {
        Some(self.inner_loop.fd())
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet::of(&self.commands.iter().map(|t| t.cap).collect::<Vec<_>>())
    }

    fn timeout(&self, cap: Capability) -> Duration {
        self.command(cap)
            .map(|t| t.timeout)
            .unwrap_or(cap.default_timeout())
    }

    fn ask(&mut self, id: AskId, ask: Ask) {
        let cap = ask.capability();
        let Some(t) = self.command(cap) else {
            return self.answer_at_next_ready(id, Outcome::Unsupported);
        };
        let timeout = t.timeout;
        let inputs = Inputs::of(&ask, t.protocol);
        let Some(run) = self.spawn(cap, &inputs) else {
            return self.answer_at_next_ready(id, Outcome::Unsupported);
        };
        let now = (self.clock)();
        self.running.push(Running {
            id,
            cap,
            run,
            due: now + timeout,
            timed_out: false,
        });
        self.arm_timer(now);
    }

    fn on_ready(&mut self, _token: usize) {
        // Polled once: a pipe at its end stays ready until `tend` reads it
        // and takes it off. What is still ready keeps the loop's fd
        // readable, and the daemon calls again.
        let mut buf = [Event::default(); 16];
        let _ = self.inner_loop.poll(&mut buf);
        self.events.extend(self.answers_due.drain(..));
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

    fn attach_timeout(&self) -> Duration {
        self.attach_timeout
    }
}

impl Drop for CommandProvider {
    /// Nothing it started outlives it, but what its commands left behind
    /// on purpose: their groups are killed, `events`' too.
    fn drop(&mut self) {
        if let Some(Events::Running { run, .. }) = self.events_run.take() {
            run.kill(&self.inner_loop);
        }
        for r in self.running.drain(..) {
            r.run.kill(&self.inner_loop);
        }
        for (group, _) in self.kills.drain(..) {
            signal_group(group, libc::SIGKILL);
        }
    }
}

/// What becomes of a run of `cap`'s stdout.
fn stdout_of(cap: Capability) -> Stdout {
    match cap {
        _ if is_open(cap) => Stdout::FirstLine,
        Capability::ReportLayout => Stdout::Whole,
        Capability::Events => Stdout::Lines,
        _ => Stdout::Null,
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
