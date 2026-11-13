//! Small helpers more than one layer uses.

use std::io;
use std::time::Duration;

/// Seconds as Lua would show them, to two places at most: `1`, `0.75`.
pub(crate) fn seconds(d: Duration) -> String {
    let s = format!("{:.2}", d.as_secs_f64());
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Waits until `fd` has something to read, at most until `due`, and at
/// most 1 s: false once `due` has passed. For a blocking wait on one fd.
pub(crate) fn wait_readable(fd: std::os::fd::RawFd, due: std::time::Instant) -> bool {
    let left = due.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return false;
    }
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    let ms = left.as_millis().clamp(1, 1000) as libc::c_int;
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, ms) };
    true
}

/// Puts every signal the daemon ignores back to its default, and unblocks
/// every signal, for a child about to `exec`: an ignored signal stays
/// ignored across `exec`, so a command could neither be ended by SIGTERM
/// nor trap it. Handled signals are reset by `exec` itself.
///
/// # Safety
///
/// For `pre_exec`: it calls only `sigaction`, `sigemptyset` and
/// `pthread_sigmask`, which are async-signal-safe, and allocates nothing.
pub(crate) unsafe fn default_signals() -> io::Result<()> {
    #[cfg(target_os = "linux")]
    const SIGNALS: libc::c_int = 65;
    #[cfg(not(target_os = "linux"))]
    const SIGNALS: libc::c_int = 32;
    // SAFETY: plain syscalls on stack values.
    unsafe {
        for sig in 1..SIGNALS {
            if sig == libc::SIGKILL || sig == libc::SIGSTOP {
                continue;
            }
            let mut was: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut was) == 0
                && was.sa_sigaction == libc::SIG_IGN
            {
                let mut dfl: libc::sigaction = std::mem::zeroed();
                dfl.sa_sigaction = libc::SIG_DFL;
                libc::sigemptyset(&mut dfl.sa_mask);
                libc::sigaction(sig, &dfl, std::ptr::null_mut());
            }
        }
        let mut none: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut none);
        if libc::pthread_sigmask(libc::SIG_SETMASK, &none, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// This thread's number, for names of a test's own: its id's digits.
#[cfg(test)]
pub(crate) fn thread_number() -> String {
    format!("{:?}", std::thread::current().id())
        .chars()
        .filter(char::is_ascii_digit)
        .collect()
}

/// The built binary, for a lib test that runs it, such as the config
/// child: the first ancestor directory of this test's executable with a
/// file named `<name>` in it, whatever Cargo's build-dir layout. None, but
/// under `CI`, when it hasn't been built, as with `cargo test --lib` alone.
#[cfg(test)]
pub(crate) fn built_binary() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let found = exe
        .ancestors()
        .skip(1)
        .map(|d| d.join(crate::name::NAME))
        .find(|p| p.is_file());
    assert!(
        found.is_some() || std::env::var_os("CI").is_none(),
        "{} isn't built beside {}",
        crate::name::NAME,
        exe.display()
    );
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_are_shown_short() {
        assert_eq!(seconds(Duration::from_secs(1)), "1");
        assert_eq!(seconds(Duration::from_millis(750)), "0.75");
        assert_eq!(seconds(Duration::from_millis(2500)), "2.5");
        assert_eq!(seconds(Duration::from_secs(3)), "3");
    }
}
