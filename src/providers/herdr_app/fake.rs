//! A fake hc core and a fake `herdr-app` for the fixture tests, replaying
//! what SPI-1214 recorded from hc 0.1.21 and Herdr app 0.1.16
//! (`tests/fixtures/herdr_app/`, whose README gives the replay rules).
//!
//! - [`FakeCore`]: a socket in a directory of its own, holding each
//!   connection open as the core does, that answers each request with the
//!   replies recorded after the same request, its `id` echoed; it keeps
//!   every request it read.
//! - [`FakeApp`]: a shell script named `herdr-app` that answers each argv
//!   with a recorded run's stdout, stderr and exit status, and logs the
//!   argv it was given.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::name::NAME;
use crate::providers::builtin::Given;

/// The fixtures' directory.
fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/herdr_app")
}

/// A scratch directory of its own, `what` in its name, made empty.
fn scratch(what: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("{NAME}-ha{what}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// One recorded request and what the core sent after it, up to the next
/// request: its reply, and any notifications.
#[derive(Debug, Clone)]
pub(super) struct Exchange {
    pub method: String,
    pub params: Value,
    /// Empty: the core says nothing, holding the connection open.
    pub replies: Vec<Value>,
}

impl Exchange {
    /// A request of `method` with `params`, never answered.
    pub(super) fn silent(method: &str, params: Value) -> Exchange {
        Exchange {
            method: method.into(),
            params,
            replies: Vec::new(),
        }
    }

    /// Its reply's `result`, to change.
    pub(super) fn result_mut(&mut self) -> &mut Value {
        let reply = self
            .replies
            .iter_mut()
            .find(|r| r.get("id").is_some())
            .expect("a reply");
        &mut reply["result"]
    }
}

/// The exchanges of recorded file `core/NAME.jsonl`, in order.
pub(super) fn recorded(name: &str) -> Vec<Exchange> {
    let file = fixtures().join(format!("core/{name}.jsonl"));
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
    let mut exchanges: Vec<Exchange> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let entry: Value = serde_json::from_str(line).unwrap();
        if let Some(sent) = entry.get("send") {
            exchanges.push(Exchange {
                method: sent["method"].as_str().unwrap().into(),
                params: sent["params"].clone(),
                replies: Vec::new(),
            });
        } else {
            let last = exchanges.last_mut().expect("a reply before any request");
            last.replies.push(entry["recv"].clone());
        }
    }
    exchanges
}

/// The first recorded exchange of `method` in file `core/NAME.jsonl`.
pub(super) fn recorded_one(name: &str, method: &str) -> Exchange {
    recorded(name)
        .into_iter()
        .find(|e| e.method == method)
        .unwrap_or_else(|| panic!("no {method} in {name}"))
}

/// `terminal.kill` of `terminal`, answering its record, finished.
pub(super) fn kill_of(terminal: &str) -> Exchange {
    let mut kill = recorded_one("terminal.kill", "terminal.kill");
    kill.params = json!({ "terminal": terminal });
    kill.result_mut()["address"] = json!(terminal);
    kill
}

/// The core's `terminal.keep {keep: false}` of `terminal`, answering its
/// record.
pub(super) fn keep_off(terminal: &str) -> Exchange {
    let mut keep = recorded_one("terminal.keep", "terminal.keep");
    keep.params = json!({ "terminal": terminal, "keep": false });
    keep.result_mut()["address"] = json!(terminal);
    keep
}

/// A fake hc core, removed as it drops.
pub(super) struct FakeCore {
    dir: PathBuf,
    socket: PathBuf,
    asked: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
}

impl FakeCore {
    /// A core answering each request with the replies of the first
    /// exchange in `exchanges` of the same method and params not yet used,
    /// else of the last such used; a request with none gets the core's
    /// `unknown_method`.
    pub(super) fn serve(exchanges: Vec<Exchange>) -> FakeCore {
        let dir = scratch("c");
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (its_asked, its_stop) = (asked.clone(), stop.clone());
        let used = Arc::new(Mutex::new(vec![false; exchanges.len()]));
        let exchanges = Arc::new(exchanges);
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                if its_stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(conn) = conn else { continue };
                let (asked, exchanges, used) = (its_asked.clone(), exchanges.clone(), used.clone());
                std::thread::spawn(move || serve_one(conn, &asked, &exchanges, &used));
            }
        });
        FakeCore {
            dir,
            socket,
            asked,
            stop,
        }
    }

    /// A core replaying recorded files `core/NAME.jsonl`, all together.
    pub(super) fn replaying(names: &[&str]) -> FakeCore {
        FakeCore::serve(names.iter().flat_map(|n| recorded(n)).collect())
    }

    /// Its socket's path, as `HC_TERMINAL_SOCKET` has it.
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

/// Answers the requests of one connection until the client closes it.
fn serve_one(
    conn: UnixStream,
    asked: &Mutex<Vec<Value>>,
    exchanges: &[Exchange],
    used: &Mutex<Vec<bool>>,
) {
    let mut reader = BufReader::new(&conn);
    let mut writer = &conn;
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(request) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        asked.lock().unwrap().push(request.clone());
        let id = request["id"].clone();
        let same = |e: &Exchange| e.method == request["method"] && e.params == request["params"];
        let found = {
            let mut used = used.lock().unwrap();
            let fresh = (0..exchanges.len()).find(|&i| !used[i] && same(&exchanges[i]));
            let at = fresh.or_else(|| (0..exchanges.len()).rev().find(|&i| same(&exchanges[i])));
            if let Some(i) = at {
                used[i] = true;
            }
            at
        };
        let replies = match found {
            Some(i) => exchanges[i].replies.clone(),
            None => vec![json!({
                "jsonrpc": "2.0",
                "id": 0,
                "error": {
                    "code": -32601,
                    "message": format!("unknown method {}", request["method"]),
                    "data": { "kind": "unknown_method" },
                },
            })],
        };
        for mut reply in replies {
            // echo the client's id, as the README's replay rules say
            if reply.get("id").is_some() {
                reply["id"] = id.clone();
            }
            let mut out = reply.to_string().into_bytes();
            out.push(b'\n');
            if writer.write_all(&out).is_err() {
                return;
            }
        }
    }
}

impl Drop for FakeCore {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // wakes its accept
        let _ = UnixStream::connect(&self.socket);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// One recorded run of `herdr-app`: its words after `herdr-app`, and what
/// it printed and exited with.
#[derive(Debug, Clone)]
pub(super) struct Run {
    pub args: Vec<String>,
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
    /// Seconds it sleeps instead of answering, for a test of its
    /// deadline.
    pub sleep: Option<f64>,
}

/// The run recorded in `app/NAME.json`: its stdout as the app prints it,
/// JSON pretty-printed and plain text as it was.
pub(super) fn recorded_run(name: &str) -> Run {
    let file = fixtures().join(format!("app/{name}.json"));
    let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
    let rec: Value = serde_json::from_str(&text).unwrap();
    let args = rec["argv"].as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let stdout = match &rec["stdout"] {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        json => format!("{}\n", serde_json::to_string_pretty(json).unwrap()),
    };
    Run {
        args,
        exit: rec["exit"].as_i64().unwrap() as i32,
        stdout,
        stderr: rec["stderr"].as_str().unwrap_or_default().to_string(),
        sleep: None,
    }
}

/// The `herdr-app do COMMAND JSON` run recorded in `app/NAME.json`, its
/// JSON as the built-in writes it, compact: the recorder spaced it.
pub(super) fn recorded_do(name: &str) -> Run {
    let mut run = recorded_run(name);
    assert_eq!(run.args[0], "do", "{name}");
    let params: Value = serde_json::from_str(&run.args[2]).unwrap();
    run.args[2] = params.to_string();
    run
}

/// A fake `herdr-app`, removed as it drops.
pub(super) struct FakeApp {
    dir: PathBuf,
}

impl FakeApp {
    /// A `herdr-app` answering each of `runs` by its words, in a
    /// directory of its own; any other words exit 64, saying so. It names
    /// its commands by path: an inside proof runs it with a `PATH` of its
    /// directory alone.
    pub(super) fn new(runs: &[Run]) -> FakeApp {
        let dir = scratch("a");
        let quoted = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let mut script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in\n",
            quoted(&dir.join("log").to_string_lossy())
        );
        for (i, run) in runs.iter().enumerate() {
            let (out, err) = (dir.join(format!("{i}.out")), dir.join(format!("{i}.err")));
            std::fs::write(&out, &run.stdout).unwrap();
            std::fs::write(&err, &run.stderr).unwrap();
            let answer = match run.sleep {
                // as its own process, so killing it ends the sleep
                Some(s) => format!("exec /bin/sleep {s}"),
                None => format!(
                    "/bin/cat {}; /bin/cat {} >&2; exit {}",
                    quoted(&out.to_string_lossy()),
                    quoted(&err.to_string_lossy()),
                    run.exit
                ),
            };
            script.push_str(&format!("{}) {answer} ;;\n", quoted(&run.args.join(" "))));
        }
        script.push_str("esac\necho \"fake herdr-app: no run for: $*\" >&2\nexit 64\n");
        let program = dir.join("herdr-app");
        std::fs::write(&program, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        FakeApp { dir }
    }

    /// The directory it is in, for a `PATH`.
    pub(super) fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its path.
    pub(super) fn program(&self) -> PathBuf {
        self.dir.join("herdr-app")
    }

    /// The words it was run with, joined by spaces, in order.
    pub(super) fn asked(&self) -> Vec<String> {
        let log = std::fs::read_to_string(self.dir.join("log")).unwrap_or_default();
        log.lines().map(String::from).collect()
    }
}

impl Drop for FakeApp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What a capability is given beside `core` and `app`: its `<NAME>_*`
/// inputs `vars`, and the choosing attach's `HC_TERMINAL_SOCKET`, and
/// `PATH` finding `app` alone.
pub(super) fn given(core: &FakeCore, app: &FakeApp, vars: &[(&str, &str)]) -> Given {
    let up = crate::name::upper();
    let mut env: Vec<(String, Vec<u8>)> = vars
        .iter()
        .map(|(k, v)| (format!("{up}_{k}"), v.as_bytes().to_vec()))
        .collect();
    env.push(("HC_TERMINAL_SOCKET".into(), core.path()));
    env.push(("PATH".into(), app.dir().as_os_str().as_bytes().to_vec()));
    env.push(("HOME".into(), b"/nowhere".to_vec()));
    Given::from(move |name| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
}
