//! Terminals' devices, for any pane provider's `find` to prove that an
//! attaching terminal is the one it names: the device of a process's
//! controlling terminal, and the device of a tty, by path or handed over,
//! as `st_rdev` has it. Equal is the same terminal. And what the process
//! table says of a process, for one that proves a terminal by who runs
//! it: its session's leader, its parent and its name.

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
/// `kp_eproc.e_ppid`, 264 into it, and `kp_proc.p_comm`, 17 bytes 243
/// into `kp_proc`, NUL-terminated.
#[cfg(target_os = "macos")]
const E_PPID: usize = 296 + 264;
#[cfg(target_os = "macos")]
const P_COMM: usize = 243;

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
    e_tdev_of(&kinfo_of(pid).ok()?)
}

/// Process `pid`'s whole `kinfo_proc`, from `sysctl KERN_PROC_PID`, which
/// works for another user's process.
#[cfg(target_os = "macos")]
fn kinfo_of(pid: u32) -> Result<Vec<u8>, Unread> {
    let pid = i32::try_from(pid).map_err(|_| Unread::Gone)?;
    if pid == 0 {
        // the kernel's own, no process to prove anything by
        return Err(Unread::Gone);
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
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
    if r != 0 || len == 0 {
        // SAFETY: signal 0 only asks whether the process is there.
        let there = unsafe { libc::kill(pid, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
        return Err(if there {
            Unread::Unreadable
        } else {
            Unread::Gone
        });
    }
    if len != KINFO_PROC_SIZE {
        return Err(Unread::Unreadable);
    }
    // SAFETY: `buf` holds `len` initialized bytes, at most its size.
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast(), len) };
    Ok(bytes.to_vec())
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

/// What the process table says of a process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Process {
    /// Its session's leader, whose pid is the session's id.
    pub leader: u32,
    pub parent: u32,
    /// Its name as the table has it, cut at 16 bytes on macOS, 15 on
    /// Linux.
    pub name: String,
}

/// Why the process table told nothing of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Unread {
    /// There's no such process.
    Gone,
    /// It is there, or may be, but the table can't be read for it.
    Unreadable,
}

/// What the process table says of process `pid`.
#[cfg(target_os = "macos")]
pub(crate) fn process(pid: u32) -> Result<Process, Unread> {
    let kinfo = kinfo_of(pid)?;
    let leader = leader_of(pid)?;
    let at = |offset: usize| u32::from_ne_bytes(kinfo[offset..offset + 4].try_into().unwrap());
    let comm = &kinfo[P_COMM..P_COMM + 17];
    let end = comm.iter().position(|&b| b == 0).unwrap_or(comm.len());
    Ok(Process {
        leader,
        parent: at(E_PPID),
        name: String::from_utf8_lossy(&comm[..end]).into_owned(),
    })
}

/// What the process table says of process `pid`: from `/proc/<pid>/stat`.
#[cfg(target_os = "linux")]
pub(crate) fn process(pid: u32) -> Result<Process, Unread> {
    if pid == 0 {
        return Err(Unread::Gone);
    }
    match std::fs::read(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat_of(&stat).ok_or(Unread::Unreadable),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Unread::Gone),
        Err(_) => Err(Unread::Unreadable),
    }
}

/// Where neither way is built: unreadable.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn process(_pid: u32) -> Result<Process, Unread> {
    Err(Unread::Unreadable)
}

/// Process `pid`'s session's leader: `getsid`.
#[cfg(target_os = "macos")]
fn leader_of(pid: u32) -> Result<u32, Unread> {
    let pid = libc::pid_t::try_from(pid).map_err(|_| Unread::Gone)?;
    if pid == 0 {
        return Err(Unread::Gone);
    }
    // SAFETY: a plain syscall.
    let sid = unsafe { libc::getsid(pid) };
    if sid >= 0 {
        return Ok(sid as u32);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Err(Unread::Gone),
        _ => Err(Unread::Unreadable),
    }
}

/// The name, parent and session of a `/proc/<pid>/stat`, `stat`: its
/// second field, the name in parentheses, may hold anything; then state,
/// ppid, pgrp, session.
#[cfg(any(target_os = "linux", test))]
fn stat_of(stat: &[u8]) -> Option<Process> {
    let open = stat.iter().position(|&b| b == b'(')?;
    let close = stat.iter().rposition(|&b| b == b')')?;
    let name = String::from_utf8_lossy(stat.get(open + 1..close)?).into_owned();
    let rest = std::str::from_utf8(&stat[close + 1..]).ok()?;
    let mut fields = rest.split_ascii_whitespace().skip(1);
    let parent = fields.next()?.parse().ok()?;
    let leader = fields.nth(1)?.parse().ok()?;
    Some(Process {
        leader,
        parent,
        name,
    })
}

/// The path of terminal device `dev`, as `/dev/ttys009`: on macOS, the
/// name the system gives it.
#[cfg(target_os = "macos")]
pub(crate) fn path_of_device(dev: u64) -> Option<String> {
    // SAFETY: devname returns a static buffer or null.
    let name = unsafe { libc::devname(dev as libc::dev_t, libc::S_IFCHR) };
    if name.is_null() {
        return None;
    }
    // SAFETY: non-null, NUL-terminated.
    let name = unsafe { std::ffi::CStr::from_ptr(name) }.to_string_lossy();
    (!name.is_empty() && !name.contains('?')).then(|| format!("/dev/{name}"))
}

/// The path of terminal device `dev`, as `/dev/pts/3`: on Linux, only a
/// pseudo-terminal's (majors 136 to 143) is named.
#[cfg(target_os = "linux")]
pub(crate) fn path_of_device(dev: u64) -> Option<String> {
    let dev = dev as libc::dev_t;
    let (major, minor) = (libc::major(dev), libc::minor(dev));
    (136..=143)
        .contains(&major)
        .then(|| format!("/dev/pts/{}", (major - 136) * 256 + minor))
}

/// Elsewhere none is named.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn path_of_device(_dev: u64) -> Option<String> {
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
    fn a_process_tells_its_session_leader_parent_and_name() {
        let me = process(std::process::id()).unwrap();
        // SAFETY: plain syscalls on this process.
        let (parent, leader) = unsafe { (libc::getppid(), libc::getsid(0)) };
        assert_eq!(me.parent, parent as u32);
        assert_eq!(me.leader, leader as u32);
        // the table cuts the name short
        let exe = std::env::current_exe().unwrap();
        let file = exe.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!me.name.is_empty() && file.starts_with(&me.name), "{me:?}");
        // a sleep in a session of its own leads it
        let child = sleeper(None);
        let theirs = process(child.id());
        end(child);
        let theirs = theirs.unwrap();
        assert_eq!(theirs.name, "sleep");
        assert_eq!(theirs.parent, std::process::id());
        assert!(theirs.leader > 0 && theirs.leader != me.leader);
    }

    #[test]
    fn a_process_that_isnt_there_has_gone() {
        assert_eq!(Err(Unread::Gone), process(u32::MAX - 1));
        assert_eq!(Err(Unread::Gone), process(0));
    }

    #[test]
    fn a_stat_line_tells_the_parent_session_and_name_whatever_the_name_holds() {
        let line = b"123 (a) (b c) S 77 123 99 34823 123 4194560";
        assert_eq!(
            Some(("a) (b c".to_string(), 77, 99)),
            stat_of(line).map(|s| (s.name, s.parent, s.leader))
        );
        assert_eq!(None, stat_of(b"123 (sh) S").map(|s| s.parent));
        assert_eq!(None, stat_of(b"garbage").map(|s| s.parent));
    }

    #[test]
    fn a_ttys_path_is_named_from_its_device() {
        let (_master, slave, path) = pty();
        let dev = of_fd(slave.as_fd()).unwrap();
        let named = path_of_device(dev).unwrap();
        // macOS's ttyname may go by /private/var; the device's name, by /dev
        assert_eq!(of_path(named.as_bytes()), Some(dev), "{named} for {path:?}");
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
