//! Passing file descriptors over a Unix socket (SCM_RIGHTS).
//! TLPI section 61.13.3.

use std::io;
use std::os::unix::io::{AsRawFd, RawFd};

/// Most fds one read takes; extras are closed.
const MAX_FDS: usize = 4;

const FD_SIZE: u32 = std::mem::size_of::<RawFd>() as u32;

/// Room for a control message carrying `n` fds, aligned for a `cmsghdr`.
#[repr(C, align(8))]
struct Control<const N: usize>([u8; N]);

/// Bytes of control buffer for `MAX_FDS` fds.
const RECV_CONTROL: usize = unsafe { libc::CMSG_SPACE(MAX_FDS as u32 * FD_SIZE) } as usize;
/// Bytes of control buffer for one fd.
const SEND_CONTROL: usize = unsafe { libc::CMSG_SPACE(FD_SIZE) } as usize;

/// Sends `bytes` with a copy of `fd` attached, on the Unix socket `sock`.
/// The fd rides with the first byte sent, so anything the caller buffers for
/// this socket must be written out before calling this. Writes all of
/// `bytes`.
pub fn send(sock: &impl AsRawFd, bytes: &[u8], fd: RawFd) -> io::Result<()> {
    assert!(!bytes.is_empty());
    let sock = sock.as_raw_fd();
    let mut control = Control::<SEND_CONTROL>([0; SEND_CONTROL]);

    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    // SAFETY: a zeroed msghdr is valid; the buffers outlive the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = SEND_CONTROL as _;
    // SAFETY: the control buffer is big enough and aligned for one cmsg
    // carrying one fd.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&msg);
        (*header).cmsg_len = libc::CMSG_LEN(FD_SIZE) as _;
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        std::ptr::copy_nonoverlapping(
            (&fd as *const RawFd).cast::<u8>(),
            libc::CMSG_DATA(header),
            FD_SIZE as usize,
        );
    }

    let mut sent = loop {
        // SAFETY: `msg` points at live buffers.
        let n = unsafe { libc::sendmsg(sock, &msg, 0) };
        if n >= 0 {
            break n as usize;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    };

    // the fd went with the first bytes; the rest goes plain
    while sent < bytes.len() {
        let rest = &bytes[sent..];
        // SAFETY: `rest` is a live slice.
        let n = unsafe { libc::write(sock, rest.as_ptr().cast(), rest.len()) };
        if n >= 0 {
            sent += n as usize;
            continue;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Received {
    /// Bytes read into the buffer; 0 when the peer hung up.
    pub len: usize,
    /// A passed fd, close-on-exec. The caller owns it.
    pub fd: Option<RawFd>,
}

/// One read into `buf`, keeping the first passed fd.
pub fn recv(sock: &impl AsRawFd, buf: &mut [u8]) -> io::Result<Received> {
    let sock = sock.as_raw_fd();
    let mut control = Control::<RECV_CONTROL>([0; RECV_CONTROL]);
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // SAFETY: a zeroed msghdr is valid; the buffers outlive the call.
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.0.as_mut_ptr().cast();
    msg.msg_controllen = RECV_CONTROL as _;

    let len = loop {
        // SAFETY: `msg` points at live buffers.
        let n = unsafe { libc::recvmsg(sock, &mut msg, 0) };
        if n >= 0 {
            break n as usize;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::ConnectionReset => return Ok(Received { len: 0, fd: None }),
            _ => return Err(err),
        }
    };

    let mut fd: Option<RawFd> = None;
    // SAFETY: the kernel filled `msg_control` with well-formed cmsgs, which
    // the CMSG_* macros walk.
    unsafe {
        let mut m = libc::CMSG_FIRSTHDR(&msg);
        while !m.is_null() {
            if (*m).cmsg_level == libc::SOL_SOCKET && (*m).cmsg_type == libc::SCM_RIGHTS {
                let data = libc::CMSG_DATA(m);
                let mut left = ((*m).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                let mut at = data;
                while left >= FD_SIZE as usize {
                    let got = std::ptr::read_unaligned(at.cast::<RawFd>());
                    at = at.add(FD_SIZE as usize);
                    left -= FD_SIZE as usize;
                    if fd.is_some() {
                        libc::close(got);
                        continue;
                    }
                    // macOS has no MSG_CMSG_CLOEXEC
                    libc::fcntl(got, libc::F_SETFD, libc::FD_CLOEXEC);
                    fd = Some(got);
                }
            }
            m = libc::CMSG_NXTHDR(&msg, m);
        }
    }
    Ok(Received { len, fd })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_passed_pipe_writes_into_the_original() {
        let (a, b) = UnixStream::pair().unwrap();
        let mut pipe: [RawFd; 2] = [0; 2];
        assert_eq!(0, unsafe { libc::pipe(pipe.as_mut_ptr()) });

        send(&a, b"p", pipe[1]).unwrap();
        unsafe { libc::close(pipe[1]) }; // the receiver's copy keeps it open

        let mut buf = [0u8; 8];
        let r = recv(&b, &mut buf).unwrap();
        assert_eq!(b"p", &buf[..r.len]);
        let fd = r.fd.expect("NoFd");
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } & libc::FD_CLOEXEC != 0);
        unsafe {
            libc::write(fd, b"hi".as_ptr().cast(), 2);
            libc::close(fd);
        }

        let mut out = [0u8; 8];
        let n = unsafe { libc::read(pipe[0], out.as_mut_ptr().cast(), out.len()) };
        assert_eq!(b"hi", &out[..n as usize]);
        unsafe { libc::close(pipe[0]) };
    }

    #[test]
    fn plain_bytes_then_hang_up() {
        let (mut a, b) = UnixStream::pair().unwrap();
        a.write_all(b"w").unwrap();
        a.flush().unwrap();
        drop(a);

        let mut buf = [0u8; 8];
        let r = recv(&b, &mut buf).unwrap();
        assert_eq!(1, r.len);
        assert_eq!(None, r.fd);
        assert_eq!(0, recv(&b, &mut buf).unwrap().len);
    }
}
