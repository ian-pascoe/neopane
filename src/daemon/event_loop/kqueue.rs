//! The kqueue backend, for macOS and FreeBSD. Uses five filters: fd
//! readable, fd writable, signals, timers and process exits.

use std::os::unix::io::RawFd;

use super::{Error, Event, Kind, Result};

pub(super) struct Backend {
    kq: RawFd,
}

impl Backend {
    pub(super) fn init() -> Result<Backend> {
        // SAFETY: plain syscalls.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(Error::CreateFailed);
        }
        // not inherited by children such as Neovim
        // SAFETY: `kq` is the open kqueue fd made above.
        unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) };
        Ok(Backend { kq })
    }

    pub(super) fn add_read(&self, fd: RawFd, token: usize) -> Result<()> {
        self.change(fd as usize, libc::EVFILT_READ, libc::EV_ADD, token)
    }

    pub(super) fn add_write(&self, fd: RawFd, token: usize) -> Result<()> {
        self.change(fd as usize, libc::EVFILT_WRITE, libc::EV_ADD, token)
    }

    /// Fails only if a write watch was there and couldn't be removed.
    pub(super) fn remove_write(&self, fd: RawFd) -> Result<()> {
        match self.change(fd as usize, libc::EVFILT_WRITE, libc::EV_DELETE, 0) {
            Err(_) if std::io::Error::last_os_error().raw_os_error() != Some(libc::ENOENT) => {
                Err(Error::WatchFailed)
            }
            _ => Ok(()),
        }
    }

    pub(super) fn remove(&self, fd: RawFd) -> Result<()> {
        self.remove_write(fd)?;
        self.change(fd as usize, libc::EVFILT_READ, libc::EV_DELETE, 0)
    }

    /// Watches `sig`. The caller has already made it ignored.
    pub(super) fn add_signal(&self, sig: libc::c_int, token: usize) -> Result<()> {
        self.change(sig as usize, libc::EVFILT_SIGNAL, libc::EV_ADD, token)
    }

    /// Called as the loop ends, before the old signal actions come back.
    /// Nothing to release: kqueue watches signals without blocking them.
    pub(super) fn release_signals(&self) {}

    pub(super) fn add_timer(&self, id: usize, ms: u32, token: usize) -> Result<()> {
        self.change_data(
            id,
            libc::EVFILT_TIMER,
            libc::EV_ADD | libc::EV_ONESHOT,
            token,
            ms as i64,
        )
    }

    pub(super) fn add_exit(&self, pid: libc::pid_t, token: usize) -> Result<()> {
        // SAFETY: an all-zero kevent is a valid value.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = pid as usize;
        change.filter = libc::EVFILT_PROC;
        change.flags = libc::EV_ADD | libc::EV_ONESHOT;
        change.fflags = libc::NOTE_EXIT;
        change.udata = token as *mut libc::c_void;
        self.apply(&change)
    }

    pub(super) fn fd(&self) -> RawFd {
        self.kq
    }

    pub(super) fn wait<'a>(&self, buf: &'a mut [Event], block: bool) -> Result<&'a [Event]> {
        // SAFETY: an all-zero kevent is a valid value.
        let mut raw: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
        let max = buf.len().min(raw.len()) as libc::c_int;
        let now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let timeout: *const libc::timespec = if block { std::ptr::null() } else { &now };
        let n: usize = loop {
            // SAFETY: no changes in, and `raw` has room for `max` events out.
            let rc = unsafe {
                libc::kevent(self.kq, std::ptr::null(), 0, raw.as_mut_ptr(), max, timeout)
            };
            if rc >= 0 {
                break rc as usize;
            }
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return Err(Error::WaitFailed);
            }
        };

        for (e, out) in raw[..n].iter().zip(buf[..n].iter_mut()) {
            if e.flags & libc::EV_ERROR != 0 {
                return Err(Error::WaitFailed);
            }
            *out = Event {
                token: e.udata as usize,
                kind: match e.filter {
                    libc::EVFILT_READ => Kind::Read,
                    libc::EVFILT_WRITE => Kind::Write,
                    libc::EVFILT_SIGNAL => Kind::Signal,
                    libc::EVFILT_TIMER => Kind::Timer,
                    libc::EVFILT_PROC => Kind::Exit,
                    _ => unreachable!(), // only these are registered
                },
                // a process's exit says EV_EOF too
                eof: e.filter != libc::EVFILT_PROC && e.flags & libc::EV_EOF != 0,
            };
        }
        Ok(&buf[..n])
    }

    /// Registers one change with the kqueue.
    fn change(&self, ident: usize, filter: i16, flags: u16, token: usize) -> Result<()> {
        self.change_data(ident, filter, flags, token, 0)
    }

    fn change_data(
        &self,
        ident: usize,
        filter: i16,
        flags: u16,
        token: usize,
        data: i64,
    ) -> Result<()> {
        // SAFETY: an all-zero kevent is a valid value.
        let mut change: libc::kevent = unsafe { std::mem::zeroed() };
        change.ident = ident;
        change.filter = filter;
        change.flags = flags;
        change.fflags = 0;
        change.data = data as _; // isize on macOS, i64 on FreeBSD
        change.udata = token as *mut libc::c_void;
        self.apply(&change)
    }

    fn apply(&self, change: &libc::kevent) -> Result<()> {
        // SAFETY: one change in, no events out.
        let rc = unsafe {
            libc::kevent(
                self.kq,
                change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            return Err(Error::WatchFailed);
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // SAFETY: `kq` is open and owned by this backend, and closed only here.
        unsafe { libc::close(self.kq) };
    }
}
