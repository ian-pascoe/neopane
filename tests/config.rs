//! The config child through the built binary: Lua fixtures run by the
//! blocking runner and checked by the daemon's checker (#93 decisions 1-13
//! and 23-29).

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use neopane::config::check::{Command, Severity};
use neopane::config::tree::{Marker, Tree};
use neopane::config::wire::{Called, Ran};
use neopane::config::{
    self, Attach, Builtin, Capability, Choice, Chosen, Outcome, Problems, Request, Spawn,
};
use neopane::name::NAME;

/// The binary Cargo built for this test. Cargo names it, as Rust 1.100's
/// build directory no longer has it beside the test's own binary.
fn exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_neopane"))
}

/// A fresh directory for one fixture.
fn fresh_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "{NAME}-config-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lua")).unwrap();
    dir
}

/// A fixture: `init.lua` in a directory of its own, run with its own
/// attach, environment and time.
struct Fixture {
    dir: PathBuf,
    attach: Attach,
    env: Vec<(OsString, OsString)>,
    time: Duration,
}

impl Fixture {
    /// `source` as `init.lua`, with `{name}` standing for the name.
    fn new(source: &str) -> Fixture {
        let dir = fresh_dir();
        std::fs::write(dir.join("init.lua"), source.replace("{name}", NAME)).unwrap();
        Fixture {
            attach: Attach {
                env: vec![(b"HOME".to_vec(), b"/home/u".to_vec())],
                cwd: dir.as_os_str().as_bytes().to_vec(),
                tty: b"/dev/ttys014".to_vec(),
                pid: 4242,
                daemon: "work".into(),
                provider: None,
            },
            env: std::env::vars_os().collect(),
            time: config::TIME,
            dir,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("init.lua")
    }

    fn run(&self) -> Outcome {
        let request = Request {
            file: self.file().as_os_str().as_bytes().to_vec(),
            version: "9.8.7".into(),
            attach: self.attach.clone(),
            time: self.time,
        };
        let spawn = Spawn {
            exe: &exe(),
            env: &self.env,
            group: None,
        };
        config::run(spawn, &request)
    }

    fn choose(&self) -> Result<Chosen, Problems> {
        config::choose(&self.run(), "init.lua")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// What `provider` returns, in a file around it.
fn returning(value: &str) -> String {
    format!("return {{ provider = function(attach) return {value} end }}")
}

/// The one error `source` gets.
fn error_of(source: &str) -> String {
    let problems = Fixture::new(source)
        .choose()
        .expect_err("an error")
        .0
        .into_iter()
        .filter(|p| p.severity == Severity::Error)
        .map(|p| p.text)
        .collect::<Vec<_>>();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    problems.into_iter().next().unwrap()
}

/// The choice `source` makes, and its warnings.
fn chosen(source: &str) -> (Choice, Vec<String>) {
    let chosen = Fixture::new(source).choose().expect("a choice");
    let warnings = chosen
        .warnings
        .warnings()
        .into_iter()
        .map(String::from)
        .collect();
    (chosen.choice, warnings)
}

/// The lines `source` prints.
fn printed(source: &str) -> Vec<String> {
    Fixture::new(source).run().printed
}

/// What `provider` returned, as the child sent it.
fn tree(value: &str) -> Tree {
    match Fixture::new(&returning(value)).run().answer.unwrap().ran {
        Ran::Returned {
            provider: Called::Returned(tree),
            ..
        } => tree,
        other => panic!("{other:?}"),
    }
}

fn s(text: &str) -> Tree {
    Tree::String(text.as_bytes().to_vec())
}

fn whole(choice: Choice) -> config::Provider {
    match choice {
        Choice::Whole(p) => p,
        other => panic!("{other:?}"),
    }
}

// What the config can return (#93 decisions 1 and 2)

#[test]
fn none_rex_a_table_or_a_pair() {
    assert_eq!(chosen(&returning("nil")).0, Choice::None);
    assert_eq!(chosen(&returning("false")).0, Choice::None);
    assert_eq!(chosen(&returning("")).0, Choice::None);
    assert_eq!(chosen(&returning("'rex'")).0, Choice::Builtin(Builtin::Rex));
    // only the first value counts
    assert_eq!(
        chosen(&returning("'rex', 'tmux'")).0,
        Choice::Builtin(Builtin::Rex)
    );
}

#[test]
fn a_whole_table_gets_its_defaults() {
    let (choice, warnings) = chosen(&returning(
        "{ open = { 'sh', '-c', 'x', '{attach}' }, find = { 'f', '{title}' }, \
         timeouts = { find = 2.5 } }",
    ));
    assert_eq!(warnings, Vec::<String>::new());
    let p = whole(choice);
    assert_eq!(p.name, "config");
    assert_eq!(p.path, "provider()");
    assert_eq!(p.protocol, 1);
    assert_eq!(p.attach_timeout, Duration::from_secs(5));
    assert_eq!(p.context_vars, Vec::<String>::new());
    assert_eq!(
        p.capabilities,
        vec![
            (
                Capability::Open,
                Command {
                    argv: vec![
                        b"sh".to_vec(),
                        b"-c".to_vec(),
                        b"x".to_vec(),
                        b"{attach}".to_vec()
                    ],
                    timeout: Some(Duration::from_secs(5)),
                }
            ),
            (
                Capability::Find,
                Command {
                    argv: vec![b"f".to_vec(), b"{title}".to_vec()],
                    timeout: Some(Duration::from_millis(2500)),
                }
            ),
        ]
    );
}

#[test]
fn every_default_timeout() {
    let all: Vec<String> = Capability::ALL
        .iter()
        .map(|c| format!("{} = {{ 'x' }}", c.name()))
        .collect();
    let p = whole(chosen(&returning(&format!("{{ {} }}", all.join(", ")))).0);
    let secs = |c| {
        p.capability(c)
            .unwrap()
            .timeout
            .map(|t: Duration| t.as_secs())
    };
    for c in [
        Capability::Open,
        Capability::OpenBeside,
        Capability::OpenFloat,
        Capability::OpenTabPage,
    ] {
        assert_eq!(secs(c), Some(5), "{c:?}");
    }
    assert_eq!(secs(Capability::Find), Some(3));
    for c in [
        Capability::Close,
        Capability::PlaceBeside,
        Capability::Resize,
        Capability::Focus,
        Capability::FocusDirection,
    ] {
        assert_eq!(secs(c), Some(2), "{c:?}");
    }
    assert_eq!(secs(Capability::ReportLayout), Some(1));
    assert_eq!(secs(Capability::Events), None);
}

#[test]
fn settings_are_read() {
    let p = whole(
        chosen(&returning(
            "{ name = 'mine', open = { 'o' }, attach_timeout = 7, protocols = { 1, 2 }, \
             context_vars = { 'TMUX', 'TMUX', 'X_1' }, find_vars = { 'TMUX_PANE' } }",
        ))
        .0,
    );
    assert_eq!(p.name, "mine");
    assert_eq!(p.attach_timeout, Duration::from_secs(7));
    assert_eq!(p.protocol, 1);
    assert_eq!(p.context_vars, ["TMUX", "X_1"]);
    assert_eq!(p.find_vars, ["TMUX_PANE"]);
}

#[test]
fn a_pair_takes_each_half_its_own() {
    let (choice, warnings) = chosen(&returning(
        "{ terminal = { name = 'ghostty', open = { 'g' }, close = { 'c', '{terminal}' }, \
           find = { 'ignored' } }, \
           layout = { name = 'aerospace', find = { 'a' }, focus = { 'f', '{terminal}' }, \
           open = { 'ignored' }, context_vars = { 'AEROSPACE' } } }",
    ));
    assert_eq!(warnings, Vec::<String>::new());
    let Choice::Pair(pair) = choice else {
        panic!("{choice:?}")
    };
    assert_eq!(pair.name, "ghostty+aerospace");
    let terminal = pair.terminal.unwrap();
    let caps = |p: &config::Provider| p.capabilities.iter().map(|(c, _)| *c).collect::<Vec<_>>();
    assert_eq!(caps(&terminal), [Capability::Open, Capability::Close]);
    assert_eq!(terminal.path, "provider().terminal");
    let layout = pair.layout.unwrap();
    assert_eq!(caps(&layout), [Capability::Find, Capability::Focus]);
    assert_eq!(layout.context_vars, ["AEROSPACE"]);
}

#[test]
fn a_pair_is_named() {
    let named = chosen(&returning(
        "{ name = 'both', terminal = { open = { 'o' } }, layout = { focus = { 'f' } } }",
    ))
    .0;
    let unnamed = chosen(&returning(
        "{ terminal = { open = { 'o' } }, layout = { focus = { 'f' } } }",
    ))
    .0;
    let alone = chosen(&returning("{ layout = { name = 'wm', focus = { 'f' } } }")).0;
    let name = |c: Choice| match c {
        Choice::Pair(p) => p.name,
        other => panic!("{other:?}"),
    };
    assert_eq!(name(named), "both");
    assert_eq!(name(unnamed), "terminal+layout");
    assert_eq!(name(alone), "wm");
}

#[test]
fn false_counts_as_absent() {
    let p = whole(
        chosen(&returning(
            "{ open = { 'o' }, close = false, terminal = false, name = false }",
        ))
        .0,
    );
    assert_eq!(p.name, "config");
    assert!(p.capability(Capability::Close).is_none());
}

// Decision 23's errors, with decision 24's messages

#[test]
fn the_file_cant_be_read() {
    let f = Fixture::new("");
    std::fs::remove_file(f.file()).unwrap();
    let problems = f.choose().unwrap_err();
    assert_eq!(
        problems.errors(),
        ["can't read init.lua: No such file or directory (os error 2)"]
    );
}

#[test]
fn the_file_fails_to_load_or_run() {
    assert_eq!(
        error_of("return {"),
        "init.lua failed to load: init.lua:1: unexpected symbol near '<eof>'"
    );
    assert_eq!(
        error_of("error('boom')"),
        "init.lua failed: init.lua:1: boom"
    );
}

#[test]
fn the_file_ends_without_answering() {
    assert_eq!(
        error_of("os.exit(0)"),
        "init.lua ended before answering (exit status 0; did it call os.exit?)"
    );
    assert_eq!(
        error_of("os.execute('kill -TERM $PPID') while true do end"),
        "init.lua was killed by signal 15 before answering"
    );
    let mut f = Fixture::new("while true do end");
    f.time = Duration::from_millis(500);
    assert_eq!(
        f.choose().unwrap_err().errors(),
        ["init.lua didn't answer within 0.5 s"]
    );
}

#[test]
fn no_table_or_no_provider_function() {
    assert_eq!(
        error_of(""),
        "init.lua returned nil, not a table with provider"
    );
    assert_eq!(
        error_of("return 'rex'"),
        "init.lua returned a string, not a table with provider"
    );
    assert_eq!(
        error_of("return {}"),
        "init.lua returned a table without provider"
    );
    assert_eq!(
        error_of("return { provider = 'rex' }"),
        "init.lua returned provider as a string, not a function"
    );
}

#[test]
fn the_files_table_is_too_big() {
    assert_eq!(
        error_of("return { provider = function() return 'rex' end, [string.rep('k', 2^20)] = 1 }"),
        "init.lua returned a table of more than 1 MiB"
    ); // many keys, each short
    assert_eq!(
        error_of(
            "local t = { provider = function() return 'rex' end } \
             for i = 1, 20000 do t[string.rep('k', 60) .. i] = 1 end return t"
        ),
        "init.lua returned a table of more than 1 MiB"
    );
}

#[test]
fn provider_raises_or_returns_the_wrong_thing() {
    assert_eq!(
        error_of("return { provider = function() error('nope') end }"),
        "provider() raised: init.lua:1: nope"
    );
    assert_eq!(
        error_of(&returning("42")),
        "provider() returned the number 42; expected nil, false, \"rex\" or a table"
    );
    assert_eq!(
        error_of(&returning("true")),
        "provider() returned true; expected nil, false, \"rex\" or a table"
    );
    assert_eq!(
        error_of(&returning("'zellij'")),
        "provider() returned \"zellij\"; the only built-in name is \"rex\""
    );
    assert_eq!(
        error_of(&returning("'tmux'")),
        format!(
            "provider() returned \"tmux\"; the only built-in name is \"rex\" (for tmux's \
             table, return {NAME}.builtin.tmux())"
        )
    );
    assert_eq!(
        error_of(&returning("print")),
        "provider() returned a function (call it: return f(attach), not f)"
    );
}

#[test]
fn a_field_of_the_wrong_type() {
    assert_eq!(
        error_of(&returning("{ layout = { focus = { 'x', 2 } } }")),
        "provider().layout.focus[2]: expected string, got number (wrap numbers in tostring())"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x', print } }")),
        "provider().open[2]: expected string, got function"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, name = 3 }")),
        "provider().name: expected string, got number (wrap numbers in tostring())"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, timeouts = { open = '3' } }")),
        "provider().timeouts.open: expected number, got string"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, context_vars = 'TMUX' }")),
        "provider().context_vars: expected a list of variable names, got string"
    );
}

#[test]
fn a_bad_argv() {
    assert_eq!(
        error_of(&returning("{ open = 'x' }")),
        "provider().open: expected a list of strings, got string"
    );
    assert_eq!(
        error_of(&returning("{ open = { cmd = 'x' } }")),
        "provider().open: expected a list of strings, but it has the key cmd"
    );
    assert_eq!(
        error_of(&returning(
            "{ terminal = { open = { 'a', 'b', 'c', 'd', nil, 'f' } } }"
        )),
        "provider().terminal.open: nothing at [5] but more after it (is a variable nil?)"
    );
    assert_eq!(
        error_of(&returning("{ open = {} }")),
        "provider().open: is empty; a command needs at least its program"
    );
    assert_eq!(
        error_of(&returning("{ open = { '' } }")),
        "provider().open[1]: the program is empty"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'a\\0b' } }")),
        "provider().open[1]: holds a NUL byte"
    );
}

#[test]
fn bad_settings() {
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, protocols = {} }")),
        "provider().protocols: is empty; give at least one protocol number"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, protocols = { 1.5 } }")),
        "provider().protocols[1]: expected a positive whole number, got 1.5"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, protocols = { 0 } }")),
        "provider().protocols[1]: expected a positive whole number, got 0"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, timeouts = { open = 0 } }")),
        "provider().timeouts.open: must be a number of seconds over 0, got 0"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, attach_timeout = 1/0 }")),
        "provider().attach_timeout: must be a number of seconds over 0, got inf"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, name = '' }")),
        "provider().name: must be a non-empty name without control characters"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, name = 'a\\nb' }")),
        "provider().name: must be a non-empty name without control characters"
    );
    assert_eq!(
        error_of(&returning("{ open = { 'x' }, context_vars = { '1X' } }")),
        "provider().context_vars[1]: \"1X\" isn't a variable name"
    );
}

#[test]
fn bad_pairs() {
    assert_eq!(
        error_of(&returning(
            "{ terminal = { open = { 'x' } }, close = { 'y' } }"
        )),
        "provider().close: a capability can't sit beside terminal and layout; move it into \
         one of them"
    );
    assert_eq!(
        error_of(&returning("{ terminal = 'ghostty' }")),
        "provider().terminal: expected a pane provider table, got string"
    );
    assert_eq!(
        error_of(&returning("{ layout = 'rex' }")),
        "provider().layout: rex is never half of a pair"
    );
    assert_eq!(
        error_of(&returning(
            "{ terminal = { terminal = { open = { 'x' } } } }"
        )),
        "provider().terminal: a pair can't be half of a pair"
    );
}

#[test]
fn nothing_usable_or_nothing_shared() {
    assert_eq!(
        error_of(&returning("{ name = 'x' }")),
        "provider(): has no usable capability"
    );
    assert_eq!(
        error_of(&returning("{ terminal = { find = { 'f' } } }")),
        "provider().terminal: has no usable capability (a terminal provider uses open and \
         close)"
    );
    assert_eq!(
        error_of(&returning(
            "{ layout = { focus = { 'x' }, protocols = { 2, 3 } } }"
        )),
        format!(
            "provider().layout: shares no protocol number with {NAME} (it speaks 1; this \
             table says 2, 3)"
        )
    );
}

#[test]
fn the_result_is_too_big_or_too_deep() {
    assert_eq!(
        error_of(&returning("{ open = { string.rep('x', 2^20) } }")),
        "provider() returned more than 1 MiB"
    );
    let deep = "{ t = ".repeat(16) + "{}" + &" }".repeat(16);
    assert_eq!(
        error_of(&returning(&deep)),
        "provider() returned tables nested more than 16 levels deep"
    );
}

// Decision 23's warnings

#[test]
fn unknown_keys_and_fields() {
    let (_, warnings) = chosen(
        "return { provider = function() return { open = { 'x' }, colour = 'red', \
         place_float = { 'p' }, 'positional' } end, log = 'debug' }",
    );
    assert_eq!(
        warnings,
        [
            "init.lua returned an unknown key, log; only provider is read",
            "provider()[1]: unknown field; ignored",
            "provider().colour: unknown field; ignored",
            "provider().place_float: unknown field; ignored",
        ]
    );
    let (_, warnings) = chosen(&returning(
        "{ terminal = { open = { 'x' } }, ['odd key'] = 1 }",
    ));
    assert_eq!(
        warnings,
        ["provider()[\"odd key\"]: unknown field; ignored"]
    );
}

#[test]
fn unknown_timeouts_and_long_ones() {
    let (choice, warnings) = chosen(&returning(
        "{ open = { 'x' }, events = { 'e' }, attach_timeout = 61, \
         timeouts = { opne = 1, events = 5, open = 90 } }",
    ));
    assert_eq!(
        warnings,
        [
            "provider().attach_timeout: 61 s is over 60 s; 60 s used",
            "provider().timeouts.events: events runs until it ends, with no timeout; ignored",
            "provider().timeouts.open: 90 s is over 60 s; 60 s used",
            "provider().timeouts.opne: no such capability; ignored",
        ]
    );
    let p = whole(choice);
    assert_eq!(p.attach_timeout, Duration::from_secs(60));
    assert_eq!(
        p.capability(Capability::Open).unwrap().timeout,
        Some(Duration::from_secs(60))
    );
}

#[test]
fn placeholders_a_capability_isnt_given() {
    let (choice, warnings) = chosen(&returning(
        "{ open = { 'x', '--title={title}', '{{pane}}', '#{pane_id}' }, \
           open_beside = { 'y', '{dir}' } }",
    ));
    assert_eq!(
        warnings,
        [
            "provider().open[2]: {title} inside a longer word isn't replaced; passed as written",
            "provider().open_beside: {dir} isn't a placeholder open_beside gets; open_beside \
             ignored",
        ]
    );
    let p = whole(choice);
    assert!(p.capability(Capability::Open).is_some());
    assert!(p.capability(Capability::OpenBeside).is_none());
    // every command gets {capability}, {protocol} and {exe}
    let (_, warnings) = chosen(&returning(
        "{ events = { '{exe}', '{capability}', '{protocol}' } }",
    ));
    assert_eq!(warnings, Vec::<String>::new());
}

#[test]
fn a_halfs_one_step_capability_shows_only_in_check() {
    let chosen = Fixture::new(&returning(
        "{ terminal = { open = { 'x' }, open_beside = { 'y' } } }",
    ))
    .choose()
    .unwrap();
    assert_eq!(chosen.warnings.warnings(), Vec::<&str>::new());
    assert_eq!(
        chosen.warnings.0[0].text,
        "provider().terminal.open_beside: a half's one-step capability is ignored; only a \
         table returned whole gets one"
    );
    assert_eq!(chosen.warnings.message(), None);
}

#[test]
fn at_most_twenty_problems() {
    let fields: Vec<String> = (0..25).map(|i| format!("u{i:02} = 1")).collect();
    let problems = Fixture::new(&returning(&format!(
        "{{ {}, open = {{ 1 }} }}",
        fields.join(", ")
    )))
    .choose()
    .unwrap_err();
    assert_eq!(problems.0.len(), 20);
    assert_eq!(
        problems.message().unwrap(),
        format!(
            "{NAME}: config: provider().open[1]: expected string, got number (wrap numbers \
             in tostring()) (and 19 more: {NAME} provider check)"
        )
    );
}

#[test]
fn an_error_after_twenty_warnings_is_kept() {
    // a01..a20 sort before open, so twenty warnings come first
    let fields: Vec<String> = (1..=20).map(|i| format!("a{i:02} = 1")).collect();
    let problems = Fixture::new(&returning(&format!(
        "{{ {}, open = 5 }}",
        fields.join(", ")
    )))
    .choose()
    .unwrap_err();
    assert_eq!(problems.0.len(), 20);
    assert_eq!(
        problems.errors(),
        ["provider().open: expected a list of strings, got number"]
    );
}

// The plain tree

#[test]
fn the_tree_keeps_key_kinds_and_marks_what_isnt_data() {
    let got = tree(
        "(function() local t = { 'a', 2.5, k = true, [true] = 'yes', [1.5] = 'n', \
         f = print, co = coroutine.create(print), u = io.stdin } t.me = t \
         t.inner = { up = t } return t end)()",
    );
    assert_eq!(
        got,
        Tree::Table(vec![
            (Tree::Number(1.0), s("a")),
            (Tree::Number(1.5), s("n")),
            (Tree::Number(2.0), Tree::Number(2.5)),
            (s("co"), Tree::Marker(Marker::Coroutine)),
            (s("f"), Tree::Marker(Marker::Function)),
            (
                s("inner"),
                Tree::Table(vec![(s("up"), Tree::Marker(Marker::Cycle))])
            ),
            (s("k"), Tree::Bool(true)),
            (s("me"), Tree::Marker(Marker::Cycle)),
            (s("u"), Tree::Marker(Marker::Userdata)),
            (Tree::Bool(true), s("yes")),
        ])
    );
}

#[test]
fn a_table_seen_twice_is_not_a_cycle() {
    let shared = Tree::Table(vec![(Tree::Number(1.0), s("x"))]);
    assert_eq!(
        tree("(function() local a = { 'x' } return { a, a } end)()"),
        Tree::Table(vec![
            (Tree::Number(1.0), shared.clone()),
            (Tree::Number(2.0), shared)
        ])
    );
}

#[test]
fn metatables_are_ignored() {
    assert_eq!(
        tree("setmetatable({}, { __index = { open = { 'x' } }, __pairs = error })"),
        Tree::Table(vec![])
    );
}

#[test]
fn strings_are_bytes() {
    assert_eq!(tree("'\\255\\0x'"), Tree::String(vec![0xff, 0, b'x']));
}

// The child's Lua (#93 decisions 4, 6 and 7)

#[test]
fn the_global_and_require_give_the_same_table() {
    let f = Fixture::new(
        "local n = require('{name}') \
         print(n == {name}, n.os, n.version, n.config_dir, type(n.builtin), type(n.inside), \
               next(n.builtin), next(n.inside)) \
         return { provider = function() end }",
    );
    let dir = f.dir.display().to_string();
    assert_eq!(
        f.run().printed,
        [format!(
            "true\t{}\t9.8.7\t{dir}\ttable\ttable\ttmux\tnil",
            std::env::consts::OS
        )]
    );
}

#[test]
fn the_attach_table() {
    let mut f = Fixture::new(
        "return { provider = function(a) \
           print(a.env.HOME, a.env.A, a.cwd == '' , a.tty, a.pid, a.daemon, a.provider) \
         end }",
    );
    f.attach.env.push((b"A".to_vec(), vec![0xff]));
    f.attach.env.push((b"HOME".to_vec(), b"/second".to_vec()));
    f.attach.provider = Some("tmux".into());
    assert_eq!(
        f.run().printed,
        ["/home/u\t\u{fffd}\tfalse\t/dev/ttys014\t4242\twork\ttmux"]
    );
    let f = Fixture::new("return { provider = function(a) print(a.provider) end }");
    assert_eq!(f.run().printed, ["nil"]);
}

#[test]
fn it_runs_in_the_attachs_directory() {
    let f = Fixture::new("print(io.open('init.lua') ~= nil) return {}");
    assert_eq!(f.run().printed, ["true"]);
    let mut f = Fixture::new("os.execute('pwd') return {}");
    f.attach.cwd = b"/nonexistent/gone".to_vec();
    // gone: it runs in /
    assert_eq!(f.run().printed, ["/"]);
}

#[test]
fn output_goes_to_the_log_line_by_line() {
    assert_eq!(
        printed(
            "print('a', 1, nil) io.write('b', 2) io.write('c\\n') \
             io.stdout:write('d\\ne'):write('\\n') io.stderr:write('f\\n') \
             io.output():write('g\\n') os.execute('echo h; echo i >&2') return {}"
        ),
        ["a\t1\tnil", "b2c", "d", "e", "f", "g", "h", "i"]
    );
}

#[test]
fn output_is_cut_at_64_kib() {
    let outcome = Fixture::new("for i = 1, 2000 do print(string.rep('x', 99)) end return {}").run();
    assert!(outcome.printed_cut);
    let lines = outcome.printed;
    // 655 lines of 100 bytes, then 36 bytes of the next
    assert_eq!(lines.len(), 656);
    assert_eq!(lines[654], "x".repeat(99));
    assert_eq!(lines[655], "x".repeat(36));
}

#[test]
fn stdin_is_empty() {
    assert_eq!(printed("print(#io.read('*a')) return {}"), ["0"]);
}

#[test]
fn every_library_but_no_c_modules() {
    // no sandbox (ADR 0007): only require is limited
    assert_eq!(
        printed(
            "print(type(debug), type(debug.traceback), package.cpath, package.loadlib, \
             #package.loaders, type(os.exit), type(io.popen), type(dofile), \
             type(string.dump)) return {}"
        ),
        ["table\tfunction\t\tnil\t2\tfunction\tfunction\tfunction\tfunction"]
    );
}

#[test]
fn require_reads_only_the_configs_lua_directory() {
    let mut f = Fixture::new(
        "print(require('mine').x, require('pkg').y, (pcall(require, 'elsewhere'))) return {}",
    );
    std::fs::write(f.dir.join("lua/mine.lua"), "return { x = 1 }").unwrap();
    std::fs::create_dir_all(f.dir.join("lua/pkg")).unwrap();
    std::fs::write(f.dir.join("lua/pkg/init.lua"), "return { y = 2 }").unwrap();
    let other = fresh_dir();
    std::fs::write(other.join("elsewhere.lua"), "return true").unwrap();
    let lua_path = format!("{}/?.lua;;", other.display());
    f.env.push(("LUA_PATH".into(), lua_path.into()));
    let lines = f.run().printed;
    std::fs::remove_dir_all(&other).unwrap();
    assert_eq!(lines, ["1\t2\tfalse"]);
}

// <name>.run (#93 decision 25)

/// What `<name>.run(args)` returned, printed as `ok code error stdout stderr`.
fn run_result(args: &str) -> String {
    let lines = printed(&format!(
        "local r = {{name}}.run({args}) \
         print(r.ok, r.code, r.error, r.stdout, r.stderr) return {{}}"
    ));
    lines.join("\n")
}

#[test]
fn run_runs_without_a_shell() {
    assert_eq!(
        run_result("{ 'sh', '-c', 'echo out; echo err >&2; exit 3' }"),
        "false\t3\tnil\tout\n\terr\n"
    );
    assert_eq!(run_result("{ 'echo', '$HOME' }"), "true\t0\tnil\t$HOME\n\t");
    assert_eq!(
        run_result("{ 'cat' }, { stdin = 'fed' }"),
        "true\t0\tnil\tfed\t"
    );
}

#[test]
fn run_times_out() {
    let started = std::time::Instant::now();
    assert_eq!(
        run_result("{ 'sh', '-c', 'echo before; exec sleep 5' }, { timeout = 0.3 }"),
        "false\tnil\ttimed out after 0.3 s\tbefore\n\t"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn run_is_cut_to_the_configs_time() {
    let mut f = Fixture::new(
        "local r = {name}.run({ 'sleep', '5' }, { timeout = 30 }) print(r.error) \
         r = {name}.run({ 'true' }) print(r.error) return {}",
    );
    f.time = Duration::from_millis(600);
    // 0.6 s less 0.25 s for the config to answer, less the time it took to
    // get there
    let lines = f.run().printed;
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].starts_with("timed out after 0."), "{lines:?}");
    assert_eq!(lines[1], "no time left (the config has 0.6 s)");
}

#[test]
fn run_takes_a_huge_timeout() {
    assert_eq!(
        run_result("{ 'true' }, { timeout = 1e20 }"),
        "true\t0\tnil\t\t"
    );
}

#[test]
fn run_reports_a_signal() {
    assert_eq!(
        run_result("{ 'sh', '-c', 'kill -9 $$' }"),
        "false\tnil\tkilled by signal 9\t\t"
    );
}

#[test]
fn run_reports_what_cant_start() {
    let r = run_result("{ '/nonexistent/program' }");
    assert!(
        r.starts_with("false\tnil\tcan't start: /nonexistent/program: No such file"),
        "{r}"
    );
}

#[test]
fn run_sets_and_removes_variables() {
    let mut f = Fixture::new(
        "local r = {name}.run({ 'sh', '-c', 'echo ${X-unset} $Y' }, \
           { env = { X = false, Y = 'set' } }) io.write(r.stdout) \
         r = {name}.run({ 'pwd' }, { cwd = '/' }) io.write(r.stdout) return {}",
    );
    f.env.push(("X".into(), "inherited".into()));
    assert_eq!(f.run().printed, ["unset set", "/"]);
}

#[test]
fn run_raises_on_bad_arguments() {
    for (args, why) in [
        ("'ls'", "run: expected a list of strings, got string"),
        ("{}", "run: argv is empty"),
        ("{ '' }", "run: argv[1], the program, is empty"),
        ("{ 'ls', 1 }", "run: argv[2] is a number, not a string"),
        ("{ 'a\\0b' }", "run: argv[1] holds a NUL byte"),
        ("{ 'ls' }, { tiemout = 1 }", "run: unknown option tiemout"),
        (
            "{ 'ls' }, { timeout = -1 }",
            "run: opts.timeout must be a number of seconds over 0, got -1",
        ),
        (
            "{ 'ls' }, { env = { X = 1 } }",
            "run: opts.env.X must be a string or false, got 1",
        ),
    ] {
        let lines = printed(&format!(
            "print(select(2, pcall({{name}}.run, {args}))) return {{}}"
        ));
        assert_eq!(lines, [format!("{NAME}.{why}")], "{args}");
    }
}

// <name>.which

#[test]
fn which_finds_programs_on_path() {
    let f = Fixture::new(
        "print({name}.which('sh'), {name}.which('no-such-program-here'), \
         {name}.which('/bin/sh'), {name}.which('/nonexistent/sh')) return {}",
    );
    let lines = f.run().printed;
    let fields: Vec<&str> = lines[0].split('\t').collect();
    assert!(
        fields[0].starts_with('/') && fields[0].ends_with("/sh"),
        "{lines:?}"
    );
    assert_eq!(&fields[1..], ["nil", "/bin/sh", "nil"]);
}

// <name>.builtin: the built-in tables (ADR 0016)

#[test]
fn builtin_tmux_is_the_tmux_table_and_passes_the_checker() {
    let (choice, warnings) = chosen(&returning("{name}.builtin.tmux()"));
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(whole(choice), config::BuiltinTable::Tmux.provider());
    // fresh each time
    let (choice, _) = chosen(&returning(
        "(function() local t = {name}.builtin.tmux() t.find = false \
           return {name}.builtin.tmux() end)()",
    ));
    assert_eq!(whole(choice), config::BuiltinTable::Tmux.provider());
}

#[test]
fn builtin_tmux_takes_no_options() {
    let (choice, _) = chosen(&returning("{name}.builtin.tmux({})"));
    assert_eq!(whole(choice), config::BuiltinTable::Tmux.provider());
    assert_eq!(
        error_of(&returning("({name}.builtin.tmux({ split = 'h' }))")),
        format!("provider() raised: init.lua:1: {NAME}.builtin.tmux: takes no options; got split")
    );
    assert_eq!(
        error_of(&returning("({name}.builtin.tmux(5))")),
        format!(
            "provider() raised: init.lua:1: {NAME}.builtin.tmux: takes no options; got the \
             number 5"
        )
    );
}

#[test]
fn provider_show_prints_what_builtin_returns() {
    let shown = std::process::Command::new(exe())
        .args(["provider", "show", "tmux"])
        .output()
        .unwrap();
    assert!(shown.status.success());
    let f = Fixture::new(
        "local function same(a, b) \
           if type(a) ~= 'table' or type(b) ~= 'table' then return a == b end \
           for k, v in pairs(a) do if not same(v, b[k]) then return false end end \
           for k in pairs(b) do if a[k] == nil then return false end end \
           return true \
         end \
         print(same(dofile({name}.config_dir .. '/shown.lua'), {name}.builtin.tmux())) \
         return {}",
    );
    std::fs::write(f.dir.join("shown.lua"), &shown.stdout).unwrap();
    assert_eq!(f.run().printed, ["true"]);
}

// <name>.default (#93 decision 27): the same as the daemon's

#[test]
fn default_agrees_with_the_daemons() {
    let source = "return { provider = function(attach) return {name}.default(attach) end }";
    // set means present, even empty, as the daemon reads its own
    let envs: [&[(&str, &str)]; 9] = [
        &[],
        &[("REX_SERVER", "ws://x"), ("REX_SESSION", "s")],
        &[("REX_SERVER", "ws://x")],
        &[("REX_SESSION", "s")],
        &[("REX_SERVER", ""), ("REX_SESSION", "s")],
        &[("REX_SERVER", ""), ("REX_SESSION", "")],
        &[
            ("TMUX", "/tmp/t,1,0"),
            ("REX_SERVER", "ws://x"),
            ("REX_SESSION", "s"),
        ],
        &[("TERM_PROGRAM", "tmux"), ("TMUX", "/tmp/t,1,0")],
        &[
            ("TERM_PROGRAM", "tmux"),
            ("TMUX", "/tmp/t,1,0"),
            ("REX_SERVER", "ws://x"),
            ("REX_SESSION", "s"),
        ],
    ];
    let want = [false, true, false, false, true, true, true, false, false];
    let tmux = Choice::Whole(config::BuiltinTable::Tmux.provider());
    let wants_tmux = [false, false, false, false, false, false, false, true, true];
    for ((env, want), wants_tmux) in envs.into_iter().zip(want).zip(wants_tmux) {
        let mut f = Fixture::new(source);
        f.attach.env = env
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect();
        let daemons = config::default(&f.attach);
        assert_eq!(f.choose().unwrap().choice, daemons, "{env:?}");
        let rex = Choice::Builtin(Builtin::Rex);
        assert_eq!(daemons == rex, want, "{env:?}");
        assert_eq!(daemons == tmux, wants_tmux, "{env:?}");
    }
}

#[test]
fn default_never_raises() {
    assert_eq!(
        printed(
            "print({name}.default(), {name}.default(1), {name}.default({}), \
             {name}.default({ env = 2 })) return {}"
        ),
        ["nil\tnil\tnil\tnil"]
    );
}

// The process group

/// Whether `pid` runs: killed, it may stay a zombie until init reaps it.
fn running(pid: i32) -> bool {
    let ps = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&ps.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

#[test]
fn nothing_the_config_starts_outlives_it() {
    let f = Fixture::new(
        "os.execute('sleep 30 & echo $! > pid') \
         print(os.execute('kill -0 $(cat pid)')) \
         return { provider = function() return 'rex' end }",
    );
    let outcome = f.run();
    assert!(outcome.answer.is_ok());
    // it ran while the config did
    assert_eq!(outcome.printed, ["0"]);
    let pid: i32 = std::fs::read_to_string(f.dir.join("pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let mut alive = running(pid);
    for _ in 0..100 {
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        alive = running(pid);
    }
    assert!(!alive, "sleep {pid} outlived the config");
}

#[test]
fn the_answer_carries_the_childs_build() {
    let answer = Fixture::new(&returning("nil")).run().answer.unwrap();
    assert_eq!(answer.build, neopane::attach::build());
}
