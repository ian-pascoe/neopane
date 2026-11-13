//! The command line, parsed by hand (#94 decisions 1-12, ADR 0014): our own
//! options first, then Neovim's arguments, unchanged. Only the first argument
//! can be a command word. Nothing here touches the file system or a process.

mod help;
mod nvim_args;
pub mod report;

use std::ffi::{OsStr, OsString};

use crate::attach::{OpenFile, PaneId, Transport, Wait};
use crate::daemon::dir::{Address, valid_name};
use crate::name::NAME;

pub use crate::daemon::find::Target;
pub use crate::name::repo_url;
pub use help::{PANE_PROVIDER_PROTOCOLS, debug_help, help, version};
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
    /// `list [--json]`: every running daemon (#94 decisions 29 and 30).
    List {
        json: bool,
    },
    /// `status [--name N | --address A] [--json]`: one daemon (#94
    /// decision 31).
    Status {
        to: Target,
        json: bool,
    },
    /// `kill [--name N | --address A | --all] [--force]` (#94 decision 32).
    Kill {
        to: KillTarget,
        force: bool,
    },
    /// `provider check [--config FILE] [--provider P] [--daemon-name N]
    /// [--no-find] [--full] [--json]` (#93 decision 30, #94 decision 33).
    ProviderCheck(ProviderCheck),
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
    /// `attach [--name N | --address A] [--provider P] [+LINE] [--] [FILE]`,
    /// and the bare `--address A`: shows a running daemon in this terminal,
    /// with FILE in a new window, the cursor on LINE.
    AttachSelf(AttachSelf),
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

/// `provider check`'s options.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ProviderCheck {
    /// `--config FILE`, as typed.
    pub config: Option<OsString>,
    /// `--provider P`: `""` is no hint; left out, `$<NAME>_PROVIDER`.
    pub provider: Option<OsString>,
    /// `--daemon-name N`, a valid daemon name.
    pub daemon_name: Option<String>,
    pub no_find: bool,
    pub full: bool,
    pub json: bool,
}

/// The daemons `kill` ends.
#[derive(Debug, PartialEq, Eq)]
pub enum KillTarget {
    /// One, found as `attach` finds it.
    One(Target),
    /// `--all`: every one running.
    All,
}

/// A self-attach to a running daemon.
#[derive(Debug, PartialEq, Eq)]
pub struct AttachSelf {
    pub to: Target,
    /// `--provider P`, for the hello.
    pub provider: Option<OsString>,
    /// As typed: the helper makes it absolute.
    pub file: Option<OsString>,
    pub line: Option<u32>,
}

/// The bare form's own options, and Neovim's arguments as typed.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Start {
    /// `--name N`, a valid daemon name.
    pub name: Option<String>,
    pub address: Option<OsString>,
    pub provider: Option<OsString>,
    pub detach: bool,
    pub no_wait: bool,
    pub nvim_args: Vec<OsString>,
    /// What reading `nvim_args` found.
    pub reading: Reading,
}

impl Start {
    /// The bare `--name N` with N running: `attach --name N`, as long as
    /// Neovim's arguments are only `[+LINE] [--] [FILE]` (#94 decision 15).
    pub fn attach_to_running(&self, name: &str) -> Result<AttachSelf, UsageError> {
        let (file, line) = attach_words(&self.nvim_args).map_err(|Misfit(word)| {
            UsageError(format!(
                "{name} is running, so it takes only [+LINE] FILE; {} is for starting a \
                 daemon (open windows from Neovim)",
                word.to_string_lossy()
            ))
        })?;
        Ok(AttachSelf {
            to: Target::Name(name.to_string()),
            provider: self.provider.clone(),
            file,
            line,
        })
    }

    /// The bare `--address A`, kept by `--no-wait`: `attach --address A`
    /// (#94 decision 16). None without `--address`.
    pub fn attach_to_address(&self) -> Result<Option<AttachSelf>, UsageError> {
        let Some(address) = &self.address else {
            return Ok(None);
        };
        let (file, line) = attach_words(&self.nvim_args).map_err(|_| attach_takes())?;
        Ok(Some(AttachSelf {
            to: Target::Address(Address::from_arg(address.clone())),
            provider: self.provider.clone(),
            file,
            line,
        }))
    }
}

/// What `<name> [ARGS]` opens inside daemon `daemon`'s own Neovim (#94
/// decision 17), waiting as `wait` says: `[+LINE] [--] FILE`, the file as
/// typed; None for nothing.
pub fn inside_file(
    words: &[OsString],
    daemon: &str,
    wait: Wait,
) -> Result<Option<OpenFile>, UsageError> {
    let only = || {
        UsageError(format!(
            "inside {daemon}'s Neovim, only [+LINE] FILE can be opened; use :tabedit, :vsplit \
             or :source there"
        ))
    };
    match attach_words(words).map_err(|_| only())? {
        (Some(file), line) => Ok(Some(OpenFile {
            file: file.into_encoded_bytes(),
            line,
            wait,
        })),
        (None, _) => Ok(None),
    }
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
        b"list" => list(rest),
        b"status" => status(rest),
        b"kill" => kill(rest),
        b"provider" if asks_help(rest) => Ok(Command::Help),
        b"provider" => match rest.first().map(|w| w.as_encoded_bytes()) {
            Some(b"check") => provider_check(&rest[1..]),
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
    let mut name = None;
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
                    "--name" => &mut name,
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
    start.name = name.as_deref().map(daemon_name).transpose()?;
    if start.name.is_some() && start.address.is_some() {
        return Err(both_choose());
    }
    if start.detach && start.provider.is_some() {
        return Err(UsageError(
            "--provider is for a terminal that chooses the pane provider, and --detach shows \
             none"
                .to_string(),
        ));
    }
    start.nvim_args = args[i..].to_vec();
    start.reading = nvim_args::read(&start.nvim_args).map_err(UsageError)?;
    let Some(address) = start.address.take() else {
        return Ok(Command::Start(start));
    };
    // exactly `attach --address A` (#94 decision 16)
    if start.detach {
        return Err(UsageError(
            "--address attaches to a running daemon, and --detach starts one: give one".into(),
        ));
    }
    if start.no_wait {
        start.address = Some(address);
        return Ok(Command::Start(start));
    }
    let (file, line) = attach_words(&start.nvim_args).map_err(|_| attach_takes())?;
    Ok(Command::AttachSelf(AttachSelf {
        to: Target::Address(Address::from_arg(address)),
        provider: start.provider,
        file,
        line,
    }))
}

/// `--name`'s value, checked: a daemon name (#94 decision 15).
fn daemon_name(value: &OsStr) -> Result<String, UsageError> {
    value
        .to_str()
        .filter(|n| valid_name(OsStr::new(n)))
        .map(str::to_string)
        .ok_or_else(|| UsageError("--name: use 1-32 letters, digits, '.', '_' or '-'".into()))
}

fn both_choose() -> UsageError {
    UsageError("--name and --address both choose a daemon: give one".to_string())
}

/// The word that doesn't fit a self-attach's `[+LINE] [--] [FILE]`: an
/// option, a second file or `+LINE`, or a `+LINE` with no file.
#[derive(Debug, PartialEq, Eq)]
struct Misfit(OsString);

/// Any self-attach word that doesn't fit (#94 decision 22).
fn attach_takes() -> UsageError {
    UsageError("attach takes [+LINE] [FILE]".into())
}

/// A self-attach's `[+LINE] [--] [FILE]`, read a word at a time.
#[derive(Debug, Default)]
struct AttachWords {
    file: Option<OsString>,
    /// The line, and its word as typed.
    line: Option<(u32, OsString)>,
    /// After `--`.
    files_only: bool,
}

impl AttachWords {
    /// Whether options may still come: not after `--`.
    fn takes_options(&self) -> bool {
        !self.files_only
    }

    fn take(&mut self, word: &OsString) -> Result<(), Misfit> {
        let bytes = word.as_encoded_bytes();
        if self.files_only {
            // a file
        } else if bytes == b"--" {
            self.files_only = true;
            return Ok(());
        } else if let Some(n) = plus_line(bytes) {
            return match self.line.replace((n, word.clone())) {
                Some(_) => Err(Misfit(word.clone())),
                None => Ok(()),
            };
        } else if (bytes.starts_with(b"-") && bytes.len() > 1) || bytes.starts_with(b"+") {
            return Err(Misfit(word.clone()));
        }
        if self.file.replace(word.clone()).is_some() {
            return Err(Misfit(word.clone()));
        }
        Ok(())
    }

    fn end(self) -> Result<(Option<OsString>, Option<u32>), Misfit> {
        match (self.file, self.line) {
            (None, Some((_, word))) => Err(Misfit(word)),
            (file, line) => Ok((file, line.map(|(n, _)| n))),
        }
    }
}

/// `words` as a self-attach's `[+LINE] [--] [FILE]`: the file and line.
fn attach_words(words: &[OsString]) -> Result<(Option<OsString>, Option<u32>), Misfit> {
    let mut read = AttachWords::default();
    for word in words {
        read.take(word)?;
    }
    read.end()
}

/// `list`'s words: `[--json]`.
fn list(words: &[OsString]) -> Result<Command, UsageError> {
    let mut json = false;
    for word in words {
        if word != "--json" {
            return Err(UsageError("list takes only --json".into()));
        }
        if std::mem::replace(&mut json, true) {
            return Err(UsageError("--json is given twice".into()));
        }
    }
    Ok(Command::List { json })
}

/// A flag `status` or `kill` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flag {
    Json,
    All,
    Force,
}

impl Flag {
    fn word(self) -> &'static str {
        match self {
            Flag::Json => "--json",
            Flag::All => "--all",
            Flag::Force => "--force",
        }
    }
}

/// The options `status` and `kill` read: `--name N`, `--address A`, and
/// the flags each takes.
#[derive(Default)]
struct DaemonOptions {
    name: Option<OsString>,
    address: Option<OsString>,
    /// The flags given, each once.
    flags: Vec<Flag>,
}

impl DaemonOptions {
    /// Reads `words`: `--name N`, `--address A`, and `flags`. Anything else
    /// is refused with `takes`.
    fn read(words: &[OsString], flags: &[Flag], takes: &str) -> Result<DaemonOptions, UsageError> {
        let mut o = DaemonOptions::default();
        let mut i = 0;
        while let Some(word) = words.get(i) {
            if let Some(&flag) = flags.iter().find(|f| *word == *f.word()) {
                if o.flags.contains(&flag) {
                    return Err(UsageError(format!("{} is given twice", flag.word())));
                }
                o.flags.push(flag);
            } else if let Some((option, attached)) = long_option(word, &["--name", "--address"]) {
                let value = option_value(option, attached, words, &mut i)?;
                let slot = match option {
                    "--name" => &mut o.name,
                    _ => &mut o.address,
                };
                if slot.replace(value).is_some() {
                    return Err(UsageError(format!("{option} is given twice")));
                }
            } else {
                return Err(UsageError(takes.to_string()));
            }
            i += 1;
        }
        Ok(o)
    }

    fn has(&self, flag: Flag) -> bool {
        self.flags.contains(&flag)
    }

    /// The daemon `--name` or `--address` chooses, else the one found.
    fn target(self) -> Result<Target, UsageError> {
        match (self.name, self.address) {
            (Some(_), Some(_)) => Err(both_choose()),
            (Some(name), None) => Ok(Target::Name(daemon_name(&name)?)),
            (None, Some(address)) => Ok(Target::Address(Address::from_arg(address))),
            (None, None) => Ok(Target::Found),
        }
    }
}

/// `status`'s words: `[--name N | --address A] [--json]`.
fn status(words: &[OsString]) -> Result<Command, UsageError> {
    let o = DaemonOptions::read(
        words,
        &[Flag::Json],
        "status takes [--name N | --address A] [--json]",
    )?;
    let json = o.has(Flag::Json);
    Ok(Command::Status {
        to: o.target()?,
        json,
    })
}

/// `kill`'s words: `[--name N | --address A | --all] [--force]`.
fn kill(words: &[OsString]) -> Result<Command, UsageError> {
    let o = DaemonOptions::read(
        words,
        &[Flag::All, Flag::Force],
        "kill takes [--name N | --address A | --all] [--force]",
    )?;
    let force = o.has(Flag::Force);
    if !o.has(Flag::All) {
        return Ok(Command::Kill {
            to: KillTarget::One(o.target()?),
            force,
        });
    }
    if o.name.is_some() || o.address.is_some() {
        return Err(UsageError(
            "--all ends every daemon, and --name or --address one: give one".into(),
        ));
    }
    Ok(Command::Kill {
        to: KillTarget::All,
        force,
    })
}

/// `provider check`'s words: `[--config FILE] [--provider P]
/// [--daemon-name N] [--no-find] [--full] [--json]`, in any order.
fn provider_check(words: &[OsString]) -> Result<Command, UsageError> {
    let mut c = ProviderCheck::default();
    let twice = |option: &str| UsageError(format!("{option} is given twice"));
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let flag = match word.as_encoded_bytes() {
            b"--no-find" => Some(&mut c.no_find),
            b"--full" => Some(&mut c.full),
            b"--json" => Some(&mut c.json),
            _ => None,
        };
        if let Some(flag) = flag {
            if std::mem::replace(flag, true) {
                return Err(twice(&word.to_string_lossy()));
            }
            i += 1;
            continue;
        }
        let Some((option, attached)) =
            long_option(word, &["--config", "--provider", "--daemon-name"])
        else {
            return Err(UsageError(
                "provider check takes [--config FILE] [--provider P] [--daemon-name N] \
                 [--no-find] [--full] [--json]"
                    .into(),
            ));
        };
        let value = option_value(option, attached, words, &mut i)?;
        let given = match option {
            "--config" if value.is_empty() => {
                return Err(UsageError(format!("{option} needs a value")));
            }
            "--config" => c.config.replace(value).is_some(),
            // "" is no hint
            "--provider" => c.provider.replace(value).is_some(),
            _ => {
                let name = value
                    .to_str()
                    .filter(|n| valid_name(OsStr::new(n)))
                    .ok_or_else(|| {
                        UsageError(format!(
                            "{option}: use 1-32 letters, digits, '.', '_' or '-'"
                        ))
                    })?;
                c.daemon_name.replace(name.to_string()).is_some()
            }
        };
        if given {
            return Err(twice(option));
        }
        i += 1;
    }
    Ok(Command::ProviderCheck(c))
}

/// `provider show`'s words: `[NAME]`.
fn provider_show(words: &[OsString]) -> Result<Command, UsageError> {
    match words {
        [] => Ok(Command::ProviderShow(None)),
        [name] if !name.as_encoded_bytes().starts_with(b"-") => Ok(Command::ProviderShow(Some(
            name.to_string_lossy().into_owned(),
        ))),
        [option] => Err(UsageError(format!(
            "provider show: unknown option {}",
            option.to_string_lossy()
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

/// `attach`'s words: the hidden `--pane` form, else a self-attach.
fn attach(words: &[OsString]) -> Result<Command, UsageError> {
    if !words.iter().any(|w| long_option(w, &["--pane"]).is_some()) {
        return attach_self(words);
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

/// `attach [--name N | --address A] [--provider P] [+LINE] [--] [FILE]`,
/// without `--pane` (#94 decision 22). Any other word is a usage error.
fn attach_self(words: &[OsString]) -> Result<Command, UsageError> {
    let (mut name, mut address, mut provider) = (None, None, None);
    let mut read = AttachWords::default();
    let mut i = 0;
    while let Some(word) = words.get(i) {
        let option = read
            .takes_options()
            .then(|| long_option(word, &["--name", "--address", "--provider"]))
            .flatten();
        if let Some((option, attached)) = option {
            let value = option_value(option, attached, words, &mut i)?;
            let slot = match option {
                "--name" => &mut name,
                "--address" => &mut address,
                _ => &mut provider,
            };
            if slot.replace(value).is_some() {
                return Err(UsageError(format!("{option} is given twice")));
            }
        } else {
            read.take(word).map_err(|_| attach_takes())?;
        }
        i += 1;
    }
    let (file, line) = read.end().map_err(|_| attach_takes())?;
    let to = match (name, address) {
        (Some(_), Some(_)) => return Err(both_choose()),
        (Some(name), None) => Target::Name(daemon_name(&name)?),
        (None, Some(address)) => Target::Address(Address::from_arg(address)),
        (None, None) => Target::Found,
    };
    Ok(Command::AttachSelf(AttachSelf {
        to,
        provider,
        file,
        line,
    }))
}

/// The line in `+LINE`: digits, from 1.
fn plus_line(word: &[u8]) -> Option<u32> {
    let digits = word.strip_prefix(b"+")?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(digits)
        .ok()?
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

#[cfg(test)]
mod tests;
