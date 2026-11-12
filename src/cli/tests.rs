use std::ffi::OsString;

use super::*;

fn args(line: &str) -> Vec<OsString> {
    line.split_whitespace().map(OsString::from).collect()
}

fn parsed(line: &str) -> Result<Command, UsageError> {
    parse(&args(line))
}

/// The bare form's start, failing on anything else.
fn started(line: &str) -> Start {
    match parsed(line) {
        Ok(Command::Start(start)) => start,
        other => panic!("{line}: {other:?}"),
    }
}

/// The message after `<name>: ` that `line` is refused with.
fn refused(line: &str) -> String {
    match parsed(line) {
        Err(UsageError(why)) => why,
        other => panic!("{line}: {other:?}"),
    }
}

fn files(line: &str) -> Vec<OsString> {
    started(line).reading.files
}

#[test]
fn the_bare_form_starts_a_daemon_with_neovims_arguments_as_typed() {
    assert_eq!(started(""), Start::default());
    for line in [
        "some/file.txt",
        "-O a b",
        "-- --name",
        "+3 notes.md",
        "-d a b",
    ] {
        assert_eq!(started(line).nvim_args, args(line), "{line}");
    }
}

#[test]
fn neovims_arguments_are_read_by_neovims_rules() {
    let none: Vec<OsString> = vec![];
    for (line, want) in [
        // u takes the next word
        ("-nu x", none.clone()),
        // c takes the rest of its word
        ("-cq", none.clone()),
        ("-c q f", args("f")),
        // q takes the next word, even an option
        ("-q -n g.txt", args("g.txt")),
        ("-q", none.clone()),
        ("-qerrs.txt f", args("f")),
        // S takes no word that starts with -
        ("-S -n", none.clone()),
        ("-S s.vim f", args("f")),
        ("-S", none.clone()),
        ("-V9log", none.clone()),
        ("-V1 f", args("f")),
        ("-op", none.clone()),
        ("-o2 a b", args("a b")),
        ("-w80 f", args("f")),
        ("-w out f", args("f")),
        ("-t main f", args("f")),
        ("-tmain f", args("f")),
        ("-s script f", args("f")),
        ("-e f", args("f")),
        // after --, every word is a file
        ("-- +3 -x", args("+3 -x")),
        ("--cmd -x f", args("f")),
        ("--CMD -x f", args("f")),
        ("--startuptime t.log f", args("f")),
        ("--clean f", args("f")),
        ("+3 f", args("f")),
        // our options as the values of Neovim's
        ("-c --name f", args("f")),
        ("-u --detach", none.clone()),
    ] {
        assert_eq!(files(line), want, "{line}");
        assert_eq!(started(line).nvim_args, args(line), "{line}");
    }
}

#[test]
fn every_refusal_says_why() {
    let listen = "is the daemon's own: it starts Neovim with --embed --headless --listen";
    let remote = format!(
        "talks to another Neovim; to open a file in a running daemon: {NAME} attach --name N FILE"
    );
    let prints = format!(
        "would make Neovim print and exit: put -h or -v first for {NAME}'s own, or run nvim -h"
    );
    for (line, want) in [
        ("--embed", format!("--embed {listen}")),
        ("--EMBED", format!("--EMBED {listen}")),
        ("--headless", format!("--headless {listen}")),
        ("--listen x", format!("--listen {listen}")),
        ("--listenx foo", format!("--listenx {listen}")),
        ("--server x", format!("--server {remote}")),
        ("--remote-silent f", format!("--remote-silent {remote}")),
        ("f --help", format!("--help {prints}")),
        ("f --version", format!("--version {prints}")),
        ("--api-info", format!("--api-info {prints}")),
        ("-Rh", format!("-Rh {prints}")),
        ("f -?", format!("-? {prints}")),
        ("-n -v", format!("-v {prints}")),
        (
            "-es",
            "-es runs Neovim without a screen: run nvim -es".to_string(),
        ),
        (
            "-Es",
            "-Es runs Neovim without a screen: run nvim -Es".to_string(),
        ),
        (
            "-e -s f",
            "-e -s runs Neovim without a screen: run nvim -e -s".to_string(),
        ),
        (
            "-E -n -s",
            "-E -s runs Neovim without a screen: run nvim -E -s".to_string(),
        ),
        (
            "-Rens",
            "-Rens runs Neovim without a screen: run nvim -Rens".to_string(),
        ),
        (
            "-l x.lua",
            "-l runs Neovim without a screen: run nvim -l".to_string(),
        ),
        (
            "-V3",
            "-V3 without a file would stop Neovim from starting: give it one, as -V3log.txt"
                .to_string(),
        ),
        (
            "-V f",
            "-V without a file would stop Neovim from starting: give it one, as -Vlog.txt"
                .to_string(),
        ),
        (
            "f -",
            "- reads text from stdin, which the daemon's Neovim can't read: save it to a file \
             first"
                .to_string(),
        ),
        (
            "-r",
            "-r with no file lists swap files: run nvim -r".to_string(),
        ),
        (
            "-nL",
            "-nL with no file lists swap files: run nvim -nL".to_string(),
        ),
    ] {
        assert_eq!(refused(line), want, "{line}");
    }
    // recovering a file is fine
    assert_eq!(files("-r f.txt"), args("f.txt"));
    assert_eq!(files("-L -- f.txt"), args("f.txt"));
    // -s in a word of its own takes the next one
    assert_eq!(files("-s -e f"), args("f"));
}

#[test]
fn our_options_go_before_neovims() {
    for (line, option, value) in [
        ("file --name x", "--name", " N"),
        ("file --name=x", "--name", " N"),
        ("-O a --address A", "--address", " A"),
        ("a --provider none", "--provider", " P"),
        ("a --detach", "--detach", ""),
        ("a --no-wait", "--no-wait", ""),
    ] {
        assert_eq!(
            refused(line),
            format!("put {option} before Neovim's arguments: {NAME} {option}{value} [NVIM-ARGS]"),
            "{line}"
        );
    }
    // after --, they are files
    assert_eq!(files("-- --name x"), args("--name x"));
}

#[test]
fn our_options_are_read_from_the_front() {
    let start = started("--name work --provider=none --detach --no-wait -O a b");
    assert_eq!(start.name, Some("work".into()));
    assert_eq!(start.provider, Some("none".into()));
    assert!(start.detach && start.no_wait);
    assert_eq!(start.nvim_args, args("-O a b"));
    let start = started("--address=/tmp/s f");
    assert_eq!(start.address, Some("/tmp/s".into()));
    assert_eq!(start.nvim_args, args("f"));
    // a value may look like an option
    assert_eq!(started("--name --detach").name, Some("--detach".into()));
    // no abbreviations
    let start = started("--provider p f");
    assert_eq!(start.provider, Some("p".into()));
    assert_eq!(start.nvim_args, args("f"));
    let start = started("--nam x");
    assert_eq!(start.name, None);
    assert_eq!(start.nvim_args, args("--nam x"));
}

#[test]
fn our_options_misused_are_usage_errors() {
    assert_eq!(refused("--name"), "--name needs a value");
    assert_eq!(refused("--name a --name b"), "--name is given twice");
    assert_eq!(
        refused("--name a --address b"),
        "--name and --address both choose a daemon: give one"
    );
}

#[test]
fn help_and_version() {
    for line in [
        "-h",
        "--help",
        "help",
        "--name x -h",
        "attach -h",
        "list --help",
        "status --json -h",
        "kill -h",
        "provider -h",
        "provider check --help",
        "version -h",
        "help -h",
    ] {
        assert_eq!(parsed(line), Ok(Command::Help), "{line}");
    }
    for line in [
        "help debug",
        "debug-keys -h",
        "debug-events --help",
        "debug-rex -h",
    ] {
        assert_eq!(parsed(line), Ok(Command::DebugHelp), "{line}");
    }
    for line in ["-v", "--version", "version", "--name x --version"] {
        assert_eq!(parsed(line), Ok(Command::Version), "{line}");
    }
    assert_eq!(refused("version x"), "version takes no arguments");
    assert_eq!(refused("help me"), "help takes only debug");
}

#[test]
fn provider_show_takes_one_name_at_most() {
    assert_eq!(parsed("provider show"), Ok(Command::ProviderShow(None)));
    assert_eq!(
        parsed("provider show rex"),
        Ok(Command::ProviderShow(Some("rex".into())))
    );
    assert_eq!(
        refused("provider show --x"),
        "provider show: unknown option --x"
    );
    for line in ["provider show a b", "provider show rex --x"] {
        assert_eq!(
            refused(line),
            "provider show takes at most one NAME",
            "{line}"
        );
    }
    assert_eq!(parsed("provider show -h"), Ok(Command::Help));
}

#[test]
fn provider_types_takes_a_config_file() {
    let types = |config: Option<&str>| {
        Ok(Command::ProviderTypes {
            config: config.map(OsString::from),
        })
    };
    assert_eq!(parsed("provider types"), types(None));
    assert_eq!(
        parsed("provider types --config c.lua"),
        types(Some("c.lua"))
    );
    assert_eq!(
        parsed("provider types --config=c.lua"),
        types(Some("c.lua"))
    );
    assert_eq!(
        refused("provider types --config a --config b"),
        "--config is given twice"
    );
    assert_eq!(refused("provider types --config"), "--config needs a value");
    assert_eq!(
        refused("provider types --config="),
        "--config needs a value"
    );
    assert_eq!(
        refused("provider types x"),
        "provider types takes only --config FILE"
    );
}

#[test]
fn command_words_are_only_the_first_argument() {
    for (line, what) in [
        ("attach", "attach"),
        ("attach f", "attach"),
        ("list", "list"),
        ("list --json", "list"),
        ("status", "status"),
        ("kill --all", "kill"),
        ("provider check", "provider check"),
    ] {
        assert_eq!(parsed(line), Ok(Command::NotBuilt(what.into())), "{line}");
    }
    assert_eq!(refused("provider"), "provider takes check, show or types");
    assert_eq!(refused("provider x"), "provider takes check, show or types");
    // anywhere else, a file
    assert_eq!(files("f list"), args("f list"));
    assert_eq!(files("./list"), args("./list"));
    // matched byte for byte
    assert_eq!(files("List"), args("List"));
}

#[test]
fn detach_is_held() {
    assert_eq!(
        refused("detach"),
        format!("detach isn't available yet; to open a file named detach, run {NAME} ./detach")
    );
}

#[test]
fn a_command_word_naming_a_file_gets_a_note() {
    let note = |line: &str, exists: bool| {
        let words = args(line);
        file_note(&words, &parse(&words), |_| exists)
    };
    assert_eq!(
        note("list", true),
        Some(format!(
            "{NAME}: list is a command; to open the file, run {NAME} ./list"
        ))
    );
    assert_eq!(
        note("provider", true),
        Some(format!(
            "{NAME}: provider is a command; to open the file, run {NAME} ./provider"
        ))
    );
    assert_eq!(note("list", false), None);
    assert_eq!(note("f", true), None);
    assert_eq!(note("debug-keys", true), None);
    assert_eq!(note("", true), None);
    // the daemon's own attach --pane, run wherever the shell was
    assert_eq!(note("attach --address /a --pane 2-abcdefghij", true), None);
    assert!(note("attach f", true).is_some());
}

#[test]
fn hidden_commands_typed_by_a_person_are_refused() {
    let daemon = format!(
        "daemon is started by {NAME} itself; to start one without a terminal, run {NAME} --detach"
    );
    let config_child = format!("config-child is started by {NAME} itself");
    for (line, why) in [
        ("daemon", &daemon),
        ("daemon /tmp/d -O a", &daemon),
        ("config-child", &config_child),
    ] {
        assert_eq!(
            started_by_us(&args(line), true),
            Err(UsageError(why.clone())),
            "{line}"
        );
        assert_eq!(started_by_us(&args(line), false), Ok(()), "{line}");
    }
    for line in ["", "list", "f daemon", "./daemon"] {
        assert_eq!(started_by_us(&args(line), true), Ok(()), "{line}");
    }
}

#[test]
fn every_command_word_is_a_command() {
    for word in COMMAND_WORDS {
        assert!(
            !matches!(parse(&args(word)), Ok(Command::Start(_))),
            "{word}"
        );
    }
}

#[test]
fn flags_take_no_value() {
    for flag in ["--detach", "--no-wait"] {
        assert_eq!(
            refused(&format!("{flag}=x f")),
            format!("{flag} takes no value")
        );
    }
}

#[test]
fn help_after_dashdash_is_a_file() {
    assert_eq!(
        parsed("attach -- -h"),
        Ok(Command::NotBuilt("attach".into()))
    );
    assert_eq!(
        parsed("list -- --help"),
        Ok(Command::NotBuilt("list".into()))
    );
}

#[test]
fn hidden_commands() {
    let daemon = |name: &str, dedup, detach, nvim_args: &str| {
        Ok(Command::Daemon {
            name: name.into(),
            dedup,
            detach,
            nvim_args: args(nvim_args),
        })
    };
    assert_eq!(
        parsed("daemon work -- -O a"),
        daemon("work", false, false, "-O a")
    );
    assert_eq!(
        parsed("daemon work --dedup -- -- f"),
        daemon("work", true, false, "-- f")
    );
    assert_eq!(
        parsed("daemon w-2 --detach --dedup --"),
        daemon("w-2", true, true, "")
    );
    let usage = "daemon takes NAME [--dedup] [--detach] -- NVIM-ARGS";
    for bad in [
        "daemon",
        "daemon work",
        "daemon work -O a",
        "daemon work --dedup --dedup --",
        "daemon work --x --",
    ] {
        assert_eq!(refused(bad), usage, "{bad}");
    }
    assert_eq!(
        refused("daemon /tmp/d --"),
        "daemon: /tmp/d isn't a daemon name"
    );
    assert_eq!(parsed("config-child"), Ok(Command::ConfigChild));
    assert_eq!(parsed("debug-keys"), Ok(Command::DebugKeys));
    assert_eq!(refused("debug-keys x"), "debug-keys takes no arguments");
    assert_eq!(
        parsed("debug-events -O a"),
        Ok(Command::DebugEvents(args("-O a")))
    );
    assert_eq!(parsed("debug-tui"), Ok(Command::DebugTui(vec![])));
    assert_eq!(parsed("debug-panes s"), Ok(Command::DebugPanes("s".into())));
    assert_eq!(parsed("debug-rex s"), Ok(Command::DebugRex("s".into())));
    assert_eq!(
        parsed("debug-latency f"),
        Ok(Command::DebugLatency("f".into()))
    );
    assert_eq!(refused("debug-rex"), "debug-rex takes one socket");
    assert_eq!(refused("debug-latency"), "debug-latency takes one file");
    // the relay test tool runs a requested pane's `{attach}`
    let pane = crate::attach::PaneId::parse("2-abcdefghij").unwrap();
    let relay = Ok(Command::DebugRelay {
        address: "/a/sock".into(),
        pane,
    });
    let attach = "/bin/x attach --address /a/sock --pane 2-abcdefghij";
    assert_eq!(parsed(&format!("debug-relay {attach}")), relay);
    // it relays whatever transport the words name
    for transport in ["relay", "fd"] {
        assert_eq!(
            parsed(&format!("debug-relay {attach} --transport {transport}")),
            relay
        );
    }
    let takes = "debug-relay takes a requested pane's attach command";
    for line in [
        "debug-relay",
        "debug-relay /bin/x",
        "debug-relay /bin/x attach",
    ] {
        assert_eq!(refused(line), takes, "{line}");
    }
    assert_eq!(refused("debug-relay /bin/x list"), takes);
    assert_eq!(parsed("pane s 3"), Ok(Command::Replaced));
    assert_eq!(parsed("pane"), Ok(Command::Replaced));
}

#[test]
fn usage_errors_are_one_line_pointing_at_help() {
    assert_eq!(
        UsageError("version takes no arguments".into()).to_string(),
        format!("{NAME}: version takes no arguments (see {NAME} --help)")
    );
}

#[test]
fn neovims_arguments_need_not_be_unicode() {
    use std::os::unix::ffi::OsStringExt;
    let file = OsString::from_vec(b"caf\xe9.txt".to_vec());
    let line = vec![OsString::from("--name=\u{e9}"), file.clone()];
    let Ok(Command::Start(start)) = parse(&line) else {
        panic!()
    };
    assert_eq!(start.name, Some("\u{e9}".into()));
    assert_eq!(start.reading.files, vec![file]);
}

#[test]
fn attach_pane_takes_an_address_and_a_pane() {
    use crate::attach::{PaneId, Transport};
    let pane = PaneId::parse("2-abcdefghij").unwrap();
    let want = |transport| {
        Ok(Command::AttachPane {
            address: "/tmp/x/sock".into(),
            pane,
            transport,
        })
    };
    assert_eq!(
        parsed("attach --address /tmp/x/sock --pane 2-abcdefghij"),
        want(None)
    );
    assert_eq!(
        parsed("attach --pane=2-abcdefghij --address /tmp/x/sock --transport relay"),
        want(Some(Transport::Relay))
    );
    assert_eq!(
        parsed("attach --address=/tmp/x/sock --pane 2-abcdefghij --transport=fd"),
        want(Some(Transport::Fd))
    );
    let only = "attach --pane takes only --address A and --transport fd|relay";
    for (line, why) in [
        (
            "attach --pane 2-abcdefghij",
            "attach --pane needs --address A",
        ),
        ("attach --address /a --pane 2", "--pane: 2 isn't a pane id"),
        (
            "attach --address /a --pane 2-abcdefghij --pane 3-abcdefghij",
            "--pane is given twice",
        ),
        ("attach --address /a --pane", "--pane needs a value"),
        (
            "attach --address /a --address /b --pane 2-abcdefghij",
            "--address is given twice",
        ),
        (
            "attach --address /a --pane 2-abcdefghij --transport x",
            "--transport takes fd or relay",
        ),
        ("attach --address /a --pane 2-abcdefghij file", only),
        ("attach --address /a --pane 2-abcdefghij +3", only),
        ("attach --address /a --pane 2-abcdefghij --provider p", only),
        ("attach --name n --pane 2-abcdefghij", only),
    ] {
        assert_eq!(refused(line), why, "{line}");
    }
}

#[test]
fn attach_with_an_address_self_attaches_with_an_optional_file_and_line() {
    let self_attach = |file: Option<&str>, line: Option<u32>| {
        Ok(Command::AttachSelf {
            address: "/tmp/x/sock".into(),
            file: file.map(OsString::from),
            line,
        })
    };
    assert_eq!(
        parsed("attach --address /tmp/x/sock"),
        self_attach(None, None)
    );
    assert_eq!(
        parsed("attach --address=/tmp/x/sock notes.md"),
        self_attach(Some("notes.md"), None)
    );
    assert_eq!(
        parsed("attach +12 --address /tmp/x/sock notes.md"),
        self_attach(Some("notes.md"), Some(12))
    );
    // after --, anything is the file
    assert_eq!(
        parsed("attach --address /tmp/x/sock -- --address"),
        self_attach(Some("--address"), None)
    );
    assert_eq!(
        parsed("attach --address /tmp/x/sock -- +3"),
        self_attach(Some("+3"), None)
    );
    for (line, why) in [
        ("attach --address /a a b", "attach takes one FILE"),
        ("attach --address /a +3", "+LINE needs a FILE"),
        ("attach --address /a +3 +4 f", "+LINE is given twice"),
        (
            "attach --address /a --address /b",
            "--address is given twice",
        ),
        ("attach --address", "--address needs a value"),
        ("attach --address /a -x f", "attach doesn't take -x"),
        ("attach --address /a --nope f", "attach doesn't take --nope"),
    ] {
        assert_eq!(refused(line), why, "{line}");
    }
    // what #121 brings
    for (line, what) in [
        ("attach --name n", "attach --name"),
        ("attach --address /a --provider=p", "attach --provider"),
        ("attach notes.md", "attach"),
    ] {
        assert_eq!(parsed(line), Ok(Command::NotBuilt(what.into())), "{line}");
    }
}
