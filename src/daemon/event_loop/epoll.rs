//! The epoll backend, for Linux. Plain fds go on the epoll as they are, an
//! fd watched for writing too under one entry with both flags.
//! Signals arrive through one signalfd and each timer is a timerfd, so the
//! epoll sees all three as readable fds, and `wait` turns them back into
//! events.
//!
//! A signalfd only gets a signal that is blocked, so `add_signal` blocks it
//! on the calling thread. Blocked signals queue even while ignored.
//! `release_signals` unblocks them again, and a loop must drop on the thread
//! that added its signals.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::io::RawFd;

use super::{Error, Event, Kind, Result};

/// One more than the highest signal number.
const SIGNALS: usize = 65;

/// The epoll flags that say the other end is gone.
const EOF: u32 = (libc::EPOLLRDHUP | libc::EPOLLHUP | libc::EPOLLERR) as u32;

/// What an fd on the epoll stands for.
#[derive(Clone, Copy)]
struct Watch {
    kind: Kind,
    /// Unused for the signalfd: each signal has its own token.
    token: usize,
    /// For an fd watched for reading, the token of its write watch, if any.
    write_token: Option<usize>,
}

/// The flags an fd is watched for reading with.
const READ: u32 = (libc::EPOLLIN | libc::EPOLLRDHUP) as u32;

struct Signals {
    /// The signalfd, made with the first signal.
    fd: Option<RawFd>,
    /// Every signal the signalfd takes.
    mask: libc::sigset_t,
    /// The ones `add_signal` blocked, so `release_signals` doesn't unblock a
    /// signal that was already blocked when the loop began.
    blocked: libc::sigset_t,
    tokens: [Option<usize>; SIGNALS],
}

pub struct Backend {
    ep: RawFd,
    watches: RefCell<HashMap<RawFd, Watch>>,
    /// The timerfd of each timer id, kept after it fires for the next time
    /// the id is added.
    timers: RefCell<HashMap<usize, RawFd>>,
    signals: RefCell<Signals>,
}

fn empty_set() -> libc::sigset_t {
    // SAFETY: sigemptyset fills the set it's given.
    unsafe {
        let mut set = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        set
    }
}

impl Backend {
    pub fn init() -> Result<Backend> {
        // SAFETY: a plain syscall. Not inherited by children such as Neovim.
        let ep = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if ep < 0 {
            return Err(Error::CreateFailed);
        }
        Ok(Backend {
            ep,
            watches: RefCell::new(HashMap::new()),
            timers: RefCell::new(HashMap::new()),
            signals: RefCell::new(Signals {
                fd: None,
                mask: empty_set(),
                blocked: empty_set(),
                tokens: [None; SIGNALS],
            }),
        })
    }

    pub fn add_read(&self, fd: RawFd, token: usize) -> Result<()> {
        self.watch(
            fd,
            Watch {
                kind: Kind::Read,
                token,
                write_token: None,
            },
        )
    }

    /// `fd` must be watched for reading already.
    pub fn add_write(&self, fd: RawFd, token: usize) -> Result<()> {
        self.set_write(fd, Some(token))
    }

    pub fn remove_write(&self, fd: RawFd) -> Result<()> {
        if !self.watches.borrow().contains_key(&fd) {
            return Ok(());
        }
        self.set_write(fd, None)
    }

    /// Changes the write watch of `fd`, watched for reading.
    fn set_write(&self, fd: RawFd, write_token: Option<usize>) -> Result<()> {
        let mut watches = self.watches.borrow_mut();
        let Some(watch) = watches.get_mut(&fd).filter(|w| w.kind == Kind::Read) else {
            return Err(Error::WatchFailed);
        };
        let mut event = libc::epoll_event {
            events: READ
                | if write_token.is_some() {
                    libc::EPOLLOUT as u32
                } else {
                    0
                },
            u64: fd as u64,
        };
        // SAFETY: `event` is valid.
        if unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_MOD, fd, &mut event) } < 0 {
            return Err(Error::WatchFailed);
        }
        watch.write_token = write_token;
        Ok(())
    }

    pub fn remove(&self, fd: RawFd) -> Result<()> {
        // SAFETY: the event is ignored for a delete, but old kernels want one.
        let rc = unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        if rc < 0 {
            return Err(Error::WatchFailed);
        }
        self.watches.borrow_mut().remove(&fd);
        Ok(())
    }

    /// Watches `sig`. The caller has already made it ignored. Blocks it on
    /// this thread, so it queues for the signalfd instead.
    pub fn add_signal(&self, sig: libc::c_int, token: usize) -> Result<()> {
        let n = usize::try_from(sig).map_err(|_| Error::WatchFailed)?;
        if n == 0 || n >= SIGNALS {
            return Err(Error::WatchFailed);
        }
        let mut signals = self.signals.borrow_mut();

        let mut one = empty_set();
        let mut before = empty_set();
        // SAFETY: the sets are valid; blocking changes only this thread's mask.
        unsafe {
            libc::sigaddset(&mut one, sig);
            if libc::pthread_sigmask(libc::SIG_BLOCK, &one, &mut before) != 0 {
                return Err(Error::WatchFailed);
            }
            if libc::sigismember(&before, sig) == 0 {
                libc::sigaddset(&mut signals.blocked, sig);
            }
            libc::sigaddset(&mut signals.mask, sig);
        }

        match signals.fd {
            Some(fd) => {
                // SAFETY: updates the mask of `fd`, the signalfd made earlier.
                if unsafe { libc::signalfd(fd, &signals.mask, 0) } < 0 {
                    return Err(Error::WatchFailed);
                }
            }
            None => {
                // SAFETY: -1 makes a new signalfd.
                let fd = unsafe {
                    libc::signalfd(-1, &signals.mask, libc::SFD_CLOEXEC | libc::SFD_NONBLOCK)
                };
                let watch = Watch {
                    kind: Kind::Signal,
                    token: 0,
                    write_token: None,
                };
                signals.fd = Some(self.adopt(fd, watch)?);
            }
        }
        signals.tokens[n] = Some(token);
        Ok(())
    }

    /// Called as the loop ends, before the old signal actions come back.
    /// Unblocks the signals `add_signal` blocked. They are still ignored, so
    /// one that is pending is discarded; with its old action back, it would
    /// run that action, which may end the process.
    pub fn release_signals(&self) {
        let mut signals = self.signals.borrow_mut();
        // SAFETY: the set is valid, and unblocking only changes this thread.
        unsafe {
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &signals.blocked, std::ptr::null_mut());
        }
        signals.blocked = empty_set();
    }

    pub fn add_timer(&self, id: usize, ms: u32, token: usize) -> Result<()> {
        let known = self.timers.borrow().get(&id).copied();
        let fd = match known {
            Some(fd) => {
                self.watches.borrow_mut().insert(
                    fd,
                    Watch {
                        kind: Kind::Timer,
                        token,
                        write_token: None,
                    },
                );
                fd
            }
            None => {
                // SAFETY: a plain syscall.
                let fd = unsafe {
                    libc::timerfd_create(
                        libc::CLOCK_MONOTONIC,
                        libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
                    )
                };
                let watch = Watch {
                    kind: Kind::Timer,
                    token,
                    write_token: None,
                };
                let fd = self.adopt(fd, watch)?;
                self.timers.borrow_mut().insert(id, fd);
                fd
            }
        };

        // a zero value would disarm the timer, so ask for a nanosecond
        // SAFETY: an all-zero itimerspec is a valid value.
        let mut at: libc::itimerspec = unsafe { std::mem::zeroed() };
        at.it_value.tv_sec = (ms / 1000) as _;
        at.it_value.tv_nsec = ((ms % 1000) as libc::c_long) * 1_000_000;
        if ms == 0 {
            at.it_value.tv_nsec = 1;
        }
        // SAFETY: `at` is valid. Setting it clears what the timer counted
        // before, so a timer that fired unread doesn't fire after its restart.
        if unsafe { libc::timerfd_settime(fd, 0, &at, std::ptr::null_mut()) } < 0 {
            return Err(Error::WatchFailed);
        }
        Ok(())
    }

    pub fn wait<'a>(&self, buf: &'a mut [Event]) -> Result<&'a [Event]> {
        if buf.is_empty() {
            return Ok(&buf[..0]);
        }
        // SAFETY: an all-zero epoll_event is a valid value.
        let mut raw: [libc::epoll_event; 64] = unsafe { std::mem::zeroed() };
        // an fd watched for reading and writing can give two events
        let max = (buf.len() / 2).clamp(1, raw.len()) as libc::c_int;
        loop {
            let ready: usize = loop {
                // SAFETY: `raw` has room for `max` events.
                let rc = unsafe { libc::epoll_wait(self.ep, raw.as_mut_ptr(), max, -1) };
                if rc >= 0 {
                    break rc as usize;
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    return Err(Error::WaitFailed);
                }
            };

            let mut n = 0;
            for e in &raw[..ready] {
                // by value: the struct is packed on some platforms
                let (fd, flags) = (e.u64 as RawFd, e.events);
                let Some(watch) = self.watches.borrow().get(&fd).copied() else {
                    continue; // removed since
                };
                let event = match watch.kind {
                    Kind::Read => {
                        // room to write; a hang-up or error shows as the write failing
                        if let Some(token) = watch.write_token
                            && flags & (libc::EPOLLOUT | libc::EPOLLHUP | libc::EPOLLERR) as u32
                                != 0
                            && n < buf.len()
                        {
                            buf[n] = Event {
                                token,
                                kind: Kind::Write,
                                eof: flags & EOF != 0,
                            };
                            n += 1;
                        }
                        (flags & (libc::EPOLLIN as u32 | EOF) != 0).then_some(Event {
                            token: watch.token,
                            kind: Kind::Read,
                            eof: flags & EOF != 0,
                        })
                    }
                    Kind::Write => unreachable!(), // a write watch rides on a read watch
                    Kind::Signal => self.take_signal(fd),
                    Kind::Timer => take_timer(fd).then_some(Event {
                        token: watch.token,
                        kind: Kind::Timer,
                        eof: false,
                    }),
                };
                if let Some(event) = event
                    && n < buf.len()
                {
                    buf[n] = event;
                    n += 1;
                }
            }
            // every event may have been spent already: wait again
            if n > 0 {
                return Ok(&buf[..n]);
            }
        }
    }

    /// Takes one signal off the signalfd. Others stay queued, and the epoll
    /// reports the fd again for the next `wait`.
    fn take_signal(&self, fd: RawFd) -> Option<Event> {
        // SAFETY: an all-zero siginfo is a valid value.
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::signalfd_siginfo>();
        // SAFETY: `info` has room for one siginfo.
        let n = unsafe { libc::read(fd, (&mut info as *mut libc::signalfd_siginfo).cast(), size) };
        if n != size as isize {
            return None; // another thread took it
        }
        let token = self
            .signals
            .borrow()
            .tokens
            .get(info.ssi_signo as usize)
            .copied()??;
        Some(Event {
            token,
            kind: Kind::Signal,
            eof: false,
        })
    }

    /// Puts the new signalfd or timerfd `fd` on the epoll, or closes it if it
    /// can't be: `fd` is negative if making it failed.
    fn adopt(&self, fd: RawFd, watch: Watch) -> Result<RawFd> {
        if fd < 0 {
            return Err(Error::CreateFailed);
        }
        if let Err(e) = self.watch(fd, watch) {
            // SAFETY: `fd` was just made and nothing else has it.
            unsafe { libc::close(fd) };
            return Err(e);
        }
        Ok(fd)
    }

    /// Adds `fd` to the epoll for reading, or updates it if it's there.
    fn watch(&self, fd: RawFd, watch: Watch) -> Result<()> {
        if self.watches.borrow().contains_key(&fd) {
            // Already added, so the same file again or, if the fd was closed
            // without `remove`, a stale watch under this number. Only the
            // first is something this can undo: a watch on a closed fd that
            // lives on in another copy is out of reach.
            // SAFETY: the event is ignored for a delete; it may fail.
            unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        }
        let mut event = libc::epoll_event {
            events: READ,
            u64: fd as u64,
        };
        // SAFETY: `event` is valid.
        if unsafe { libc::epoll_ctl(self.ep, libc::EPOLL_CTL_ADD, fd, &mut event) } < 0 {
            return Err(Error::WatchFailed);
        }
        self.watches.borrow_mut().insert(fd, watch);
        Ok(())
    }
}

/// Reads the timerfd's count, so it stops being readable. False if it had
/// none: the timer was restarted since.
fn take_timer(fd: RawFd) -> bool {
    let mut ticks: u64 = 0;
    // SAFETY: `ticks` has room for the 8 bytes a timerfd gives.
    let n = unsafe { libc::read(fd, (&mut ticks as *mut u64).cast(), 8) };
    n == 8
}

impl Drop for Backend {
    fn drop(&mut self) {
        // SAFETY: the fds are open and owned by this backend, and closed only
        // here.
        unsafe {
            libc::close(self.ep);
            if let Some(fd) = self.signals.get_mut().fd {
                libc::close(fd);
            }
            for fd in self.timers.get_mut().values() {
                libc::close(*fd);
            }
        }
    }
}
