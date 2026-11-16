//! The built-in Herdr app pane provider, `herdr-app` (ADR 0019, ADR 0020),
//! against a live hc core: a private one, its own `HC_SESSION` and state
//! (`HcCore`), never the user's. Headless, the window is a fake
//! `herdr-app` that acts on that core as the app does ([`FakeWindow`]):
//! it makes terminals with `terminal.run` and writes the core's doc, so
//! the proof, `close`, the doc and the core's keep are the core's own.
//!
//! The tests against a real Herdr app window, a private one under a
//! `HERDR_APP_HOME` of their own, are in `mod window`, ignored: run them
//! with `just test-herdr-app`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neopane::name::NAME;
use serde_json::{Value, json};

mod support;
use support::*;

/// Waits up to 30 s for `f` to make `want` true; what it last made.
fn until<T>(want: impl Fn(&T) -> bool, f: impl Fn() -> T) -> T {
    let due = Instant::now() + Duration::from_secs(30);
    loop {
        let got = f();
        if want(&got) || Instant::now() > due {
            return got;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A program that runs until it is killed, for a terminal to hold.
const IDLE: &[&str] = &["/bin/sleep", "100000"];

/// The version the fake `herdr-app` says it is: the fixtures'.
const APP_VERSION: &str = "0.1.16";

/// A fake Herdr app window over a live hc core: a `herdr-app` script in a
/// directory of its own, for a `PATH`. `--version` it answers itself, and
/// `status` from a file the test keeps, as fast as the app answers, for
/// the inside proof's 0.5 s; with [`FakeWindow::close`], it says there's
/// no window, as `herdr-app` does. Each `do` is handed to a thread of the
/// test's, which acts on the core as the app does: a split or a tab is a
/// new terminal, `terminal.run` with the core's keep on, and a block
/// written into the core's doc, shaped as the app shapes it
/// (`docs/research/herdr-app.md`); `pane.split` and `pane.focus` move the
/// focus as the app does, and `layout.resize` sets a node's size. Its
/// directory is its `HERDR_APP_HOME`, with no `view.json`: nothing is
/// zoomed. Any other `do` it refuses, as the app refuses an unknown
/// command: `pane.go`, which `focus_direction` asks, is deliberately
/// unexercised, for a test of it to add to [`act`]. Unlike the app, it
/// leaves a finished terminal's block in the doc. Stopped as it drops.
struct FakeWindow {
    dir: PathBuf,
    socket: PathBuf,
    asked: Arc<Mutex<Vec<(String, Value)>>>,
    focus: Arc<Mutex<Focus>>,
    stop: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

/// Where a fake window's focus is: the tab it shows, and each tab's
/// focused pane, by tab id.
#[derive(Default)]
struct Focus {
    shown: Option<String>,
    of_tab: std::collections::HashMap<String, String>,
}

/// What a fake window's terminals run with: their working directory, and
/// variables over the core's own.
#[derive(Clone)]
struct Spawn {
    /// The window's own directory, its `status` there.
    dir: PathBuf,
    socket: PathBuf,
    cwd: PathBuf,
    env: serde_json::Map<String, Value>,
}

impl FakeWindow {
    /// A window over `core`, its terminals in `cwd` with `env` set, the
    /// `PATH` it is on among them.
    fn start(core: &HcCore, cwd: &Path, env: &[(&str, &OsStr)]) -> FakeWindow {
        let dir = core.dir.join("app");
        std::fs::create_dir_all(&dir).unwrap();
        let quoted = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
        // a `do` is a request file of its words, a line each, answered
        // through a fifo of its own: its exit status, then its stdout
        let script = format!(
            r#"#!/bin/sh
d={dir}
case "$1" in
--version) echo 'herdr-app {APP_VERSION}'; exit 0 ;;
esac
if [ -e "$d/closed" ]; then
  echo "herdr-app: no Herdr window at $d/herdr-app.sock: Connection refused" >&2
  exit 2
fi
case "$1" in
status) /bin/cat "$d/status.json"; exit 0 ;;
do) ;;
*) echo "fake herdr-app: no $1" >&2; exit 64 ;;
esac
r="$d/r.$$"
/usr/bin/mkfifo "$r.fifo" || exit 70
printf '%s\n' "$2" "$3" > "$r.tmp"
/bin/mv "$r.tmp" "$r.req"
{{ read -r code; /bin/cat; }} < "$r.fifo"
/bin/rm -f "$r.fifo"
exit "$code"
"#,
            dir = quoted(&dir)
        );
        let program = dir.join("herdr-app");
        std::fs::write(&program, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();

        let spawn = Spawn {
            dir: dir.clone(),
            socket: core.socket.clone(),
            cwd: cwd.to_path_buf(),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), v.to_str().unwrap().into()))
                .collect(),
        };
        let asked = Arc::new(Mutex::new(Vec::new()));
        let focus = Arc::new(Mutex::new(Focus::default()));
        let stop = Arc::new(AtomicBool::new(false));
        write_status(&dir, &Value::Null, &focus.lock().unwrap());
        let serving = {
            let (dir, asked, focus, stop) =
                (dir.clone(), asked.clone(), focus.clone(), stop.clone());
            std::thread::spawn(move || serve(&dir, &spawn, &asked, &focus, &stop))
        };
        FakeWindow {
            dir,
            socket: core.socket.clone(),
            asked,
            focus,
            stop,
            serving: Some(serving),
        }
    }

    /// Writes `doc` into the core as the window's, showing its first tab,
    /// its first pane focused.
    fn show(&self, doc: &Value) {
        let version =
            hc_call(&self.socket, "doc.get", json!({ "name": "herdr-app" })).unwrap()["version"]
                .as_u64()
                .unwrap();
        let set = json!({ "name": "herdr-app", "value": doc, "version": version });
        hc_call(&self.socket, "doc.set", set).unwrap();
        let mut focus = self.focus.lock().unwrap();
        *focus = Focus::default();
        if let Some((tab, pane)) = tabs_and_panes(doc).into_iter().next() {
            focus.shown = Some(tab.clone());
            focus.of_tab.insert(tab, pane);
        }
        write_status(&self.dir, doc, &focus);
    }

    /// The directory its `herdr-app` is in, for a `PATH`.
    fn bin(&self) -> &Path {
        &self.dir
    }

    /// From now on, no window answers.
    fn close(&self) {
        std::fs::write(self.dir.join("closed"), "").unwrap();
    }

    /// The `do` commands it was asked, with their arguments, in order.
    fn asked(&self) -> Vec<(String, Value)> {
        self.asked.lock().unwrap().clone()
    }
}

impl Drop for FakeWindow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

/// Each tab of `doc` and its first pane, in the doc's order.
fn tabs_and_panes(doc: &Value) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for space in doc["spaces"].as_array().into_iter().flatten() {
        for tab in space["tabs"].as_array().into_iter().flatten() {
            let first = nodes(&json!({ "spaces": [{ "tabs": [tab] }] }))
                .into_iter()
                .find(|(_, n)| n["type"] == "block");
            if let (Some(id), Some((_, block))) = (tab["id"].as_str(), first) {
                found.push((id.to_string(), block["id"].as_str().unwrap().to_string()));
            }
        }
    }
    found
}

/// The space and tab of `doc` that hold pane `pane`.
fn home_of(doc: &Value, pane: &str) -> Option<(String, String)> {
    let path = find_block(doc, pane)?;
    let space = &doc["spaces"][path[0]];
    let tab = &space["tabs"][path[1]];
    Some((
        space["id"].as_str()?.to_string(),
        tab["id"].as_str()?.to_string(),
    ))
}

/// Writes what a fake window's `status` prints, into `dir`, as the app's
/// is recorded: the shown tab, its space, and its focused pane, from
/// `doc` and `focus`.
fn write_status(dir: &Path, doc: &Value, focus: &Focus) {
    let shown = focus.shown.as_deref();
    let pane = shown.and_then(|t| focus.of_tab.get(t));
    let space = pane.and_then(|p| home_of(doc, p)).map(|(space, _)| space);
    let status = json!({
        "zoom": 1.0, "font_size": 14.0, "ui_font_size": 13.0,
        "space": space, "tab": shown, "focused_pane": pane,
        "layout": "main", "agent": null, "floats": {}, "overlays": [], "errors": [],
    });
    let tmp = dir.join("status.json.tmp");
    std::fs::write(&tmp, format!("{status}\n")).unwrap();
    std::fs::rename(&tmp, dir.join("status.json")).unwrap();
}

/// Answers each `do` request a fake window's script leaves in `dir`, on a
/// thread of its own, until `stop`.
fn serve(
    dir: &Path,
    spawn: &Spawn,
    asked: &Mutex<Vec<(String, Value)>>,
    focus: &Mutex<Focus>,
    stop: &AtomicBool,
) {
    // one at a time, as the app's acts change one doc
    while !stop.load(Ordering::SeqCst) {
        let requests: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|d| {
                d.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "req"))
                    .collect()
            })
            .unwrap_or_default();
        if requests.is_empty() {
            std::thread::sleep(Duration::from_millis(5));
        }
        for request in requests {
            let Ok(words) = std::fs::read_to_string(&request) else {
                continue;
            };
            let _ = std::fs::remove_file(&request);
            let mut words = words.lines();
            let command = words.next().unwrap_or_default().to_string();
            let args: Value =
                serde_json::from_str(words.next().unwrap_or_default()).unwrap_or(Value::Null);
            asked.lock().unwrap().push((command.clone(), args.clone()));
            let (code, out) = match act(spawn, &mut focus.lock().unwrap(), &command, &args) {
                Ok(reply) => (0, reply),
                Err(why) => (1, json!({ "error": why })),
            };
            answer(&request.with_extension("fifo"), code, &format!("{out}\n"));
        }
    }
}

/// Writes `code` and `out` into `fifo`, once its script reads it, for up
/// to 10 s: a script killed meanwhile never does.
fn answer(fifo: &Path, code: i32, out: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let due = Instant::now() + Duration::from_secs(10);
    while Instant::now() < due {
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo);
        if let Ok(mut f) = opened {
            let _ = f.write_all(format!("{code}\n{out}").as_bytes());
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What the app answers `do COMMAND ARGS`, acting on the core and moving
/// `focus`; or its `error`.
fn act(spawn: &Spawn, focus: &mut Focus, command: &str, args: &Value) -> Result<Value, String> {
    let call = |method: &str, params: Value| hc_call(&spawn.socket, method, params);
    let (mut doc, version) = {
        let got = call("doc.get", json!({ "name": "herdr-app" }))?;
        (got["value"].clone(), got["version"].as_u64().unwrap_or(0))
    };
    let run = |argv: &Value| -> Result<String, String> {
        let made = call(
            "terminal.run",
            json!({
                "argv": argv, "cwd": spawn.cwd, "env": spawn.env,
                "keep": true, "no_parent": true,
            }),
        )?;
        Ok(made["address"].as_str().unwrap_or_default().to_string())
    };
    let reply = match command {
        "pane.split" => {
            let pane = args["pane"].as_str().ok_or("no pane")?;
            let axis = match args["direction"].as_str() {
                Some("right") => "row",
                Some("down") => "column",
                other => return Err(format!("bad direction {other:?}")),
            };
            if find_block(&doc, pane).is_none() {
                return Err(format!("no pane \"{pane}\""));
            }
            let terminal = run(&args["argv"])?;
            let new = fresh_id(&doc, "p");
            split(&mut doc, pane, axis, block(&new, &terminal));
            // it takes its tab's focus, without showing its tab
            let (_, tab) = home_of(&doc, &new).unwrap();
            focus.of_tab.insert(tab, new.clone());
            json!({ "pane": new, "terminal": terminal })
        }
        "layout.resize" => {
            let id = args["id"].as_str().ok_or("no id")?;
            let node = sized(&mut doc, id).ok_or(format!("no node \"{id}\""))?;
            // as the app: a weight, else pixels, else nothing changes
            if let Some(weight) = args["weight"].as_f64() {
                node["size"] = json!({ "weight": weight });
            } else if let Some(px) = args["px"].as_f64() {
                node["size"] = json!({ "px": px });
            }
            json!({ "ok": true })
        }
        "pane.focus" => {
            let pane = args["pane"].as_str().ok_or("no pane")?;
            let (_, tab) = home_of(&doc, pane).ok_or(format!("no pane \"{pane}\""))?;
            // its tab shown, if it wasn't
            focus.shown = Some(tab.clone());
            focus.of_tab.insert(tab, pane.to_string());
            json!({ "ok": true })
        }
        "pane.swap" => {
            let pane = args["pane"].as_str().ok_or("no pane")?;
            swap(
                &mut doc,
                pane,
                args["direction"].as_str().unwrap_or_default(),
            );
            json!({ "ok": true })
        }
        "tab.new" => {
            let space = args["space"].as_str().ok_or("no space")?;
            let at = doc["spaces"]
                .as_array()
                .and_then(|s| s.iter().position(|s| s["id"] == space))
                .ok_or(format!("no space \"{space}\""))?;
            // a bare tab holds the login shell
            let terminal = run(&json!(["/bin/sh"]))?;
            let (tab, pane) = (fresh_id(&doc, "tb"), fresh_id(&doc, "p"));
            let root = json!({ "type": "row", "id": tab, "children": [block(&pane, &terminal)] });
            doc["spaces"][at]["tabs"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "id": tab, "name": "", "root": root }));
            // `focus: false`: the shown tab stays
            focus.of_tab.insert(tab.clone(), pane);
            json!({ "space": space, "tab": tab, "terminal": terminal })
        }
        other => {
            return Err(format!(
                "unknown command \"{other}\": see herdr-app commands"
            ));
        }
    };
    call(
        "doc.set",
        json!({ "name": "herdr-app", "value": doc, "version": version }),
    )?;
    write_status(&spawn.dir, &doc, focus);
    Ok(reply)
}

/// A block of the doc: pane `pane` showing terminal `terminal`.
fn block(pane: &str, terminal: &str) -> Value {
    json!({ "type": "block", "id": pane, "content": { "terminal": terminal } })
}

/// The doc of one space, `sp1`, of one tab, `tb1`, its one pane `p1`
/// showing terminal `terminal`, as the app writes a window's first.
fn doc_of(terminal: &str) -> Value {
    let root = json!({ "type": "row", "id": "tb1", "children": [block("p1", terminal)] });
    json!({ "spaces": [{ "id": "sp1", "name": "test", "tabs": [
        { "id": "tb1", "name": "", "root": root },
    ] }] })
}

/// An id starting `prefix` that `doc` doesn't use yet.
fn fresh_id(doc: &Value, prefix: &str) -> String {
    let text = doc.to_string();
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|id| !text.contains(&format!("\"{id}\"")))
        .unwrap()
}

/// Every node of `doc`'s tabs, depth first, with the path of indexes to
/// it: a space's, a tab's, then a child's at each level.
fn nodes(doc: &Value) -> Vec<(Vec<usize>, Value)> {
    fn walk(node: &Value, path: Vec<usize>, out: &mut Vec<(Vec<usize>, Value)>) {
        out.push((path.clone(), node.clone()));
        for (i, child) in node["children"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let mut deeper = path.clone();
            deeper.push(i);
            walk(child, deeper, out);
        }
    }
    let mut out = Vec::new();
    for (s, space) in doc["spaces"].as_array().into_iter().flatten().enumerate() {
        for (t, tab) in space["tabs"].as_array().into_iter().flatten().enumerate() {
            walk(&tab["root"], vec![s, t], &mut out);
        }
    }
    out
}

/// The path to block `pane`, as [`nodes`] gives it.
fn find_block(doc: &Value, pane: &str) -> Option<Vec<usize>> {
    nodes(doc)
        .into_iter()
        .find(|(_, n)| n["type"] == "block" && n["id"] == pane)
        .map(|(path, _)| path)
}

/// The node of `doc`'s tabs with id `id`, a pane or a split, for its size.
fn sized<'a>(doc: &'a mut Value, id: &str) -> Option<&'a mut Value> {
    let path = nodes(doc)
        .into_iter()
        .find(|(path, n)| n["id"] == id && path.len() > 2)
        .map(|(path, _)| path)?;
    Some(node_at(doc, &path))
}

/// The node at `path` in `doc`.
fn node_at<'a>(doc: &'a mut Value, path: &[usize]) -> &'a mut Value {
    let mut node = &mut doc["spaces"][path[0]]["tabs"][path[1]]["root"];
    for &i in &path[2..] {
        node = &mut node["children"][i];
    }
    node
}

/// Splits pane `pane` of `doc` along `axis`, `new` after it, as the app
/// does: within its parent, halving its weight, if that runs along
/// `axis`; else in a new split in its place, which takes its size.
fn split(doc: &mut Value, pane: &str, axis: &str, mut new: Value) {
    let path = find_block(doc, pane).unwrap();
    let (at, index) = (&path[..path.len() - 1], path[path.len() - 1]);
    let id = fresh_id(doc, "s");
    let parent = node_at(doc, at);
    if path.len() > 2 && parent["type"] == axis {
        let origin = &mut parent["children"][index];
        let weight = origin["size"]["weight"].as_f64().unwrap_or(1.0) / 2.0;
        origin["size"] = json!({ "weight": weight });
        new["size"] = json!({ "weight": weight });
        parent["children"]
            .as_array_mut()
            .unwrap()
            .insert(index + 1, new);
        return;
    }
    let origin = node_at(doc, &path);
    let size = origin.as_object_mut().unwrap().remove("size");
    let mut wrapped = json!({ "type": axis, "id": id, "children": [origin.clone(), new] });
    if let Some(size) = size {
        wrapped["size"] = size;
    }
    *origin = wrapped;
}

/// Swaps pane `pane` of `doc` with its neighbour toward `direction`, if
/// its parent runs that way and it has one.
fn swap(doc: &mut Value, pane: &str, direction: &str) {
    let path = find_block(doc, pane).unwrap();
    let (at, index) = (&path[..path.len() - 1], path[path.len() - 1]);
    let (axis, back) = match direction {
        "left" => ("row", true),
        "right" => ("row", false),
        "up" => ("column", true),
        _ => ("column", false),
    };
    let parent = node_at(doc, at);
    let children = parent["children"].as_array_mut().unwrap();
    let other = if back {
        index.checked_sub(1)
    } else {
        Some(index + 1)
    };
    if let Some(other) = other.filter(|&o| o < children.len())
        && parent["type"] == axis
    {
        parent["children"]
            .as_array_mut()
            .unwrap()
            .swap(index, other);
    }
}

/// The weight of the block of `doc` showing terminal `terminal`: 1 where
/// it has no size, as the app counts it.
fn weight_of(doc: &Value, terminal: &str) -> f64 {
    let (_, block) = nodes(doc)
        .into_iter()
        .find(|(_, n)| n["type"] == "block" && n["content"]["terminal"] == terminal)
        .unwrap_or_else(|| panic!("no block shows {terminal}: {doc}"));
    block["size"]["weight"].as_f64().unwrap_or(1.0)
}

/// The terminals a tab's blocks show, in the doc's order: each tab's, by
/// tab id.
fn tabs_of(doc: &Value) -> Vec<(String, Vec<String>)> {
    let mut tabs = Vec::new();
    for space in doc["spaces"].as_array().into_iter().flatten() {
        for tab in space["tabs"].as_array().into_iter().flatten() {
            let shown: Vec<String> = nodes(&json!({ "spaces": [{ "tabs": [tab] }] }))
                .into_iter()
                .filter(|(_, n)| n["type"] == "block")
                .map(|(_, n)| n["content"]["terminal"].as_str().unwrap().to_string())
                .collect();
            tabs.push((tab["id"].as_str().unwrap().to_string(), shown));
        }
    }
    tabs
}

/// `provider builtin herdr-app CAPABILITY`, as a self-attach in a
/// terminal of `core` runs it: the core's socket, and a `PATH` finding
/// `window`'s `herdr-app` alone.
fn capability(core: &HcCore, window: &FakeWindow, cap: &str) -> std::process::Command {
    let up = NAME.to_ascii_uppercase();
    let mut cmd = provider_builtin(&["herdr-app", cap]);
    cmd.env(format!("{up}_PROTOCOL"), "1")
        .env(format!("{up}_CAPABILITY"), cap)
        .env("HC_TERMINAL_SOCKET", &core.socket)
        .env("HERDR_APP_HOME", window.bin())
        .env("PATH", window.bin())
        .env("HOME", core.dir.join("home"))
        .stdin(Stdio::null());
    cmd
}

#[test]
fn find_proves_a_core_terminal_by_its_tty_and_its_one_block() {
    if !have_hc() {
        return;
    }
    let core = HcCore::start("find");
    let window = FakeWindow::start(&core, &core.dir, &[]);
    let t = core.run(IDLE, &core.dir, &[]);
    let other = core.run(IDLE, &core.dir, &[]);
    let tty = core.tty_of(&t).expect("the terminal's tty");
    let up = NAME.to_ascii_uppercase();
    let find = |terminal: &str, tty: &str| {
        capability(&core, &window, "find")
            .env(format!("{up}_ENV_HC_TERMINAL_ID"), terminal)
            .env(format!("{up}_TTY"), tty)
            .output()
            .unwrap()
    };
    let refused = |out: std::process::Output, why: &str| {
        assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
        assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
        assert!(text(&out.stderr).contains(why), "{}", text(&out.stderr));
    };

    // the app hasn't written its doc yet
    refused(find(&t, &tty), &format!("{t} has no Herdr app pane"));

    core.set_doc(&doc_of(&t), core.doc().1);
    let out = find(&t, &tty);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), format!("{t}\n"));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));

    // another terminal's tty isn't its
    let theirs = core.tty_of(&other).expect("the other terminal's tty");
    refused(
        find(&t, &theirs),
        &format!("this terminal ({theirs}) isn't hc terminal {t}'s ({tty})"),
    );

    // shown twice, it's no one pane
    let mut twice = doc_of(&t);
    split(&mut twice, "p1", "row", block("p2", &t));
    core.set_doc(&twice, core.doc().1);
    refused(find(&t, &tty), &format!("{t} shows in 2 Herdr app panes"));
    core.set_doc(&doc_of(&t), core.doc().1);

    // no window answers
    window.close();
    refused(find(&t, &tty), "no Herdr window at ");
}

#[test]
fn close_kills_the_terminal_and_is_done_for_one_finished_or_unknown() {
    if !have_hc() {
        return;
    }
    let core = HcCore::start("close");
    let window = FakeWindow::start(&core, &core.dir, &[]);
    let t = core.run(IDLE, &core.dir, &[]);
    let up = NAME.to_ascii_uppercase();
    let close = |terminal: &str| {
        capability(&core, &window, "close")
            .env(format!("{up}_TERMINAL"), terminal)
            .output()
            .unwrap()
    };
    for terminal in [t.as_str(), t.as_str(), "t99"] {
        let out = close(terminal);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(out.stdout.is_empty() && out.stderr.is_empty());
    }
    let record = core.terminal(&t);
    assert_eq!(record["state"], "finished", "{record}");
    assert_eq!(record["reason"], "killed", "{record}");
    // the window isn't asked
    assert!(window.asked().is_empty());
}

#[test]
fn open_beside_splits_the_origins_pane_running_the_attach_its_keep_off() {
    if !have_hc() {
        return;
    }
    let core = HcCore::start("beside");
    let window = FakeWindow::start(&core, &core.dir, &[]);
    let up = NAME.to_ascii_uppercase();
    // the new pane first along the tab's one split, or second
    for (direction, first) in [
        ("right", false),
        ("left", true),
        ("down", false),
        ("up", true),
    ] {
        let origin = core.run(IDLE, &core.dir, &[]);
        window.show(&doc_of(&origin));
        let out = capability(&core, &window, "open_beside")
            .env(format!("{up}_ORIGIN"), &origin)
            .env(format!("{up}_DIRECTION"), direction)
            .env(format!("{up}_RATIO"), "0.3")
            .env(format!("{up}_ATTACH"), "/bin/sleep 100000")
            .env(format!("{up}_TITLE"), "t")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{direction}: {}",
            text(&out.stderr)
        );
        assert!(out.stderr.is_empty(), "{direction}: {}", text(&out.stderr));
        let new = text(&out.stdout).trim().to_string();
        let record = core.terminal(&new);
        assert_eq!(record["state"], "live", "{direction}: {record}");
        assert_eq!(
            record["argv"],
            json!(["/bin/sleep", "100000"]),
            "{direction}"
        );
        // the core gives it no shell again once it's lost
        assert_eq!(record["keep"], false, "{direction}: {record}");
        let order = if first {
            [new.clone(), origin.clone()]
        } else {
            [origin.clone(), new.clone()]
        };
        let doc = core.doc().0;
        assert_eq!(
            tabs_of(&doc),
            [("tb1".to_string(), order.to_vec())],
            "{direction}"
        );
        // RATIO is the new pane's share of the weight the pair has
        let (theirs, ours) = (weight_of(&doc, &new), weight_of(&doc, &origin));
        let share = theirs / (theirs + ours);
        assert!((share - 0.3).abs() < 1e-3, "{direction}: {share}: {doc}");
        core.call("terminal.kill", json!({ "terminal": new }));
        core.call("terminal.kill", json!({ "terminal": origin }));
    }
    let swaps: Vec<Value> = window
        .asked()
        .into_iter()
        .filter(|(c, _)| c == "pane.swap")
        .map(|(_, args)| args["direction"].clone())
        .collect();
    assert_eq!(swaps, [json!("left"), json!("up")]);
}

#[test]
fn open_tab_page_makes_a_tab_in_the_origins_space_of_the_attach_alone_its_keep_off() {
    if !have_hc() {
        return;
    }
    let core = HcCore::start("tabpage");
    let window = FakeWindow::start(&core, &core.dir, &[]);
    let origin = core.run(IDLE, &core.dir, &[]);
    window.show(&doc_of(&origin));
    let up = NAME.to_ascii_uppercase();
    let out = capability(&core, &window, "open_tab_page")
        .env(format!("{up}_ORIGIN"), &origin)
        .env(format!("{up}_ATTACH"), "/bin/sleep 100000")
        .env(format!("{up}_TITLE"), "t")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
    let new = text(&out.stdout).trim().to_string();
    let record = core.terminal(&new);
    assert_eq!(record["state"], "live", "{record}");
    assert_eq!(record["argv"], json!(["/bin/sleep", "100000"]));
    assert_eq!(record["keep"], false, "{record}");

    // a bare tab in the origin's space, not shown, the attach split in
    let asked = window.asked();
    assert_eq!(asked[0].0, "tab.new");
    assert_eq!(asked[0].1, json!({ "focus": false, "space": "sp1" }));
    let tabs = tabs_of(&core.doc().0);
    assert_eq!(tabs[0], ("tb1".to_string(), vec![origin.clone()]));
    let (tab, shown) = &tabs[1];
    let shell = &shown[0];
    assert_eq!(shown, &[shell.clone(), new.clone()], "tab {tab}");
    // its first shell killed: the attach is left alone in it
    let record = core.terminal(shell);
    assert_eq!(record["state"], "finished", "{record}");
    assert_eq!(record["reason"], "killed", "{record}");
}

/// What `neopane ARGS…` printed, stdout and stderr together, run in a new
/// terminal of `core` with `env` over the core's own, once it ends: a
/// Herdr app pane's, as `window` shows it in a tab of its own before it
/// starts.
fn in_pane(core: &HcCore, window: &FakeWindow, args: &[&str], env: &[(&str, &OsStr)]) -> String {
    let (out, go) = (core.dir.join("printed"), core.dir.join("go"));
    let exe = exe();
    let mut argv = vec![
        "/bin/sh",
        "-c",
        r#"while [ ! -e "$1" ]; do sleep 0.05; done; out=$2; shift 2; "$0" "$@" >"$out" 2>&1"#,
        exe.to_str().unwrap(),
        go.to_str().unwrap(),
        out.to_str().unwrap(),
    ];
    argv.extend(args);
    let t = core.run(&argv, &core.dir, env);
    window.show(&doc_of(&t));
    std::fs::write(&go, "").unwrap();
    let record = until(|r: &Value| r["state"] == "finished", || core.terminal(&t));
    assert_eq!(record["state"], "finished", "{record}");
    std::fs::read_to_string(&out).unwrap()
}

#[test]
fn provider_check_in_a_pane_with_a_stale_zellijs_variables_proves_it_inside_the_herdr_app() {
    if !have_hc() {
        return;
    }
    let core = HcCore::start("inside");
    let window = FakeWindow::start(&core, &core.dir, &[]);
    let path = std::env::join_paths([window.bin(), Path::new("/usr/bin"), Path::new("/bin")]);
    let env = [
        (
            neopane::daemon::dir::tmpdir_var(),
            core.dir.as_os_str().to_owned(),
        ),
        ("XDG_CONFIG_HOME".into(), core.dir.join("config").into()),
        ("PATH".into(), path.unwrap()),
        ("HERDR_APP_HOME".into(), window.bin().into()),
        // copied from another terminal: zellij's is named too, so the
        // proofs decide
        ("ZELLIJ".into(), "0".into()),
        ("ZELLIJ_PANE_ID".into(), "3".into()),
    ];
    let env: Vec<(&str, &OsStr)> = env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect();
    let printed = in_pane(&core, &window, &["provider", "check", "--json"], &env);
    let v: Value = serde_json::from_str(&printed).unwrap_or_else(|e| panic!("{e}: {printed}"));
    assert_eq!(v["default"]["named"], json!(["zellij", "herdr-app"]), "{v}");
    let proofs = v["default"]["inside"].as_array().unwrap();
    assert_eq!(proofs[0]["ok"], false, "{v}");
    let proof = &proofs[1];
    assert_eq!(proof["name"], "herdr-app", "{v}");
    assert_eq!(proof["ok"], true, "{v}");
    let t = v["find"]["id"].as_str().unwrap_or_default();
    assert_eq!(proof["why"], format!("hc terminal {t}, Herdr app pane p1"));
    assert_eq!(v["chose"]["name"], "herdr-app", "{v}");
    assert_eq!(v["find"]["verdict"], "accepted", "{v}");
    assert_eq!(v["result"]["state"], "ok", "{v}");
}

#[test]
fn in_a_pane_a_split_opens_a_pane_running_the_attach_and_close_kills_it() {
    if !have_nvim() || !have_hc() {
        return;
    }
    let core = HcCore::start("live");
    let tmp = core.dir.join("d");
    std::fs::create_dir(&tmp).unwrap();
    let bin = core.dir.join("app");
    let env = [
        (
            neopane::daemon::dir::tmpdir_var(),
            tmp.as_os_str().to_owned(),
        ),
        ("XDG_CONFIG_HOME".into(), tmp.as_os_str().to_owned()),
        ("XDG_STATE_HOME".into(), tmp.as_os_str().to_owned()),
        ("PATH".into(), path_with(&bin)),
        ("HERDR_APP_HOME".into(), bin.clone().into()),
    ];
    let env: Vec<(&str, &OsStr)> = env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_os_str()))
        .collect();
    let window = FakeWindow::start(&core, &tmp, &env);
    assert_eq!(window.bin(), bin);
    let exe = exe();
    let origin = core.run(&[exe.to_str().unwrap(), "--name", "work"], &tmp, &env);
    window.show(&doc_of(&origin));
    let sock = base_of(&tmp).join("work/nvim.sock");
    // ends the daemon as it drops, before the window and the core
    let _ending = Ending {
        sock: sock.clone(),
        helpers: Vec::new(),
        tmp: tmp.clone(),
    };
    let log = || std::fs::read_to_string(base_of(&tmp).join("work/log")).unwrap_or_default();
    let status = || json(&under(&tmp, &["status", "--json", "--name", "work"]));

    until_expr(&sock, "1", |_| true).expect("work's Neovim never answered");
    let s = until(|s: &Value| s["provider"] == "herdr-app", status);
    assert_eq!(s["provider"], "herdr-app", "{}", log());

    remote_expr(&sock, "execute('vsplit')").unwrap();
    let s = until(|s: &Value| s["panes"] == 2, status);
    assert_eq!(s["panes"], 2, "its attach never came: {}", log());
    // swapped to the origin's left, maybe after the attach came
    let left = |t: &Vec<(String, Vec<String>)>| {
        t.len() == 1 && t.first().and_then(|(_, shown)| shown.get(1)) == Some(&origin)
    };
    let tabs = until(left, || tabs_of(&core.doc().0));
    let [(_, shown)] = tabs.as_slice() else {
        panic!("{tabs:?}");
    };
    assert_eq!(shown.len(), 2, "{tabs:?}");
    assert_eq!(shown[1], origin, "{tabs:?}");
    let new = shown[0].clone();
    let record = core.terminal(&new);
    assert!(
        record["argv"].to_string().contains("\"attach\""),
        "{record}"
    );
    assert_eq!(record["keep"], false, "{record}");

    remote_expr(&sock, "execute('close')").unwrap();
    let s = until(|s: &Value| s["panes"] == 1, status);
    assert_eq!(s["panes"], 1, "{}", log());
    let record = until(|r: &Value| r["state"] == "finished", || core.terminal(&new));
    assert_eq!(record["state"], "finished", "{record}");
}

/// Against a real Herdr app window, opt-in: `just test-herdr-app`, never
/// `just test` or CI. Each opens a private window of the installed
/// Herdr.app (macOS), under a `HERDR_APP_HOME` of its own, on a private
/// core, and, where AeroSpace runs, moves it at once to a workspace no
/// monitor shows, showing each monitor's workspace again after, and the
/// focused window: the window opens on the focused screen.
mod window {
    use super::*;

    /// The installed app's binary: also its `herdr-app` command, chosen by
    /// its name.
    const APP: &str = "/Applications/Herdr.app/Contents/MacOS/Herdr";

    /// A private Herdr app window on a private core, quit and stopped as it
    /// drops.
    pub(super) struct AppWindow {
        pub core: HcCore,
        /// Its `HERDR_APP_HOME`.
        pub home: PathBuf,
        /// Where its `herdr-app`, `hc` and `herdr-core` are, for a `PATH`.
        pub bin: PathBuf,
        app: std::process::Child,
    }

    impl AppWindow {
        /// Opens one for test `test`, and waits for its first tab's
        /// terminal: panics without macOS, Herdr.app or `hc`.
        pub(super) fn open(test: &str) -> AppWindow {
            // panics rather than skips, deliberately: these run only when
            // asked for, and a skip would pass with nothing tested
            assert!(
                cfg!(target_os = "macos") && Path::new(APP).exists(),
                "the window tests need Herdr.app installed in /Applications"
            );
            assert!(have_hc(), "the window tests need hc on PATH");
            let core = HcCore::start(test);
            let (home, bin) = (core.dir.join("apphome"), core.dir.join("bin"));
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(&bin).unwrap();
            let hc = which("hc").expect("hc on PATH");
            let herdr_core = std::fs::canonicalize(&hc).unwrap();
            std::os::unix::fs::symlink(&herdr_core, bin.join("herdr-core")).unwrap();
            std::os::unix::fs::symlink(&herdr_core, bin.join("hc")).unwrap();
            std::os::unix::fs::symlink(APP, bin.join("herdr-app")).unwrap();
            std::fs::write(
                home.join("settings.json"),
                json!({ "core": bin.join("herdr-core") }).to_string(),
            )
            .unwrap();
            // no welcome overlay, and no panels taking its width
            std::fs::write(
                home.join("shared.json"),
                json!({ "frame.welcomed": "skipped", "workspace.panels": [] }).to_string(),
            )
            .unwrap();

            let shown = Shown::now();
            let log = std::fs::File::create(core.dir.join("app.log")).unwrap();
            let mut cmd = std::process::Command::new(APP);
            let app = HcCore::isolate(&mut cmd)
                .envs(core.env())
                .env("HERDR_APP_HOME", &home)
                .env("PATH", path_with(&bin))
                .env("SHELL", "/bin/sh")
                .current_dir(&core.dir)
                .stdin(Stdio::null())
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap();
            let window = AppWindow {
                core,
                home,
                bin,
                app,
            };
            if let Some(shown) = shown {
                shown.hide(window.app.id());
            }
            // a new home's window has no space: its first, of one tab
            // of one pane, running a shell
            let space = json!({ "name": "test", "folder": window.core.dir, "focus": true });
            let opened = until(
                |o: &Option<Value>| o.is_some(),
                || window.try_app(&["do", "space.new", &space.to_string()]),
            );
            assert!(opened.is_some(), "space.new never answered");
            let first = until(
                |t: &Option<String>| t.is_some(),
                || window.terminals().into_iter().next(),
            );
            assert!(
                first.is_some(),
                "the window never wrote its doc: {}",
                std::fs::read_to_string(window.core.dir.join("app.log")).unwrap_or_default()
            );
            window
        }

        /// The terminals its doc shows, each tab's in order.
        pub(super) fn terminals(&self) -> Vec<String> {
            tabs_of(&self.core.doc().0)
                .into_iter()
                .flat_map(|(_, shown)| shown)
                .collect()
        }

        /// `provider builtin herdr-app CAPABILITY`, as a self-attach in one
        /// of its panes runs it.
        pub(super) fn capability(&self, cap: &str) -> std::process::Command {
            let up = NAME.to_ascii_uppercase();
            let mut cmd = provider_builtin(&["herdr-app", cap]);
            cmd.env(format!("{up}_PROTOCOL"), "1")
                .env(format!("{up}_CAPABILITY"), cap)
                .env("HC_TERMINAL_SOCKET", &self.core.socket)
                .env("HERDR_APP_HOME", &self.home)
                .env("PATH", path_with(&self.bin))
                .env("HOME", self.core.dir.join("home"))
                .stdin(Stdio::null());
            cmd
        }

        /// `herdr-app ARGS…` on this window: its one JSON object.
        pub(super) fn app(&self, args: &[&str]) -> Value {
            self.try_app(args)
                .unwrap_or_else(|| panic!("herdr-app {args:?} failed"))
        }

        /// `herdr-app ARGS…` on this window: its one JSON object, if it
        /// succeeded.
        fn try_app(&self, args: &[&str]) -> Option<Value> {
            let out = std::process::Command::new(self.bin.join("herdr-app"))
                .args(args)
                .env("HERDR_APP_HOME", &self.home)
                .stdin(Stdio::null())
                .output()
                .ok()?;
            out.status
                .success()
                .then(|| serde_json::from_slice(&out.stdout).ok())
                .flatten()
        }
    }

    impl Drop for AppWindow {
        fn drop(&mut self) {
            let _ = std::process::Command::new(self.bin.join("herdr-app"))
                .args(["do", "app.quit"])
                .env("HERDR_APP_HOME", &self.home)
                .stdin(Stdio::null())
                .output();
            let due = Instant::now() + Duration::from_secs(10);
            while self.app.try_wait().ok().flatten().is_none() && Instant::now() < due {
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = self.app.kill();
            let _ = self.app.wait();
            // the core, stopped next, as the app is gone
        }
    }

    /// Where `program` is on `PATH`.
    fn which(program: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(program))
            .find(|p| p.is_file())
    }

    /// What AeroSpace shows: each monitor's workspace, the focused one,
    /// and the focused window.
    struct Shown {
        visible: Vec<String>,
        focused: String,
        window: Option<u64>,
    }

    /// `aerospace ARGS…`'s JSON; None if it doesn't answer.
    fn aerospace(args: &[&str]) -> Option<Value> {
        let out = std::process::Command::new("aerospace")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| serde_json::from_slice(&out.stdout).ok())
            .flatten()
    }

    impl Shown {
        /// What it shows now; None without AeroSpace.
        fn now() -> Option<Shown> {
            let visible =
                aerospace(&["list-workspaces", "--monitor", "all", "--visible", "--json"])?;
            let focused = aerospace(&["list-workspaces", "--focused", "--json"])?;
            let window = aerospace(&["list-windows", "--focused", "--json"]);
            let name = |w: &Value| w["workspace"].as_str().unwrap_or_default().to_string();
            Some(Shown {
                visible: visible.as_array()?.iter().map(name).collect(),
                focused: name(&focused[0]),
                window: window.and_then(|w| w[0]["window-id"].as_u64()),
            })
        }

        /// Moves app `pid`'s window, once it has one, to a workspace no
        /// monitor shows, then shows again what was shown.
        fn hide(&self, pid: u32) {
            let pid = pid.to_string();
            let listed = || {
                aerospace(&["list-windows", "--monitor", "all", "--pid", &pid, "--json"])
                    .and_then(|w| w[0]["window-id"].as_u64())
            };
            let Some(id) = until(Option::is_some, listed) else {
                eprintln!("AeroSpace never listed the Herdr app's window: it stays where it is");
                return;
            };
            let hidden = format!("{NAME}-test");
            let id = id.to_string();
            let run = |args: &[&str]| {
                let _ = std::process::Command::new("aerospace")
                    .args(args)
                    .stdin(Stdio::null())
                    .output();
            };
            run(&["move-node-to-workspace", "--window-id", &id, &hidden]);
            for w in self.visible.iter().filter(|w| **w != self.focused) {
                run(&["workspace", w]);
            }
            run(&["workspace", &self.focused]);
            if let Some(window) = self.window {
                run(&["focus", "--window-id", &window.to_string()]);
            }
        }
    }

    #[test]
    #[ignore = "opens a Herdr app window: just test-herdr-app"]
    fn in_a_window_find_open_beside_open_tab_page_and_close_act_on_its_panes() {
        let window = AppWindow::open("window");
        let core = &window.core;
        let origin = window.terminals().first().cloned().expect("its first pane");
        let up = NAME.to_ascii_uppercase();

        // find proves its first pane's terminal
        let tty = core.tty_of(&origin).expect("its terminal's tty");
        let out = window
            .capability("find")
            .env(format!("{up}_ENV_HC_TERMINAL_ID"), &origin)
            .env(format!("{up}_TTY"), &tty)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), format!("{origin}\n"));

        // a pane on its left, running the attach, its keep off
        let out = window
            .capability("open_beside")
            .env(format!("{up}_ORIGIN"), &origin)
            .env(format!("{up}_DIRECTION"), "left")
            .env(format!("{up}_RATIO"), "0.5")
            .env(format!("{up}_ATTACH"), "/bin/sleep 100000")
            .env(format!("{up}_TITLE"), "t")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
        let beside = text(&out.stdout).trim().to_string();
        let record = core.terminal(&beside);
        assert_eq!(record["argv"], json!(["/bin/sleep", "100000"]), "{record}");
        assert_eq!(record["keep"], false, "{record}");
        let both = [beside.clone(), origin.clone()];
        let tabs = until(
            |t: &Vec<(String, Vec<String>)>| shown(t, 0) == Some(&both[..]),
            || tabs_of(&core.doc().0),
        );
        assert_eq!(shown(&tabs, 0), Some(&both[..]), "{tabs:?}");

        // a tab of the attach alone, not shown
        let shown_tab = window.app(&["status"])["tab"].clone();
        let out = window
            .capability("open_tab_page")
            .env(format!("{up}_ORIGIN"), &origin)
            .env(format!("{up}_ATTACH"), "/bin/sleep 100000")
            .env(format!("{up}_TITLE"), "t")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
        let paged = text(&out.stdout).trim().to_string();
        assert_eq!(core.terminal(&paged)["keep"], false);
        let alone = std::slice::from_ref(&paged);
        let tabs = until(
            |t: &Vec<(String, Vec<String>)>| t.len() == 2 && shown(t, 1) == Some(alone),
            || tabs_of(&core.doc().0),
        );
        assert_eq!(tabs.len(), 2, "{tabs:?}");
        assert_eq!(shown(&tabs, 1), Some(alone), "{tabs:?}");
        assert_eq!(window.app(&["status"])["tab"], shown_tab);

        // close kills each; its pane, and the tab, go
        for t in [&beside, &paged] {
            let out = window
                .capability("close")
                .env(format!("{up}_TERMINAL"), t)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        }
        let only = std::slice::from_ref(&origin);
        let tabs = until(
            |t: &Vec<(String, Vec<String>)>| t.len() == 1 && shown(t, 0) == Some(only),
            || tabs_of(&core.doc().0),
        );
        assert_eq!(tabs.len(), 1, "{tabs:?}");
        assert_eq!(shown(&tabs, 0), Some(only), "{tabs:?}");
    }

    /// The terminals tab `i` of `tabs` shows; None if there's no such tab.
    fn shown(tabs: &[(String, Vec<String>)], i: usize) -> Option<&[String]> {
        tabs.get(i).map(|(_, shown)| shown.as_slice())
    }
}
