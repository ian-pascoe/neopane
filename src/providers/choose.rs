//! Choosing a pane provider for a self-attach (ADR 0007, #87 decisions
//! D5–D7, #92 decisions 24 and 25). The user's config, if there is
//! one, chooses: it runs in the config child, on a thread of its own so the
//! daemon's loop never waits for it (`ConfigRun`), and what it returned is
//! checked by the daemon (`by_config`). With no file, the built-in
//! `<name>.default` chooses, run in the daemon itself: Rex when the
//! attach's environment has `REX_SERVER` and `REX_SESSION`, else none. The
//! hint `none` skips both.
//!
//! Choosing Rex only starts connecting to it, which never blocks: the
//! daemon's loop takes the steps, and its `find` confirms the choice.

use std::ffi::OsString;
use std::io::PipeReader;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::command::{CommandProvider, Spec, commands_env};
use super::rex::RexProvider;
use crate::attach::env_value;
use crate::config::check::Severity;
use crate::config::{self, Builtin, Choice, Problems};
use crate::daemon::pane::AttachHello;
use crate::daemon::provider::PaneProvider;
use crate::log::{log_info, log_warn};
use crate::name::NAME;

/// An attach's environment: names and values, raw.
pub(crate) type Env = [(Vec<u8>, Vec<u8>)];

/// The hint that skips choosing: `--provider none`, `$<NAME>_PROVIDER=none`.
pub(crate) const HINT_NONE: &str = "none";

/// Opens a connection to the Rex server at a `unix://` URL. Tests fake it.
pub(crate) type Connect = Box<dyn FnMut(&str) -> std::io::Result<UnixStream>>;

/// The `Connect` the daemon runs with.
pub(crate) fn connect() -> Connect {
    Box::new(|url| {
        crate::rex::client::connect_to(url).map_err(|e| match e {
            crate::rex::client::ClientError::Io(e) => e,
            e => std::io::Error::other(e.to_string()),
        })
    })
}

/// What came of choosing.
pub(crate) enum Chosen {
    /// A pane provider, which its `find` still has to confirm, and the time
    /// choosing it took, connecting: the choice's 3 s count it.
    Provider {
        held: Box<dyn PaneProvider>,
        spent: Duration,
    },
    /// None, and why; shown in Neovim as a warning if `warn`.
    None { why: String, warn: bool },
    /// None: the config failed, or returned something malformed.
    ConfigError,
}

/// What the in-process `<name>.default` chooses for an attach whose
/// environment is `env`: the config's own `<name>.default`, so the two
/// never differ.
pub(crate) fn default_choice(env: &Env) -> Choice {
    config::default(&config::Attach {
        env: env.to_vec(),
        ..Default::default()
    })
}

/// Chooses for a self-attach with environment `env` and `hint`, connecting
/// to Rex through `connect` if that's the choice.
pub(crate) fn choose(env: &Env, hint: Option<&str>, connect: &mut Connect) -> Chosen {
    let started = Instant::now();
    if hint == Some(HINT_NONE) {
        return Chosen::None {
            why: format!("the hint is {HINT_NONE}"),
            warn: false,
        };
    }
    match default_choice(env) {
        Choice::Builtin(Builtin::Rex) => start_rex(env, connect, started),
        _ => Chosen::None {
            why: "REX_SERVER and REX_SESSION aren't both set".into(),
            warn: false,
        },
    }
}

/// Starts connecting to the Rex server and session `env` names. One that
/// can't be reached, as when the variables are stale, means none.
fn start_rex(env: &Env, connect: &mut Connect, since: Instant) -> Chosen {
    let var =
        |name: &str| String::from_utf8_lossy(env_value(env, name).unwrap_or_default()).into_owned();
    let (server, session) = (var("REX_SERVER"), var("REX_SESSION"));
    let started = connect(&server).and_then(|stream| {
        RexProvider::connecting(stream, &session).map_err(|e| std::io::Error::other(e.to_string()))
    });
    match started {
        Ok(rex) => Chosen::Provider {
            held: Box::new(rex),
            spent: since.elapsed(),
        },
        Err(e) => {
            // where, for the log: a warning that long would wait at a prompt
            log_warn!("{NAME} daemon: rex: can't reach Rex at {server}: {e}");
            Chosen::None {
                why: format!("rex: can't reach Rex: {e}"),
                warn: true,
            }
        }
    }
}

/// The config file a daemon reads, from its start environment `env` and
/// working directory `cwd` (#94 decisions 33 and 39): `$<NAME>_CONFIG`,
/// made absolute against `cwd`, else `$XDG_CONFIG_HOME/<name>/init.lua`,
/// else `~/.config/<name>/init.lua`. None without `HOME`.
pub(crate) fn config_path(env: &[(OsString, OsString)], cwd: &Path) -> Option<ConfigFile> {
    let var = |name: &str| {
        env.iter()
            .find(|(k, _)| k.as_bytes() == name.as_bytes())
            .map(|(_, v)| v.clone())
    };
    let named = var(&config::config_var()).is_some_and(|v| !v.is_empty());
    config::default_file(var, cwd).map(|path| ConfigFile { path, named })
}

/// The config file a daemon reads, and whether `$<NAME>_CONFIG` named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigFile {
    pub path: PathBuf,
    pub named: bool,
}

/// How a self-attach chooses.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ChooseBy<'a> {
    /// The built-in default, in the daemon: there's no config file, or the
    /// hint is `none`.
    Default,
    /// The config file, which is there.
    Config(&'a Path),
    /// The file `$<NAME>_CONFIG` names, which isn't there: a config error.
    Missing(&'a Path),
}

/// How a self-attach with `hint` chooses, the daemon's config file being
/// `file`: only the default places being empty means the default chooses.
pub(crate) fn choose_by<'a>(file: Option<&'a ConfigFile>, hint: Option<&str>) -> ChooseBy<'a> {
    match file {
        _ if hint == Some(HINT_NONE) => ChooseBy::Default,
        Some(f) if f.path.exists() => ChooseBy::Config(&f.path),
        Some(f) if f.named => ChooseBy::Missing(&f.path),
        _ => ChooseBy::Default,
    }
}

/// What the log and the Neovim warning say of a config file
/// `$<NAME>_CONFIG` names that isn't there.
pub(crate) fn missing_line(file: &Path) -> String {
    format!(
        "config: {} isn't there ({} names it); no pane provider",
        file.display(),
        config::config_var()
    )
}

/// The config running for a choice: the blocking runner, on a thread of
/// its own. Its `fd` turns readable once the outcome is in.
pub(crate) struct ConfigRun {
    /// Its end comes when the thread is done.
    done: PipeReader,
    outcome: mpsc::Receiver<config::Outcome>,
    /// The child's process group while it runs, else 0.
    group: Arc<AtomicI32>,
    /// The config file.
    pub file: PathBuf,
}

impl ConfigRun {
    /// Runs config `file` for `attach`, in a child of executable `exe`, in
    /// the attach's working directory and the commands' environment, with
    /// the config's time (#93 decision 3).
    pub(crate) fn start(exe: &Path, file: &Path, attach: config::Attach) -> std::io::Result<Self> {
        let (done, tell_done) = std::io::pipe()?;
        let (tell, outcome) = mpsc::channel();
        let env: Vec<(OsString, OsString)> = commands_env(&attach.env)
            .into_iter()
            .map(|(k, v)| (OsString::from_vec(k), OsString::from_vec(v)))
            .collect();
        let request = config::Request {
            file: file.as_os_str().as_bytes().to_vec(),
            version: env!("CARGO_PKG_VERSION").into(),
            attach,
            time: config::TIME,
        };
        let exe = exe.to_path_buf();
        let group = Arc::new(AtomicI32::new(0));
        let its_group = group.clone();
        std::thread::Builder::new()
            .name("config".into())
            .spawn(move || {
                let spawn = config::Spawn {
                    exe: &exe,
                    env: &env,
                    group: Some(&its_group),
                };
                let _ = tell.send(config::run(spawn, &request));
                // closing it wakes the loop
                drop(tell_done);
            })?;
        Ok(ConfigRun {
            done,
            outcome,
            group,
            file: file.to_path_buf(),
        })
    }

    /// What the loop watches: readable once the outcome is in.
    pub(crate) fn fd(&self) -> RawFd {
        self.done.as_raw_fd()
    }

    /// The outcome, once it is in.
    pub(crate) fn outcome(&self) -> Option<config::Outcome> {
        match self.outcome.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(config::Outcome {
                printed: Vec::new(),
                printed_cut: false,
                answer: Err(config::Failure::CantStart("its thread failed".into())),
                took: Duration::ZERO,
            }),
        }
    }
}

impl Drop for ConfigRun {
    /// The daemon ends mid-choice: nothing the config started outlives it.
    fn drop(&mut self) {
        let group = self.group.swap(0, Ordering::SeqCst);
        if group > 0 {
            // SAFETY: a plain syscall; the runner hasn't reaped the child,
            // or it would have cleared `group` first.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
}

/// What the config's `outcome` chooses for the choosing attach, whose
/// hello is `hello`, connecting to Rex through `connect` if that's the
/// choice; and the Neovim warning its problems make, if any
/// (#93 decision 20). Any error means none, never the default. What it
/// printed and its problems go to the log.
pub(crate) fn by_config(
    outcome: &config::Outcome,
    file: &Path,
    hello: &AttachHello,
    connect: &mut Connect,
) -> (Chosen, Option<String>) {
    let file_name = file
        .file_name()
        .map_or_else(|| config::FILE_NAME.into(), |n| n.to_string_lossy());
    outcome.log();
    log_info!(
        "{NAME} daemon: config: ran {} in {:.1} ms",
        file.display(),
        outcome.took.as_secs_f64() * 1000.0
    );
    match config::choose(outcome, &file_name) {
        Err(problems) => {
            log_problems(&problems);
            (Chosen::ConfigError, problems.message())
        }
        Ok(chosen) => {
            log_problems(&chosen.warnings);
            let warning = chosen.warnings.message();
            (of_choice(&chosen.choice, hello, connect), warning)
        }
    }
}

/// Logs each of `problems`.
fn log_problems(problems: &Problems) {
    for p in &problems.0 {
        match p.severity {
            Severity::Error | Severity::Warning => log_warn!("{NAME} daemon: config: {}", p.text),
            Severity::CheckOnly => log_info!("{NAME} daemon: config: {}", p.text),
        }
    }
}

/// The pane provider for the config's `choice`, for the choosing attach,
/// whose hello is `hello`.
fn of_choice(choice: &Choice, hello: &AttachHello, connect: &mut Connect) -> Chosen {
    let started = Instant::now();
    match choice {
        Choice::None => Chosen::None {
            why: "the config chose none".into(),
            warn: false,
        },
        Choice::Builtin(Builtin::Rex) => start_rex(&hello.env, connect, started),
        Choice::Whole(_) | Choice::Pair(_) => {
            let spec = Spec::of(choice).expect("a table or a pair has a spec");
            let name = spec.name.clone();
            match CommandProvider::new(spec, &hello.env, &hello.cwd) {
                Ok(p) => Chosen::Provider {
                    held: Box::new(p),
                    spent: started.elapsed(),
                },
                Err(e) => Chosen::None {
                    why: format!("{name}: can't start: {e}"),
                    warn: true,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> Vec<(Vec<u8>, Vec<u8>)> {
        vars.iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn the_default_chooses_rex_only_with_rex_server_and_rex_session_set() {
        let rex = Choice::Builtin(Builtin::Rex);
        let both = [("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")];
        assert_eq!(rex, default_choice(&env(&both)));
        // set, even if empty
        assert_eq!(
            rex,
            default_choice(&env(&[("REX_SERVER", ""), ("REX_SESSION", "")]))
        );
        assert_eq!(Choice::None, default_choice(&env(&both[..1])));
        assert_eq!(Choice::None, default_choice(&env(&both[1..])));
        assert_eq!(
            Choice::None,
            default_choice(&env(&[("TMUX", "/tmp/t,1,0")]))
        );
    }

    fn connects_nowhere() -> Connect {
        Box::new(|url| panic!("connected to {url}"))
    }

    fn none(c: Chosen) -> (String, bool) {
        match c {
            Chosen::None { why, warn } => (why, warn),
            Chosen::Provider { .. } => panic!("chose a pane provider"),
            Chosen::ConfigError => panic!("a config error"),
        }
    }

    #[test]
    fn the_config_file_comes_from_the_start_environment() {
        let var = config::config_var();
        let os = |vars: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            vars.iter().map(|(k, v)| (k.into(), v.into())).collect()
        };
        let cwd = Path::new("/launch");
        let home = ("HOME", "/home/u");
        let at = |vars: &[(&str, &str)]| config_path(&os(vars), cwd).map(|f| f.path);
        // made absolute once, against where the daemon started
        assert_eq!(
            Some(PathBuf::from("/launch/my.lua")),
            at(&[home, (&var, "my.lua")])
        );
        assert_eq!(
            Some(PathBuf::from("/etc/x.lua")),
            at(&[home, (&var, "/etc/x.lua")])
        );
        // empty counts as unset
        let xdg = format!("/xdg/{NAME}/init.lua");
        assert_eq!(
            Some(PathBuf::from(&xdg)),
            at(&[home, (&var, ""), ("XDG_CONFIG_HOME", "/xdg")])
        );
        let dotconfig = format!("/home/u/.config/{NAME}/init.lua");
        assert_eq!(Some(PathBuf::from(&dotconfig)), at(&[home]));
        assert_eq!(None, at(&[]));
        // named, or found at a default place
        let named = |vars: &[(&str, &str)]| config_path(&os(vars), cwd).map(|f| f.named);
        assert_eq!(Some(true), named(&[home, (&var, "my.lua")]));
        assert_eq!(Some(false), named(&[home, (&var, "")]));
    }

    #[test]
    fn a_config_run_dropped_mid_choice_takes_what_it_started_with_it() {
        // the built binary runs the config child; skipped without it
        let exe = std::env::current_exe().unwrap();
        let bin = exe.parent().unwrap().parent().unwrap().join(NAME);
        if !bin.is_file() {
            assert!(std::env::var_os("CI").is_none(), "no {}", bin.display());
            return;
        }
        let dir = std::env::temp_dir().join(format!("{NAME}-cfg-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("pid");
        let source = format!(
            "os.execute('echo $$ > {} && exec sleep 30')\nreturn {{ provider = function() end }}\n",
            pid_file.display()
        );
        std::fs::write(dir.join("init.lua"), source).unwrap();
        let attach = config::Attach {
            cwd: b"/".to_vec(),
            ..Default::default()
        };
        let run = ConfigRun::start(&bin, &dir.join("init.lua"), attach).unwrap();
        let due = Instant::now() + Duration::from_secs(10);
        let pid = loop {
            let read = std::fs::read_to_string(&pid_file).unwrap_or_default();
            if let Ok(pid) = read.trim().parse::<i32>() {
                break pid;
            }
            assert!(Instant::now() < due, "the config never ran its command");
            std::thread::sleep(Duration::from_millis(10));
        };
        drop(run);
        // SAFETY: signal 0 only checks the process is there.
        let alive = || unsafe { libc::kill(pid, 0) } == 0;
        // well within the config's 3 s, after which the runner kills it
        let due = Instant::now() + Duration::from_secs(1);
        while alive() && Instant::now() < due {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!alive(), "the config's sleep outlived its run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_named_config_that_isnt_there_is_an_error() {
        let here = ConfigFile {
            path: std::env::current_exe().unwrap(),
            named: false,
        };
        let gone = |named| ConfigFile {
            path: PathBuf::from("/nowhere/init.lua"),
            named,
        };
        assert_eq!(ChooseBy::Config(&here.path), choose_by(Some(&here), None));
        assert_eq!(ChooseBy::Default, choose_by(Some(&gone(false)), None));
        let named = gone(true);
        assert_eq!(
            ChooseBy::Missing(&named.path),
            choose_by(Some(&named), None)
        );
        assert_eq!(ChooseBy::Default, choose_by(None, Some("tmux")));
        // the hint none skips the file, there or not
        assert_eq!(ChooseBy::Default, choose_by(Some(&here), Some("none")));
        assert_eq!(ChooseBy::Default, choose_by(Some(&named), Some("none")));
    }

    #[test]
    fn the_hint_none_or_no_rex_chooses_none_quietly() {
        let in_rex = env(&[("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")]);
        let (_, warn) = none(choose(&in_rex, Some("none"), &mut connects_nowhere()));
        assert!(!warn);
        let (_, warn) = none(choose(&env(&[]), None, &mut connects_nowhere()));
        assert!(!warn);
    }

    #[test]
    fn a_rex_that_cant_be_reached_chooses_none_with_a_warning() {
        // stale REX_*: a Ghostty window started from a Rex shell, Rex since quit
        let stale = env(&[("REX_SERVER", "unix:///gone"), ("REX_SESSION", "session:s")]);
        let mut refused: Connect = Box::new(|_| Err(std::io::ErrorKind::ConnectionRefused.into()));
        let (why, warn) = none(choose(&stale, None, &mut refused));
        assert!(warn);
        assert!(why.starts_with("rex: can't reach Rex: "), "{why}");
    }

    #[test]
    fn rex_is_chosen_connecting() {
        let in_rex = env(&[("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")]);
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let mut ours = Some(ours);
        let mut connect: Connect = Box::new(move |url| {
            assert_eq!("unix:///s", url);
            Ok(ours.take().unwrap())
        });
        let Chosen::Provider { held, spent } = choose(&in_rex, Some("tmux"), &mut connect) else {
            panic!("chose none");
        };
        assert_eq!("rex", held.name());
        assert!(spent < Duration::from_secs(3));
    }
}
