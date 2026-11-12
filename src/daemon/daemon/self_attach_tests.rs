//! Self-attaches at any time (#105), with a real Neovim and, but where a
//! test says otherwise, Rex closed (no pane provider), so windows go
//! unshown: which window each takes, in which order, and what goes when its
//! terminal does.

use super::*;
use crate::attach::{Hello, Message, Purpose};
use crate::daemon::attach_conn::{self, test_terminal};
use crate::daemon::fdpass;
use crate::daemon::provider::fake::FakeProvider;
use crate::daemon::surface::{cells, open_pty};
use crate::nvim::nvim::{spawn_for_test_with, wait_response};
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU32, Ordering};

/// A terminal: its pty's master, read until it closes, and the helper's
/// end of its socket, kept open while it is attached.
struct Term {
    helper: Option<UnixStream>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Term {
    /// A new 80 x 24 pty, its master drained; its slave, to hand over.
    fn open() -> (Term, i32) {
        let (master, slave) = open_pty(80, 24);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // SAFETY: `buf` is valid for its length.
            while unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
            // SAFETY: closing the pty's master.
            unsafe { libc::close(master) };
        });
        let term = Term {
            helper: None,
            reader: Some(reader),
        };
        (term, slave)
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.helper = None;
        // the reader ends once the daemon's copy of the slave closes, with
        // its pane: not joined, so a test that fails doesn't hang
        drop(self.reader.take());
    }
}

/// A daemon over a real Neovim, the launcher's terminal its first pane, and
/// a socket for later attaches.
struct Rig {
    d: Daemon,
    server: UnixListener,
    sock: String,
    /// Neovim's `--listen` socket, and the test's channel on it.
    listen: String,
    query: crate::nvim::nvim::Nvim,
    terms: Vec<Term>,
    /// Files the test made, removed as it ends.
    files: Vec<String>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.sock);
        let _ = std::fs::remove_file(&self.listen);
        for f in &self.files {
            let _ = std::fs::remove_file(f);
        }
    }
}

impl Rig {
    /// Neovim started with `args`, and no pane provider. None when Neovim
    /// isn't installed.
    fn start(args: &[&str]) -> Option<Rig> {
        Rig::start_with(args, None, None)
    }

    /// Neovim started with `args`, `provider` the pane provider and
    /// `terminal` its id for the first pane.
    fn start_with(
        args: &[&str],
        provider: Option<Held>,
        terminal: Option<TerminalId>,
    ) -> Option<Rig> {
        static RIGS: AtomicU32 = AtomicU32::new(0);
        let n = RIGS.fetch_add(1, Ordering::SeqCst);
        let tmp = |what: &str| {
            std::env::temp_dir()
                .join(format!(
                    "{NAME}-self-{what}-{}-{n}.sock",
                    std::process::id()
                ))
                .to_string_lossy()
                .into_owned()
        };
        let (sock, listen) = (tmp("d"), tmp("nvim"));
        let _ = std::fs::remove_file(&listen);
        let mut argv = vec![
            "nvim",
            "--clean",
            "--embed",
            "-n",
            "--cmd",
            "set noswapfile",
        ];
        argv.extend_from_slice(&["--listen", &listen]);
        argv.extend_from_slice(args);
        let nvim = spawn_for_test_with(&argv)?;
        // a channel of the test's own, for questions: its answers don't
        // take the daemon's frames
        let deadline = Instant::now() + Duration::from_secs(10);
        let query = loop {
            match UnixStream::connect(&listen) {
                Ok(s) => break crate::nvim::nvim::for_test_on(s),
                Err(e) => {
                    assert!(Instant::now() < deadline, "{e}");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        let event_loop = Loop::init().unwrap();
        event_loop
            .add_read(nvim.read_fd(), Token::NvimChannel as usize)
            .unwrap();
        let _ = std::fs::remove_file(&sock);
        let server = UnixListener::bind(&sock).unwrap();
        let mut d = Daemon::new(
            event_loop,
            NvimSession::new(nvim),
            provider,
            String::new(),
            crate::daemon::dir::Address::from_arg(sock.clone().into()),
        );
        let (mut term, slave) = Term::open();
        let (socket, helper) = UnixStream::pair().unwrap();
        term.helper = Some(helper);
        let surface = Surface::create(socket, slave, cells(80, 24));
        d.start(&mut Some(SelfAttach {
            surface,
            terminal,
            hello: None,
        }))
        .unwrap();
        Some(Rig {
            d,
            server,
            sock,
            listen,
            query,
            terms: vec![term],
            files: Vec::new(),
        })
    }

    /// `-p` with three files, named for test `tag`, holding one line each:
    /// `alpha`, `bravo`, `charlie`; and the files.
    fn three_tabs(tag: &str) -> Option<(Rig, Vec<String>)> {
        let files = three_files(tag);
        let mut args = vec!["-p"];
        args.extend(files.iter().map(String::as_str));
        let started = Rig::start(&args);
        if started.is_none() {
            remove(&files);
        }
        let mut rig = started?;
        rig.files = files.clone();
        Some((rig, files))
    }

    /// Self-attaches a new terminal, asking for `file` at `line`, through
    /// `Daemon::accept`: its pane's id.
    fn attach(&mut self, file: Option<&str>, line: Option<u32>) -> u32 {
        let (mut term, slave) = Term::open();
        let purpose = Purpose::SelfAttach {
            terminal: test_terminal(80, 24),
            provider: None,
            file: file.map(|f| f.as_bytes().to_vec()),
            line,
        };
        let hello = Message::Hello(Hello::new(purpose)).encode().unwrap();
        let helper = UnixStream::connect(&self.sock).unwrap();
        fdpass::send(&helper, &hello, slave).unwrap();
        // SAFETY: the daemon has its own copy now.
        unsafe { libc::close(slave) };
        self.d.accept(&self.server).unwrap();
        assert!(matches!(first_frame(&helper), Message::Accept(_)));
        term.helper = Some(helper);
        self.terms.push(term);
        self.d.panes.list.last().unwrap().id
    }

    /// Runs the daemon's loop, as `run` does, until `done`. Panics after 20 s.
    fn pump(&mut self, done: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut events = [Event::default(); 16];
        while !done(self) {
            assert!(Instant::now() < deadline, "TestTimedOut");
            self.d.event_loop.add_timer(100, 50, usize::MAX).unwrap();
            let n = self.d.event_loop.wait(&mut events).unwrap().len();
            for &e in &events[..n] {
                if e.token != usize::MAX {
                    self.d.handle(e).unwrap();
                }
            }
            assert!(self.d.stopped.is_none(), "{:?}", self.d.stopped);
            self.d.after_batch().unwrap();
        }
    }

    /// The window pane `id` shows, once it shows one drawn.
    fn win_of(&mut self, id: u32) -> i64 {
        self.pump(|r| {
            r.d.panes
                .by_id(id)
                .and_then(|p| p.neovim_window_handle)
                .is_some_and(|w| r.d.grids.grid_of(w).is_some())
        });
        self.d
            .panes
            .by_id(id)
            .unwrap()
            .neovim_window_handle
            .unwrap()
    }

    /// Runs Lua `code` returning a list of ints, on the test's own channel.
    fn ints(&mut self, code: &str) -> Vec<i64> {
        ints(&mut self.query, code)
    }

    /// The window showing `file`, the first if several do.
    fn win_on(&mut self, file: &str) -> i64 {
        self.ints(&format!(
            "return {{ vim.fn.win_findbuf(vim.fn.bufnr({file:?}))[1] }}"
        ))[0]
    }

    /// The windows on each of `files`, once Neovim is set up.
    fn wins_on(&mut self, files: &[String]) -> Vec<i64> {
        self.pump(|r| r.d.nvim.ready);
        files.iter().map(|f| self.win_on(f)).collect()
    }
}

/// The first frame `helper` is sent, within a second.
fn first_frame(helper: &UnixStream) -> Message {
    // fails on macOS once the daemon has hung up, its answer read below
    let _ = helper.set_read_timeout(Some(Duration::from_secs(1)));
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(m) = attach_conn::next_message(&mut got).unwrap() {
            return m;
        }
        let n = (&*helper).read(&mut buf).unwrap();
        assert!(n > 0, "hung up without an answer");
        got.extend_from_slice(&buf[..n]);
    }
}

/// Runs Lua `code`, which returns a list of ints, and waits for them.
fn ints(nvim: &mut crate::nvim::nvim::Nvim, code: &str) -> Vec<i64> {
    let id = nvim.exec_lua(code, 0);
    nvim.flush().unwrap();
    let frame = wait_response(nvim, id);
    let rpc::Message::Response {
        err, mut result, ..
    } = frame.message().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(None, err.map(String::from_utf8_lossy));
    let n = rpc::expect_array(&mut result).unwrap();
    (0..n)
        .map(|_| rpc::expect_int(&mut result).unwrap())
        .collect()
}

/// `-p` with three files, named for test `tag`, holding one line each:
/// `alpha`, `bravo`, `charlie`.
fn three_files(tag: &str) -> Vec<String> {
    ["alpha", "bravo", "charlie"]
        .iter()
        .map(|word| {
            let f = std::env::temp_dir()
                .join(format!("{NAME}-{tag}-{word}-{}", std::process::id()))
                .to_string_lossy()
                .into_owned();
            std::fs::write(&f, format!("{word}\n")).unwrap();
            f
        })
        .collect()
}

fn remove(files: &[String]) {
    for f in files {
        let _ = std::fs::remove_file(f);
    }
}

/// Neovim's current window, and the current tab page's number.
const CURRENT: &str = "return { vim.api.nvim_get_current_win(), vim.fn.tabpagenr() }";

#[test]
fn a_second_self_attach_takes_the_next_tab_pages_window_and_makes_it_current() {
    let Some((mut rig, files)) = Rig::three_tabs("order") else {
        return;
    };
    let wins = rig.wins_on(&files);
    assert_eq!(wins[0], rig.win_of(1));
    let second = rig.attach(None, None);
    assert_eq!(wins[1], rig.win_of(second));
    let third = rig.attach(None, None);
    assert_eq!(wins[2], rig.win_of(third));
    // the last becomes current, with its home tab page
    rig.pump(|r| r.d.nvim.current_win == wins[2]);
    assert_eq!(vec![wins[2], 3], rig.ints(CURRENT));
}

#[test]
fn the_current_window_is_taken_first_when_unshown() {
    let files = three_files("current");
    let Some(mut rig) = Rig::start(&["-p", &files[0], &files[1]]) else {
        remove(&files);
        return;
    };
    rig.files = files.clone();
    let wins = rig.wins_on(&files[..2]);
    rig.win_of(1);
    // a tab page made after setup: with Rex closed, its window is unshown
    let _ = rig.d.nvim.client.exec_lua("vim.cmd('tabnew')", 0);
    rig.d.nvim.client.flush().unwrap();
    rig.pump(|r| r.d.nvim.current_win != 0 && r.d.nvim.current_win != wins[0]);
    let new = rig.d.nvim.current_win;
    assert!(!wins.contains(&new));
    let second = rig.attach(None, None);
    assert_eq!(new, rig.win_of(second));
    // then the next in tab page order
    let third = rig.attach(None, None);
    assert_eq!(wins[1], rig.win_of(third));
}

#[test]
fn with_no_unshown_window_a_self_attach_opens_a_new_one() {
    let Some(mut rig) = Rig::start(&[]) else {
        return;
    };
    rig.pump(|r| r.d.nvim.ready);
    let first = rig.win_of(1);
    let second = rig.attach(None, None);
    let win = rig.win_of(second);
    assert_ne!(first, win);
    // in the same tab page, on the buffer the current window showed
    let bufs = rig.ints(&format!(
        "return {{ vim.api.nvim_win_get_buf({first}), vim.api.nvim_win_get_buf({win}), \
         vim.fn.tabpagenr('$') }}"
    ));
    assert_eq!(bufs[0], bufs[1]);
    assert_eq!(1, bufs[2]);
}

/// A file of nine numbered lines, for test `tag`.
fn nine_lines(tag: &str) -> String {
    let file = std::env::temp_dir()
        .join(format!("{NAME}-{tag}-{}", std::process::id()))
        .to_string_lossy()
        .into_owned();
    std::fs::write(&file, "1\n2\n3\n4\n5\n6\n7\n8\n9\n").unwrap();
    file
}

#[test]
fn a_file_with_a_line_opens_in_a_new_window_with_the_cursor_there() {
    let file = nine_lines("line");
    let Some(mut rig) = Rig::start(&[]) else {
        let _ = std::fs::remove_file(&file);
        return;
    };
    rig.files.push(file.clone());
    rig.pump(|r| r.d.nvim.ready);
    let first = rig.win_of(1);
    let second = rig.attach(Some(&file), Some(7));
    let win = rig.win_of(second);
    assert_ne!(first, win);
    rig.pump(|r| r.d.nvim.current_win == win);
    let at = rig.ints(&format!(
        "return {{ vim.fn.bufnr({file:?}), vim.api.nvim_win_get_buf({win}), \
         vim.api.nvim_win_get_cursor({win})[1] }}"
    ));
    assert_eq!(at[0], at[1], "not on the file");
    assert_eq!(7, at[2]);
}

#[test]
fn attaches_before_neovim_is_ready_get_windows_in_the_order_they_came() {
    let Some((mut rig, files)) = Rig::three_tabs("early") else {
        return;
    };
    // setup hasn't been answered yet
    assert!(!rig.d.nvim.ready);
    let second = rig.attach(None, None);
    let third = rig.attach(None, None);
    let got = [rig.win_of(1), rig.win_of(second), rig.win_of(third)];
    assert_eq!(rig.wins_on(&files), got);
    // the last to come is current
    rig.pump(|r| r.d.nvim.current_win == got[2]);
}

#[test]
fn a_file_attach_after_an_unshown_one_before_neovim_is_ready_waits_its_turn() {
    let file = nine_lines("turn");
    let Some((mut rig, files)) = Rig::three_tabs("turn") else {
        let _ = std::fs::remove_file(&file);
        return;
    };
    rig.files.push(file.clone());
    assert!(!rig.d.nvim.ready);
    let second = rig.attach(None, None);
    let third = rig.attach(Some(&file), None);
    let (shown, opened) = (rig.win_of(second), rig.win_of(third));
    let wins = rig.wins_on(&files);
    assert_eq!(wins[1], shown);
    assert_eq!(rig.win_on(&file), opened);
    // each was made current in turn: the file's last
    rig.pump(|r| r.d.nvim.current_win == opened);
    assert_eq!(opened, rig.ints(CURRENT)[0]);
}

#[test]
fn a_window_whose_requested_pane_is_on_its_way_isnt_taken() {
    // a pane provider that never answers its opens
    let fake = Held::Fake(FakeProvider::opening());
    let Some(mut rig) = Rig::start_with(&[], Some(fake), Some(TerminalId::from("t0"))) else {
        return;
    };
    rig.pump(|r| r.d.nvim.ready);
    let first = rig.win_of(1);
    let _ = rig.d.nvim.client.exec_lua("vim.cmd('vsplit')", 0);
    rig.d.nvim.client.flush().unwrap();
    rig.pump(|r| r.d.panes.list.iter().any(|p| !p.is_self_attached()));
    let requested = rig.d.panes.list.iter().find(|p| !p.is_self_attached());
    let split = requested.unwrap().neovim_window_handle.unwrap();
    assert!(matches!(
        rig.d.provider.as_mut().unwrap().fake().asked()[..],
        [Ask::OpenBeside { .. }]
    ));
    let second = rig.attach(None, None);
    let win = rig.win_of(second);
    assert!(win != split && win != first, "{win}");
}

#[test]
fn closing_a_self_attached_terminal_closes_only_its_windows() {
    let Some((mut rig, files)) = Rig::three_tabs("close") else {
        return;
    };
    let wins = rig.wins_on(&files);
    rig.win_of(1);
    let second = rig.attach(None, None);
    rig.win_of(second);
    let third = rig.attach(None, None);
    rig.win_of(third);
    // the second terminal goes
    rig.terms[1].helper = None;
    rig.pump(|r| r.d.panes.by_id(second).is_none());
    rig.pump(|r| r.d.grids.grid_of(wins[1]).is_none());
    let valid = rig.ints(&format!(
        "return vim.tbl_map(function(w) return vim.api.nvim_win_is_valid(w) and 1 or 0 end, \
         {{ {}, {}, {} }})",
        wins[0], wins[1], wins[2]
    ));
    assert_eq!(vec![1, 0, 1], valid);
    assert!(rig.d.panes.by_id(1).is_some() && rig.d.panes.by_id(third).is_some());
    assert_eq!(None, rig.d.stopped);
}

/// A self-attach's hello from a terminal on `tty` that inherited the
/// daemon's address, as one started in its `:terminal` does.
fn hello_with_address(rig: &Rig, tty: &[u8]) -> Vec<u8> {
    let mut terminal = test_terminal(80, 24);
    terminal.env = vec![(
        crate::name::upper().into_bytes(),
        rig.sock.as_bytes().to_vec(),
    )];
    terminal.tty = tty.to_vec();
    let purpose = Purpose::SelfAttach {
        terminal,
        provider: None,
        file: None,
        line: None,
    };
    Message::Hello(Hello::new(purpose)).encode().unwrap()
}

#[test]
fn an_attach_from_neovims_own_terminal_is_refused_and_one_from_a_pty_inside_it_isnt() {
    let Some(mut rig) = Rig::start(&[]) else {
        return;
    };
    rig.pump(|r| r.d.nvim.ready);
    rig.win_of(1);
    let _ = rig.d.nvim.client.exec_lua("vim.cmd('terminal')", 0);
    rig.d.nvim.client.flush().unwrap();
    // its pty, once it's running
    let pty = loop {
        let id = rig.query.exec_lua(
            "for _, c in ipairs(vim.api.nvim_list_chans()) do \
               if c.pty and c.pty ~= '' then return c.pty end \
             end \
             return ''",
            0,
        );
        rig.query.flush().unwrap();
        let frame = wait_response(&mut rig.query, id);
        let rpc::Message::Response { mut result, .. } = frame.message().unwrap() else {
            unreachable!()
        };
        let pty = rpc::expect_str(&mut result).unwrap().to_vec();
        if !pty.is_empty() {
            break pty;
        }
        rig.pump(|_| true);
    };
    // from the terminal itself, and from a pty a tmux in it would make
    for (tty, nested) in [(pty, true), (b"/dev/ttys999".to_vec(), false)] {
        let (_term, slave) = Term::open();
        let helper = UnixStream::connect(&rig.sock).unwrap();
        fdpass::send(&helper, &hello_with_address(&rig, &tty), slave).unwrap();
        // SAFETY: the daemon has its own copy now.
        unsafe { libc::close(slave) };
        rig.d.accept(&rig.server).unwrap();
        rig.pump(|r| r.d.nested_checks.is_empty());
        match first_frame(&helper) {
            Message::End(z) if nested => {
                assert_eq!(Some(crate::attach::Refusal::Nested), z.as_refusal())
            }
            Message::Accept(_) if !nested => {}
            other => panic!("{nested}: {other:?}"),
        }
    }
}

#[test]
fn a_new_windows_pane_holds_it_before_an_autocommand_can_redraw() {
    // a config that redraws as windows are made and entered
    let redraw = "autocmd WinNew,WinEnter,BufEnter,BufWinEnter * redraw";
    let Some(mut rig) = Rig::start(&["--cmd", redraw]) else {
        return;
    };
    rig.pump(|r| r.d.nvim.ready);
    rig.win_of(1);
    let second = rig.attach(None, None);
    let win = rig.win_of(second);
    // never drawn in the first pane's layout, nor given a pane of its own
    assert_eq!(2, rig.d.panes.list.len());
    assert!(rig.d.panes.by_id(1).unwrap().layout.is_none());
    assert_eq!(Some(second), rig.d.panes.by_win(win).map(|p| p.id));
}
