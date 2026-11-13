//! `neopane debug-keys`: shows each input event with the raw bytes that
//! made it, so you can see what the terminal sends. Ctrl+Q quits.

use std::io::{self, Write};

use super::Result;
use crate::term::input::{self, Event, Key};
use crate::term::raw::Saved;
use crate::term::signals::Signals;
use crate::term::terminal::Terminal;

/// Terminal modes `debug-keys` turns on, and the sequences that turn them
/// off again.
const MODES_ON: &str = concat!(
    "\x1b[?1049h",                 // alternate screen: keep the shell's scrollback clean
    "\x1b[?2004h", // bracketed paste: pastes arrive wrapped in ESC[200~ ... ESC[201~
    "\x1b[?1004h", // focus reporting: ESC[I and ESC[O
    "\x1b[?1002h", // mouse: presses, releases and drags
    "\x1b[?1006h", // mouse: SGR encoding (ESC[<b;x;yM), no limit on size
    input::kitty_push_literal!(), // kitty keyboard protocol, after the screen switch
    input::kitty_query_literal!(), // ask for its flags: the parser reads the answer
);
// Undone in reverse: pop kitty's flags while still on the alternate screen.
const MODES_OFF: &str = concat!(
    input::kitty_pop_literal!(),
    "\x1b[?1006l\x1b[?1002l\x1b[?1004l\x1b[?2004l\x1b[?1049l"
);

pub fn run() -> Result<()> {
    let saved = Saved::raw(libc::STDIN_FILENO)?;
    let mut term = Terminal::new(libc::STDIN_FILENO);
    let result = run_in(&mut term);
    // after the modes are off (run_in wrote them), the terminal's settings come back
    let _ = term.flush();
    saved.restore();
    result
}

fn run_in(term: &mut Terminal) -> Result<()> {
    // before reading the size, so a resize in between isn't missed
    let mut signals = Signals::init(&[libc::SIGWINCH])?;

    term.writer.extend_from_slice(MODES_ON.as_bytes());
    let result = show_keys(term, &mut signals);
    // Runs before term.deinit, so the modes are off before the terminal's
    // settings come back.
    term.writer.extend_from_slice(MODES_OFF.as_bytes());
    result
}

fn show_keys(term: &mut Terminal, signals: &mut Signals) -> Result<()> {
    let ws = term.size()?;
    write!(
        term.writer,
        "\x1b[H\x1b[2Jdebug-keys: {}x{}. Type, paste, click or resize. Ctrl+Q quits.\r\n",
        ws.ws_col, ws.ws_row
    )?;
    term.flush()?;

    let mut parser = input::Parser::default();
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
        ];
        // an ESC that ended a read waits for the rest of its sequence, a while
        let wait = if parser.flush_due() {
            input::ESCAPE_TIMEOUT_MS as libc::c_int
        } else {
            -1
        };
        // SAFETY: `fds` is a valid array of two pollfds.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait) };
        if ready < 0 {
            // a signal interrupted poll; its byte is waiting in the pipe
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::other("PollFailed").into());
        }

        if fds[1].revents != 0 {
            let fired = signals.drain()?;
            if fired.contains(libc::SIGWINCH) {
                let ws = term.size()?;
                write!(term.writer, "resize {}x{}\r\n", ws.ws_col, ws.ws_row)?;
                term.flush()?;
            }
        }
        if ready == 0
            && let Some(event) = parser.flush_pending()
        {
            write!(term.writer, "  {event}\r\n")?;
            if is_quit(&event) {
                term.flush()?;
                return Ok(());
            }
            term.flush()?;
            continue;
        }
        if fds[0].revents == 0 {
            continue;
        }

        // SAFETY: `buf` is valid for `buf.len()` bytes.
        let n = unsafe { libc::read(term.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(io::Error::other("ReadFailed").into());
        }
        if n == 0 {
            return Ok(()); // the terminal went away
        }
        let bytes = &buf[..n as usize];

        // One line per read: the raw bytes, then the events they made.
        term.writer.extend_from_slice(b"read ");
        for b in bytes {
            write!(term.writer, "{b:02x} ")?;
        }
        term.writer.extend_from_slice(b"\r\n");

        parser.feed(bytes);
        while let Some(event) = parser.next() {
            write!(term.writer, "  {event}\r\n")?;
            if is_quit(&event) {
                term.flush()?;
                return Ok(());
            }
        }
        term.flush()?;
    }
}

fn is_quit(event: &Event) -> bool {
    match event {
        Event::Key(k) => k.mods.ctrl && !k.mods.alt && k.key == Key::Codepoint('q' as u32),
        _ => false,
    }
}
