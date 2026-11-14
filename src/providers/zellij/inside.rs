//! `inside.zellij`, and the proof `find` runs first: whether an attach's
//! terminal is directly a pane of a zellij server, from the process table
//! alone, running no zellij command. zellij gives no pane's tty or pid,
//! and a command pane ignores OSC 2, so neither a tty nor a title can
//! prove one. What it starts in a pane, a shell or a command, leads its
//! own session on the pane's tty, a child of the server; whatever runs
//! from there shares that session.
//!
//! In order: `ZELLIJ_PANE_ID` and `ZELLIJ_SESSION_NAME` are set; the attach
//! has a tty; its process's session leader is on that tty; and the
//! leader's parent is a process whose base name holds `zellij`. A process
//! table it can't read proves nothing either way: the variables are
//! trusted, as `(unproven: …)` says.
//!
//! The variables alone prove nothing: a multiplexer's panes inherit
//! whatever started its server, so a tmux started in a zellij pane gives
//! every tmux pane that zellij pane's `ZELLIJ_*`.

use crate::config::{Attach, Proof};
use crate::providers::tty::{self, Process, Unread};

/// Whether `attach`'s terminal is a pane of a zellij server, and why.
pub(crate) fn inside(attach: &Attach) -> Proof {
    let var = |name| crate::attach::env_value(&attach.env, name);
    let looked = Looked {
        pane: var("ZELLIJ_PANE_ID"),
        session: var("ZELLIJ_SESSION_NAME"),
        tty: &attach.tty,
        pid: attach.pid,
    };
    match prove(&looked, tty::process, tty::of_pid) {
        Ok(proven) => Proof {
            ok: true,
            why: proven.to_string(),
        },
        Err(why) => Proof { ok: false, why },
    }
}

/// What the proof looks at: the attaching terminal's `ZELLIJ_PANE_ID` and
/// `ZELLIJ_SESSION_NAME`, its tty, and the attach's pid.
pub(super) struct Looked<'a> {
    pub pane: Option<&'a [u8]>,
    pub session: Option<&'a [u8]>,
    pub tty: &'a [u8],
    pub pid: u32,
}

/// The pane a proof found: `terminal_N`, and whether the process table
/// was read for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Proven {
    pub pane: u32,
    pub unproven: bool,
}

impl std::fmt::Display for Proven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "zellij pane terminal_{}", self.pane)?;
        if self.unproven {
            write!(f, " (unproven: can't read the process table)")?;
        }
        Ok(())
    }
}

/// The proof, reading the process table through `table` and each
/// process's terminal through `tty_of`: the pane, or why not.
pub(super) fn prove(
    looked: &Looked,
    table: impl Fn(u32) -> Result<Process, Unread>,
    tty_of: impl Fn(u32) -> Option<u64>,
) -> Result<Proven, String> {
    let pane = looked
        .pane
        .filter(|p| !p.is_empty())
        .ok_or("ZELLIJ_PANE_ID isn't set")?;
    let pane = std::str::from_utf8(pane)
        .ok()
        .filter(|p| p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| {
            format!(
                "ZELLIJ_PANE_ID {:?} isn't a terminal pane's number",
                String::from_utf8_lossy(pane)
            )
        })?;
    if looked.session.is_none_or(<[u8]>::is_empty) {
        return Err("ZELLIJ_SESSION_NAME isn't set".into());
    }
    if looked.tty.is_empty() {
        return Err("the attach has no tty".into());
    }
    let shown = String::from_utf8_lossy(looked.tty).into_owned();
    let device =
        tty::of_path(looked.tty).ok_or_else(|| format!("the attach's tty {shown:?} isn't one"))?;
    let unproven = Ok(Proven {
        pane,
        unproven: true,
    });
    let not = |why: String| {
        Err(format!(
            "this terminal ({shown}) isn't a zellij pane's: {why}"
        ))
    };
    let pid = looked.pid;
    let attach = match table(pid) {
        Ok(p) => p,
        Err(Unread::Gone) => return Err(format!("the attach's process (pid {pid}) has gone")),
        Err(Unread::Unreadable) => return unproven,
    };
    let at = attach.leader;
    let leader = match table(at) {
        Ok(p) => p,
        Err(Unread::Gone) => return not(format!("its session leader (pid {at}) has gone")),
        Err(Unread::Unreadable) => return unproven,
    };
    match tty_of(at) {
        Some(theirs) if theirs == device => {}
        Some(theirs) => {
            let path = tty::path_of_device(theirs).unwrap_or_else(|| "another terminal".into());
            return not(format!(
                "its session leader ({}, pid {at}) is on {path}",
                leader.name
            ));
        }
        None => {
            return not(format!(
                "its session leader ({}, pid {at}) has no terminal",
                leader.name
            ));
        }
    }
    let up = leader.parent;
    let parent = match table(up) {
        Ok(p) => p,
        Err(Unread::Gone) => {
            return not(format!("its session leader's parent (pid {up}) has gone"));
        }
        Err(Unread::Unreadable) => return unproven,
    };
    let base = parent.name.rsplit('/').next().unwrap_or_default();
    if !base.contains("zellij") {
        return not(format!(
            "its session leader's parent is {} (pid {up})",
            parent.name
        ));
    }
    Ok(Proven {
        pane,
        unproven: false,
    })
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, FromRawFd, OwnedFd};

    use super::*;

    /// A new pty, kept open: its slave's device and path.
    struct Pty {
        _master: OwnedFd,
        _slave: OwnedFd,
        device: u64,
        path: String,
    }

    fn pty() -> Pty {
        let (mut master, mut slave) = (-1, -1);
        let mut name = [0 as libc::c_char; 128];
        // SAFETY: valid out-pointers; `name` holds any pty's name.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                name.as_mut_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(0, r);
        // SAFETY: both are ours, owned from here; `name` is NUL-terminated.
        let (master, slave, path) = unsafe {
            (
                OwnedFd::from_raw_fd(master),
                OwnedFd::from_raw_fd(slave),
                std::ffi::CStr::from_ptr(name.as_ptr())
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let device = tty::of_fd(slave.as_fd()).unwrap();
        Pty {
            _master: master,
            _slave: slave,
            device,
            path,
        }
    }

    fn process(leader: u32, parent: u32, name: &str) -> Process {
        Process {
            leader,
            parent,
            name: name.into(),
        }
    }

    /// The attach (pid 4242) in zsh (4100), the session leader, whose
    /// parent is `server` (812); `on` is zsh's terminal.
    fn table(server: &'static str) -> impl Fn(u32) -> Result<Process, Unread> {
        move |pid| match pid {
            4242 => Ok(process(4100, 4100, "helper")),
            4100 => Ok(process(4100, 812, "zsh")),
            812 => Ok(process(812, 1, server)),
            _ => Err(Unread::Gone),
        }
    }

    fn looked<'a>(tty: &'a str) -> Looked<'a> {
        Looked {
            pane: Some(b"3"),
            session: Some(b"work"),
            tty: tty.as_bytes(),
            pid: 4242,
        }
    }

    #[test]
    fn a_terminal_whose_session_leader_a_zellij_server_started_is_its_pane() {
        let t = pty();
        let on = |_| Some(t.device);
        for server in ["zellij", "/opt/bin/zellij", "zellij-0.46"] {
            let proven = prove(&looked(&t.path), table(server), on).unwrap();
            assert_eq!(proven.to_string(), "zellij pane terminal_3", "{server}");
        }
    }

    #[test]
    fn the_variables_and_a_tty_come_first() {
        let t = pty();
        let on = |_| Some(t.device);
        let why = |l: &Looked| prove(l, table("zellij"), on).unwrap_err();
        let base = looked(&t.path);
        assert_eq!(
            why(&Looked { pane: None, ..base }),
            "ZELLIJ_PANE_ID isn't set"
        );
        let base = looked(&t.path);
        assert_eq!(
            why(&Looked {
                pane: Some(b""),
                ..base
            }),
            "ZELLIJ_PANE_ID isn't set"
        );
        let base = looked(&t.path);
        assert_eq!(
            why(&Looked {
                pane: Some(b"terminal_3"),
                ..base
            }),
            "ZELLIJ_PANE_ID \"terminal_3\" isn't a terminal pane's number"
        );
        let base = looked(&t.path);
        assert_eq!(
            why(&Looked {
                session: None,
                ..base
            }),
            "ZELLIJ_SESSION_NAME isn't set"
        );
        assert_eq!(why(&looked("")), "the attach has no tty");
        assert_eq!(why(&looked("/")), "the attach's tty \"/\" isn't one");
    }

    #[test]
    fn the_attachs_process_gone_or_the_leader_elsewhere_or_not_zellijs_disproves_it() {
        let t = pty();
        let other = pty();
        let on = |_| Some(t.device);
        let gone = Looked {
            pid: 1,
            ..looked(&t.path)
        };
        assert_eq!(
            prove(&gone, table("zellij"), on).unwrap_err(),
            "the attach's process (pid 1) has gone"
        );
        let elsewhere = prove(&looked(&t.path), table("zellij"), |_| Some(other.device));
        assert_eq!(
            elsewhere.unwrap_err(),
            format!(
                "this terminal ({}) isn't a zellij pane's: its session leader (zsh, pid 4100) is \
                 on {}",
                t.path,
                tty::path_of_device(other.device).unwrap()
            )
        );
        let none = prove(&looked(&t.path), table("zellij"), |_| None);
        assert_eq!(
            none.unwrap_err(),
            format!(
                "this terminal ({}) isn't a zellij pane's: its session leader (zsh, pid 4100) has \
                 no terminal",
                t.path
            )
        );
        let tmux = prove(&looked(&t.path), table("tmux"), on);
        assert_eq!(
            tmux.unwrap_err(),
            format!(
                "this terminal ({}) isn't a zellij pane's: its session leader's parent is tmux \
                 (pid 812)",
                t.path
            )
        );
        // a name only holding zellij in a directory isn't zellij
        let path = prove(&looked(&t.path), table("/zellij/bin/sh"), on);
        assert!(path.is_err());
    }

    #[test]
    fn a_process_table_it_cant_read_trusts_the_variables_unproven() {
        let t = pty();
        let on = |_| Some(t.device);
        for unreadable in [4242, 4100, 812] {
            let table = |pid| {
                if pid == unreadable {
                    return Err(Unread::Unreadable);
                }
                table("tmux")(pid)
            };
            let proven = prove(&looked(&t.path), table, on).unwrap();
            assert_eq!(
                proven.to_string(),
                "zellij pane terminal_3 (unproven: can't read the process table)"
            );
        }
    }

    #[test]
    fn a_process_on_a_terminal_of_no_zellij_pane_with_a_panes_variables_is_disproved() {
        // this test, under cargo: whatever its session leader is, it isn't
        // a child of zellij's, unless the tests run in a zellij pane
        let t = pty();
        let attach = Attach {
            env: vec![
                (b"ZELLIJ_PANE_ID".to_vec(), b"3".to_vec()),
                (b"ZELLIJ_SESSION_NAME".to_vec(), b"work".to_vec()),
            ],
            tty: t.path.clone().into_bytes(),
            pid: std::process::id(),
            ..Default::default()
        };
        let proof = inside(&attach);
        assert!(!proof.ok, "{proof:?}");
        assert!(
            proof.why.starts_with(&format!(
                "this terminal ({}) isn't a zellij pane's: its session leader",
                t.path
            )),
            "{proof:?}"
        );
    }
}
