//! The kqueue backend, for macOS and FreeBSD. Uses three filters: fd
//! readable, signals and timers.

use std::os::unix::io::RawFd;

use super::{Error, Event, Kind, Result};

pub struct Backend {
    kq: RawFd,
}

impl Backend {
    pub fn init() -> Result<Backend> {
        // SAFETY: plain syscalls.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(Error::CreateFailed);
        }
        // not inherited by children such as Neovim
        unsafe { libc::fcntl(kq, libc::F_SETFD, libc::FD_CLOEXEC) };
        Ok(Backend { kq })
    }

    pub fn add_read(&self, fd: RawFd, token: usize) -> Result<()> {
        self.change(fd as usize, libc::EVFILT_READ, libc::EV_ADD, token)
    }

    pub fn remove(&self, fd: RawFd) -> Result<()> {
        self.change(fd as usize, libc::EVFILT_READ, libc::EV_DELETE, 0)
    }

    /// Watches `sig`. The caller has already made it ignored.
    pub fn add_signal(&self, sig: libc::c_int, token: usize) -> Result<()> {
        self.change(sig as usize, libc::EVFILT_SIGNAL, libc::EV_ADD, token)
    }

    pub fn add_timer(&self, id: usize, ms: u32, token: usize) -> Result<()> {
        self.change_data(
            id,
            libc::EVFILT_TIMER,
            libc::EV_ADD | libc::EV_ONESHOT,
            token,
            ms as i64,
        )
    }

    pub fn wait<'a>(&self, buf: &'a mut [Event]) -> Result<&'a [Event]> {
        // SAFETY: an all-zero kevent is a valid value.
        let mut raw: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
        let max = buf.len().min(raw.len()) as libc::c_int;
        let n: usize = loop {
            // SAFETY: no changes in, and `raw` has room for `max` events out.
            let rc = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    raw.as_mut_ptr(),
                    max,
                    std::ptr::null(),
                )
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
                    libc::EVFILT_SIGNAL => Kind::Signal,
                    libc::EVFILT_TIMER => Kind::Timer,
                    _ => unreachable!(), // only these are registered
                },
                eof: e.flags & libc::EV_EOF != 0,
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
        // SAFETY: one change in, no events out.
        let rc = unsafe {
            libc::kevent(
                self.kq,
                &change,
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
        unsafe { libc::close(self.kq) };
    }
}
