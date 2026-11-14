//! The zellij session the choosing attach runs in, and running `zellij`
//! on it. Every call is `zellij --session NAME action …`, `NAME` from
//! `ZELLIJ_SESSION_NAME`, in the choosing attach's environment: zellij
//! 0.46 finds a session by name through its socket directory
//! (`ZELLIJ_SOCKET_DIR`) and its cache directory (`HOME`'s), both the
//! attach's. `ZELLIJ_PANE_ID` is taken from every call but a split's or
//! a float's, which names its origin: zellij takes it as the pane issuing
//! the action.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::providers::builtin::Given;

/// The oldest zellij the built-in works with: on 0.44 a split beside a
/// given pane printed an id and opened nothing, and 0.45 is untested.
pub(super) const OLDEST: (u32, u32, u32) = (0, 46, 0);

/// A zellij session, by its name.
pub(super) struct Session {
    name: Vec<u8>,
    /// `zellij`, found on `PATH`; a test's fake.
    program: OsString,
    /// The environment its calls run in alone, `provider check`'s; None:
    /// this process's.
    env: Option<Vec<(Vec<u8>, Vec<u8>)>>,
}

/// How a call of zellij failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Ran {
    /// It couldn't start: why.
    Failed(String),
    /// It exited unsuccessfully, with `code`, none if a signal ended it,
    /// its stderr's first line `said`: why.
    Exited {
        code: Option<i32>,
        said: String,
        why: String,
    },
    /// It was killed at its deadline, or never started for lack of time:
    /// whatever it was to do may be done or not.
    TimedOut(String),
    /// The session isn't there: ended, or never started, so none of its
    /// panes are.
    NoSession(String),
}

impl std::fmt::Display for Ran {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ran::Failed(why)
            | Ran::Exited { why, .. }
            | Ran::TimedOut(why)
            | Ran::NoSession(why) => f.write_str(why),
        }
    }
}

impl From<Ran> for String {
    fn from(ran: Ran) -> String {
        ran.to_string()
    }
}

impl Session {
    /// The session `ZELLIJ_SESSION_NAME` names, or why there's none.
    pub(super) fn of(given: &Given) -> Result<Session, String> {
        let name = given.env("ZELLIJ_SESSION_NAME").unwrap_or_default();
        if name.is_empty() {
            return Err(
                "ZELLIJ_SESSION_NAME isn't set: the choosing attach isn't in zellij".into(),
            );
        }
        Ok(Session {
            name,
            program: "zellij".into(),
            env: None,
        })
    }

    /// Session `name`, its calls run in environment `env` alone, `zellij`
    /// found on its `PATH`.
    pub(super) fn within(name: &[u8], env: Vec<(Vec<u8>, Vec<u8>)>) -> Session {
        Session {
            name: name.to_vec(),
            program: "zellij".into(),
            env: Some(env),
        }
    }

    /// Session `name`, its calls run by `program`, a fake zellij.
    #[cfg(test)]
    pub(super) fn faked(program: &std::path::Path, name: &str) -> Session {
        Session {
            name: name.into(),
            program: program.into(),
            env: None,
        }
    }

    /// Its name, for messages.
    pub(super) fn name(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    /// `zellij --session NAME action args…`'s stdout, or why it failed.
    pub(super) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, Ran> {
        self.run_as(None, args, None)
    }

    /// The same, issued from terminal pane `pane`, with
    /// `ZELLIJ_PANE_ID=pane`, which a `new-pane --no-focus` opens beside or
    /// over, in its tab.
    pub(super) fn run_from<S: AsRef<OsStr>>(&self, pane: u32, args: &[S]) -> Result<Vec<u8>, Ran> {
        self.run_as(Some(pane), args, None)
    }

    /// The same, killed at `due` if it hasn't exited by then, and not run
    /// at all past it; issued from terminal pane `pane`, if given, with
    /// `ZELLIJ_PANE_ID=pane`, which a `new-pane --no-focus` splits.
    pub(super) fn run_until<S: AsRef<OsStr>>(
        &self,
        pane: Option<u32>,
        args: &[S],
        due: Instant,
    ) -> Result<Vec<u8>, Ran> {
        self.run_as(pane, args, Some(due))
    }

    fn run_as<S: AsRef<OsStr>>(
        &self,
        pane: Option<u32>,
        args: &[S],
        due: Option<Instant>,
    ) -> Result<Vec<u8>, Ran> {
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        let shown = format!("zellij action {}", shown.join(" "));
        if due.is_some_and(|due| Instant::now() >= due) {
            return Err(Ran::TimedOut(format!("{shown}: no time left to run it")));
        }
        let mut cmd = Command::new(&self.program);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(
                env.iter()
                    .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
            );
        }
        cmd.arg("--session")
            .arg(OsStr::from_bytes(&self.name))
            .arg("action")
            .args(args)
            .env_remove("ZELLIJ_PANE_ID");
        if let Some(pane) = pane {
            cmd.env("ZELLIJ_PANE_ID", pane.to_string());
        }
        let (status, out, err) = exec(cmd, &shown, due)?;
        if status.success() {
            return Ok(out);
        }
        let err = String::from_utf8_lossy(&err);
        let first = err.lines().next().unwrap_or_default();
        let why = format!("{shown} failed ({status}): {first}");
        if self.is_gone(first) {
            return Err(Ran::NoSession(why));
        }
        Err(Ran::Exited {
            code: status.code(),
            said: first.to_string(),
            why,
        })
    }

    /// Whether `line`, the first of zellij's stderr, says this session
    /// isn't there, as 0.46 says it: `There is no active session!` with
    /// none at all, `Session 'NAME' not found. …` with others.
    fn is_gone(&self, line: &str) -> bool {
        let missing = format!("Session '{}' not found", self.name());
        line == "There is no active session!" || line.starts_with(&missing)
    }
}

/// Runs `cmd`, `shown` in messages, with stdin `/dev/null`: its status,
/// stdout and stderr, killed if it hasn't exited by `due`, if given.
fn exec(
    mut cmd: Command,
    shown: &str,
    due: Option<Instant>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Ran> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| Ran::Failed(format!("can't run zellij: {e}")))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut out);
            }
            out
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if due.is_none_or(|due| Instant::now() < due) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Ran::TimedOut(format!("{shown}: killed, out of time")));
            }
        }
    };
    Ok((
        status,
        stdout.join().unwrap_or_default(),
        stderr.join().unwrap_or_default(),
    ))
}

/// The installed zellij's version, as `zellij --version` prints it after
/// `zellij `, such as `0.46.0`.
pub(super) fn version() -> Result<String, String> {
    let out = Command::new("zellij")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("can't run zellij: {e}"))?;
    Ok(version_of(&out.stdout))
}

/// The installed zellij's version, run in environment `env` alone, `zellij`
/// found on its `PATH`, killed at `due`.
pub(super) fn version_within(env: &[(Vec<u8>, Vec<u8>)], due: Instant) -> Result<String, Ran> {
    let mut cmd = Command::new("zellij");
    cmd.arg("--version").env_clear().envs(
        env.iter()
            .map(|(k, v)| (OsStr::from_bytes(k), OsStr::from_bytes(v))),
    );
    let (_, out, _) = exec(cmd, "zellij --version", Some(due))?;
    Ok(version_of(&out))
}

/// The version `zellij --version` printed as `out`, after `zellij `, such
/// as `0.46.0`.
fn version_of(out: &[u8]) -> String {
    let shown = String::from_utf8_lossy(out);
    let shown = shown.trim();
    shown.strip_prefix("zellij ").unwrap_or(shown).to_string()
}

/// Why zellij `version` is too old for the built-in, if it is: older
/// than [`OLDEST`]. A version it can't read, such as one built from a
/// branch, isn't.
pub(super) fn too_old(version: &str) -> Option<String> {
    let (a, b, c) = OLDEST;
    is_too_old(version).then(|| format!("zellij {version} is older than {a}.{b}.{c}"))
}

/// Whether zellij `version` is older than [`OLDEST`]; one it can't read
/// isn't.
pub(super) fn is_too_old(version: &str) -> bool {
    let mut parts = version.split(['.', '-', '+']);
    let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
    matches!((next(), next(), next()), (Some(a), Some(b), Some(c)) if (a, b, c) < OLDEST)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zellij_older_than_0_46_0_is_too_old_naming_its_version() {
        assert_eq!(
            Some("zellij 0.45.1 is older than 0.46.0".into()),
            too_old("0.45.1")
        );
        assert!(too_old("0.44.0").is_some());
        for fine in [
            "0.46.0",
            "0.46.1",
            "0.47.0",
            "1.0.0",
            "0.46.0-rc1",
            "main",
            "",
        ] {
            assert_eq!(None, too_old(fine), "{fine}");
        }
    }
}
