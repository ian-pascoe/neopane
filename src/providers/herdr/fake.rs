//! A fake herdr for the fixture tests: a socket in a directory of its own
//! that answers each connection's one request with a reply recorded from
//! herdr 0.9.5, chosen by its method, and keeps every request it read.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::name::NAME;
use crate::providers::builtin::Given;

/// A reply recorded from herdr 0.9.5, by its file name in `fixtures/`
/// less `0.9.5-` and `.json`.
pub(super) fn recorded(name: &str) -> String {
    fixture(&format!("{name}.json"))
}

/// The lines of a subscription recorded from herdr 0.9.5, its first the
/// reply to `events.subscribe`, by its file name in `fixtures/` less
/// `0.9.5-` and `.ndjson`.
pub(super) fn recorded_stream(name: &str) -> Vec<String> {
    fixture(&format!("{name}.ndjson"))
        .lines()
        .map(String::from)
        .collect()
}

fn fixture(name: &str) -> String {
    let file = format!(
        "{}/src/providers/herdr/fixtures/0.9.5-{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// An attach command, as `ATTACH` has it: its words are `/bin/np`,
/// `attach`, `--address` and `/tmp/a b`.
pub(super) const ATTACH: &str = "/bin/np attach --address '/tmp/a b'";

/// What a capability is given beside `fake`: its `<NAME>_*` inputs
/// `vars`, and the choosing attach's `HERDR_SOCKET_PATH`.
pub(super) fn given(fake: &Fake, vars: &[(&str, &str)]) -> Given {
    let up = crate::name::upper();
    let mut env: Vec<(String, Vec<u8>)> = vars
        .iter()
        .map(|(k, v)| (format!("{up}_{k}"), v.as_bytes().to_vec()))
        .collect();
    env.push(("HERDR_SOCKET_PATH".into(), fake.path()));
    Given::from(move |name| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
}

/// What the fake answers a method with.
#[derive(Clone)]
pub(super) enum Reply {
    /// This line.
    Line(String),
    /// Nothing, holding the connection open until the client gives up.
    Silence,
    /// These lines, as a subscription sends them, then the connection
    /// held open while the fake lives, or closed if `end`.
    Stream { lines: Vec<String>, end: bool },
}

/// A fake herdr, removed as it drops.
pub(super) struct Fake {
    dir: PathBuf,
    pub socket: PathBuf,
    asked: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
}

impl Fake {
    /// A fake answering each method in `replies` with its replies in turn,
    /// the last again once they run out; a method not there gets herdr's
    /// `unknown_method`.
    pub(super) fn serve(replies: Vec<(&'static str, Reply)>) -> Fake {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("{NAME}-hf-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (its_asked, its_stop) = (asked.clone(), stop.clone());
        // the subscriptions held open, closed as the fake drops
        let held: Arc<Mutex<Vec<UnixStream>>> = Arc::default();
        std::thread::spawn(move || {
            let mut used: HashMap<String, usize> = HashMap::new();
            for conn in listener.incoming() {
                if its_stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let mut line = Vec::new();
                let mut reader = BufReader::new(&conn);
                if reader.read_until(b'\n', &mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                let method = request["method"].as_str().unwrap_or_default().to_string();
                its_asked.lock().unwrap().push(request);
                let theirs: Vec<&Reply> = replies
                    .iter()
                    .filter(|(m, _)| *m == method)
                    .map(|(_, r)| r)
                    .collect();
                let at = used.entry(method).or_default();
                let reply = theirs.get(*at).or(theirs.last()).copied().cloned();
                *at += 1;
                match reply {
                    Some(Reply::Line(l)) => answer(&conn, &l),
                    Some(Reply::Silence) => {
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_secs(5));
                            drop(conn);
                        });
                    }
                    Some(Reply::Stream { lines, end }) => {
                        for l in &lines {
                            answer(&conn, l);
                        }
                        if !end {
                            held.lock().unwrap().push(conn);
                        }
                    }
                    None => answer(
                        &conn,
                        r#"{"id":"x","error":{"code":"unknown_method","message":"unknown method"}}"#,
                    ),
                }
            }
        });
        Fake {
            dir,
            socket,
            asked,
            stop,
        }
    }

    /// Its socket's path, as `HERDR_SOCKET_PATH` has it.
    pub(super) fn path(&self) -> Vec<u8> {
        self.socket.as_os_str().as_bytes().to_vec()
    }

    /// The requests it read, in order.
    pub(super) fn asked(&self) -> Vec<Value> {
        self.asked.lock().unwrap().clone()
    }

    /// The methods it was asked, in order.
    pub(super) fn methods(&self) -> Vec<String> {
        self.asked()
            .iter()
            .map(|r| r["method"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// Writes `line`, ending it, and closes, as herdr does.
fn answer(conn: &UnixStream, line: &str) {
    let mut conn = conn;
    let _ = conn.write_all(line.trim_end().as_bytes());
    let _ = conn.write_all(b"\n");
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wakes its accept
        let _ = UnixStream::connect(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
