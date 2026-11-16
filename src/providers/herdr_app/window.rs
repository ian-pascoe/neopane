//! The Herdr app window, through the `herdr-app` command: found on `PATH`,
//! else at `~/.local/bin/herdr-app`, where an installed Herdr.app keeps
//! it; else none, naming both places. Every run is `herdr-app ARGS…`,
//! answering one JSON object on stdout, killed at its deadline. It acts on
//! the window under `HERDR_APP_HOME` (else `~/.config/herdr-app`), as the
//! environment it runs in has it: the choosing attach's.
//!
//! A window act is `herdr-app do COMMAND JSON`, one [`Window::run`]; with
//! no window, `herdr-app` exits 2, saying so on stderr; a command it
//! refuses exits 1 with `{error}`.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::providers::builtin::Given;
use crate::providers::command::on_path;
use crate::providers::deadline::{Unfinished, output_by};

/// The most `--version` has.
const VERSION_TIME: Duration = Duration::from_millis(500);

/// The `herdr-app` command.
#[derive(Debug, Clone)]
pub(super) struct Window {
    program: PathBuf,
    /// The environment its runs have alone, `provider check`'s and an
    /// inside proof's; None: this process's.
    env: Option<Vec<(Vec<u8>, Vec<u8>)>>,
}

/// How a run of `herdr-app` failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ran {
    /// It couldn't start, or ran out of time: why.
    Failed(String),
    /// The app refused it, as `{error}`: the app's words.
    Refused(String),
    /// No window answers under its home: `herdr-app`'s stderr line.
    NoWindow(String),
    /// It exited otherwise, or printed no JSON object: why.
    Unreadable(String),
}

impl std::fmt::Display for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ran::Failed(why) | Ran::NoWindow(why) | Ran::Unreadable(why) => f.write_str(why),
            Ran::Refused(why) => write!(f, "herdr-app: {why}"),
        }
    }
}

impl From<Ran> for String {
    fn from(ran: Ran) -> String {
        ran.to_string()
    }
}

/// Where `herdr-app` is: on `path`, a `PATH`, else at
/// `~/.local/bin/herdr-app` under `home`; else why not, naming both.
pub(super) fn locate(path: Option<&[u8]>, home: Option<&[u8]>) -> Result<PathBuf, String> {
    if let Some(found) = on_path(path.unwrap_or_default(), "herdr-app") {
        return Ok(found.into());
    }
    let local = home
        .filter(|h| !h.is_empty())
        .map(|h| Path::new(OsStr::from_bytes(h)).join(".local/bin/herdr-app"));
    if let Some(local) = &local
        && is_executable(local)
    {
        return Ok(local.clone());
    }
    let local = local.map_or_else(
        || "~/.local/bin/herdr-app".to_string(),
        |l| l.display().to_string(),
    );
    Err(format!("no herdr-app on PATH, nor at {local}"))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

impl Window {
    /// The `herdr-app` the choosing attach's `PATH` or `HOME` finds, run in
    /// this process's environment, the attach's; or why there's none.
    pub(super) fn of(given: &Given) -> Result<Window, String> {
        let program = locate(given.env("PATH").as_deref(), given.env("HOME").as_deref())?;
        Ok(Window { program, env: None })
    }

    /// The `herdr-app` environment `env` finds, run in it alone.
    pub(super) fn within(env: Vec<(Vec<u8>, Vec<u8>)>) -> Result<Window, String> {
        let var = |name: &str| {
            env.iter()
                .find(|(k, _)| k == name.as_bytes())
                .map(|(_, v)| v.clone())
        };
        let program = locate(var("PATH").as_deref(), var("HOME").as_deref())?;
        Ok(Window {
            program,
            env: Some(env),
        })
    }

    /// Its path.
    pub(super) fn path(&self) -> &Path {
        &self.program
    }

    /// `herdr-app ARGS…`'s one JSON object, killed at `due`; or why
    /// there's none.
    pub(super) fn run<S: AsRef<OsStr>>(&self, args: &[S], due: Instant) -> Result<Value, Ran> {
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        let shown = format!("herdr-app {}", shown.join(" "));
        let (status, out, err) = self.exec(args, &shown, due)?;
        let first = String::from_utf8_lossy(&err)
            .lines()
            .next()
            .unwrap_or_default()
            .to_string();
        if status.code() == Some(2) && first.contains("no Herdr window") {
            return Err(Ran::NoWindow(first));
        }
        let reply: Option<Value> = serde_json::from_slice(&out).ok().filter(Value::is_object);
        match (status.success(), reply) {
            (_, Some(reply)) if reply["error"].is_string() => Err(Ran::Refused(
                reply["error"].as_str().unwrap_or_default().to_string(),
            )),
            (true, Some(reply)) => Ok(reply),
            (true, None) => Err(Ran::Unreadable(format!("{shown} printed no JSON object"))),
            (false, _) => Err(Ran::Unreadable(format!(
                "{shown} failed ({status}): {first}"
            ))),
        }
    }

    /// `herdr-app status`: the window answers, with what it shows.
    pub(super) fn status(&self, due: Instant) -> Result<Value, Ran> {
        self.run(&["status"], due)
    }

    /// Its version, as `herdr-app --version` prints it after `herdr-app `,
    /// such as `0.1.16`; killed at `due`, or [`VERSION_TIME`] from now if
    /// sooner.
    pub(super) fn version(&self, due: Instant) -> Result<String, Ran> {
        let shown = "herdr-app --version";
        let due = due.min(Instant::now() + VERSION_TIME);
        let (status, out, _) = self.exec(&["--version"], shown, due)?;
        let out = String::from_utf8_lossy(&out);
        let version = out.trim();
        let version = version.strip_prefix("herdr-app ").unwrap_or(version);
        if !status.success() || version.is_empty() {
            return Err(Ran::Failed(format!("{shown} failed ({status})")));
        }
        Ok(version.to_string())
    }

    /// Runs it with `args`: its status, stdout and stderr, killed if it
    /// hasn't exited by `due`, and not run past it.
    fn exec<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        shown: &str,
        due: Instant,
    ) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Ran> {
        if Instant::now() >= due {
            return Err(Ran::Failed(format!("{shown}: no time left to run it")));
        }
        let mut cmd = Command::new(&self.program);
        cmd.args(args);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(
                env.iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            );
        }
        match output_by(cmd, Some(due)) {
            Ok(out) => Ok((out.status, out.stdout, out.stderr)),
            Err(Unfinished::CantStart(e)) => Err(Ran::Failed(format!(
                "can't run {}: {e}",
                self.program.display()
            ))),
            Err(Unfinished::TimedOut) => Err(Ran::Failed(format!("{shown}: killed, out of time"))),
        }
    }

    /// A window run by `program`, a test's fake.
    #[cfg(test)]
    pub(super) fn faked(program: &Path) -> Window {
        Window {
            program: program.to_path_buf(),
            env: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeApp, recorded_run};
    use super::*;

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }

    #[test]
    fn status_is_the_one_json_object_it_prints() {
        let app = FakeApp::new(&[recorded_run("status")]);
        let status = Window::faked(&app.program()).status(soon()).unwrap();
        assert_eq!(status["focused_pane"], "p4c229a9b57964c201d7ab577ccf4a2ea");
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn no_window_a_refusal_and_an_unknown_reply_each_say_so() {
        let app = FakeApp::new(&[
            recorded_run("err-no-window-status"),
            recorded_run("err-unknown-command"),
        ]);
        let w = Window::faked(&app.program());
        let Err(Ran::NoWindow(why)) = w.status(soon()) else {
            panic!("a window");
        };
        assert!(
            why.starts_with("herdr-app: no Herdr window at /tmp/spi1214/apphome/herdr-app.sock"),
            "{why}"
        );
        let refused = w.run(&["do", "no.such", "{}"], soon()).unwrap_err();
        let Ran::Refused(why) = &refused else {
            panic!("{refused:?}")
        };
        assert!(why.starts_with("unknown command \"no.such\""), "{why}");
        let other = w.run(&["ls"], soon()).unwrap_err();
        assert_eq!(
            other,
            Ran::Unreadable(
                "herdr-app ls failed (exit status: 64): fake herdr-app: no run for: ls".into()
            )
        );
    }

    #[test]
    fn version_is_after_its_name() {
        let app = FakeApp::new(&[recorded_run("version")]);
        assert_eq!(
            Window::faked(&app.program()).version(soon()),
            Ok("0.1.16".into())
        );
    }

    #[test]
    fn version_keeps_to_the_deadline_it_is_given() {
        let mut slow = recorded_run("version");
        slow.sleep = Some(5.0);
        let app = FakeApp::new(&[slow]);
        let started = Instant::now();
        let ran =
            Window::faked(&app.program()).version(Instant::now() + Duration::from_millis(100));
        assert_eq!(
            ran,
            Err(Ran::Failed(
                "herdr-app --version: killed, out of time".into()
            ))
        );
        assert!(started.elapsed() < Duration::from_millis(400));
    }

    #[test]
    fn a_slow_app_is_killed_at_its_deadline() {
        let mut slow = recorded_run("status");
        slow.sleep = Some(5.0);
        let app = FakeApp::new(&[slow]);
        let started = Instant::now();
        let ran = Window::faked(&app.program())
            .status(Instant::now() + Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(
            ran,
            Ran::Failed("herdr-app status: killed, out of time".into())
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn it_is_found_on_path_then_in_local_bin_else_naming_both() {
        let app = FakeApp::new(&[]);
        let dir = app.dir().as_os_str().as_bytes();
        assert_eq!(locate(Some(dir), None), Ok(app.program()));
        // a home whose .local/bin has it
        let home = app.dir().join("home");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::os::unix::fs::symlink(app.program(), home.join(".local/bin/herdr-app")).unwrap();
        let home = home.as_os_str().as_bytes();
        assert_eq!(
            locate(Some(b"/nowhere"), Some(home)),
            Ok(Path::new(OsStr::from_bytes(home)).join(".local/bin/herdr-app"))
        );
        assert_eq!(
            locate(Some(b"/nowhere"), Some(b"/no/home")),
            Err("no herdr-app on PATH, nor at /no/home/.local/bin/herdr-app".into())
        );
        assert_eq!(
            locate(None, None),
            Err("no herdr-app on PATH, nor at ~/.local/bin/herdr-app".into())
        );
    }
}
