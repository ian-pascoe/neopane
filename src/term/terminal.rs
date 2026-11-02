//! A terminal in raw mode, with buffered output.

use std::io;
use std::os::fd::RawFd;

pub struct Terminal {
    /// Not necessarily stdin; the daemon will drive panes' terminals.
    pub fd: RawFd,
    /// Settings before raw mode, restored on deinit.
    original: libc::termios,
    /// Buffered output. Write a frame, then flush.
    ///
    /// A growable `Vec`, written to the fd only by `flush` (so `init` takes no
    /// buffer).
    pub writer: Vec<u8>,
}

/// Calls `f` until it isn't interrupted by a signal, as `tcgetattr` and
/// `tcsetattr` need.
fn retry(mut f: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if f() == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

impl Terminal {
    /// Switches `fd` to raw mode. Fails if it isn't a terminal.
    pub fn init(fd: RawFd) -> io::Result<Terminal> {
        // SAFETY: an all-zero termios is a valid out-parameter for tcgetattr.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `original` is a valid termios.
        retry(|| unsafe { libc::tcgetattr(fd, &mut original) })?;

        let mut raw = original;
        // SAFETY: `raw` is a valid termios.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: `raw` is a valid termios.
        retry(|| unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) })?;

        Ok(Terminal {
            fd,
            original,
            writer: Vec::new(),
        })
    }

    /// Flushes, then restores the original settings. Errors are ignored.
    pub fn deinit(&mut self) {
        let _ = self.flush();
        // SAFETY: `original` is the termios `init` read.
        let _ = retry(|| unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.original) });
    }

    /// Current size in cells.
    pub fn size(&self) -> io::Result<libc::winsize> {
        let mut ws = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: TIOCGWINSZ writes a winsize through the pointer.
        if unsafe { libc::ioctl(self.fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
            return Err(io::Error::other("NoTerminalSize"));
        }
        Ok(ws)
    }

    /// Sends the buffered frame.
    pub fn flush(&mut self) -> io::Result<()> {
        write_all(self.fd, &self.writer)?;
        self.writer.clear();
        Ok(())
    }
}

/// Writes all of `bytes`, looping over short writes and EINTR.
fn write_all(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        // SAFETY: `remaining` is valid for `remaining.len()` bytes.
        let n = unsafe { libc::write(fd, remaining.as_ptr().cast(), remaining.len()) };
        if n >= 0 {
            offset += n as usize;
        } else if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        } else {
            return Err(io::Error::other("WriteFailed"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Runs init, size, flush and deinit over a pty.
    #[test]
    fn raw_mode_over_a_pty() {
        let (mut master, mut slave) = (-1, -1);
        let mut ws = libc::winsize {
            ws_row: 7,
            ws_col: 30,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid out-pointers; no name or termios.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut ws,
            )
        };
        assert_eq!(0, rc);

        let mut t = Terminal::init(slave).unwrap();
        let size = t.size().unwrap();
        assert_eq!((7, 30), (size.ws_row, size.ws_col));

        t.writer.extend_from_slice(b"hello");
        t.flush().unwrap();
        assert!(t.writer.is_empty());
        let mut buf = [0u8; 16];
        // SAFETY: `buf` is valid for 16 bytes.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(b"hello", &buf[..n as usize]);

        t.deinit();
        // SAFETY: closing the fds `openpty` made.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn not_a_terminal() {
        // SAFETY: a throwaway pipe.
        let mut fds = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        assert!(Terminal::init(fds[0]).is_err());
        // SAFETY: closing the pipe above.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }
}
