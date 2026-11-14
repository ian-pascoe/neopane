//! The proof that a terminal is a herdr pane, which `find`, the config's
//! `<name>.inside.herdr` and the built-in default's proofs all run, within
//! [`INSIDE_TIME`]: `ping`, refusing a protocol below
//! [`PROTOCOL`](super::client::PROTOCOL); then `pane.process_info` for the
//! pane the terminal's `HERDR_PANE_ID` names, and the controlling terminal
//! of its `shell_pid` (`providers::tty`), compared with the attach's tty
//! by device. herdr's own `tty` field is always empty, and
//! `HERDR_PANE_ID` alone isn't proof: a tmux inside the pane passes it on
//! to its own panes.
//!
//! Unprovable, the pane having no process or its terminal being
//! unreadable, `find` trusts `HERDR_PANE_ID`, the pane being there, as
//! Rex's trusts `REX_BLOCK`; `inside.herdr` says no.

use std::time::Duration;

use serde_json::json;

use super::client::{Herdr, PROTOCOL, UNREADABLE, text};
use crate::attach::env_value;
use crate::config::{Attach, INSIDE_TIME, Proof};
use crate::providers::builtin::Given;
use crate::providers::tty;

/// What an attach claims: its `HERDR_ENV`, `HERDR_PANE_ID` and
/// `HERDR_SOCKET_PATH`, and its tty.
pub(super) struct Claim<'a> {
    pub herdr_env: Option<&'a [u8]>,
    pub pane: Option<&'a [u8]>,
    pub socket: Option<&'a [u8]>,
    pub tty: &'a [u8],
}

/// The pane a claim names, as herdr names it, whether the proof held,
/// and the protocol herdr speaks.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Found {
    pub pane: String,
    /// None: proved. Some: unprovable, and why.
    pub unproven: Option<String>,
    pub protocol: u64,
}

/// Proves `claim` within `time`, reading a process's controlling terminal
/// through `device_of` (`tty::of_pid`): the pane, or why it isn't the
/// attach's.
pub(super) fn prove(
    claim: &Claim,
    time: Duration,
    device_of: impl Fn(u32) -> Option<u64>,
) -> Result<Found, String> {
    if claim.herdr_env != Some(b"1") {
        return Err("HERDR_ENV isn't 1".into());
    }
    let pane = set(claim.pane).ok_or("HERDR_PANE_ID isn't set")?;
    let socket = set(claim.socket).ok_or("HERDR_SOCKET_PATH isn't set")?;
    if claim.tty.is_empty() {
        return Err("the attach has no tty".into());
    }
    let shown = String::from_utf8_lossy(claim.tty).into_owned();
    let ours =
        tty::of_path(claim.tty).ok_or_else(|| format!("the attach's tty {shown:?} isn't one"))?;
    let herdr = Herdr::at(socket, time);
    let protocol = protocol_of(&herdr)?;
    let pane = String::from_utf8_lossy(pane);
    let info = herdr.call("pane.process_info", json!({ "pane_id": pane }))?;
    let info = &info["process_info"];
    let pane = text(info, "pane_id")?.to_string();
    let found = |unproven: Option<String>| Found {
        pane: pane.clone(),
        unproven,
        protocol,
    };
    let pid = match &info["shell_pid"] {
        serde_json::Value::Null => {
            return Ok(found(Some(format!("herdr pane {pane} has no process"))));
        }
        pid => pid
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .ok_or(UNREADABLE)?,
    };
    let Some(theirs) = device_of(pid) else {
        return Ok(found(Some(format!(
            "can't read the terminal of herdr pane {pane}'s process (pid {pid})"
        ))));
    };
    if theirs != ours {
        let path = tty::path_of_device(theirs).unwrap_or_else(|| format!("device {theirs:#x}"));
        return Err(format!(
            "this terminal ({shown}) isn't herdr pane {pane}'s ({path})"
        ));
    }
    Ok(found(None))
}

/// `v`, unless unset or empty.
fn set(v: Option<&[u8]>) -> Option<&[u8]> {
    v.filter(|v| !v.is_empty())
}

/// The protocol herdr speaks, from its `ping`, or why it's refused: one
/// below [`PROTOCOL`], naming both.
fn protocol_of(herdr: &Herdr) -> Result<u64, String> {
    let pong = herdr.call("ping", json!({}))?;
    let protocol = pong["protocol"].as_u64().ok_or(UNREADABLE)?;
    if protocol < PROTOCOL {
        let version = pong["version"].as_str().unwrap_or("?");
        return Err(format!(
            "herdr {version} speaks protocol {protocol}; the built-in herdr needs {PROTOCOL} or newer"
        ));
    }
    Ok(protocol)
}

/// `find`: the attaching terminal's pane, or why it isn't one.
pub(super) fn find(given: &Given) -> Result<String, String> {
    find_by(given, tty::of_pid)
}

/// `find`, reading a process's terminal through `device_of`.
fn find_by(given: &Given, device_of: impl Fn(u32) -> Option<u64>) -> Result<String, String> {
    let (herdr_env, pane, socket) = (
        given.env("HERDR_ENV"),
        given.var("env_herdr_pane_id"),
        given.env("HERDR_SOCKET_PATH"),
    );
    let tty = given.var("tty").unwrap_or_default();
    let claim = Claim {
        herdr_env: herdr_env.as_deref(),
        pane: pane.as_deref(),
        socket: socket.as_deref(),
        tty: &tty,
    };
    let found = prove(&claim, INSIDE_TIME, device_of)?;
    if found.protocol > PROTOCOL {
        eprintln!(
            "herdr find: herdr speaks protocol {}, newer than {PROTOCOL}: trying it",
            found.protocol
        );
    }
    if let Some(why) = &found.unproven {
        eprintln!("herdr find: {why}: trusting HERDR_PANE_ID, as the pane is there");
    }
    Ok(found.pane)
}

/// `inside.herdr`: whether `attach`'s terminal is directly the herdr pane
/// its `HERDR_PANE_ID` names, and why. Unprovable is no.
pub(crate) fn inside(attach: &Attach) -> Proof {
    inside_by(attach, tty::of_pid)
}

fn inside_by(attach: &Attach, device_of: impl Fn(u32) -> Option<u64>) -> Proof {
    let env = |name| env_value(&attach.env, name);
    let claim = Claim {
        herdr_env: env("HERDR_ENV"),
        pane: env("HERDR_PANE_ID"),
        socket: env("HERDR_SOCKET_PATH"),
        tty: &attach.tty,
    };
    match prove(&claim, INSIDE_TIME, device_of) {
        Ok(Found {
            pane,
            unproven: None,
            ..
        }) => Proof {
            ok: true,
            why: format!("herdr pane {pane}"),
        },
        Ok(Found {
            unproven: Some(why),
            ..
        })
        | Err(why) => Proof { ok: false, why },
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    use super::super::fake::{Fake, Reply, recorded};
    use super::*;

    /// A new pty, kept open while its fds are: its slave's path and
    /// device.
    struct Pty {
        _fds: (OwnedFd, OwnedFd),
        path: String,
        device: u64,
    }

    fn pty() -> Pty {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: both fds are written; the rest may be null.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(0, r);
        // SAFETY: both are ours, owned from here.
        let fds = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let mut name = [0u8; 256];
        // SAFETY: `name` is valid for its length.
        assert_eq!(0, unsafe {
            libc::ttyname_r(fds.1.as_raw_fd(), name.as_mut_ptr().cast(), name.len())
        });
        let end = name.iter().position(|&b| b == 0).unwrap();
        let path = String::from_utf8(name[..end].to_vec()).unwrap();
        let device = tty::of_path(path.as_bytes()).unwrap();
        Pty {
            _fds: fds,
            path,
            device,
        }
    }

    /// A herdr 0.9.5 that answers `ping`, and `pane.process_info` with
    /// `info`.
    fn herdr(info: String) -> Fake {
        Fake::serve(vec![
            ("ping", Reply::Line(recorded("ping"))),
            ("pane.process_info", Reply::Line(info)),
        ])
    }

    /// An attach in pane `w1:p1` of `fake`, on tty `tty`, its environment
    /// less `unset` and with `set`.
    fn attach(fake: &Fake, tty: &str, unset: &[&str], set: &[(&str, &str)]) -> Attach {
        let socket = String::from_utf8(fake.path()).unwrap();
        let env = [
            ("HERDR_ENV", "1"),
            ("HERDR_PANE_ID", "w1:p1"),
            ("HERDR_SOCKET_PATH", socket.as_str()),
        ]
        .into_iter()
        .filter(|(k, _)| !unset.contains(k) && !set.iter().any(|(s, _)| s == k))
        .chain(set.iter().copied())
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
        Attach {
            env,
            tty: tty.as_bytes().to_vec(),
            ..Attach::default()
        }
    }

    fn proof(ok: bool, why: &str) -> Proof {
        Proof {
            ok,
            why: why.into(),
        }
    }

    /// `w1:p1`'s process, pid 52774 in the recording, on `device`.
    fn on(device: u64) -> impl Fn(u32) -> Option<u64> {
        move |pid| (pid == 52774).then_some(device)
    }

    #[test]
    fn a_pane_whose_process_is_on_the_attachs_terminal_is_proved() {
        let tty = pty();
        let fake = herdr(recorded("process-info"));
        let a = attach(&fake, &tty.path, &[], &[]);
        assert_eq!(
            inside_by(&a, on(tty.device)),
            proof(true, "herdr pane w1:p1")
        );
        assert_eq!(fake.methods(), ["ping", "pane.process_info"]);
        assert_eq!(fake.asked()[1]["params"]["pane_id"], "w1:p1");
    }

    #[test]
    fn a_pane_whose_process_is_on_another_terminal_is_disproved_naming_both() {
        let (ours, theirs) = (pty(), pty());
        let fake = herdr(recorded("process-info"));
        let a = attach(&fake, &ours.path, &[], &[]);
        let why = format!(
            "this terminal ({}) isn't herdr pane w1:p1's ({})",
            ours.path, theirs.path
        );
        assert_eq!(inside_by(&a, on(theirs.device)), proof(false, &why));
        let given = given_of(&a);
        assert_eq!(find_by(&given, on(theirs.device)), Err(why));
    }

    #[test]
    fn the_variables_and_tty_are_checked_before_herdr_is_asked() {
        let tty = pty();
        let fake = herdr(recorded("process-info"));
        type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str, &'a str);
        let cases: [Case; 5] = [
            (&["HERDR_ENV"], &[], &tty.path, "HERDR_ENV isn't 1"),
            (&[], &[("HERDR_ENV", "0")], &tty.path, "HERDR_ENV isn't 1"),
            (
                &["HERDR_PANE_ID"],
                &[],
                &tty.path,
                "HERDR_PANE_ID isn't set",
            ),
            (
                &["HERDR_SOCKET_PATH"],
                &[],
                &tty.path,
                "HERDR_SOCKET_PATH isn't set",
            ),
            (&[], &[], "", "the attach has no tty"),
        ];
        for (unset, set, tty_path, why) in cases {
            let a = attach(&fake, tty_path, unset, set);
            assert_eq!(
                inside_by(&a, on(tty.device)),
                proof(false, why),
                "{unset:?} {set:?}"
            );
        }
        assert!(fake.asked().is_empty());
    }

    #[test]
    fn herdr_unreachable_silent_refusing_or_unreadable_says_so() {
        let tty = pty();
        let gone = attach(
            &herdr(String::new()),
            &tty.path,
            &["HERDR_SOCKET_PATH"],
            &[("HERDR_SOCKET_PATH", "/nowhere/h.sock")],
        );
        let p = inside_by(&gone, on(tty.device));
        assert!(!p.ok);
        assert!(
            p.why.starts_with("can't reach herdr at /nowhere/h.sock: "),
            "{}",
            p.why
        );

        let silent = Fake::serve(vec![("ping", Reply::Silence)]);
        let a = attach(&silent, &tty.path, &[], &[]);
        assert_eq!(
            inside_by(&a, on(tty.device)),
            proof(false, "herdr didn't answer in 0.5 s")
        );

        let missing = herdr(recorded("pane-not-found"));
        let a = attach(&missing, &tty.path, &[], &[]);
        assert_eq!(
            inside_by(&a, on(tty.device)),
            proof(false, "herdr: pane not found")
        );

        let garbled = herdr(r#"{"id":"x","result":{"type":"pane_process_info"}}"#.into());
        let a = attach(&garbled, &tty.path, &[], &[]);
        assert_eq!(
            inside_by(&a, on(tty.device)),
            proof(false, "herdr's answer can't be read")
        );
    }

    #[test]
    fn unprovable_is_no_to_inside_and_trusted_by_find() {
        let tty = pty();
        // the recording, its pane's process gone
        let none = recorded("process-info").replace("\"shell_pid\":52774", "\"shell_pid\":null");
        assert_ne!(none, recorded("process-info"));
        let fake = herdr(none);
        let a = attach(&fake, &tty.path, &[], &[]);
        let why = "herdr pane w1:p1 has no process";
        assert_eq!(inside_by(&a, on(tty.device)), proof(false, why));
        assert_eq!(find_by(&given_of(&a), on(tty.device)), Ok("w1:p1".into()));

        let fake = herdr(recorded("process-info"));
        let a = attach(&fake, &tty.path, &[], &[]);
        let why = "can't read the terminal of herdr pane w1:p1's process (pid 52774)";
        assert_eq!(inside_by(&a, |_| None), proof(false, why));
        assert_eq!(find_by(&given_of(&a), |_| None), Ok("w1:p1".into()));
    }

    #[test]
    fn find_refuses_a_protocol_below_22_naming_both_and_tries_a_newer_one() {
        let tty = pty();
        let old = recorded("ping")
            .replace("\"protocol\":22", "\"protocol\":21")
            .replace("0.9.5", "0.9.4");
        let fake = Fake::serve(vec![
            ("ping", Reply::Line(old)),
            ("pane.process_info", Reply::Line(recorded("process-info"))),
        ]);
        let a = attach(&fake, &tty.path, &[], &[]);
        assert_eq!(
            find_by(&given_of(&a), on(tty.device)),
            Err("herdr 0.9.4 speaks protocol 21; the built-in herdr needs 22 or newer".into())
        );
        assert_eq!(fake.methods(), ["ping"]);

        let newer = recorded("ping").replace("\"protocol\":22", "\"protocol\":23");
        let fake = Fake::serve(vec![
            ("ping", Reply::Line(newer)),
            ("pane.process_info", Reply::Line(recorded("process-info"))),
        ]);
        let a = attach(&fake, &tty.path, &[], &[]);
        assert_eq!(find_by(&given_of(&a), on(tty.device)), Ok("w1:p1".into()));
    }

    /// What `find` is given for `attach`, as the choosing attach: its
    /// environment, its tty as `TTY` and its `HERDR_PANE_ID` as
    /// `ENV_HERDR_PANE_ID`.
    fn given_of(attach: &Attach) -> Given {
        let up = crate::name::upper();
        let mut env = attach.env.clone();
        env.push((format!("{up}_TTY").into_bytes(), attach.tty.clone()));
        if let Some(pane) = env_value(&attach.env, "HERDR_PANE_ID") {
            env.push((
                format!("{up}_ENV_HERDR_PANE_ID").into_bytes(),
                pane.to_vec(),
            ));
        }
        Given::from(move |name| env_value(&env, name).map(<[u8]>::to_vec))
    }
}
