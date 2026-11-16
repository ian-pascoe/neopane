//! The hc core's JSON-RPC socket, as the built-in speaks it: JSON-RPC 2.0,
//! one JSON value a line, over the Unix socket `HC_TERMINAL_SOCKET` names,
//! held open for every request of one capability, which share one
//! deadline; a cleanup after it ran out has a connection of its own
//! ([`Core::again`]). A connection starts with `hello`, whose `generation`,
//! `version` and `methods` are kept: the built-in speaks generation 1, and
//! needs only the methods it calls. A reply's fields the built-in doesn't
//! read are ignored, as are lines that answer no request of its own, such
//! as event notifications.

use std::ffi::OsStr;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::providers::builtin::Given;
use crate::providers::deadline::readable_by;
use crate::util::seconds;

/// The protocol generation the built-in speaks.
pub(super) const GENERATION: u64 = 1;

/// The most of one line read: a doc of a big window is well under it.
const MAX_LINE: usize = 16 << 20;

/// The hc core at a socket, with a connection open and the time its
/// requests have, all together.
pub(super) struct Core {
    socket: Vec<u8>,
    conn: UnixStream,
    /// What was read past the last line taken.
    pending: Vec<u8>,
    /// The last request's id.
    id: u64,
    time: Duration,
    due: Instant,
    hello: Hello,
}

/// What the core's `hello` said of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hello {
    /// None if it didn't say.
    pub generation: Option<u64>,
    /// Such as `0.1.21`; `?` if it didn't say.
    pub version: String,
    pub methods: Vec<String>,
}

/// Why a request got no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Refused {
    /// The core's own `error.data.kind`, such as `not_found`, when it
    /// answered with one.
    pub kind: Option<String>,
    /// Why, for stderr or a proof.
    pub why: String,
}

impl From<Refused> for String {
    fn from(r: Refused) -> String {
        r.why
    }
}

impl From<String> for Refused {
    fn from(why: String) -> Refused {
        Refused { kind: None, why }
    }
}

/// Why a reply, or a field the built-in needs in one, is no use.
pub(super) const UNREADABLE: &str = "the hc core's answer can't be read";

/// An hc terminal, as `terminal.get` and `terminal.kill` answer it: the
/// fields the built-in reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Terminal {
    /// Its address, such as `t7`: its terminal id.
    pub address: String,
    /// Its `state` is `live`, not `finished`.
    pub live: bool,
    /// The process it runs; None if it has none.
    pub pid: Option<u32>,
    /// Its `cols` and `rows`, the cells it has; None if it didn't say.
    pub size: Option<[u32; 2]>,
}

impl Terminal {
    fn of(record: &Value) -> Result<Terminal, Refused> {
        let address = record["address"].as_str().ok_or(UNREADABLE.to_string())?;
        let state = record["state"].as_str().ok_or(UNREADABLE.to_string())?;
        Ok(Terminal {
            address: address.into(),
            live: state == "live",
            pid: record["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
            size: cells(&record["cols"])
                .zip(cells(&record["rows"]))
                .map(|(c, r)| [c, r]),
        })
    }
}

impl Core {
    /// Connects to the core at `socket`, its requests having `time` from
    /// now, `hello` first; or why it can't.
    pub(super) fn connect(socket: &[u8], time: Duration) -> Result<Core, String> {
        let due = Instant::now() + time;
        let shown = String::from_utf8_lossy(socket);
        let conn = UnixStream::connect(OsStr::from_bytes(socket))
            .map_err(|e| format!("can't reach the hc core at {shown}: {e}"))?;
        let mut core = Core {
            socket: socket.to_vec(),
            conn,
            pending: Vec::new(),
            id: 0,
            time,
            due,
            hello: Hello {
                generation: None,
                version: "?".into(),
                methods: Vec::new(),
            },
        };
        let hello = core.call("hello", json!({}))?;
        core.hello = Hello {
            generation: hello["generation"].as_u64(),
            version: hello["version"].as_str().unwrap_or("?").into(),
            methods: hello["methods"]
                .as_array()
                .map(|ms| {
                    ms.iter()
                        .filter_map(|m| m.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        };
        Ok(core)
    }

    /// The core the choosing attach's `HC_TERMINAL_SOCKET` names,
    /// connected; or why there's none.
    pub(super) fn of(given: &Given, time: Duration) -> Result<Core, String> {
        match given.env("HC_TERMINAL_SOCKET") {
            Some(socket) if !socket.is_empty() => Core::connect(&socket, time),
            _ => Err(
                "HC_TERMINAL_SOCKET isn't set: the choosing attach isn't in the Herdr app".into(),
            ),
        }
    }

    /// When its requests' time runs out.
    pub(super) fn due(&self) -> Instant {
        self.due
    }

    /// What its `hello` said.
    pub(super) fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Why the built-in can't speak to this core for `methods`, if it
    /// can't: another generation than [`GENERATION`], or a method its
    /// `hello` doesn't list, the first such named.
    pub(super) fn lacks(&self, methods: &[&str]) -> Option<String> {
        match self.hello.generation {
            Some(GENERATION) => {}
            Some(other) => {
                return Some(format!(
                    "the hc core speaks generation {other}; the built-in herdr-app speaks \
                     {GENERATION}"
                ));
            }
            None => return Some("the hc core's hello names no generation".into()),
        }
        methods
            .iter()
            .find(|m| !self.hello.methods.iter().any(|h| h == *m))
            .map(|m| format!("the hc core has no {m}"))
    }

    /// The `result` of request `method` with `params`, or why there's
    /// none.
    pub(super) fn call(&mut self, method: &str, params: Value) -> Result<Value, Refused> {
        self.id += 1;
        let id = self.id;
        let mut line =
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        line.push('\n');
        if Instant::now() >= self.due {
            return Err(self.late().into());
        }
        (&self.conn)
            .write_all(line.as_bytes())
            .map_err(|e| self.cant_reach(&e))?;
        loop {
            let line = self.line()?;
            let Ok(reply) = serde_json::from_slice::<Value>(&line) else {
                return Err(UNREADABLE.to_string().into());
            };
            if reply.get("id").and_then(Value::as_u64) == Some(id) {
                return result_of(&reply);
            }
            // a notification, or a reply to no request of ours
        }
    }

    /// `terminal.get` of terminal `id`.
    pub(super) fn terminal(&mut self, id: &str) -> Result<Terminal, Refused> {
        Terminal::of(&self.call("terminal.get", json!({ "terminal": id }))?)
    }

    /// `terminal.list`: every live terminal.
    pub(super) fn list(&mut self) -> Result<Vec<Terminal>, Refused> {
        let listed = self.call("terminal.list", json!({}))?;
        let terminals = listed["terminals"]
            .as_array()
            .ok_or(UNREADABLE.to_string())?;
        terminals.iter().map(Terminal::of).collect()
    }

    /// `terminal.kill` of terminal `id`: its record, finished.
    pub(super) fn kill(&mut self, id: &str) -> Result<Terminal, Refused> {
        Terminal::of(&self.call("terminal.kill", json!({ "terminal": id }))?)
    }

    /// The core's `terminal.keep` of terminal `id`: whether the core gives
    /// it a shell again after it's lost. Not `herdr-app`'s `terminal.keep
    /// {terminal, place}`, which shows a terminal in the window.
    pub(super) fn keep(&mut self, id: &str, keep: bool) -> Result<Terminal, Refused> {
        Terminal::of(&self.call("terminal.keep", json!({ "terminal": id, "keep": keep }))?)
    }

    /// The same core on a new connection, its requests having `time` from
    /// now: for cleaning up after this one's time ran out.
    pub(super) fn again(&self, time: Duration) -> Result<Core, String> {
        Core::connect(&self.socket, time)
    }

    /// The Herdr app's doc, `doc.get {name: "herdr-app"}`'s `value`:
    /// null if never written.
    pub(super) fn doc(&mut self) -> Result<Value, Refused> {
        let got = self.call("doc.get", json!({ "name": "herdr-app" }))?;
        got.get("value")
            .cloned()
            .ok_or_else(|| UNREADABLE.to_string().into())
    }

    /// The next line, without its newline, before the deadline.
    fn line(&mut self) -> Result<Vec<u8>, Refused> {
        let mut buf = [0u8; 16 << 10];
        loop {
            if let Some(end) = self.pending.iter().position(|&b| b == b'\n') {
                let line = self.pending[..end].to_vec();
                self.pending.drain(..=end);
                return Ok(line);
            }
            if self.pending.len() > MAX_LINE {
                return Err(UNREADABLE.to_string().into());
            }
            if !readable_by(&self.conn, self.due).map_err(|e| self.cant_reach(&e))? {
                return Err(self.late().into());
            }
            match (&self.conn).read(&mut buf) {
                Ok(0) => return Err("the hc core closed the connection".to_string().into()),
                Ok(n) => self.pending.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) => return Err(self.cant_reach(&e).into()),
            }
        }
    }

    fn late(&self) -> String {
        format!("the hc core didn't answer in {} s", seconds(self.time))
    }

    fn cant_reach(&self, e: &std::io::Error) -> String {
        format!(
            "can't reach the hc core at {}: {e}",
            String::from_utf8_lossy(&self.socket)
        )
    }
}

/// A count of cells, `v`, if it is one.
fn cells(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

/// The `result` of `reply`, or why there's none: the core's own error,
/// or a reply that can't be read.
fn result_of(reply: &Value) -> Result<Value, Refused> {
    if let Some(error) = reply.get("error").filter(|e| e.is_object()) {
        let kind = error["data"]["kind"].as_str().map(String::from);
        let message = error["message"]
            .as_str()
            .map(String::from)
            .or_else(|| kind.clone())
            .ok_or(UNREADABLE.to_string())?;
        return Err(Refused {
            kind,
            why: format!("hc core: {message}"),
        });
    }
    match reply.get("result") {
        Some(result) if !result.is_null() => Ok(result.clone()),
        _ => Err(UNREADABLE.to_string().into()),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Exchange, FakeCore, recorded, recorded_one};
    use super::*;

    const TIME: Duration = Duration::from_secs(2);

    #[test]
    fn a_connection_says_hello_first_and_keeps_its_generation_version_and_methods() {
        let fake = FakeCore::replaying(&["hello"]);
        let core = Core::connect(&fake.path(), TIME).unwrap();
        let hello = core.hello();
        assert_eq!(hello.generation, Some(1));
        assert_eq!(hello.version, "0.1.21");
        assert!(hello.methods.iter().any(|m| m == "terminal.kill"));
        assert_eq!(
            core.lacks(&["terminal.get", "doc.get", "terminal.kill"]),
            None
        );
        let asked = fake.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0],
            json!({ "jsonrpc": "2.0", "id": 1, "method": "hello", "params": {} })
        );
    }

    #[test]
    fn another_generation_or_a_method_hello_doesnt_list_is_lacked() {
        let mut hello = recorded_one("hello", "hello");
        hello.result_mut()["generation"] = json!(2);
        let fake = FakeCore::serve(vec![hello]);
        let core = Core::connect(&fake.path(), TIME).unwrap();
        assert_eq!(
            core.lacks(&[]).as_deref(),
            Some("the hc core speaks generation 2; the built-in herdr-app speaks 1")
        );

        let mut hello = recorded_one("hello", "hello");
        let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
        methods.retain(|m| m != "terminal.kill");
        let fake = FakeCore::serve(vec![hello]);
        let core = Core::connect(&fake.path(), TIME).unwrap();
        assert_eq!(
            core.lacks(&["terminal.get", "terminal.kill"]).as_deref(),
            Some("the hc core has no terminal.kill")
        );
    }

    #[test]
    fn requests_share_one_connection_each_with_an_id_of_its_own() {
        let fake = FakeCore::replaying(&["nested-terminal.get"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let t1 = core.terminal("t1").unwrap();
        assert_eq!(
            t1,
            Terminal {
                address: "t1".into(),
                live: true,
                pid: Some(9087),
                size: Some([39, 52]),
            }
        );
        assert_eq!(core.terminal("t3").unwrap().pid, Some(9932));
        let ids: Vec<Value> = fake.asked().iter().map(|r| r["id"].clone()).collect();
        assert_eq!(ids, [json!(1), json!(2), json!(3)]);
        assert_eq!(fake.asked()[2]["params"], json!({ "terminal": "t3" }));
    }

    #[test]
    fn the_list_is_every_live_terminal_with_its_cols_and_rows() {
        let fake = FakeCore::replaying(&["shapes-doc-and-list"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let listed = core.list().unwrap();
        let sizes: Vec<(&str, Option<[u32; 2]>)> = listed
            .iter()
            .map(|t| (t.address.as_str(), t.size))
            .collect();
        assert_eq!(
            sizes,
            [
                ("t1", Some([39, 52])),
                ("t2", Some([39, 24])),
                ("t3", Some([39, 24])),
                ("t4", Some([38, 52])),
                ("t5", Some([17, 52])),
                ("t6", Some([17, 24])),
                ("t7", Some([17, 10])),
                ("t8", Some([6, 10])),
                ("t9", Some([6, 10])),
            ]
        );
        assert_eq!(fake.methods(), ["hello", "terminal.list"]);
        assert_eq!(fake.asked()[1]["params"], json!({}));
    }

    #[test]
    fn the_cores_error_is_its_message_with_its_kind_kept() {
        let fake = FakeCore::replaying(&["terminal.get-not-found"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let refused = core.terminal("t99").unwrap_err();
        assert_eq!(refused.kind.as_deref(), Some("not_found"));
        assert_eq!(refused.why, "hc core: no terminal \"t99\"");

        let fake = FakeCore::replaying(&["unknown-method"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let refused = core.call("no.such", json!({})).unwrap_err();
        assert_eq!(refused.kind.as_deref(), Some("unknown_method"));
    }

    #[test]
    fn a_finished_terminal_is_read_as_not_live_and_kill_answers_its_record() {
        let fake = FakeCore::replaying(&["terminal.get-finished", "terminal.kill"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let t10 = core.terminal("t10").unwrap();
        assert!(!t10.live);
        let killed = core.kill("t10").unwrap();
        assert_eq!(killed.address, "t10");
        assert!(!killed.live);
    }

    #[test]
    fn keep_sets_the_cores_keep_and_answers_the_record() {
        let fake = FakeCore::replaying(&["terminal.keep", "terminal.keep-not-found"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        assert_eq!(core.keep("t3", false).unwrap().address, "t3");
        assert_eq!(
            fake.asked()[1]["params"],
            json!({ "terminal": "t3", "keep": false })
        );
        let refused = core.keep("t99", false).unwrap_err();
        assert_eq!(refused.kind.as_deref(), Some("not_found"));
    }

    #[test]
    fn again_is_a_new_connection_with_time_of_its_own() {
        let fake = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            Exchange::silent("terminal.get", json!({ "terminal": "t1" })),
        ]);
        let mut core = Core::connect(&fake.path(), Duration::from_millis(200)).unwrap();
        assert!(core.terminal("t1").is_err());
        let again = core.again(TIME).unwrap();
        assert_eq!(again.hello().version, "0.1.21");
        assert_eq!(fake.methods(), ["hello", "terminal.get", "hello"]);
    }

    #[test]
    fn the_doc_is_the_herdr_apps_and_null_if_never_written() {
        let fake = FakeCore::replaying(&["nested-doc.get"]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        let doc = core.doc().unwrap();
        assert_eq!(doc["spaces"][0]["name"], "spike");
        assert_eq!(fake.asked()[1]["params"], json!({ "name": "herdr-app" }));

        let mut unwritten = recorded_one("doc.get-unwritten", "doc.get");
        unwritten.params = json!({ "name": "herdr-app" });
        let fake = FakeCore::serve(vec![recorded_one("hello", "hello"), unwritten]);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        assert_eq!(core.doc().unwrap(), Value::Null);
    }

    #[test]
    fn notifications_between_are_skipped() {
        // a doc.get answered after an event the subscription sent
        let mut exchanges = recorded("nested-doc.get");
        let doc = exchanges
            .iter_mut()
            .find(|e| e.method == "doc.get")
            .unwrap();
        doc.replies.insert(
            0,
            json!({ "jsonrpc": "2.0", "method": "event", "params": { "seq": 3, "type": "doc" } }),
        );
        let fake = FakeCore::serve(exchanges);
        let mut core = Core::connect(&fake.path(), TIME).unwrap();
        assert_eq!(core.doc().unwrap()["spaces"][0]["name"], "spike");
    }

    #[test]
    fn no_core_cant_be_reached_and_a_silent_one_runs_out_of_time() {
        let why = Core::connect(b"/nowhere/hc.sock", TIME).err().unwrap();
        assert!(
            why.starts_with("can't reach the hc core at /nowhere/hc.sock: "),
            "{why}"
        );

        let fake = FakeCore::serve(vec![
            recorded_one("hello", "hello"),
            Exchange::silent("terminal.get", json!({ "terminal": "t1" })),
        ]);
        let mut core = Core::connect(&fake.path(), Duration::from_millis(200)).unwrap();
        let started = Instant::now();
        let refused = core.terminal("t1").unwrap_err();
        assert_eq!(refused.why, "the hc core didn't answer in 0.2 s");
        assert!(started.elapsed() < Duration::from_secs(1));
        // the deadline is shared: the next request has none left
        let again = core.call("hello", json!({})).unwrap_err();
        assert_eq!(again.why, "the hc core didn't answer in 0.2 s");
    }

    #[test]
    fn of_needs_hc_terminal_socket() {
        let given = Given::from(|_| None);
        let why = Core::of(&given, TIME).err().unwrap();
        assert_eq!(
            why,
            "HC_TERMINAL_SOCKET isn't set: the choosing attach isn't in the Herdr app"
        );
    }
}
