//! The proof that a terminal is directly a Herdr app pane's hc terminal,
//! which `find`, the config's `<name>.inside.herdr_app` and the built-in
//! default's proofs all run, within [`INSIDE_TIME`]:
//!
//! 1. The terminal's `HC_TERMINAL_ID` names a terminal of this machine: a
//!    remote one, `machine/t7`, is refused.
//! 2. The hc core at `HC_TERMINAL_SOCKET` says `hello` in generation 1,
//!    with `terminal.get` and `doc.get`.
//! 3. `terminal.get` finds the terminal live, and its process's
//!    controlling terminal (`providers::tty`) is the attach's tty, by
//!    device: `HC_TERMINAL_ID` alone isn't proof, as a multiplexer run in
//!    the pane passes it on to its own panes.
//! 4. One block of the core's doc shows it: its Herdr app pane.
//! 5. `herdr-app status` answers: a window under the attach's
//!    `HERDR_APP_HOME` shows the doc.
//!
//! Unprovable, the terminal having no process or its tty being
//! unreadable, `find` trusts `HC_TERMINAL_ID`, the terminal being live in
//! a pane, as herdr's trusts `HERDR_PANE_ID`; `inside.herdr_app` says no.

use std::time::{Duration, Instant};

use super::core::Core;
use super::doc::blocks_of;
use super::window::Window;
use super::{methods, versions};
use crate::attach::env_value;
use crate::config::{Attach, Capability, INSIDE_TIME, Proof};
use crate::providers::builtin::Given;
use crate::providers::command::commands_env;
use crate::providers::tty;

/// What an attach claims: its `HC_TERMINAL_ID` and `HC_TERMINAL_SOCKET`,
/// and its tty.
pub(super) struct Claim<'a> {
    pub terminal: Option<&'a [u8]>,
    pub socket: Option<&'a [u8]>,
    pub tty: &'a [u8],
}

/// The terminal a claim names, its pane, and whether the proof held.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Found {
    pub terminal: String,
    pub pane: String,
    /// None: proved. Some: unprovable, and why.
    pub unproven: Option<String>,
}

/// Proves `claim` within `time`, reading a process's controlling terminal
/// through `device_of` (`tty::of_pid`), the window through `window`: the
/// terminal and its pane, or why it isn't the attach's.
pub(super) fn prove(
    claim: &Claim,
    time: Duration,
    device_of: impl Fn(u32) -> Option<u64>,
    window: impl Fn() -> Result<Window, String>,
) -> Result<Found, String> {
    let due = Instant::now() + time;
    let terminal = set(claim.terminal).ok_or("HC_TERMINAL_ID isn't set")?;
    let terminal = String::from_utf8_lossy(terminal).into_owned();
    if let Some((machine, _)) = terminal.split_once('/') {
        return Err(format!(
            "hc terminal {terminal} is on machine {machine}: the built-in herdr-app \
             doesn't reach remote spaces"
        ));
    }
    let socket = set(claim.socket).ok_or("HC_TERMINAL_SOCKET isn't set")?;
    if claim.tty.is_empty() {
        return Err("the attach has no tty".into());
    }
    let shown = String::from_utf8_lossy(claim.tty).into_owned();
    let ours =
        tty::of_path(claim.tty).ok_or_else(|| format!("the attach's tty {shown:?} isn't one"))?;
    let app = window()?;
    let mut core = Core::connect(socket, time)?;
    if let Some(why) = core.lacks(methods(Capability::Find)) {
        return Err(format!(
            "{why} ({})",
            versions(&core.hello().version, &window)
        ));
    }
    let record = core.terminal(&terminal)?;
    if !record.live {
        return Err(format!("hc terminal {terminal} has finished"));
    }
    let unproven = match record.pid {
        None => Some(format!("hc terminal {terminal} has no process")),
        Some(pid) => match device_of(pid) {
            None => Some(format!(
                "can't read the terminal of hc terminal {terminal}'s process (pid {pid})"
            )),
            Some(theirs) if theirs != ours => {
                let path =
                    tty::path_of_device(theirs).unwrap_or_else(|| format!("device {theirs:#x}"));
                return Err(format!(
                    "this terminal ({shown}) isn't hc terminal {terminal}'s ({path})"
                ));
            }
            Some(_) => None,
        },
    };
    let doc = core.doc()?;
    let pane = match blocks_of(&doc, &terminal).as_slice() {
        [] => return Err(format!("{terminal} has no Herdr app pane")),
        [one] => one.pane.clone(),
        several => {
            return Err(format!(
                "{terminal} shows in {} Herdr app panes",
                several.len()
            ));
        }
    };
    app.status(due).map_err(String::from)?;
    Ok(Found {
        terminal,
        pane,
        unproven,
    })
}

/// `v`, unless unset or empty.
fn set(v: Option<&[u8]>) -> Option<&[u8]> {
    v.filter(|v| !v.is_empty())
}

/// `find`: the attaching terminal's hc terminal, or why it isn't one.
pub(super) fn find(given: &Given) -> Result<String, String> {
    find_by(given, tty::of_pid)
}

/// `find`, reading a process's terminal through `device_of`.
fn find_by(given: &Given, device_of: impl Fn(u32) -> Option<u64>) -> Result<String, String> {
    let (terminal, socket) = (
        given.var("env_hc_terminal_id"),
        given.env("HC_TERMINAL_SOCKET"),
    );
    let tty = given.var("tty").unwrap_or_default();
    let claim = Claim {
        terminal: terminal.as_deref(),
        socket: socket.as_deref(),
        tty: &tty,
    };
    let found = prove(&claim, INSIDE_TIME, device_of, || Window::of(given))?;
    if let Some(why) = &found.unproven {
        eprintln!("herdr-app find: {why}: trusting HC_TERMINAL_ID, as its pane is there");
    }
    Ok(found.terminal)
}

/// `inside.herdr_app`: whether `attach`'s terminal is directly the hc
/// terminal its `HC_TERMINAL_ID` names, in a Herdr app pane, and why.
/// Unprovable is no.
pub(crate) fn inside(attach: &Attach) -> Proof {
    inside_by(attach, tty::of_pid, || {
        Window::within(commands_env(&attach.env))
    })
}

fn inside_by(
    attach: &Attach,
    device_of: impl Fn(u32) -> Option<u64>,
    window: impl Fn() -> Result<Window, String>,
) -> Proof {
    let env = |name| env_value(&attach.env, name);
    let claim = Claim {
        terminal: env("HC_TERMINAL_ID"),
        socket: env("HC_TERMINAL_SOCKET"),
        tty: &attach.tty,
    };
    match prove(&claim, INSIDE_TIME, device_of, window) {
        Ok(Found {
            terminal,
            pane,
            unproven: None,
        }) => Proof {
            ok: true,
            why: format!("hc terminal {terminal}, Herdr app pane {pane}"),
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

    use serde_json::json;

    use super::super::fake::{Exchange, FakeApp, FakeCore, recorded, recorded_one, recorded_run};
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

    /// The nested tab's core: `t1`…`t3` and its doc.
    fn core() -> FakeCore {
        FakeCore::replaying(&["nested-terminal.get", "nested-doc.get"])
    }

    /// A window that answers `status`.
    fn app() -> FakeApp {
        FakeApp::new(&[recorded_run("status"), recorded_run("version")])
    }

    /// An attach in hc terminal `t3` of `core`, on tty `tty`, its
    /// environment less `unset` and with `set`; `PATH` finding `app`.
    fn attach(
        core: &FakeCore,
        app: &FakeApp,
        tty: &str,
        unset: &[&str],
        set: &[(&str, &str)],
    ) -> Attach {
        let socket = String::from_utf8(core.path()).unwrap();
        let path = app.dir().to_string_lossy().into_owned();
        let env = [
            ("HC_TERMINAL_ID", "t3"),
            ("HC_TERMINAL_SOCKET", socket.as_str()),
            ("PATH", path.as_str()),
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

    /// `inside.herdr_app` of `attach`, the window found as the real one
    /// is.
    fn inside_of(attach: &Attach, device_of: impl Fn(u32) -> Option<u64>) -> Proof {
        inside_by(attach, device_of, || {
            Window::within(commands_env(&attach.env))
        })
    }

    /// What `find` is given for `attach`, as the choosing attach.
    fn given_of(attach: &Attach) -> Given {
        let up = crate::name::upper();
        let mut env = attach.env.clone();
        env.push((format!("{up}_TTY").into_bytes(), attach.tty.clone()));
        if let Some(id) = env_value(&attach.env, "HC_TERMINAL_ID") {
            env.push((format!("{up}_ENV_HC_TERMINAL_ID").into_bytes(), id.to_vec()));
        }
        Given::from(move |name| env_value(&env, name).map(<[u8]>::to_vec))
    }

    fn proof(ok: bool, why: &str) -> Proof {
        Proof {
            ok,
            why: why.into(),
        }
    }

    /// `t3`'s process, pid 9932 in the recording, on `device`.
    fn on(device: u64) -> impl Fn(u32) -> Option<u64> {
        move |pid| (pid == 9932).then_some(device)
    }

    #[test]
    fn a_live_terminal_on_the_attachs_tty_in_one_pane_of_an_answering_window_is_proved() {
        let (tty, core, app) = (pty(), core(), app());
        let a = attach(&core, &app, &tty.path, &[], &[]);
        assert_eq!(
            inside_of(&a, on(tty.device)),
            proof(
                true,
                "hc terminal t3, Herdr app pane p4c229a9b57964c201d7ab577ccf4a2ea"
            )
        );
        assert_eq!(core.methods(), ["hello", "terminal.get", "doc.get"]);
        assert_eq!(core.asked()[1]["params"], json!({ "terminal": "t3" }));
        assert_eq!(app.asked(), ["status"]);
        assert_eq!(find_by(&given_of(&a), on(tty.device)), Ok("t3".into()));
    }

    #[test]
    fn a_terminal_whose_process_is_on_another_tty_is_disproved_naming_both() {
        let (ours, theirs, core, app) = (pty(), pty(), core(), app());
        let a = attach(&core, &app, &ours.path, &[], &[]);
        let why = format!(
            "this terminal ({}) isn't hc terminal t3's ({})",
            ours.path, theirs.path
        );
        assert_eq!(inside_of(&a, on(theirs.device)), proof(false, &why));
        assert_eq!(find_by(&given_of(&a), on(theirs.device)), Err(why));
    }

    #[test]
    fn a_remote_terminal_the_variables_and_tty_are_checked_before_the_core_is_asked() {
        let (tty, core, app) = (pty(), core(), app());
        type Case<'a> = (&'a [&'a str], &'a [(&'a str, &'a str)], &'a str, &'a str);
        let cases: [Case; 5] = [
            (
                &["HC_TERMINAL_ID"],
                &[],
                &tty.path,
                "HC_TERMINAL_ID isn't set",
            ),
            (
                &[],
                &[("HC_TERMINAL_SOCKET", "")],
                &tty.path,
                "HC_TERMINAL_SOCKET isn't set",
            ),
            (&[], &[], "", "the attach has no tty"),
            (
                &[],
                &[("HC_TERMINAL_ID", "mini/t7")],
                &tty.path,
                "hc terminal mini/t7 is on machine mini: the built-in herdr-app doesn't \
                 reach remote spaces",
            ),
            (
                &["PATH"],
                &[("HOME", "/no/home")],
                &tty.path,
                "no herdr-app on PATH, nor at /no/home/.local/bin/herdr-app",
            ),
        ];
        for (unset, set, tty_path, why) in cases {
            let a = attach(&core, &app, tty_path, unset, set);
            assert_eq!(
                inside_of(&a, on(tty.device)),
                proof(false, why),
                "{unset:?} {set:?}"
            );
        }
        assert!(core.asked().is_empty());
    }

    #[test]
    fn a_terminal_finished_unknown_paneless_or_in_two_panes_is_refused() {
        let (tty, app) = (pty(), app());
        let mut finished = recorded_one("terminal.get-finished", "terminal.get");
        finished.params = json!({ "terminal": "t3" });
        let finished = FakeCore::serve(vec![recorded_one("hello", "hello"), finished]);
        let a = attach(&finished, &app, &tty.path, &[], &[]);
        assert_eq!(
            inside_of(&a, on(tty.device)),
            proof(false, "hc terminal t3 has finished")
        );

        let unknown = FakeCore::replaying(&["terminal.get-not-found", "nested-doc.get"]);
        let a = attach(&unknown, &app, &tty.path, &[], &[("HC_TERMINAL_ID", "t99")]);
        assert_eq!(
            inside_of(&a, |_| Some(tty.device)),
            proof(false, "hc core: no terminal \"t99\"")
        );

        // t3's block gone from the doc
        let mut exchanges = recorded("nested-terminal.get");
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        let text = doc.replies[0].to_string().replace("\"t3\"", "\"t9\"");
        doc.replies[0] = serde_json::from_str(&text).unwrap();
        exchanges.push(doc.clone());
        let paneless = FakeCore::serve(exchanges);
        let a = attach(&paneless, &app, &tty.path, &[], &[]);
        assert_eq!(
            inside_of(&a, on(tty.device)),
            proof(false, "t3 has no Herdr app pane")
        );

        // t2's block showing t3 too
        let mut exchanges = recorded("nested-terminal.get");
        let mut doc = recorded_one("nested-doc.get", "doc.get");
        let text = doc.replies[0].to_string().replace("\"t2\"", "\"t3\"");
        doc.replies[0] = serde_json::from_str(&text).unwrap();
        exchanges.push(doc);
        let twice = FakeCore::serve(exchanges);
        let a = attach(&twice, &app, &tty.path, &[], &[]);
        assert_eq!(
            inside_of(&a, on(tty.device)),
            proof(false, "t3 shows in 2 Herdr app panes")
        );
    }

    #[test]
    fn no_window_or_a_silent_core_says_so() {
        let (tty, core) = (pty(), core());
        let none = FakeApp::new(&[recorded_run("err-no-window-status")]);
        let a = attach(&core, &none, &tty.path, &[], &[]);
        let p = inside_of(&a, on(tty.device));
        assert!(!p.ok);
        assert!(
            p.why.starts_with("herdr-app: no Herdr window at "),
            "{}",
            p.why
        );

        let silent = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            Exchange::silent("terminal.get", json!({ "terminal": "t3" })),
        ]);
        let app = app();
        let a = attach(&silent, &app, &tty.path, &[], &[]);
        assert_eq!(
            inside_of(&a, on(tty.device)),
            proof(false, "the hc core didn't answer in 0.5 s")
        );
    }

    #[test]
    fn a_core_of_another_generation_is_refused_naming_both_versions() {
        let (tty, app) = (pty(), app());
        let mut hello = recorded_one("hello", "hello");
        hello.result_mut()["generation"] = json!(2);
        hello.result_mut()["version"] = json!("0.2.0");
        let core = FakeCore::serve(vec![hello]);
        let a = attach(&core, &app, &tty.path, &[], &[]);
        assert_eq!(
            find_by(&given_of(&a), on(tty.device)),
            Err(
                "the hc core speaks generation 2; the built-in herdr-app speaks 1 \
                 (hc core 0.2.0, herdr-app 0.1.16)"
                    .into()
            )
        );
        assert_eq!(core.methods(), ["hello"]);
    }

    #[test]
    fn unprovable_is_no_to_inside_and_trusted_by_find() {
        let (tty, core, app) = (pty(), core(), app());
        let a = attach(&core, &app, &tty.path, &[], &[]);
        let why = "can't read the terminal of hc terminal t3's process (pid 9932)";
        assert_eq!(inside_of(&a, |_| None), proof(false, why));
        assert_eq!(find_by(&given_of(&a), |_| None), Ok("t3".into()));
    }
}
