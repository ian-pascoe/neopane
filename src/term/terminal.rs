//! A terminal's output, buffered. Its settings are its own process's
//! (`super::raw`).

use std::io;
use std::os::fd::RawFd;

pub(crate) struct Terminal {
    /// Not necessarily stdin; the daemon drives panes' terminals.
    pub fd: RawFd,
    /// Buffered output. Write a frame, then flush.
    ///
    /// A growable `Vec`, written to the fd only by `flush`.
    pub writer: Vec<u8>,
}

impl Terminal {
    /// Output to `fd`, nothing buffered yet. Its settings are left alone.
    pub(crate) fn new(fd: RawFd) -> Terminal {
        Terminal {
            fd,
            writer: Vec::new(),
        }
    }

    /// Current size in cells.
    pub(crate) fn size(&self) -> io::Result<libc::winsize> {
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
    pub(crate) fn flush(&mut self) -> io::Result<()> {
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

/// A pty of `cols` x `rows`: master and slave fds.
#[cfg(test)]
pub(crate) fn open_pty(cols: u16, rows: u16) -> (RawFd, RawFd) {
    let (mut master, mut slave) = (-1, -1);
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
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
    (master, slave)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Runs size and flush over a pty.
    #[test]
    fn output_over_a_pty() {
        let (master, slave) = open_pty(30, 7);
        let mut t = Terminal::new(slave);
        let size = t.size().unwrap();
        assert_eq!((7, 30), (size.ws_row, size.ws_col));

        t.writer.extend_from_slice(b"hello");
        t.flush().unwrap();
        assert!(t.writer.is_empty());
        let mut buf = [0u8; 16];
        // SAFETY: `buf` is valid for 16 bytes.
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(b"hello", &buf[..n as usize]);

        // SAFETY: closing the fds `open_pty` made.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }
}
