//! One pane's terminal.
//!
//! The terminal's buffer is a `Vec`, so nothing points into a Surface. It is
//! still handed around as a `Box<Surface>` (what `create` returns and a `Pane`
//! owns), which keeps a `Pane` small to move around in the `Panes` list.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use crate::nvim::forwarder::Forwarder;
use crate::screen::pane_view::PaneView;
use crate::term::modes;
use crate::term::terminal::Terminal;

pub struct Surface {
    /// Closing it makes the helper exit, and Rex close the pane.
    pub helper_socket: UnixStream,
    pub terminal: Terminal,
    pub view: PaneView,
    pub forwarder: Forwarder,
}

impl Surface {
    /// Takes over `helper_socket` and the terminal `tty`. If it fails, both
    /// are closed.
    pub fn create(helper_socket: UnixStream, tty: RawFd) -> std::io::Result<Box<Surface>> {
        let mut terminal = match Terminal::init(tty) {
            Ok(terminal) => terminal,
            Err(e) => {
                // SAFETY: `tty` is the fd this call was given to own.
                unsafe { libc::close(tty) };
                return Err(e);
            }
        };

        terminal
            .writer
            .extend_from_slice(modes::EDITOR_ON.as_bytes());
        let ws = match terminal.size() {
            Ok(ws) => ws,
            Err(e) => {
                terminal.deinit();
                // SAFETY: as above.
                unsafe { libc::close(tty) };
                return Err(e);
            }
        };
        Ok(Box::new(Surface {
            helper_socket,
            terminal,
            view: PaneView::init(u32::from(ws.ws_col), u32::from(ws.ws_row)),
            forwarder: Forwarder {
                ctrl_q_quits: false,
                send_focus: false,
                ..Forwarder::default()
            },
        }))
    }

    /// `restore` is false when the terminal is already gone. A `message` is
    /// left on the restored terminal: how a daemon that gives up tells the pane's
    /// user why.
    pub fn destroy(mut self: Box<Surface>, restore: bool, message: Option<&[u8]>) {
        if restore {
            self.terminal
                .writer
                .extend_from_slice(modes::EDITOR_OFF.as_bytes());
            self.terminal.deinit();
            if let Some(m) = message {
                // SAFETY: `m` is valid for `m.len()` bytes.
                unsafe { libc::write(self.terminal.fd, m.as_ptr().cast(), m.len()) };
            }
        }
        // the terminal's fd and the helper socket are closed as `self` drops
    }

    /// The window that fits the pane, less the `msg_rows` at the bottom kept for
    /// Neovim's messages and command line ('cmdheight'). Never empty.
    pub fn window_size(&self, msg_rows: u32) -> [u32; 2] {
        window_fitting(self.view.cols, self.view.rows, msg_rows)
    }

    /// The helper socket's fd, for the event loop.
    pub fn helper_fd(&self) -> RawFd {
        self.helper_socket.as_raw_fd()
    }
}

impl Drop for Surface {
    /// Closes the terminal's fd, then (as the field drops) the helper socket.
    /// It doesn't restore the terminal: `destroy` does that first.
    fn drop(&mut self) {
        // SAFETY: the surface owns the terminal's fd.
        unsafe { libc::close(self.terminal.fd) };
    }
}

/// `Surface::window_size` for a pane of `cols` x `rows`, one that may not exist yet.
/// The only place the rule is.
pub fn window_fitting(cols: u32, rows: u32, msg_rows: u32) -> [u32; 2] {
    [cols.max(1), rows.saturating_sub(msg_rows).max(1)]
}

/// A pty of `cols` x `rows`: master and slave fds.
#[cfg(test)]
pub fn open_pty(cols: u16, rows: u16) -> (RawFd, RawFd) {
    let (mut master, mut slave) = (-1, -1);
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: valid out-pointers; no name or termios.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut ws,
        )
    };
    assert_eq!(0, rc);
    (master, slave)
}

#[cfg(test)]
impl Surface {
    /// A surface over a throwaway pty of `cols` x `rows`, for tests of what
    /// holds one. Nothing reads what it draws.
    pub fn for_test(cols: u32, rows: u32) -> Box<Surface> {
        let (master, slave) = open_pty(cols as u16, rows as u16);
        let (socket, _other) = UnixStream::pair().unwrap();
        let mut surface = Surface::create(socket, slave).unwrap();
        // SAFETY: closing the pty's master, which nothing else holds.
        unsafe { libc::close(master) };
        surface.terminal.writer.clear();
        surface.view = PaneView::init(cols, rows);
        surface
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_the_pane_less_the_message_rows_and_never_empty() {
        // window_size only reads `view`
        let mut surface = Surface::for_test(80, 24);
        // cmdheight 0 reserves none: the window fills the pane
        assert_eq!([80, 24], surface.window_size(0));
        assert_eq!([80, 23], surface.window_size(1));
        assert_eq!([80, 22], surface.window_size(2));
        assert_eq!([80, 1], surface.window_size(40));

        surface.view = PaneView::init(0, 0);
        assert_eq!([1, 1], surface.window_size(0));
        assert_eq!([1, 1], surface.window_size(1));

        surface.view = PaneView::init(10, 1);
        assert_eq!([10, 1], surface.window_size(1));
        assert_eq!([10, 1], surface.window_size(0));
    }

    // Runs create and destroy over a pty.
    #[test]
    fn create_takes_the_ptys_size_and_destroy_restores_it() {
        let (master, slave) = open_pty(90, 30);
        let (socket, helper) = UnixStream::pair().unwrap();
        let surface = Surface::create(socket, slave).unwrap();
        assert_eq!((90, 30), (surface.view.cols, surface.view.rows));
        assert!(!surface.forwarder.ctrl_q_quits);
        assert!(!surface.forwarder.send_focus);

        // `create` buffered the editor modes; destroy flushes them, then the
        // modes' undoing and the message. Restoring the terminal waits for its
        // output to be read, so a thread plays the terminal emulator.
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 1024];
            // The slave is closed by `destroy`, so the read ends then.
            loop {
                // SAFETY: `buf` is valid for its length.
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n as usize]);
            }
            got
        });
        surface.destroy(true, Some(b"bye"));
        let got = reader.join().unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains(modes::EDITOR_ON), "{got:?}");
        assert!(got.contains(modes::EDITOR_OFF), "{got:?}");
        assert!(got.ends_with("bye"), "{got:?}");

        // the helper's end sees the hang-up
        let mut byte = [0u8; 1];
        // SAFETY: `byte` is valid for 1 byte.
        let n = unsafe { libc::read(helper.as_raw_fd(), byte.as_mut_ptr().cast(), 1) };
        assert_eq!(0, n);
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
    }

    #[test]
    fn create_fails_and_closes_on_something_that_is_not_a_terminal() {
        let mut fds = [0; 2];
        // SAFETY: a throwaway pipe.
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        let (socket, _helper) = UnixStream::pair().unwrap();
        assert!(Surface::create(socket, fds[0]).is_err());
        // SAFETY: fds[0] was closed by `create`, so this must fail with EBADF.
        assert_eq!(-1, unsafe { libc::close(fds[0]) });
        // SAFETY: closing the pipe's other end.
        unsafe { libc::close(fds[1]) };
    }
}
