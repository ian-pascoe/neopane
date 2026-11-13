//! One pane's terminal.
//!
//! The terminal's buffer is a `Vec`, so nothing points into a Surface. It is
//! still handed around as a `Box<Surface>` (what `create` returns and a `Pane`
//! owns), which keeps a `Pane` small to move around in the `Panes` list.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

use super::backlog::Backlog;
use super::terminal_io::{FdIo, RelayIo, TerminalIo};
use crate::attach::{End, Ending, Message, Size, Transport};
use crate::nvim::forwarder::Forwarder;
use crate::screen::pane_view::PaneView;
use crate::term::modes;

/// What a frame drawn into the terminal's writer did, sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// The terminal took it all.
    All,
    /// Some waits for room in the terminal.
    Waiting,
}

pub(crate) struct Surface {
    /// How the terminal's bytes travel. Dropped first: under `fd`, closing
    /// the terminal before the helper socket.
    pub io: Box<dyn TerminalIo>,
    /// Closing it makes the helper exit, and Rex close the pane.
    pub helper_socket: UnixStream,
    /// What the helper sent that isn't a whole frame yet.
    pub helper_in: Vec<u8>,
    /// The frame being drawn. Write a frame, then `send_frame`.
    pub writer: Vec<u8>,
    pub view: PaneView,
    pub forwarder: Forwarder,
    /// Output the terminal hasn't taken yet.
    pub backlog: Backlog,
}

impl Surface {
    /// Takes over `helper_socket` and the terminal `tty` it handed over, of
    /// `size` as its hello says: the daemon never asks a terminal its size,
    /// nor sets its settings, which are the helper's (ADR 0013).
    pub(crate) fn create(helper_socket: UnixStream, tty: RawFd, size: Size) -> Box<Surface> {
        // SAFETY: the caller hands the fd over.
        let tty = unsafe { OwnedFd::from_raw_fd(tty) };
        Surface::with_io(helper_socket, Box::new(FdIo { tty }), size)
    }

    /// Takes over an accepted attach's `helper_socket` and its terminal of
    /// `size`, as `transport` carries it: `tty`, the terminal handed over,
    /// under `fd`, which `attach_conn::check_terminal` makes sure came.
    pub(crate) fn for_attach(
        helper_socket: UnixStream,
        transport: Transport,
        tty: Option<OwnedFd>,
        size: Size,
    ) -> Box<Surface> {
        match (transport, tty) {
            (Transport::Fd, Some(tty)) => Surface::create(helper_socket, tty.into_raw_fd(), size),
            (Transport::Fd, None) => panic!("an fd attach without its terminal was taken"),
            // a file sent along all the same is closed: the relay doesn't use it
            (Transport::Relay, _) => Surface::relayed(helper_socket, size),
        }
    }

    /// Takes over `helper_socket`, which relays a terminal of `size`.
    pub(crate) fn relayed(helper_socket: UnixStream, size: Size) -> Box<Surface> {
        Surface::with_io(helper_socket, Box::new(RelayIo), size)
    }

    fn with_io(helper_socket: UnixStream, io: Box<dyn TerminalIo>, size: Size) -> Box<Surface> {
        Box::new(Surface {
            io,
            helper_socket,
            helper_in: Vec::new(),
            writer: modes::EDITOR_ON.as_bytes().to_vec(),
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
    /// closes the terminal and the helper socket. What waits is dropped
    /// (see `TerminalIo::end`), and nothing more is written to the
    /// terminal. The helper then undoes what the daemon wrote and restores
    /// the terminal.
    pub(crate) fn destroy(self: Box<Surface>, ending: Ending, message: &str) {
        self.end_with(End::ending(ending, message, false));
    }

    /// `destroy`, telling the helper `end`.
    pub(crate) fn end_with(self: Box<Surface>, end: End) {
        self.io.end(&self.helper_socket, &self.backlog, end);
        // the terminal's fd and the helper socket are closed as `self` drops
    }

    /// The window that fits the pane, less the `msg_rows` at the bottom kept for
    /// Neovim's messages and command line ('cmdheight'). Never empty.
    pub(crate) fn window_size(&self, msg_rows: u32) -> [u32; 2] {
        window_fitting(self.view.cols, self.view.rows, msg_rows)
    }

    /// Sends the frame drawn into the writer, without waiting for the
    /// terminal to take it. An error means the terminal is gone.
    pub(crate) fn send_frame(&mut self) -> std::io::Result<Sent> {
        self.io.wrap(&mut self.writer);
        let fd = self.output_fd();
        self.backlog.send(fd, &mut self.writer)?;
        Ok(if self.backlog.is_empty() {
            Sent::All
        } else {
            Sent::Waiting
        })
    }

    /// Writes pane title `title` into the terminal, its own title saved
    /// first (ADR 0006), and tells the helper the undo bytes that put it
    /// back, in a new `u` sent before it (#91 decision 6). Never waits,
    /// as `send_frame`.
    pub(crate) fn write_title(&mut self, title: &str) -> std::io::Result<Sent> {
        let u = Message::Undo(modes::undo_with_title())
            .encode()
            .expect("the undo bytes fit a `u`");
        let mut out = Vec::new();
        self.io.send_undo(&self.helper_socket, &u, &mut out);
        self.writer.extend_from_slice(&modes::set_title(title));
        self.io.wrap(&mut self.writer);
        out.append(&mut self.writer);
        let fd = self.output_fd();
        self.backlog.send(fd, &mut out)?;
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

    /// The fd output goes to, watched for room while some waits.
    pub(crate) fn output_fd(&self) -> RawFd {
        self.io.output_fd(self.helper_fd())
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
        // a terminal that takes every frame, so drawing never hangs it up
        let null = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/null")
            .unwrap();
        let (socket, _other) = UnixStream::pair().unwrap();
        let tty = std::os::fd::IntoRawFd::into_raw_fd(null);
        let mut surface = Surface::create(socket, tty, cells(cols as u16, rows as u16));
        surface.writer.clear();
        surface.view = PaneView::init(cols, rows);
        surface
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::Ending;
    use crate::daemon::attach_conn;

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
        assert!(!surface.writer.is_empty());
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
        surface.writer = vec![b'x'; 1 << 20];
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

    /// Every frame `helper` reads until the daemon hangs up, and the bytes
    /// after the last whole one.
    fn frames_and_rest(helper: &UnixStream) -> (Vec<crate::attach::Message>, Vec<u8>) {
        use crate::attach::{Frame, Message};
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut &*helper, &mut bytes).unwrap();
        let mut said = Vec::new();
        let mut at = 0;
        while let Some((f, n)) = Frame::decode(&bytes[at..]).unwrap() {
            at += n;
            said.extend(Message::decode(&f).unwrap());
        }
        (said, bytes[at..].to_vec())
    }

    // A relayed attach's end finishes the `o` in flight, so the helper
    // reads whole frames, and says the drawn frame was cut.
    #[test]
    fn a_relayed_end_finishes_the_o_in_flight_then_says_why() {
        use crate::attach::Message;
        let (socket, helper) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut surface = Surface::relayed(socket, cells(80, 24));
        // several `o` frames, more than the socket holds
        surface.writer = vec![b'x'; 1 << 20];
        assert_eq!(Sent::Waiting, surface.send_frame().unwrap());
        let reader = std::thread::spawn(move || frames_and_rest(&helper));
        let started = std::time::Instant::now();
        surface.destroy(Ending::Closed, "bye");
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        let (said, rest) = reader.join().unwrap();
        assert!(rest.is_empty(), "{} bytes of a frame", rest.len());
        let Some((Message::End(z), outputs)) = said.split_last() else {
            panic!("no end")
        };
        assert_eq!(
            (Ending::Closed, "bye", true),
            (z.as_ending(), z.message.as_str(), z.cut)
        );
        // the first `o` whole, perhaps more, never all 16
        let n = outputs.len();
        assert!((1..16).contains(&n), "{n} `o` frames");
        assert!(
            outputs
                .iter()
                .all(|m| matches!(m, Message::Output(o) if o.len() == 64 << 10))
        );
    }

    // A helper that never reads holds the end up 1 s at most; then the
    // attach ends without a `z`, which the helper takes for a crash.
    #[test]
    fn a_relayed_end_waits_1_s_at_most() {
        use crate::attach::Message;
        let (socket, helper) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut surface = Surface::relayed(socket, cells(80, 24));
        surface.writer = vec![b'x'; 1 << 20];
        assert_eq!(Sent::Waiting, surface.send_frame().unwrap());
        let started = std::time::Instant::now();
        surface.destroy(Ending::Closed, "bye");
        let took = started.elapsed();
        assert!(took >= std::time::Duration::from_millis(900), "{took:?}");
        // and not much more, if CI is loaded
        assert!(took < std::time::Duration::from_millis(2500), "{took:?}");
        let (said, rest) = frames_and_rest(&helper);
        assert!(!said.iter().any(|m| matches!(m, Message::End(_))));
        assert!(!rest.is_empty(), "the `o` in flight is cut off");
    }

    // Relayed, the new `u` goes between whole frames, before the title.
    #[test]
    fn a_relayed_pane_title_follows_its_u_on_the_helper_socket() {
        use crate::attach::Message;
        let (socket, helper) = UnixStream::pair().unwrap();
        socket.set_nonblocking(true).unwrap();
        let mut surface = Surface::relayed(socket, cells(80, 24));
        surface.writer.clear();
        assert_eq!(Sent::All, surface.write_title("np-abcdefghij").unwrap());
        surface.destroy(Ending::Closed, "");
        let (said, _) = frames_and_rest(&helper);
        let [Message::Undo(u), Message::Output(o), Message::End(_)] = &said[..] else {
            panic!("{said:?}");
        };
        assert_eq!(&modes::undo_with_title(), u);
        assert!(u.ends_with(b"\x1b]2;\x1b\\\x1b[23;2t"));
        assert_eq!(
            b"\x1b[22;2t\x1b]2;np-abcdefghij\x1b\\".as_slice(),
            o.as_slice()
        );
    }
}
