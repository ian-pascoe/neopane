//! The command runner against small `sh` scripts (#92 decision 33: fake
//! scripts test only the runner).

use super::*;
use crate::attach::PaneId;
use crate::daemon::provider::Direction;
use crate::name;

/// A runner with one command per `(capability, argv)`, each given
/// `timeout`, for an attach with `env` in `cwd`.
fn runner(
    commands: &[(Capability, &[&str])],
    timeout: Duration,
    env: &[(&str, &str)],
    cwd: &str,
) -> CommandProvider {
    let spec = Spec {
        name: "test".into(),
        commands: commands
            .iter()
            .map(|(cap, argv)| CapCommand {
                cap: *cap,
                argv: argv.iter().map(|w| w.as_bytes().to_vec()).collect(),
                timeout,
                protocol: 1,
            })
            .collect(),
        attach_timeout: config::check::ATTACH_TIMEOUT,
    };
    let env: Vec<(Vec<u8>, Vec<u8>)> = env
        .iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
        .collect();
    CommandProvider::new(spec, &env, cwd.as_bytes()).unwrap()
}

/// A runner whose `open` runs `script` under `sh -c`, its other words after.
fn opening(script: &str, words: &[&str]) -> CommandProvider {
    let mut argv = vec!["sh", "-c", script, "sh"];
    argv.extend(words);
    runner(
        &[(Capability::Open, &argv)],
        Duration::from_secs(5),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    )
}

fn open(attach: &[&str]) -> Ask {
    Ask::Open {
        pane: PaneId::parse("7-abcdefghij").unwrap(),
        attach: attach.iter().map(|w| w.to_string()).collect(),
        title: "np-abcdefghij".into(),
        cols: 80,
        rows: 24,
    }
}

fn close() -> Ask {
    Ask::Close {
        terminal: TerminalId::from("t1"),
    }
}

/// Waits up to `within` for `p` to answer, handing it its loop's readiness
/// as the daemon would.
fn answer(p: &mut CommandProvider, within: Duration) -> Option<(AskId, Outcome)> {
    let deadline = Instant::now() + within;
    loop {
        if let Some(ProviderEvent::Answered { id, outcome }) = p.next() {
            return Some((id, outcome));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut fds = [libc::pollfd {
            fd: p.fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let ms = left.as_millis().clamp(1, 100) as libc::c_int;
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
        p.on_ready(RUN_TOKEN);
    }
}

/// What `p` answers `ask` with, within 5 s.
fn outcome(p: &mut CommandProvider, ask: Ask) -> Outcome {
    p.ask(AskId(1), ask);
    let (id, outcome) = answer(p, Duration::from_secs(5)).expect("an answer");
    assert_eq!(AskId(1), id);
    outcome
}

fn terminal(id: &str) -> Outcome {
    Outcome::Done(Answer::Terminal(TerminalId::from(id)))
}

/// A script that prints its arguments, each in brackets, as its id.
const ARGS: &str = r#"printf '[%s]' "$@"; echo"#;

#[test]
fn placeholders_replace_whole_words_only() {
    let mut p = opening(
        ARGS,
        &[
            "{attach}",
            "{{title}}",
            "{title}",
            "--t={title}",
            "#{pane_id}",
            "{cols}",
            "{pane}",
        ],
    );
    assert_eq!(
        terminal(
            "[np][attach][a b][{title}][np-abcdefghij][--t={title}][#{pane_id}][80][7-abcdefghij]"
        ),
        outcome(&mut p, open(&["np", "attach", "a b"]))
    );
}

#[test]
fn a_value_the_act_doesnt_have_is_one_empty_word() {
    let argv = ["sh", "-c", ARGS, "sh", "{area_bounds}", "{x}"];
    let mut p = runner(
        &[(Capability::OpenFloat, &argv)],
        Duration::from_secs(5),
        &[],
        "/",
    );
    let ask = Ask::OpenFloat {
        pane: PaneId::parse("7-abcdefghij").unwrap(),
        attach: vec![],
        title: String::new(),
        origin: TerminalId::from("t0"),
        x: -3,
        y: 0,
        cols: 10,
        rows: 5,
        origin_cols: 80,
        origin_rows: 24,
        area_bounds: None,
    };
    assert_eq!(terminal("[][-3]"), outcome(&mut p, ask));
}

#[test]
fn the_attach_variable_is_quoted_for_sh() {
    let up = name::upper();
    let script = format!(r#"eval "set -- ${up}_ATTACH"; {ARGS}"#);
    let mut p = opening(&script, &[]);
    let words = ["/bin/np", "attach", "it's", "", "a b", "$HOME", "x=1,y"];
    assert_eq!(
        terminal("[/bin/np][attach][it's][][a b][$HOME][x=1,y]"),
        outcome(&mut p, open(&words))
    );
}

#[test]
fn the_environment_is_the_attachs_less_the_name_and_the_per_act_variables() {
    let up = name::upper();
    let script = format!(
        "echo ${{{up}-unset}},${{{up}_PANE_X-unset}},${{{up}_ENV_TMUX_PANE-unset}},\
         ${{{up}_CONFIG-unset}},${{{up}_TITLE}},${{{up}_CAPABILITY}},${{{up}_PROTOCOL}},$HOME"
    );
    let argv = ["sh", "-c", script.as_str()];
    let (name_var, stale, env_var, config) = (
        up.clone(),
        format!("{up}_TITLE"),
        format!("{up}_ENV_TMUX_PANE"),
        format!("{up}_CONFIG"),
    );
    let pane_x = format!("{up}_PANE_X");
    let mut p = runner(
        &[(Capability::Open, &argv)],
        Duration::from_secs(5),
        &[
            ("HOME", "/home/u"),
            (&name_var, "1"),
            (&stale, "stale"),
            (&env_var, "%3"),
            (&config, "/cfg/init.lua"),
            (&pane_x, "kept"),
        ],
        "/",
    );
    assert_eq!(
        terminal("unset,kept,unset,/cfg/init.lua,np-abcdefghij,open,1,/home/u"),
        outcome(&mut p, open(&[]))
    );
}

#[test]
fn it_runs_in_the_attachs_directory_or_else_in_the_root() {
    let argv = ["sh", "-c", "pwd -P"];
    for (cwd, shown) in [("/usr", "/usr"), ("/no/such/dir", "/"), ("", "/")] {
        let mut p = runner(
            &[(Capability::Open, &argv)],
            Duration::from_secs(5),
            &[],
            cwd,
        );
        assert_eq!(terminal(shown), outcome(&mut p, open(&[])), "{cwd}");
    }
}

#[test]
fn exit_69_or_no_program_is_unsupported_and_any_other_failure_fails() {
    let mut p = runner(
        &[(Capability::Close, &["sh", "-c", "exit 69"])],
        Duration::from_secs(5),
        &[],
        "/",
    );
    assert_eq!(Outcome::Unsupported, outcome(&mut p, close()));

    let mut p = runner(
        &[(
            Capability::Close,
            &["sh", "-c", "echo 'no such pane' >&2; echo more >&2; exit 1"],
        )],
        Duration::from_secs(5),
        &[],
        "/",
    );
    assert_eq!(
        Outcome::Failed("exit 1: no such pane".into()),
        outcome(&mut p, close())
    );

    let mut p = runner(
        &[(Capability::Close, &["sh", "-c", "kill -9 $$"])],
        Duration::from_secs(5),
        &[],
        "/",
    );
    assert_eq!(
        Outcome::Failed("killed by signal 9".into()),
        outcome(&mut p, close())
    );

    let mut p = runner(
        &[(Capability::Close, &["/no/such/program"])],
        Duration::from_secs(5),
        &[],
        "/",
    );
    p.ask(AskId(1), close());
    // never inline: only once its loop says so
    assert_eq!(None, p.next());
    assert_eq!(
        Some((AskId(1), Outcome::Unsupported)),
        answer(&mut p, Duration::from_secs(5))
    );
}

#[test]
fn an_answer_known_at_once_isnt_held_up_by_a_slow_command_asked_after() {
    let mut p = runner(
        &[(Capability::Close, &["sleep", "3"])],
        Duration::from_secs(5),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    );
    let focus = Ask::Focus {
        terminal: TerminalId::from("t1"),
    };
    p.ask(AskId(1), focus);
    p.ask(AskId(2), close());
    assert_eq!(
        Some((AskId(1), Outcome::Unsupported)),
        answer(&mut p, Duration::from_millis(500))
    );
}

#[test]
fn a_command_other_than_an_open_is_done_with_nothing_whatever_it_prints() {
    let mut p = runner(
        &[(Capability::Focus, &["sh", "-c", "echo hi"])],
        Duration::from_secs(5),
        &[],
        "/",
    );
    let ask = Ask::Focus {
        terminal: TerminalId::from("t1"),
    };
    assert_eq!(Outcome::Done(Answer::Nothing), outcome(&mut p, ask));
}

#[test]
fn an_opens_id_is_its_first_line_trimmed_at_most_4_kib() {
    let mut p = opening("printf '  %%3 \\nsecond\\n'", &[]);
    assert_eq!(terminal("%3"), outcome(&mut p, open(&[])));

    let mut p = opening("true", &[]);
    assert_eq!(Outcome::Done(Answer::Nothing), outcome(&mut p, open(&[])));

    let mut p = opening("head -c 4096 /dev/zero | tr '\\0' x; echo", &[]);
    assert_eq!(terminal(&"x".repeat(4096)), outcome(&mut p, open(&[])));

    let mut p = opening("head -c 4097 /dev/zero | tr '\\0' x; echo", &[]);
    assert_eq!(
        Outcome::Failed("its terminal id is over 4 KiB".into()),
        outcome(&mut p, open(&[]))
    );

    // a first line too long even to keep, with no end
    let mut p = opening("head -c 200000 /dev/zero | tr '\\0' x", &[]);
    assert_eq!(
        Outcome::Failed("its terminal id is over 4 KiB".into()),
        outcome(&mut p, open(&[]))
    );
}

/// Whether process `pid` is still there.
fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only checks.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[test]
fn a_command_past_its_time_is_ended_with_its_group_and_answers_nothing() {
    let dir = std::env::temp_dir().join(format!("{}-timeout-{}", name::NAME, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let pid_file = dir.join("pid");
    // a grandchild that ignores SIGTERM: only the SIGKILL a second later ends it
    let script = format!(
        "(trap '' TERM; exec sleep 30) & echo $! > {}; wait",
        pid_file.display()
    );
    let argv = ["sh", "-c", script.as_str()];
    let mut p = runner(
        &[(Capability::Close, &argv)],
        Duration::from_millis(300),
        &[],
        "/",
    );
    let started = Instant::now();
    p.ask(AskId(1), close());
    let grandchild = loop {
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<libc::pid_t>().ok())
        {
            break pid;
        }
        assert!(started.elapsed() < Duration::from_secs(5), "no grandchild");
        std::thread::sleep(Duration::from_millis(10));
    };
    // past its time the leader goes with SIGTERM; it answers nothing, as
    // the act has timed out
    assert_eq!(None, answer(&mut p, Duration::from_millis(900)));
    assert!(alive(grandchild), "the grandchild ignores SIGTERM");
    let _ = answer(&mut p, Duration::from_millis(1500));
    let gone = Instant::now();
    while alive(grandchild) {
        // an orphan that died is reaped by init soon after
        assert!(
            gone.elapsed() < Duration::from_secs(3),
            "the grandchild lives"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(started.elapsed() >= Duration::from_millis(1300));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_command_past_its_time_says_the_layout_changed_once_it_ends() {
    let mut p = runner(
        &[(Capability::Close, &["sleep", "30"])],
        Duration::from_millis(200),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    );
    p.ask(AskId(1), close());
    // SIGTERM at 200 ms ends it; it answers nothing
    let told = told(&mut p, |t| !t.is_empty());
    assert_eq!(vec![ProviderEvent::LayoutChanged], told);
    assert!(p.running.is_empty());
}

#[test]
fn its_capabilities_are_its_commands_and_its_timeouts_theirs() {
    let p = runner(
        &[
            (Capability::Close, &["true"]),
            (Capability::FocusDirection, &["true"]),
        ],
        Duration::from_millis(1500),
        &[],
        "/",
    );
    let caps = p.capabilities();
    assert!(caps.has(Capability::Close) && caps.has(Capability::FocusDirection));
    assert!(!caps.has(Capability::Open));
    assert_eq!(Duration::from_millis(1500), p.timeout(Capability::Close));
    assert_eq!(Duration::from_secs(5), p.timeout(Capability::Open));
}

#[test]
fn a_split_place_gets_its_direction_and_ratio() {
    let argv = [
        "sh",
        "-c",
        "echo $0 $1 $2 >&2; exit 3",
        "{direction}",
        "{ratio}",
        "{size}",
    ];
    let mut p = runner(
        &[(Capability::PlaceBeside, &argv)],
        Duration::from_secs(5),
        &[],
        "/",
    );
    let ask = Ask::PlaceBeside {
        terminal: TerminalId::from("t2"),
        origin: TerminalId::from("t1"),
        dir: Direction::Left,
        size: 30,
        ratio: 1.0 / 3.0,
    };
    assert_eq!(
        Outcome::Failed("exit 3: left 0.3333 30".into()),
        outcome(&mut p, ask)
    );
}

/// A runner whose `report_layout` runs `script` under `sh -c`.
fn reporting(script: &str) -> CommandProvider {
    runner(
        &[(Capability::ReportLayout, &["sh", "-c", script])],
        Duration::from_secs(5),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    )
}

#[test]
fn a_report_is_read_into_a_live_layout() {
    let mut p = reporting(
        r#"echo '{"areas":[{"id":"w1","terminals":[{"terminal":"%1","rect":[0,0,1,1]}]}],'
           echo '"focus":"%1"}'"#,
    );
    assert!(p.capabilities().has(Capability::ReportLayout));
    let Outcome::Done(Answer::Layout(l)) = outcome(&mut p, Ask::ReportLayout) else {
        panic!("no layout");
    };
    assert_eq!(Some(TerminalId::from("%1")), l.current);
    assert_eq!(
        Some([0.0, 0.0, 1.0, 1.0]),
        l.area_of(&TerminalId::from("%1")).and_then(|a| a.rect())
    );
}

#[test]
fn a_malformed_or_too_long_report_fails_and_exit_69_is_unsupported() {
    let mut p = reporting("echo '{\"areas\":'");
    let Outcome::Failed(why) = outcome(&mut p, Ask::ReportLayout) else {
        panic!("not failed");
    };
    assert!(why.contains("isn't JSON"), "{why}");

    // valid JSON, but past 1 MiB
    let mut p = reporting(
        r#"printf '{"areas":[],"pad":"'; head -c 1048576 /dev/zero | tr '\0' x; echo '"}'"#,
    );
    assert_eq!(
        Outcome::Failed("its report is over 1024 KiB".into()),
        outcome(&mut p, Ask::ReportLayout)
    );

    let mut p = reporting("exit 69");
    assert_eq!(Outcome::Unsupported, outcome(&mut p, Ask::ReportLayout));
}

/// A runner whose `events` runs `script` under `sh -c`.
fn telling(script: &str) -> CommandProvider {
    runner(
        &[(Capability::Events, &["sh", "-c", script])],
        Duration::from_secs(5),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    )
}

/// A directory of a test's own, named for this process and thread, removed
/// when dropped.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "{}-{tag}-{}-{}",
            name::NAME,
            std::process::id(),
            crate::util::thread_number()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    fn join(&self, file: &str) -> std::path::PathBuf {
        self.0.join(file)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What `p` tells, other than answers, until `enough` says it's enough,
/// handing it its loop's readiness as the daemon would; at most 5 s.
fn told(p: &mut CommandProvider, enough: impl Fn(&[ProviderEvent]) -> bool) -> Vec<ProviderEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut told = Vec::new();
    loop {
        while let Some(e) = p.next() {
            told.push(e);
        }
        if enough(&told) {
            return told;
        }
        assert!(Instant::now() < deadline, "only told {told:?}");
        wait_ready(p, Duration::from_millis(100));
    }
}

/// Waits up to `most` for `p`'s loop to be ready, then hands it on.
fn wait_ready(p: &mut CommandProvider, most: Duration) {
    let mut fds = [libc::pollfd {
        fd: p.fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, most.as_millis() as libc::c_int) };
    p.on_ready(RUN_TOKEN);
}

fn focused(t: &str) -> ProviderEvent {
    ProviderEvent::Focused(TerminalId::from(t))
}

fn changes(told: &[ProviderEvent]) -> usize {
    told.iter()
        .filter(|e| **e == ProviderEvent::LayoutChanged)
        .count()
}

#[test]
fn each_events_line_says_the_layout_changed_after_its_focus_and_closed() {
    let mut p =
        telling(r#"echo '{"focus":"x","closed":"y","other":1}'; echo again; exec sleep 30"#);
    assert!(p.capabilities().has(Capability::Events));
    // once at its start, as it may have missed changes, then for each line
    let told = told(&mut p, |t| changes(t) >= 3);
    assert_eq!(
        vec![
            ProviderEvent::LayoutChanged,
            focused("x"),
            ProviderEvent::Closed(TerminalId::from("y")),
            ProviderEvent::LayoutChanged,
            ProviderEvent::LayoutChanged,
        ],
        told
    );
}

#[test]
fn an_events_line_over_64_kib_is_dropped() {
    let mut p = telling(
        r#"printf '{"focus":"'; head -c 65536 /dev/zero | tr '\0' x; echo '"}'
           echo '{"focus":"z"}'; exec sleep 30"#,
    );
    let told = told(&mut p, |t| t.contains(&focused("z")));
    assert!(
        !told
            .iter()
            .any(|e| matches!(e, ProviderEvent::Focused(t) if t.as_str() != "z")),
        "{told:?}"
    );
    // the start's, and the short line's
    assert_eq!(2, changes(&told));
}

/// A runner whose `events` is `argv`, its time told by the returned cell.
fn events_on_clock(argv: &[&str]) -> (CommandProvider, std::rc::Rc<std::cell::Cell<Instant>>) {
    let now = std::rc::Rc::new(std::cell::Cell::new(Instant::now()));
    let clock = {
        let now = now.clone();
        Box::new(move || now.get())
    };
    let spec = Spec {
        name: "test".into(),
        commands: vec![CapCommand {
            cap: Capability::Events,
            argv: argv.iter().map(|w| w.as_bytes().to_vec()).collect(),
            timeout: Duration::from_secs(2),
            protocol: 1,
        }],
        attach_timeout: config::check::ATTACH_TIMEOUT,
    };
    let env = [(b"PATH".to_vec(), b"/usr/bin:/bin".to_vec())];
    let p = CommandProvider::with_clock(spec, &env, b"/", clock).unwrap();
    (p, now)
}

/// Waits for `p`'s `events` to end, its end seen at `at`.
fn ended_at(p: &mut CommandProvider, now: &std::cell::Cell<Instant>, at: Instant) {
    now.set(at);
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.events_running() {
        assert!(Instant::now() < deadline, "it doesn't end");
        wait_ready(p, Duration::from_millis(100));
    }
}

/// Whether `p`'s `events` runs once its loop is handed on at `t`.
fn running_at(p: &mut CommandProvider, now: &std::cell::Cell<Instant>, t: Instant) -> bool {
    now.set(t);
    p.on_ready(RUN_TOKEN);
    p.events_running()
}

/// How many lines file `f` has.
fn lines_in(f: &std::path::Path) -> usize {
    std::fs::read_to_string(f)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn a_crashing_events_starts_again_after_1_s_then_2_s_and_1_s_after_a_minute_running() {
    let dir = Scratch::new("events");
    let starts = dir.join("starts");
    let script = format!("echo >> {}; exit 1", starts.display());
    let (mut p, now) = events_on_clock(&["sh", "-c", script.as_str()]);
    let t0 = now.get();
    let s = Duration::from_secs(1);
    let ms = Duration::from_millis(1);
    ended_at(&mut p, &now, t0);
    assert_eq!(1, lines_in(&starts));
    assert!(!running_at(&mut p, &now, t0 + s - ms));
    assert!(running_at(&mut p, &now, t0 + s));
    ended_at(&mut p, &now, t0 + s);
    assert_eq!(2, lines_in(&starts));
    assert!(!running_at(&mut p, &now, t0 + 3 * s - ms));
    assert!(running_at(&mut p, &now, t0 + 3 * s));
    // a minute running: the wait is 1 s again
    ended_at(&mut p, &now, t0 + 63 * s);
    assert_eq!(3, lines_in(&starts));
    assert!(!running_at(&mut p, &now, t0 + 64 * s - ms));
    assert!(running_at(&mut p, &now, t0 + 64 * s));
}

#[test]
fn an_events_restart_that_cant_start_waits_longer_and_tries_again() {
    let dir = Scratch::new("events-restart");
    let program = dir.join("events");
    let starts = dir.join("starts");
    let script = format!("#!/bin/sh\necho >> {}\nexit 1\n", starts.display());
    let write = || {
        std::fs::write(&program, &script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    write();
    let (mut p, now) = events_on_clock(&[program.to_str().unwrap()]);
    let t0 = now.get();
    let s = Duration::from_secs(1);
    ended_at(&mut p, &now, t0);
    std::fs::remove_file(&program).unwrap();
    // the restart at 1 s can't start: it waits 2 s more
    assert!(!running_at(&mut p, &now, t0 + s));
    assert!(p.capabilities().has(Capability::Events));
    write();
    assert!(!running_at(
        &mut p,
        &now,
        t0 + 3 * s - Duration::from_millis(1)
    ));
    assert!(running_at(&mut p, &now, t0 + 3 * s));
    ended_at(&mut p, &now, t0 + 3 * s);
    assert_eq!(2, lines_in(&starts));
}

#[test]
fn events_that_cant_start_at_first_or_exit_69_are_unsupported_for_good() {
    let p = runner(
        &[(Capability::Events, &["/no/such/program"])],
        Duration::from_secs(5),
        &[],
        "/",
    );
    assert!(!p.capabilities().has(Capability::Events));
    assert!(p.events_run.is_none());

    let mut p = telling("exit 69");
    let deadline = Instant::now() + Duration::from_secs(5);
    while p.capabilities().has(Capability::Events) {
        assert!(Instant::now() < deadline, "still has events");
        wait_ready(&mut p, Duration::from_millis(100));
    }
    assert!(p.events_run.is_none());
}

#[test]
fn events_dies_with_the_pane_provider() {
    let dir = Scratch::new("events-die");
    let pid_file = dir.join("pid");
    // a grandchild in its group, as well as itself
    let script = format!("sleep 30 & echo $$ $! > {}; wait", pid_file.display());
    let p = telling(&script);
    let started = Instant::now();
    let pids: Vec<libc::pid_t> = loop {
        if let Ok(s) = std::fs::read_to_string(&pid_file)
            && s.ends_with('\n')
        {
            break s.split_whitespace().map(|w| w.parse().unwrap()).collect();
        }
        assert!(started.elapsed() < Duration::from_secs(5), "no pids");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(pids.iter().all(|&pid| alive(pid)));
    drop(p);
    let gone = Instant::now();
    while pids.iter().any(|&pid| alive(pid)) {
        // an orphan that died is reaped by init soon after
        assert!(gone.elapsed() < Duration::from_secs(3), "events lives");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn every_capability_it_runs_and_every_variable_it_sets_is_in_the_docs() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/capabilities.md"),
    )
    .unwrap();
    for cap in RUNS {
        assert!(doc.contains(&format!("| `{}`", cap.name())), "{cap}");
        for p in crate::config::check::Capability::ALL
            .iter()
            .find(|c| c.name() == cap.name())
            .unwrap()
            .placeholders()
        {
            assert!(doc.contains(&format!("`{p}`")), "{cap}: {p}");
        }
    }
    for v in inputs::Var::ALL {
        let shown = v.name().to_ascii_uppercase();
        assert!(doc.contains(&format!("`{shown}`")), "{shown}");
    }
}

#[test]
fn a_command_doesnt_inherit_the_daemons_ignored_signals() {
    // the daemon ignores SIGTERM, as it watches it on its loop
    // SAFETY: plain sigaction calls; the old action is put back below.
    let mut was: libc::sigaction = unsafe { std::mem::zeroed() };
    unsafe {
        let mut ignore: libc::sigaction = std::mem::zeroed();
        ignore.sa_sigaction = libc::SIG_IGN;
        libc::sigaction(libc::SIGTERM, &ignore, &mut was);
    }
    let mut p = runner(
        &[(Capability::Close, &["sh", "-c", "exec sleep 30"])],
        Duration::from_millis(300),
        &[("PATH", "/usr/bin:/bin")],
        "/",
    );
    let started = Instant::now();
    p.ask(AskId(1), close());
    // SAFETY: as above.
    unsafe { libc::sigaction(libc::SIGTERM, &was, std::ptr::null_mut()) };
    // the timeout's SIGTERM ends it, well before the SIGKILL 1 s later
    while !p.running.is_empty() {
        assert!(
            started.elapsed() < Duration::from_millis(1200),
            "SIGTERM didn't end it"
        );
        assert_eq!(None, answer(&mut p, Duration::from_millis(50)));
    }
}

/// A checked table named `name` giving `caps`, each running `name`, with
/// attach timeout `secs`.
fn table(name: &str, caps: &[config::Capability], secs: u64) -> config::Provider {
    config::Provider {
        name: name.into(),
        path: format!("provider().{name}"),
        capabilities: caps
            .iter()
            .map(|&c| {
                let command = config::check::Command {
                    argv: vec![name.as_bytes().to_vec()],
                    timeout: Some(Duration::from_secs(3)),
                };
                (c, command)
            })
            .collect(),
        attach_timeout: Duration::from_secs(secs),
        protocol: 1,
        context_vars: Vec::new(),
        find_vars: Vec::new(),
    }
}

#[test]
fn a_pairs_spec_runs_its_terminal_halfs_open_and_close_with_its_attach_timeout_and_the_rest_from_its_layout_half()
 {
    use config::Capability as C;
    let terminal = table("ghostty", &[C::Open, C::Close], 9);
    let layout = table("aerospace", &[C::PlaceBeside, C::Focus], 2);
    let pair = config::Choice::Pair(config::Pair {
        name: "ghostty+aerospace".into(),
        terminal: Some(terminal.clone()),
        layout: Some(layout.clone()),
    });
    let spec = Spec::of(&pair).unwrap();
    let ran: Vec<(Capability, &[u8])> = spec
        .commands
        .iter()
        .map(|c| (c.cap, &c.argv[0][..]))
        .collect();
    assert_eq!(
        vec![
            (Capability::Open, &b"ghostty"[..]),
            (Capability::Close, b"ghostty"),
            (Capability::PlaceBeside, b"aerospace"),
            (Capability::Focus, b"aerospace"),
        ],
        ran
    );
    assert_eq!(Duration::from_secs(9), spec.attach_timeout);
    let p = CommandProvider::new(spec, &[], b"/").unwrap();
    assert_eq!(Duration::from_secs(9), p.attach_timeout());

    // a layout half alone has the default; a whole table its own
    let alone = config::Choice::Pair(config::Pair {
        name: "aerospace".into(),
        terminal: None,
        layout: Some(layout),
    });
    let default = config::check::ATTACH_TIMEOUT;
    assert_eq!(default, Spec::of(&alone).unwrap().attach_timeout);
    let whole = Spec::of(&config::Choice::Whole(terminal)).unwrap();
    assert_eq!(Duration::from_secs(9), whole.attach_timeout);
    assert_eq!(None, Spec::of(&config::Choice::None));
}
