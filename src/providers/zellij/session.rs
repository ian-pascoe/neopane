//! The zellij session the choosing attach runs in, and running `zellij`
//! on it. Every call is `zellij --session NAME action …`, `NAME` from
//! `ZELLIJ_SESSION_NAME`, in the choosing attach's environment: zellij
//! 0.46 finds a session by name through its socket directory
//! (`ZELLIJ_SOCKET_DIR`) and its cache directory (`HOME`'s), both the
//! attach's. `ZELLIJ_PANE_ID` is taken from every call but a split's,
//! which names its origin: zellij takes it as the pane issuing the
//! action.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::process::{Command, Stdio};

use crate::providers::builtin::Given;

/// The oldest zellij the built-in works with: on 0.44 a split beside a
/// given pane printed an id and opened nothing, and 0.45 is untested.
pub(super) const OLDEST: (u32, u32, u32) = (0, 46, 0);

/// A zellij session, by its name.
pub(super) struct Session {
    name: Vec<u8>,
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
        Ok(Session { name })
    }

    /// Its name, for messages.
    pub(super) fn name(&self) -> String {
        String::from_utf8_lossy(&self.name).into_owned()
    }

    /// `zellij --session NAME action args…`'s stdout, or why it failed.
    pub(super) fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, String> {
        self.run_as(None, args)
    }

    /// The same, issued from terminal pane `pane`: with
    /// `ZELLIJ_PANE_ID=pane`, which a `new-pane --no-focus` splits.
    pub(super) fn run_from<S: AsRef<OsStr>>(
        &self,
        pane: u32,
        args: &[S],
    ) -> Result<Vec<u8>, String> {
        self.run_as(Some(pane), args)
    }

    fn run_as<S: AsRef<OsStr>>(&self, pane: Option<u32>, args: &[S]) -> Result<Vec<u8>, String> {
        let mut cmd = Command::new("zellij");
        cmd.arg("--session")
            .arg(OsStr::from_bytes(&self.name))
            .arg("action")
            .args(args)
            .env_remove("ZELLIJ_PANE_ID")
            .stdin(Stdio::null());
        if let Some(pane) = pane {
            cmd.env("ZELLIJ_PANE_ID", pane.to_string());
        }
        let out = cmd.output().map_err(|e| format!("can't run zellij: {e}"))?;
        if out.status.success() {
            return Ok(out.stdout);
        }
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or_default();
        let shown: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        Err(format!(
            "zellij action {} failed ({}): {first}",
            shown.join(" "),
            out.status
        ))
    }
}

/// The installed zellij's version, as `zellij --version` prints it after
/// `zellij `, such as `0.46.0`.
pub(super) fn version() -> Result<String, String> {
    let out = Command::new("zellij")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("can't run zellij: {e}"))?;
    let shown = String::from_utf8_lossy(&out.stdout);
    let shown = shown.trim();
    Ok(shown.strip_prefix("zellij ").unwrap_or(shown).to_string())
}

/// Why zellij `version` is too old for the built-in, if it is: older
/// than [`OLDEST`]. A version it can't read, such as one built from a
/// branch, isn't.
pub(super) fn too_old(version: &str) -> Option<String> {
    let mut parts = version.split(['.', '-', '+']);
    let mut next = || parts.next().and_then(|p| p.parse::<u32>().ok());
    let (major, minor, patch) = (next()?, next()?, next()?);
    let (a, b, c) = OLDEST;
    ((major, minor, patch) < OLDEST).then(|| format!("zellij {version} is older than {a}.{b}.{c}"))
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
