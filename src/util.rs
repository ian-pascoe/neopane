//! Small helpers more than one layer uses.

use std::io;
use std::time::Duration;

/// Seconds as Lua would show them, to two places at most: `1`, `0.75`.
pub(crate) fn seconds(d: Duration) -> String {
    let s = format!("{:.2}", d.as_secs_f64());
    s.trim_end_matches('0').trim_end_matches('.').to_string()
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
