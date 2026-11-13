//! `provider check`'s Rex section (#94 decision 33): the Rex server the
//! attach's `REX_SERVER` names, its version and API revision, the
//! capabilities it is too old for, and what Rex's `find` proves of this
//! terminal, on a connection of its own, within the choice's 3 s. It
//! replaces `debug-rex`'s connection check.

use std::time::{Duration, Instant};

use serde_json::json;

use super::find::{Find, Tty};
use crate::attach::env_value;
use crate::daemon::provider::Capability;
use crate::name::NAME;
use crate::providers::rex::find::{self as rex_find, TtyOfPid, Verdict};
use crate::providers::rex::revision_needed;
use crate::rex::client::{self, Client, Message, ResponseResult};

/// What `provider check` learned of Rex.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RexReport {
    /// `REX_SERVER`.
    pub server: String,
    /// `server.version`'s `rex_version`.
    pub version: Option<String>,
    /// `server.version`'s, else the welcome's.
    pub api_revision: Option<i64>,
    /// Each capability the server's revision is too old for, and the
    /// revision it needs.
    pub missing: Vec<(Capability, i64)>,
    /// Why Rex can't be the pane provider, if it can't: no pane provider.
    pub error: Option<String>,
}

/// What Rex's `find` proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RexVerdict {
    /// The block's terminal is this one.
    Proven,
    /// Out of reach, and why.
    Disproved(String),
    /// Rex or the system couldn't say, and why: `REX_BLOCK` is trusted.
    Unprovable(String),
}

/// The time Rex has to answer: the choice's.
const TIME: Duration = crate::config::TIME;

/// Asks the Rex server and session `env` names what it is, and, with
/// `finding`, this terminal, whether it is the block `REX_BLOCK` names.
pub(super) fn probe(
    env: &[(Vec<u8>, Vec<u8>)],
    finding: Option<&Tty>,
) -> (RexReport, Option<Find>) {
    let var = |name: &str| String::from_utf8_lossy(env_value(env, name).unwrap_or_default());
    let server = var("REX_SERVER").into_owned();
    let session = var("REX_SESSION").into_owned();
    let started = Instant::now();
    let client = client::connect_to(&server).and_then(|stream| Client::start(stream, NAME));
    let client = match client {
        Ok(c) => c,
        Err(e) => {
            let report = RexReport {
                server,
                version: None,
                api_revision: None,
                missing: Vec::new(),
                error: Some(format!("can't reach Rex: {}", unwrapped(e))),
            };
            return (report, None);
        }
    };
    let find = finding.map(|tty| FindIn {
        block: rex_find::block_in(env),
        device: tty.device,
    });
    let due = started + TIME;
    asked(client, server, &session, find, due, rex_find::tty_of_pid)
}

/// What `find` is asked: the block `REX_BLOCK` names, or why none, and
/// this terminal's device.
pub(super) struct FindIn {
    pub block: Result<String, String>,
    pub device: Option<u64>,
}

/// A connection error as the daemon words it: an I/O error's own text.
fn unwrapped(e: client::ClientError) -> String {
    match e {
        client::ClientError::Io(e) => e.to_string(),
        e => e.to_string(),
    }
}

/// Asks Rex on `client`, connecting to `server`: `server.version`, and with
/// `find`, `session.attach` to `session`, then the block's processes,
/// whose terminal `tty_of_pid` tells. Gives up at `due`.
pub(super) fn asked(
    mut client: Client,
    server: String,
    session: &str,
    find: Option<FindIn>,
    due: Instant,
    tty_of_pid: TtyOfPid,
) -> (RexReport, Option<Find>) {
    let started = Instant::now();
    let mut report = RexReport {
        server,
        version: None,
        api_revision: None,
        missing: Vec::new(),
        error: None,
    };
    let mut found = None;
    let mut block = String::new();
    let mut device = None;
    let mut attach_id = None;
    let mut process_id = None;
    let fail = |report: &mut RexReport, why: String| {
        report.error.get_or_insert(why);
    };
    let version_id = match client.request("server.version", None) {
        Ok(id) => Some(id),
        Err(e) => {
            fail(&mut report, format!("can't ask Rex: {e}"));
            None
        }
    };
    match find {
        // refused unasked, as the daemon's is
        Some(FindIn {
            block: Err(why), ..
        }) => {
            found = Some(Find::Rex {
                took: Duration::ZERO,
                block: String::new(),
                verdict: RexVerdict::Disproved(why),
            });
        }
        Some(FindIn {
            block: Ok(b),
            device: d,
        }) => {
            block = b;
            device = d;
            match client.request("session.attach", Some(&json!({ "session_id": session }))) {
                Ok(id) => attach_id = Some(id),
                Err(e) => fail(&mut report, format!("can't ask Rex: {e}")),
            }
        }
        None => {}
    }
    let waiting = |report: &RexReport, found: &Option<Find>, attach_id: Option<u64>| {
        report.error.is_none()
            && (report.version.is_none() && version_id.is_some()
                || (attach_id.is_some() && found.is_none()))
    };
    while waiting(&report, &found, attach_id) {
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            fail(
                &mut report,
                format!("no answer within {} s", TIME.as_secs()),
            );
            break;
        }
        let mut fds = [libc::pollfd {
            fd: client.fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let ms = left.as_millis().clamp(1, 1000) as libc::c_int;
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        match client.read() {
            Ok(true) => {}
            Ok(false) => fail(&mut report, "Rex hung up".into()),
            Err(e) => fail(&mut report, format!("Rex failed: {e}")),
        }
        if client.welcomed() && report.api_revision.is_none() && report.version.is_none() {
            report.api_revision = Some(client.api_revision);
        }
        loop {
            let message = match client.next() {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    fail(&mut report, format!("Rex failed: {e}"));
                    break;
                }
            };
            let r = match message {
                Message::Response(r) => r,
                Message::Event(_) => continue,
                Message::Closed(_) => {
                    fail(&mut report, "Rex closed the connection".into());
                    break;
                }
            };
            if Some(r.id) == version_id {
                report.version = Some(match &r.result {
                    ResponseResult::Ok(v) => {
                        // the server's own; the welcome's is the one agreed
                        if let Some(rev) = client::get_int(v, "api_revision") {
                            report.api_revision = Some(rev);
                        }
                        client::get_str(v, "rex_version")
                            .unwrap_or("unknown")
                            .to_string()
                    }
                    ResponseResult::Err(e) => format!("unknown ({}: {})", e.code, e.message),
                });
            } else if Some(r.id) == attach_id && process_id.is_none() {
                if let ResponseResult::Err(e) = &r.result {
                    fail(
                        &mut report,
                        format!("Rex refused session.attach: {}: {}", e.code, e.message),
                    );
                    break;
                }
                let payload = json!({ "session_id": session, "block_id": block });
                match client.request("com.superlogical.terminal.process", Some(&payload)) {
                    Ok(id) => process_id = Some(id),
                    Err(e) => fail(&mut report, format!("can't ask Rex: {e}")),
                }
            } else if Some(r.id) == process_id {
                let verdict = match rex_find::verdict(&r.result, device, tty_of_pid) {
                    Verdict::Proven => RexVerdict::Proven,
                    Verdict::Disproved(why) => {
                        RexVerdict::Disproved(match other_tty(&r.result, tty_of_pid) {
                            Some(tty) => format!("{why} ({tty})"),
                            None => why,
                        })
                    }
                    Verdict::Unprovable(why) => RexVerdict::Unprovable(why),
                };
                found = Some(Find::Rex {
                    took: started.elapsed(),
                    block: block.clone(),
                    verdict,
                });
            }
        }
    }
    if let Some(rev) = report.api_revision {
        report.missing = crate::providers::rex::CAPABILITIES
            .iter()
            .map(|&c| (c, revision_needed(c)))
            .filter(|&(_, needs)| needs > rev)
            .collect();
    }
    if report.error.is_some() {
        found = None;
    }
    (report, found)
}

/// The path of the terminal of the block's child, in Rex's answer to
/// `terminal.process`, if the system names it.
fn other_tty(result: &ResponseResult, tty_of_pid: TtyOfPid) -> Option<String> {
    let ResponseResult::Ok(ok) = result else {
        return None;
    };
    let pid = client::get_int(client::get(ok, "child"), "pid")?;
    device_path(tty_of_pid(u32::try_from(pid).ok()?)?)
}

/// The path of terminal device `dev`, as `/dev/ttys009`.
#[cfg(target_os = "macos")]
fn device_path(dev: u64) -> Option<String> {
    // SAFETY: devname returns a static buffer or null.
    let name = unsafe { libc::devname(dev as libc::dev_t, libc::S_IFCHR) };
    if name.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated.
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    (!name.is_empty() && !name.contains('?')).then(|| format!("/dev/{name}"))
}

/// Elsewhere it isn't named.
#[cfg(not(target_os = "macos"))]
fn device_path(_dev: u64) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    use serde_json::{Value, json};

    use super::*;
    use crate::rex::client::tests::read_client_frame;
    use crate::rex::session::tests::server_frame;

    const BLOCK: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";
    const OURS: u64 = 0x1000_0006;

    /// Process 42's terminal is ours; 43's another; the rest have none.
    fn fake_tty_of_pid(pid: u32) -> Option<u64> {
        match pid {
            42 => Some(OURS),
            43 => Some(OURS + 1),
            _ => None,
        }
    }

    /// A fake Rex on the other end of a socket: answers each request by
    /// method with `answers`, a payload or an error code, and keeps what
    /// was asked.
    fn fake_rex(
        mut theirs: UnixStream,
        answers: Vec<(&'static str, Value)>,
    ) -> std::thread::JoinHandle<Vec<(String, Value)>> {
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            for _ in 0..answers.len() {
                let frame = read_client_frame(&mut theirs);
                let v: Value = serde_json::from_slice(&frame).unwrap();
                let method = v["method"].as_str().unwrap().to_string();
                let answer = answers
                    .iter()
                    .find(|(m, _)| *m == method)
                    .map(|(_, a)| a.clone())
                    .unwrap_or_else(|| panic!("asked {method}"));
                let mut out = answer;
                out["type"] = json!("response");
                out["id"] = v["id"].clone();
                out["seq"] = json!(0);
                theirs.write_all(&server_frame(&out.to_string())).unwrap();
                asked.push((method, v["payload"].clone()));
            }
            asked
        })
    }

    fn ok(payload: Value) -> Value {
        json!({ "payload": payload })
    }

    fn version() -> (&'static str, Value) {
        (
            "server.version",
            ok(json!({ "rex_version": "0.1.0-preview.14" })),
        )
    }

    fn run(
        answers: Vec<(&'static str, Value)>,
        find: Option<FindIn>,
        api_revision: i64,
    ) -> (RexReport, Option<Find>, Vec<(String, Value)>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let rex = fake_rex(theirs, answers);
        ours.set_nonblocking(true).unwrap();
        let mut client = Client::from_stream(ours);
        client.api_revision = api_revision;
        let due = Instant::now() + Duration::from_secs(3);
        let (report, found) = asked(
            client,
            "unix:///rex".into(),
            "session:s",
            find,
            due,
            fake_tty_of_pid,
        );
        (report, found, rex.join().unwrap())
    }

    fn verdict(found: Option<Find>) -> RexVerdict {
        match found {
            Some(Find::Rex { verdict, block, .. }) => {
                assert_eq!(BLOCK, block);
                verdict
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rex_tells_its_version_and_api_revision_and_what_it_lacks() {
        let (report, found, asked) = run(vec![version()], None, 6);
        assert_eq!(None, found);
        assert_eq!(None, report.error);
        assert_eq!(Some("0.1.0-preview.14".into()), report.version);
        assert_eq!(Some(6), report.api_revision);
        assert!(
            report.missing.contains(&(Capability::Find, 7)),
            "{report:?}"
        );
        assert_eq!(vec![("server.version".to_string(), Value::Null)], asked);

        let (report, _, _) = run(vec![version()], None, 12);
        assert_eq!(Vec::<(Capability, i64)>::new(), report.missing);

        // server.version's own revision, over the one the welcome agreed
        let answer = ok(json!({ "rex_version": "0.1.0", "api_revision": 12 }));
        let (report, _, _) = run(vec![("server.version", answer)], None, 6);
        assert_eq!(Some(12), report.api_revision);
        assert_eq!(Vec::<(Capability, i64)>::new(), report.missing);
    }

    #[test]
    fn find_proves_the_block_on_this_terminal_after_attaching() {
        let answers = vec![
            version(),
            ("session.attach", ok(json!({ "revision": 1 }))),
            (
                "com.superlogical.terminal.process",
                ok(json!({ "child": { "pid": 42 } })),
            ),
        ];
        let find = FindIn {
            block: Ok(BLOCK.into()),
            device: Some(OURS),
        };
        let (report, found, asked) = run(answers, Some(find), 12);
        assert_eq!(None, report.error);
        assert_eq!(RexVerdict::Proven, verdict(found));
        let methods: Vec<&str> = asked.iter().map(|(m, _)| m.as_str()).collect();
        assert_eq!(
            vec![
                "server.version",
                "session.attach",
                "com.superlogical.terminal.process"
            ],
            methods
        );
        assert_eq!(
            json!({ "session_id": "session:s", "block_id": BLOCK }),
            asked[2].1
        );
    }

    #[test]
    fn find_disproves_another_terminal_and_trusts_what_cant_be_proved() {
        let process = |pid| {
            vec![
                version(),
                ("session.attach", ok(json!({}))),
                (
                    "com.superlogical.terminal.process",
                    ok(json!({ "child": { "pid": pid } })),
                ),
            ]
        };
        let find = || {
            Some(FindIn {
                block: Ok(BLOCK.into()),
                device: Some(OURS),
            })
        };
        let (_, found, _) = run(process(43), find(), 12);
        let RexVerdict::Disproved(why) = verdict(found) else {
            panic!("not disproved");
        };
        assert!(why.starts_with("REX_BLOCK names another terminal"), "{why}");
        let (_, found, _) = run(process(44), find(), 12);
        assert_eq!(
            RexVerdict::Unprovable("no controlling terminal for pid 44".into()),
            verdict(found)
        );
    }

    #[test]
    fn find_without_a_block_is_disproved_unasked() {
        let find = FindIn {
            block: Err("REX_BLOCK is unset".into()),
            device: Some(OURS),
        };
        let (report, found, asked) = run(vec![version()], Some(find), 12);
        assert_eq!(None, report.error);
        match found {
            Some(Find::Rex { verdict, .. }) => {
                assert_eq!(RexVerdict::Disproved("REX_BLOCK is unset".into()), verdict)
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(1, asked.len());
    }

    #[test]
    fn a_session_rex_refuses_is_an_error() {
        let answers = vec![
            version(),
            (
                "session.attach",
                json!({ "error": { "code": "not_found", "message": "no session", "status": 404 } }),
            ),
        ];
        let find = FindIn {
            block: Ok(BLOCK.into()),
            device: Some(OURS),
        };
        let (report, found, _) = run(answers, Some(find), 12);
        assert_eq!(
            Some("Rex refused session.attach: not_found: no session".into()),
            report.error
        );
        assert_eq!(None, found);
    }

    #[test]
    fn a_rex_that_isnt_there_cant_be_reached() {
        let env = vec![
            (b"REX_SERVER".to_vec(), b"unix:///nowhere/rex.sock".to_vec()),
            (b"REX_SESSION".to_vec(), b"session:s".to_vec()),
        ];
        let (report, found) = probe(&env, None);
        assert_eq!(None, found);
        assert_eq!("unix:///nowhere/rex.sock", report.server);
        let error = report.error.unwrap();
        assert!(error.starts_with("can't reach Rex: "), "{error}");
    }
}
