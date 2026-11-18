//! `events`: live focus and layout from tmux 3.8's `wait-for -E`, without
//! attaching a client. One waiter per event of [`EVENTS`], each a `tmux
//! wait-for -E NAME`, which ends at that event anywhere on the server.
//!
//! - A waiter that ends with the event is armed again at once. Once no
//!   waiter has ended for [`SETTLE`] since the last was armed again, and
//!   tmux lists every waiter armed, so that each has registered with the
//!   server and what it missed meanwhile shows in the read, it prints one
//!   line: `{"focus":"%N"}` if the focused pane, found as `report_layout`
//!   finds it, changed, else a bare line, so the layout is read again.
//!   Its first line, once the waiters are armed, have settled and are
//!   listed likewise, is where focus starts, if a client shows any: the
//!   daemon follows moves from there. Else it is a bare line: the layout
//!   may have changed before the waiters registered.
//! - A listing is `tmux wait-for -E -l EVENT`, which names each waiter by
//!   its `wait-for` client's pid, so `tmux` must be run, not forked from a
//!   wrapper, for the pid it spawned to be the one listed.
//! - A waiter that fails at once, within [`AT_ONCE`], is dropped; one that
//!   fails later, such as at a server's restart, is armed again, and fails
//!   at once if the server is gone. Once every waiter is gone, it fails.
//! - Below 3.8 there's no `wait-for -E`: it exits 69, and the daemon reads
//!   the layout after each act instead.
//!
//! Control mode, the other way to hear tmux unasked, is no use: a control
//! client counts as attached, runs the user's `client-attached` hooks, and
//! keeps every pane of its session from losing focus. Hooks set on the
//! server would outlive a daemon killed.

use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;

use super::Failure;
use super::layout::focused;
use super::server::{Server, older_than};
use crate::providers::builtin::{DONE, Given, print_line};

/// The first tmux with `wait-for -E`.
pub(super) const FIRST_WITH_EVENTS: (u32, u32) = (3, 8);

/// The events waited for: what moves or resizes panes or windows, or the
/// client the focus is read from. An option that resizes panes, such as
/// `pane-border-status`, ends only `pane-resized`: the layout stays the
/// same.
pub(super) const EVENTS: [&str; 10] = [
    "window-layout-changed",
    "pane-resized",
    "window-pane-changed",
    "session-window-changed",
    "client-session-changed",
    "window-linked",
    "window-unlinked",
    "client-attached",
    "client-detached",
    "client-active",
];

/// How long after an event it waits for the rest of its burst.
const SETTLE: Duration = Duration::from_millis(30);

/// The longest a burst settles, however many waiters keep ending.
const MOST_SETTLE: Duration = Duration::from_millis(250);

/// How often it asks tmux which waiters are registered, while some aren't.
const LISTING_EVERY: Duration = Duration::from_millis(10);

/// A waiter failing sooner than this after it was armed failed at once.
const AT_ONCE: Duration = Duration::from_secs(1);

/// Runs `events` on the server `TMUX` names, until every waiter is gone
/// or the daemon stops reading: why it ended.
pub(super) fn events(given: &Given) -> Failure {
    let server = match Server::of(given) {
        Ok(s) => s,
        Err(why) => return why.into(),
    };
    let version = match Server::version() {
        Ok(v) => v,
        Err(why) => return why.into(),
    };
    if older_than(&version, FIRST_WITH_EVENTS) {
        let (major, minor) = FIRST_WITH_EVENTS;
        return Failure::Unsupported(format!(
            "tmux {version} can't wait for events: that needs {major}.{minor} or newer"
        ));
    }
    let mut waiters = TmuxWaiters::on(server);
    watch(&mut waiters, |line| print_line(line.as_bytes()) == DONE)
}

/// A waiter that ended.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Ended {
    pub event: &'static str,
    /// Whether it ended with its event, not failing.
    pub woke: bool,
    /// How long it ran.
    pub ran: Duration,
}

/// What the loop runs on: tmux's waiters, or a test's.
pub(super) trait Waiters {
    /// Starts a waiter on `event`; `next` tells when it ends.
    fn arm(&mut self, event: &'static str);
    /// The next waiter to end, waiting at most `within`, or until one does
    /// with None.
    fn next(&mut self, within: Option<Duration>) -> Option<Ended>;
    /// Whether every waiter armed that hasn't ended has registered with the
    /// server: it is listed as waiting on its event. A waiter that ended is
    /// no longer waited for.
    fn registered(&mut self) -> bool;
    /// The focused pane, as `report_layout` finds it, if any.
    fn focus(&mut self) -> Option<String>;
}

/// Runs the loop on `waiters`, printing each line with `print`, which
/// says false once the reader is gone: why it ended.
pub(super) fn watch(waiters: &mut impl Waiters, mut print: impl FnMut(&str) -> bool) -> Failure {
    for event in EVENTS {
        waiters.arm(event);
    }
    let mut live = EVENTS.len();
    // where focus starts, read once the waiters have registered, so what
    // happens meanwhile is in it: the daemon follows only moves from there.
    // Printed, bare, when no client shows focus too: see the module doc
    let mut held = VecDeque::new();
    settle(waiters, None, &mut held, &mut live);
    let mut focus = waiters.focus();
    let first = match &focus {
        Some(pane) => json!({ "focus": pane }).to_string(),
        None => String::new(),
    };
    if !print(&first) {
        return Failure::Failed("the reader is gone".into());
    }
    while live > 0 {
        let first = match held.pop_front() {
            Some(first) => first,
            None => match waiters.next(None) {
                Some(first) => first,
                None => continue,
            },
        };
        if !settle(waiters, Some(first), &mut held, &mut live) {
            continue;
        }
        let now = waiters.focus();
        let line = match &now {
            Some(pane) if now != focus => json!({ "focus": pane }).to_string(),
            _ => String::new(),
        };
        if now.is_some() {
            focus = now;
        }
        if !print(&line) {
            return Failure::Failed("the reader is gone".into());
        }
    }
    Failure::Failed("every waiter failed".into())
}

/// Takes `first`, then every waiter that ends until none has for
/// [`SETTLE`] since the last was armed again, at most [`MOST_SETTLE`] in
/// all, past which those still ending are put in `held`, not armed
/// again, to start the next burst. Then it waits until every waiter armed
/// has registered with the server (`registered`), however long that takes,
/// taking those that end meanwhile likewise: a read after it sees what a
/// waiter that was registering missed. One that woke, or failed late, is
/// armed again; one that failed at once is dropped from `live`. Whether
/// any woke, and was armed again.
fn settle(
    waiters: &mut impl Waiters,
    first: Option<Ended>,
    held: &mut VecDeque<Ended>,
    live: &mut usize,
) -> bool {
    let start = Instant::now();
    let mut due = start + SETTLE;
    let mut woke = false;
    let mut ended = first;
    // the last burst's, taken before anything new
    let mut backlog = std::mem::take(held);
    loop {
        if ended.is_none() {
            ended = backlog.pop_front();
        }
        if let Some(e) = ended.take() {
            if e.woke || e.ran >= AT_ONCE {
                if Instant::now() >= start + MOST_SETTLE {
                    held.push_back(e);
                } else {
                    woke |= e.woke;
                    waiters.arm(e.event);
                    due = (Instant::now() + SETTLE).min(start + MOST_SETTLE);
                }
            } else {
                *live -= 1;
            }
        }
        if !backlog.is_empty() {
            continue;
        }
        let left = due.saturating_duration_since(Instant::now());
        ended = waiters.next(Some(left));
        if ended.is_some() {
            continue;
        }
        // the burst is over; what's armed may not have registered, and a
        // change before it does ends no waiter
        while !waiters.registered() {
            ended = waiters.next(Some(LISTING_EVERY));
            if ended.is_some() {
                break;
            }
        }
        if ended.is_none() {
            return woke;
        }
    }
}

/// tmux's waiters: each a `tmux wait-for -E EVENT`, waited for on a thread
/// of its own, which tells the loop as it ends. Those still running are
/// killed as it drops.
struct TmuxWaiters {
    server: Server,
    tx: Sender<Ended>,
    rx: Receiver<Ended>,
    /// The pids of the waiters running.
    running: Arc<Mutex<Vec<u32>>>,
    /// The waiters running that tmux hasn't been seen to list yet, by
    /// event and pid; one that ends leaves it.
    unlisted: Arc<Mutex<Vec<(&'static str, u32)>>>,
}

impl TmuxWaiters {
    fn on(server: Server) -> TmuxWaiters {
        let (tx, rx) = channel();
        TmuxWaiters {
            server,
            tx,
            rx,
            running: Arc::default(),
            unlisted: Arc::default(),
        }
    }
}

/// The pids of the waiters `tmux wait-for -E -l EVENT` printed as `out`,
/// one `client-PID` per line: the pid of each `wait-for` client.
fn listed_pids(out: &[u8]) -> Vec<u32> {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|line| line.trim().strip_prefix("client-")?.parse().ok())
        .collect()
}

impl Waiters for TmuxWaiters {
    fn arm(&mut self, event: &'static str) {
        let armed = Instant::now();
        let tx = self.tx.clone();
        // its stdout is ours, the daemon's lines; its stderr goes to the log
        let spawned = self
            .server
            .command(&["wait-for", "-E", event])
            .stdout(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                eprintln!("tmux events: can't run tmux wait-for -E {event}: {e}");
                let _ = tx.send(Ended {
                    event,
                    woke: false,
                    ran: Duration::ZERO,
                });
                return;
            }
        };
        let pid = child.id();
        self.running.lock().unwrap().push(pid);
        self.unlisted.lock().unwrap().push((event, pid));
        let running = Arc::clone(&self.running);
        let unlisted = Arc::clone(&self.unlisted);
        std::thread::spawn(move || {
            let exited = exited_unreaped(|| wait_unreaped(pid));
            let woke = reap(&mut child, &running, exited);
            unlisted.lock().unwrap().retain(|&(_, p)| p != pid);
            let _ = tx.send(Ended {
                event,
                woke,
                ran: armed.elapsed(),
            });
        });
    }

    fn next(&mut self, within: Option<Duration>) -> Option<Ended> {
        match within {
            None => self.rx.recv().ok(),
            Some(d) => self.rx.recv_timeout(d).ok(),
        }
    }

    fn registered(&mut self) -> bool {
        let mut events: Vec<&str> = Vec::new();
        for &(event, _) in self.unlisted.lock().unwrap().iter() {
            if !events.contains(&event) {
                events.push(event);
            }
        }
        if events.is_empty() {
            return true;
        }
        // one tmux asks for every event: `wait-for -E -l A ; wait-for -E -l B`
        let mut args = Vec::new();
        for event in events {
            if !args.is_empty() {
                args.push(";");
            }
            args.extend(["wait-for", "-E", "-l", event]);
        }
        // a listing that fails shows none: they stay unlisted, and are
        // asked about again, until they register or end
        let listed = self.server.run(&args).map(|out| listed_pids(&out));
        let mut unlisted = self.unlisted.lock().unwrap();
        if let Ok(listed) = listed {
            unlisted.retain(|(_, pid)| !listed.contains(pid));
        }
        unlisted.is_empty()
    }

    fn focus(&mut self) -> Option<String> {
        focused(&self.server).ok().flatten()
    }
}

/// Waits for waiter `pid` to exit, leaving it unreaped, so its pid stays
/// ours while it's listed.
fn wait_unreaped(pid: u32) -> std::io::Result<()> {
    // SAFETY: zeroed siginfo_t is valid; waitid writes it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: a plain syscall, on our own child, with a valid out-pointer.
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Whether `wait`, a [`wait_unreaped`], saw its waiter exit: tried again
/// while a signal interrupts it, false on any other error.
fn exited_unreaped(mut wait: impl FnMut() -> std::io::Result<()>) -> bool {
    loop {
        match wait() {
            Ok(()) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

/// Reaps waiter `child`, listed in `running`, and gives whether it woke.
/// One known to have `exited` is unlisted and reaped under the lock, which
/// doesn't block, so `Drop` never signals a pid reused meanwhile. Else it
/// is unlisted first, so `Drop` can no longer signal it, and reaped off the
/// lock: it may run until its event.
fn reap(child: &mut std::process::Child, running: &Mutex<Vec<u32>>, exited: bool) -> bool {
    let pid = child.id();
    let mut listed = running.lock().unwrap();
    listed.retain(|&p| p != pid);
    if exited {
        return child.wait().is_ok_and(|s| s.success());
    }
    drop(listed);
    child.wait().is_ok_and(|s| s.success())
}

impl Drop for TmuxWaiters {
    fn drop(&mut self) {
        // a waiter listed isn't reaped while the lock is held, so its pid
        // is still ours
        for &pid in self.running.lock().unwrap().iter() {
            // SAFETY: a plain syscall, to a waiter of ours.
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A step of a fake's script.
    enum Step {
        /// A waiter ends.
        End(Ended),
        /// No waiter ends within the settle.
        Quiet,
        /// A waiter ends after a while, real time.
        Late(Duration, Ended),
    }

    /// What the loop asked of a fake, in order.
    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Arm(&'static str),
        Next(Option<Duration>),
        Registered(bool),
        Focus,
    }

    /// Waiters that end as their script says; focus is each of `focus` in
    /// turn, the last kept. They have registered as each of `listed` says
    /// in turn, then always.
    #[derive(Default)]
    struct Fake {
        script: VecDeque<Step>,
        armed: Vec<&'static str>,
        listed: VecDeque<bool>,
        focus: VecDeque<Option<String>>,
        last_focus: Option<String>,
        calls: Vec<Call>,
    }

    impl Waiters for Fake {
        fn arm(&mut self, event: &'static str) {
            self.calls.push(Call::Arm(event));
            self.armed.push(event);
        }

        fn registered(&mut self) -> bool {
            let listed = self.listed.pop_front().unwrap_or(true);
            self.calls.push(Call::Registered(listed));
            listed
        }

        fn next(&mut self, within: Option<Duration>) -> Option<Ended> {
            self.calls.push(Call::Next(within));
            loop {
                match self.script.pop_front() {
                    Some(Step::End(e)) => return Some(e),
                    Some(Step::Late(after, e)) => {
                        std::thread::sleep(after);
                        return Some(e);
                    }
                    Some(Step::Quiet) if within.is_some() => return None,
                    Some(Step::Quiet) => continue,
                    None if within.is_some() => return None,
                    None => panic!("nothing more ends, and the loop waits for ever"),
                }
            }
        }

        fn focus(&mut self) -> Option<String> {
            self.calls.push(Call::Focus);
            if let Some(f) = self.focus.pop_front() {
                self.last_focus = f;
            }
            self.last_focus.clone()
        }
    }

    fn woke(event: &'static str) -> Step {
        Step::End(Ended {
            event,
            woke: true,
            ran: Duration::from_secs(5),
        })
    }

    fn failed(event: &'static str, ran: Duration) -> Step {
        Step::End(Ended {
            event,
            woke: false,
            ran,
        })
    }

    /// A quiet start, `steps`, then every waiter failing at once.
    fn then_all_fail(steps: Vec<Step>) -> VecDeque<Step> {
        let mut all = vec![Step::Quiet];
        all.extend(steps);
        all.extend(EVENTS.map(|e| failed(e, Duration::ZERO)));
        all.into()
    }

    /// Whether, before each focus read, the loop last waited a whole
    /// settle for more waiters to end, then found them registered: what
    /// `calls` says.
    fn settled_before_each_read(calls: &[Call]) -> bool {
        calls.iter().enumerate().all(|(i, c)| {
            *c != Call::Focus
                || matches!(
                    calls[..i].iter().rev().find(|c| !matches!(c, Call::Registered(_))),
                    Some(Call::Next(Some(d))) if *d >= SETTLE - Duration::from_millis(3)
                )
        })
    }

    /// Whether, before each focus read, the last the loop asked of the
    /// waiters was whether they had registered, and they had, with none
    /// armed since: what `calls` says.
    fn registered_before_each_read(calls: &[Call]) -> bool {
        calls.iter().enumerate().all(|(i, c)| {
            *c != Call::Focus
                || matches!(
                    calls[..i]
                        .iter()
                        .rev()
                        .find(|c| matches!(c, Call::Arm(_) | Call::Registered(_))),
                    Some(Call::Registered(true))
                )
        })
    }

    /// What `fake` printed, and why it ended.
    fn run(fake: &mut Fake) -> (Vec<String>, Failure) {
        let mut lines = Vec::new();
        let why = watch(fake, |l| {
            lines.push(l.to_string());
            true
        });
        (lines, why)
    }

    #[test]
    fn it_arms_one_waiter_per_event_and_fails_once_every_one_failed_at_once() {
        let mut fake = Fake {
            script: then_all_fail(Vec::new()),
            ..Fake::default()
        };
        let (_, why) = run(&mut fake);
        assert_eq!(fake.armed, EVENTS);
        assert!(matches!(why, Failure::Failed(_)));
    }

    /// What changed before the waiters registered, such as a
    /// `pane-border-status` set, ended none of them: the first line asks
    /// for the layout to be read again, a bare one where no client shows
    /// focus.
    #[test]
    fn its_first_line_comes_once_the_waiters_settled_bare_where_no_client_shows_focus() {
        let mut fake = Fake {
            script: then_all_fail(Vec::new()),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, [""]);
        assert!(settled_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    /// SPI-1281: a `wait-for` that is slow to start registers after the
    /// settle, and what changes before it does ends no waiter. The first
    /// line, which makes the daemon read the layout again, waits for every
    /// waiter to be listed, however long that takes.
    #[test]
    fn its_first_line_waits_for_every_waiter_to_register() {
        let mut fake = Fake {
            // each a poll, in which no waiter ends
            script: then_all_fail(vec![Step::Quiet, Step::Quiet, Step::Quiet]),
            listed: [false, false, false].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, [""]);
        let asked: Vec<_> = fake
            .calls
            .iter()
            .filter(|c| matches!(c, Call::Registered(_)))
            .collect();
        assert_eq!(
            asked[..3],
            [&Call::Registered(false); 3],
            "{:?}",
            fake.calls
        );
        assert!(registered_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    /// A waiter armed again after its event registers late likewise: the
    /// line for the event waits for it.
    #[test]
    fn a_line_waits_for_the_waiter_armed_again_to_register() {
        let mut fake = Fake {
            script: then_all_fail(vec![
                woke("window-layout-changed"),
                Step::Quiet,
                Step::Quiet,
                Step::Quiet,
            ]),
            // at the start, then after the event
            listed: [true, false, false, true].into(),
            focus: [Some("%0".into()), Some("%1".into())].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, [r#"{"focus":"%0"}"#, r#"{"focus":"%1"}"#]);
        assert!(registered_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    /// A waiter that wakes while the loop waits for another to register
    /// is in the same burst: it is armed again, and waited for too.
    #[test]
    fn a_waiter_waking_while_another_registers_is_armed_again_and_waited_for() {
        let mut fake = Fake {
            script: then_all_fail(vec![
                // the start: nothing ends in the settle, one waiter isn't
                // registered, and another wakes meanwhile
                woke("window-pane-changed"),
                Step::Quiet,
            ]),
            listed: [false, true].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(fake.armed[EVENTS.len()..].contains(&"window-pane-changed"));
        assert!(registered_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    /// Waiters that keep waking while the loop waits for one to register
    /// don't keep it from printing for ever: past [`MOST_SETTLE`] they are
    /// not armed again, the line is printed, and they start the next burst.
    #[test]
    fn a_burst_past_the_most_settle_is_cut_while_a_waiter_registers() {
        let late = |event, ms| {
            Step::Late(
                Duration::from_millis(ms),
                Ended {
                    event,
                    woke: true,
                    ran: Duration::from_secs(5),
                },
            )
        };
        let mut fake = Fake {
            script: then_all_fail(vec![
                late("window-layout-changed", 80),
                late("window-pane-changed", 80),
                // at 360 ms: past the most a burst settles
                late("pane-resized", 200),
                Step::Quiet,
            ]),
            listed: [false].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines.len(), 2, "{lines:?}");
        let first_read = fake.calls.iter().position(|c| *c == Call::Focus).unwrap();
        let armed_before_read = |event| {
            fake.calls[..first_read]
                .iter()
                .filter(|c| **c == Call::Arm(event))
                .count()
        };
        // the first two were armed again in the burst, the third wasn't
        assert_eq!(armed_before_read("window-layout-changed"), 2);
        assert_eq!(armed_before_read("window-pane-changed"), 2);
        assert_eq!(armed_before_read("pane-resized"), 1);
        // it was armed again in the next burst
        assert_eq!(
            fake.armed.iter().filter(|e| **e == "pane-resized").count(),
            2
        );
        assert!(registered_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    #[test]
    fn an_event_arms_its_waiter_again_and_prints_the_focus_only_when_it_moved() {
        let mut fake = Fake {
            script: then_all_fail(vec![
                woke("window-layout-changed"),
                Step::Quiet,
                woke("window-pane-changed"),
                Step::Quiet,
                woke("client-active"),
                Step::Quiet,
            ]),
            // at the start, then after each event
            focus: ["%0", "%0", "%3", "%3"].map(|f| Some(f.into())).into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        // where focus starts, first
        let start = r#"{"focus":"%0"}"#;
        assert_eq!(lines, [start, "", r#"{"focus":"%3"}"#, ""]);
        assert_eq!(
            fake.armed[EVENTS.len()..],
            [
                "window-layout-changed",
                "window-pane-changed",
                "client-active"
            ]
        );
    }

    #[test]
    fn a_burst_within_the_settle_prints_one_line_arming_each_waiter_again() {
        let mut fake = Fake {
            script: then_all_fail(vec![
                woke("window-layout-changed"),
                woke("window-pane-changed"),
                woke("window-layout-changed"),
                Step::Quiet,
            ]),
            focus: [None, Some("%2".into())].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, ["", r#"{"focus":"%2"}"#]);
        assert_eq!(
            fake.armed[EVENTS.len()..],
            [
                "window-layout-changed",
                "window-pane-changed",
                "window-layout-changed"
            ]
        );
    }

    #[test]
    fn a_waiter_failing_after_a_while_is_armed_again_and_prints_nothing() {
        let mut fake = Fake {
            script: then_all_fail(vec![
                failed("client-active", Duration::from_secs(30)),
                Step::Quiet,
            ]),
            ..Fake::default()
        };
        let (lines, why) = run(&mut fake);
        // the first line alone
        assert_eq!(lines, [""]);
        assert_eq!(fake.armed[EVENTS.len()..], ["client-active"]);
        // it was armed again, so one more had to fail at once
        assert!(matches!(why, Failure::Failed(_)));
    }

    #[test]
    fn once_no_client_shows_focus_a_bare_line_is_printed() {
        let mut fake = Fake {
            script: then_all_fail(vec![woke("client-detached"), Step::Quiet]),
            focus: [Some("%1".into()), None].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, [r#"{"focus":"%1"}"#, ""]);
    }

    #[test]
    fn it_ends_once_the_reader_is_gone() {
        let mut fake = Fake {
            script: vec![Step::Quiet, woke("window-linked"), Step::Quiet].into(),
            ..Fake::default()
        };
        let why = watch(&mut fake, |_| false);
        assert!(matches!(why, Failure::Failed(w) if w.contains("reader")));
    }

    #[test]
    fn it_settles_before_reading_where_focus_starts_taking_an_event_meanwhile_into_it() {
        let mut fake = Fake {
            script: {
                // what ends while the waiters register goes into the start
                let mut steps: VecDeque<Step> =
                    vec![woke("window-pane-changed"), Step::Quiet].into();
                steps.extend(then_all_fail(Vec::new()).into_iter().skip(1));
                steps
            },
            focus: [Some("%4".into())].into(),
            ..Fake::default()
        };
        let (lines, _) = run(&mut fake);
        assert_eq!(lines, [r#"{"focus":"%4"}"#]);
        assert_eq!(fake.armed[EVENTS.len()..], ["window-pane-changed"]);
        // a settle first, not a read
        let after_arming = fake.calls.iter().find(|c| !matches!(c, Call::Arm(_)));
        assert!(
            matches!(after_arming, Some(Call::Next(Some(_)))),
            "{:?}",
            fake.calls
        );
        assert!(settled_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    #[test]
    fn a_waiter_armed_again_late_in_a_burst_has_a_whole_settle_before_the_read() {
        let late = Ended {
            event: "window-layout-changed",
            woke: true,
            ran: Duration::from_secs(5),
        };
        let mut fake = Fake {
            script: then_all_fail(vec![
                woke("window-pane-changed"),
                Step::Late(Duration::from_millis(25), late),
                Step::Quiet,
            ]),
            ..Fake::default()
        };
        run(&mut fake);
        assert!(settled_before_each_read(&fake.calls), "{:?}", fake.calls);
    }

    #[test]
    fn a_listing_names_each_waiter_by_its_clients_pid() {
        assert_eq!(listed_pids(b"client-52293\nclient-52294\n"), [52293, 52294]);
        assert_eq!(listed_pids(b""), Vec::<u32>::new());
        // not a waiter's line
        assert_eq!(listed_pids(b"/dev/ttys004\nclient-x\n"), Vec::<u32>::new());
    }

    #[test]
    fn waiting_for_a_waiter_to_exit_tries_again_when_interrupted_and_gives_up_on_an_error() {
        let mut tries = 0;
        let exited = exited_unreaped(|| {
            tries += 1;
            if tries < 3 {
                Err(std::io::Error::from_raw_os_error(libc::EINTR))
            } else {
                Ok(())
            }
        });
        assert!(exited);
        assert_eq!(tries, 3);
        let exited = exited_unreaped(|| Err(std::io::Error::from_raw_os_error(libc::ECHILD)));
        assert!(!exited);
    }

    #[test]
    fn a_waiter_not_known_to_have_exited_is_released_before_its_reaped_off_the_lock() {
        let mut child = std::process::Command::new("sleep")
            .arg("0.3")
            .spawn()
            .unwrap();
        let pid = child.id();
        let running = Arc::new(Mutex::new(vec![pid, 1]));
        let theirs = Arc::clone(&running);
        let reaping = std::thread::spawn(move || reap(&mut child, &theirs, false));
        std::thread::sleep(Duration::from_millis(100));
        // it waits for its waiter without the lock, no longer listed
        let listed = running.try_lock().expect("reap holds the lock").clone();
        assert_eq!(listed, [1]);
        assert!(reaping.join().unwrap(), "sleep exits 0");
    }

    #[test]
    fn a_waiter_known_to_have_exited_is_reaped_and_unlisted() {
        let mut child = std::process::Command::new("false").spawn().unwrap();
        let pid = child.id();
        let running = Mutex::new(vec![pid]);
        assert!(exited_unreaped(|| wait_unreaped(pid)));
        assert!(!reap(&mut child, &running, true));
        assert!(running.lock().unwrap().is_empty());
    }
}
