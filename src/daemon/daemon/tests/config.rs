//! The config choosing at a self-attach (#129), without Neovim: the config
//! child is the built binary, running real Lua; the daemon's loop is
//! pumped by hand, so which attach waits for which is forced, not timed.

use std::path::{Path, PathBuf};

use super::*;
use crate::daemon::attach_conn::{self_hello, test_terminal};
use crate::daemon::pane::Reach;
use crate::providers::choose::{Chosen, ConfigFile};

/// The built binary, which runs the config child (`util::built_binary`).
fn child_exe() -> Option<String> {
    crate::util::built_binary().map(|b| b.to_string_lossy().into_owned())
}

/// A fresh directory for test `tag`, its config `init_lua` there, with
/// `{dir}` standing for the directory.
fn config_dir(tag: &str, init_lua: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("{NAME}-cfg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = init_lua.replace("{dir}", dir.to_str().unwrap());
    std::fs::write(dir.join("init.lua"), source).unwrap();
    dir
}

/// A daemon with no pane provider, Neovim set up, whose config is
/// `file`, `$<NAME>_CONFIG` naming it if `named`. None without the binary.
fn fixture_config(file: &Path, named: bool) -> Option<Fixture> {
    let exe = child_exe()?;
    let mut fx = with_provider(None);
    fx.d.nvim.ready = true;
    fx.d.exe = exe;
    fx.d.name = "work".into();
    fx.d.config_file = Some(ConfigFile {
        path: file.to_path_buf(),
        named,
    });
    Some(fx)
}

/// Self-attaches a terminal with no `REX_*` to `fx`'s daemon: its id, its
/// helper's end and its pty.
fn attach(fx: &mut Fixture, tag: &str) -> (u32, UnixStream, Pty) {
    let (helper, pty) = self_attach_with(fx, tag, &self_hello(test_terminal(80, 24)));
    fx.d.window_ask = None;
    (fx.d.panes.list.last().unwrap().id, helper, pty)
}

/// The pane the config runs for, if one runs.
fn config_runs_for(d: &Daemon) -> Option<u32> {
    d.config_run.as_ref().map(|(id, _)| *id)
}

/// Waits up to 10 s for the config running to answer, and has the daemon
/// take its answer, as its loop would.
fn config_answers(d: &mut Daemon) {
    let fd = d.config_run.as_ref().expect("a config runs").1.fd();
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 10_000) };
    assert_eq!(1, ready, "the config never answered");
    d.on_config().unwrap();
}

#[test]
fn a_config_error_is_no_pane_provider_with_one_warning() {
    let dir = config_dir(
        "error",
        "return { provider = function() return \"tmux\" end }\n",
    );
    let Some(mut fx) = fixture_config(&dir.join("init.lua"), false) else {
        return;
    };
    let (id, _helper, _pty) = attach(&mut fx, "cfg-error");
    assert_eq!(Some(id), config_runs_for(&fx.d));
    assert_eq!("(choosing)", fx.d.provider_shown());
    let _ = sent_to_nvim(&mut fx);
    config_answers(&mut fx.d);
    assert!(fx.d.provider.is_none());
    assert_eq!("none (config error)", fx.d.provider_shown());
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    let warned = sent_to_nvim(&mut fx);
    let warning = format!("{NAME}: config: provider() returned \"tmux\"");
    assert_eq!(1, warned.matches(&warning).count(), "{warned:?}");
    // the next self-attach runs the file again
    let (next, _h, _p) = attach(&mut fx, "cfg-error-2");
    assert_eq!(Some(next), config_runs_for(&fx.d));
    config_answers(&mut fx.d);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_config_named_but_missing_is_an_error_and_a_default_place_empty_is_the_default() {
    let dir = config_dir("missing", "");
    let missing = dir.join("nowhere.lua");
    let Some(mut fx) = fixture_config(&missing, true) else {
        return;
    };
    let _ = sent_to_nvim(&mut fx);
    let (id, _helper, _pty) = attach(&mut fx, "cfg-missing");
    assert_eq!(None, config_runs_for(&fx.d));
    assert_eq!("none (config error)", fx.d.provider_shown());
    assert_eq!(Reach::Out, fx.d.panes.by_id(id).unwrap().reach);
    let warned = sent_to_nvim(&mut fx);
    let warning = format!(
        "{NAME}: config: {} isn't there ({}_CONFIG names it); no pane provider",
        missing.display(),
        NAME.to_ascii_uppercase()
    );
    assert_eq!(1, warned.matches(&warning).count(), "{warned:?}");

    // not named: the built-in default chooses, here none, quietly
    let Some(mut fx) = fixture_config(&missing, false) else {
        return;
    };
    let _ = sent_to_nvim(&mut fx);
    let _attached = attach(&mut fx, "cfg-default");
    assert_eq!(None, config_runs_for(&fx.d));
    assert_eq!("none", fx.d.provider_shown());
    let warned = sent_to_nvim(&mut fx);
    assert!(!warned.contains("config:"), "{warned:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_self_attach_while_the_config_runs_waits_and_the_file_runs_once() {
    // the config notes its run, then waits for the test's go
    let dir = config_dir(
        "wait",
        r#"local f = io.open("{dir}/runs", "a")
f:write("ran\n")
f:close()
while not io.open("{dir}/go") do os.execute("sleep 0.02") end
return { provider = function() return { name = "fake", open = { "true" } } end }
"#,
    );
    let Some(mut fx) = fixture_config(&dir.join("init.lua"), false) else {
        return;
    };
    let (first, _h1, _p1) = attach(&mut fx, "cfg-wait-1");
    assert_eq!(Some(first), config_runs_for(&fx.d));
    let (second, _h2, _p2) = attach(&mut fx, "cfg-wait-2");
    // it waits its turn: the config isn't run for it
    assert_eq!(Some(first), config_runs_for(&fx.d));
    assert_eq!(vec![second], fx.d.waiting_choice);
    assert_eq!(Reach::Finding, fx.d.panes.by_id(second).unwrap().reach);

    std::fs::write(dir.join("go"), "").unwrap();
    config_answers(&mut fx.d);
    assert_eq!("fake", fx.d.provider_shown());
    assert!(fx.d.waiting_choice.is_empty());
    assert_eq!(None, config_runs_for(&fx.d));
    for id in [first, second] {
        assert_eq!(Reach::In, fx.d.panes.by_id(id).unwrap().reach, "pane {id}");
    }
    let runs = std::fs::read_to_string(dir.join("runs")).unwrap();
    assert_eq!("ran\n", runs);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn start_with_no_first_choice_nor_config_running_runs_the_config() {
    // serve.rs found a config but couldn't start it: no first choice and
    // no config running, so `start` chooses
    let dir = config_dir("start", "return { provider = function() return nil end }\n");
    let Some(mut fx) = fixture_config(&dir.join("init.lua"), false) else {
        return;
    };
    let (socket, helper) = UnixStream::pair().unwrap();
    let pty = Pty::new(80, 24);
    // SAFETY: a copy of the slave, which the surface takes.
    let slave = unsafe { libc::dup(pty.slave) };
    let surface = Surface::create(socket, slave, surface::cells(80, 24));
    let hello = crate::daemon::pane::AttachHello {
        cwd: b"/".to_vec(),
        ..Default::default()
    };
    fx.d.start(&mut Some(SelfAttach {
        surface,
        terminal: None,
        hello: Some(Box::new(hello)),
    }))
    .unwrap();
    let id = fx.d.panes.list[0].id;
    assert_eq!(Some(id), config_runs_for(&fx.d));
    config_answers(&mut fx.d);
    assert_eq!("none", fx.d.provider_shown());
    drop(helper);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_pane_provider_with_only_open_opens_a_window_unplaced() {
    // no `find`: no pane has a terminal id to open beside
    let mut fx = with_provider(None);
    fx.d.test_choose = Some(Box::new(|_| Chosen::Provider {
        held: Box::new(FakeProvider::with(&[Capability::Open])),
        spent: Duration::ZERO,
    }));
    fx.d.nvim.ready = true;
    draw_external(&mut fx.d, 2, 1000, 80, 23);
    let (_helper, _pty) = self_attach_with(&mut fx, "unplaced", &self_hello(test_terminal(80, 24)));
    let p = fx.d.panes.list.last_mut().unwrap();
    assert_eq!(Reach::In, p.reach);
    assert!(p.terminal.is_none());
    p.neovim_window_handle = Some(1000);
    fx.d.window_ask = None;
    draw_external(&mut fx.d, 3, 1001, 40, 23);
    fx.d.nvim.current_win = 1001;
    fx.d.changes.note_split(windows::Split {
        win: 1001,
        side_by_side: true,
        before: false,
        from: 1000,
        tab: 0,
    });
    fx.d.reconcile().unwrap();
    let (_, ask) = fake(&mut fx.d).asks.last().expect("an ask").clone();
    let Ask::Open { pane, .. } = ask else {
        panic!("not an open: {ask:?}");
    };
    let p = fx.d.panes.by_attach_id(&pane).unwrap();
    assert_eq!(Some(1001), p.neovim_window_handle);
    assert_eq!((0, 0), (p.tab, p.shown_tab));
    assert!(p.awaiting_size);
    assert!(p.placing.is_none());
}
