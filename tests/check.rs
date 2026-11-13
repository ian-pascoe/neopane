//! `provider check` through the built binary (#93 decision 30, #94
//! decision 33): the config found and run as the daemon would, the choice's
//! report, and `find` run as a self-attach here would.

mod support;

use std::collections::BTreeSet;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

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
            .env_remove("REX_BLOCK")
            .env_remove("TERM_PROGRAM")
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
fn a_named_file_that_isnt_there_is_an_error() {
    let home = Home::new("missing");
    let out = home.check(&["--config", "gone.lua"]);
    assert_eq!(Some(1), out.status.code(), "{}", stdout(&out));
    let gone = home
        .dir
        .join("work")
        .canonicalize()
        .unwrap()
        .join("gone.lua");
    assert_eq!(
        format!(
            "result  error: {NAME}: config: {} isn't there ({}_CONFIG names it); no pane provider",
            gone.display(),
            NAME.to_ascii_uppercase()
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
         \x20 attach timeout  5 s (default)\n",
        "layout  aero at provider().layout, protocol 1\n\
         \x20 find  sh -c 'echo found; exit 0'  (3 s)\n\
         \x20 place_beside  absent: a terminal it opened stays where it opened\n",
        "\n  focus  aero focus {terminal}  (1.5 s)\n",
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
        "\ntable  mux at provider(), protocol 1\n",
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
            "config", "attach", "ran", "chose", "tables", "one_step", "find", "rex", "problems",
            "result",
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
        "problem" => &["level", "path", "message"],
        "result" => &["state", "message"],
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
    assert_eq!(serde_json::json!([]), v["problems"]);
    assert_eq!("ok", v["result"]["state"]);

    let home = Home::new("json-error").with_config("return { provider = 1, x = 2 }");
    let out = home.check(&["--json"]);
    assert_eq!(Some(1), out.status.code());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
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

/// `provider check` with `args` in a pseudo-terminal of its own: its exit
/// status and everything written to the terminal.
fn in_pty(home: &Home, args: &[&str]) -> (Option<i32>, String) {
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
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut child = home
        .command(args)
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave)
        .spawn()
        .unwrap();
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
    // the slave's copies are gone with the child: the reader sees EIO
    (status.code(), reader.join().unwrap())
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
