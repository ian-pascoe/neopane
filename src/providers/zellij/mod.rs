//! The built-in zellij pane provider's command (ADR 0016): `provider
//! builtin zellij CAPABILITY`, which its table runs for each capability.
//! It acts on the zellij session the choosing attach runs in, as
//! `ZELLIJ_SESSION_NAME` names it, found through the attach's
//! `ZELLIJ_SOCKET_DIR` and `HOME` (`session.rs`).
//!
//! - Every zellij call is `zellij --session NAME action …`, without the
//!   choosing attach's `ZELLIJ_PANE_ID`, which zellij takes as the pane
//!   issuing it; only a split names one, its origin's.
//! - `find` (`find.rs`): the inside proof (`inside.rs`), then zellij 0.46.0
//!   at least, and the pane listed and not exited; prints `terminal_N`.
//! - `open_beside` and `close` (`panes.rs`): `new-pane --no-focus` right of
//!   or below the origin, then a `move-pane` swap for left or up, the
//!   tab's floats hidden around the split if they show; `close-pane`.
//! - `report_layout` (`layout.rs`): one area per tab, from `list-panes`
//!   and `list-tabs`.
//! - `inside.zellij` (`inside.rs`), no capability: the attach's process
//!   table proves its terminal a pane of a zellij server, running no
//!   zellij command; `find` runs the same proof.
//!
//! Each capability gives the line it prints, or why it failed; `run`
//! prints it, or the failure as one `zellij CAPABILITY: why` line on
//! stderr, and gives the exit status. Every terminal id read or given is
//! `terminal_` and digits.

mod find;
mod inside;
mod layout;
mod panes;
mod session;

pub(crate) use inside::inside;

use super::builtin::{DONE, FAILED, Given, UNSUPPORTED, print_line};
use crate::config::Capability;

/// Runs capability `cap` with `given`: prints what it came to, its
/// failure as one `zellij CAPABILITY: why` line on stderr, and gives its
/// exit status.
pub(crate) fn run(cap: Capability, given: &Given) -> u8 {
    let done = match cap {
        Capability::Find => find::find(given).map(Some),
        Capability::OpenBeside => panes::open_beside(given).map(Some),
        Capability::Close => panes::close(given).map(|()| None),
        Capability::ReportLayout => layout::report_layout(given).map(Some),
        _ => return UNSUPPORTED,
    };
    match done {
        Ok(Some(line)) => print_line(line.as_bytes()),
        Ok(None) => DONE,
        Err(why) => {
            eprintln!("zellij {}: {why}", cap.name());
            FAILED
        }
    }
}

/// The number of terminal pane `terminal_N`, if `id` is one: what
/// zellij's `-p` takes, and `ZELLIJ_PANE_ID` holds.
fn terminal_number(id: &[u8]) -> Option<u32> {
    let n = id.strip_prefix(b"terminal_")?;
    if n.is_empty() || !n.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(n).ok()?.parse().ok()
}

/// Input `name`'s terminal pane, `terminal_N`: its number, or why not.
fn terminal_of(given: &Given, name: &str) -> Result<u32, String> {
    let id = given.var(name).unwrap_or_default();
    terminal_number(&id).ok_or_else(|| {
        format!(
            "{} {:?} isn't a zellij terminal pane's id",
            name.to_ascii_uppercase(),
            String::from_utf8_lossy(&id)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminal_id_is_terminal_and_digits() {
        assert_eq!(Some(3), terminal_number(b"terminal_3"));
        assert_eq!(Some(120), terminal_number(b"terminal_120"));
        for not in [
            &b"3"[..],
            b"plugin_3",
            b"terminal_",
            b"terminal_3x",
            b"%3",
            b"",
        ] {
            assert_eq!(None, terminal_number(not), "{not:?}");
        }
    }
}
