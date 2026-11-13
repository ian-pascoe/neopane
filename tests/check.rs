//! `provider check` through the built binary (#93 decision 30, #94
//! decision 33): the config found and run as the daemon would, the choice's
//! report, and `find` run as a self-attach here would.

mod support;

use std::collections::BTreeSet;
use std::io::Read;
use std::io::Write;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};

use neopane::name::NAME;
use serde_json::Value;
use support::{exe, scratch, text};

/// A home of its own for one test, with `work/` to run in and the config,
/// if any, at `~/.config/<name>/init.lua`.
struct Home {
    dir: PathBuf,
}

impl Home {
    fn new(test: &str) -> Home {
        let dir = scratch(&format!("check-{test}"));
        std::fs::create_dir_all(dir.join("work")).unwrap();
        Home { dir }
    }

    /// `source` as the config, `{name}` standing for the name.
    fn with_config(self, source: &str) -> Home {
        let at = self.config();
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, source.replace("{name}", NAME)).unwrap();
        self
    }

    fn config(&self) -> PathBuf {
        self.dir.join(".config").join(NAME).join("init.lua")
    }

    /// `provider check` with `args`, in `work/`, stdin not a terminal, with
    /// no pane provider or config variables around it.
    fn command(&self, args: &[&str]) -> Command {
        let upper = NAME.to_ascii_uppercase();
        let mut c = Command::new(exe());
        c.args(["provider", "check"])
            .args(args)
            .current_dir(self.dir.join("work"))
            .env("HOME", &self.dir)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove(&upper)
            .env_remove(format!("{upper}_CONFIG"))
            .env_remove(format!("{upper}_PROVIDER"))
            .env_remove("REX_SERVER")
            .env_remove("REX_SESSION")
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("TERM_PROGRAM")
            .env_remove("REX_BLOCK")
            .stdin(Stdio::null());
        c
    }

    fn check(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn stdout(out: &Output) -> &str {
    text(&out.stdout)
}

/// The report's line starting with `label` and two spaces.
fn line<'a>(out: &'a Output, label: &str) -> &'a str {
    let start = format!("{label}  ");
    stdout(out)
        .lines()
        .find(|l| l.starts_with(&start))
        .unwrap_or_else(|| panic!("no {label} line in:\n{}", stdout(out)))
}

const PAIR: &str = r#"
print("choosing")
return {
  provider = function(attach)
    return {
      terminal = { name = "ghostty", open = { "ghostty-open", "--title", "{title}", "{attach}" } },
      layout = {
        name = "aero",
        find = { "sh", "-c", "echo found; exit 0" },
        focus = { "aero", "focus", "{terminal}" },
        timeouts = { focus = 1.5 },
        context_vars = { "TERM_PROGRAM" },
        find_vars = { "WINDOWID" },
        open_tab_page = { "aero-tab" },
      },
    }
  end,
}
"#;

#[test]
fn with_no_file_the_default_chooses() {
    let home = Home::new("no-file");
    let out = home.check(&[]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!(
        format!("config  no file: {NAME}.default chooses"),
        line(&out, "config")
    );
    assert_eq!(
        format!("chose  none, by {NAME}.default"),
        line(&out, "chose")
    );
    assert_eq!("result  none", line(&out, "result"));
}

#[test]
fn the_hint_none_skips_the_file() {
    let home = Home::new("hint-none").with_config("error('never run')");
    let out = home.check(&["--provider", "none"]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!("config  skipped: hint none", line(&out, "config"));
    assert!(!stdout(&out).contains("never run"), "{}", stdout(&out));
    assert_eq!("result  none", line(&out, "result"));
}

#[test]
fn a_config_error_exits_1_quoting_the_neovim_message() {
    let home = Home::new("error").with_config("return { provider = function() error('boom') end }");
    let out = home.check(&[]);
    assert_eq!(Some(1), out.status.code(), "{}", stdout(&out));
    let result = line(&out, "result");
    let want = format!("result  error: {NAME}: config: provider() raised: ");
    assert!(result.starts_with(&want), "{result}");
    assert!(result.ends_with(": boom"), "{result}");
    assert!(stdout(&out).contains("errors\n  provider() raised: "));
}

#[test]
fn a_named_file_that_isnt_there_is_an_error_saying_what_named_it() {
    let home = Home::new("missing");
    let work = home.dir.join("work").canonicalize().unwrap();
    let out = home.check(&["--config", "gone.lua"]);
    assert_eq!(Some(1), out.status.code(), "{}", stdout(&out));
    assert_eq!(
        format!(
            "result  error: {NAME}: config: {} isn't there (--config names it); no pane provider",
            work.join("gone.lua").display(),
        ),
        line(&out, "result")
    );
    assert_eq!("find  not run: the config failed", line(&out, "find"));
    let upper = NAME.to_ascii_uppercase();
    let out = home
        .command(&[])
        .env(format!("{upper}_CONFIG"), "gone.lua")
        .output()
        .unwrap();
    assert_eq!(Some(1), out.status.code(), "{}", stdout(&out));
    assert_eq!(
        format!(
            "result  error: {NAME}: config: {} isn't there ({upper}_CONFIG names it); no pane \
             provider",
            work.join("gone.lua").display(),
        ),
        line(&out, "result")
    );
}

#[test]
fn a_pairs_report_shows_each_table_in_use() {
    let home = Home::new("pair").with_config(PAIR);
    let out = home
        .command(&[])
        .env("TERM_PROGRAM", "ghostty")
        .output()
        .unwrap();
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let report = stdout(&out);
    assert_eq!(
        format!("config  {}", home.config().display()),
        line(&out, "config")
    );
    assert!(line(&out, "ran").ends_with(", 1 line printed"), "{report}");
    assert!(report.contains("\n  choosing\n"), "{report}");
    assert_eq!(
        "chose  a pair, ghostty+aero, at provider(), by provider()",
        line(&out, "chose")
    );
    for want in [
        "terminal  ghostty at provider().terminal, protocol 1\n\
         \x20 open  ghostty-open --title {title} {attach}  (5 s)\n\
         \x20 close  absent: a terminal it opened closes only as its attach ends\n\
         \x20 open_beside  absent: open, then place_beside\n\
         \x20 open_float  absent: float groups are drawn in panes\n\
         \x20 open_tab_page  absent: tab pages stay whole in the pane they opened in\n\
         \x20 attach timeout  5 s (default)\n",
        "layout  aero at provider().layout, protocol 1\n\
         \x20 find  sh -c 'echo found; exit 0'  (3 s)\n\
         \x20 place_beside  absent: a terminal it opened stays where it opened\n",
        "\n  focus  aero focus {terminal}  (1.5 s)\n",
        // a half's one-step capability is ignored, and says why
        "\n  open_tab_page  ignored: a half's one-step capability is ignored; only a table \
         returned whole gets one\n",
        "\n  live layout  no: each pane is its own layout area\n\
         \x20 reach  TERM_PROGRAM=ghostty\n\
         \x20 find vars  WINDOWID\n",
        "\none-step  none: a pair opens, then places\n",
    ] {
        assert!(report.contains(want), "{want}\nnot in:\n{report}");
    }
    // stdin isn't a terminal
    assert_eq!(
        "find  not run: stdin isn't a terminal, so there's no title to write and nothing to find",
        line(&out, "find")
    );
    assert_eq!("result  ok (find not run)", line(&out, "result"));
}

#[test]
fn a_whole_tables_report_shows_its_one_step_capabilities_and_what_falls_back() {
    let home = Home::new("whole").with_config(
        r#"return { provider = function()
          return { name = "mux", open = { "o" }, open_beside = { "ob", "{direction}" },
                   report_layout = { "r" }, attach_timeout = 9, x = 1 }
        end }"#,
    );
    let out = home.check(&[]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let report = stdout(&out);
    for want in [
        "\nwhole  mux at provider(), protocol 1\n",
        "\n  open_float  absent: float groups are drawn in panes\n",
        "\n  live layout  yes: report_layout (read after each act)\n",
        "\n  attach timeout  9 s\n",
        "\n  reach  every terminal (no context variables)\n",
        "\none-step  open_beside\n",
        "\nwarnings\n  provider().x: unknown field; ignored\n",
    ] {
        assert!(report.contains(want), "{want}\nnot in:\n{report}");
    }
}

#[test]
fn no_find_skips_find() {
    let home = Home::new("no-find").with_config(PAIR);
    let out = home.check(&["--no-find"]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!("find  not run: --no-find", line(&out, "find"));
    assert_eq!("result  ok (find not run)", line(&out, "result"));
}

#[test]
fn full_cuts_nothing() {
    let long = "x".repeat(120);
    let source = format!(
        "for i = 1, 25 do print('line ' .. i) end\n\
         return {{ provider = function() return {{ open = {{ '{long}' }} }} end }}"
    );
    let home = Home::new("full").with_config(&source);
    let cut = home.check(&[]);
    let report = stdout(&cut);
    assert!(
        report.contains("\n  line 20\n  (5 more: --full)\n"),
        "{report}"
    );
    assert!(!report.contains("line 21"), "{report}");
    let open = report.lines().find(|l| l.starts_with("  open  ")).unwrap();
    assert_eq!(100, open.chars().count(), "{open}");
    assert!(open.ends_with('…'), "{open}");
    let full = home.check(&["--full"]);
    let report = stdout(&full);
    assert!(report.contains("\n  line 25\n"), "{report}");
    assert!(
        report.contains(&format!("  open  {long}  (5 s)\n")),
        "{report}"
    );
}

/// The keys #94 decision 33 documents, by object.
fn documented(object: &str) -> BTreeSet<&'static str> {
    let keys: &[&str] = match object {
        "top" => &[
            "config", "attach", "ran", "chose", "tables", "one_step", "find", "rex", "tmux",
            "default", "problems", "result",
        ],
        "config" => &["path", "state"],
        "attach" => &["tty", "pid", "cwd", "daemon", "provider"],
        "ran" => &["seconds", "printed", "error"],
        "chose" => &["kind", "name", "path", "how"],
        "table" => &[
            "role",
            "name",
            "path",
            "protocol",
            "capabilities",
            "settings",
            "live_layout",
            "context_vars",
            "find_vars",
        ],
        "capability" => &["state", "argv", "timeout", "fallback", "why"],
        "find" => &["ran", "title", "exit", "seconds", "id", "verdict", "why"],
        "rex" => &["server", "version", "api_revision", "missing", "find"],
        "tmux" => &["path", "version", "socket", "answers", "error", "missing"],
        "problem" => &["level", "path", "message"],
        "result" => &["state", "message", "refused"],
        "default" => &["named", "inside"],
        _ => unreachable!(),
    };
    keys.iter().copied().collect()
}

/// Fails unless `v` is an object whose keys are among `object`'s.
fn keys_in(v: &Value, object: &str) {
    let have: BTreeSet<&str> = v
        .as_object()
        .unwrap_or_else(|| panic!("{object} isn't an object: {v}"))
        .keys()
        .map(String::as_str)
        .collect();
    let known = documented(object);
    let extra: Vec<_> = have.difference(&known).collect();
    assert!(extra.is_empty(), "{object} has undocumented {extra:?}: {v}");
}

#[test]
fn json_has_the_documented_keys() {
    let home = Home::new("json").with_config(PAIR);
    let out = home.check(&["--json"]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    keys_in(&v, "top");
    assert_eq!(
        documented("top"),
        v.as_object().unwrap().keys().map(String::as_str).collect()
    );
    for object in ["config", "attach", "ran", "chose", "find", "result"] {
        keys_in(&v[object], object);
    }
    assert_eq!("file", v["config"]["state"]);
    assert_eq!("pair", v["chose"]["kind"]);
    assert_eq!("provider", v["chose"]["how"]);
    assert_eq!("", v["attach"]["tty"]);
    assert_eq!("work", v["attach"]["daemon"]);
    assert_eq!(serde_json::json!(["choosing"]), v["ran"]["printed"]);
    let tables = v["tables"].as_array().unwrap();
    assert_eq!(2, tables.len());
    for t in tables {
        keys_in(t, "table");
        for c in t["capabilities"].as_object().unwrap().values() {
            keys_in(c, "capability");
        }
    }
    assert_eq!("terminal", tables[0]["role"]);
    assert_eq!("layout", tables[1]["role"]);
    assert_eq!("present", tables[1]["capabilities"]["focus"]["state"]);
    assert_eq!(1.5, tables[1]["capabilities"]["focus"]["timeout"]);
    assert_eq!("absent", tables[1]["capabilities"]["resize"]["state"]);
    assert_eq!("no", tables[1]["live_layout"]);
    assert_eq!(
        serde_json::json!({"TERM_PROGRAM": null}),
        tables[1]["context_vars"]
    );
    assert_eq!(serde_json::json!([]), v["one_step"]);
    assert_eq!("not run", v["find"]["verdict"]);
    assert_eq!(false, v["find"]["ran"]);
    assert_eq!(Value::Null, v["rex"]);
    assert_eq!(Value::Null, v["tmux"]);
    let why = "a half's one-step capability is ignored; only a table returned whole gets one";
    assert_eq!(
        serde_json::json!({ "state": "ignored", "why": why }),
        tables[1]["capabilities"]["open_tab_page"]
    );
    assert_eq!(
        serde_json::json!({ "state": "absent", "fallback": "float groups are drawn in panes" }),
        tables[0]["capabilities"]["open_float"]
    );
    assert_eq!(
        serde_json::json!([{"level": "warning", "path": "provider().layout.open_tab_page",
                            "message": why}]),
        v["problems"]
    );
    // a check-only warning shows nowhere in Neovim
    assert_eq!(
        serde_json::json!({ "state": "ok", "message": null }),
        v["result"]
    );

    let home = Home::new("json-error").with_config("return { provider = 1, x = 2 }");
    let out = home.check(&["--json"]);
    assert_eq!(Some(1), out.status.code());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        serde_json::json!({"ran": false, "title": null, "exit": null, "seconds": null,
                           "id": null, "verdict": "not run", "why": "the config failed"}),
        v["find"]
    );
    let mut problems = v["problems"].as_array().unwrap().clone();
    for p in &problems {
        keys_in(p, "problem");
    }
    // in the order the file's keys came
    problems.sort_by_key(|p| p["level"].as_str().unwrap().to_string());
    assert_eq!(
        vec![
            serde_json::json!({"level": "error", "path": "init.lua", "message": "returned provider as a number, not a function"}),
            serde_json::json!({"level": "warning", "path": "init.lua", "message": "returned an unknown key, x; only provider is read"}),
        ],
        problems
    );
    assert_eq!("error", v["result"]["state"]);
    assert_eq!(
        format!(
            "{NAME}: config: init.lua returned provider as a number, not a function (and 1 more: {NAME} provider check)"
        ),
        v["result"]["message"]
    );
}

/// A pseudo-terminal of a test's own.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    fn open() -> Pty {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: valid out-pointers; no name, termios or size.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(0, rc);
        // SAFETY: openpty made both, and they are ours alone.
        unsafe {
            Pty {
                master: OwnedFd::from_raw_fd(master),
                slave: OwnedFd::from_raw_fd(slave),
            }
        }
    }

    /// A `sleep` whose controlling terminal is this one, as a Rex block's
    /// shell's is its terminal.
    fn leader(&self) -> Child {
        let mut c = Command::new("sleep");
        c.arg("30")
            .stdin(self.slave.try_clone().unwrap())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            c.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        c.spawn().unwrap()
    }

    /// Runs `command` in this terminal: its exit status and everything it
    /// wrote. `leader`, if any, is ended once it has exited.
    fn run(self, mut command: Command, leader: Option<Child>) -> (Option<i32>, String) {
        let mut child = command
            .stdin(self.slave.try_clone().unwrap())
            .stdout(self.slave.try_clone().unwrap())
            .stderr(self.slave)
            .spawn()
            .unwrap();
        // its copy of the terminal goes, or the reader never sees the end
        drop(command);
        let master = self.master;
        let reader = std::thread::spawn(move || {
            let mut master = std::fs::File::from(master);
            let mut all = Vec::new();
            let mut buf = [0u8; 4096];
            while let Ok(n) = master.read(&mut buf) {
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n]);
            }
            String::from_utf8_lossy(&all).into_owned()
        });
        let status = child.wait().unwrap();
        if let Some(mut leader) = leader {
            let _ = leader.kill();
            let _ = leader.wait();
        }
        // the slave's copies are all gone: the reader sees EIO
        (status.code(), reader.join().unwrap())
    }
}

/// `provider check` with `args` in a pseudo-terminal of its own: its exit
/// status and everything written to the terminal.
fn in_pty(home: &Home, args: &[&str]) -> (Option<i32>, String) {
    Pty::open().run(home.command(args), None)
}

/// The pane title a report's find line names.
fn title_in(report: &str) -> String {
    let at = report.find(&format!("title {NAME}-")).expect("no title") + "title ".len();
    report[at..]
        .split([',', ' ', ')'])
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn find_runs_with_the_pane_title_written_then_restored() {
    // find prints the title it was given, and the pane id
    let home = Home::new("find").with_config(&format!(
        r#"return {{ provider = function()
          return {{ name = "t", find = {{ "sh", "-c", 'echo "${upper}_TITLE ${upper}_PANE"' }} }}
        end }}"#,
        upper = NAME.to_ascii_uppercase()
    ));
    let (code, written) = in_pty(&home, &[]);
    assert_eq!(Some(0), code, "{written}");
    let title = title_in(&written);
    let token = title.strip_prefix(&format!("{NAME}-")).unwrap();
    assert_eq!(10, token.len(), "{title}");
    let find = written.lines().find(|l| l.starts_with("find  ")).unwrap();
    assert_eq!(
        format!(
            "find  accepted: id {title} 0-{token} (exit 0, title {title} written, then restored)"
        ),
        strip_time(find.trim_end())
    );
    // pushed and set before the report, then cleared and popped
    let set = format!("\x1b[22;2t\x1b]2;{title}\x1b\\");
    let put_back = "\x1b]2;\x1b\\\x1b[23;2t";
    let at = written.find(&set).expect("no title written");
    assert!(written[at..].contains(put_back), "{written:?}");
    assert!(written.contains("result  ok"), "{written}");
}

#[test]
fn find_refusing_this_terminal_exits_1() {
    let home = Home::new("find-refused").with_config(
        r#"return { provider = function()
          return { name = "t", find = { "sh", "-c", "echo nope >&2; exit 1" } }
        end }"#,
    );
    let (code, written) = in_pty(&home, &[]);
    assert_eq!(Some(1), code, "{written}");
    assert!(
        written.contains(&format!(
            "result  error: {NAME}: t: find didn't confirm this terminal (exit 1: nope); no pane provider"
        )),
        "{written}"
    );
}

/// `line` with the time before its title, `, 4.2 ms` or `, 1.5 s`, taken
/// out.
fn strip_time(line: &str) -> String {
    let Some(at) = line.find(", title") else {
        return line.to_string();
    };
    let start = line[..at].rfind(", ").unwrap();
    format!("{}{}", &line[..start], &line[at..])
}

#[test]
fn a_file_in_another_place_is_found_as_the_daemon_finds_it() {
    let home = Home::new("xdg");
    let xdg = home.dir.join("xdg");
    std::fs::create_dir_all(xdg.join(NAME)).unwrap();
    let file = xdg.join(NAME).join("init.lua");
    std::fs::write(&file, "return { provider = function() return nil end }").unwrap();
    let out = home
        .command(&[])
        .env("XDG_CONFIG_HOME", &xdg)
        .output()
        .unwrap();
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!(format!("config  {}", file.display()), line(&out, "config"));
    assert_eq!("chose  none, by provider()", line(&out, "chose"));
    assert_eq!("result  none", line(&out, "result"));
}

const BLOCK: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

/// A fake Rex server on a Unix socket in `home`, for one connection: it
/// upgrades it and welcomes it, says it is version 9.9.9 at API revision
/// `revision`, and that the Rex block's child is process `child`.
fn fake_rex(home: &Home, revision: i64, child: Option<u32>) -> PathBuf {
    let path = home.dir.join("rex.sock");
    let listener = UnixListener::bind(&path).unwrap();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut byte).unwrap();
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .unwrap();
        let mut sha = sha1_smol::Sha1::new();
        sha.update(key.as_bytes());
        sha.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        use base64::Engine;
        let accept = base64::engine::general_purpose::STANDARD.encode(sha.digest().bytes());
        write!(
            s,
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\
             Sec-WebSocket-Protocol: rex.control.v1\r\n\r\n"
        )
        .unwrap();
        let _hello = read_frame(&mut s);
        let welcome = serde_json::json!({
            "type": "welcome", "protocol": "rex.control.v1",
            "limits": { "max_message_bytes": 1 << 20 }, "api_revision": revision,
        });
        send(&mut s, &welcome);
        while let Some(frame) = read_frame(&mut s) {
            let v: Value = serde_json::from_slice(&frame).unwrap();
            let payload = match v["method"].as_str().unwrap() {
                "server.version" => {
                    serde_json::json!({ "rex_version": "9.9.9", "api_revision": revision })
                }
                "com.superlogical.terminal.process" => {
                    serde_json::json!({ "child": { "pid": child.unwrap() } })
                }
                _ => serde_json::json!({}),
            };
            let answer = serde_json::json!({
                "type": "response", "id": v["id"], "seq": 0, "payload": payload,
            });
            send(&mut s, &answer);
        }
    });
    path
}

/// A client's masked text frame from `s`, None at its end or a close.
fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut head = [0u8; 2];
    s.read_exact(&mut head).ok()?;
    if head[0] & 0x0f == 8 {
        return None;
    }
    let len = match head[1] & 0x7f {
        126 => {
            let mut n = [0u8; 2];
            s.read_exact(&mut n).ok()?;
            u16::from_be_bytes(n) as usize
        }
        127 => {
            let mut n = [0u8; 8];
            s.read_exact(&mut n).ok()?;
            u64::from_be_bytes(n) as usize
        }
        n => n as usize,
    };
    let mut mask = [0u8; 4];
    s.read_exact(&mut mask).ok()?;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body).ok()?;
    Some(
        body.iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect(),
    )
}

/// Sends `v` to the client as a server's text frame.
fn send(s: &mut UnixStream, v: &Value) {
    let text = v.to_string();
    let mut frame = vec![0x81];
    if text.len() < 126 {
        frame.push(text.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(text.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(text.as_bytes());
    let _ = s.write_all(&frame);
}

/// `provider check` with `args`, Rex at `rex` chosen by the default.
fn in_rex(home: &Home, rex: &std::path::Path, args: &[&str]) -> Command {
    let mut c = home.command(args);
    c.env("REX_SERVER", format!("unix://{}", rex.display()))
        .env("REX_SESSION", "session:s")
        .env("REX_BLOCK", BLOCK);
    c
}

#[test]
fn rexs_section_names_the_server_its_version_and_api_revision() {
    let home = Home::new("rex");
    let rex = fake_rex(&home, 12, None);
    let out = in_rex(&home, &rex, &[]).output().unwrap();
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!(
        format!("chose  \"rex\", built in, by {NAME}.default"),
        line(&out, "chose")
    );
    assert!(
        stdout(&out).contains(&format!(
            "\nrex  unix://{}\n  version  9.9.9\n  api revision  12\nfind  not run: ",
            rex.display()
        )),
        "{}",
        stdout(&out)
    );
    assert_eq!("result  ok (find not run)", line(&out, "result"));
}

#[test]
fn rex_as_json() {
    let home = Home::new("rex-json");
    // older than any this build speaks
    let rex = fake_rex(&home, 0, None);
    let out = in_rex(&home, &rex, &["--json"]).output().unwrap();
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    keys_in(&v["rex"], "rex");
    assert_eq!(format!("unix://{}", rex.display()), v["rex"]["server"]);
    assert_eq!("9.9.9", v["rex"]["version"]);
    assert_eq!(0, v["rex"]["api_revision"]);
    assert_eq!(Value::Null, v["rex"]["find"]);
    let missing = v["rex"]["missing"].as_array().unwrap();
    assert_eq!(1, missing.len(), "{missing:?}");
    assert_eq!(Value::Null, missing[0]["capability"]);
    assert!(missing[0]["needs"].as_i64().unwrap() > 0);
    assert_eq!("builtin", v["chose"]["kind"]);
    assert_eq!("default", v["chose"]["how"]);
    assert_eq!(
        serde_json::json!(["open_beside", "open_float", "open_tab_page"]),
        v["one_step"]
    );
}

#[test]
fn an_old_rex_server_misses_every_capability() {
    let home = Home::new("rex-old");
    let rex = fake_rex(&home, 0, None);
    let out = in_rex(&home, &rex, &[]).output().unwrap();
    let missing = stdout(&out)
        .lines()
        .find(|l| l.starts_with("  missing  "))
        .unwrap_or_else(|| panic!("{}", stdout(&out)));
    assert!(
        missing.starts_with("  missing  every capability (needs Rex API revision "),
        "{missing}"
    );
    assert!(missing.ends_with(", this server has 0)"), "{missing}");
}

#[test]
fn rexs_find_proves_the_rex_block_whose_terminal_is_this_one() {
    let home = Home::new("rex-proven");
    let pty = Pty::open();
    let leader = pty.leader();
    let rex = fake_rex(&home, 12, Some(leader.id()));
    let (code, written) = pty.run(in_rex(&home, &rex, &[]), Some(leader));
    assert_eq!(Some(0), code, "{written}");
    assert!(
        written.contains(&format!("find  proven: {BLOCK} is this terminal's")),
        "{written}"
    );
    assert!(written.contains("result  ok\r\n"), "{written}");
    // Rex panes never get a title
    assert!(!written.contains("\x1b]2;"), "{written:?}");
}

#[test]
fn rexs_find_disproves_another_terminals_rex_block() {
    let home = Home::new("rex-disproved");
    let other = Pty::open();
    let leader = other.leader();
    let rex = fake_rex(&home, 12, Some(leader.id()));
    let (code, written) = Pty::open().run(in_rex(&home, &rex, &[]), None);
    let mut leader = leader;
    let _ = leader.kill();
    let _ = leader.wait();
    drop(other);
    // with no config, a guess refused is none, and no error (SPI-1135)
    assert_eq!(Some(0), code, "{written}");
    let find = written
        .lines()
        .find(|l| l.starts_with("find  "))
        .unwrap()
        .trim_end();
    assert!(
        find.starts_with("find  disproved: REX_BLOCK names another terminal (/dev/"),
        "{find}"
    );
    // the message a config's choice would warn with names no terminal
    assert!(
        written.contains(&format!(
            "result  none (find refused): {NAME}: rex: find didn't confirm this terminal \
             (REX_BLOCK names another terminal); no pane provider; with no config, the daemon \
             logs this and warns of nothing, {NAME}.default having guessed\r\n"
        )),
        "{written}"
    );
}

/// `provider check` with `args` in a pane of a tmux server whose `TMUX` is
/// `tmux`, its `PATH` being `path`.
fn in_tmux(home: &Home, tmux: &str, path: &std::ffi::OsStr, args: &[&str]) -> Output {
    home.command(args)
        .env("TMUX", tmux)
        .env("TERM_PROGRAM", "tmux")
        .env("PATH", path)
        .output()
        .unwrap()
}

/// The lines of `out`'s tmux section.
fn tmux_section(out: &Output) -> Vec<&str> {
    let mut lines = stdout(out).lines().skip_while(|l| !l.starts_with("tmux  "));
    let head = lines
        .next()
        .unwrap_or_else(|| panic!("no tmux section:\n{}", stdout(out)));
    std::iter::once(head)
        .chain(lines.take_while(|l| l.starts_with("  ")))
        .collect()
}

#[test]
fn the_default_tmux_tables_report_has_a_tmux_section_naming_tmux_its_server_and_version() {
    if !support::have_tmux() {
        return;
    }
    let home = Home::new("tmux");
    let tmux = support::TmuxServer::start(&home.dir, &[], &[]);
    let path = std::env::var_os("PATH").unwrap();
    let out = in_tmux(&home, &tmux.tmux_var(), &path, &[]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let which = Command::new("sh")
        .args(["-c", "command -v tmux"])
        .output()
        .unwrap();
    let version = Command::new("tmux").arg("-V").output().unwrap();
    let version = text(&version.stdout)
        .trim()
        .trim_start_matches("tmux ")
        .to_string();
    let section = tmux_section(&out);
    assert_eq!(
        section[..4],
        [
            format!("tmux  {}", text(&which.stdout).trim()),
            format!("  version  {version}"),
            format!("  socket  {}", tmux.socket.display()),
            "  server  answers".to_string(),
        ]
    );

    let out = in_tmux(&home, &tmux.tmux_var(), &path, &["--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    keys_in(&v["tmux"], "tmux");
    assert_eq!(v["tmux"]["version"], version.as_str());
    assert_eq!(v["tmux"]["socket"], tmux.socket.to_str().unwrap());
    assert_eq!(v["tmux"]["answers"], true);
    assert_eq!(v["tmux"]["error"], Value::Null);
    drop(tmux);
}

/// A `tmux` in `dir` that says it's `version`, and answers whatever it's
/// asked of a server with nothing.
fn fake_tmux(dir: &std::path::Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    let fake = dir.join("tmux");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\n[ \"$1\" = -V ] && echo 'tmux {version}'\nexit 0\n"),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_tmux_too_old_for_a_capability_in_use_says_it_exits_69() {
    let home = Home::new("tmux-3.4");
    let bin = home.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_tmux(&bin, "3.4");
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &[]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    assert_eq!(
        tmux_section(&out)[1..],
        [
            "  version  3.4",
            "  socket  /tmp/nowhere",
            "  server  answers",
            "  open_float  exits 69: needs tmux 3.7, this is 3.4",
            "  events  exits 69: needs tmux 3.8, this is 3.4",
        ]
    );
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &["--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["tmux"]["missing"],
        serde_json::json!([
            { "capability": "open_float", "needs": "3.7" },
            { "capability": "events", "needs": "3.8" },
        ])
    );

    // below the floor, every capability
    fake_tmux(&bin, "3.1c");
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &[]);
    assert!(
        tmux_section(&out).contains(&"  missing  every capability (needs tmux 3.2, this is 3.1c)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn a_changed_copy_of_the_tmux_table_gets_the_section_for_what_it_still_runs() {
    let home = Home::new("tmux-copy").with_config(
        r#"return { provider = function()
          local t = {name}.builtin.tmux()
          t.name = "mine"
          t.events = nil
          t.open_beside = { "my-split", "{attach}" }
          return t
        end }"#,
    );
    let bin = home.dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    fake_tmux(&bin, "3.4");
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &[]);
    assert_eq!(Some(0), out.status.code(), "{}", stdout(&out));
    let section = tmux_section(&out);
    assert!(
        section.contains(&"  open_float  exits 69: needs tmux 3.7, this is 3.4"),
        "{section:?}"
    );
    assert!(
        !section.iter().any(|l| l.starts_with("  events")),
        "{section:?}"
    );

    // a table that runs no built-in has none
    let home = Home::new("tmux-none").with_config(
        r#"return { provider = function() return { name = "x", open = { "o" } } end }"#,
    );
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &["--json"]);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(Value::Null, v["tmux"], "{v}");
    let out = in_tmux(&home, "/tmp/nowhere,1,0", bin.as_os_str(), &[]);
    assert!(!stdout(&out).contains("\ntmux  "), "{}", stdout(&out));
}
