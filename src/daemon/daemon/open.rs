//! Waited files (ADR 0015, #94 decisions 17 and 28): an `open` control
//! request opens its file in a new window split off Neovim's current one. It
//! is answered once Neovim has the window, or with `wait`, once the file is
//! done: no window shows its buffer, or the buffer went. Neovim is asked
//! without blocking the loop, as it may wait at a prompt (E325).

use std::os::unix::net::UnixStream;

use super::*;
use crate::attach::{Hello, Purpose, Reply};
use crate::daemon::attach_conn;
use crate::daemon::control::shown_files;

/// An `open` asked of Neovim, unanswered.
pub(crate) struct Asked {
    pub(super) req: u32,
    stream: UnixStream,
    wait: bool,
    /// The file, as messages show it.
    shown: String,
}

/// A waited file Neovim has open, until it's done.
pub(crate) struct Waiting {
    buf: i64,
    /// Its buffer's full name, as the unsaved changes name it.
    name: Vec<u8>,
    stream: UnixStream,
    shown: String,
}

/// The `open` requests the daemon has yet to answer.
#[derive(Default)]
pub(crate) struct Opens {
    pub(super) asked: Vec<Asked>,
    pub(super) waiting: Vec<Waiting>,
}

impl Daemon {
    /// An `open` control request, `hello`, on `stream`: its file is opened,
    /// and it is answered once Neovim has it, or with `wait`, once it's done.
    pub(super) fn on_open(&mut self, stream: UnixStream, hello: Hello) -> Result<()> {
        let Purpose::Open {
            cwd,
            file,
            line,
            wait,
            ..
        } = hello.purpose
        else {
            unreachable!("on_hello passes only open");
        };
        let file = attach_conn::file_against_cwd(&file, &cwd);
        let home = std::env::var_os("HOME");
        let home = home.as_deref().map(|h| h.as_encoded_bytes());
        let shown = shown_files(std::slice::from_ref(&file), Some(&cwd), home);
        if !self.nvim.ready {
            let message = format!("{}'s Neovim isn't ready to open {shown}", self.name);
            self.answer(&stream, false, Some(message), None);
            return Ok(());
        }
        log_info!(
            "{NAME} daemon: opening {shown}{}",
            if wait { ", waited" } else { "" }
        );
        let req = windows::edit(&mut self.nvim.client, &file, line, wait);
        self.nvim.client.flush()?;
        self.opens.asked.push(Asked {
            req,
            stream,
            wait,
            shown,
        });
        Ok(())
    }

    /// Neovim answered request `id`: true if it opened an `open`'s file,
    /// which is answered now, unless it's waited for.
    pub(super) fn on_open_answer(
        &mut self,
        id: u32,
        err: Option<&[u8]>,
        mut result: Decoder<'_>,
    ) -> Result<bool> {
        let Some(i) = self.opens.asked.iter().position(|a| a.req == id) else {
            return Ok(false);
        };
        let asked = self.opens.asked.swap_remove(i);
        if let Some(e) = err {
            let message = format!("{} didn't open: {}", asked.shown, neovim_error(e));
            log_info!("{NAME} daemon: {message}");
            self.answer(&asked.stream, false, Some(message), None);
            return Ok(true);
        }
        let edited = windows::parse_edit(&mut result)?;
        match (asked.wait, edited.watched) {
            (false, _) => self.answer(&asked.stream, true, None, None),
            // done already
            (true, false) => self.answer(&asked.stream, true, None, Some(true)),
            (true, true) => self.opens.waiting.push(Waiting {
                buf: edited.buf,
                name: edited.name,
                stream: asked.stream,
                shown: asked.shown,
            }),
        }
        Ok(true)
    }

    /// Waited buffer `buf` is done with: its opens are answered. Its last
    /// window went `by_pane`, as its terminal hung up, with unsaved changes
    /// in it, it's no success.
    pub(super) fn on_done(&mut self, buf: i64, by_pane: bool) {
        let (done, left) = std::mem::take(&mut self.opens.waiting)
            .into_iter()
            .partition(|w| w.buf == buf);
        self.opens.waiting = left;
        for w in done {
            let unsaved = by_pane && self.lifetime.unsaved().contains(&w.name);
            let message =
                unsaved.then(|| format!("{}'s terminal closed with unsaved changes", w.shown));
            log_info!(
                "{NAME} daemon: {} is done{}",
                w.shown,
                if unsaved { ", unsaved" } else { "" }
            );
            self.answer(&w.stream, !unsaved, message, Some(true));
        }
    }

    /// Stopping by request: every open still unanswered is told so.
    pub(super) fn stop_opens(&mut self) {
        let opens = std::mem::take(&mut self.opens);
        let streams = opens
            .asked
            .into_iter()
            .map(|a| (a.stream, a.shown, a.wait.then_some(true)))
            .chain(
                opens
                    .waiting
                    .into_iter()
                    .map(|w| (w.stream, w.shown, Some(true))),
            );
        for (stream, shown, waited) in streams {
            let message = format!("{} was stopped before {shown} was done", self.name);
            self.answer(&stream, false, Some(message), waited);
        }
    }

    /// Answers an `open` on `stream`.
    fn answer(&self, stream: &UnixStream, ok: bool, message: Option<String>, waited: Option<bool>) {
        let reply = Reply {
            waited,
            ..attach_conn::reply(&self.name, ok, message, None)
        };
        attach_conn::send_reply(stream, reply);
    }
}

/// Neovim's error `e`, from its error number on, as `E325: ATTENTION`; or
/// its first line.
fn neovim_error(e: &[u8]) -> String {
    let e = String::from_utf8_lossy(e);
    let line = e.lines().next().unwrap_or_default();
    let bytes = line.as_bytes();
    let at = (0..bytes.len()).find(|&i| {
        bytes[i] == b'E'
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
            && bytes[i + 1..]
                .iter()
                .position(|b| !b.is_ascii_digit())
                .is_some_and(|n| n > 0 && bytes[i + 1 + n] == b':')
    });
    line[at.unwrap_or(0)..].to_string()
}

#[cfg(test)]
mod tests {
    use super::neovim_error;

    #[test]
    fn neovim_errors_are_shown_from_their_number() {
        assert_eq!(
            "E325: ATTENTION",
            neovim_error(b"Error executing lua: [string]:3: Vim(split):E325: ATTENTION\nmore")
        );
        assert_eq!("no number", neovim_error(b"no number"));
        assert_eq!("E1: x", neovim_error(b"E1: x"));
    }
}
