//! Control requests (#91 decision 26, #94 decisions 29–32): one `r` frame,
//! then the daemon closes. `status` is answered from what the daemon
//! tracks, never waiting on Neovim. `stop` ends the daemon, unless it has
//! unsaved changes and wasn't forced: its attaches end `stopped`, the
//! terminals it requested close, and Neovim's channel closes, so its swap
//! files are kept.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use super::*;
use crate::attach::{ATTACH_PROTOCOL_VERSION, Hello, Purpose, Status};
use crate::daemon::attach_conn;
use crate::daemon::control::shown_files;
use crate::daemon::provider::Capability;

/// How long a stopping daemon waits for the pane provider to close the
/// terminals it requested.
const STOP_WAIT_MS: u32 = 2000;

/// Why there's no pane provider, as `status` shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoProvider {
    /// None was chosen, or the one chosen went.
    None,
    /// The hint `none` skipped choosing.
    HintNone,
    /// The choice was refused: `find` didn't confirm it in time.
    FindRefused,
    // `none (config error)` comes with the config's states (#129)
}

impl std::fmt::Display for NoProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NoProvider::None => "none",
            NoProvider::HintNone => "none (hint none)",
            NoProvider::FindRefused => "none (find refused)",
        })
    }
}

/// What Neovim told of itself, for `status`.
#[derive(Debug, Default)]
pub(crate) struct Told {
    /// Its global working directory, once told.
    pub(crate) dir: Option<Vec<u8>>,
    /// The ptys of its running `:terminal` jobs.
    pub(crate) terminals: Vec<Vec<u8>>,
}

impl Daemon {
    /// A control request, `hello`, on `stream`: answered, and closed.
    pub(super) fn on_control(&mut self, stream: UnixStream, hello: Hello) -> Result<()> {
        let same = hello.v == ATTACH_PROTOCOL_VERSION;
        match hello.purpose {
            Purpose::Status { .. } => {
                log::log_debug!("{NAME} daemon: status asked ({})", hello.build);
                let status = same.then(|| self.status());
                attach_conn::send_reply(
                    &stream,
                    attach_conn::reply(&self.name, true, None, status),
                );
                Ok(())
            }
            Purpose::Stop { force, .. } => self.on_stop(stream, force),
            _ => unreachable!("on_hello passes only status and stop"),
        }
    }

    /// `stop`, `force`d or not: refused while something is unsaved, unless
    /// forced; else answered, and the daemon ends.
    fn on_stop(&mut self, stream: UnixStream, force: bool) -> Result<()> {
        let unsaved = self.lifetime.unsaved();
        if !force && !unsaved.is_empty() {
            let home = std::env::var_os("HOME");
            let files = shown_files(
                unsaved,
                self.told.dir.as_deref(),
                home.as_deref().map(|h| h.as_encoded_bytes()),
            );
            let message = format!(
                "{} has unsaved changes: {files} (save them, or kill --force to keep them only \
                 in swap files)",
                self.name
            );
            log_info!("{NAME} daemon: refused to stop: {message}");
            attach_conn::send_reply(
                &stream,
                attach_conn::reply(&self.name, false, Some(message), None),
            );
            return Ok(());
        }
        attach_conn::send_reply(&stream, attach_conn::reply(&self.name, true, None, None));
        drop(stream);
        if self.stopping {
            return Ok(());
        }
        log_info!(
            "{NAME} daemon: stopped by request{}",
            if force { ", forced" } else { "" }
        );
        self.stop_by_request()
    }

    /// Ends the daemon, as `stop` asked: what waits is cancelled, every
    /// attach ends `stopped`, and the terminals it requested close. It
    /// stops once the pane provider has closed them, or after
    /// `STOP_WAIT_MS`.
    fn stop_by_request(&mut self) -> Result<()> {
        self.stopping = true;
        // hellos not yet answered
        for (_, p) in std::mem::take(&mut self.pending) {
            self.event_loop.remove(p.stream.as_raw_fd())?;
            attach_conn::refuse_stopping(p.stream);
        }
        for c in std::mem::take(&mut self.nested_checks) {
            attach_conn::refuse_stopping(c.arrived.stream);
        }
        // the acts not yet asked are for panes that are going
        let dropped = self.asks.drop_waiting();
        if dropped > 0 {
            log_info!("{NAME} daemon: dropped {dropped} act(s) not yet asked");
        }
        let closes: Vec<TerminalId> = self
            .panes
            .list
            .iter()
            .filter(|p| !p.is_self_attached() && p.closing != Some(Close::RequestFailed))
            .filter_map(|p| self.close_id(p))
            .collect();
        self.panes.unregister_all(&self.event_loop);
        self.panes.deinit(Ending::Stopped, None);
        for t in closes {
            self.act(asks::close(t));
        }
        self.drain_provider()?;
        if self.stopping_done() {
            return Ok(());
        }
        self.add_timer(Token::StopTimer, STOP_WAIT_MS)
    }

    /// Stopping by request, it stops once no act is left asked: true if it
    /// has.
    pub(super) fn stopping_done(&mut self) -> bool {
        if self.stopping && self.stopped.is_none() && !self.asks.busy() {
            self.stop(StopReason::Stopped);
        }
        self.stopped.is_some()
    }

    /// The stop timer fired: the closes still unanswered are left.
    pub(super) fn stop_wait_over(&mut self) {
        if self.stopping && self.stopped.is_none() {
            log_info!("{NAME} daemon: the pane provider didn't close every terminal in time");
            self.stop(StopReason::Stopped);
        }
    }

    /// What `status` adds while versions agree, from what the daemon
    /// tracks.
    pub(super) fn status(&self) -> Status {
        let names = |has: &dyn Fn(Capability) -> bool| {
            Capability::ALL
                .into_iter()
                .filter(|&c| has(c))
                .map(|c| c.name().to_string())
                .collect::<Vec<_>>()
        };
        let caps = self.provider.as_ref().map(|p| p.capabilities());
        Status {
            panes: self.attached_panes(),
            unsaved: self.lifetime.unsaved().to_vec(),
            provider: self.provider_shown(),
            dir: self.told.dir.clone(),
            capabilities: Some(names(&|c| caps.is_some_and(|s| s.has(c)))),
            unsupported: Some(names(&|c| {
                self.provider.is_some() && self.asks.unsupported(c)
            })),
            terminals: Some(self.told.terminals.clone()),
            ..attach_conn::idle_status(&self.sock, &self.log)
        }
    }

    /// Attached Neovim panes: those with a terminal, window and float group
    /// panes alike.
    fn attached_panes(&self) -> u32 {
        let n = self
            .panes
            .list
            .iter()
            .filter(|p| p.terminal_surface.is_some() && p.closing.is_none())
            .count();
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    /// The pane provider as `status` shows it: its name, `(choosing)` while
    /// its `find` hasn't confirmed it, or `none` and why.
    pub(super) fn provider_shown(&self) -> String {
        match &self.provider {
            Some(_) if self.choosing.is_some() => "(choosing)".to_string(),
            Some(p) => p.name().to_string(),
            None => self.no_provider.to_string(),
        }
    }
}
