//! One pane's terminal.
//!
//! The terminal's buffer is a `Vec`, so nothing points into a Surface. It is
//! still handed around as a `Box<Surface>` (what `create` returns and a `Pane`
//! owns), which keeps a `Pane` small to move around in the `Panes` list.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use std::time::{Duration, Instant};

use super::attach_conn;
use super::backlog::{self, Backlog};
use crate::attach::{End, Size};
use crate::nvim::forwarder::Forwarder;
use crate::screen::pane_view::PaneView;
use crate::term::modes;
use crate::term::terminal::Terminal;

/// How long a pane going back to its shell waits for its terminal to take
/// the modes' undoing.
const RESTORE_WAIT: Duration = Duration::from_secs(1);

/// How `Surface::destroy` leaves the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Restore {
    /// Not at all: the terminal is already gone.
    No,
    /// Without waiting: a pane closing while the daemon runs, which Rex closes
    /// next. Its terminal may not be read, and what it can't take at once is
    /// dropped.
    Now,
    /// Waiting up to `RESTORE_WAIT` for the terminal to take it all: the
    /// daemon is ending, and the launcher's pane goes back to its shell.
    Waiting,
}

/// What a frame drawn into the terminal's writer did, sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// The terminal took it all.
    All,
    /// Some waits for room in the terminal.
    Waiting,
}

pub(crate) struct Surface {
    /// Closing it makes the helper exit, and Rex close the pane.
    pub helper_socket: UnixStream,
    /// What the helper sent that isn't a whole frame yet.
    pub helper_in: Vec<u8>,
    pub terminal: Terminal,
    pub view: PaneView,
    pub forwarder: Forwarder,
    /// Output the terminal hasn't taken yet.
    pub backlog: Backlog,
}

impl Surface {
    /// Takes over `helper_socket` and the terminal `tty`, of `size` as its
    /// hello says: the daemon never asks a terminal its size. If it fails,
    /// both are closed.
    pub(crate) fn create(
        helper_socket: UnixStream,
        tty: RawFd,
        size: Size,
    ) -> std::io::Result<Box<Surface>> {
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
        Ok(Box::new(Surface {
            helper_socket,
            helper_in: Vec::new(),
            terminal,
            view: PaneView::init(u32::from(size.cols), u32::from(size.rows)),
            forwarder: Forwarder {
                ctrl_q_quits: false,
                send_focus: false,
                ..Forwarder::default()
            },
            backlog: Backlog::default(),
        }))
    }

    /// Restores the terminal as `restore` says, then ends the attach with
    /// `end`, which says why: a helper that is gone already gets none.
    pub(crate) fn destroy(mut self: Box<Surface>, restore: Restore, end: End) {
        let wait = match restore {
            Restore::No => return, // the fds close as `self` drops
            Restore::Now => Duration::ZERO,
            Restore::Waiting => RESTORE_WAIT,
        };
        let deadline = Instant::now() + wait;
        let left = || deadline.saturating_duration_since(Instant::now());
        // the frame being written is finished first, so the modes' undoing
        // isn't read as part of an escape sequence
        let mut out = self.backlog.take_unfinished();
        out.append(&mut self.terminal.writer);
        out.extend_from_slice(modes::EDITOR_OFF.as_bytes());
        let fd = self.terminal.fd;
        backlog::write_within(fd, &out, left());
        self.terminal.restore_now();
        attach_conn::send_end(&self.helper_socket, end);
        // the terminal's fd and the helper socket are closed as `self` drops
    }

    /// The window that fits the pane, less the `msg_rows` at the bottom kept for
    /// Neovim's messages and command line ('cmdheight'). Never empty.
    pub(crate) fn window_size(&self, msg_rows: u32) -> [u32; 2] {
        window_fitting(self.view.cols, self.view.rows, msg_rows)
    }

    /// Sends the frame drawn into the terminal's writer, without waiting for
    /// the terminal to take it. An error means the terminal is gone.
    pub(crate) fn send_frame(&mut self) -> std::io::Result<Sent> {
        self.backlog
            .send(self.terminal.fd, &mut self.terminal.writer)?;
        Ok(if self.backlog.is_empty() {
            Sent::All
        } else {
            Sent::Waiting
        })
    }

    /// The helper socket's fd, for the event loop.
    pub(crate) fn helper_fd(&self) -> RawFd {
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
pub(crate) fn window_fitting(cols: u32, rows: u32, msg_rows: u32) -> [u32; 2] {
    [cols.max(1), rows.saturating_sub(msg_rows).max(1)]
}

/// `cols` x `rows`, as a hello gives a size.
#[cfg(test)]
pub(crate) fn cells(cols: u16, rows: u16) -> Size {
    Size {
        cols,
        rows,
        px: None,
    }
}

/// A pty of `cols` x `rows`: master and slave fds.
#[cfg(test)]
pub(crate) fn open_pty(cols: u16, rows: u16) -> (RawFd, RawFd) {
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
    pub(crate) fn for_test(cols: u32, rows: u32) -> Box<Surface> {
        let (master, slave) = open_pty(cols as u16, rows as u16);
        let (socket, _other) = UnixStream::pair().unwrap();
        let mut surface = Surface::create(socket, slave, cells(cols as u16, rows as u16)).unwrap();
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
    use crate::attach::Ending;

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
    fn create_takes_the_hellos_size_and_destroy_restores_and_ends_the_attach() {
        let (master, slave) = open_pty(90, 30);
        let (socket, helper) = UnixStream::pair().unwrap();
        // the hello's size, not the pty's: the daemon never asks a terminal
        let surface = Surface::create(socket, slave, cells(70, 20)).unwrap();
        assert_eq!((70, 20), (surface.view.cols, surface.view.rows));
        assert!(!surface.forwarder.ctrl_q_quits);
        assert!(!surface.forwarder.send_focus);

        // `create` buffered the editor modes; destroy flushes them, then the
        // modes' undoing. Restoring the terminal waits for its
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
        surface.destroy(Restore::Waiting, End::ending(Ending::Failed, "bye", false));
        let got = reader.join().unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains(modes::EDITOR_ON), "{got:?}");
        assert!(got.ends_with(modes::EDITOR_OFF), "{got:?}");

        // the helper is told why, then hung up on
        let mut said = Vec::new();
        std::io::Read::read_to_end(&mut &helper, &mut said).unwrap();
        let (frame, n) = crate::attach::Frame::decode(&said).unwrap().unwrap();
        assert_eq!(said.len(), n);
        let Some(crate::attach::Message::End(z)) = crate::attach::Message::decode(&frame).unwrap()
        else {
            panic!("{frame:?}")
        };
        assert_eq!((Ending::Failed, "bye"), (z.as_ending(), z.message.as_str()));
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
    }

    #[test]
    fn create_fails_and_closes_on_something_that_is_not_a_terminal() {
        let mut fds = [0; 2];
        // SAFETY: a throwaway pipe.
        assert_eq!(0, unsafe { libc::pipe(fds.as_mut_ptr()) });
        let (socket, _helper) = UnixStream::pair().unwrap();
        assert!(Surface::create(socket, fds[0], cells(80, 24)).is_err());
        // SAFETY: fds[0] was closed by `create`, so this must fail with EBADF.
        assert_eq!(-1, unsafe { libc::close(fds[0]) });
        // SAFETY: closing the pipe's other end.
        unsafe { libc::close(fds[1]) };
    }
}
