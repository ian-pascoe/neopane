//! The command line, parsed by hand (#94 decisions 1-12, ADR 0014): our own
//! options first, then Neovim's arguments, unchanged. Only the first argument
//! can be a command word. Nothing here touches the file system or a process.

mod help;
mod nvim_args;

use std::ffi::{OsStr, OsString};

use crate::attach::{PaneId, Transport};
use crate::name::NAME;

pub use help::{PANE_PROVIDER_PROTOCOLS, debug_help, help, repo_url, version};
pub use nvim_args::Reading;

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// The bare form: start a daemon and show its Neovim here.
    Start(Start),
    /// `-h`, `--help`, `help`, `COMMAND -h`: the help text on stdout.
    Help,
    /// `help debug`, `debug-… -h`: the developer tools on stdout.
    DebugHelp,
    /// `-v`, `--version`, `version`.
    Version,
    /// A command whose handler comes later: it exits 1 with
    /// `<name>: <what> isn't built yet`.
    NotBuilt(String),
    /// `attach --address A --pane ID [--transport fd|relay]`: a requested
    /// pane's helper, which the daemon's attach command runs.
    AttachPane {
        address: OsString,
        pane: PaneId,
        transport: Transport,
    },
    /// `pane`, which older daemons ran: it exits 1 with [`PANE_REPLACED`].
    Replaced,
    /// `daemon DIR NVIM-ARGS…`, which the launcher starts.
    Daemon {
        dir: OsString,
        nvim_args: Vec<OsString>,
    },
    /// `config-child`, which the daemon starts.
    ConfigChild,
    DebugKeys,
    DebugEvents(Vec<OsString>),
    DebugTui(Vec<OsString>),
    DebugPanes(OsString),
    DebugRex(OsString),
    /// Hidden: for `docs/latency.md`.
    DebugLatency(OsString),
}

/// The bare form's own options, and Neovim's arguments as typed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Start {
    pub name: Option<OsString>,
    pub address: Option<OsString>,
    pub provider: Option<OsString>,
    pub detach: bool,
    pub no_wait: bool,
    pub nvim_args: Vec<OsString>,
    /// What reading `nvim_args` found.
    pub reading: Reading,
}

/// A command line that can't be run: exit 2. Displayed as one line,
/// `<name>: <why> (see <name> --help)`.
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{NAME}: {} (see {NAME} --help)", self.0)
    }
}

impl std::error::Error for UsageError {}

/// What `pane` exits 1 with, after `<name>: `.
pub const PANE_REPLACED: &str =
    "pane was replaced by attach --pane (this came from an older daemon)";

/// The command words a person types. Each runs its command even when a file
/// of that name exists.
pub const COMMAND_WORDS: [&str; 7] = [
    "attach", "list", "status", "kill", "provider", "help", "version",
];

/// Parses the arguments after the program's own name.
pub fn parse(args: &[OsString]) -> Result<Command, UsageError> {
    let Some(first) = args.first() else {
        return Ok(Command::Start(Start::default()));
    };
    let rest = &args[1..];
    let word = first.as_encoded_bytes();
    let asks_help = |words: &[OsString]| words.iter().any(|w| is_help(w));
    let debug_asks_help = rest.first().is_some_and(|w| is_help(w));
    let one = |what: &str| match rest {
        [only] => Ok(only.clone()),
        _ => Err(UsageError(format!(
            "{what} takes one {}",
            what_it_takes(what)
        ))),
    };
    match word {
        b"attach" | b"list" | b"status" | b"kill" if asks_help(rest) => Ok(Command::Help),
        b"attach" => attach(rest),
        b"list" | b"status" | b"kill" => {
            Ok(Command::NotBuilt(first.to_string_lossy().into_owned()))
        }
        b"provider" if asks_help(rest) => Ok(Command::Help),
        b"provider" => match rest.first().map(|w| w.as_encoded_bytes()) {
            Some(sub @ (b"check" | b"show" | b"types")) => Ok(Command::NotBuilt(format!(
                "provider {}",
                String::from_utf8_lossy(sub)
            ))),
            _ => Err(UsageError(
                "provider takes check, show or types".to_string(),
            )),
        },
        b"help" => match rest {
            [] => Ok(Command::Help),
            [w] if w == "debug" => Ok(Command::DebugHelp),
            _ if asks_help(rest) => Ok(Command::Help),
            _ => Err(UsageError("help takes only debug".to_string())),
        },
        b"version" if asks_help(rest) => Ok(Command::Help),
        b"version" if rest.is_empty() => Ok(Command::Version),
        b"version" => Err(UsageError("version takes no arguments".to_string())),
        b"detach" => Err(UsageError(format!(
            "detach isn't available yet; to open a file named detach, run {NAME} ./detach"
        ))),
        b"pane" => Ok(Command::Replaced),
        b"daemon" => match rest {
            [dir, nvim_args @ ..] => Ok(Command::Daemon {
                dir: dir.clone(),
                nvim_args: nvim_args.to_vec(),
            }),
            [] => Err(UsageError("daemon takes a directory".to_string())),
        },
        b"config-child" => Ok(Command::ConfigChild),
        b"debug-keys" | b"debug-events" | b"debug-tui" | b"debug-panes" | b"debug-rex"
            if debug_asks_help =>
        {
            Ok(Command::DebugHelp)
        }
        b"debug-keys" if rest.is_empty() => Ok(Command::DebugKeys),
        b"debug-keys" => Err(UsageError("debug-keys takes no arguments".to_string())),
        b"debug-events" => Ok(Command::DebugEvents(rest.to_vec())),
        b"debug-tui" => Ok(Command::DebugTui(rest.to_vec())),
        b"debug-panes" => one("debug-panes").map(Command::DebugPanes),
        b"debug-rex" => one("debug-rex").map(Command::DebugRex),
        b"debug-latency" => one("debug-latency").map(Command::DebugLatency),
        _ => start(args),
    }
}

/// The note for a command word that names a file too, given whether a file
/// or directory of that name exists.
pub fn file_note(args: &[OsString], exists: impl FnOnce(&OsStr) -> bool) -> Option<String> {
    let first = args.first()?;
    let word = COMMAND_WORDS.into_iter().find(|w| first == *w)?;
    exists(first)
        .then(|| format!("{NAME}: {word} is a command; to open the file, run {NAME} ./{word}"))
}

/// What `what`'s one argument is called in its error.
fn what_it_takes(what: &str) -> &'static str {
    match what {
        "debug-latency" => "file",
        _ => "socket",
    }
}

fn is_help(word: &OsStr) -> bool {
    word == "-h" || word == "--help"
}

/// The bare form: our options from the front, then Neovim's arguments.
fn start(args: &[OsString]) -> Result<Command, UsageError> {
    let mut start = Start::default();
    let mut i = 0;
    while let Some(word) = args.get(i) {
        let bytes = word.as_encoded_bytes();
        match bytes {
            b"-h" | b"--help" => return Ok(Command::Help),
            b"-v" | b"--version" => return Ok(Command::Version),
            b"--detach" => start.detach = true,
            b"--no-wait" => start.no_wait = true,
            _ => {
                let Some((option, slot)) = [
                    ("--name", &mut start.name),
                    ("--address", &mut start.address),
                    ("--provider", &mut start.provider),
                ]
                .into_iter()
                .find(|(option, _)| {
                    bytes
                        .strip_prefix(option.as_bytes())
                        .is_some_and(|r| r.is_empty() || r[0] == b'=')
                }) else {
                    break;
                };
                let value = if bytes.len() == option.len() {
                    i += 1;
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| UsageError(format!("{option} needs a value")))?
                } else {
                    let value = &bytes[option.len() + 1..];
                    // SAFETY: split right after an ASCII `=`, from an OsString.
                    unsafe { OsStr::from_encoded_bytes_unchecked(value) }.to_os_string()
                };
                if slot.replace(value).is_some() {
                    return Err(UsageError(format!("{option} is given twice")));
                }
            }
        }
        i += 1;
    }
    if start.name.is_some() && start.address.is_some() {
        return Err(UsageError(
            "--name and --address both choose a daemon: give one".to_string(),
        ));
    }
    start.nvim_args = args[i..].to_vec();
    start.reading = nvim_args::read(&start.nvim_args).map_err(UsageError)?;
    Ok(Command::Start(start))
}

/// `attach`'s words. Only the hidden `--pane` form is built yet.
fn attach(words: &[OsString]) -> Result<Command, UsageError> {
    let is_pane = |w: &OsString| {
        let w = w.as_encoded_bytes();
        w == b"--pane" || w.starts_with(b"--pane=")
    };
    if !words.iter().any(is_pane) {
        return Ok(Command::NotBuilt("attach".to_string()));
    }
    let only =
        || UsageError("attach --pane takes only --address A and --transport fd|relay".into());
    let (mut address, mut pane, mut transport) = (None, None, None);
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let bytes = word.as_encoded_bytes();
        let Some(option) = ["--address", "--pane", "--transport"]
            .into_iter()
            .find(|o| {
                bytes
                    .strip_prefix(o.as_bytes())
                    .is_some_and(|r| r.is_empty() || r[0] == b'=')
            })
        else {
            return Err(only());
        };
        let value = if bytes.len() == option.len() {
            i += 1;
            words
                .get(i)
                .cloned()
                .ok_or_else(|| UsageError(format!("{option} needs a value")))?
        } else {
            // SAFETY: split right after an ASCII `=`, from an OsString.
            unsafe { OsStr::from_encoded_bytes_unchecked(&bytes[option.len() + 1..]) }
                .to_os_string()
        };
        let twice = || UsageError(format!("{option} is given twice"));
        let text = value.to_str();
        match option {
            "--address" if address.is_some() => return Err(twice()),
            "--address" => address = Some(value),
            "--pane" if pane.is_some() => return Err(twice()),
            "--pane" => {
                pane = Some(text.and_then(PaneId::parse).ok_or_else(|| {
                    UsageError(format!(
                        "--pane: {} isn't a pane id",
                        value.to_string_lossy()
                    ))
                })?)
            }
            _ if transport.is_some() => return Err(twice()),
            _ => {
                transport = Some(
                    text.and_then(Transport::from_name)
                        .ok_or_else(|| UsageError("--transport takes fd or relay".into()))?,
                )
            }
        }
        i += 1;
    }
    let (Some(address), Some(pane)) = (address, pane) else {
        return Err(UsageError("attach --pane needs --address A".into()));
    };
    Ok(Command::AttachPane {
        address,
        pane,
        transport: transport.unwrap_or(Transport::Fd),
    })
}

#[cfg(test)]
mod tests;
