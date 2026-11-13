//! `neopane [nvim args]`: opens Neovim in this pane. Starts the daemon in the
//! background, then hands it this terminal and waits as its pane helper.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use super::dir;
use super::pane_helper;
use crate::attach::Transport;
use crate::log::{self, LogLevel, Point, Traces};
use crate::name::NAME;

/// The launcher is a top layer: any error will do.
pub(crate) type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Why the launcher gave up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    NotATerminal,
    /// The daemon said why it didn't start.
    Refused(String),
    /// The daemon ended without a word: the first line it wrote to stderr,
    /// if any, and the base directory its log would be under.
    DaemonFailed(Option<String>, String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::NotATerminal => write!(
                f,
                "stdin isn't a terminal; to start a daemon without one, use --detach"
            ),
            Error::Refused(why) => write!(f, "{why}"),
            Error::DaemonFailed(said, base) => {
                let said = said.as_deref().unwrap_or("it exited with no message");
                write!(f, "the daemon didn't start: {said} (logs: {base})")
            }
        }
    }
}

impl std::error::Error for Error {}

/// The daemon's one line on the ready pipe.
#[derive(Debug, PartialEq, Eq)]
enum Ready {
    /// `ok <name>`: it listens, under this name.
    Ok(String),
    /// `error <reason>`.
    Error(String),
    /// Nothing, or nothing it says.
    Nothing,
}

fn ready(line: &str) -> Ready {
    let line = line.strip_suffix('\n').unwrap_or(line);
    if let Some(name) = line.strip_prefix("ok ")
        && dir::valid_name(OsStr::new(name))
    {
        return Ready::Ok(name.to_string());
    }
    match line.strip_prefix("error ") {
        Some(reason) => Ready::Error(reason.to_string()),
        None => Ready::Nothing,
    }
}

/// How the launcher starts its daemon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Launch {
    /// `--name N`: the daemon is named exactly N. Without it, it is named
    /// after this directory, de-duplicated.
    pub name: Option<String>,
    /// `--provider P`, as given, for the first attach's hello.
    pub provider: Option<std::ffi::OsString>,
}

/// Starts a daemon and attaches this terminal to it: how the attach ended,
/// for the exit status. A failure before the attach is held as a failed
/// attach is, with a key wait unless a shell started it as a job.
pub fn run<S: AsRef<OsStr>>(nvim_args: &[S], launch: Launch) -> pane_helper::Exit {
    let started = std::time::Instant::now();
    match start(nvim_args, launch) {
        Ok(exit) => exit,
        Err(e) => pane_helper::launcher_failed(e.to_string(), started),
    }
}

fn start<S: AsRef<OsStr>>(nvim_args: &[S], launch: Launch) -> Result<pane_helper::Exit> {
    // L6 is timed from here
    let started = log::now_ns();
    // SAFETY: isatty only inspects the fd.
    if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
        return Err(Error::NotATerminal.into());
    }
    // warned of before the daemon starts, which warns in its log
    let transport = Transport::from_env(|w| eprintln!("{NAME}: {w}"));
    let base = dir::base()?;
    let name = spawn(&base, nvim_args, launch.name, false)?;
    // the daemon warns of a level it doesn't know
    if log::from_env().0 == LogLevel::Debug {
        let mut traces = Traces::new();
        traces.set_on(true);
        traces.record(Point::Exec, u64::from(std::process::id()), started, None);
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(base.log(&name))?;
        traces.write_into(&mut log)?;
    }
    // the daemon outlives us; it's never waited for here
    let opening = pane_helper::Opening {
        provider: launch.provider,
        ..Default::default()
    };
    Ok(pane_helper::run_self(
        &base.address(&name),
        transport,
        opening,
    ))
}

/// `--detach` (#94 decision 18): starts a kept daemon, named `name` or
/// after this directory, and returns its name once its Neovim has answered.
/// No terminal is needed.
pub fn detach<S: AsRef<OsStr>>(nvim_args: &[S], name: Option<String>) -> Result<String> {
    let base = dir::base()?;
    spawn(&base, nvim_args, name, true)
}

/// Starts the daemon `name`, else one named after this directory,
/// de-duplicated, in a session of its own, and reads its ready line: the
/// name it took.
fn spawn<S: AsRef<OsStr>>(
    base: &dir::Base,
    nvim_args: &[S],
    name: Option<String>,
    detach: bool,
) -> Result<String> {
    let (name, dedup) = match name {
        Some(name) => (name, None),
        None => (
            default_name(&std::env::current_dir().unwrap_or_default()),
            Some("--dedup"),
        ),
    };

    let exe = std::env::current_exe()?.canonicalize()?;
    let mut command = Command::new(&exe);
    // `ps` shows `<name> daemon NAME`
    command
        .arg0(NAME)
        .args(["daemon", &name])
        .args(dedup)
        .args(detach.then_some("--detach"))
        .arg("--")
        .args(nvim_args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // out of the pane's session, so closing the pane doesn't signal it
    // SAFETY: setsid is async-signal-safe, and touches no memory.
    unsafe {
        command.pre_exec(|| match libc::setsid() {
            -1 => Err(std::io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let mut daemon = command.spawn()?;

    let mut line = String::new();
    let stdout = daemon.stdout.take().expect("piped");
    let _ = BufReader::new(stdout).read_line(&mut line);
    match ready(&line) {
        Ready::Ok(name) => {
            // (from here it writes to its log, not to this pipe)
            drop(daemon.stderr.take());
            Ok(name)
        }
        Ready::Error(why) => {
            gone(&mut daemon);
            Err(Error::Refused(why).into())
        }
        Ready::Nothing => {
            let said = gone(&mut daemon);
            Err(Error::DaemonFailed(said, base.to_string()).into())
        }
    }
}

/// The unsaved-file question (#94 decision 19), for FILE as typed, whose
/// changes daemons `holding` hold, the first by name asked about.
pub fn unsaved_question(file: &str, holding: &[String]) -> String {
    let (first, others) = holding.split_first().expect("a daemon holds it");
    let also = match others {
        [] => String::new(),
        _ => format!(" (also in {})", others.join(", ")),
    };
    format!(
        "{NAME}: {file} has unsaved changes in {first}{also}, which no terminal shows. Attach to \
         it? [Y/n] "
    )
}

/// What the unsaved-file question was answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// `y`, `Y` or Enter: attach.
    Yes,
    /// `n` or `N`: start the daemon as asked.
    No,
    /// End of input, as Ctrl-D: nothing starts.
    Quit,
}

/// Asks `question` on `out` and reads a line of `input`, until it is an
/// answer. At the end of input the line is ended, for the shell's prompt.
pub fn ask(question: &str, input: &mut impl BufRead, out: &mut impl std::io::Write) -> Answer {
    loop {
        let _ = out.write_all(question.as_bytes());
        let _ = out.flush();
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) | Err(_) => {
                let _ = out.write_all(b"\n");
                return Answer::Quit;
            }
            Ok(_) => {}
        }
        match line.trim_end_matches(['\n', '\r']) {
            "" | "y" | "Y" => return Answer::Yes,
            "n" | "N" => return Answer::No,
            _ => {}
        }
    }
}

/// The name of a daemon started in `cwd`: from its last component.
fn default_name(cwd: &std::path::Path) -> String {
    dir::clean_name(cwd.file_name().unwrap_or_default())
}

/// Waits for a daemon that failed to start to end: the first line it wrote
/// to stderr, if any.
fn gone(daemon: &mut Child) -> Option<String> {
    let mut said = Vec::new();
    if let Some(mut stderr) = daemon.stderr.take() {
        let _ = stderr.read_to_end(&mut said);
    }
    let _ = daemon.wait();
    String::from_utf8_lossy(&said)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{Answer, Ready, ask, default_name, ready, unsaved_question};
    use crate::name::NAME;
    use std::path::Path;

    #[test]
    fn the_unsaved_file_question_names_the_first_daemon_and_the_others() {
        let names = |n: &[&str]| n.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            format!(
                "{NAME}: notes.md has unsaved changes in notes, which no terminal shows. Attach \
                 to it? [Y/n] "
            ),
            unsaved_question("notes.md", &names(&["notes"]))
        );
        assert_eq!(
            format!(
                "{NAME}: notes.md has unsaved changes in notes (also in notes-2, notes-3), which \
                 no terminal shows. Attach to it? [Y/n] "
            ),
            unsaved_question("notes.md", &names(&["notes", "notes-2", "notes-3"]))
        );
    }

    #[test]
    fn the_question_is_asked_until_it_is_answered() {
        let asked = |typed: &str| {
            let mut out = Vec::new();
            let answer = ask("Q? ", &mut typed.as_bytes(), &mut out);
            (answer, String::from_utf8(out).unwrap())
        };
        for yes in ["y\n", "Y\n", "\n"] {
            assert_eq!((Answer::Yes, "Q? ".into()), asked(yes), "{yes:?}");
        }
        for no in ["n\n", "N\n"] {
            assert_eq!((Answer::No, "Q? ".into()), asked(no), "{no:?}");
        }
        assert_eq!((Answer::Quit, "Q? \n".into()), asked(""));
        assert_eq!((Answer::Yes, "Q? Q? Q? ".into()), asked("x\nyes\ny\n"));
        assert_eq!((Answer::Quit, "Q? Q? \n".into()), asked("x\n"));
    }

    #[test]
    fn a_daemon_is_named_after_the_directory_it_started_in() {
        assert_eq!("neox.1", default_name(Path::new("/home/me/src/neo x.1")));
        assert_eq!("default", default_name(Path::new("/")));
        assert_eq!("default", default_name(Path::new("/home/\u{e9}")));
    }

    #[test]
    fn the_ready_line_names_the_daemon_or_says_why_not() {
        assert_eq!(Ready::Ok("work-2".into()), ready("ok work-2\n"));
        assert_eq!(
            Ready::Error("work is already running".into()),
            ready("error work is already running\n")
        );
        for nothing in ["", "\n", "ok \n", "ok a/b\n", "r", "okwork\n"] {
            assert_eq!(Ready::Nothing, ready(nothing), "{nothing:?}");
        }
    }
}
