//! Turns Unix signals into fd events using the self-pipe trick.

use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI32, Ordering};

pub struct Signals {
    /// Read end. Poll it; readable when a signal has fired.
    pub read_fd: RawFd,
    /// Write end, written by the handler.
    pub write_fd: RawFd,
    /// Old actions by signal number, restored on deinit.
    old: [Option<libc::sigaction>; 32],
}

/// Write end for the handler, which can't see `self`. -1 if no Signals exists.
static HANDLER_FD: AtomicI32 = AtomicI32::new(-1);

/// The calling thread's `errno`.
fn errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// `pipe` with both ends non-blocking and close-on-exec (macOS has no
/// `pipe2`).
fn pipe_nonblock_cloexec() -> io::Result<[RawFd; 2]> {
    let mut fds: [RawFd; 2] = [-1, -1];
    // SAFETY: `fds` has room for the two fds `pipe` writes.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in fds {
        // SAFETY: `fd` is a pipe end we just made.
        let ok = unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            let fd_flags = libc::fcntl(fd, libc::F_GETFD);
            fl != -1
                && fd_flags != -1
                && libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK) != -1
                && libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) != -1
        };
        if !ok {
            let err = io::Error::last_os_error();
            // SAFETY: closing the fds we made.
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(err);
        }
    }
    Ok(fds)
}

fn sigaction(sig: i32, action: &libc::sigaction, old: Option<&mut libc::sigaction>) {
    let old_ptr = match old {
        Some(o) => o as *mut libc::sigaction,
        None => std::ptr::null_mut(),
    };
    // SAFETY: both pointers are valid (or null for `old`).
    let rc = unsafe { libc::sigaction(sig, action, old_ptr) };
    // Only an invalid signal number fails, which is a bug in the caller.
    debug_assert_eq!(rc, 0);
}

/// Writes the signal number to the pipe. Only async-signal-safe calls here.
extern "C" fn handle(sig: libc::c_int) {
    // write can clobber errno mid-check in the interrupted code
    // SAFETY: `__error` returns this thread's errno slot.
    let saved = unsafe { *libc::__error() };
    let byte = [sig as u8];
    // SAFETY: `write` is async-signal-safe; `byte` is a valid 1-byte buffer.
    unsafe {
        libc::write(HANDLER_FD.load(Ordering::Relaxed), byte.as_ptr().cast(), 1);
        *libc::__error() = saved;
    }
}

impl Signals {
    /// Creates the pipe and installs the handler for each signal. One per process.
    pub fn init(signals: &[i32]) -> io::Result<Signals> {
        // one Signals per process
        assert_eq!(HANDLER_FD.load(Ordering::Relaxed), -1);

        // non-blocking so neither end can hang; close-on-exec so children don't inherit it
        let fds = pipe_nonblock_cloexec()?;
        let mut this = Signals {
            read_fd: fds[0],
            write_fd: fds[1],
            old: [None; 32],
        };

        // before installing, so the handler never sees -1
        HANDLER_FD.store(this.write_fd, Ordering::Relaxed);

        // SAFETY: an all-zero `sigaction` is valid; the fields are set below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handle as extern "C" fn(libc::c_int) as usize;
        // SAFETY: `sa_mask` is a valid sigset_t.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        action.sa_flags = libc::SA_RESTART;

        for &sig in signals {
            let n = sig as usize;
            assert!(n < this.old.len());
            // SAFETY: an all-zero `sigaction` is a valid out-parameter.
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            sigaction(sig, &action, Some(&mut old));
            this.old[n] = Some(old);
        }

        Ok(this)
    }

    /// Restores the old actions, then closes the pipe.
    pub fn deinit(&mut self) {
        if self.read_fd < 0 {
            return; // already deinitialised
        }
        // restore first, so no handler writes to a closed fd
        for (n, maybe_old_action) in self.old.iter().enumerate() {
            if let Some(old_action) = maybe_old_action {
                sigaction(n as i32, old_action, None);
            }
        }
        HANDLER_FD.store(-1, Ordering::Relaxed);
        // SAFETY: closing the pipe we made.
        unsafe {
            libc::close(self.read_fd);
            libc::close(self.write_fd);
        }
        // Mark it closed.
        self.read_fd = -1;
        self.write_fd = -1;
        self.old = [None; 32];
    }

    /// Empties the pipe and returns which signals fired. Call when read_fd is readable.
    pub fn drain(&mut self) -> io::Result<Set> {
        let mut set = Set::default();
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: `buf` is valid for `buf.len()` bytes.
            let n = unsafe { libc::read(self.read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                match errno() {
                    libc::EINTR => continue,
                    libc::EAGAIN => return Ok(set), // pipe empty
                    _ => return Err(io::Error::last_os_error()),
                }
            }
            // EOF means every write end is closed, but we hold one
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "WriteEndClosed",
                ));
            }
            for &byte in &buf[..n as usize] {
                // `IntegerBitSet(32).set` needs byte < 32; the handler only
                // writes signal numbers, which are.
                set.bits |= 1u32 << (byte & 31);
            }
        }
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Which signals fired since the last drain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Set {
    pub bits: u32,
}

impl Set {
    pub fn contains(self, sig: i32) -> bool {
        (0..32).contains(&sig) && self.bits & (1u32 << sig) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_then_drain() {
        let mut s = Signals::init(&[libc::SIGWINCH]).unwrap();
        // SAFETY: raising a signal we handle.
        unsafe {
            libc::raise(libc::SIGWINCH);
            libc::raise(libc::SIGWINCH);
        }
        let fired = s.drain().unwrap();
        assert!(fired.contains(libc::SIGWINCH));
        assert!(!fired.contains(libc::SIGTERM));
        s.deinit();
    }

    #[test]
    fn empty_drain() {
        let mut s = Signals::init(&[libc::SIGWINCH]).unwrap();
        assert_eq!(0u32, s.drain().unwrap().bits);
        s.deinit();
    }
}
