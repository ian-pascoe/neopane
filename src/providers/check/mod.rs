//! `provider check` (#93 decision 30, #94 decision 33): the user's config
//! found and run as the daemon would, in the same child, with the same
//! checker and 3 s, for an attach built from this shell; then the chosen
//! pane provider's `find`, run as a self-attach here would, and nothing
//! else it can do. What it all came to is a [`Check`], which `report.rs`
//! prints as text or JSON.
//!
//! - The file is `--config FILE`, else where the daemon would look, and the
//!   hint `--provider P`, else `$<NAME>_PROVIDER`: `none` skips the file.
//! - A command `find` gets the pane title `<name>-<token>`, written into
//!   this terminal first and put back after, and the pane id `0-<token>`
//!   (`find.rs`). Rex's needs no title; its section has the server's
//!   version and API revision, and what its `find` proved (`rex.rs`).
//! - With stdin not a terminal, or `--no-find`, `find` doesn't run.
//!
//! Exit 0 for a pane provider, or none by design; 1 for an error or `find`
//! refusing this terminal. Warnings don't change it.

mod find;
mod report;
mod rex;

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::choose::{self, ChooseBy, ConfigFile, HINT_NONE};
use super::command::commands_env;
use crate::attach::env_value;
use crate::config::check::Severity;
use crate::config::wire::{Called, Ran as LuaRan};
use crate::config::{self, Attach, Builtin, Choice, Outcome, Problems};
use crate::name::{NAME, upper};

pub(crate) use find::Find;

/// What `provider check` prints, and its exit status.
pub struct Exit {
    pub out: String,
    pub status: u8,
}

/// `provider check`'s options, as the command line gave them.
#[derive(Debug, Default)]
pub struct Options {
    /// `--config FILE`, as typed.
    pub config: Option<OsString>,
    /// `--provider P`: `""` is no hint; left out, `$<NAME>_PROVIDER`.
    pub provider: Option<OsString>,
    /// `--daemon-name N`.
    pub daemon_name: Option<String>,
    pub no_find: bool,
    pub full: bool,
    pub json: bool,
}

/// Runs `provider check` with `options`, in this process's terminal.
pub fn run(options: &Options) -> Exit {
    // the pane provider's log lines are the daemon's: only warnings here
    crate::log::set_level(crate::log::LogLevel::Warn);
    let check = gather(options);
    let out = if options.json {
        report::json(&check)
    } else {
        report::text(&check, options.full)
    };
    Exit {
        out,
        status: u8::from(check.result.is_error()),
    }
}

/// What `provider check` found.
#[derive(Debug)]
pub(crate) struct Check {
    pub config: ConfigState,
    pub attach: Attach,
    /// The config's run, if it ran.
    pub ran: Option<Ran>,
    /// What was chosen and how; None when the config failed.
    pub chose: Option<(Choice, How)>,
    /// Every problem found, errors and warnings, check-only ones too.
    pub problems: Problems,
    pub find: Find,
    /// For `"rex"`.
    pub rex: Option<rex::RexReport>,
    pub result: Verdict,
}

/// Where the config is, or why there's none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigState {
    File(PathBuf),
    /// None at the default places: `<name>.default` chooses.
    NoFile,
    /// The hint is `none`.
    Skipped,
}

/// The config's run.
#[derive(Debug)]
pub(crate) struct Ran {
    pub took: Duration,
    pub printed: Vec<String>,
    pub printed_cut: bool,
    /// The Lua error, the timeout, or the ended-early message.
    pub error: Option<String>,
}

/// What made the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum How {
    /// The config's `provider()`.
    Provider,
    /// `<name>.default`, with no file.
    Default,
    /// Nothing: the hint is `none`.
    Hint,
}

/// What it all came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// A pane provider; `find_ran` false when `find` wasn't run.
    Ok {
        find_ran: bool,
        /// The Neovim message the warnings make, if any.
        warning: Option<String>,
    },
    /// None, by design.
    None { warning: Option<String> },
    /// No pane provider for an error, or `find` refusing: the Neovim
    /// message.
    Error(String),
}

impl Verdict {
    fn is_error(&self) -> bool {
        matches!(self, Verdict::Error(_))
    }
}

/// Gathers what `provider check` reports, in this process's terminal.
fn gather(o: &Options) -> Check {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let env = crate::daemon::pane_helper::environment();
    // "" is no hint; left out, the variable's, empty counting as unset
    let hint = match &o.provider {
        Some(given) => Some(given.as_bytes().to_vec()),
        None => env_value(&env, &format!("{}_PROVIDER", upper())).map(<[u8]>::to_vec),
    }
    .filter(|h| !h.is_empty())
    .map(|h| String::from_utf8_lossy(&h).into_owned());
    let tty = find::stdin_tty();
    let attach = Attach {
        env: env.clone(),
        cwd: cwd.as_os_str().as_bytes().to_vec(),
        tty: tty.as_ref().map(|t| t.path.clone()).unwrap_or_default(),
        pid: std::process::id(),
        daemon: o
            .daemon_name
            .clone()
            .unwrap_or_else(|| crate::daemon::dir::clean_name(cwd.file_name().unwrap_or_default())),
        provider: hint.clone().filter(|h| h != HINT_NONE),
    };
    let file = match &o.config {
        Some(f) => Some(ConfigFile {
            path: cwd.join(f),
            named: true,
        }),
        None => {
            let os: Vec<(OsString, OsString)> = env
                .iter()
                .map(|(k, v)| (OsString::from_vec(k.clone()), OsString::from_vec(v.clone())))
                .collect();
            choose::config_path(&os, &cwd)
        }
    };
    let mut check = Check {
        config: ConfigState::NoFile,
        attach,
        ran: None,
        chose: None,
        problems: Problems::default(),
        find: Find::NotRun("the config failed".into()),
        rex: None,
        result: Verdict::None { warning: None },
    };
    match choose::choose_by(file.as_ref(), hint.as_deref()) {
        ChooseBy::Default if hint.as_deref() == Some(HINT_NONE) => {
            check.config = ConfigState::Skipped;
            check.chose = Some((Choice::None, How::Hint));
        }
        ChooseBy::Default => {
            let choice = choose::default_choice(&check.attach.env);
            check.chose = Some((choice, How::Default));
        }
        ChooseBy::Missing(path) => {
            check.config = ConfigState::File(path.to_path_buf());
            let line = match o.config {
                Some(_) => choose::missing_line_named_by(path, "--config"),
                None => choose::missing_line(path),
            };
            let text = line.strip_prefix("config: ").unwrap_or(&line).to_string();
            check.problems = config::check::failed(text);
            check.result = Verdict::Error(format!("{NAME}: {line}"));
        }
        ChooseBy::Config(path) => {
            check.config = ConfigState::File(path.to_path_buf());
            run_config(&mut check, path);
        }
    }
    if let Some((choice, _)) = &check.chose {
        let choice = choice.clone();
        decide(&mut check, &choice, o.no_find, tty.as_ref());
    }
    check
}

/// Runs config `file` in the config child, as the daemon would, and checks
/// what it answered.
fn run_config(check: &mut Check, file: &Path) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(NAME));
    let env: Vec<(OsString, OsString)> = commands_env(&check.attach.env)
        .into_iter()
        .map(|(k, v)| (OsString::from_vec(k), OsString::from_vec(v)))
        .collect();
    let request = config::Request {
        file: file.as_os_str().as_bytes().to_vec(),
        version: env!("CARGO_PKG_VERSION").into(),
        attach: check.attach.clone(),
        time: config::TIME,
    };
    let spawn = config::Spawn {
        exe: &exe,
        env: &env,
        group: None,
    };
    let outcome = config::run(spawn, &request);
    let file_name = file
        .file_name()
        .map_or_else(|| config::FILE_NAME.into(), |n| n.to_string_lossy());
    check.ran = Some(Ran {
        took: outcome.took,
        printed: outcome.printed.clone(),
        printed_cut: outcome.printed_cut,
        error: run_error(&outcome, &file_name),
    });
    match config::choose(&outcome, &file_name) {
        Ok(chosen) => {
            check.problems = chosen.warnings;
            check.chose = Some((chosen.choice, How::Provider));
        }
        Err(problems) => {
            let message = problems.message().unwrap_or_default();
            check.problems = problems;
            check.result = Verdict::Error(message);
        }
    }
}

/// Why the config's run gave no choice, as Lua or the runner said.
fn run_error(outcome: &Outcome, file: &str) -> Option<String> {
    match &outcome.answer {
        Err(failure) => Some(failure.message(file)),
        Ok(answer) => match &answer.ran {
            LuaRan::CantRead(e) => Some(format!("can't read {file}: {e}")),
            LuaRan::LoadFailed(e) | LuaRan::Failed(e) => Some(e.clone()),
            LuaRan::Returned {
                provider: Called::Raised(e),
                ..
            } => Some(e.clone()),
            _ => None,
        },
    }
}

/// Why `find` isn't run without a terminal.
const NO_TERMINAL: &str =
    "stdin isn't a terminal, so there's no title to write and nothing to find";

/// What the choice came to: `find` run, unless `no_find` or stdin isn't a
/// terminal, and Rex asked what it is.
fn decide(check: &mut Check, choice: &Choice, no_find: bool, tty: Option<&find::Tty>) {
    let warning = check
        .problems
        .0
        .iter()
        .any(|p| p.severity != Severity::CheckOnly)
        .then(|| check.problems.message())
        .flatten();
    let not_run = if no_find {
        Some("--no-find".to_string())
    } else if tty.is_none() {
        Some(NO_TERMINAL.to_string())
    } else {
        None
    };
    match choice {
        Choice::None => {
            check.find = Find::NotRun("no pane provider".into());
            check.result = Verdict::None { warning };
        }
        Choice::Builtin(Builtin::Rex) => {
            let finding = match (&not_run, tty) {
                (None, Some(tty)) => Some(tty),
                _ => None,
            };
            let (report, found) = rex::probe(&check.attach, finding);
            check.result = match (&report.error, &found) {
                (Some(why), _) => Verdict::Error(format!("{NAME}: rex: {why}; no pane provider")),
                (None, Some(f)) if f.refused() => Verdict::Error(f.refusal("rex")),
                (None, f) => Verdict::Ok {
                    find_ran: f.is_some(),
                    warning,
                },
            };
            check.find = found.unwrap_or_else(|| {
                Find::NotRun(not_run.unwrap_or_else(|| "Rex couldn't be asked".into()))
            });
            check.rex = Some(report);
        }
        Choice::Whole(config::Provider { name, .. }) | Choice::Pair(config::Pair { name, .. }) => {
            let found = match (not_run, tty) {
                (None, Some(tty)) => find::run(choice, &check.attach, tty),
                (why, _) => Find::NotRun(why.unwrap_or_else(|| NO_TERMINAL.into())),
            };
            check.result = if found.refused() {
                Verdict::Error(found.refusal(name))
            } else {
                Verdict::Ok {
                    find_ran: !matches!(found, Find::NotRun(_)),
                    warning,
                }
            };
            check.find = found;
        }
    }
}
