//! An event loop: register fds, signals and timers once, then wait for
//! whichever is ready. Each watch carries a token that comes back with its
//! events.
//!
//! The platform-neutral part lives here. A backend (`kqueue` for macOS and
//! FreeBSD, `epoll` for Linux) supplies `Backend`, which this file drives; to
//! add a platform, write a module with the same `Backend` methods and select
//! it below.

use std::fmt;
use std::os::unix::io::RawFd;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod kqueue;
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
use kqueue::Backend;

#[cfg(target_os = "linux")]
mod epoll;
#[cfg(target_os = "linux")]
use epoll::Backend;

#[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "linux")))]
compile_error!("no event-loop backend for this platform yet");

/// What went wrong with the event loop's backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Error {
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

pub(crate) type Result<T> = std::result::Result<T, Error>;

/// The number of signals whose old action is kept.
const NSIG: usize = 32;

pub(crate) struct Loop {
    /// Declared before `old`, but dropped after `Loop::drop` restores the
    /// signal actions.
    backend: Backend,
    /// Signal actions before add_signal, restored on deinit.
    old: [Option<libc::sigaction>; NSIG],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Event {
    pub token: usize,
    pub kind: Kind,
    /// The other end hung up. There may still be bytes to read first.
    pub eof: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Kind {
    #[default]
    Read,
    /// There's room to write.
    Write,
    Signal,
    Timer,
    /// A child process watched with `add_exit` ended.
    Exit,
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
    pub(crate) fn init() -> Result<Loop> {
        Ok(Loop {
            backend: Backend::init()?,
            old: [None; NSIG],
        })
    }

    /// Watches `fd` for reading. Adding it again replaces the token.
    pub(crate) fn add_read(&self, fd: RawFd, token: usize) -> Result<()> {
        self.backend.add_read(fd, token)
    }

    /// Watches `fd`, already watched for reading, for room to write too,
    /// reported under `token`. Adding it again replaces the token.
    pub(crate) fn add_write(&self, fd: RawFd, token: usize) -> Result<()> {
        self.backend.add_write(fd, token)
    }

    /// Stops watching `fd` for room to write; its read watch stays. Fine if
    /// it wasn't watched for writing.
    pub(crate) fn remove_write(&self, fd: RawFd) -> Result<()> {
        self.backend.remove_write(fd)
    }

    /// Stops watching `fd`, for reading and writing. Call it before closing `fd`: kqueue drops a watch
    /// when its fd closes, but epoll only when every copy of the open file
    /// closes, and a watch left behind reports under the old token, or the
    /// token of whatever gets the fd number next.
    pub(crate) fn remove(&self, fd: RawFd) -> Result<()> {
        self.backend.remove(fd)
    }

    /// Watches `sig`, and ignores it so its default action never runs.
    ///
    /// Some backends (epoll) block the signal on the calling thread, so any
    /// thread that already exists keeps it unblocked and may swallow it. Add
    /// signals before starting other threads, and drop the loop on the thread
    /// that added them.
    pub(crate) fn add_signal(&mut self, sig: libc::c_int, token: usize) -> Result<()> {
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
    pub(crate) fn add_timer(&self, id: usize, ms: u32, token: usize) -> Result<()> {
        self.backend.add_timer(id, ms, token)
    }

    /// Watches child `pid` for its end, once, reported as `Kind::Exit`.
    /// It isn't reaped: the caller waits for it. Fails if it has been
    /// reaped already.
    ///
    /// Not SIGCHLD: `add_signal` ignores the signal it watches, and an
    /// ignored SIGCHLD reaps children by itself, losing their exit statuses.
    pub(crate) fn add_exit(&self, pid: libc::pid_t, token: usize) -> Result<()> {
        self.backend.add_exit(pid, token)
    }

    /// The loop's own fd: readable while something is ready, so this loop
    /// can be watched from another.
    pub(crate) fn fd(&self) -> RawFd {
        self.backend.fd()
    }

    /// Blocks until something is ready, then fills the start of `buf`.
    pub(crate) fn wait<'a>(&self, buf: &'a mut [Event]) -> Result<&'a [Event]> {
        self.backend.wait(buf, true)
    }

    /// Fills the start of `buf` with what is ready now, without blocking.
    pub(crate) fn poll<'a>(&self, buf: &'a mut [Event]) -> Result<&'a [Event]> {
        self.backend.wait(buf, false)
    }
}

impl Drop for Loop {
    /// Releases the signals and restores their old actions; the backend
    /// closes itself after.
    fn drop(&mut self) {
        // while they're still ignored: a signal the backend held back is
        // discarded, not delivered to the old action
        self.backend.release_signals();
        // restore next, so no signal is ignored once nothing watches it
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

    /// Raises `sig` where the backend sees it. kqueue watches the whole
    /// process and misses a signal raised for one thread; epoll's signalfd
    /// has the signal blocked on this thread only, and a signal for the
    /// process is dropped while it's ignored and the main thread, which
    /// isn't this one, doesn't block it.
    fn raise_for_loop(sig: libc::c_int) {
        // SAFETY: raising a signal the caller has ignored and watches.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::raise(sig)
        };
        // SAFETY: as above, for the whole process.
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::kill(libc::getpid(), sig)
        };
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

    // What a pane's terminal gets while output waits for it: a write watch
    // beside its read watch, under its own token, taken off once drained.
    #[test]
    fn a_write_watch_reports_room_beside_the_read_watch_until_removed() {
        let loop_ = Loop::init().unwrap();
        let (master, slave) = crate::daemon::surface::open_pty(80, 24);
        loop_.add_read(slave, 1).unwrap();
        loop_.add_write(slave, 2).unwrap();
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 2,
                kind: Kind::Write,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );

        // both at once: input waiting (a line, as the pty is in cooked
        // mode), and room to write
        // SAFETY: two bytes from a static buffer.
        unsafe { libc::write(master, b"x\n".as_ptr().cast(), 2) };
        let mut got = loop_.wait(&mut buf).unwrap().to_vec();
        got.sort_by_key(|e| e.token);
        assert_eq!(
            vec![
                Event {
                    token: 1,
                    kind: Kind::Read,
                    eof: false
                },
                Event {
                    token: 2,
                    kind: Kind::Write,
                    eof: false
                }
            ],
            got
        );

        // off again, the read watch stays
        loop_.remove_write(slave).unwrap();
        assert_eq!(
            &[Event {
                token: 1,
                kind: Kind::Read,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );

        // and `remove` takes both off
        loop_.add_write(slave, 2).unwrap();
        loop_.remove(slave).unwrap();
        loop_.add_timer(1, 50, 3).unwrap();
        assert_eq!(
            &[Event {
                token: 3,
                kind: Kind::Timer,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        close(master);
        close(slave);
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
        raise_for_loop(libc::SIGUSR1);
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 9,
                kind: Kind::Signal,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );

        drop(loop_);
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(libc::SIGUSR1, std::ptr::null(), &mut now) };
        assert_ne!(libc::SIG_IGN, now.sa_sigaction);
    }

    #[test]
    fn adding_an_fd_again_replaces_its_token() {
        let loop_ = Loop::init().unwrap();
        let p = pipe().unwrap();
        loop_.add_read(p[0], 1).unwrap();
        loop_.add_read(p[0], 2).unwrap();

        write_x(p[1]);
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 2,
                kind: Kind::Read,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        for fd in p {
            close(fd);
        }
    }

    #[test]
    fn a_timer_fires_once_and_not_before_its_time() {
        let loop_ = Loop::init().unwrap();
        loop_.add_timer(1, 100, 11).unwrap();
        let started = std::time::Instant::now();
        let mut buf = [Event::default(); 4];
        let got = loop_.wait(&mut buf).unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(90));
        assert_eq!(
            &[Event {
                token: 11,
                kind: Kind::Timer,
                eof: false
            }][..],
            got
        );

        // it's spent: the next thing to fire is a later timer, not this again
        loop_.add_timer(2, 20, 12).unwrap();
        assert_eq!(
            &[Event {
                token: 12,
                kind: Kind::Timer,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
    }

    #[test]
    fn adding_a_timer_again_restarts_it() {
        let loop_ = Loop::init().unwrap();
        loop_.add_timer(1, 300, 11).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        loop_.add_timer(1, 300, 12).unwrap();
        let restarted = std::time::Instant::now();
        let mut buf = [Event::default(); 4];
        // fires once, with the new token, a full period after the restart
        assert_eq!(
            &[Event {
                token: 12,
                kind: Kind::Timer,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        assert!(restarted.elapsed() >= std::time::Duration::from_millis(280));
    }

    #[test]
    fn a_childs_exit_arrives_once_and_leaves_it_to_be_reaped() {
        let loop_ = Loop::init().unwrap();
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sleep 0.1; exit 3"])
            .spawn()
            .unwrap();
        loop_.add_exit(child.id() as libc::pid_t, 5).unwrap();
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 5,
                kind: Kind::Exit,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        assert_eq!(Some(3), child.wait().unwrap().code());
        assert!(loop_.poll(&mut buf).unwrap().is_empty());
    }

    #[test]
    fn a_child_that_ended_before_it_was_watched_is_reported_too() {
        let loop_ = Loop::init().unwrap();
        let mut child = std::process::Command::new("true").spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        // a zombie: ended, not reaped. Some kernels refuse to watch one; its
        // watcher then finds it ended by trying to reap it.
        match loop_.add_exit(child.id() as libc::pid_t, 5) {
            Ok(()) => {
                let mut buf = [Event::default(); 4];
                assert_eq!(Kind::Exit, loop_.wait(&mut buf).unwrap()[0].kind);
                child.wait().unwrap();
            }
            Err(_) => assert!(child.try_wait().unwrap().is_some(), "refused, yet running"),
        }
    }

    #[test]
    fn poll_returns_at_once_with_what_is_ready() {
        let loop_ = Loop::init().unwrap();
        let p = pipe().unwrap();
        loop_.add_read(p[0], 1).unwrap();
        let mut buf = [Event::default(); 4];
        assert!(loop_.poll(&mut buf).unwrap().is_empty());
        write_x(p[1]);
        assert_eq!(1, loop_.poll(&mut buf).unwrap()[0].token);
        for fd in p {
            close(fd);
        }
    }

    #[test]
    fn a_loop_is_readable_on_another_while_it_has_something_ready() {
        let outer = Loop::init().unwrap();
        let inner = Loop::init().unwrap();
        outer.add_read(inner.fd(), 1).unwrap();
        inner.add_timer(1, 20, 2).unwrap();
        let mut buf = [Event::default(); 4];
        assert_eq!(1, outer.wait(&mut buf).unwrap()[0].token);
        assert_eq!(2, inner.poll(&mut buf).unwrap()[0].token);
        // spent: the outer loop has nothing more
        outer.add_timer(1, 50, 3).unwrap();
        assert_eq!(3, outer.wait(&mut buf).unwrap()[0].token);
    }

    #[test]
    fn a_timer_of_zero_milliseconds_fires() {
        let loop_ = Loop::init().unwrap();
        loop_.add_timer(1, 0, 11).unwrap();
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

    #[test]
    fn a_signal_nobody_waited_for_is_discarded_when_the_loop_ends() {
        let mut loop_ = Loop::init().unwrap();
        loop_.add_signal(libc::SIGUSR2, 9).unwrap();
        raise_for_loop(libc::SIGUSR2);
        // SIGUSR2's old action ends the process: it must not arrive now
        drop(loop_);

        // SAFETY: a null new action only reads the current one.
        let mut now: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(libc::SIGUSR2, std::ptr::null(), &mut now) };
        assert_ne!(libc::SIG_IGN, now.sa_sigaction);
        // and the signal isn't left blocked on this thread
        // SAFETY: a null set only reads this thread's mask.
        let mut blocked: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, std::ptr::null(), &mut blocked) };
        // SAFETY: `blocked` was filled in above.
        assert_eq!(0, unsafe { libc::sigismember(&blocked, libc::SIGUSR2) });
    }

    // What a pane's terminal fd is: the helper and the shell hold copies, so
    // the file outlives the daemon's close. A watch left on it would report
    // under the fd number's next owner.
    #[test]
    fn a_removed_fd_stays_quiet_when_its_number_goes_to_another_file() {
        let loop_ = Loop::init().unwrap();
        let old = pipe().unwrap();
        // SAFETY: duplicating an open fd, which keeps the file alive.
        let other_copy = unsafe { libc::dup(old[0]) };
        loop_.add_read(old[0], 1).unwrap();
        loop_.remove(old[0]).unwrap();

        // the new pipe's read end takes over the old number, closing it
        let new = pipe().unwrap();
        // SAFETY: both fds are open and ours.
        assert_eq!(old[0], unsafe { libc::dup2(new[0], old[0]) });
        close(new[0]);
        loop_.add_read(old[0], 2).unwrap();

        // into the old file, readable through `other_copy`; the timer ends the wait
        write_x(old[1]);
        loop_.add_timer(1, 100, 3).unwrap();
        let mut buf = [Event::default(); 4];
        assert_eq!(
            &[Event {
                token: 3,
                kind: Kind::Timer,
                eof: false
            }][..],
            loop_.wait(&mut buf).unwrap()
        );
        for fd in [old[0], old[1], new[1], other_copy] {
            close(fd);
        }
    }
}
