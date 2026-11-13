use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use neopane::attach::Transport;
use neopane::cli::{self, AttachSelf, Command, Start, Target, UsageError};
use neopane::daemon::dir::{self, Address};
use neopane::daemon::find::{self, find_daemon};
use neopane::daemon::{launch, pane_helper, serve};
use neopane::debug;
use neopane::name::NAME;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Runs the command line: 0 on success, 1 on a failure or refusal (one
/// `<name>: …` line on stderr), 2 on a usage error.
fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if let Err(err) = cli::started_by_us(&args, stdout_is_terminal()) {
        return usage_error(err);
    }
    let parsed = cli::parse(&args);
    let exists = |word: &OsStr| Path::new(word).symlink_metadata().is_ok();
    if let Some(note) = cli::file_note(&args, &parsed, exists) {
        eprintln!("{note}");
    }
    let command = match parsed {
        Ok(command) => command,
        Err(err) => return usage_error(err),
    };
    match run(command) {
        Ok(code) => code,
        Err(err) => fail(&err.to_string()),
    }
}

fn run(command: Command) -> Result<ExitCode> {
    let done = |r: Result<()>| r.map(|()| ExitCode::SUCCESS);
    match command {
        Command::Start(start) => Ok(run_start(start)),
        Command::Help => Ok(print(&cli::help())),
        Command::DebugHelp => Ok(print(&cli::debug_help())),
        Command::Version => Ok(print(&cli::version())),
        Command::NotBuilt(what) => Ok(fail(&format!("{what} isn't built yet"))),
        Command::ProviderShow(name) => {
            match neopane::config::show::show(name.as_deref(), neopane::attach::build()) {
                Ok(lua) => Ok(print(&lua)),
                Err(why) => Ok(fail(&why)),
            }
        }
        Command::ProviderTypes { config } => Ok(write_types(config)),
        // started by the launcher, not by people
        Command::Daemon {
            name,
            dedup,
            detach,
            nvim_args,
        } => done(serve::run(&name, dedup, detach, &nvim_args)),
        Command::ConfigChild => match neopane::config::child::main() {
            Ok(()) => Ok(ExitCode::SUCCESS),
            Err(why) => Ok(fail(&why)),
        },
        Command::AttachPane {
            address,
            pane,
            transport,
        } => {
            let want = pane_helper::Want::Requested(pane);
            let address = Address::from_arg(address);
            let transport =
                transport.unwrap_or_else(|| Transport::from_env(|w| eprintln!("{NAME}: {w}")));
            Ok(helper_exit(pane_helper::run(&address, want, transport)))
        }
        Command::AttachSelf(attach) => Ok(attach_self(attach)),
        Command::DebugKeys => done(debug::keys::run()),
        Command::DebugEvents(nvim_args) => done(debug::events::run(&nvim_args)),
        Command::DebugTui(nvim_args) => done(debug::tui::run(&nvim_args)),
        Command::DebugPanes(socket) => done(debug::panes::run(&socket)),
        Command::DebugRex(socket) => done(debug::rex::run(&socket)),
        Command::DebugLatency(path) => done(debug::latency::run(&path)),
        Command::DebugRelay { address, pane } => Ok(helper_exit(debug::relay::run(
            &Address::from_arg(address),
            pane,
        ))),
    }
}

/// `provider types`: the file is `--config`, else where the daemon would
/// look, made absolute against the working directory.
fn write_types(config: Option<OsString>) -> ExitCode {
    use neopane::config::types;
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    let file = match config {
        Some(file) => Some(cwd.join(file)),
        None => neopane::config::default_file(|name| std::env::var_os(name), &cwd),
    };
    let Some(file) = file else {
        return fail("can't find the config directory: HOME isn't set");
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let report = types::write(&file, neopane::attach::build(), home.as_deref());
    let code = print(&report.out);
    match report.error {
        Some(why) => fail(&why),
        None => code,
    }
}

/// The bare form (#94 decisions 13 and 15): `--name N` attaches when N is
/// running, else starts it; without it, a daemon named after this directory
/// starts.
fn run_start(start: Start) -> ExitCode {
    if let Some(option) = unbuilt_option(&start) {
        return fail(&format!("{option} isn't built yet"));
    }
    if let Some(name) = &start.name {
        let running = dir::base().is_ok_and(|base| base.address(name).answers());
        if running {
            return match start.attach_to_running(name) {
                Ok(attach) => attach_self(attach),
                Err(err) => usage_error(err),
            };
        }
    }
    let launch = launch::Launch {
        name: start.name,
        provider: hint(start.provider),
    };
    helper_exit(launch::run(&start.nvim_args, launch))
}

/// A self-attach to a running daemon, found as #94 decision 23 says. A
/// daemon not found is a failed self-attach, held as one.
fn attach_self(attach: AttachSelf) -> ExitCode {
    let started = std::time::Instant::now();
    let target = match attach.to {
        Target::Address(a) => find::Target::Address(Address::from_arg(a)),
        Target::Name(n) => find::Target::Name(n),
        Target::Found => find::Target::Found,
    };
    let inside = std::env::var_os(find::inside_var());
    let warn = &mut |w: String| eprintln!("{NAME}: {w}");
    let address = match find_daemon(dir::base, &target, inside.as_deref(), warn) {
        Ok(address) => address,
        Err(e) => return helper_exit(pane_helper::launcher_failed(e.to_string(), started)),
    };
    // made absolute here, links not resolved: the daemon has its own
    // working directory
    let file = match attach.file.map(std::path::absolute).transpose() {
        Ok(f) => f.map(|p| p.into_os_string().into_encoded_bytes()),
        Err(e) => return fail(&format!("can't make the file's path absolute: {e}")),
    };
    let transport = Transport::from_env(|w| eprintln!("{NAME}: {w}"));
    let opening = pane_helper::Opening {
        file,
        line: attach.line,
        provider: hint(attach.provider),
    };
    helper_exit(pane_helper::run_self(&address, transport, opening))
}

/// `--provider P`, for the hello; empty is none given.
fn hint(provider: Option<OsString>) -> Option<String> {
    provider
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| !p.is_empty())
}

/// The first of `start`'s options that does nothing yet.
fn unbuilt_option(start: &Start) -> Option<&'static str> {
    [(start.detach, "--detach"), (start.no_wait, "--no-wait")]
        .into_iter()
        .find_map(|(given, option)| given.then_some(option))
}

fn stdout_is_terminal() -> bool {
    // SAFETY: isatty only inspects the fd.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}

/// Prints what was asked for on stdout. A reader that went away (a broken
/// pipe) ends it quietly.
fn print(text: &str) -> ExitCode {
    let mut out = io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => fail(&format!("can't write to stdout: {e}")),
    }
}

/// Prints a helper's line, if any, and exits with its status.
fn helper_exit(exit: pane_helper::Exit) -> ExitCode {
    if let Some(m) = exit.message {
        eprintln!("{NAME}: {m}");
    }
    ExitCode::from(exit.status)
}

/// Prints `<name>: why` and exits 1.
fn fail(why: &str) -> ExitCode {
    eprintln!("{NAME}: {why}");
    ExitCode::from(1)
}

/// Prints the usage error and exits 2.
fn usage_error(err: UsageError) -> ExitCode {
    eprintln!("{err}");
    ExitCode::from(2)
}
