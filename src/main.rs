use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use neorex::cli::{self, Command, Start, UsageError};
use neorex::daemon::{launch, pane_helper, serve};
use neorex::debug;
use neorex::name::NAME;

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
        Command::Start(start) => {
            if let Some(option) = unbuilt_option(&start) {
                return Ok(fail(&format!("{option} isn't built yet")));
            }
            Ok(helper_exit(launch::run(&start.nvim_args)?))
        }
        Command::Help => Ok(print(&cli::help())),
        Command::DebugHelp => Ok(print(&cli::debug_help())),
        Command::Version => Ok(print(&cli::version())),
        Command::NotBuilt(what) => Ok(fail(&format!("{what} isn't built yet"))),
        // started by the launcher, not by people
        Command::Daemon { dir, nvim_args } => done(serve::run(&dir.to_string_lossy(), &nvim_args)),
        Command::ConfigChild => Ok(fail("config-child isn't built yet")),
        Command::AttachPane {
            address,
            pane,
            transport,
        } => {
            let want = pane_helper::Want::Requested(pane);
            Ok(helper_exit(pane_helper::run(&address, want, transport)))
        }
        Command::Replaced => Ok(fail(cli::PANE_REPLACED)),
        Command::DebugKeys => done(debug::keys::run()),
        Command::DebugEvents(nvim_args) => done(debug::events::run(&nvim_args)),
        Command::DebugTui(nvim_args) => done(debug::tui::run(&nvim_args)),
        Command::DebugPanes(socket) => done(debug::panes::run(&socket)),
        Command::DebugRex(socket) => done(debug::rex::run(&socket)),
        Command::DebugLatency(path) => done(debug::latency::run(&path)),
    }
}

/// The first of `start`'s options that does nothing yet.
fn unbuilt_option(start: &Start) -> Option<&'static str> {
    [
        (start.name.is_some(), "--name"),
        (start.address.is_some(), "--address"),
        (start.detach, "--detach"),
        (start.no_wait, "--no-wait"),
    ]
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
