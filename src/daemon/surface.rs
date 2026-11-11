//! One pane's terminal.
//!
//! The terminal's buffer is a `Vec`, so nothing points into a Surface. It is
//! still handed around as a `Box<Surface>` (what `create` returns and a `Pane`
//! owns), which keeps a `Pane` small to move around in the `Panes` list.

use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use super::attach_conn;
use super::backlog::Backlog;
use crate::attach::{End, Ending, Size};
use crate::nvim::forwarder::Forwarder;
use crate::screen::pane_view::PaneView;
use crate::term::modes;
use crate::term::terminal::Terminal;

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
    /// hello says: the daemon never asks a terminal its size, nor sets its
    /// settings, which are the helper's (ADR 0013).
    pub(crate) fn create(helper_socket: UnixStream, tty: RawFd, size: Size) -> Box<Surface> {
        let mut terminal = Terminal::new(tty);
        terminal
            .writer
            .extend_from_slice(modes::EDITOR_ON.as_bytes());
        Box::new(Surface {
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
        })
    }

    /// Ends the attach as `ending`, with `message` for the helper, and
    /// closes the terminal and the helper socket. Nothing more is written to
    /// the terminal: what waits is dropped, and the `z` says whether that cut
    /// a frame short. The helper then undoes what the daemon wrote and
    /// restores the terminal.
    pub(crate) fn destroy(self: Box<Surface>, ending: Ending, message: &str) {
        let end = End::ending(ending, message, self.backlog.cut());
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
    /// The terminal's settings are the helper's to restore.
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

#[cfg(test)]
pub(crate) use crate::term::terminal::open_pty;

#[cfg(test)]
impl Surface {
    /// A surface over a throwaway pty of `cols` x `rows`, for tests of what
    /// holds one. Nothing reads what it draws.
    pub(crate) fn for_test(cols: u32, rows: u32) -> Box<Surface> {
        let (master, slave) = open_pty(cols as u16, rows as u16);
        let (socket, _other) = UnixStream::pair().unwrap();
        let mut surface = Surface::create(socket, slave, cells(cols as u16, rows as u16));
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
    fn create_takes_the_hellos_size_and_destroy_ends_the_attach_writing_nothing_more() {
        let (master, slave) = open_pty(90, 30);
        let before = crate::term::raw::settings(slave);
        let (socket, helper) = UnixStream::pair().unwrap();
        // the hello's size, not the pty's: the daemon never asks a terminal
        let surface = Surface::create(socket, slave, cells(70, 20));
        assert_eq!((70, 20), (surface.view.cols, surface.view.rows));
        assert!(!surface.forwarder.ctrl_q_quits);
        assert!(!surface.forwarder.send_focus);
        // nor sets its settings
        assert!(crate::term::raw::same(
            &before,
            &crate::term::raw::settings(slave)
        ));

        // `create` buffered the editor modes; destroy drops them unsent
        assert!(!surface.terminal.writer.is_empty());
        surface.destroy(Ending::Failed, "bye");
        // SAFETY: plain fcntl and read calls on the pty's master.
        unsafe {
            let fl = libc::fcntl(master, libc::F_GETFL);
            libc::fcntl(master, libc::F_SETFL, fl | libc::O_NONBLOCK);
            let mut buf = [0u8; 64];
            let n = libc::read(master, buf.as_mut_ptr().cast(), buf.len());
            assert!(n <= 0, "{:?}", &buf[..n as usize]);
        }

        // the helper is told why, then hung up on
        let said = attach_conn::read_all_frames(&helper);
        let [crate::attach::Message::End(z)] = &said[..] else {
            panic!("{said:?}")
        };
        assert_eq!((Ending::Failed, "bye"), (z.as_ending(), z.message.as_str()));
        assert!(!z.cut);
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
    }

    #[test]
    fn a_frame_the_terminal_took_part_of_is_cut() {
        let (master, slave) = open_pty(80, 24);
        let (socket, helper) = UnixStream::pair().unwrap();
        let mut surface = Surface::create(socket, slave, cells(80, 24));
        // more than the pty takes: nobody reads its master
        surface.terminal.writer = vec![b'x'; 1 << 20];
        assert_eq!(Sent::Waiting, surface.send_frame().unwrap());
        surface.destroy(Ending::Closed, "");
        let said = attach_conn::read_all_frames(&helper);
        let [crate::attach::Message::End(z)] = &said[..] else {
            panic!("{said:?}")
        };
        assert!(z.cut);
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
    }
}
