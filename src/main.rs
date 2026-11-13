use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use neopane::attach::Transport;
use neopane::cli::{self, AttachSelf, Command, KillTarget, Start, Target, UsageError, report};
use neopane::daemon::control::{self, Failure, Killed, Listed};
use neopane::daemon::dir::{self, Address};
use neopane::daemon::find::{self, NotFound, find_daemon};
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
        Command::List { json } => Ok(list(json)),
        Command::Status { to, json } => Ok(status(&to, json)),
        Command::Kill { to, force } => Ok(kill(to, force)),
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
        let running = dir::base().is_ok_and(|base| base.live(name));
        if running {
            return match start.attach_to_running(name) {
                Ok(attach) => attach_self(attach),
                Err(err) => usage_error(err),
            };
        }
    }
    let launch = launch::Launch {
        name: start.name,
        provider: start.provider,
    };
    helper_exit(launch::run(&start.nvim_args, launch))
}

/// A self-attach to a running daemon, found as #94 decision 23 says. A
/// daemon not found is a failed self-attach, held as one.
fn attach_self(attach: AttachSelf) -> ExitCode {
    let started = std::time::Instant::now();
    let inside = std::env::var_os(find::inside_var());
    let warn = &mut |w: String| eprintln!("{NAME}: {w}");
    let address = match find_daemon(dir::base, &attach.to, inside.as_deref(), warn) {
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
        provider: attach.provider,
    };
    helper_exit(pane_helper::run_self(&address, transport, opening))
}

/// `$HOME`, for showing paths under it as `~`.
fn home() -> Option<Vec<u8>> {
    std::env::var_os("HOME").map(OsString::into_encoded_bytes)
}

/// `list [--json]` (#94 decisions 29 and 30): every running daemon, asked
/// at once.
fn list(json: bool) -> ExitCode {
    let base = match dir::base() {
        Ok(b) => b,
        Err(e) => return fail(&e.to_string()),
    };
    let listed = control::list(&base, control::LIST_WITHIN);
    if json {
        return print(&report::list_json(&listed));
    }
    if listed.is_empty() {
        eprintln!("{NAME}: no daemon is running");
    }
    print(&report::list_table(&listed, home().as_deref()))
}

/// The daemon `to` names, found as `attach` finds one.
fn found(to: &Target) -> std::result::Result<Address, NotFound> {
    let inside = std::env::var_os(find::inside_var());
    let warn = &mut |w: String| eprintln!("{NAME}: {w}");
    find_daemon(dir::base, to, inside.as_deref(), warn)
}

/// The daemon at `address`, for messages: its name, else its address.
fn who(address: &Address) -> String {
    address.daemon_name().unwrap_or_else(|| address.to_string())
}

/// Why the daemon at `address` gave no reply, as one line.
fn no_reply(address: &Address, e: &Failure) -> String {
    let who = who(address);
    match e {
        Failure::NoAnswer(_) => match address.log() {
            Some(log) => format!("{who} {e} (log: {})", log.display()),
            None => format!("{who} {e}"),
        },
        Failure::Refused(z) => format!("{who} refused: {}", z.message),
        Failure::Failed(why) => format!("{who}: {why}"),
    }
}

/// `status [--name N | --address A] [--json]` (#94 decision 31).
fn status(to: &Target, json: bool) -> ExitCode {
    let address = match found(to) {
        Ok(a) => a,
        Err(e) => return fail(&e.to_string()),
    };
    let reply = match control::ask(&address, control::status_purpose(), control::STATUS_WITHIN) {
        Ok(r) => r,
        Err(e) => return fail(&no_reply(&address, &e)),
    };
    if json {
        let l = Listed {
            name: reply.name.clone(),
            address,
            answer: Ok(reply),
        };
        return print(&format!("{}\n", report::daemon_json(&l)));
    }
    print(&report::status_text(
        &reply,
        &address.to_string(),
        home().as_deref(),
    ))
}

/// `kill [--name N | --address A | --all] [--force]` (#94 decision 32):
/// `stop`, then waiting for the daemon to be gone. One line per refusal
/// or failure.
fn kill(to: KillTarget, force: bool) -> ExitCode {
    let addresses = match to {
        KillTarget::One(t) => match found(&t) {
            Ok(a) => vec![a],
            Err(NotFound::NoneRunning) => return fail("no daemon is running"),
            Err(NotFound::Several(names)) => {
                return fail(&format!(
                    "{} daemons are running ({}); choose one with --name, or use --all",
                    names.len(),
                    names.join(", ")
                ));
            }
            Err(e) => return fail(&e.to_string()),
        },
        KillTarget::All => match dir::base() {
            Ok(base) => base.running().iter().map(|n| base.address(n)).collect(),
            Err(e) => return fail(&e.to_string()),
        },
    };
    let killing: Vec<_> = addresses
        .into_iter()
        .map(|a| {
            let thread = std::thread::spawn({
                let a = a.clone();
                move || control::kill(&a, force, control::KILL_WITHIN)
            });
            (a, thread)
        })
        .collect();
    let mut failed = false;
    for (address, thread) in killing {
        let line = match thread.join() {
            Ok(Killed::Gone) => continue,
            Ok(Killed::Refused(why)) => why,
            Ok(Killed::StillEnding { name, pid }) => {
                let secs = control::KILL_WITHIN.as_secs();
                format!("{name} is still ending after {secs} s (pid {pid})")
            }
            Ok(Killed::NoAnswer(e)) => no_reply(&address, &e),
            Err(_) => format!("{}: killing it failed", who(&address)),
        };
        eprintln!("{NAME}: {line}");
        failed = true;
    }
    ExitCode::from(u8::from(failed))
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
