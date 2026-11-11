//! Attaches (ADR 0013): new connections waiting for their hello, read
//! without blocking the loop, answered, and the frames an attached helper
//! sends after.

use std::os::fd::{AsRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Instant;

use super::*;
use crate::attach::{End, Hello, Message, PaneId, Purpose, Refusal, Size, Terminal};
use crate::daemon::attach_conn::{self, Arrived, HELLO_TIMEOUT, Pending, Step};
use crate::nvim::msgpack;

/// Loop tokens of connections waiting for their hello start here, far above
/// any pane's.
pub(crate) const PENDING_BASE: usize = 1 << (usize::BITS - 2);

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
        let fd = pending.stream.as_raw_fd();
        match pending.read() {
            Step::Waiting(p) => {
                self.pending.push((token, p));
                Ok(())
            }
            Step::Gone => {
                self.event_loop.remove(fd)?;
                Ok(())
            }
            Step::Arrived(a) => {
                self.event_loop.remove(fd)?;
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
        self.arm_hello_timer()
    }

    /// Starts the hello timer for the earliest deadline, unless it runs for
    /// one that early.
    fn arm_hello_timer(&mut self) -> Result<()> {
        let Some(due) = self.pending.iter().map(|(_, p)| p.deadline).min() else {
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
        let Arrived {
            stream,
            tty,
            hello,
            at,
            rest,
        } = a;
        let hello = match hello {
            Ok(h) => h,
            Err(e) => {
                let z = attach_conn::refusal_of(&e);
                log_warn!("{NAME} daemon: refused a hello: {}", z.message);
                attach_conn::refuse(stream, z);
                return Ok(());
            }
        };
        let Hello { build, purpose, .. } = hello;
        match purpose {
            Purpose::Requested { pane, terminal } => {
                self.attach_requested(stream, tty, pane, terminal, &build, at, rest)
            }
            // the launcher's comes before the loop runs (#105 opens it up)
            other => {
                let z = End::refusal(
                    Refusal::Hello,
                    format!("this daemon takes no {} hello yet", other.name()),
                );
                log_warn!("{NAME} daemon: refused a hello: {}", z.message);
                attach_conn::refuse(stream, z);
                Ok(())
            }
        }
    }

    /// A requested pane's terminal: matched to the pane its id was minted
    /// for, if it is still good, and refused otherwise.
    #[allow(clippy::too_many_arguments)]
    fn attach_requested(
        &mut self,
        stream: UnixStream,
        tty: Option<OwnedFd>,
        pane: PaneId,
        terminal: Terminal,
        build: &str,
        at: u64,
        rest: Vec<u8>,
    ) -> Result<()> {
        let shown = pane.to_string();
        log_info!(
            "{NAME} daemon: hello: {}",
            attach_conn::describe("requested", Some(&shown), &terminal, build)
        );
        let id = self
            .panes
            .by_attach_id(&pane)
            .filter(|p| p.terminal_surface.is_none())
            .map(|p| p.id);
        if let Err(z) = attach_conn::check_terminal(&terminal, tty.as_ref()) {
            return self.refuse_requested(stream, z, id.map(|id| (id, pane)));
        }
        if let Err(reason) = self.panes.ids.take(&pane) {
            let message = match reason {
                Refusal::PaneTaken => format!("pane {pane} already has a terminal"),
                Refusal::PaneExpired => format!("pane {pane} is no longer wanted"),
                _ => format!("no pane {pane} was asked for"),
            };
            return self.refuse_requested(stream, End::refusal(reason, message), None);
        }
        // a pane goes with its id, so a good id has its pane
        let Some(id) = id else {
            let z = End::refusal(Refusal::PaneExpired, format!("pane {pane} is gone"));
            return self.refuse_requested(stream, z, None);
        };
        let tty = tty.expect("checked").into_raw_fd();
        // (a failing `Surface::create` closes the helper's socket and terminal
        // itself)
        let mut surface = Surface::create(stream, tty, terminal.size)?;
        if let Err(e) = attach_conn::send_accept(&surface.helper_socket, &self.name, &self.log) {
            log_warn!("{NAME} daemon: pane {shown}'s helper went: {e}");
            surface.destroy(
                super::super::surface::Restore::Now,
                End::ending(crate::attach::Ending::Failed, "", false),
            );
            return Ok(());
        }
        log::trace_at(Point::Hello, u64::from(id), at);
        log::trace(Point::Accepted, u64::from(id));
        surface.helper_socket.set_nonblocking(true)?;
        surface.helper_in = rest;
        let pane = self.panes.by_id_mut(id).unwrap();
        pane.terminal_surface = Some(surface);
        pane.register_with(&self.event_loop)?;
        if pane.awaiting_layout {
            let v = &pane.terminal_surface.as_ref().unwrap().view;
            if v.cols != REX_DEFAULT_SIZE[0] || v.rows != REX_DEFAULT_SIZE[1] {
                pane.awaiting_layout = false;
            } else {
                self.add_timer(Token::LayoutTimer, LAYOUT_WAIT_MS)?;
            }
        }
        self.size_window(id);
        if self.panes.by_id(id).unwrap().layer.is_some()
            && let Some(req) = self.floating().follow_layer(id)
            && let Some(l) = &mut self.panes.by_id_mut(id).unwrap().layer
        {
            l.fit = Fit::Asked(req);
        }
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
    /// waiting for its terminal, `failed`, gives up on it and closes its
    /// Rex block.
    fn refuse_requested(
        &mut self,
        stream: UnixStream,
        z: End,
        failed: Option<(u32, PaneId)>,
    ) -> Result<()> {
        let reason = z.as_refusal();
        let quiet = matches!(
            reason,
            Some(Refusal::PaneExpired | Refusal::PaneUnknown | Refusal::Stopping)
        );
        let line = format!("a pane's terminal was refused: {}", z.message);
        if quiet {
            log_info!("{NAME} daemon: {line}");
        } else {
            log_warn!("{NAME} daemon: {line}");
            self.warn_in_nvim(&format!("{NAME}: {line}"));
        }
        attach_conn::refuse(stream, z);
        if let Some((id, pane)) = failed {
            self.panes.ids.expire(&pane);
            let p = self.panes.by_id_mut(id).unwrap();
            p.closing = Some(Close::RequestFailed);
            if let Some(b) = p.rex_block {
                self.rex.close_block(&block_str(&b))?;
            }
            self.reconcile()?;
            self.draw()?;
        }
        self.nvim.client.flush()?;
        Ok(())
    }

    /// Shows `message` in Neovim as a warning, kept in its message history.
    fn warn_in_nvim(&mut self, message: &str) {
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

    /// Pane `id`'s helper sent something: its terminal's new size, as `s`.
    /// A helper that hung up or broke the protocol closes the pane.
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

    /// Acts on the whole frames pane `id`'s helper has sent: the latest size.
    fn read_helper_frames(&mut self, id: u32) -> Result<()> {
        let mut size: Option<Size> = None;
        let s = self
            .panes
            .by_id_mut(id)
            .and_then(|p| p.terminal_surface.as_deref_mut())
            .expect("an attached pane has a terminal");
        loop {
            match attach_conn::next_message(&mut s.helper_in) {
                Ok(Some(Message::Size(new))) => size = Some(new),
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
