//! An event loop: register fds, signals and timers once, then wait for
//! whichever is ready. Each watch carries a token that comes back with its
//! events.
//!
//! The platform-neutral part lives here. A backend (`kqueue` for macOS and
//! FreeBSD) supplies `Backend`, which this file drives; to add a platform,
//! write a module with the same `Backend` methods and select it below.

use std::fmt;
use std::os::unix::io::RawFd;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod kqueue;
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use kqueue::Backend;

#[cfg(not(any(target_os = "macos", target_os = "freebsd")))]
compile_error!("no event-loop backend for this platform yet");

/// What went wrong with the event loop's backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Creating the backend's polling handle failed.
    CreateFailed,
    /// Adding or removing a watch failed.
    WatchFailed,
    /// Waiting for events failed.
    WaitFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CreateFailed => f.write_str("CreateFailed"),
            Error::WatchFailed => f.write_str("WatchFailed"),
            Error::WaitFailed => f.write_str("WaitFailed"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// The number of signals whose old action is kept.
const NSIG: usize = 32;

pub struct Loop {
    /// Declared before `old`, but dropped after `Loop::drop` restores the
    /// signal actions.
    backend: Backend,
    /// Signal actions before add_signal, restored on deinit.
    old: [Option<libc::sigaction>; NSIG],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub token: usize,
    pub kind: Kind,
    /// The other end hung up. There may still be bytes to read first.
    pub eof: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Read,
    Signal,
    Timer,
}

impl Default for Event {
    /// A blank event, for sizing the buffer `wait` fills.
    fn default() -> Event {
        Event {
            token: 0,
            kind: Kind::Read,
            eof: false,
        }
    }
}

impl Loop {
    pub fn init() -> Result<Loop> {
        Ok(Loop {
            backend: Backend::init()?,
            old: [None; NSIG],
        })
    }

    /// Restores the old signal actions, then closes the backend.
    /// (Dropping the loop does the same; this just makes it explicit.)
    pub fn deinit(self) {}

    /// Watches `fd` for reading. Adding it again replaces the token.
    pub fn add_read(&self, fd: RawFd, token: usize) -> Result<()> {
        self.backend.add_read(fd, token)
    }

    /// Stops watching `fd`. Not needed before closing it.
    pub fn remove(&self, fd: RawFd) -> Result<()> {
        self.backend.remove(fd)
    }

    /// Watches `sig`, and ignores it so its default action never runs.
    pub fn add_signal(&mut self, sig: libc::c_int, token: usize) -> Result<()> {
        let n = sig as usize;
        // only the first time, so a second call can't save our own SIG_IGN
        if self.old[n].is_none() {
            // SAFETY: sigaction with a null new action only reads the old one.
            let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
            unsafe { libc::sigaction(sig, std::ptr::null(), &mut old) };
            self.old[n] = Some(old);
        }
        // SAFETY: a zeroed sigaction has an empty mask and no flags.
        let mut ignore: libc::sigaction = unsafe { std::mem::zeroed() };
        ignore.sa_sigaction = libc::SIG_IGN;
        unsafe { libc::sigaction(sig, &ignore, std::ptr::null_mut()) };
        self.backend.add_signal(sig, token)
    }

    /// Fires once, `ms` milliseconds from now. Adding timer `id` again
    /// restarts it.
    pub fn add_timer(&self, id: usize, ms: u32, token: usize) -> Result<()> {
        self.backend.add_timer(id, ms, token)
    }

    /// Blocks until something is ready, then fills the start of `buf`.
    pub fn wait<'a>(&self, buf: &'a mut [Event]) -> Result<&'a [Event]> {
        self.backend.wait(buf)
    }
}

impl Drop for Loop {
    /// Restores the old signal actions; the backend closes itself after.
    fn drop(&mut self) {
        // restore first, so no signal is ignored once nothing watches it
        for (n, maybe_old_action) in self.old.iter().enumerate() {
            if let Some(old_action) = maybe_old_action {
                // SAFETY: restores an action `sigaction` itself returned.
                unsafe { libc::sigaction(n as libc::c_int, old_action, std::ptr::null_mut()) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pipe() -> std::result::Result<[RawFd; 2], &'static str> {
        let mut p: [RawFd; 2] = [0; 2];
        // SAFETY: `p` has room for the two fds.
        if unsafe { libc::pipe(p.as_mut_ptr()) } != 0 {
            return Err("Pipe");
        }
        Ok(p)
    }

    fn close(fd: RawFd) {
        unsafe { libc::close(fd) };
    }

    fn write_x(fd: RawFd) {
        unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) };
    }

    #[test]
    fn a_readable_pipe_then_its_hang_up() {
        let loop_ = Loop::init().unwrap();
        let p = pipe().unwrap();
        loop_.add_read(p[0], 7).unwrap();

        write_x(p[1]);
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 7,
                kind: Kind::Read,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );

        close(p[1]);
        assert_eq!(
            &[Event {
                token: 7,
                kind: Kind::Read,
                eof: true
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        close(p[0]);
    }

    #[test]
    fn only_ready_fds_are_reported_removed_ones_arent() {
        let loop_ = Loop::init().unwrap();
        let a = pipe().unwrap();
        let b = pipe().unwrap();
        loop_.add_read(a[0], 1).unwrap();
        loop_.add_read(b[0], 2).unwrap();
        loop_.remove(a[0]).unwrap();

        write_x(a[1]);
        write_x(b[1]);
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 2,
                kind: Kind::Read,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        for fd in a.into_iter().chain(b) {
            close(fd);
        }
    }

    #[test]
    fn signals_arrive_as_events_and_the_old_action_comes_back() {
        let mut loop_ = Loop::init().unwrap();
        loop_.add_signal(libc::SIGUSR1, 9).unwrap();
        // the whole process: raise only signals this thread, which kqueue misses
        unsafe { libc::kill(libc::getpid(), libc::SIGUSR1) };
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 9,
                kind: Kind::Signal,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );

        loop_.deinit();
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut now) };
        assert_ne!(libc::SIG_IGN, now.sa_sigaction);
    }

    #[test]
    fn a_timer_fires_once() {
        let loop_ = Loop::init().unwrap();
        loop_.add_timer(1, 5, 11).unwrap();
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 11,
                kind: Kind::Timer,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
    }
}
