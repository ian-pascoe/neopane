//! Terminals' devices, for any pane provider's `find` to prove that an
//! attaching terminal is the one it names: the device of a process's
//! controlling terminal, and the device of a tty, by path or handed over,
//! as `st_rdev` has it. Equal is the same terminal.

use std::os::fd::{AsRawFd, BorrowedFd};

/// The device of the tty at `path`, if it is one.
pub(crate) fn of_path(path: &[u8]) -> Option<u64> {
    if path.is_empty() {
        return None;
    }
    let path = std::ffi::CString::new(path).ok()?;
    // SAFETY: a zeroed stat is valid for stat to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: stat on a NUL-terminated path.
    let r = unsafe { libc::stat(path.as_ptr(), &mut st) };
    (r == 0).then(|| device_of(&st))?
}

/// The device of the tty `fd` is open on, if it is one.
pub(crate) fn of_fd(fd: BorrowedFd<'_>) -> Option<u64> {
    // SAFETY: a zeroed stat is valid for fstat to fill.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fstat on a borrowed, open fd.
    let r = unsafe { libc::fstat(fd.as_raw_fd(), &mut st) };
    (r == 0).then(|| device_of(&st))?
}

/// The device `st` is, if a character device, as process tables have it.
fn device_of(st: &libc::stat) -> Option<u64> {
    if st.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return None;
    }
    // macOS's dev_t is signed; its process table's terminal isn't
    #[cfg(target_os = "macos")]
    let dev = u64::from(st.st_rdev as u32);
    #[cfg(not(target_os = "macos"))]
    #[allow(
        clippy::unnecessary_cast,
        reason = "dev_t is u64 on Linux, not on every Unix"
    )]
    let dev = st.st_rdev as u64;
    Some(dev)
}

/// `struct kinfo_proc` (<sys/sysctl.h>), which `libc` doesn't have: 648
/// bytes on 64-bit macOS, `kp_eproc` at 296 and its `e_tdev` 276 into it.
#[cfg(any(target_os = "macos", test))]
const KINFO_PROC_SIZE: usize = 648;
#[cfg(any(target_os = "macos", test))]
const E_TDEV: usize = 296 + 276;

/// The controlling terminal in a `kinfo_proc`, `kinfo`, if it is one whole
/// and has one: `NODEV` is none.
#[cfg(any(target_os = "macos", test))]
fn e_tdev_of(kinfo: &[u8]) -> Option<u64> {
    const NODEV: u32 = u32::MAX;
    if kinfo.len() != KINFO_PROC_SIZE {
        return None;
    }
    let tdev = u32::from_ne_bytes(kinfo[E_TDEV..E_TDEV + 4].try_into().ok()?);
    (tdev != NODEV).then_some(u64::from(tdev))
}

/// The device of process `pid`'s controlling terminal, if it has one:
/// `sysctl KERN_PROC_PID`'s `kp_eproc.e_tdev`, which works for another
/// user's process, as `login` is.
#[cfg(target_os = "macos")]
pub(crate) fn of_pid(pid: u32) -> Option<u64> {
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROC,
        libc::KERN_PROC_PID,
        i32::try_from(pid).ok()?,
    ];
    let mut buf = [0u64; KINFO_PROC_SIZE / 8];
    let mut len = KINFO_PROC_SIZE;
    // SAFETY: `buf` is valid for `len` bytes, and `mib` for its length.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // a pid with no process answers no bytes
    if r != 0 {
        return None;
    }
    // SAFETY: `buf` holds `len` initialized bytes, at most its size.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), len) };
    e_tdev_of(bytes)
}

/// The device of process `pid`'s controlling terminal, if it has one:
/// field 7 of `/proc/<pid>/stat`, `tty_nr`, as a `dev_t`.
#[cfg(target_os = "linux")]
pub(crate) fn of_pid(pid: u32) -> Option<u64> {
    let stat = std::fs::read(format!("/proc/{pid}/stat")).ok()?;
    tty_nr_of_stat(&stat).map(|nr| {
        // the kernel's encoding: minor's low byte, major, then minor's rest
        let major = (nr >> 8) & 0xfff;
        let minor = (nr & 0xff) | ((nr >> 12) & 0xfff00);
        libc::makedev(major, minor) as u64
    })
}

/// Field 7 of a `/proc/<pid>/stat`, `tty_nr`, if not 0: none. Its second
/// field, the command's name in parentheses, may hold anything.
#[cfg(any(target_os = "linux", test))]
fn tty_nr_of_stat(stat: &[u8]) -> Option<u32> {
    let after = stat.iter().rposition(|&b| b == b')')?;
    let rest = std::str::from_utf8(&stat[after + 1..]).ok()?;
    // state, ppid, pgrp, session, tty_nr
    let nr: i64 = rest.split_ascii_whitespace().nth(4)?.parse().ok()?;
    u32::try_from(nr).ok().filter(|&n| n != 0)
}

/// Where neither way is built: unknown.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn of_pid(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    use super::*;

    /// A new pty: its master, its slave, and the slave's path.
    fn pty() -> (OwnedFd, OwnedFd, Vec<u8>) {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: both fds are written; the rest may be null.
        let r = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(0, r);
        // SAFETY: both are ours, owned from here.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let mut name = [0u8; 256];
        // SAFETY: `name` is valid for its length.
        assert_eq!(0, unsafe {
            libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr().cast(), name.len())
        });
        let end = name.iter().position(|&b| b == 0).unwrap();
        (master, slave, name[..end].to_vec())
    }

    /// A `sleep` in a session of its own, with `tty` its controlling
    /// terminal if given, else none.
    fn sleeper(tty: Option<&OwnedFd>) -> Child {
        let tty = tty.map(|t| t.as_raw_fd());
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if let Some(fd) = tty
                    && libc::ioctl(fd, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    }

    fn end(mut child: Child) {
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn the_terminal_of_a_process_is_the_device_of_its_controlling_ttys_path() {
        let (_master, slave, path) = pty();
        let ours = of_path(&path);
        assert!(ours.is_some());
        let child = sleeper(Some(&slave));
        let theirs = of_pid(child.id());
        end(child);
        assert_eq!(ours, theirs);
    }

    #[test]
    fn a_process_with_no_terminal_or_none_at_all_has_no_tty() {
        let child = sleeper(None);
        let theirs = of_pid(child.id());
        end(child);
        assert_eq!(None, theirs);
        assert_eq!(None, of_pid(u32::MAX - 1));
    }

    #[test]
    fn a_ttys_device_is_the_same_handed_over_or_by_its_path() {
        let (_master, slave, path) = pty();
        assert!(of_fd(slave.as_fd()).is_some());
        assert_eq!(of_fd(slave.as_fd()), of_path(&path));
        // not a terminal
        assert_eq!(None, of_path(b"/"));
        assert_eq!(None, of_path(b""));
        assert_eq!(None, of_path(b"/no/such/tty"));
        let dir = std::fs::File::open("/").unwrap();
        assert_eq!(None, of_fd(dir.as_fd()));
    }

    #[test]
    fn a_stat_lines_tty_is_its_seventh_field_past_any_parenthesis_in_its_name() {
        let line = b"123 (a) (b c) S 1 123 123 34823 123 4194560";
        assert_eq!(Some(34823), tty_nr_of_stat(line));
        assert_eq!(None, tty_nr_of_stat(b"123 (sh) S 1 123 123 0 -1"));
        assert_eq!(None, tty_nr_of_stat(b"garbage"));
    }

    #[test]
    fn a_kinfo_procs_terminal_is_its_e_tdev_unless_nodev_or_cut_short() {
        let mut kinfo = vec![0u8; KINFO_PROC_SIZE];
        kinfo[E_TDEV..E_TDEV + 4].copy_from_slice(&0x1000_0006u32.to_ne_bytes());
        assert_eq!(Some(0x1000_0006), e_tdev_of(&kinfo));
        kinfo[E_TDEV..E_TDEV + 4].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert_eq!(None, e_tdev_of(&kinfo));
        assert_eq!(None, e_tdev_of(&kinfo[..KINFO_PROC_SIZE - 1]));
        assert_eq!(None, e_tdev_of(&[]));
    }
}
