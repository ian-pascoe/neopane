//! The user's config choosing the pane provider, end to end (#129,
//! ADR 0007): a real daemon in a pty, its config through `$<NAME>_CONFIG`,
//! and fake capability scripts, whose `open` attaches its terminal with
//! the relay test tool.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neopane::name::NAME;

mod support;
use support::*;

/// A daemon named `work` started in a pty under a fresh `tmp`, its config
/// `init.lua` there. Ends it as it drops.
struct Work {
    tmp: PathBuf,
    sock: PathBuf,
    config: PathBuf,
    ending: Ending,
}

impl Work {
    /// `init_lua` as the config, with `{dir}` standing for the test's
    /// directory and `{exe}` for the binary.
    fn new(test: &str, init_lua: &str) -> Work {
        let tmp = scratch(test);
        let config = tmp.join("init.lua");
        let source = init_lua
            .replace("{dir}", tmp.to_str().unwrap())
            .replace("{exe}", exe().to_str().unwrap());
        std::fs::write(&config, source).unwrap();
        let sock = base_of(&tmp).join("work/nvim.sock");
        Work {
            ending: Ending {
                sock: sock.clone(),
                helpers: Vec::new(),
                tmp: tmp.clone(),
            },
            tmp,
            sock,
            config,
        }
    }

    /// Runs `<name> --name work` in a pty of its own: the first starts the
    /// daemon, the rest attach to it. Wide enough for a warning to show
    /// without a hit-enter prompt, which would hold Neovim's answers.
    fn attach(&mut self) {
        let var = format!("{}_CONFIG", NAME.to_ascii_uppercase());
        let helper = in_pty_with(
            &self.tmp,
            &self.tmp,
            &["--name", "work"],
            &[(&var, self.config.as_os_str())],
            160,
        );
        self.ending.helpers.push(helper);
    }

    /// Waits for its Neovim to answer.
    fn until_up(&self) {
        until_expr(&self.sock, "1", |_| true).expect("work's Neovim never answered");
    }

    /// What `status --json` says of it.
    fn status(&self) -> serde_json::Value {
        json(&under(&self.tmp, &["status", "--json", "--name", "work"]))
    }

    /// Waits up to 20 s for `status` to make `want` true, and returns what it
    /// last said.
    fn until_status(&self, want: impl Fn(&serde_json::Value) -> bool) -> serde_json::Value {
        let due = Instant::now() + Duration::from_secs(20);
        loop {
            let s = self.status();
            if want(&s) || Instant::now() > due {
                return s;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits up to 10 s for Neovim's messages to hold `message`, and returns
    /// them as they last were.
    fn until_message(&self, message: &str) -> String {
        let due = Instant::now() + Duration::from_secs(10);
        loop {
            let messages = remote_expr(&self.sock, "execute('messages')").unwrap_or_default();
            if messages.contains(message) || Instant::now() > due {
                return messages;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The `list` row's PROVIDER column.
    fn listed_provider(&self) -> String {
        let out = under(&self.tmp, &["list"]);
        let shown = text(&out.stdout);
        let row = shown
            .lines()
            .find(|l| l.starts_with("work "))
            .unwrap_or_else(|| panic!("no row for work: {shown}"));
        // NAME, PANES, UNSAVED, then PROVIDER, which may hold spaces, then
        // DIRECTORY
        let header = shown.lines().next().unwrap();
        let from = header.find("PROVIDER").unwrap();
        let to = header.find("DIRECTORY").unwrap();
        row[from..to].trim().to_string()
    }

    /// The daemon's log, for a failure to show.
    fn log(&self) -> String {
        std::fs::read_to_string(base_of(&self.tmp).join("work/log")).unwrap_or_default()
    }

    fn path(&self, file: &str) -> PathBuf {
        self.tmp.join(file)
    }
}

/// The lines of `file`, none if it isn't there.
fn lines_of(file: &Path) -> Vec<String> {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

/// Waits up to 20 s for `file` to have `n` lines.
fn until_lines(file: &Path, n: usize) -> Vec<String> {
    let due = Instant::now() + Duration::from_secs(20);
    loop {
        let lines = lines_of(file);
        if lines.len() >= n || Instant::now() > due {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A fake pane provider's `open`: attaches the terminal with the relay test
/// tool, in the background, and notes its pid in `{dir}/relays`; it prints
/// no terminal id.
const OPEN_BY_RELAY: &str = r#"return {
  provider = function(attach)
    return {
      name = "fake",
      open = { "sh", "-c",
        '"$0" debug-relay "$0" "$@" </dev/null >/dev/null 2>&1 & echo $! >> {dir}/relays',
        "{attach}" },
    }
  end,
}
"#;

#[test]
fn a_config_table_opens_a_terminal_per_window_whose_hang_up_closes_it() {
    if !have_nvim() {
        return;
    }
    let mut work = Work::new("open", OPEN_BY_RELAY);
    work.attach();
    work.until_up();
    let s = work.until_status(|s| s["provider"] == "fake");
    assert_eq!(s["provider"], "fake");
    assert_eq!(s["capabilities"], serde_json::json!(["open"]));
    assert_eq!(s["panes"], 1);
    let wins = remote_expr(&work.sock, "len(nvim_list_wins())").unwrap();

    // :vsplit gets a terminal of its own, attached through the relay
    remote_expr(&work.sock, "execute('vsplit')").unwrap();
    let relays = until_lines(&work.path("relays"), 1);
    assert_eq!(relays.len(), 1, "open never ran: {}", work.log());
    let s = work.until_status(|s| s["panes"] == 2);
    assert_eq!(s["panes"], 2, "the window's terminal never attached");

    // its terminal hangs up: its window closes
    let pid: i32 = relays[0].trim().parse().unwrap();
    // SAFETY: a plain syscall, to the relay tool this test's config started.
    unsafe { libc::kill(pid, libc::SIGKILL) };
    let s = work.until_status(|s| s["panes"] == 1);
    assert_eq!(s["panes"], 1);
    let left = until_expr(&work.sock, "len(nvim_list_wins())", |n| n == wins);
    assert_eq!(left, Some(wins));
}

#[test]
fn a_config_error_warns_once_and_list_shows_it() {
    if !have_nvim() {
        return;
    }
    let mut work = Work::new(
        "error",
        "return { provider = function(attach) return \"tmux\" end }\n",
    );
    work.attach();
    work.until_up();
    let s = work.until_status(|s| s["provider"] != "(choosing)");
    assert_eq!(s["provider"], "none (config error)");
    assert_eq!(work.listed_provider(), "none (config error)");
    let warning =
        format!("{NAME}: config: provider() returned \"tmux\"; the only built-in name is \"rex\"");
    let messages = work.until_message(&warning);
    assert_eq!(messages.matches(&warning).count(), 1, "{messages}");
}

#[test]
fn a_find_that_refuses_the_terminal_means_no_pane_provider() {
    if !have_nvim() {
        return;
    }
    let mut work = Work::new(
        "refused",
        r#"return {
  provider = function(attach)
    return { name = "fake", find = { "sh", "-c", "exit 1" }, focus = { "true" } }
  end,
}
"#,
    );
    work.attach();
    work.until_up();
    let s = work.until_status(|s| s["provider"] != "(choosing)");
    assert_eq!(s["provider"], "none (find refused)");
    let warning =
        format!("{NAME}: fake: find didn't confirm this terminal (exit 1); no pane provider");
    let messages = work.until_message(&warning);
    assert_eq!(messages.matches(&warning).count(), 1, "{messages}");
}

#[test]
fn two_self_attaches_in_quick_succession_choose_once() {
    if !have_nvim() {
        return;
    }
    // each run notes itself, then waits for the test's go: the second
    // attach comes while the first's runs
    let mut work = Work::new(
        "twice",
        r#"local f = io.open("{dir}/runs", "a")
f:write("ran\n")
f:close()
while not io.open("{dir}/go") do os.execute("sleep 0.02") end
return {
  provider = function(attach)
    return { name = "fake", find = { "true" } }
  end,
}
"#,
    );
    work.attach();
    assert_eq!(until_lines(&work.path("runs"), 1), ["ran"]);
    // from here to `go` must fit in the config's 3 s: on a machine too
    // loaded for that, the config times out and the test fails, rather
    // than passing without the two attaches overlapping
    work.attach();
    // both attached, the first's choice still running
    let s = work.until_status(|s| s["panes"] == 2);
    assert_eq!(
        (&s["provider"], &s["panes"]),
        (&"(choosing)".into(), &2.into())
    );
    std::fs::write(work.path("go"), "").unwrap();
    let s = work.until_status(|s| s["provider"] == "fake");
    assert_eq!((&s["provider"], &s["panes"]), (&"fake".into(), &2.into()));
    assert_eq!(lines_of(&work.path("runs")), ["ran"]);
}
