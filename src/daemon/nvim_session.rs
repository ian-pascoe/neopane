//! Our side of the Neovim connection: setup requests, the current window,
//! and what Neovim was last told about focus.
//!
//! The session owns its `Nvim` (as `rex::session::Session` owns its `Client`),
//! and the daemon reaches it as `session.client`.

use std::io;

use super::focus_gate::{AimForwarder, FocusGate};
use super::pane::Pane;
use crate::nvim::forwarder::{self, Taking};
use crate::nvim::msgpack::Decoder;
use crate::nvim::nvim::{Nvim, UiOptions};
use crate::nvim::windows;

/// How many float windows that have been current are remembered.
const MAX_ENTERED: usize = 32;

pub(crate) struct NvimSession {
    pub client: Nvim,
    /// Neovim's current window, as the Lua last said; 0 before setup.
    pub current_win: i64,
    /// Our window switches, and the input typed after them.
    pub gate: FocusGate,
    /// The switch the timer is for, see `watch_switch`.
    pub watched_switch: Option<u32>,
    pub channel_req: u32,
    pub setup_req: u32,
    /// Setup answered: windows can be adopted and given panes.
    pub ready: bool,
    /// Whether some pane's terminal has focus, as Neovim was last told.
    pub ui_focused: bool,
    /// Neovim's current window when it's a float, else 0.
    pub current_float: i64,
    /// The editor size last asked for.
    pub editor_size: [u32; 2],
    /// The tabline width Neovim was last told (see `Geometry::tabline_width`);
    /// None, as it starts, while the tabline hides nothing.
    pub tabline_width: Option<u32>,
    /// Float windows that have been current, newest last; the oldest drop
    /// off. Neovim never reuses a handle, so closed ones do no harm.
    pub entered_floats: [i64; MAX_ENTERED],
    pub entered_len: u8,
    /// The screen floats of the float group panes' groups, a list per
    /// group, as Neovim was last told: they move into whichever tab page is
    /// current.
    pub carried: Vec<Vec<i64>>,
    /// The windows panes show, sorted, as Neovim was last told, and room for
    /// the next list.
    pub shown: Vec<i64>,
    shown_next: Vec<i64>,
    /// The windows of each pane holding several tab pages, as Neovim was
    /// last told.
    pub held: Vec<Vec<i64>>,
}

impl NvimSession {
    /// A session on `client`, before setup.
    pub(crate) fn new(client: Nvim) -> NvimSession {
        NvimSession {
            client,
            current_win: 0,
            gate: FocusGate::default(),
            watched_switch: None,
            channel_req: 0,
            setup_req: 0,
            ready: false,
            ui_focused: true,
            current_float: 0,
            editor_size: [0, 0],
            tabline_width: None,
            entered_floats: [0; MAX_ENTERED],
            entered_len: 0,
            carried: Vec::new(),
            shown: Vec::new(),
            shown_next: Vec::new(),
            held: Vec::new(),
        }
    }

    /// Float `win` became current: its group may go native.
    pub(crate) fn note_entered(&mut self, win: i64) {
        if self.entered_floats().contains(&win) {
            return;
        }
        if self.entered_len as usize == self.entered_floats.len() {
            let len = self.entered_len as usize;
            self.entered_floats.copy_within(1..len, 0);
            self.entered_len -= 1;
        }
        self.entered_floats[self.entered_len as usize] = win;
        self.entered_len += 1;
    }

    pub(crate) fn entered_floats(&self) -> &[i64] {
        &self.entered_floats[0..self.entered_len as usize]
    }

    /// Makes `win`, the window of pane `pane`, Neovim's current window: the
    /// pane has focus. Input sent from here on goes to it, once Neovim has
    /// switched.
    pub(crate) fn make_current(&mut self, pane: u32, win: i64) -> io::Result<()> {
        self.current_win = win;
        self.gate.switch_to(&mut self.client, win, pane)
    }

    /// Makes `win`, a float in floating pane `pane`, current. Doesn't touch
    /// `current_win`, which floats don't change. Input waits as for `make_current`.
    pub(crate) fn switch_to_float(&mut self, pane: u32, win: i64) -> io::Result<()> {
        self.gate.switch_to(&mut self.client, win, pane)
    }

    /// What in `bytes`, read from pane `pane`, whose window is `win`, would
    /// make it current, and have the pane provider focus it: nothing if it
    /// already is, or the pane provider has focused it there
    /// (`focused_there`), else what was typed or clicked into it. Scrolling
    /// it, or moving the pointer over it, doesn't: those reach its window
    /// through `type_into` all the same, by its grid, without a switch.
    /// `panes` is as for `type_into`. (`&mut`: aiming a forwarder sets its
    /// grid.)
    pub(crate) fn takes_focus(
        &self,
        panes: &mut impl AimForwarder,
        pane: u32,
        win: Option<i64>,
        focused_there: bool,
        bytes: &[u8],
    ) -> Taking {
        let Some(w) = win else {
            return Taking::Nothing;
        };
        if w == self.current_win || focused_there {
            return Taking::Nothing;
        }
        let Some(fwd) = panes.aim_forwarder(pane) else {
            return Taking::Nothing;
        };
        fwd.taking(bytes)
    }

    /// As `type_into_at`, for a read that came now.
    #[cfg(test)]
    pub(crate) fn type_into(
        &mut self,
        panes: &mut impl AimForwarder,
        pane: u32,
        bytes: &[u8],
    ) -> forwarder::Result<()> {
        self.gate.send(&mut self.client, panes, pane, bytes)
    }

    /// Sends `bytes`, typed into pane `pane` and read at `at`, to Neovim
    /// after the window switches before it. `panes.aim_forwarder(pane)` finds
    /// the pane's `Forwarder`.
    pub(crate) fn type_into_at(
        &mut self,
        panes: &mut impl AimForwarder,
        pane: u32,
        bytes: &[u8],
        at: std::time::Instant,
    ) -> forwarder::Result<()> {
        self.gate.send_at(&mut self.client, panes, pane, bytes, at)
    }

    /// The timeout for an ESC pane `pane` read last: sends it as the Escape
    /// key (see `Forwarder::flush_pending`). False, doing nothing, while a
    /// window switch is in flight: the ESC may still begin a sequence that is
    /// held behind it, so try again later.
    pub(crate) fn flush_pending(
        &mut self,
        panes: &mut impl AimForwarder,
        pane: u32,
    ) -> forwarder::Result<bool> {
        if self.gate.in_flight().is_some() {
            return Ok(false);
        }
        if let Some(fwd) = panes.aim_forwarder(pane) {
            fwd.flush_pending(&mut self.client)?;
        }
        Ok(true)
    }

    /// Neovim answered request `id`, with error `err` or else `result`: ours
    /// may let held input go on. (See
    /// `FocusGate::answered`.)
    pub(crate) fn answered(
        &mut self,
        panes: &mut impl AimForwarder,
        id: u32,
        err: Option<&[u8]>,
        result: Decoder<'_>,
    ) -> forwarder::Result<()> {
        self.gate.answered(&mut self.client, panes, id, err, result)
    }

    /// The window switch Neovim hasn't answered yet, if any. Until it does,
    /// what Neovim says about its current window is out of date.
    pub(crate) fn switch_in_flight(&self) -> Option<u32> {
        self.gate.in_flight()
    }

    /// Whether to start the timer for the switch in flight: it's a new one.
    /// Then `recheck_switch` is due after a while.
    pub(crate) fn watch_switch(&mut self) -> bool {
        let Some(id) = self.gate.in_flight() else {
            return false;
        };
        if self.watched_switch == Some(id) {
            return false;
        }
        self.watched_switch = Some(id);
        true
    }

    /// The timer fired. A switch still unanswered may be stuck behind a command
    /// waiting for a key, so asks Neovim whether it is, which a busy one won't
    /// answer, and lets held Ctrl-Cs go if it looks busy. A newer switch than
    /// the one watched only starts its wait. True to start the timer again.
    /// `panes` is as for `type_into`.
    pub(crate) fn recheck_switch(
        &mut self,
        panes: &mut impl AimForwarder,
    ) -> forwarder::Result<bool> {
        let Some(id) = self.gate.in_flight() else {
            return Ok(false);
        };
        if self.watched_switch != Some(id) {
            self.watched_switch = Some(id);
            return Ok(true);
        }
        self.gate.probe_again(&mut self.client, panes)?;
        Ok(true)
    }

    /// Attaches as Neovim's UI at `cols` x `rows` and asks for our channel.
    pub(crate) fn attach_ui(&mut self, cols: u32, rows: u32) -> io::Result<()> {
        self.client.ui_attach(
            cols,
            rows,
            UiOptions {
                ext_termcolors: true,
                ext_multigrid: true,
            },
        )?;
        self.channel_req = windows::ask_channel(&mut self.client);
        self.client.flush()
    }

    /// Sizes Neovim's screen to current pane `p`'s layout area, in `p`'s
    /// cells (ADR 0008), so screen floats such as pickers can span it; only
    /// when that size changes, and not while `p` is in no layout area. The
    /// command line and messages show in `p`. Not before setup: the first
    /// window still fills the screen, and growing the screen to the layout
    /// area then shrinking the window to its pane leaves it scrolled, as a
    /// dashboard drawn for the bigger size would be.
    pub(crate) fn fit_ui_to(&mut self, p: &Pane) -> io::Result<()> {
        if !self.ready {
            return Ok(());
        }
        // while it's in no layout area, the screen keeps its size
        let Some(size) = super::geometry::area_size(p) else {
            return Ok(());
        };
        if size == self.editor_size {
            return Ok(());
        }
        self.editor_size = size;
        self.client.ui_try_resize(size[0], size[1])
    }

    /// Tells Neovim `groups`, the screen floats of each float group pane's
    /// group, if they changed: each, with the floats hanging from it, moves
    /// into whichever tab page is current. The caller flushes.
    pub(crate) fn tell_carried(&mut self, groups: Vec<Vec<i64>>) {
        if !self.ready || groups == self.carried {
            return;
        }
        windows::carry(&mut self.client, &groups);
        self.carried = groups;
    }

    /// Tells Neovim `held`, the windows of each pane holding several tab
    /// pages, if they changed: `gt` goes to a tab page's window last current
    /// in the current pane. The caller flushes.
    pub(crate) fn tell_panes_hold(&mut self, held: Vec<Vec<i64>>) {
        if !self.ready || held == self.held {
            return;
        }
        windows::panes_hold(&mut self.client, &held);
        self.held = held;
    }

    /// Tells Neovim the windows panes show, which `fill` adds to a list, if
    /// they changed: `<C-w>w` steps only through those. The caller flushes.
    pub(crate) fn tell_panes_show(&mut self, fill: impl FnOnce(&mut Vec<i64>)) {
        let mut next = std::mem::take(&mut self.shown_next);
        next.clear();
        fill(&mut next);
        next.sort_unstable();
        next.dedup();
        if !self.ready || next == self.shown {
            self.shown_next = next;
            return;
        }
        windows::panes_show(&mut self.client, &next);
        self.shown_next = std::mem::replace(&mut self.shown, next);
    }

    /// Tells Neovim the tabline width `cols` if it changed, so its tabline
    /// fits the tabline panes. The caller flushes.
    pub(crate) fn tell_tabline_width(&mut self, cols: Option<u32>) {
        if !self.ready || cols == self.tabline_width {
            return;
        }
        self.tabline_width = cols;
        windows::set_tabline_width(&mut self.client, cols);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::focus_gate::{Item, is_blocking};
    use crate::name::NAME;
    use crate::nvim::forwarder::Forwarder;
    use crate::nvim::msgpack;
    use crate::nvim::nvim::{for_test_on, spawn_for_test_with};
    use crate::nvim::rpc;
    use std::time::Duration;

    #[test]
    fn entered_floats_are_kept_once_and_the_oldest_drop_off() {
        // nothing here talks to Neovim
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut s = NvimSession::new(for_test_on(ours));
        s.note_entered(1000);
        s.note_entered(1000);
        assert_eq!(&[1000], s.entered_floats());
        for i in 0..s.entered_floats.len() {
            s.note_entered(2000 + i as i64);
        }
        assert_eq!(s.entered_floats.len(), s.entered_floats().len());
        assert_eq!(2000, s.entered_floats()[0]);
        assert_eq!(2031, s.entered_floats()[31]);
    }

    /// The rig's two forwarders, the panes' with ids 0, for the first window,
    /// and 1. (A field of their own, so the session can be borrowed beside
    /// them.)
    struct Fwds([Forwarder; 2]);

    impl AimForwarder for Fwds {
        fn aim_forwarder(&mut self, id: u32) -> Option<&mut Forwarder> {
            self.0.get_mut(id as usize)
        }
    }

    /// Neovim with two side-by-side windows, and a session on it as the daemon
    /// has: typing and focus changes go through its methods.
    struct Rig {
        session: NvimSession,
        fwds: Fwds,
        /// Each showing its own buffer: `one` and `two`. The second is current.
        wins: [i64; 2],
    }

    #[derive(Default)]
    struct Reply {
        /// An owned copy of a string result.
        text: Option<String>,
        blocking: bool,
    }

    impl Rig {
        /// None when Neovim isn't installed.
        fn start() -> Option<Rig> {
            let nvim = spawn_for_test_with(&[
                "nvim",
                "--clean",
                "--embed",
                "-n",
                "--cmd",
                "set noswapfile",
            ])?;
            let fwd = || Forwarder {
                ctrl_q_quits: false,
                send_focus: false,
                ..Forwarder::default()
            };
            let mut r = Rig {
                session: NvimSession::new(nvim),
                fwds: Fwds([fwd(), fwd()]),
                wins: [0, 0],
            };
            r.session
                .client
                .ui_attach(40, 10, UiOptions::default())
                .unwrap();
            // windows.lua's go, which the session's switches call: these
            // windows have no tab pages to switch
            let _ = r.lua(&format!(
                "_G.{NAME} = {{ go = vim.api.nvim_set_current_win }} return ''"
            ));
            let ids = r.lua(
                "vim.bo.swapfile = false -- killed with changes, Neovim leaves swap files behind\n\
                 vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'one' })\n\
                 local first = vim.api.nvim_get_current_win()\n\
                 vim.cmd('vsplit | enew')\n\
                 vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'two' })\n\
                 return first .. ',' .. vim.api.nvim_get_current_win()",
            );
            let mut it = ids.split(',');
            for w in r.wins.iter_mut() {
                *w = it.next().unwrap().parse().unwrap();
            }
            r.session.current_win = r.wins[1];
            Some(r)
        }

        /// As `start`, with `windows.lua` running, as the daemon has it, and
        /// two tab pages: the first holds pane 0's window, showing `one`, the
        /// second pane 1's, current.
        fn with_tab_pages() -> Option<Rig> {
            let nvim = spawn_for_test_with(&["nvim", "--clean", "--embed", "-n"])?;
            let fwd = || Forwarder {
                ctrl_q_quits: false,
                send_focus: false,
                ..Forwarder::default()
            };
            let mut r = Rig {
                session: NvimSession::new(nvim),
                fwds: Fwds([fwd(), fwd()]),
                wins: [0, 0],
            };
            r.session.attach_ui(40, 10).unwrap();
            // our channel, then setup
            let chan = r.raw_answer(r.session.channel_req, |res| {
                windows::parse_channel(res).unwrap()
            });
            let id = windows::setup(&mut r.session.client, chan, 40, 9);
            r.session.client.flush().unwrap();
            let first = r.raw_answer(id, |res| windows::parse_setup(res).unwrap()[0]);
            let other = r.lua(&format!(
                "vim.api.nvim_buf_set_lines(0, 0, -1, false, {{ 'one' }})\n\
                 vim.cmd('tabnew')\n\
                 for _, w in ipairs(vim.api.nvim_list_wins()) do {NAME}.adopt(w, 20, 5) end\n\
                 return tostring(vim.api.nvim_get_current_win())"
            ));
            r.wins = [first, other.parse().unwrap()];
            r.session.current_win = r.wins[1];
            Some(r)
        }

        /// Reads until request `id` is answered, and gives its result to `read`.
        fn raw_answer<T>(&mut self, id: u32, read: impl Fn(&mut Decoder<'_>) -> T) -> T {
            for _ in 0..200 {
                assert!(self.session.client.read().unwrap(), "NvimExited");
                while let Some(frame) = self.session.client.next().unwrap() {
                    if let rpc::Message::Response {
                        id: got,
                        err: None,
                        mut result,
                    } = frame.message().unwrap()
                        && got == id
                    {
                        return read(&mut result);
                    }
                }
            }
            panic!("TestTimedOut");
        }

        /// Reads what Neovim has sent, giving every answer to the session.
        /// Returns the reply to `id`, if it's among them.
        fn step(&mut self, id: Option<u32>) -> Option<Reply> {
            let mut fds = [libc::pollfd {
                fd: self.session.client.read_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: one valid pollfd.
            if unsafe { libc::poll(fds.as_mut_ptr(), 1, 3000) } <= 0 {
                panic!("TestTimedOut");
            }
            assert!(self.session.client.read().unwrap(), "NvimExited");
            let mut reply = None;
            while let Some(frame) = self.session.client.next().unwrap() {
                match frame.message().unwrap() {
                    rpc::Message::Response {
                        id: got,
                        err,
                        result,
                    } => {
                        self.session
                            .answered(&mut self.fwds, got, err, result)
                            .unwrap();
                        if Some(got) != id {
                            continue;
                        }
                        let mut text = result;
                        let mut map = result;
                        reply = Some(Reply {
                            text: rpc::expect_str(&mut text)
                                .ok()
                                .map(|s| String::from_utf8_lossy(s).into_owned()),
                            blocking: is_blocking(&mut map).unwrap_or(false),
                        });
                    }
                    rpc::Message::Request { id: req, .. } => {
                        self.session.client.respond_error(req, "test").unwrap()
                    }
                    rpc::Message::Notification { .. } => {}
                }
            }
            reply
        }

        /// Reads until `id` is answered.
        fn wait(&mut self, id: u32) -> Reply {
            for _ in 0..200 {
                if let Some(reply) = self.step(Some(id)) {
                    return reply;
                }
            }
            panic!("TestTimedOut");
        }

        /// The timer, as the Daemon has it, firing for the switch in flight.
        fn tick(&mut self) {
            let _ = self.session.watch_switch();
            let _ = self.session.recheck_switch(&mut self.fwds).unwrap();
        }

        /// Reads until the probe asked, if any, is answered.
        fn probe_answered(&mut self) {
            if let Some(id) = self.session.gate.probe {
                let _ = self.wait(id);
            }
        }

        /// Runs Lua and returns the string it returns.
        fn lua(&mut self, code: &str) -> String {
            let id = self.session.client.exec_lua(code, 0);
            self.session.client.flush().unwrap();
            self.wait(id).text.expect("NotAString")
        }

        /// Reads until every switch is answered, and what waited for them sent.
        fn settle(&mut self) {
            while let Some(id) = self.session.switch_in_flight() {
                let _ = self.wait(id);
            }
        }

        /// Whether Neovim is waiting for a key to finish a command. Answered
        /// at once even then, which `lua` isn't.
        fn blocking(&mut self) -> bool {
            let id = self.session.client.request("nvim_get_mode", 0);
            self.session.client.flush().unwrap();
            self.wait(id).blocking
        }

        /// A tick's worth of time, for Neovim to read what it was sent.
        fn pause(&self) {
            std::thread::sleep(Duration::from_millis(100));
        }

        /// Reads until Neovim is waiting for a key.
        fn until_blocking(&mut self) {
            for _ in 0..500 {
                if self.blocking() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("NeverBlocked");
        }

        /// The text of the windows' buffers, as `first|second`, after all that was typed.
        fn texts(&mut self) -> String {
            self.settle();
            let code = format!(
                "local t = {{}}\n\
                 for _, w in ipairs({{ {}, {} }}) do\n\
                 \x20 table.insert(t, table.concat(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(w), 0, -1, false), '/'))\n\
                 end\n\
                 return table.concat(t, '|')",
                self.wins[0], self.wins[1]
            );
            self.lua(&code)
        }

        fn expect_texts(&mut self, want: &str) {
            assert_eq!(want, self.texts());
        }

        fn expect_nothing_held(&self) {
            assert_eq!(0, self.session.gate.queue.len());
            assert_eq!(None, self.session.switch_in_flight());
        }

        /// Makes Neovim busy for `ms`, without waiting: it reads nothing until
        /// it's done.
        fn busy(&mut self, ms: u32) {
            let _ = self
                .session
                .client
                .exec_lua(&format!("vim.uv.sleep({ms})"), 0);
            self.session.client.flush().unwrap();
        }

        /// Makes Neovim busy until it's interrupted, without waiting: it
        /// reads only `nvim_input` meanwhile. The request to wait on.
        fn busy_until_interrupted(&mut self) -> u32 {
            let id = self.session.client.request("nvim_command", 1);
            msgpack::write_str(self.session.client.writer(), "while 1 | endwhile");
            self.session.client.flush().unwrap();
            id
        }

        /// Maps Alt-x to nothing: unmapped, Neovim takes it as Escape and `x`.
        fn alt_x_does_nothing(&mut self) {
            let _ = self.lua("vim.keymap.set('n', '<M-x>', '<Nop>') return 'ok'");
        }

        /// Fills the first window's buffer with 100 lines.
        fn fill_first(&mut self) {
            let code = format!(
                "local b = vim.api.nvim_win_get_buf({})\n\
                 local l = {{}}\n\
                 for i = 1, 100 do l[i] = 'line ' .. i end\n\
                 vim.api.nvim_buf_set_lines(b, 0, -1, false, l)\n\
                 return 'ok'",
                self.wins[0]
            );
            let _ = self.lua(&code);
        }

        /// The first line `win` shows, after all that was typed.
        fn topline(&mut self, win: i64) -> i64 {
            self.settle();
            self.lua(&format!(
                "return tostring(vim.fn.getwininfo({win})[1].topline)"
            ))
            .parse()
            .unwrap()
        }

        /// Input read from pane `pane`, as the Daemon takes it: the pane is
        /// made current if the input takes focus. Whether it did.
        fn input(&mut self, pane: u32, bytes: &[u8]) -> bool {
            let win = self.wins[pane as usize];
            let focus = self
                .session
                .takes_focus(&mut self.fwds, pane, Some(win), false, bytes)
                != Taking::Nothing;
            if focus {
                self.session.make_current(pane, win).unwrap();
            }
            self.session.type_into(&mut self.fwds, pane, bytes).unwrap();
            focus
        }

        /// The window Neovim has as current.
        fn current_win(&mut self) -> i64 {
            self.lua("return tostring(vim.api.nvim_get_current_win())")
                .parse()
                .unwrap()
        }

        fn make_current(&mut self, pane: u32, win: i64) {
            self.session.make_current(pane, win).unwrap();
        }

        fn type_into(&mut self, pane: u32, bytes: &[u8]) {
            self.session.type_into(&mut self.fwds, pane, bytes).unwrap();
        }

        /// `type_into` for a read that came `ms` after `t0`.
        fn type_into_at(&mut self, pane: u32, bytes: &[u8], t0: std::time::Instant, ms: u64) {
            let at = t0 + std::time::Duration::from_millis(ms);
            self.session
                .type_into_at(&mut self.fwds, pane, bytes, at)
                .unwrap();
        }

        fn takes_focus(
            &mut self,
            pane: u32,
            win: Option<i64>,
            focused_there: bool,
            bytes: &[u8],
        ) -> bool {
            self.session
                .takes_focus(&mut self.fwds, pane, win, focused_there, bytes)
                != Taking::Nothing
        }
    }

    #[test]
    fn keys_typed_after_rex_moves_focus_to_a_pane_go_to_its_window() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        r.expect_texts("ne|two");
        assert_eq!(r.wins[0], r.session.current_win);
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn held_keys_keep_their_order_across_switches_and_reads() {
        let Some(mut r) = Rig::start() else { return };
        // the first window in three reads, then the second, then the first again
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"A");
        r.type_into(0, b"x");
        r.type_into(0, b"y");
        r.make_current(1, r.wins[1]);
        // Escape and the key after it, a timeout later, are two reads, not Alt
        let t0 = std::time::Instant::now();
        r.type_into_at(1, b"\x1b", t0, 0);
        r.type_into_at(1, b"A", t0, 60);
        r.type_into_at(1, b"z", t0, 60);
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"z");
        r.expect_texts("onexyz|twoz");
    }

    #[test]
    fn input_for_a_pane_thats_gone_is_dropped_the_rest_still_goes() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        r.type_into(7, b"x");
        r.type_into(0, b"x");
        r.expect_texts("e|two");
    }

    #[test]
    fn a_command_waiting_for_a_key_gets_one_then_the_switch_happens() {
        let Some(mut r) = Rig::start() else { return };
        // `f` waits for a character, and Neovim answers no switch until it has one
        r.type_into(1, b"f");
        r.until_blocking();
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        r.type_into(0, b"x");
        // nothing asks the moment the switch is sent, which a key sent just before may not have been read for
        assert_eq!(None, r.session.gate.probe);
        r.tick();
        r.probe_answered();
        r.expect_texts("ne|two");
        // the first `x` finished the `f` in the window it was in; the second
        // went to the window switched to
        r.expect_nothing_held();
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn a_key_typed_after_a_later_switch_can_finish_the_command_that_stops_it() {
        let Some(mut r) = Rig::start() else { return };
        r.type_into(1, b"f");
        r.until_blocking();
        // two switches, and the only key typed is after the second
        r.make_current(0, r.wins[0]);
        r.make_current(1, r.wins[1]);
        r.type_into(1, b"x");
        r.type_into(1, b"x");
        r.tick();
        r.probe_answered();
        r.expect_texts("one|wo");
        r.expect_nothing_held();
        let cur = r.current_win();
        assert_eq!(r.wins[1], cur);
    }

    #[test]
    fn a_busy_neovim_keeps_the_keys_held_however_often_its_checked() {
        let Some(mut r) = Rig::start() else { return };
        r.busy(300);
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        // the timer fires, twice, before Neovim answers anything
        r.tick();
        let probe = r.session.gate.probe.unwrap();
        assert!(r.session.recheck_switch(&mut r.fwds).unwrap());
        assert_eq!(Some(probe), r.session.gate.probe);
        assert_eq!(1, r.session.gate.queue.len());
        r.expect_texts("ne|two");
    }

    /// A Ctrl-C typed as `ctrl_c` just after a switch, into a Neovim that's
    /// busy: it waits until a tick finds Neovim not answering, then goes.
    fn check_a_held_ctrl_c_interrupts_a_busy_neovim(ctrl_c: &[u8]) {
        let Some(mut r) = Rig::start() else { return };
        let busy = r.busy_until_interrupted();
        r.make_current(0, r.wins[0]);
        r.type_into(0, ctrl_c);
        r.type_into(0, b"x");
        // the first tick asks, the next finds no answer
        r.pause();
        r.tick();
        assert_eq!(2, r.session.gate.queue.len());
        r.pause();
        r.tick();
        let _ = r.wait(busy);
        // the `x` typed after it waited for the switch
        r.expect_texts("ne|two");
        r.expect_nothing_held();
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn a_held_ctrl_c_interrupts_a_busy_neovim_once_a_tick_finds_it_busy() {
        check_a_held_ctrl_c_interrupts_a_busy_neovim(b"\x03");
    }

    #[test]
    fn a_held_kitty_ctrl_c_interrupts_a_busy_neovim_once_a_tick_finds_it_busy() {
        check_a_held_ctrl_c_interrupts_a_busy_neovim(b"\x1b[99;5u");
    }

    #[test]
    fn an_escape_held_a_timeout_before_a_ctrl_c_that_goes_ahead_is_still_the_escape_key() {
        let Some(mut r) = Rig::start() else { return };
        r.alt_x_does_nothing();
        let busy = r.busy_until_interrupted();
        r.make_current(0, r.wins[0]);
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"\x1b", t0, 0);
        r.pause();
        r.tick();
        r.pause();
        r.type_into_at(0, b"\x03", t0, 100);
        assert_eq!(2, r.session.gate.queue.len());
        // close after the Ctrl-C, but a timeout after the Escape: not Alt-x
        r.type_into_at(0, b"x", t0, 110);
        let _ = r.wait(busy);
        r.expect_texts("ne|two");
        r.expect_nothing_held();
    }

    #[test]
    fn an_escape_held_before_a_ctrl_c_that_goes_ahead_times_out_against_it_not_the_next_read() {
        let Some(mut r) = Rig::start() else { return };
        r.alt_x_does_nothing();
        let busy = r.busy_until_interrupted();
        r.make_current(0, r.wins[0]);
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"\x1b", t0, 0);
        r.pause();
        r.tick();
        r.pause();
        r.type_into_at(0, b"\x03", t0, 60);
        // another pane's read between: the `x` after it is close to neither
        // the Ctrl-C nor it, so the Escape is a key of its own, not Alt-x
        r.type_into_at(1, b"x", t0, 65);
        r.type_into_at(0, b"x", t0, 70);
        let _ = r.wait(busy);
        r.expect_texts("e|two");
        r.expect_nothing_held();
    }

    #[test]
    fn a_ctrl_c_read_just_after_a_held_esc_joins_it_and_waits() {
        let Some(mut r) = Rig::start() else { return };
        let busy = r.busy_until_interrupted();
        r.make_current(0, r.wins[0]);
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"\x1b", t0, 0);
        r.pause();
        r.tick();
        r.pause();
        r.type_into_at(0, b"\x03", t0, 1);
        r.tick();
        assert_eq!(2, r.session.gate.queue.len());
        // one read after them, alone, goes, leaving its mark
        r.type_into_at(0, b"\x03", t0, 2);
        assert_eq!(3, r.session.gate.queue.len());
        assert!(matches!(&r.session.gate.queue[2], Item::Input { bytes, .. } if bytes.is_empty()));
        let _ = r.wait(busy);
        r.settle();
        r.expect_nothing_held();
    }

    /// A Ctrl-C typed as `ctrl_c` while Neovim looks busy: it goes at once.
    fn check_a_ctrl_c_typed_while_neovim_looks_busy_goes_at_once(ctrl_c: &[u8]) {
        let Some(mut r) = Rig::start() else { return };
        let busy = r.busy_until_interrupted();
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        r.pause();
        r.tick();
        assert!(r.session.gate.probe.is_some());
        r.pause();
        r.type_into(0, ctrl_c);
        // `x`, and a mark of when the Ctrl-C came
        assert_eq!(2, r.session.gate.queue.len());
        r.type_into(0, b"p");
        let _ = r.wait(busy);
        // `x` and `p` went after the switch, in order: `p` puts back what `x` cut
        r.expect_texts("noe|two");
        r.expect_nothing_held();
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn a_ctrl_c_typed_while_neovim_looks_busy_goes_at_once_and_the_keys_around_it_wait() {
        check_a_ctrl_c_typed_while_neovim_looks_busy_goes_at_once(b"\x03");
    }

    #[test]
    fn a_kitty_ctrl_c_typed_while_neovim_looks_busy_goes_at_once_and_the_keys_around_it_wait() {
        check_a_ctrl_c_typed_while_neovim_looks_busy_goes_at_once(b"\x1b[99;5u");
    }

    #[test]
    fn a_ctrl_c_typed_after_a_switch_into_a_neovim_thats_not_busy_keeps_its_place() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"A");
        r.type_into(0, b"x");
        r.type_into(0, b"\x03");
        assert_eq!(3, r.session.gate.queue.len());
        // the timer asks, and Neovim answers
        r.tick();
        r.probe_answered();
        // ahead of `Ax`, it would leave Neovim in insert mode. (Neovim drops
        // the keys it hasn't run when it reads Ctrl-C, so the `x` may be gone.)
        r.settle();
        r.expect_nothing_held();
        let mode = r.lua("return vim.api.nvim_get_mode().mode");
        assert_eq!("n", mode);
    }

    #[test]
    fn a_command_that_needs_two_keys_gets_them_one_tick_apart() {
        let Some(mut r) = Rig::start() else { return };
        // Ctrl-W waits for a character, and then `g` for another
        r.type_into(1, b"\x17");
        r.until_blocking();
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"g");
        r.type_into(0, b"z");
        r.type_into(0, b"x");
        r.tick();
        r.probe_answered();
        // `g` went. A key just went through: Neovim may not have read it when
        // it's asked, so the next tick doesn't ask, and the one after does
        r.pause();
        assert!(r.session.recheck_switch(&mut r.fwds).unwrap());
        assert_eq!(None, r.session.gate.probe);
        assert!(r.session.recheck_switch(&mut r.fwds).unwrap());
        assert!(r.session.gate.probe.is_some());
        r.probe_answered();
        // `z` finished it, and `x` waited for the switch
        r.expect_texts("ne|two");
        r.expect_nothing_held();
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn an_escape_after_the_key_thats_let_through_isnt_lost_nor_joined_to_the_next_key() {
        let Some(mut r) = Rig::start() else { return };
        r.type_into(1, b"f");
        r.until_blocking();
        r.make_current(0, r.wins[0]);
        // two Escapes in one read; the first ends the `f`
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"\x1b\x1b", t0, 0);
        r.type_into_at(0, b"x", t0, 60);
        r.tick();
        r.probe_answered();
        // the second Escape went with the rest, so `x` isn't Alt-x
        r.expect_texts("ne|two");
        r.expect_nothing_held();
    }

    #[test]
    fn a_held_esc_and_the_rest_of_its_sequence_read_close_together_make_the_key() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"Ax", t0, 0);
        // Left arrow split after its ESC: in insert mode, `y` goes before `x`
        r.type_into_at(0, b"\x1b", t0, 1);
        r.type_into_at(0, b"[D", t0, 2);
        r.type_into_at(0, b"y", t0, 3);
        r.expect_texts("oneyx|two");
        r.expect_nothing_held();
    }

    #[test]
    fn an_esc_fed_before_a_switch_times_out_against_the_read_held_behind_it() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        r.settle();
        let t0 = std::time::Instant::now();
        // direct: nothing is in flight; the ESC ends the read
        r.type_into_at(0, b"Ax\x1b", t0, 0);
        r.make_current(0, r.wins[0]);
        r.type_into_at(0, b"Az", t0, 100);
        r.expect_texts("onexz|two");
        r.expect_nothing_held();
    }

    #[test]
    fn a_held_esc_a_timeout_before_the_next_read_is_the_escape_key() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        let t0 = std::time::Instant::now();
        r.type_into_at(0, b"Ax", t0, 0);
        r.type_into_at(0, b"\x1b", t0, 1);
        // Escape left insert mode, so this `A` appends again
        r.type_into_at(0, b"Az", t0, 100);
        r.expect_texts("onexz|two");
        r.expect_nothing_held();
    }

    #[test]
    fn the_timer_watches_each_switch_once_and_does_nothing_for_one_thats_done() {
        let Some(mut r) = Rig::start() else { return };
        assert!(!r.session.watch_switch());
        assert!(!r.session.recheck_switch(&mut r.fwds).unwrap());

        r.make_current(0, r.wins[0]);
        assert!(r.session.watch_switch());
        assert!(!r.session.watch_switch());
        // a switch queued behind it isn't the one in flight
        r.make_current(1, r.wins[1]);
        assert!(!r.session.watch_switch());
        assert!(r.session.recheck_switch(&mut r.fwds).unwrap());

        // the first answered, the second in flight: its wait starts over
        r.type_into(1, b"x");
        let first = r.session.switch_in_flight().unwrap();
        let _ = r.wait(first);
        let second = r.session.switch_in_flight().unwrap();
        assert_ne!(first, second);
        // a stale timer: it only starts the new switch's wait
        assert!(r.session.recheck_switch(&mut r.fwds).unwrap());
        assert_eq!(Some(second), r.session.watched_switch);
        r.settle();
        // nothing in flight: the timer stops
        assert!(!r.session.recheck_switch(&mut r.fwds).unwrap());
        r.expect_texts("one|wo");
    }

    #[test]
    fn a_switch_that_fails_drops_its_panes_keys_not_another_panes_and_focus_is_kept() {
        let Some(mut r) = Rig::start() else { return };
        // a window that isn't there: it closed before the switch ran
        r.make_current(0, 9999);
        // a focus report, with a key: the key's dropped, the report is read
        r.type_into(0, b"\x1b[Ix");
        // another pane's key, typed meanwhile, still goes
        r.type_into(1, b"x");
        r.make_current(0, r.wins[0]);
        r.type_into(0, b"x");
        r.expect_texts("ne|wo");
        r.expect_nothing_held();
        assert_eq!(Some(true), r.fwds.0[0].focused);
    }

    #[test]
    fn dropping_input_that_ends_a_paste_ends_it_in_neovim() {
        let Some(mut r) = Rig::start() else { return };
        let _ = r.lua(
            "_G.phases = {}\n\
             vim.paste = function(_, phase)\n\
             \x20 table.insert(_G.phases, phase)\n\
             \x20 return true\n\
             end\n\
             return 'ok'",
        );
        // a paste begun, then a switch that fails, and the paste's end behind it
        r.type_into(0, b"\x1b[200~ab");
        r.make_current(0, 9999);
        r.type_into(0, b"\x1b[201~");
        r.settle();
        let phases = r.lua("return table.concat(_G.phases, ',')");
        assert_eq!("1,3", phases);
    }

    #[test]
    fn focus_reports_dont_wait_for_a_switch_unless_the_parser_is_partway_through_one() {
        let Some(mut r) = Rig::start() else { return };
        r.make_current(0, r.wins[0]);
        r.type_into(1, b"\x1b[O");
        assert_eq!(Some(false), r.fwds.0[1].focused);
        assert_eq!(0, r.session.gate.queue.len());

        // partway through a sequence, a report would be read as part of it
        r.fwds.0[0].parser.set_partway_for_test(true);
        r.type_into(0, b"\x1b[I");
        assert_eq!(1, r.session.gate.queue.len());
        assert_eq!(None, r.fwds.0[0].focused);
        r.fwds.0[0].parser.set_partway_for_test(false);
        r.settle();
        assert_eq!(Some(true), r.fwds.0[0].focused);
    }

    // SGR mouse reports at column 31, row 2 (1-based), where the first window is
    const WHEEL_DOWN_FIRST: &[u8] = b"\x1b[<65;31;2M";
    const WHEEL_UP_FIRST: &[u8] = b"\x1b[<64;31;2M";
    const CLICK_FIRST: &[u8] = b"\x1b[<0;31;2M";

    #[test]
    fn a_wheel_event_in_an_unfocused_pane_scrolls_its_window_and_moves_no_focus() {
        let Some(mut r) = Rig::start() else { return };
        r.fill_first();
        assert!(!r.input(0, WHEEL_DOWN_FIRST));
        assert_eq!(4, r.topline(r.wins[0]));
        r.expect_nothing_held();
        assert_eq!(r.wins[1], r.session.current_win);
        let cur = r.current_win();
        assert_eq!(r.wins[1], cur);
    }

    #[test]
    fn what_is_typed_into_another_tab_pages_pane_goes_there_after_its_tab_page_is_current() {
        let Some(mut r) = Rig::with_tab_pages() else {
            return;
        };
        assert!(r.input(0, b"x"));
        r.settle();
        assert_eq!(
            format!("1 {}", r.wins[0]),
            r.lua("return vim.fn.tabpagenr() .. ' ' .. vim.api.nvim_get_current_win()")
        );
        assert_eq!(
            "ne",
            r.lua(&format!(
                "return vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf({}), 0, -1, false)[1]",
                r.wins[0]
            ))
        );
    }

    #[test]
    fn a_click_in_an_unfocused_pane_makes_it_current() {
        let Some(mut r) = Rig::start() else { return };
        r.fill_first();
        assert!(r.input(0, CLICK_FIRST));
        r.settle();
        assert_eq!(r.wins[0], r.session.current_win);
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn what_is_typed_into_an_unfocused_pane_still_makes_it_current() {
        let Some(mut r) = Rig::start() else { return };
        assert!(r.input(0, b"x"));
        r.expect_texts("ne|two");
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn only_input_that_takes_focus_to_a_pane_that_has_none_makes_it_current() {
        let Some(mut r) = Rig::start() else { return };
        let wheel = b"\x1b[<65;31;2M";
        let (w0, w1) = (r.wins[0], r.wins[1]);
        assert!(r.takes_focus(0, Some(w0), false, b"x"));
        assert!(!r.takes_focus(0, Some(w0), false, wheel));
        // motion with no button, a release, and the focus reports Rex sends as it moves focus away
        assert!(!r.takes_focus(0, Some(w0), false, b"\x1b[<35;31;2M"));
        assert!(!r.takes_focus(0, Some(w0), false, b"\x1b[<0;31;2m"));
        assert!(!r.takes_focus(0, Some(w0), false, b"\x1b[O"));
        // the pane provider has the pane focused, or Neovim's current window is its window, or it has none
        assert!(!r.takes_focus(0, Some(w0), true, b"x"));
        assert!(!r.takes_focus(1, Some(w1), false, b"x"));
        assert!(!r.takes_focus(0, None, false, b"x"));
        // a pane that's gone
        assert!(!r.takes_focus(7, Some(w0), false, b"x"));
    }

    #[test]
    fn a_wheel_event_waits_its_turn_behind_a_switch_and_the_keys_before_it_and_starts_none() {
        let Some(mut r) = Rig::start() else { return };
        r.fill_first();
        // `G` in the first window, then wheel up over it: in the other order the
        // view would end at the bottom, line 93
        assert!(r.input(0, b"G"));
        assert!(!r.input(0, WHEEL_UP_FIRST));
        assert_eq!(2, r.session.gate.queue.len());
        r.settle();
        r.expect_nothing_held();
        assert_eq!(90, r.topline(r.wins[0]));
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }

    #[test]
    fn a_wheel_event_in_a_pane_behind_the_one_being_switched_to_starts_no_switch() {
        let Some(mut r) = Rig::start() else { return };
        r.fill_first();
        // the second window is current; Rex moves focus to the first, and the wheel is over the second
        r.make_current(0, r.wins[0]);
        // over pane 1's window at column 5, row 2
        assert!(!r.input(1, b"\x1b[<65;5;2M"));
        assert_eq!(1, r.session.gate.queue.len());
        r.settle();
        assert_eq!(r.wins[0], r.session.current_win);
        let cur = r.current_win();
        assert_eq!(r.wins[0], cur);
    }
}
