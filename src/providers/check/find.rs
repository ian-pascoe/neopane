//! `provider check`'s `find`, run as a self-attach here would (#93
//! decision 30): the pane title `<name>-<token>` written into this
//! terminal, the chosen pane provider's `find` asked with the pane id
//! `0-<token>`, through the pane provider seam, then the old title put
//! back, or cleared where the terminal keeps none. Nothing else is asked.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use crate::attach::PaneId;
use crate::config::{Attach, Choice};
use crate::daemon::pane_helper::tty_name;
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, Outcome, PaneProvider, ProviderEvent,
};
use crate::name::NAME;
use crate::providers::command::{CommandProvider, Spec};
use crate::providers::rex::find::Verdict;
use crate::term::modes::{TITLE_POP, TITLE_PUSH};
use crate::util::{seconds, wait_readable};

/// This process's terminal: stdin, if it is one.
#[derive(Debug)]
pub(crate) struct Tty {
    pub fd: OwnedFd,
    /// Its path, such as `/dev/ttys014`.
    pub path: Vec<u8>,
    /// Its device, as the daemon reads an attach's.
    pub device: Option<u64>,
}

/// Stdin, if it is a terminal.
pub(super) fn stdin_tty() -> Option<Tty> {
    // SAFETY: isatty only inspects the fd.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
        return None;
    }
    let fd = std::io::stdin().as_fd().try_clone_to_owned().ok()?;
    let path = tty_name(fd.as_raw_fd());
    let device = crate::daemon::attach_conn::tty_device(Some(&fd), b"");
    Some(Tty { fd, path, device })
}

/// What `find` came to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Find {
    /// Not run, and why.
    NotRun(String),
    /// The pane provider has no `find`: the choice is accepted, with no id.
    Absent,
    /// A command `find` ran, with pane title `title`, for `took`.
    Ran {
        title: String,
        took: Duration,
        answer: Answered,
    },
    /// Rex's, for the Rex block `REX_BLOCK` names; `theirs` the path of the
    /// terminal of that Rex block, where the system names it.
    Rex {
        took: Duration,
        block: String,
        verdict: Verdict,
        theirs: Option<String>,
    },
}

/// How a command `find` answered (#93 decision 17's (a)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Answered {
    /// Exit 0 and a line: accepted, with that id.
    Id(String),
    /// Exit 0 and nothing: accepted, with no id.
    NoId,
    /// Exit 69, or it couldn't start: accepted, with no id.
    Unsupported,
    /// Anything else: refused, and why, as the daemon's warning says it.
    Failed(String),
    /// No answer in its time: refused.
    TimedOut(Duration),
}

impl Find {
    /// Whether it refused this terminal: no pane provider.
    pub(crate) fn refused(&self) -> bool {
        matches!(
            self,
            Find::Ran {
                answer: Answered::Failed(_) | Answered::TimedOut(_),
                ..
            } | Find::Rex {
                verdict: Verdict::Disproved(_),
                ..
            }
        )
    }

    /// The Neovim message for its refusal, by pane provider `provider`, as
    /// the daemon words it.
    pub(crate) fn refusal(&self, provider: &str) -> String {
        let why = match self {
            Find::Ran {
                answer: Answered::TimedOut(t),
                ..
            } => format!("no answer within {} s", seconds(*t)),
            Find::Ran {
                answer: Answered::Failed(why),
                ..
            }
            | Find::Rex {
                verdict: Verdict::Disproved(why),
                ..
            } => format!("find didn't confirm this terminal ({why})"),
            _ => String::new(),
        };
        format!("{NAME}: {provider}: {why}; no pane provider")
    }

    /// The terminal id it gave, if any.
    pub(crate) fn id(&self) -> Option<&str> {
        match self {
            Find::Ran {
                answer: Answered::Id(id),
                ..
            } => Some(id),
            Find::Rex { block, verdict, .. } if !matches!(verdict, Verdict::Disproved(_)) => {
                Some(block)
            }
            _ => None,
        }
    }
}

/// How an ask ended.
pub(super) enum Asked {
    Answered(Outcome),
    /// The pane provider failed as a whole, and why.
    Gone(String),
    TimedOut,
}

/// Asks `provider` to `find` this terminal, `tty`, for `attach`, with pane
/// id `pane`, as the daemon asks for a self-attach; waits for its answer
/// until `due`.
pub(super) fn ask(
    provider: &mut dyn PaneProvider,
    attach: &Attach,
    tty: &Tty,
    pane: PaneId,
    due: Instant,
) -> Asked {
    const ID: AskId = AskId(1);
    let find_vars = provider.find_vars();
    let env = attach
        .env
        .iter()
        .filter(|(k, _)| find_vars.iter().any(|v| v.as_bytes() == k.as_slice()))
        .cloned()
        .collect();
    provider.ask(
        ID,
        Ask::Find {
            pane,
            pid: attach.pid,
            tty: String::from_utf8_lossy(&tty.path).into_owned(),
            device: tty.device,
            title: crate::daemon::pane::title(&pane),
            env,
        },
    );
    let Some(fd) = provider.fd() else {
        return Asked::Gone("it can't be watched".into());
    };
    loop {
        // an answer due now comes at the next `on_ready`, at once
        provider.on_ready(0);
        while let Some(event) = provider.next() {
            match event {
                ProviderEvent::Answered { id: ID, outcome } => return Asked::Answered(outcome),
                ProviderEvent::Gone { reason } => return Asked::Gone(reason),
                _ => {}
            }
        }
        if !wait_readable(fd, due) {
            return Asked::TimedOut;
        }
    }
}

/// Runs `choice`'s command `find` for `attach`, in terminal `tty`, as the
/// daemon would for a self-attach: with the pane title written first, and
/// put back after.
pub(super) fn run(choice: &Choice, attach: &Attach, tty: &Tty) -> Find {
    let Some(mut spec) = Spec::of(choice) else {
        return Find::Absent;
    };
    // nothing else runs: no `events` either
    spec.commands.retain(|c| c.cap == Capability::Find);
    let Some(timeout) = spec.commands.first().map(|c| c.timeout) else {
        return Find::Absent;
    };
    let pane = PaneId::unnumbered();
    let title = crate::daemon::pane::title(&pane);
    let mut provider = match CommandProvider::new(spec, &attach.env, &attach.cwd) {
        Ok(p) => p,
        Err(e) => {
            return Find::Ran {
                title,
                took: Duration::ZERO,
                answer: Answered::Failed(format!("can't run it: {e}")),
            };
        }
    };
    write_to(tty, &format!("{TITLE_PUSH}\x1b]2;{title}\x1b\\"));
    let started = Instant::now();
    let asked = ask(&mut provider, attach, tty, pane, started + timeout);
    let took = started.elapsed();
    // nothing it started outlives it
    drop(provider);
    write_to(tty, TITLE_POP);
    let answer = match asked {
        Asked::Answered(Outcome::Done(Answer::Terminal(id))) => Answered::Id(id.to_string()),
        Asked::Answered(Outcome::Done(_)) => Answered::NoId,
        Asked::Answered(Outcome::Unsupported) => Answered::Unsupported,
        Asked::Answered(Outcome::Failed(why)) | Asked::Gone(why) => Answered::Failed(why),
        Asked::TimedOut => Answered::TimedOut(timeout),
    };
    Find::Ran {
        title,
        took,
        answer,
    }
}

/// Writes `text` to terminal `tty`, as ordinary output; a terminal that
/// won't take it is left as it is.
fn write_to(tty: &Tty, text: &str) {
    if let Ok(fd) = tty.fd.try_clone() {
        let mut f = std::fs::File::from(fd);
        let _ = f.write_all(text.as_bytes()).and_then(|()| f.flush());
    }
}

/// The exit status `why`, a failed run's, starts with, if any: `exit 1:
/// …`. A run that exited 0 with a bad id failed too.
pub(crate) fn exit_of(why: &str) -> Option<i32> {
    match why.strip_prefix("exit ") {
        Some(rest) => rest.split(':').next()?.parse().ok(),
        None if why.starts_with("killed by signal") => None,
        None => Some(0),
    }
}
