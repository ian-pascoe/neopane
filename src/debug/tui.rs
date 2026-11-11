//! `neorex debug-tui`: runs Neovim in this terminal through neorex's own
//! grid and renderer. Ctrl+Q or :q quits.

use std::ffi::{OsStr, OsString};
use std::io;

use super::Result;
use crate::name::NAME;
use crate::nvim::forwarder::Forwarder;
use crate::nvim::nvim::{Nvim, UiOptions};
use crate::nvim::rpc;
use crate::screen::grids::Grids;
use crate::screen::render;
use crate::term::input;
use crate::term::modes;
use crate::term::signals::Signals;
use crate::term::terminal::Terminal;

/// Runs `nvim --embed` plus `nvim_args`.
pub fn run<S: AsRef<OsStr>>(nvim_args: &[S]) -> Result<()> {
    let mut term = Terminal::init(libc::STDIN_FILENO)?;
    let result = run_in(&mut term, nvim_args);
    term.deinit();
    result
}

fn run_in<S: AsRef<OsStr>>(term: &mut Terminal, nvim_args: &[S]) -> Result<()> {
    let mut signals = Signals::init(&[libc::SIGWINCH])?;

    term.writer.extend_from_slice(modes::EDITOR_ON.as_bytes());
    term.flush()?;
    let result = drive(term, &mut signals, nvim_args);
    term.writer.extend_from_slice(modes::EDITOR_OFF.as_bytes());
    result
}

fn drive<S: AsRef<OsStr>>(
    term: &mut Terminal,
    signals: &mut Signals,
    nvim_args: &[S],
) -> Result<()> {
    let mut argv: Vec<OsString> = vec!["nvim".into(), "--embed".into()];
    argv.extend(nvim_args.iter().map(|a| a.as_ref().to_os_string()));

    let mut nvim = Nvim::spawn(&argv)?;

    // without ext_multigrid everything is grid 1
    let mut grids = Grids::default();

    let ws = term.size()?;
    // termcolors: unset colors stay the terminal's, so the theme shows through
    nvim.ui_attach(
        ws.ws_col as u32,
        ws.ws_row as u32,
        UiOptions {
            ext_termcolors: true,
            ..UiOptions::default()
        },
    )?;

    let mut forwarder = Forwarder::default();
    let mut buf = [0u8; 4096];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: term.fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: signals.read_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: nvim.read_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // an ESC that ended a read waits for the rest of its sequence, a while
        let wait = if forwarder.escape_pending() {
            input::ESCAPE_TIMEOUT_MS as libc::c_int
        } else {
            -1
        };
        // SAFETY: `fds` is a valid array of three pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::other("PollFailed").into());
        }
        if ready == 0 {
            // no more input came: the ESC was the Escape key
            forwarder.flush_pending(&mut nvim)?;
            continue;
        }

        if fds[1].revents != 0 {
            let fired = signals.drain()?;
            if fired.contains(libc::SIGWINCH) {
                let size = term.size()?;
                nvim.ui_try_resize(size.ws_col as u32, size.ws_row as u32)?;
            }
        }

        if fds[2].revents != 0 {
            if !nvim.read()? {
                return Ok(());
            }
            while let Some(frame) = nvim.next()? {
                match frame.message()? {
                    rpc::Message::Notification { method, mut params } => {
                        if method == b"redraw"
                            && grids.apply_redraw(&mut params)?
                            && let Some(grid) = grids.grids.get_mut(&1)
                        {
                            render::frame(grid, &grids.style, &mut term.writer);
                            term.flush()?;
                        }
                    }
                    // nothing here serves requests; neovim would wait forever
                    rpc::Message::Request { id, .. } => {
                        nvim.respond_error(id, &format!("{NAME} debug-tui serves no requests"))?;
                    }
                    rpc::Message::Response { .. } => {}
                }
            }
        }

        if fds[0].revents != 0 {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::read(term.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(io::Error::other("ReadFailed").into());
            }
            if n == 0 {
                return Ok(());
            }
            if !forwarder.feed(&mut nvim, &buf[..n as usize])? {
                return Ok(());
            }
        }
    }
}
