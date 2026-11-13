//! `provider check`'s `find`, run as a self-attach here would (#93
//! decision 30): the pane title `<name>-<token>` written into this
//! terminal, the chosen pane provider's `find` run with the pane id
//! `0-<token>` by the command runner, then the old title put back, or
//! cleared where the terminal keeps none. Nothing else is run.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use super::rex::RexVerdict;
use crate::attach::PaneId;
use crate::config::{Attach, Choice};
use crate::daemon::provider::{
    Answer, Ask, AskId, Capability, Outcome, PaneProvider, ProviderEvent,
};
use crate::name::NAME;
use crate::providers::command::{CommandProvider, Spec};
use crate::term::modes::{TITLE_POP, TITLE_PUSH};
use crate::util::seconds;

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
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for its length.
    let r = unsafe { libc::ttyname_r(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
    let path = if r == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        buf[..end].to_vec()
    } else {
        Vec::new()
    };
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
    /// Rex's, for the block `REX_BLOCK` names.
    Rex {
        took: Duration,
        block: String,
        verdict: RexVerdict,
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
                verdict: RexVerdict::Disproved(_),
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
                verdict: RexVerdict::Disproved(why),
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
            Find::Rex { block, verdict, .. } if !matches!(verdict, RexVerdict::Disproved(_)) => {
                Some(block)
            }
            _ => None,
        }
    }
}

/// Runs `choice`'s `find` for `attach`, in terminal `tty`, as the daemon
/// would for a self-attach: with the pane title written first, and put
/// back after.
pub(super) fn run(choice: &Choice, attach: &Attach, tty: &Tty) -> Find {
    let Some(mut spec) = Spec::of(choice) else {
        return Find::Absent;
    };
    // nothing else runs: no `events` either
    spec.commands.retain(|c| c.cap == Capability::Find);
    let Some(timeout) = spec.commands.first().map(|c| c.timeout) else {
        return Find::Absent;
    };
    let find_vars = spec.find_vars.clone();
    let pane = PaneId::unnumbered();
    let title = crate::daemon::pane::title(&pane);
    let started = Instant::now();
    let mut provider = match CommandProvider::new(spec, &attach.env, &attach.cwd) {
        Ok(p) => p,
        Err(e) => {
            return Find::Ran {
                title,
                took: started.elapsed(),
                answer: Answered::Failed(format!("can't run it: {e}")),
            };
        }
    };
    let env = attach
        .env
        .iter()
        .filter(|(k, _)| find_vars.iter().any(|v| v.as_bytes() == k.as_slice()))
        .cloned()
        .collect();
    let ask = Ask::Find {
        pane,
        pid: attach.pid,
        tty: String::from_utf8_lossy(&tty.path).into_owned(),
        device: tty.device,
        title: title.clone(),
        env,
    };
    write_to(tty, &format!("{TITLE_PUSH}\x1b]2;{title}\x1b\\"));
    let started = Instant::now();
    let answer = answer(&mut provider, ask, started + timeout, timeout);
    let took = started.elapsed();
    // nothing it started outlives it
    drop(provider);
    write_to(tty, TITLE_POP);
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

/// Asks `provider` `ask` and waits for its answer until `due`.
fn answer(provider: &mut dyn PaneProvider, ask: Ask, due: Instant, timeout: Duration) -> Answered {
    const ID: AskId = AskId(1);
    provider.ask(ID, ask);
    let Some(fd) = provider.fd() else {
        return Answered::Failed("it can't be watched".into());
    };
    loop {
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Answered::TimedOut(timeout);
        }
        let mut fds = [libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        let ms = left.as_millis().clamp(1, 1000) as libc::c_int;
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        provider.on_ready(0);
        while let Some(event) = provider.next() {
            let ProviderEvent::Answered { id: ID, outcome } = event else {
                continue;
            };
            return match outcome {
                Outcome::Done(Answer::Terminal(id)) => Answered::Id(id.to_string()),
                Outcome::Done(_) => Answered::NoId,
                Outcome::Unsupported => Answered::Unsupported,
                Outcome::Failed(why) => Answered::Failed(why),
            };
        }
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
