//! Raw mode, and the terminal settings to put back after it.
//!
//! Only a terminal's own process sets them: the helper for a pane's terminal
//! (ADR 0013), and the debug commands for theirs. The daemon never does, so
//! a daemon that dies can't leave a terminal raw.

use std::io;
use std::os::fd::RawFd;

/// A terminal's settings and its open file's status flags, as they were
/// before raw mode.
pub(crate) struct Saved {
    fd: RawFd,
    termios: libc::termios,
    flags: libc::c_int,
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

impl Saved {
    /// Saves terminal `fd`'s settings and file status flags. Fails if it
    /// isn't a terminal.
    pub(crate) fn save(fd: RawFd) -> io::Result<Saved> {
        // SAFETY: an all-zero termios is a valid out-parameter.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `termios` is a valid termios.
        retry(|| unsafe { libc::tcgetattr(fd, &mut termios) })?;
        // SAFETY: F_GETFL only reads the file's flags.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Saved { fd, termios, flags })
    }

    /// Saves terminal `fd`'s settings, then puts it in raw mode.
    pub(crate) fn raw(fd: RawFd) -> io::Result<Saved> {
        let saved = Saved::save(fd)?;
        saved.make_raw()?;
        Ok(saved)
    }

    /// Raw mode, at once: what was typed and not yet read stays, for the
    /// daemon to read.
    pub(crate) fn make_raw(&self) -> io::Result<()> {
        let mut raw = self.termios;
        // SAFETY: `raw` is a valid termios.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: `raw` is a valid termios.
        retry(|| unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &raw) })
    }

    /// One key at a time, unechoed, with no signal keys: for a wait for a
    /// key, which `restore` ends.
    pub(crate) fn make_keyed(&self) -> io::Result<()> {
        let mut keyed = self.termios;
        keyed.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
        keyed.c_cc[libc::VMIN] = 1;
        keyed.c_cc[libc::VTIME] = 0;
        // SAFETY: `keyed` is a valid termios.
        retry(|| unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &keyed) })
    }

    /// The file status flags as saved: blocking again, if they were.
    pub(crate) fn restore_flags(&self) {
        // SAFETY: F_SETFL with the file's own saved flags.
        unsafe { libc::fcntl(self.fd, libc::F_SETFL, self.flags) };
    }

    /// Drops input not yet read, then puts back the settings and the file
    /// status flags. Errors are ignored: there is nothing left to do.
    pub(crate) fn restore(&self) {
        // SAFETY: plain calls on the terminal's fd; `termios` is the one
        // `save` read.
        unsafe { libc::tcflush(self.fd, libc::TCIFLUSH) };
        let _ = retry(|| unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.termios) });
        self.restore_flags();
    }
}

/// Puts terminal `fd` in raw mode, as a pane's helper does before it hands
/// the terminal over: for tests of the daemon, which reads keys from it.
#[cfg(test)]
pub(crate) fn as_the_helper_does(fd: RawFd) {
    Saved::raw(fd).unwrap();
}

/// Terminal `fd`'s settings now.
#[cfg(test)]
pub(crate) fn settings(fd: RawFd) -> libc::termios {
    // SAFETY: an all-zero termios is a valid out-parameter.
    let mut t: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `t` is a valid termios.
    assert_eq!(0, unsafe { libc::tcgetattr(fd, &mut t) });
    t
}

/// Whether two settings are the same, as far as raw mode goes. PENDIN, which
/// macOS sets as canonical mode comes back, says only that pending input is
/// to be reprinted.
#[cfg(test)]
pub(crate) fn same(a: &libc::termios, b: &libc::termios) -> bool {
    let l = |t: &libc::termios| t.c_lflag & !libc::PENDIN;
    (a.c_iflag, a.c_oflag, a.c_cflag, l(a)) == (b.c_iflag, b.c_oflag, b.c_cflag, l(b))
        && a.c_cc == b.c_cc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::terminal::open_pty;

    #[test]
    fn raw_mode_keeps_typeahead_and_restore_puts_back_settings_and_flags() {
        let (master, slave) = open_pty(80, 24);
        let before = settings(slave);
        // SAFETY: writing to the pty's master.
        assert_eq!(3, unsafe { libc::write(master, b"abc".as_ptr().cast(), 3) });
        let saved = Saved::raw(slave).unwrap();
        assert_eq!(0, settings(slave).c_lflag & libc::ICANON);
        let mut buf = [0u8; 8];
        // SAFETY: `buf` is valid for its length.
        let n = unsafe { libc::read(slave, buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(b"abc", &buf[..n as usize]);

        // as a daemon killed mid-write leaves it
        // SAFETY: plain fcntl calls on the pty.
        unsafe {
            let fl = libc::fcntl(slave, libc::F_GETFL);
            libc::fcntl(slave, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        saved.restore();
        assert!(same(&before, &settings(slave)));
        // SAFETY: F_GETFL only reads.
        assert_eq!(
            0,
            unsafe { libc::fcntl(slave, libc::F_GETFL) } & libc::O_NONBLOCK
        );
        // SAFETY: closing the pty.
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
    }

    #[test]
    fn not_a_terminal() {
        let mut fds = [0; 2];
        // SAFETY: a throwaway pipe.
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        assert!(Saved::raw(fds[0]).is_err());
        // SAFETY: closing the pipe above.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    /// The daemon never sets a terminal's settings (ADR 0013): in
    /// `src/daemon/`, only the helper does, through `Saved`.
    #[test]
    fn no_daemon_code_sets_a_terminals_settings() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/daemon");
        let mut files = vec![dir];
        let mut checked = 0;
        while let Some(path) = files.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    files.push(entry.unwrap().path());
                }
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            checked += 1;
            let text = std::fs::read_to_string(&path).unwrap();
            for bad in ["tcsetattr", "cfmakeraw", "TCSAFLUSH", "TCSADRAIN"] {
                assert!(!text.contains(bad), "{} calls {bad}", path.display());
            }
            let helper = path.file_name().is_some_and(|n| n == "pane_helper.rs");
            assert!(
                helper || !text.contains("Saved::"),
                "{} saves a terminal's settings: only the helper sets them",
                path.display()
            );
        }
        assert!(checked > 10, "found only {checked} files");
    }
}
