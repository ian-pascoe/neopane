use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use neopane::attach::{OpenFile, Transport, Wait};
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
        Command::ProviderCheck(c) => {
            let options = neopane::providers::check::Options {
                config: c.config,
                provider: c.provider,
                daemon_name: c.daemon_name,
                no_find: c.no_find,
                full: c.full,
                json: c.json,
            };
            let checked = neopane::providers::check::run(&options);
            let code = print(&checked.out);
            Ok(if checked.status == 0 {
                code
            } else {
                ExitCode::from(checked.status)
            })
        }
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
        Command::AttachSelf(attach) => Ok(attach_self(attach, inside(), Wait::UntilDone)),
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

/// The bare form (#94 decisions 13, 15 and 17): in a daemon's own
/// `:terminal`, it opens a file there; else `--name N` attaches when N is
/// running, else starts it; without it, a daemon named after this directory
/// starts, once the unsaved-file question, if asked, is answered no.
/// `--no-wait` matters only inside.
fn run_start(start: Start) -> ExitCode {
    if start.detach {
        return detach(start);
    }
    let inside = inside();
    let wait = Wait::unless(start.no_wait);
    match start.attach_to_address() {
        Ok(Some(attach)) => return attach_self(attach, inside, wait),
        Ok(None) => {}
        Err(err) => return usage_error(err),
    }
    if let Some(name) = &start.name {
        let base = dir::base();
        // naming the daemon it runs in (#94 decision 24)
        if let Some(i) = &inside
            && base
                .as_ref()
                .is_ok_and(|b| i.address.same_daemon(&b.address(name)))
        {
            return open_inside(i, &start.nvim_args, wait);
        }
        if base.is_ok_and(|base| base.live(name)) {
            return match start.attach_to_running(name) {
                Ok(attach) => attach_self(attach, inside, wait),
                Err(err) => usage_error(err),
            };
        }
    } else if let Some(i) = &inside {
        return open_inside(i, &start.nvim_args, wait);
    }
    match ask_about_unsaved(&start) {
        Some(Ok(attach)) => return attach_self(attach, inside, wait),
        Some(Err(code)) => return code,
        None => {}
    }
    let launch = launch::Launch {
        name: start.name,
        provider: start.provider,
    };
    helper_exit(launch::run(&start.nvim_args, launch))
}

/// The unsaved-file question (#94 decision 19), asked when the daemon
/// about to start is for one file whose unsaved changes a daemon holds,
/// unattached, and a person is there to answer: the attach to it, for
/// yes; the exit status, for no answer; None to start the daemon.
fn ask_about_unsaved(start: &Start) -> Option<std::result::Result<AttachSelf, ExitCode>> {
    let (file, line) = start.question_file()?;
    // SAFETY: isatty only inspects the fd.
    let terminals = unsafe { libc::isatty(0) == 1 && libc::isatty(2) == 1 };
    if !terminals {
        return None;
    }
    let holding = dir::base().ok()?.holding_unsaved(Path::new(&file));
    let first = holding.first()?.clone();
    let question = launch::unsaved_question(&file.to_string_lossy(), &holding);
    // Ctrl-C exits 1 while it's asked
    let before = exit_on_interrupt();
    let answer = launch::ask(&question, &mut io::stdin().lock(), &mut io::stderr());
    // SAFETY: putting back the handler there was.
    unsafe { libc::signal(libc::SIGINT, before) };
    match answer {
        launch::Answer::Yes => Some(Ok(AttachSelf {
            to: Target::Name(first),
            provider: start.provider.clone(),
            file: Some(file),
            line,
        })),
        launch::Answer::No => None,
        launch::Answer::Quit => Some(Err(ExitCode::from(1))),
    }
}

/// `--detach` (#94 decision 18): starts a kept daemon, from anywhere, and
/// prints its name once its Neovim answers. A failure is one line, with no
/// key wait.
fn detach(start: Start) -> ExitCode {
    if let Some(name) = &start.name
        && dir::base().is_ok_and(|base| base.live(name))
    {
        return fail(&format!(
            "{name} is already running; attach with {NAME} attach --name {name}"
        ));
    }
    match launch::detach(&start.nvim_args, start.name) {
        Ok(name) => print(&format!("{name}\n")),
        Err(e) => fail(&e.to_string()),
    }
}

/// The daemon whose own Neovim this runs in, as `$<NAME>` proves (#94
/// decision 17); unproven, it's ignored with a warning.
fn inside() -> Option<find::Inside> {
    let var = std::env::var_os(find::inside_var());
    let tty = find::this_tty();
    find::prove_inside(var.as_deref(), tty.as_deref(), &mut |w| {
        eprintln!("{NAME}: {w}")
    })
}

/// Inside daemon `inside`'s own Neovim, Neovim's arguments `args` open
/// only `[+LINE] [--] FILE` (#94 decision 17).
fn open_inside(inside: &find::Inside, args: &[OsString], wait: Wait) -> ExitCode {
    match cli::inside_file(args, &inside.name, wait) {
        Ok(Some(open)) => open_waited(inside, open),
        Ok(None) => already_inside(inside),
        Err(err) => usage_error(err),
    }
}

fn already_inside(inside: &find::Inside) -> ExitCode {
    fail(&format!(
        "already inside {}'s Neovim; open a file with {NAME} FILE",
        inside.name
    ))
}

/// Opens `open`'s file, as typed, in `inside`'s Neovim, beside this
/// terminal (ADR 0015): exits 0 once it's done, or for `--no-wait`, once
/// Neovim has it.
fn open_waited(inside: &find::Inside, open: OpenFile) -> ExitCode {
    use std::os::unix::ffi::OsStringExt;
    let typed = OsString::from_vec(open.file);
    let shown = typed.to_string_lossy().into_owned();
    let file = match std::path::absolute(&typed) {
        Ok(p) => p.into_os_string().into_encoded_bytes(),
        Err(e) => return fail(&format!("can't make the file's path absolute: {e}")),
    };
    let cwd = std::env::current_dir()
        .map(|d| d.into_os_string().into_encoded_bytes())
        .unwrap_or_default();
    exit_on_interrupt();
    let wait = open.wait;
    let open = OpenFile { file, ..open };
    let answer = control::open(&inside.address, cwd, inside.tty.clone(), open);
    match control::opened_line(&inside.name, &shown, wait, &answer) {
        None => ExitCode::SUCCESS,
        Some(line) => fail(&line),
    }
}

/// Ctrl-C exits 1, quietly (#94 decision 28). The handler there was.
fn exit_on_interrupt() -> libc::sighandler_t {
    extern "C" fn quit(_: libc::c_int) {
        // SAFETY: _exit is async-signal-safe.
        unsafe { libc::_exit(1) }
    }
    // SAFETY: `quit` only calls _exit.
    unsafe { libc::signal(libc::SIGINT, quit as *const () as libc::sighandler_t) }
}

/// A self-attach to a running daemon, found as #94 decision 23 says, or
/// in the daemon whose own Neovim this runs in, its file opened there
/// (decision 24). A daemon not found is a failed self-attach, held as one.
fn attach_self(attach: AttachSelf, inside: Option<find::Inside>, wait: Wait) -> ExitCode {
    let started = std::time::Instant::now();
    let address = match find_daemon(dir::base, &attach.to, inside.as_ref()) {
        Ok(address) => address,
        Err(e) => return helper_exit(pane_helper::launcher_failed(e.to_string(), started)),
    };
    if let Some(i) = inside.filter(|i| i.address.same_daemon(&address)) {
        return match attach.file {
            Some(file) => open_waited(
                &i,
                OpenFile {
                    file: file.into_encoded_bytes(),
                    line: attach.line,
                    wait,
                },
            ),
            None => already_inside(&i),
        };
    }
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
    let inside = match to {
        Target::Found => inside(),
        _ => None,
    };
    find_daemon(dir::base, to, inside.as_ref())
}

/// `status [--name N | --address A] [--json]` (#94 decision 31).
fn status(to: &Target, json: bool) -> ExitCode {
    let address = match found(to) {
        Ok(a) => a,
        Err(e) => return fail(&e.to_string()),
    };
    let reply = match control::ask(&address, control::status_purpose(), control::STATUS_WITHIN) {
        Ok(r) => r,
        Err(e) => return fail(&control::no_reply(&address, &e)),
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
        let killed = thread
            .join()
            .unwrap_or_else(|_| Killed::NoAnswer(Failure::Failed("killing it failed".into())));
        if let Some(line) = control::killed_line(&address, &killed) {
            eprintln!("{NAME}: {line}");
            failed = true;
        }
    }
    ExitCode::from(u8::from(failed))
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
