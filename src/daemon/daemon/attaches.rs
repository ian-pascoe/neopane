//! Attaches (ADR 0013): new connections waiting for their hello, read
//! without blocking the loop, answered, and the frames an attached helper
//! sends after.

use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use super::*;
use crate::attach::{End, Hello, Message, PaneId, Purpose, Refusal, Size, Transport};
use crate::daemon::attach_conn::{self, Arrived, HELLO_TIMEOUT, Nesting, Pending, Step};
use crate::daemon::pane::FileAt;
use crate::nvim::msgpack;

/// Loop tokens of connections waiting for their hello start here, far above
/// any pane's.
pub(crate) const PENDING_BASE: usize = 1 << (usize::BITS - 2);

/// How long a self-attach that may be nested waits for Neovim to list its
/// terminals; one it hasn't by then is refused as nested.
const NESTED_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

/// A self-attach whose environment says it may come from the daemon's own
/// Neovim, waiting for Neovim's terminals, as request `req`, until
/// `deadline`.
pub(crate) struct NestedCheck {
    arrived: Arrived,
    pub(super) req: u32,
    deadline: Instant,
}

impl Daemon {
    /// A new connection: it gets `HELLO_TIMEOUT` for its hello, read as it
    /// comes, without blocking the loop. Most send it at once.
    pub(super) fn accept(&mut self, server: &UnixListener) -> Result<()> {
        let (stream, _) = server.accept()?;
        let Some(pending) = Pending::new(stream, HELLO_TIMEOUT, self.uid)? else {
            log_warn!("{NAME} daemon: refused a connection from another user");
            return Ok(());
        };
        let token = PENDING_BASE + self.next_pending;
        self.next_pending += 1;
        self.event_loop
            .add_read(pending.stream.as_raw_fd(), token)?;
        self.pending.push((token, pending));
        self.arm_hello_timer()?;
        self.on_pending(token)
    }

    /// Pending connection `token` has something to read.
    pub(super) fn on_pending(&mut self, token: usize) -> Result<()> {
        let Some(i) = self.pending.iter().position(|(t, _)| *t == token) else {
            return Ok(());
        };
        let (_, pending) = self.pending.swap_remove(i);
        match pending.read() {
            Step::Waiting(p) => {
                self.pending.push((token, p));
                Ok(())
            }
            // off the loop before its socket closes
            Step::Gone(stream) => {
                self.event_loop.remove(stream.as_raw_fd())?;
                Ok(())
            }
            Step::Arrived(a) => {
                self.event_loop.remove(a.stream.as_raw_fd())?;
                self.on_hello(a)
            }
        }
    }

    /// The hello timer fired: connections whose hello hasn't come in time
    /// are refused.
    pub(super) fn time_out_hellos(&mut self, now: Instant) -> Result<()> {
        self.hello_timer_due = None;
        let mut i = self.pending.len();
        while i > 0 {
            i -= 1;
            if self.pending[i].1.deadline <= now {
                let (_, p) = self.pending.swap_remove(i);
                self.event_loop.remove(p.stream.as_raw_fd())?;
                log_warn!("{NAME} daemon: refused a connection: no hello in time");
                p.time_out();
            }
        }
        let mut i = self.nested_checks.len();
        while i > 0 {
            i -= 1;
            if self.nested_checks[i].deadline <= now {
                let c = self.nested_checks.swap_remove(i);
                log_warn!("{NAME} daemon: Neovim didn't list its terminals in time");
                self.refuse_nested(c.arrived.stream);
            }
        }
        self.arm_hello_timer()
    }

    /// Starts the hello timer for the earliest deadline, unless it runs for
    /// one that early.
    pub(super) fn arm_hello_timer(&mut self) -> Result<()> {
        let pending = self.pending.iter().map(|(_, p)| p.deadline);
        let checks = self.nested_checks.iter().map(|c| c.deadline);
        let Some(due) = pending.chain(checks).min() else {
            return Ok(());
        };
        if self.hello_timer_due.is_some_and(|armed| armed <= due) {
            return Ok(());
        }
        let ms = due
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        self.hello_timer_due = Some(due);
        self.add_timer(Token::HelloTimer, u32::try_from(ms).unwrap_or(u32::MAX))
    }

    /// A hello arrived whole: answer it.
    fn on_hello(&mut self, a: Arrived) -> Result<()> {
        match &a.hello {
            // a requested pane's warns, and its pane, if it can still be
            // told, is given up
            Err(e) if a.requested => {
                let z = attach_conn::refusal_of(e);
                let failed = a.pane.and_then(|p| self.waiting_pane(&p).map(|id| (id, p)));
                self.refuse_requested(a.stream, z, failed)
            }
            Err(e) => {
                let z = attach_conn::refusal_of(e);
                log_warn!("{NAME} daemon: refused a hello: {}", z.message);
                attach_conn::refuse(a.stream, z);
                Ok(())
            }
            Ok(Hello {
                purpose: Purpose::Requested { .. },
                ..
            }) => self.attach_requested(a),
            Ok(Hello {
                purpose: Purpose::SelfAttach { .. },
                ..
            }) => self.attach_self(a),
            // control requests come later
            Ok(h) => {
                let z = End::refusal(
                    Refusal::Hello,
                    format!("this daemon takes no {} hello yet", h.purpose.name()),
                );
                log_warn!("{NAME} daemon: refused a hello: {}", z.message);
                attach_conn::refuse(a.stream, z);
                Ok(())
            }
        }
    }

    /// A terminal attaching of its own accord, `a`, at any time (#105): it
    /// gets a pane at once, and its window once Neovim has one for it.
    /// Refused from the daemon's own Neovim (`nested`): when the environment
    /// says it may be, Neovim is asked for its terminals first.
    fn attach_self(&mut self, a: Arrived) -> Result<()> {
        let Ok(Hello {
            build,
            purpose: Purpose::SelfAttach { terminal, .. },
            ..
        }) = &a.hello
        else {
            unreachable!("on_hello checked it's a self-attach's hello");
        };
        log_info!(
            "{NAME} daemon: hello: {}",
            attach_conn::describe("self", None, terminal, build)
        );
        match attach_conn::nesting(terminal, self.sock.as_arg()) {
            Nesting::No => self.accept_self(a),
            Nesting::Yes => {
                self.refuse_nested(a.stream);
                Ok(())
            }
            Nesting::IfNeovims => {
                let req = windows::list_chans(&mut self.nvim.client);
                self.nvim.client.flush()?;
                self.nested_checks.push(NestedCheck {
                    arrived: a,
                    req,
                    deadline: Instant::now() + NESTED_WAIT,
                });
                self.arm_hello_timer()
            }
        }
    }

    /// Refuses a self-attach from the daemon's own Neovim.
    fn refuse_nested(&self, stream: UnixStream) {
        let z = End::refusal(
            Refusal::Nested,
            format!(
                "a terminal in {}'s own Neovim can't attach to it",
                self.name
            ),
        );
        log_info!("{NAME} daemon: refused a hello: {}", z.message);
        attach_conn::refuse(stream, z);
    }

    /// Neovim answered request `id`: true if it listed its channels for
    /// self-attaches that may be nested, which are answered now.
    pub(super) fn on_nested_answer(
        &mut self,
        id: u32,
        err: Option<&[u8]>,
        mut result: Decoder<'_>,
    ) -> Result<bool> {
        let Some(i) = self.nested_checks.iter().position(|c| c.req == id) else {
            return Ok(false);
        };
        let check = self.nested_checks.remove(i);
        let ptys = match err {
            None => windows::parse_chan_ptys(&mut result)?,
            Some(e) => {
                log_warn!(
                    "{NAME} daemon: couldn't list Neovim's terminals: {}",
                    String::from_utf8_lossy(e)
                );
                Vec::new()
            }
        };
        let tty = match &check.arrived.hello {
            Ok(Hello {
                purpose: Purpose::SelfAttach { terminal, .. },
                ..
            }) => &terminal.tty,
            _ => unreachable!("only self-attaches are checked"),
        };
        if err.is_some() || ptys.contains(tty) {
            self.refuse_nested(check.arrived.stream);
            return Ok(true);
        }
        self.accept_self(check.arrived)?;
        Ok(true)
    }

    /// Self-attach `a`, not from the daemon's own Neovim: accepted, unless
    /// its terminal can't be taken.
    fn accept_self(&mut self, a: Arrived) -> Result<()> {
        let Arrived {
            stream,
            tty,
            hello,
            at,
            rest,
            ..
        } = a;
        let Ok(Hello {
            purpose:
                Purpose::SelfAttach {
                    terminal,
                    provider,
                    file,
                    line,
                },
            ..
        }) = hello
        else {
            unreachable!("on_hello checked it's a self-attach's hello");
        };
        if let Err(z) = attach_conn::check_terminal(&terminal, tty.as_ref()) {
            log_warn!("{NAME} daemon: refused a hello: {}", z.message);
            attach_conn::refuse(stream, z);
            return Ok(());
        }
        let self_hello = attach_conn::self_hello_of(&terminal, tty.as_ref(), provider);
        let opening = file.map(|f| FileAt {
            file: attach_conn::file_against_cwd(&f, &terminal.cwd),
            line,
        });
        let mut surface = Surface::for_attach(stream, terminal.transport, tty, terminal.size);
        if let Err(e) = attach_conn::send_accept(&surface.helper_socket, &self.name, &self.log) {
            log_warn!("{NAME} daemon: a self-attach's helper went: {e}");
            surface.destroy(crate::attach::Ending::Failed, "");
            return Ok(());
        }
        surface.helper_socket.set_nonblocking(true)?;
        surface.helper_in = rest;
        let pane = self.panes.add_self_attached(SelfAttach {
            surface,
            terminal: None,
            hello: Some(Box::new(self_hello)),
        });
        let id = pane.id;
        pane.opening = opening;
        log::trace_at(Point::Hello, u64::from(id), at);
        log::trace(Point::Accepted, u64::from(id));
        pane.register_with(&self.event_loop)?;
        self.attached_again();
        self.wait_for_report(id, Instant::now())?;
        self.give_windows()?;
        self.draw()?;
        // its window is on its way: now the pane provider
        self.choose_or_reach(id)?;
        self.nvim.client.flush()?;
        // a size sent right after the hello
        if self.panes.by_id(id).is_some() {
            self.read_helper_frames(id)?;
        }
        Ok(())
    }

    /// The pane still waiting for the terminal `pane` names: its id is good,
    /// and it has none yet.
    fn waiting_pane(&self, pane: &PaneId) -> Option<u32> {
        self.panes.ids.check(pane).ok()?;
        self.panes
            .by_attach_id(pane)
            .filter(|p| p.terminal_surface.is_none())
            .map(|p| p.id)
    }

    /// A requested pane's terminal, `a`: matched to the pane its id was
    /// minted for, if it is still good, and refused otherwise. The id is
    /// checked first, so a terminal no pane waits for is only logged.
    fn attach_requested(&mut self, a: Arrived) -> Result<()> {
        let Arrived {
            stream,
            tty,
            hello,
            at,
            rest,
            ..
        } = a;
        let Ok(Hello {
            build,
            purpose: Purpose::Requested { pane, terminal },
            ..
        }) = hello
        else {
            unreachable!("on_hello checked it's a requested pane's hello");
        };
        let shown = pane.to_string();
        log_info!(
            "{NAME} daemon: hello: {}",
            attach_conn::describe("requested", Some(&shown), &terminal, &build)
        );
        if let Err(reason) = self.panes.ids.check(&pane) {
            let message = match reason {
                Refusal::PaneTaken => format!("pane {pane} already has a terminal"),
                Refusal::PaneExpired => format!("pane {pane} is no longer wanted"),
                _ => format!("no pane {pane} was asked for"),
            };
            return self.refuse_requested(stream, End::refusal(reason, message), None);
        }
        // a pane goes with its id, so a good id has its pane
        let Some(id) = self.waiting_pane(&pane) else {
            let z = End::refusal(Refusal::PaneExpired, format!("pane {pane} is gone"));
            return self.refuse_requested(stream, z, None);
        };
        if let Err(z) = attach_conn::check_terminal(&terminal, tty.as_ref()) {
            return self.refuse_requested(stream, z, Some((id, pane)));
        }
        self.panes.ids.take(&pane).expect("checked");
        let mut surface = Surface::for_attach(stream, terminal.transport, tty, terminal.size);
        if let Err(e) = attach_conn::send_accept(&surface.helper_socket, &self.name, &self.log) {
            log_warn!("{NAME} daemon: pane {shown}'s helper went: {e}");
            surface.destroy(crate::attach::Ending::Failed, "");
            return Ok(());
        }
        log::trace_at(Point::Hello, u64::from(id), at);
        log::trace(Point::Accepted, u64::from(id));
        surface.helper_socket.set_nonblocking(true)?;
        surface.helper_in = rest;
        // a terminal the pane provider hasn't laid out yet says this size
        let unlaid = self
            .provider
            .as_ref()
            .and_then(|p| p.seam_ref().unlaid_size());
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.terminal_surface = Some(surface);
        pane.attach_due = None;
        pane.register_with(&self.event_loop)?;
        if pane.awaiting_size {
            let v = &pane.terminal_surface.as_ref().unwrap().view;
            if unlaid != Some([v.cols, v.rows]) {
                pane.awaiting_size = false;
            } else {
                self.add_timer(Token::LayoutTimer, SPLIT_SIZE_WAIT_MS)?;
            }
        }
        self.size_window(id);
        self.place_opened(id);
        if self.panes.by_id(id).unwrap().layer.is_some()
            && let Some(req) = self.floating().follow_layer(id)
            && let Some(l) = &mut self.panes.by_id_mut(id).unwrap().layer
        {
            l.fit = Fit::Asked(req);
        }
        self.wait_for_report(id, Instant::now())?;
        self.read_layout_again();
        self.nvim.client.flush()?;
        self.draw()?;
        // a size sent right after the hello
        if self.panes.by_id(id).is_some() {
            self.read_helper_frames(id)?;
        }
        Ok(())
    }

    /// Refuses a requested pane's terminal with `z`. Unless the pane is no
    /// longer wanted or its id unknown, Neovim shows a warning. A pane still
    /// waiting for its terminal, `failed`, is given up. Its terminal refused
    /// for its version, the binary at the daemon's exe path is another than
    /// it runs: until that file changes again, new windows are drawn in their
    /// origin panes, which warns once (#91 decision 24).
    fn refuse_requested(
        &mut self,
        stream: UnixStream,
        z: End,
        failed: Option<(u32, PaneId)>,
    ) -> Result<()> {
        let reason = z.as_refusal();
        let ours = failed.is_some() && reason == Some(Refusal::Version);
        let quiet = matches!(
            reason,
            Some(Refusal::PaneExpired | Refusal::PaneUnknown | Refusal::Stopping)
        ) || (ours && self.version_refused.is_some());
        let mut line = format!("a pane's terminal was refused: {}", z.message);
        if ours && self.version_refused.is_none() {
            self.version_refused = Some(VersionRefused::of(&self.exe));
            line += &format!(
                "; new windows are drawn in their origin panes until {} changes",
                self.exe
            );
        }
        if quiet {
            log_info!("{NAME} daemon: {line}");
        } else {
            log_warn!("{NAME} daemon: {line}");
            self.warn_in_nvim(&format!("{NAME}: {line}"));
        }
        attach_conn::refuse(stream, z);
        if let Some((id, _)) = failed {
            self.give_up(id)?;
        }
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Shows `message` in Neovim as a warning, kept in its message history.
    pub(super) fn warn_in_nvim(&mut self, message: &str) {
        let c = &mut self.nvim.client;
        let _ = c.request("nvim_echo", 3);
        let w = c.writer();
        msgpack::write_array_header(w, 1);
        msgpack::write_array_header(w, 2);
        msgpack::write_str(w, message);
        msgpack::write_str(w, "WarningMsg");
        msgpack::write_bool(w, true);
        msgpack::write_map_header(w, 0);
    }

    /// Pane `id`'s helper sent something: its terminal's new size, as `s`,
    /// or, relayed, what was typed, as `i`. A helper that hung up or broke
    /// the protocol closes the pane.
    pub(super) fn on_helper(&mut self, id: u32) -> Result<()> {
        let mut buf = [0u8; 4096];
        let fd = self.surface_of(id).helper_fd();
        let n = loop {
            // SAFETY: `buf` is valid for its length.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                break n as usize;
            }
            match std::io::Error::last_os_error().kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => return Ok(()),
                _ => break 0,
            }
        };
        if n == 0 {
            return self.pane_gone(id);
        }
        let s = self
            .panes
            .by_id_mut(id)
            .and_then(|p| p.terminal_surface.as_deref_mut())
            .expect("a pane with events has a terminal");
        s.helper_in.extend_from_slice(&buf[..n]);
        self.read_helper_frames(id)
    }

    /// Acts on the whole frames pane `id`'s helper has sent, in order: what
    /// was typed, and the latest size, which goes before anything typed
    /// after it.
    fn read_helper_frames(&mut self, id: u32) -> Result<()> {
        let mut size: Option<Size> = None;
        loop {
            let Some(s) = self
                .panes
                .by_id_mut(id)
                .and_then(|p| p.terminal_surface.as_deref_mut())
            else {
                return Ok(());
            };
            match attach_conn::next_message(&mut s.helper_in) {
                Ok(Some(Message::Size(new))) => size = Some(new),
                Ok(Some(Message::Input(keys))) if s.io.transport() == Transport::Relay => {
                    if let Some(size) = size.take() {
                        self.handle_resize(id, size)?;
                    }
                    self.on_keys(id, &keys)?;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    log_warn!("{NAME} daemon: pane {id}'s helper broke the protocol: {e}");
                    return self.pane_gone(id);
                }
            }
        }
        match size {
            Some(size) => self.handle_resize(id, size),
            None => Ok(()),
        }
    }
}
