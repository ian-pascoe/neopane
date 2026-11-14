//! `find`: a zellij 0.46.0 or newer; the inside proof (`inside.rs`) for
//! the attaching terminal, its `ZELLIJ_PANE_ID` given as
//! `<NAME>_ENV_ZELLIJ_PANE_ID` and its session the choosing attach's, as
//! the context variables require; and the pane listed by the session, not
//! exited. It prints `terminal_N`, else exits 1.

use serde_json::Value;

use super::inside::{Looked, prove};
use super::session::{Session, too_old, version};
use crate::providers::builtin::Given;
use crate::providers::tty;

/// The attaching terminal's pane, or why there's none.
pub(super) fn find(given: &Given) -> Result<String, String> {
    let session = Session::of(given)?;
    if let Some(why) = too_old(&version()?) {
        return Err(why);
    }
    let pid = given.var("pid").unwrap_or_default();
    let pid = std::str::from_utf8(&pid)
        .ok()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| format!("PID {:?} isn't a pid", String::from_utf8_lossy(&pid)))?;
    let (pane, name, tty) = (
        given.var("env_zellij_pane_id"),
        given.env("ZELLIJ_SESSION_NAME"),
        given.var("tty").unwrap_or_default(),
    );
    let looked = Looked {
        pane: pane.as_deref(),
        session: name.as_deref(),
        tty: &tty,
        pid,
    };
    let proven = prove(&looked, tty::process, tty::of_pid)?;
    let panes = session.run(&["list-panes", "--json"])?;
    listed(&panes, proven.pane).map_err(|why| format!("session {}: {why}", session.name()))?;
    if proven.unproven {
        eprintln!("zellij find: {proven}");
    }
    Ok(format!("terminal_{}", proven.pane))
}

/// Whether `list-panes --json`, `panes`, lists terminal pane `pane`, not
/// exited, or why not.
fn listed(panes: &[u8], pane: u32) -> Result<(), String> {
    let panes: Vec<Value> =
        serde_json::from_slice(panes).map_err(|e| format!("list-panes printed no list: {e}"))?;
    let found = panes.iter().find(|p| {
        p["id"].as_u64() == Some(u64::from(pane)) && p["is_plugin"].as_bool() == Some(false)
    });
    match found {
        None => Err(format!("no pane terminal_{pane}")),
        Some(p) if p["exited"].as_bool() == Some(true) => {
            Err(format!("terminal_{pane} has exited"))
        }
        Some(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pane_must_be_listed_a_terminal_not_exited() {
        let panes = include_bytes!("fixtures/0.46-panes.json");
        assert_eq!(Ok(()), listed(panes, 0));
        assert_eq!(Ok(()), listed(panes, 6));
        assert_eq!(Err("no pane terminal_9".into()), listed(panes, 9));
        let exited = br#"[{"id": 2, "is_plugin": false, "exited": true}]"#;
        assert_eq!(Err("terminal_2 has exited".into()), listed(exited, 2));
        let plugin = br#"[{"id": 2, "is_plugin": true, "exited": false}]"#;
        assert!(listed(plugin, 2).is_err());
        assert!(listed(b"nonsense", 2).is_err());
    }
}
