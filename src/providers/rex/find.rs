//! Rex's `find` (#92 decision 26, ADR 0006): whether a self-attached
//! terminal is the Rex block its `REX_BLOCK` names.
//!
//! Rex has no tty for a block, only its processes: `terminal.process` gives
//! the block's `child`, `login`'s on macOS. Its controlling terminal is the
//! block's, so it is compared with the attaching terminal's device, which
//! the daemon reads as it accepts the attach. Equal is in reach, with the
//! block as its id; another terminal, or a block not in Rex's session, is
//! out of reach. Where Rex or the system can't say, `REX_BLOCK` is trusted.

use crate::rex::client::{self, ResponseResult};
use crate::rex::ids;

/// The variable naming the attaching terminal's block: `find`'s only one.
pub(crate) const BLOCK_VAR: &str = "REX_BLOCK";

/// The device of process `pid`'s controlling terminal, as `st_rdev` has
/// one, if it has one and the system says.
pub(crate) type TtyOfPid = fn(u32) -> Option<u64>;

/// What a `find` came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// The block's terminal is the attach's.
    Proven,
    /// Out of reach, and why.
    Disproved(String),
    /// Rex or the system couldn't say, and why: `REX_BLOCK` is trusted.
    Unprovable(String),
}

/// The block `REX_BLOCK` names in `env`, the `find` variables' values, or
/// why it names none: out of reach.
pub(crate) fn block_in(env: &[(Vec<u8>, Vec<u8>)]) -> Result<String, String> {
    let Some(b) = crate::attach::env_value(env, BLOCK_VAR) else {
        return Err(format!("{BLOCK_VAR} is unset"));
    };
    match ids::block(b) {
        Ok(_) => Ok(String::from_utf8_lossy(b).into_owned()),
        Err(_) => Err(format!(
            "{BLOCK_VAR} isn't a block id: {}",
            String::from_utf8_lossy(b)
        )),
    }
}

/// The verdict on Rex's answer to `terminal.process` for the block, given
/// the attaching terminal's `device`.
pub(crate) fn verdict(
    result: &ResponseResult,
    device: Option<u64>,
    tty_of_pid: TtyOfPid,
) -> Verdict {
    let ok = match result {
        ResponseResult::Err(e) if e.code == "not_found" => {
            return Verdict::Disproved(format!("{BLOCK_VAR} names no block of this Rex session"));
        }
        ResponseResult::Err(e) => {
            return Verdict::Unprovable(format!(
                "Rex's terminal.process failed: {}: {}",
                e.code, e.message
            ));
        }
        ResponseResult::Ok(ok) => ok,
    };
    let Some(pid) = client::get_int(client::get(ok, "child"), "pid") else {
        return Verdict::Unprovable("Rex's terminal.process gave no child".into());
    };
    let Some(theirs) = u32::try_from(pid).ok().and_then(tty_of_pid) else {
        return Verdict::Unprovable(format!("no controlling terminal for pid {pid}"));
    };
    let Some(ours) = device else {
        return Verdict::Unprovable("the attaching terminal's device is unknown".into());
    };
    if ours == theirs {
        Verdict::Proven
    } else {
        Verdict::Disproved(format!("{BLOCK_VAR} names another terminal"))
    }
}

/// `struct kinfo_proc` (<sys/sysctl.h>), which `libc` doesn't have: 648
/// bytes on 64-bit macOS, `kp_eproc` at 296 and its `e_tdev` 276 into it.
#[cfg(any(target_os = "macos", test))]
const KINFO_PROC_SIZE: usize = 648;
#[cfg(any(target_os = "macos", test))]
const E_TDEV: usize = 296 + 276;

/// The controlling terminal in a `kinfo_proc`, `kinfo`, if it is one whole
/// and has one: `NODEV` is none.
#[cfg(any(target_os = "macos", test))]
fn e_tdev_of(kinfo: &[u8]) -> Option<u64> {
    const NODEV: u32 = u32::MAX;
    if kinfo.len() != KINFO_PROC_SIZE {
        return None;
    }
    let tdev = u32::from_ne_bytes(kinfo[E_TDEV..E_TDEV + 4].try_into().ok()?);
    (tdev != NODEV).then_some(u64::from(tdev))
}

/// Process `pid`'s controlling terminal: `sysctl KERN_PROC_PID`'s
/// `kp_eproc.e_tdev`, which works for another user's process, as `login` is.
#[cfg(target_os = "macos")]
pub(crate) fn tty_of_pid(pid: u32) -> Option<u64> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        i32::try_from(pid).ok()?,
    ];
    let mut buf = [0u64; KINFO_PROC_SIZE / 8];
    let mut len = KINFO_PROC_SIZE;
    // SAFETY: `buf` is valid for `len` bytes, and `mib` for its length.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // a pid with no process answers no bytes
    if r != 0 {
        return None;
    }
    // SAFETY: `buf` holds `len` initialized bytes, at most its size.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), len) };
    e_tdev_of(bytes)
}

/// Process `pid`'s controlling terminal: field 7 of `/proc/<pid>/stat`,
/// `tty_nr`, as a `dev_t`.
#[cfg(target_os = "linux")]
pub(crate) fn tty_of_pid(pid: u32) -> Option<u64> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    tty_nr_of_stat(&stat).map(|nr| {
        // the kernel's encoding: minor's low byte, major, then minor's rest
        let major = (nr >> 8) & 0xfff;
        let minor = (nr & 0xff) | ((nr >> 12) & 0xfff00);
        libc::makedev(major, minor) as u64
    })
}

/// Field 7 of a `/proc/<pid>/stat`, `tty_nr`, if not 0: none. Its second
/// field, the command's name in parentheses, may hold anything.
#[cfg(any(target_os = "linux", test))]
fn tty_nr_of_stat(stat: &[u8]) -> Option<u32> {
    let after = stat.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&stat[after + 1..]).ok()?;
    // state, ppid, pgrp, session, tty_nr
    let nr: i64 = rest.split_ascii_whitespace().nth(4)?.parse().ok()?;
    u32::try_from(nr).ok().filter(|&n| n != 0)
}

/// Where neither way is built: unprovable.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn tty_of_pid(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    use serde_json::{Value, json};

    use super::*;
    use crate::attach::PaneId;
    use crate::daemon::provider::{Answer, Ask, AskId, Outcome, PaneProvider, ProviderEvent};
    use crate::providers::rex::RexProvider;
    use crate::rex::session::tests::{sent, server_frame, session};

    const BLOCK: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

    /// The device the fake gives process 42's terminal; every other process
    /// has none.
    const LOGIN_TTY: u64 = 0x1000_0006;

    fn fake_tty_of_pid(pid: u32) -> Option<u64> {
        (pid == 42).then_some(LOGIN_TTY)
    }

    /// Rex's pane provider on a socket, the fake telling terminals, and
    /// Rex's end.
    fn rex() -> (RexProvider, UnixStream) {
        let (s, theirs) = session();
        let mut p = RexProvider::new(s);
        p.tty_of_pid = fake_tty_of_pid;
        (p, theirs)
    }

    /// A `find` for an attach with `REX_BLOCK` `block`, if any, on terminal
    /// `device`.
    fn find(block: Option<&str>, device: Option<u64>) -> Ask {
        Ask::Find {
            pane: PaneId::parse("1-abcdefghij").unwrap(),
            pid: 7,
            tty: "/dev/ttys006".into(),
            device,
            title: String::new(),
            env: block
                .map(|b| (BLOCK_VAR.as_bytes().to_vec(), b.as_bytes().to_vec()))
                .into_iter()
                .collect(),
        }
    }

    /// Rex sends `frame` as an answer to request 1, which the provider reads:
    /// what it then tells.
    fn rex_says(p: &mut RexProvider, theirs: &mut UnixStream, frame: Value) -> Vec<ProviderEvent> {
        let mut frame = frame;
        frame["type"] = json!("response");
        frame["id"] = json!("1");
        frame["seq"] = json!(0);
        theirs.write_all(&server_frame(&frame.to_string())).unwrap();
        p.on_ready(0);
        std::iter::from_fn(|| p.next()).collect()
    }

    fn child(pid: i64) -> Value {
        json!({ "payload": { "child": { "pid": pid, "user_id": 0, "name": "login" } } })
    }

    fn refused(code: &str) -> Value {
        json!({ "error": { "code": code, "message": "no", "status": 404 } })
    }

    fn outcome(events: &[ProviderEvent]) -> &Outcome {
        match events {
            [
                ProviderEvent::Answered {
                    id: AskId(9),
                    outcome,
                },
            ] => outcome,
            _ => panic!("{events:?}"),
        }
    }

    fn found() -> Outcome {
        Outcome::Done(Answer::Terminal(BLOCK.into()))
    }

    /// The requests Rex read, as method and payload, once `p` is dropped.
    fn requests(p: RexProvider, theirs: UnixStream) -> Vec<(String, Value)> {
        drop(p);
        sent(theirs)
            .iter()
            .map(|t| {
                let v: Value = serde_json::from_str(t).unwrap();
                (v["method"].as_str().unwrap().into(), v["payload"].clone())
            })
            .collect()
    }

    #[test]
    fn find_proves_the_block_whose_childs_terminal_is_the_attachs() {
        let (mut p, mut theirs) = rex();
        p.ask(AskId(9), find(Some(BLOCK), Some(LOGIN_TTY)));
        let told = rex_says(&mut p, &mut theirs, child(42));
        assert_eq!(&found(), outcome(&told));
        assert_eq!(
            vec![(
                "com.superlogical.terminal.process".to_string(),
                json!({ "session_id": "session:s", "block_id": BLOCK })
            )],
            requests(p, theirs)
        );
    }

    #[test]
    fn find_disproves_a_block_on_another_terminal_or_not_in_the_session() {
        // REX_BLOCK leaked into a terminal Rex doesn't run
        let (mut p, mut theirs) = rex();
        p.ask(AskId(9), find(Some(BLOCK), Some(LOGIN_TTY + 1)));
        let told = rex_says(&mut p, &mut theirs, child(42));
        assert!(
            matches!(outcome(&told), Outcome::Failed(why) if why == "REX_BLOCK names another terminal"),
            "{told:?}"
        );

        let (mut p, mut theirs) = rex();
        p.ask(AskId(9), find(Some(BLOCK), Some(LOGIN_TTY)));
        let told = rex_says(&mut p, &mut theirs, refused("not_found"));
        assert!(matches!(outcome(&told), Outcome::Failed(_)), "{told:?}");
    }

    #[test]
    fn find_without_a_rex_block_is_out_of_reach_unasked() {
        for block in [None, Some("block:short")] {
            let (mut p, theirs) = rex();
            p.ask(AskId(9), find(block, Some(LOGIN_TTY)));
            let told: Vec<_> = std::iter::from_fn(|| p.next()).collect();
            assert!(matches!(outcome(&told), Outcome::Failed(_)), "{told:?}");
            assert!(requests(p, theirs).is_empty());
        }
    }

    #[test]
    fn find_trusts_the_block_where_it_cant_be_proved() {
        let cases = [
            // the system won't say for this pid
            (child(43), Some(LOGIN_TTY)),
            // the attach's terminal is relayed, with no tty
            (child(42), None),
            // Rex can't say
            (refused("method_not_found"), Some(LOGIN_TTY)),
            (json!({ "payload": {} }), Some(LOGIN_TTY)),
        ];
        for (answer, device) in cases {
            let (mut p, mut theirs) = rex();
            p.ask(AskId(9), find(Some(BLOCK), device));
            let told = rex_says(&mut p, &mut theirs, answer.clone());
            assert_eq!(&found(), outcome(&told), "{answer}");
        }
    }

    #[test]
    fn a_stat_lines_tty_is_its_seventh_field_past_any_parenthesis_in_its_name() {
        let line = b"123 (a) (b c) S 1 123 123 34823 123 4194560";
        assert_eq!(Some(34823), tty_nr_of_stat(line));
        assert_eq!(None, tty_nr_of_stat(b"123 (sh) S 1 123 123 0 -1"));
        assert_eq!(None, tty_nr_of_stat(b"garbage"));
    }

    /// In a Rex pane, its own block, as a self-attach from it finds it, on
    /// macOS (and on Linux, #133). Skips elsewhere.
    #[test]
    fn live_find_proves_its_own_block() {
        let var = |name: &str| std::env::var(name).ok();
        let (Some(url), Some(session), Some(block)) =
            (var("REX_SERVER"), var("REX_SESSION"), var("REX_BLOCK"))
        else {
            eprintln!("skipping: not in a Rex pane");
            return;
        };
        // stdin, the block's terminal: as a helper hands it over (/dev/tty
        // is a device of its own)
        use std::os::fd::AsFd;
        // SAFETY: isatty only inspects the fd.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 0 {
            eprintln!("skipping: stdin isn't a terminal");
            return;
        }
        let tty = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
        let device = crate::daemon::attach_conn::tty_device(Some(&tty), b"");
        assert!(device.is_some());
        let stream = crate::rex::client::connect_to(&url).unwrap();
        let mut p = RexProvider::connecting(stream, &session).unwrap();
        let mut ask = find(Some(&block), device);
        if let Ask::Find { pid, .. } = &mut ask {
            *pid = std::process::id();
        }
        p.ask(AskId(9), ask);
        let fd = p.session.client.fd();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(std::time::Instant::now() < deadline, "no answer from Rex");
            let mut fds = [libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(fds.as_mut_ptr(), 1, 100) };
            p.on_ready(0);
            while let Some(e) = p.next() {
                if let ProviderEvent::Answered { outcome, .. } = e {
                    assert_eq!(
                        Outcome::Done(Answer::Terminal(block.as_str().into())),
                        outcome
                    );
                    // proven, not trusted
                    assert!(!p.trusted, "REX_BLOCK was trusted unproved");
                    return;
                }
            }
        }
    }

    #[test]
    fn a_kinfo_procs_terminal_is_its_e_tdev_unless_nodev_or_cut_short() {
        let mut kinfo = vec![0u8; KINFO_PROC_SIZE];
        kinfo[E_TDEV..E_TDEV + 4].copy_from_slice(&0x1000_0006u32.to_ne_bytes());
        assert_eq!(Some(0x1000_0006), e_tdev_of(&kinfo));
        kinfo[E_TDEV..E_TDEV + 4].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert_eq!(None, e_tdev_of(&kinfo));
        assert_eq!(None, e_tdev_of(&kinfo[..KINFO_PROC_SIZE - 1]));
        assert_eq!(None, e_tdev_of(&[]));
    }

    #[test]
    fn a_process_with_no_terminal_or_none_at_all_has_no_tty() {
        assert_eq!(None, tty_of_pid(u32::MAX - 1));
    }
}
