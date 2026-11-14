//! herdr's socket API, as the built-in speaks it: newline-delimited JSON
//! over the Unix socket `HERDR_SOCKET_PATH` names, one request per
//! connection, which herdr closes after its one reply. A reply's fields
//! the built-in doesn't read are ignored. Every request of one capability
//! shares one deadline.

use std::ffi::OsStr;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::name::NAME;
use crate::providers::builtin::Given;
use crate::util::seconds;

/// The oldest protocol the built-in speaks: herdr 0.9.5's. An older herdr
/// is refused; a newer one is tried.
pub(super) const PROTOCOL: u64 = 22;

/// The most of a reply read: a session snapshot of a big session is well
/// under it.
const MAX_REPLY: usize = 16 << 20;

/// The herdr server at a socket, with the time its requests have, all
/// together.
pub(super) struct Herdr {
    socket: Vec<u8>,
    time: Duration,
    due: Instant,
}

/// Why a request got no result.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refused {
    /// herdr's own `error.code`, when it answered with one.
    pub code: Option<String>,
    /// Why, for stderr or a proof.
    pub why: String,
}

impl From<Refused> for String {
    fn from(r: Refused) -> String {
        r.why
    }
}

impl Herdr {
    /// The server at `socket`, its requests having `time` from now.
    pub(super) fn at(socket: &[u8], time: Duration) -> Herdr {
        Herdr {
            socket: socket.to_vec(),
            time,
            due: Instant::now() + time,
        }
    }

    /// The server the choosing attach's `HERDR_SOCKET_PATH` names, its
    /// requests having `time` from now; or why there's none.
    pub(super) fn of(given: &Given, time: Duration) -> Result<Herdr, String> {
        match given.env("HERDR_SOCKET_PATH") {
            Some(socket) if !socket.is_empty() => Ok(Herdr::at(&socket, time)),
            _ => Err("HERDR_SOCKET_PATH isn't set: the choosing attach isn't in herdr".into()),
        }
    }

    /// The same server, its requests having `time` from now, whatever
    /// this one's have left: for a cleanup after a request ran out.
    pub(super) fn again(&self, time: Duration) -> Herdr {
        Herdr::at(&self.socket, time)
    }

    /// The `result` of request `method` with `params`, or why there's
    /// none.
    pub(super) fn call(&self, method: &str, params: Value) -> Result<Value, Refused> {
        let line = self
            .exchange(method, params)
            .map_err(|why| Refused { code: None, why })?;
        result_of(&line)
    }

    /// Sends one request and reads its reply's line.
    fn exchange(&self, method: &str, params: Value) -> Result<Vec<u8>, String> {
        let path = OsStr::from_bytes(&self.socket);
        let cant_reach = |e: std::io::Error| {
            format!(
                "can't reach herdr at {}: {e}",
                String::from_utf8_lossy(&self.socket)
            )
        };
        let late = || format!("herdr didn't answer in {} s", seconds(self.time));
        let left = || {
            let left = self.due.saturating_duration_since(Instant::now());
            (!left.is_zero()).then_some(left).ok_or_else(late)
        };
        left()?;
        let mut conn = UnixStream::connect(path).map_err(cant_reach)?;
        let mut request = json!({ "id": NAME, "method": method, "params": params }).to_string();
        request.push('\n');
        conn.set_write_timeout(Some(left()?)).map_err(cant_reach)?;
        conn.write_all(request.as_bytes())
            .map_err(|e| match e.kind() {
                ErrorKind::WouldBlock | ErrorKind::TimedOut => late(),
                _ => cant_reach(e),
            })?;
        let mut reply = Vec::new();
        let mut buf = [0u8; 16 << 10];
        loop {
            conn.set_read_timeout(Some(left()?)).map_err(cant_reach)?;
            match conn.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    reply.extend_from_slice(&buf[..n]);
                    if reply.contains(&b'\n') || reply.len() > MAX_REPLY {
                        break;
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    return Err(late());
                }
                Err(e) => return Err(cant_reach(e)),
            }
        }
        let end = reply
            .iter()
            .position(|&b| b == b'\n')
            .unwrap_or(reply.len());
        reply.truncate(end);
        Ok(reply)
    }
}

/// The `result` of reply `line`, or why there's none: herdr's own error,
/// or a reply that can't be read.
pub(super) fn result_of(line: &[u8]) -> Result<Value, Refused> {
    let unreadable = || Refused {
        code: None,
        why: UNREADABLE.into(),
    };
    let reply: Value = serde_json::from_slice(line).map_err(|_| unreadable())?;
    if let Some(error) = reply.get("error").filter(|e| e.is_object()) {
        let code = error["code"].as_str().map(String::from);
        let message = error["message"]
            .as_str()
            .map(String::from)
            .or_else(|| code.clone())
            .ok_or_else(unreadable)?;
        return Err(Refused {
            code,
            why: format!("herdr: {message}"),
        });
    }
    match reply.get("result") {
        Some(result) if result.is_object() => Ok(result.clone()),
        _ => Err(unreadable()),
    }
}

/// Why a reply, or a field the built-in needs in one, is no use.
pub(super) const UNREADABLE: &str = "herdr's answer can't be read";

/// String field `key` of `v`, or [`UNREADABLE`].
pub(super) fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| UNREADABLE.into())
}

#[cfg(test)]
mod tests {
    use super::super::fake::{Fake, Reply, recorded};
    use super::*;

    #[test]
    fn a_request_is_one_line_and_its_result_the_replys_with_unknown_fields_kept_aside() {
        let fake = Fake::serve(vec![("ping", Reply::Line(recorded("ping")))]);
        let herdr = Herdr::at(&fake.path(), Duration::from_secs(2));
        let pong = herdr.call("ping", json!({})).unwrap();
        assert_eq!(pong["protocol"], 22);
        assert_eq!(pong["version"], "0.9.5");
        let asked = fake.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0]["method"], "ping");
        assert_eq!(asked[0]["params"], json!({}));
        assert_eq!(asked[0]["id"], NAME);
    }

    #[test]
    fn herdrs_error_is_its_message_with_its_code_kept() {
        let fake = Fake::serve(vec![(
            "pane.process_info",
            Reply::Line(recorded("pane-not-found")),
        )]);
        let herdr = Herdr::at(&fake.path(), Duration::from_secs(2));
        let refused = herdr
            .call("pane.process_info", json!({ "pane_id": "w1:p9" }))
            .unwrap_err();
        assert_eq!(refused.code.as_deref(), Some("pane_not_found"));
        assert_eq!(refused.why, "herdr: pane not found");
    }

    #[test]
    fn a_reply_with_neither_result_nor_error_or_no_json_cant_be_read() {
        for line in [
            &b"nope"[..],
            b"{}",
            b"{\"result\":3}",
            b"",
            b"{\"error\":{}}",
        ] {
            assert_eq!(result_of(line).unwrap_err().why, UNREADABLE, "{line:?}");
        }
        // a code with no message says the code
        let r = result_of(br#"{"error":{"code":"x_y"}}"#).unwrap_err();
        assert_eq!(r.why, "herdr: x_y");
    }

    #[test]
    fn no_server_cant_be_reached_and_a_silent_one_runs_out_of_time() {
        let gone = Herdr::at(b"/nowhere/h.sock", Duration::from_secs(2));
        let why = gone.call("ping", json!({})).unwrap_err().why;
        assert!(
            why.starts_with("can't reach herdr at /nowhere/h.sock: "),
            "{why}"
        );

        let fake = Fake::serve(vec![("ping", Reply::Silence)]);
        let herdr = Herdr::at(&fake.path(), Duration::from_millis(200));
        let started = Instant::now();
        let refused = herdr.call("ping", json!({})).unwrap_err();
        assert_eq!(refused.why, "herdr didn't answer in 0.2 s");
        assert!(started.elapsed() < Duration::from_secs(1));
        // the deadline is shared: the next request has none left
        let again = herdr.call("ping", json!({})).unwrap_err();
        assert_eq!(again.why, "herdr didn't answer in 0.2 s");
    }
}
