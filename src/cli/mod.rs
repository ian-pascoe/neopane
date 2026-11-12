//! The command line, parsed by hand (#94 decisions 1-12, ADR 0014): our own
//! options first, then Neovim's arguments, unchanged. Only the first argument
//! can be a command word. Nothing here touches the file system or a process.

mod help;
mod nvim_args;

use std::ffi::{OsStr, OsString};

use crate::attach::{PaneId, Transport};
use crate::daemon::dir::valid_name;
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
    /// `provider show [NAME]`: a built-in pane provider as Lua, or with no
    /// NAME, the built-ins.
    ProviderShow(Option<String>),
    /// `provider types [--config FILE]`: the config's Lua types.
    ProviderTypes {
        config: Option<OsString>,
    },
    /// `attach --address A --pane ID [--transport fd|relay]`: a requested
    /// pane's helper, which the daemon's attach command runs. Without
    /// `--transport`, `<NAME>_TRANSPORT` chooses.
    AttachPane {
        address: OsString,
        pane: PaneId,
        transport: Option<Transport>,
    },
    /// `pane`, which older daemons ran: it exits 1 with [`PANE_REPLACED`].
    Replaced,
    /// `daemon NAME [--dedup] [--detach] -- NVIM-ARGS…`, which the launcher
    /// starts.
    Daemon {
        name: String,
        /// Take `NAME-2`, `NAME-3`, … if NAME is live.
        dedup: bool,
        detach: bool,
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
    /// Hidden: `debug-relay {attach}`, the relay test tool, which attaches
    /// a requested pane over the relay with no pty, on stdin and stdout.
    DebugRelay {
        address: OsString,
        pane: PaneId,
    },
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
    // a -h after -- is a file
    let asks_help =
        |words: &[OsString]| words.iter().take_while(|w| *w != "--").any(|w| is_help(w));
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
            Some(b"check") => Ok(Command::NotBuilt("provider check".to_string())),
            Some(b"show") => provider_show(&rest[1..]),
            Some(b"types") => provider_types(&rest[1..]),
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
        b"daemon" => daemon(rest),
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
        b"debug-relay" => debug_relay(rest),
        _ => start(args),
    }
}

/// The note for a command word that names a file too, given what `args`
/// parsed to and whether a file or directory of that name exists. The
/// daemon's own `attach --pane` gets none.
pub fn file_note(
    args: &[OsString],
    parsed: &Result<Command, UsageError>,
    exists: impl FnOnce(&OsStr) -> bool,
) -> Option<String> {
    if matches!(parsed, Ok(Command::AttachPane { .. })) {
        return None;
    }
    let first = args.first()?;
    let word = COMMAND_WORDS.into_iter().find(|w| first == *w)?;
    exists(first)
        .then(|| format!("{NAME}: {word} is a command; to open the file, run {NAME} ./{word}"))
}

/// The hidden commands only `<name>` itself starts: refused when a person
/// typed them, which `stdout_is_terminal` tells. Checked before [`parse`],
/// so a person gets this whatever follows the word.
pub fn started_by_us(args: &[OsString], stdout_is_terminal: bool) -> Result<(), UsageError> {
    if !stdout_is_terminal {
        return Ok(());
    }
    match args.first().map(|w| w.as_encoded_bytes()) {
        Some(b"daemon") => Err(UsageError(format!(
            "daemon is started by {NAME} itself; to start one without a terminal, run {NAME} \
             --detach"
        ))),
        Some(b"config-child") => Err(UsageError(format!(
            "config-child is started by {NAME} itself"
        ))),
        _ => Ok(()),
    }
}

/// `word` as one of the long `options`, as `--opt` or `--opt=VALUE`: the
/// option, and its attached value if any.
fn long_option<'a>(word: &OsStr, options: &[&'a str]) -> Option<(&'a str, Option<OsString>)> {
    let bytes = word.as_encoded_bytes();
    options.iter().find_map(|&option| {
        let rest = bytes.strip_prefix(option.as_bytes())?;
        match rest {
            [] => Some((option, None)),
            // SAFETY: split right after an ASCII `=`, from an OsStr.
            [b'=', value @ ..] => Some((
                option,
                Some(unsafe { OsStr::from_encoded_bytes_unchecked(value) }.to_os_string()),
            )),
            _ => None,
        }
    })
}

/// The value of `option` in `words[*i]`: attached, or else the next word,
/// which `i` then moves to.
fn option_value(
    option: &str,
    attached: Option<OsString>,
    words: &[OsString],
    i: &mut usize,
) -> Result<OsString, UsageError> {
    if let Some(value) = attached {
        return Ok(value);
    }
    *i += 1;
    words
        .get(*i)
        .cloned()
        .ok_or_else(|| UsageError(format!("{option} needs a value")))
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
                if let Some((flag, Some(_))) = long_option(word, &["--detach", "--no-wait"]) {
                    return Err(UsageError(format!("{flag} takes no value")));
                }
                let Some((option, attached)) =
                    long_option(word, &["--name", "--address", "--provider"])
                else {
                    break;
                };
                let value = option_value(option, attached, args, &mut i)?;
                let slot = match option {
                    "--name" => &mut start.name,
                    "--address" => &mut start.address,
                    _ => &mut start.provider,
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

/// `provider show`'s words: `[NAME]`.
fn provider_show(words: &[OsString]) -> Result<Command, UsageError> {
    match words {
        [] => Ok(Command::ProviderShow(None)),
        [name] if !name.as_encoded_bytes().starts_with(b"-") => Ok(Command::ProviderShow(Some(
            name.to_string_lossy().into_owned(),
        ))),
        _ => Err(UsageError("provider show takes at most one NAME".into())),
    }
}

/// `provider types`' words: `[--config FILE]`.
fn provider_types(words: &[OsString]) -> Result<Command, UsageError> {
    let mut config = None;
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let Some((option, attached)) = long_option(word, &["--config"]) else {
            return Err(UsageError("provider types takes only --config FILE".into()));
        };
        let value = option_value(option, attached, words, &mut i)?;
        if value.is_empty() {
            return Err(UsageError(format!("{option} needs a value")));
        }
        if config.replace(value).is_some() {
            return Err(UsageError(format!("{option} is given twice")));
        }
        i += 1;
    }
    Ok(Command::ProviderTypes { config })
}

/// `debug-relay`'s words: a requested pane's `{attach}`, its exe first.
fn debug_relay(words: &[OsString]) -> Result<Command, UsageError> {
    let takes = || UsageError("debug-relay takes a requested pane's attach command".into());
    let [_exe, attach_word, rest @ ..] = words else {
        return Err(takes());
    };
    if attach_word != "attach" {
        return Err(takes());
    }
    match attach(rest) {
        Ok(Command::AttachPane { address, pane, .. }) => Ok(Command::DebugRelay { address, pane }),
        Ok(_) => Err(takes()),
        Err(e) => Err(e),
    }
}

/// `daemon`'s words: `NAME [--dedup] [--detach] -- NVIM-ARGS`.
fn daemon(words: &[OsString]) -> Result<Command, UsageError> {
    let usage = || UsageError("daemon takes NAME [--dedup] [--detach] -- NVIM-ARGS".into());
    let [name, rest @ ..] = words else {
        return Err(usage());
    };
    let name = name
        .to_str()
        .filter(|n| valid_name(OsStr::new(n)))
        .ok_or_else(|| {
            UsageError(format!(
                "daemon: {} isn't a daemon name",
                name.to_string_lossy()
            ))
        })?;
    let (mut dedup, mut detach) = (false, false);
    let Some(end) = rest.iter().position(|w| w == "--") else {
        return Err(usage());
    };
    for word in &rest[..end] {
        match word.as_encoded_bytes() {
            b"--dedup" if !dedup => dedup = true,
            b"--detach" if !detach => detach = true,
            _ => return Err(usage()),
        }
    }
    Ok(Command::Daemon {
        name: name.to_string(),
        dedup,
        detach,
        nvim_args: rest[end + 1..].to_vec(),
    })
}

/// `attach`'s words. Only the hidden `--pane` form is built yet.
fn attach(words: &[OsString]) -> Result<Command, UsageError> {
    if !words.iter().any(|w| long_option(w, &["--pane"]).is_some()) {
        return Ok(Command::NotBuilt("attach".to_string()));
    }
    let only =
        || UsageError("attach --pane takes only --address A and --transport fd|relay".into());
    let (mut address, mut pane, mut transport) = (None, None, None);
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let Some((option, attached)) = long_option(word, &["--address", "--pane", "--transport"])
        else {
            return Err(only());
        };
        let value = option_value(option, attached, words, &mut i)?;
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
        transport,
    })
}

#[cfg(test)]
mod tests;
