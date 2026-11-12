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
