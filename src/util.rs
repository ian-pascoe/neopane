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
/// Fills `buf` with random bytes, from the kernel: `arc4random_buf`, or
/// `getrandom` on Linux, which musl's `libc` bindings have as well.
#[cfg(any(target_os = "macos", target_os = "freebsd"))]
pub(crate) fn random(buf: &mut [u8]) {
    // SAFETY: `buf` is valid for writes of its length.
    unsafe { libc::arc4random_buf(buf.as_mut_ptr().cast(), buf.len()) }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
pub(crate) fn random(buf: &mut [u8]) {
    let mut filled = 0;
    while filled < buf.len() {
        // SAFETY: `buf[filled..]` is valid for writes, and getrandom writes at most
        // `buf.len() - filled` bytes starting inside `buf`.
        let n =
            unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if n < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            // only a bad pointer or flags fail, which is a bug here
            panic!("getrandom failed");
        }
        filled += n as usize;
    }
}

#[cfg(test)]
pub(crate) fn thread_number() -> String {
    format!("{:?}", std::thread::current().id())
        .chars()
        .filter(char::is_ascii_digit)
        .collect()
}

/// The built binary, for a lib test that runs it, such as the config
/// child: the first ancestor directory of this test's executable with a
/// file named `<name>` in it, or the name it is kept under there, whatever
/// Cargo's build-dir layout; kept there under a name of its own
/// (`keep_beside`), so another Cargo in the same target directory never
/// takes it away mid-test (SPI-1282). The one kept from an earlier run
/// counts as built. Where it can't be kept, `<name>` itself, as Cargo
/// places it. None, but under `CI`, when it hasn't been built, as with
/// `cargo test --lib` alone.
#[cfg(test)]
pub(crate) fn built_binary() -> Option<std::path::PathBuf> {
    static BUILT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let found = exe
                .ancestors()
                .skip(1)
                .map(|d| d.join(crate::name::NAME))
                .find(|p| p.is_file() || kept_name(p).is_file());
            assert!(
                found.is_some() || std::env::var_os("CI").is_none(),
                "{} isn't built beside {}",
                crate::name::NAME,
                exe.display()
            );
            found.map(|f| keep_beside(&f).unwrap_or(f))
        })
        .clone()
}

/// The name `file` is kept under beside it (`keep_beside`).
#[cfg(test)]
fn kept_name(file: &std::path::Path) -> std::path::PathBuf {
    let name = crate::name::NAME;
    file.with_file_name(format!(".{name}-kept"))
}

/// Keeps `file` beside it under a name that never names nothing: a hard
/// link, made under a name of this process's and renamed into place, and
/// that name. Cargo places a binary by removing it and linking or cloning
/// it anew, so for a moment its own name names nothing, even when nothing
/// was rebuilt; the kept name is only ever replaced by a rename, at once,
/// and any test process may replace it. In that moment, the file kept
/// before stands. The kept link holds the binary it links till the next
/// keeps another.
#[cfg(test)]
fn keep_beside(file: &std::path::Path) -> io::Result<std::path::PathBuf> {
    let kept = kept_name(file);
    let mut made = kept.clone().into_os_string();
    made.push(format!("-{}", std::process::id()));
    let made = std::path::PathBuf::from(made);
    let _ = std::fs::remove_file(&made);
    if let Err(e) = std::fs::hard_link(file, &made) {
        return if kept.is_file() { Ok(kept) } else { Err(e) };
    }
    let renamed = std::fs::rename(&made, &kept);
    // a rename onto a link of the same file leaves both
    let _ = std::fs::remove_file(&made);
    renamed.map(|()| kept)
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

    #[test]
    fn a_kept_binary_outlives_its_file_and_is_only_ever_replaced_whole() {
        let name = crate::name::NAME;
        let dir = std::env::temp_dir().join(format!("{name}-kept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, "one").unwrap();
        let kept = keep_beside(&file).unwrap();
        assert_ne!(file, kept);
        assert_eq!(Some(dir.as_path()), kept.parent());
        // Cargo places the binary again: it removes it, then links it anew
        std::fs::remove_file(&file).unwrap();
        assert_eq!("one", std::fs::read_to_string(&kept).unwrap());
        // kept in that moment: the one kept before stands
        assert_eq!(kept, keep_beside(&file).unwrap());
        assert_eq!("one", std::fs::read_to_string(&kept).unwrap());
        std::fs::write(&file, "two").unwrap();
        assert_eq!(kept, keep_beside(&file).unwrap());
        assert_eq!("two", std::fs::read_to_string(&kept).unwrap());
        // kept again, as another test process would: nothing else is left
        assert_eq!(kept, keep_beside(&file).unwrap());
        assert_eq!(2, std::fs::read_dir(&dir).unwrap().count());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
