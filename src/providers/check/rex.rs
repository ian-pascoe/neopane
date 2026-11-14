//! `provider check`'s Rex section (#94 decision 33): the Rex server the
//! attach's `REX_SERVER` names, its version and API revision, whether it is
//! too old for this build, and what Rex's `find` proves of this terminal,
//! all on one connection, within the choice's 3 s. It is the way to check
//! Rex's connection; `debug-rex` stays a developer's tool.
//!
//! `server.version` is asked first, on the bare connection; then Rex's own
//! pane provider takes the connection, attaches to the Rex session and is
//! asked `find`, as the daemon asks it.

use std::time::{Duration, Instant};

use super::find::{self, Asked, Find, Tty};
use crate::attach::{PaneId, env_value};
use crate::config::Attach;
use crate::daemon::provider::{Answer, Outcome};
use crate::name::NAME;
use crate::providers::rex::RexProvider;
use crate::providers::rex::find::BLOCK_VAR;
use crate::providers::tty;
use crate::rex::client::{self, API_REVISION, Client, Message, ResponseResult};
use crate::util::wait_readable;

/// What `provider check` learned of Rex.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RexReport {
    /// `REX_SERVER`.
    pub server: String,
    /// `server.version`'s `rex_version`.
    pub version: Option<String>,
    /// `server.version`'s, else the welcome's.
    pub api_revision: Option<i64>,
    /// The API revision this build needs, when the server's is older: every
    /// capability is missing. Rex doesn't say which revision each method
    /// came in, so it isn't told capability by capability.
    pub missing: Option<i64>,
    /// Why Rex can't be the pane provider, if it can't: no pane provider.
    pub error: Option<String>,
}

/// The time Rex has to answer: the choice's.
const TIME: Duration = crate::config::TIME;

/// Asks the Rex server and Rex session `attach`'s environment names what
/// it is, and, with `finding`, whether this terminal is the Rex block
/// `REX_BLOCK` names.
pub(super) fn probe(attach: &Attach, finding: Option<&Tty>) -> (RexReport, Option<Find>) {
    let var = |name: &str| {
        String::from_utf8_lossy(env_value(&attach.env, name).unwrap_or_default()).into_owned()
    };
    let server = var("REX_SERVER");
    let due = Instant::now() + TIME;
    let mut report = RexReport {
        server,
        version: None,
        api_revision: None,
        missing: None,
        error: None,
    };
    let client = client::connect_to(&report.server).and_then(|s| Client::start(s, NAME));
    let mut client = match client {
        Ok(c) => c,
        Err(e) => {
            report.error = Some(format!("can't reach Rex: {}", unwrapped(e)));
            return (report, None);
        }
    };
    if let Err(why) = ask_version(&mut client, &mut report, due) {
        report.error = Some(why);
        return (report, None);
    }
    report.missing = report
        .api_revision
        .filter(|&rev| rev < API_REVISION)
        .map(|_| API_REVISION);
    let Some(tty) = finding else {
        return (report, None);
    };
    let session = var("REX_SESSION");
    let mut rex = match RexProvider::attaching(client, &session) {
        Ok(p) => p,
        Err(e) => {
            report.error = Some(format!("can't ask Rex: {e}"));
            return (report, None);
        }
    };
    let started = Instant::now();
    let asked = find::ask(&mut rex, attach, tty, PaneId::unnumbered(), due);
    let took = started.elapsed();
    let block = match &asked {
        Asked::Answered(Outcome::Done(Answer::Terminal(id))) => id.to_string(),
        _ => var(BLOCK_VAR),
    };
    match (asked, rex.last_find.take()) {
        (Asked::Answered(_), Some(found)) => {
            let found = Find::Rex {
                took,
                block,
                verdict: found.verdict,
                theirs: found.theirs.and_then(tty::path_of_device),
            };
            (report, Some(found))
        }
        (Asked::Answered(outcome), None) => {
            report.error = Some(format!("Rex answered find with {outcome:?}"));
            (report, None)
        }
        (Asked::Gone(reason), _) => {
            report.error = Some(reason);
            (report, None)
        }
        (Asked::TimedOut, _) => {
            report.error = Some(format!("no answer within {} s", TIME.as_secs()));
            (report, None)
        }
    }
}

/// A connection error as the daemon words it: an I/O error's own text.
fn unwrapped(e: client::ClientError) -> String {
    match e {
        client::ClientError::Io(e) => e.to_string(),
        e => e.to_string(),
    }
}

/// Asks Rex on `client` for `server.version`, waiting until `due`: its
/// version and API revision go in `report`, the welcome's revision where
/// it gives none. Why it couldn't, if it couldn't.
fn ask_version(client: &mut Client, report: &mut RexReport, due: Instant) -> Result<(), String> {
    let id = client
        .request("server.version", None)
        .map_err(|e| format!("can't ask Rex: {e}"))?;
    loop {
        match client.read() {
            Ok(true) => {}
            Ok(false) => return Err("Rex hung up".into()),
            Err(e) => return Err(format!("Rex failed: {e}")),
        }
        while let Some(m) = client.next().map_err(|e| format!("Rex failed: {e}"))? {
            let r = match m {
                Message::Response(r) if r.id == id => r,
                Message::Response(_) | Message::Event(_) => continue,
                Message::Closed(_) => return Err("Rex closed the connection".into()),
            };
            report.api_revision = Some(client.api_revision);
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
            return Ok(());
        }
        if !wait_readable(client.fd(), due) {
            return Err(format!("no answer within {} s", TIME.as_secs()));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    use serde_json::{Value, json};

    use super::*;
    use crate::rex::client::tests::{answer_upgrade, read_client_frame};
    use crate::rex::session::tests::server_frame;

    /// A fake Rex on the other end of a socket: welcomes the client at API
    /// revision `revision`, then answers each request by method from
    /// `answers`, and anything else with an empty payload, until it has
    /// answered the last of `answers`. What was asked, method and payload.
    fn fake_rex(
        mut theirs: UnixStream,
        revision: i64,
        answers: Vec<(&'static str, Value)>,
    ) -> std::thread::JoinHandle<Vec<(String, Value)>> {
        std::thread::spawn(move || {
            answer_upgrade(&mut theirs);
            let _hello = read_client_frame(&mut theirs);
            let welcome = json!({
                "type": "welcome",
                "protocol": client::PROTOCOL,
                "limits": { "max_message_bytes": 65536 },
                "api_revision": revision,
            });
            theirs
                .write_all(&server_frame(&welcome.to_string()))
                .unwrap();
            let last = answers.last().map(|(m, _)| *m);
            let mut asked = Vec::new();
            loop {
                let v: Value = serde_json::from_slice(&read_client_frame(&mut theirs)).unwrap();
                let method = v["method"].as_str().unwrap().to_string();
                let mut out = answers
                    .iter()
                    .find(|(m, _)| *m == method)
                    .map_or_else(|| ok(json!({})), |(_, a)| a.clone());
                out["type"] = json!("response");
                out["id"] = v["id"].clone();
                out["seq"] = json!(0);
                theirs.write_all(&server_frame(&out.to_string())).unwrap();
                asked.push((method.clone(), v["payload"].clone()));
                if Some(method.as_str()) == last {
                    return asked;
                }
            }
        })
    }

    fn ok(payload: Value) -> Value {
        json!({ "payload": payload })
    }

    fn version(revision: Option<i64>) -> (&'static str, Value) {
        let mut v = json!({ "rex_version": "0.1.0-preview.14" });
        if let Some(r) = revision {
            v["api_revision"] = json!(r);
        }
        ("server.version", ok(v))
    }

    #[test]
    fn rex_tells_its_version_and_api_revision() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let rex = fake_rex(theirs, API_REVISION, vec![version(Some(API_REVISION + 5))]);
        let mut client = Client::start(ours, NAME).unwrap();
        let mut report = RexReport {
            server: String::new(),
            version: None,
            api_revision: None,
            missing: None,
            error: None,
        };
        let due = Instant::now() + Duration::from_secs(3);
        ask_version(&mut client, &mut report, due).unwrap();
        assert_eq!(Some("0.1.0-preview.14".into()), report.version);
        // the server's own, over the welcome's
        assert_eq!(Some(API_REVISION + 5), report.api_revision);
        assert_eq!(1, rex.join().unwrap().len());
    }

    #[test]
    fn without_the_servers_own_revision_the_welcomes_is_told() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let rex = fake_rex(theirs, API_REVISION - 1, vec![version(None)]);
        let mut client = Client::start(ours, NAME).unwrap();
        let mut report = RexReport {
            server: String::new(),
            version: None,
            api_revision: None,
            missing: None,
            error: None,
        };
        let due = Instant::now() + Duration::from_secs(3);
        ask_version(&mut client, &mut report, due).unwrap();
        assert_eq!(Some(API_REVISION - 1), report.api_revision);
        rex.join().unwrap();
    }

    #[test]
    fn a_rex_that_isnt_there_cant_be_reached() {
        let attach = Attach {
            env: vec![
                (b"REX_SERVER".to_vec(), b"unix:///nowhere/rex.sock".to_vec()),
                (b"REX_SESSION".to_vec(), b"session:s".to_vec()),
            ],
            ..Default::default()
        };
        let (report, found) = probe(&attach, None);
        assert_eq!(None, found);
        assert_eq!("unix:///nowhere/rex.sock", report.server);
        let error = report.error.unwrap();
        assert!(error.starts_with("can't reach Rex: "), "{error}");
    }
}
