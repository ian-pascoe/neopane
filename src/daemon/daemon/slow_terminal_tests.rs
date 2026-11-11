//! A pane whose terminal nobody reads, as Rex may leave a hidden or throttled
//! one, beside a pane that is read: a real Neovim, a daemon on this thread,
//! and threads playing the two terminal emulators. Each test runs under
//! both transports: over a pty handed over, and relayed with no pty.

use super::*;
use crate::attach::{Frame, Message, Transport};
use crate::daemon::attach_conn;
use crate::daemon::fdpass;
use crate::daemon::provider::fake::FakeProvider;
use crate::daemon::surface::{cells, open_pty};
use crate::nvim::nvim::spawn_for_test_with;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

const TERMINAL: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

/// The timer that bounds each of the rig's waits on the loop, so it checks
/// whether it's done: its id, and the token it fires with.
const PUMP_TIMER_ID: usize = 100;
const PUMP_TIMER_TOKEN: usize = usize::MAX;

/// How long the unread pane is held before the daemon counts as stalled.
const STALL_LIMIT: Duration = Duration::from_secs(3);

/// What an emulator reads a pane's output from, and types into.
enum End {
    /// A pty's master.
    Pty(RawFd),
    /// The helper's end of a relayed pane's socket: `o` frames out, `i`
    /// frames in.
    Relay(UnixStream),
}

impl End {
    fn fd(&self) -> RawFd {
        match self {
            End::Pty(master) => *master,
            End::Relay(s) => s.as_raw_fd(),
        }
    }
}

/// A terminal emulator for one pane: reads what it is sent into `seen`,
/// except while `held`.
struct Emulator {
    end: Arc<End>,
    seen: Arc<Mutex<Vec<u8>>>,
    held: Arc<AtomicBool>,
    /// Set once the daemon was blocked past `STALL_LIMIT` by this terminal
    /// being held. Holding stops then, so the test ends instead of hanging.
    stalled: Arc<AtomicBool>,
    /// Set as the rig ends.
    done: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Emulator {
    fn start(end: End) -> Emulator {
        let end = Arc::new(end);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::new(AtomicBool::new(false));
        let stalled = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, held, stalled, done) =
                (seen.clone(), held.clone(), stalled.clone(), done.clone());
            let end = end.clone();
            std::thread::spawn(move || {
                let master = end.fd();
                let mut held_since: Option<Instant> = None;
                let mut buf = [0u8; 4096];
                // relayed bytes not yet a whole frame
                let mut framed = Vec::new();
                while !done.load(Ordering::SeqCst) {
                    if held.load(Ordering::SeqCst) {
                        let since = *held_since.get_or_insert_with(Instant::now);
                        if since.elapsed() > STALL_LIMIT && !stalled.load(Ordering::SeqCst) {
                            stalled.store(true, Ordering::SeqCst);
                            held.store(false, Ordering::SeqCst);
                        }
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    held_since = None;
                    let mut fds = [libc::pollfd {
                        fd: master,
                        events: libc::POLLIN,
                        revents: 0,
                    }];
                    // SAFETY: one valid pollfd.
                    if unsafe { libc::poll(fds.as_mut_ptr(), 1, 20) } <= 0 {
                        continue;
                    }
                    // SAFETY: `buf` is valid for its length.
                    let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                    if n <= 0 {
                        // the slave closed: nothing more to read
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    let got = &buf[..n as usize];
                    match *end {
                        End::Pty(_) => seen.lock().unwrap().extend_from_slice(got),
                        End::Relay(_) => {
                            framed.extend_from_slice(got);
                            while let Some((f, n)) = Frame::decode(&framed).unwrap() {
                                framed.drain(..n);
                                if let Some(Message::Output(o)) = Message::decode(&f).unwrap() {
                                    seen.lock().unwrap().extend_from_slice(&o);
                                }
                            }
                        }
                    }
                }
            })
        };
        Emulator {
            end,
            seen,
            held,
            stalled,
            done,
            thread: Some(thread),
        }
    }

    fn seen(&self) -> String {
        self.seen_from(0)
    }

    /// What the pane was sent from byte `at` on.
    fn seen_from(&self, at: usize) -> String {
        String::from_utf8_lossy(&self.seen.lock().unwrap()[at..]).into_owned()
    }

    fn seen_len(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Types `text` into the pane.
    fn type_text(&self, text: &str) {
        match &*self.end {
            End::Pty(master) => {
                // SAFETY: `text` is valid for its length.
                let n = unsafe { libc::write(*master, text.as_ptr().cast(), text.len()) };
                assert_eq!(text.len() as isize, n);
            }
            End::Relay(s) => {
                let frame = Message::Input(text.as_bytes().to_vec()).encode().unwrap();
                std::io::Write::write_all(&mut &*s, &frame).unwrap();
            }
        }
    }
}

impl Drop for Emulator {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let End::Pty(master) = *self.end {
            // SAFETY: the emulator owns the master.
            unsafe { libc::close(master) };
        }
    }
}

/// A daemon over a real Neovim and a pane provider that answers nothing, with the
/// startup pane (`panes[0]`) and a pane for a split beside it (`panes[1]`).
struct Rig {
    /// Dropped first: its panes are hung up while the emulators still read.
    d: Daemon,
    panes: Vec<Emulator>,
    /// The helpers' ends of the panes' sockets: kept open, or the panes close.
    helper_ends: Vec<UnixStream>,
    sock: String,
}

impl Drop for Rig {
    /// Lets every terminal be read again, so nothing is left held.
    fn drop(&mut self) {
        for e in &self.panes {
            e.held.store(false, Ordering::SeqCst);
        }
        let _ = std::fs::remove_file(&self.sock);
    }
}

/// Makes a relayed pane's socket buffer `opt` small, as a pty's is, so a
/// frame of the startup pane is more than it holds: the sender's counts on
/// Linux, the receiver's on macOS.
fn small_buffer(s: &UnixStream, opt: libc::c_int) {
    let size: libc::c_int = 4096;
    // SAFETY: a valid int option of the size given.
    let r = unsafe {
        libc::setsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            opt,
            (&size as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    assert_eq!(0, r);
}

impl Rig {
    /// Its panes' terminals attached over `transport`. None when Neovim
    /// isn't installed.
    fn start(transport: Transport) -> Option<Rig> {
        let nvim = spawn_for_test_with(&[
            "nvim",
            "--clean",
            "--embed",
            "-n",
            "--cmd",
            "set noswapfile",
        ])?;
        let event_loop = Loop::init().unwrap();
        event_loop
            .add_read(nvim.read_fd(), Token::NvimChannel as usize)
            .unwrap();
        let mut d = Daemon::new(
            event_loop,
            NvimSession::new(nvim),
            Some(Held::Fake(FakeProvider::opening())),
            String::new(),
            crate::daemon::dir::Address::from_arg(Default::default()),
        );

        // the startup pane: big, so a frame of it is bigger than a pty, or
        // a socket made small, holds
        let (socket0, helper0) = UnixStream::pair().unwrap();
        let (surface0, pane0) = match transport {
            Transport::Fd => {
                let (master0, slave0) = open_pty(200, 60);
                let surface = Surface::create(socket0, slave0, cells(200, 60));
                (surface, Emulator::start(End::Pty(master0)))
            }
            Transport::Relay => {
                socket0.set_nonblocking(true).unwrap();
                small_buffer(&socket0, libc::SO_SNDBUF);
                small_buffer(&helper0, libc::SO_RCVBUF);
                let surface = Surface::relayed(socket0, cells(200, 60));
                (
                    surface,
                    Emulator::start(End::Relay(helper0.try_clone().unwrap())),
                )
            }
        };
        let startup = Pane::startup(Some(TerminalId::from(TERMINAL)), surface0);
        d.start(startup).unwrap();

        // one per rig: the tests run side by side
        static RIGS: AtomicU32 = AtomicU32::new(0);
        let n = RIGS.fetch_add(1, Ordering::SeqCst);
        let sock = std::env::temp_dir()
            .join(format!("{NAME}-slow-{}-{n}.sock", std::process::id()))
            .to_string_lossy()
            .into_owned();
        let _ = std::fs::remove_file(&sock);
        let server = UnixListener::bind(&sock).unwrap();
        let mut rig = Rig {
            d,
            panes: vec![pane0],
            helper_ends: vec![helper0],
            sock,
        };
        rig.pump(|r| r.d.nvim.ready);

        // a split: Neovim's new window asks for a pane, whose helper connects
        // as its terminal would run it
        rig.lua("vim.cmd('vsplit | enew')");
        rig.pump(|r| r.d.panes.by_id(1).is_some());
        let helper = UnixStream::connect(&rig.sock).unwrap();
        let id = rig.d.panes.by_id(1).unwrap().attach_id.unwrap();
        let mut terminal = attach_conn::test_terminal(100, 30);
        terminal.transport = transport;
        let hello = attach_conn::requested_hello(id, terminal);
        let emulator = match transport {
            Transport::Fd => {
                let (master1, slave1) = open_pty(100, 30);
                fdpass::send(&helper, &hello, slave1).unwrap();
                // SAFETY: the daemon has its own copy now.
                unsafe { libc::close(slave1) };
                Emulator::start(End::Pty(master1))
            }
            Transport::Relay => {
                small_buffer(&helper, libc::SO_RCVBUF);
                std::io::Write::write_all(&mut &helper, &hello).unwrap();
                Emulator::start(End::Relay(helper.try_clone().unwrap()))
            }
        };
        rig.helper_ends.push(helper);
        rig.panes.push(emulator);
        rig.d.accept(&server).unwrap();
        if transport == Transport::Relay {
            small_buffer(&rig.surface(1).helper_socket, libc::SO_SNDBUF);
        }
        rig.pump(|r| !r.panes[1].seen().is_empty());
        Some(rig)
    }

    /// Runs the daemon's loop, as `run` does, until `done`. Panics after 20 s.
    fn pump(&mut self, done: impl Fn(&Rig) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut events = [Event::default(); 16];
        while !done(self) {
            assert!(Instant::now() < deadline, "TestTimedOut");
            self.d
                .event_loop
                .add_timer(PUMP_TIMER_ID, 50, PUMP_TIMER_TOKEN)
                .unwrap();
            let n = self.d.event_loop.wait(&mut events).unwrap().len();
            for &e in &events[..n] {
                if e.token != PUMP_TIMER_TOKEN {
                    self.d.handle(e).unwrap();
                }
            }
            assert!(self.d.stopped.is_none(), "{:?}", self.d.stopped);
            self.d.after_batch().unwrap();
        }
    }

    /// Runs `code` in Neovim, without waiting for its answer.
    fn lua(&mut self, code: &str) {
        let _ = self.d.nvim.client.exec_lua(code, 0);
        self.d.nvim.client.flush().unwrap();
    }

    /// The window of pane `id`.
    fn win(&self, id: u32) -> i64 {
        self.d
            .panes
            .by_id(id)
            .unwrap()
            .neovim_window_handle
            .unwrap()
    }

    fn surface(&mut self, id: u32) -> &mut Surface {
        self.d
            .panes
            .by_id_mut(id)
            .and_then(|p| p.terminal_surface.as_deref_mut())
            .unwrap()
    }

    /// The text of row `row` of pane `id`'s window, as the daemon has it.
    fn row_text(&self, id: u32, row: u32) -> String {
        let grid = self.d.grids.grid_of(self.win(id)).unwrap();
        self.d.grids.get(grid).unwrap().row_text(row)
    }

    /// Redraws pane `id`'s window `frames` times over, every cell changing
    /// each time: far more than a pty holds. Returns once the daemon has the
    /// last frame, whose rows are `Z`s, `A`s, `B`s and so on, unwrapped.
    fn churn(&mut self, id: u32, frames: u32) {
        let code = format!(
            "local w = {}\n\
             local b = vim.api.nvim_win_get_buf(w)\n\
             vim.wo[w].wrap = false\n\
             for i = 1, {frames} do\n\
             \x20 local l = {{}}\n\
             \x20 for r = 1, 100 do l[r] = string.rep(string.char(65 + ({frames} - i + r + 24) % 26), 300) end\n\
             \x20 vim.api.nvim_buf_set_lines(b, 0, -1, false, l)\n\
             \x20 vim.cmd('redraw')\n\
             end",
            self.win(id)
        );
        self.lua(&code);
        self.pump(|r| r.row_text(id, 0).starts_with("ZZZ"));
    }
}

// The reproduction for #39. Pane 0's terminal isn't read while Neovim
// redraws it many times over; typing into pane 1 must still reach Neovim, and
// its echo pane 1, within `STALL_LIMIT`.
fn a_pane_nobody_reads_doesnt_stop_another_pane_or_its_input(transport: Transport) {
    let Some(mut rig) = Rig::start(transport) else {
        return;
    };
    rig.panes[0].held.store(true, Ordering::SeqCst);
    rig.churn(0, 10);
    rig.panes[1].type_text("ihello");
    rig.pump(|r| r.panes[1].seen().contains("hello"));
    // (had it stalled, its emulator would have let it go on, to end the test)
    assert!(
        !rig.panes[0].stalled.load(Ordering::SeqCst),
        "the daemon waited for the unread pane"
    );
}

/// The start and end of each frame the daemon draws.
const FRAME_START: &str = "\x1b[?2026h";
const FRAME_END: &str = "\x1b[?2026l";

/// The last whole frame in `shown`, if it has one.
fn last_frame(shown: &str) -> Option<&str> {
    let end = shown.rfind(FRAME_END)?;
    let start = shown[..end].rfind(FRAME_START)?;
    Some(&shown[start..end])
}

/// The row of `cols` letters `Rig::churn`'s last frame has at row `r`.
fn churned_row(r: u32, cols: usize) -> String {
    char::from(b'A' + ((r + 25) % 26) as u8)
        .to_string()
        .repeat(cols)
}

/// The text `frame` draws at row `r`, from its left edge: what follows the
/// cursor's move there, and a change of colors, up to the next sequence.
fn row_at(frame: &str, r: u32) -> Option<&str> {
    let at = format!("\x1b[{};1H", r + 1);
    let mut rest = &frame[frame.find(&at)? + at.len()..];
    if rest.starts_with("\x1b[0") {
        rest = &rest[rest.find('m')? + 1..];
    }
    Some(&rest[..rest.find('\x1b').unwrap_or(rest.len())])
}

// An unread pane's waiting output is capped: the frames past it are dropped,
// and once the terminal reads again the pane is drawn whole, in one frame.
fn a_pane_whose_frames_were_dropped_is_drawn_in_full_once_it_reads_again(transport: Transport) {
    let Some(mut rig) = Rig::start(transport) else {
        return;
    };
    rig.surface(0).backlog.cap = 16 * 1024;
    let msg_rows = rig.d.msg_rows;
    // the window's text, over its status line
    let rows = rig.surface(0).window_size(msg_rows)[1] - 1;
    rig.panes[0].held.store(true, Ordering::SeqCst);
    let before = rig.panes[0].seen_len();
    rig.churn(0, 10);
    rig.panes[0].held.store(false, Ordering::SeqCst);
    // the second row has `A`s in the last of them only
    let second = churned_row(1, 200);
    rig.pump(|r| {
        last_frame(&r.panes[0].seen_from(before)).and_then(|f| row_at(f, 1)) == Some(&second)
    });

    let shown = rig.panes[0].seen_from(before);
    let frames = shown.matches(FRAME_START).count();
    assert!(frames < 10, "{frames} frames: none dropped");
    // the screen cleared, then every row
    let last = last_frame(&shown).unwrap();
    assert!(last.contains("\x1b[2J"), "{last:?}");
    for r in 0..rows {
        assert_eq!(
            Some(churned_row(r, 200).as_str()),
            row_at(last, r),
            "row {r}"
        );
    }
    assert!(!rig.panes[0].stalled.load(Ordering::SeqCst));
}

// Closing a pane writes nothing more to its terminal (its helper restores
// it), so one nobody reads mustn't hold the daemon up.
fn a_pane_nobody_reads_closes_without_stopping_the_others(transport: Transport) {
    let Some(mut rig) = Rig::start(transport) else {
        return;
    };
    rig.panes[1].held.store(true, Ordering::SeqCst);
    rig.churn(1, 20);
    let win = rig.win(1);
    rig.lua(&format!("vim.api.nvim_win_close({win}, true)"));
    rig.pump(|r| r.d.panes.by_id(1).is_none());
    let before = rig.panes[0].seen_len();
    rig.panes[0].type_text("ihello");
    rig.pump(|r| r.panes[0].seen_from(before).contains("hello"));
    assert!(
        !rig.panes[1].stalled.load(Ordering::SeqCst),
        "the daemon waited for the closing pane"
    );
}

/// Runs each test under both transports, as `<test>::fd` and
/// `<test>::relay`.
macro_rules! over_both_transports {
    ($($test:ident),*) => {$(
        mod $test {
            use super::*;

            #[test]
            fn fd() {
                super::$test(Transport::Fd);
            }

            #[test]
            fn relay() {
                super::$test(Transport::Relay);
            }
        }
    )*};
}

over_both_transports!(
    a_pane_nobody_reads_doesnt_stop_another_pane_or_its_input,
    a_pane_whose_frames_were_dropped_is_drawn_in_full_once_it_reads_again,
    a_pane_nobody_reads_closes_without_stopping_the_others
);
