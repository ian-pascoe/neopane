//! Links a pane provider's terminal to a Neovim window. The three parts arrive in any
//! order.
//!
//! A `Pane` is a plain value whose Surface is boxed (only the Surface needs a
//! stable address), and the rest of the daemon refers to a pane by its `id`,
//! a `u32` looked up in `Panes`, which a borrow of `Panes` couldn't outlive
//! anyway.

use super::dir::Address;
use super::event_loop::{self, Loop};
use super::live_layout::Area;
use super::pane_layout::PaneLayout;
use super::provider::{Ask, Direction, TerminalId};
use super::surface::Surface;
use crate::attach::{self, End, Ending, PaneId, Transport};
use crate::name::NAME;
use crate::nvim::windows::Split;
use crate::screen::float_groups::Group;

/// Never 0: Neovim's API reads 0 as the current window.
pub(crate) type NeovimWindowHandle = i64;

/// An accepted self-attach: its terminal, and the pane provider's id for it
/// if known.
pub(crate) struct SelfAttach {
    pub surface: Box<Surface>,
    pub terminal: Option<TerminalId>,
}

impl SelfAttach {
    /// Ends the attach as failed, with `message` for its helper to print.
    pub(crate) fn fail(self, message: &str) {
        self.surface.destroy(Ending::Failed, message);
    }
}

pub(crate) struct Pane {
    /// The daemon's own number for it, for its loop tokens.
    pub id: u32,
    /// Its pane id: the one its helper's hello names, for a requested pane;
    /// minted as it attaches, for a self-attached one.
    pub attach_id: Option<PaneId>,
    /// Its terminal attached of its own accord. Placement never moves it.
    pub self_attached: bool,
    /// A self-attached pane's window, while it has none: what it asked for.
    pub taking: Option<Taking>,
    /// Its terminal's id, from the pane provider: Rex's is its block.
    pub terminal: Option<TerminalId>,
    /// None for a floating pane of floats, and a self-attached pane before
    /// it takes a window.
    /// Of a pane showing several windows, the one last current in it.
    pub neovim_window_handle: Option<NeovimWindowHandle>,
    /// The windows it shows, laid out as Neovim lays out splits, once it shows
    /// more than one: windows that had no terminal of their own (ADR 0009).
    pub layout: Option<PaneLayout>,
    /// The size each window of `layout` was last asked to be.
    pub layout_asked: Vec<(NeovimWindowHandle, LastAskedWinSize)>,
    /// Owned.
    pub terminal_surface: Option<Box<Surface>>,
    pub closing: Option<Close>,
    /// Its window's home tab page, if that tab page gets a tab page area of
    /// its own (ADR 0012), else 0. Its windows share that area.
    pub tab: i64,
    /// Set for a float group pane, showing a group of floats, not a window.
    pub layer: Option<Layer>,
    /// Its layout area, as the live layout last said (ADR 0008).
    pub area: Area,
    /// When an `Unreported` pane becomes its own area: a second after its
    /// terminal attached with its id.
    pub report_due: Option<std::time::Instant>,
    /// A new split's terminal said the size the pane provider gives one it
    /// hasn't laid out yet: its window keeps the guessed size until the first
    /// size message, a second, or the pane provider is dropped.
    pub awaiting_size: bool,
    /// Opened by `open`, for want of `open_beside`: where it is placed, with
    /// `place_beside`, once it has its terminal id and its terminal has
    /// attached (ADR 0006).
    pub placing: Option<Placing>,
    pub last_asked_win_size: LastAskedWinSize,
    /// Where Neovim split a requested pane's window off: where it is drawn
    /// if its terminal never comes (#112).
    pub split: Option<Split>,
    /// When a requested pane whose terminal is open stops waiting for it to
    /// attach (#88 decision 15).
    pub attach_due: Option<std::time::Instant>,
}

/// How a self-attached pane gets its window (#105).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Taking {
    /// A new window on this file, the cursor on this line: not asked yet.
    File(Vec<u8>, Option<u32>),
    /// Asked of Neovim, as request `req`: the unshown window `win`, or a new
    /// window if none.
    Asked {
        req: u32,
        win: Option<NeovimWindowHandle>,
    },
}

/// Where a pane opened by `open` goes: beside `origin`'s terminal, as
/// `place_beside` takes it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Placing {
    pub origin: TerminalId,
    pub dir: Direction,
    pub size: u32,
    pub ratio: f64,
}

impl Placing {
    /// The `place_beside` that puts `terminal` there.
    pub(crate) fn ask(self, terminal: TerminalId) -> Ask {
        Ask::PlaceBeside {
            terminal,
            origin: self.origin,
            dir: self.dir,
            size: self.size,
            ratio: self.ratio,
        }
    }
}

/// What `Pane::last_asked_win_size` holds: the grid, and the window's cols x rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LastAskedWinSize {
    pub grid: u32,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Fit {
    #[default]
    None,
    /// The request's id.
    Asked(u32),
    /// Answered: the next flush has the new layout.
    Answered,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Layer {
    pub group: Group,
    /// Its floats being refitted to its first size: not drawn until Neovim
    /// has answered and redrawn them, so the guessed size never shows.
    pub fit: Fit,
    /// Its terminal's rect in its layout area, as the live layout last said:
    /// x, y, w, h. None until it says.
    pub rect: Option<[f64; 4]>,
    /// The cells at each edge of `rect` that aren't its terminal's, such as
    /// a frame: left, top, right, bottom.
    pub inset: [u16; 4],
}

impl Layer {
    /// A layer for `group`, not yet placed.
    pub(crate) fn new(group: Group) -> Layer {
        Layer {
            group,
            fit: Fit::None,
            rect: None,
            inset: [0; 4],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Close {
    /// The terminal hung up, so its helper isn't told.
    TerminalHungUp,
    WindowClosed,
    RequestFailed,
    Shutdown,
}

/// Above Daemon's fixed tokens, so they never clash.
pub(crate) const TOKEN_BASE: usize = 16;

/// Which of a pane's fds an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum PaneFd {
    HelperSocket = 0,
    /// The terminal's input under `fd`, and the room its output waits for,
    /// on the terminal or, under `relay`, the helper socket.
    Terminal = 1,
}

/// What `decode_token` finds in a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodedToken {
    pub id: u32,
    pub source_fd: PaneFd,
}

/// The pane may be gone; look it up by id.
pub(crate) fn decode_token(t: usize) -> DecodedToken {
    DecodedToken {
        id: u32::try_from((t - TOKEN_BASE) / 2).expect("pane token out of range"),
        source_fd: if (t - TOKEN_BASE).is_multiple_of(2) {
            PaneFd::HelperSocket
        } else {
            PaneFd::Terminal
        },
    }
}

/// The command a requested pane's terminal runs: `{attach}`, with the pane
/// id minted for it, and `--transport relay` under the relay diagnostic.
pub(crate) fn attach_words(
    exe: &str,
    address: &Address,
    transport: Transport,
    pane: &Pane,
) -> Vec<String> {
    let id = pane
        .attach_id
        .as_ref()
        .expect("a requested pane has a pane id");
    attach::attach_command(exe.as_ref(), address.as_arg(), id, transport)
        .iter()
        .map(|w| w.to_string_lossy().into_owned())
        .collect()
}

/// The pane title of the pane with id `id` (ADR 0006): `<name>-` and its
/// token.
pub(crate) fn title(id: &PaneId) -> String {
    format!("{NAME}-{}", id.token())
}

/// The line an ending carries when nothing failed.
fn ending_message(ending: Ending) -> &'static str {
    match ending {
        Ending::Closed => "the pane closed",
        Ending::Quit => "Neovim quit",
        Ending::Stopped => "the daemon was stopped",
        Ending::Failed => "the daemon failed",
        Ending::Detached => "detached",
    }
}

impl Pane {
    /// Whether it shows window `win`, alone or among others.
    pub(crate) fn holds(&self, win: NeovimWindowHandle) -> bool {
        self.neovim_window_handle == Some(win)
            || self.layout.as_ref().is_some_and(|l| l.contains(win))
    }

    /// The windows it shows.
    pub(crate) fn windows(&self) -> Vec<NeovimWindowHandle> {
        match &self.layout {
            Some(l) => l.windows(),
            None => self.neovim_window_handle.into_iter().collect(),
        }
    }

    /// When the ESC that ended the pane's last read, if one waits, is due to
    /// be the Escape key.
    pub(crate) fn escape_deadline(&self) -> Option<std::time::Instant> {
        self.terminal_surface
            .as_ref()
            .and_then(|s| s.forwarder.escape_deadline())
    }

    /// A pane with just `id`: no parts, nothing set.
    fn blank(id: u32) -> Pane {
        Pane {
            id,
            attach_id: None,
            self_attached: false,
            taking: None,
            terminal: None,
            neovim_window_handle: None,
            layout: None,
            layout_asked: Vec::new(),
            terminal_surface: None,
            closing: None,
            tab: 0,
            layer: None,
            area: Area::Unreported,
            report_due: None,
            awaiting_size: false,
            placing: None,
            last_asked_win_size: LastAskedWinSize::default(),
            split: None,
            attach_due: None,
        }
    }

    /// e.g. `<C-w>v`.
    pub(crate) fn from_neovim_window(id: u32, window_handle: NeovimWindowHandle) -> Pane {
        assert!(window_handle != 0);
        Pane {
            neovim_window_handle: Some(window_handle),
            ..Pane::blank(id)
        }
    }

    /// A float group pane for a group of floats.
    pub(crate) fn from_group(id: u32, group: Group) -> Pane {
        Pane {
            layer: Some(Layer::new(group)),
            ..Pane::blank(id)
        }
    }

    /// A self-attached pane for `attach`, a terminal that attached of its
    /// own accord. It has no window yet.
    pub(crate) fn self_attached(id: u32, attach: SelfAttach) -> Pane {
        Pane {
            terminal: attach.terminal,
            terminal_surface: Some(attach.surface),
            self_attached: true,
            ..Pane::blank(id)
        }
    }

    /// Whether its terminal attached of its own accord rather than for a
    /// pane the daemon asked for.
    pub(crate) fn is_self_attached(&self) -> bool {
        self.self_attached
    }

    /// The self-attach a self-attached pane was made for, taken back from
    /// it; None once its terminal is gone.
    pub(crate) fn into_self_attach(mut self) -> Option<SelfAttach> {
        Some(SelfAttach {
            surface: self.terminal_surface.take()?,
            terminal: self.terminal.take(),
        })
    }

    /// A floating pane ready to draw its floats.
    pub(crate) fn layer_shown(&self) -> bool {
        self.layer.is_some_and(|l| l.fit == Fit::None)
            && self.terminal_surface.is_some()
            && self.closing.is_none()
    }

    /// A floating pane shown or on its way: its floats stay out of other panes.
    pub(crate) fn layer_hides(&self) -> bool {
        self.layer.is_some() && self.closing.is_none()
    }

    /// Holds the pane id, not a pointer, so a stale event can't reach a freed pane.
    pub(crate) fn token(&self, source_fd: PaneFd) -> usize {
        TOKEN_BASE + 2 * self.id as usize + source_fd as usize
    }

    /// Hanging up makes the helper restore its terminal and exit, and Rex
    /// close the pane. Its helper is told why its attach ends: its pane
    /// closed, or, as the daemon ends, `ending`, with Neovim's `exit` status
    /// if Neovim quit. One that is gone already is told nothing.
    pub(crate) fn deinit(self, ending: Ending, exit: Option<u8>) {
        let ending = match self.closing {
            Some(Close::TerminalHungUp) => return, // the fds close as `self` drops
            Some(Close::WindowClosed | Close::RequestFailed) => Ending::Closed,
            Some(Close::Shutdown) | None => ending,
        };
        let mut end = End::ending(ending, ending_message(ending), false);
        if ending == Ending::Quit {
            end.exit = exit;
        }
        if let Some(surface) = self.terminal_surface {
            surface.end_with(end);
        }
    }

    /// Give helper and terminal fds to the loop
    pub(crate) fn register_with(&self, event_loop: &Loop) -> event_loop::Result<()> {
        let surface = self.terminal_surface.as_ref().expect("no terminal surface");
        event_loop.add_read(surface.helper_fd(), self.token(PaneFd::HelperSocket))?;
        match surface.io.input_fd() {
            Some(fd) => event_loop.add_read(fd, self.token(PaneFd::Terminal)),
            None => Ok(()),
        }
    }

    /// Takes the helper and terminal fds off the loop. Call it before the pane
    /// is hung up: closing the terminal's fd doesn't take it off the loop on
    /// Linux, as the helper and Rex's shell hold copies of it. A pane that was
    /// never registered is fine.
    pub(crate) fn unregister_from(&self, event_loop: &Loop) {
        if let Some(surface) = &self.terminal_surface {
            let _ = event_loop.remove(surface.helper_fd());
            if let Some(fd) = surface.io.input_fd() {
                let _ = event_loop.remove(fd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::event_loop::Kind;
    use super::*;

    // A Surface here is a real one over a pty, closed as it drops.

    #[test]
    fn each_constructor_starts_with_the_parts_it_was_given() {
        let from_window = Pane::from_neovim_window(3, 1000);
        assert_eq!(3, from_window.id);
        assert_eq!(1000, from_window.neovim_window_handle.unwrap());
        assert!(from_window.terminal.is_none() && from_window.terminal_surface.is_none());

        let attached = Pane::self_attached(1, attach(Some(TerminalId::from("t"))));
        assert_eq!(1, attached.id);
        assert!(attached.terminal.is_some() && attached.terminal_surface.is_some());
        assert!(attached.neovim_window_handle.is_none() && attached.is_self_attached());
        let back = attached.into_self_attach().unwrap();
        assert_eq!(Some(TerminalId::from("t")), back.terminal);

        let without_block = Pane::self_attached(2, attach(None));
        assert!(without_block.terminal.is_none() && without_block.terminal_surface.is_some());
    }

    /// A self-attach of an 80×24 terminal, with the pane provider's id
    /// `terminal`.
    fn attach(terminal: Option<TerminalId>) -> SelfAttach {
        SelfAttach {
            surface: Surface::for_test(80, 24),
            terminal,
        }
    }

    /// `{attach}` ends in `--transport relay` when the daemon started
    /// under the relay diagnostic.
    #[test]
    fn a_requested_panes_command_carries_the_daemons_transport() {
        let pane = Pane {
            attach_id: PaneId::parse("4-abcdefghij"),
            ..Pane::blank(4)
        };
        let address = Address::from_arg("/run/d/sock".into());
        let words = |t| attach_words("/bin/x", &address, t, &pane).join(" ");
        let fd = "/bin/x attach --address /run/d/sock --pane 4-abcdefghij";
        assert_eq!(fd, words(Transport::Fd));
        assert_eq!(format!("{fd} --transport relay"), words(Transport::Relay));
    }

    #[test]
    fn tokens_round_trip_and_never_collide() {
        for id in [0u32, 1, 7, 100_000] {
            let pane = Pane::blank(id);
            for fd in [PaneFd::HelperSocket, PaneFd::Terminal] {
                let t = pane.token(fd);
                assert!(t >= TOKEN_BASE);
                let back = decode_token(t);
                assert_eq!(id, back.id);
                assert_eq!(fd, back.source_fd);
            }
            assert_ne!(
                pane.token(PaneFd::HelperSocket),
                pane.token(PaneFd::Terminal)
            );
            // the next pane's tokens start after this one's
            let next = Pane::blank(id + 1);
            assert!(next.token(PaneFd::HelperSocket) > pane.token(PaneFd::Terminal));
        }
    }

    // The pane's fds go to the loop under its tokens.
    #[test]
    fn register_with_watches_both_fds_under_the_panes_tokens() {
        use super::super::event_loop::Event;
        use super::super::surface::open_pty;
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (master, slave) = open_pty(80, 24);
        crate::term::raw::as_the_helper_does(slave);
        let (socket, mut helper) = UnixStream::pair().unwrap();
        let pane = Pane::self_attached(
            1,
            SelfAttach {
                surface: Surface::create(socket, slave, super::super::surface::cells(80, 24)),
                terminal: None,
            },
        );
        let event_loop = Loop::init().unwrap();
        pane.register_with(&event_loop).unwrap();

        let mut events = [Event::default(); 4];
        helper.write_all(b"w").unwrap();
        let got = event_loop.wait(&mut events).unwrap();
        assert_eq!(1, got.len());
        assert_eq!(pane.token(PaneFd::HelperSocket), got[0].token);
        assert_eq!(Kind::Read, got[0].kind);

        // take the helper's byte, so only the terminal is left to report
        let helper_fd = pane.terminal_surface.as_ref().unwrap().helper_fd();
        let mut byte = [0u8; 1];
        // SAFETY: `byte` is valid for one byte.
        assert_eq!(1, unsafe {
            libc::read(helper_fd, byte.as_mut_ptr().cast(), 1)
        });

        // SAFETY: writing to the pty's master, so the terminal has input.
        assert_eq!(1, unsafe { libc::write(master, b"x".as_ptr().cast(), 1) });
        let got = event_loop.wait(&mut events).unwrap();
        assert_eq!(1, got.len());
        assert_eq!(pane.token(PaneFd::Terminal), got[0].token);
        assert_eq!(Kind::Read, got[0].kind);

        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        pane.deinit(Ending::Stopped, None);
    }

    // Closing the terminal's fd doesn't end its watch on Linux, so a pane's
    // fds come off the loop first.
    #[test]
    fn unregister_from_stops_both_fds_reporting() {
        use super::super::event_loop::Event;
        use super::super::surface::open_pty;
        use std::io::Write;
        use std::os::unix::net::UnixStream;

        let (master, slave) = open_pty(80, 24);
        let (socket, mut helper) = UnixStream::pair().unwrap();
        let pane = Pane::self_attached(
            1,
            SelfAttach {
                surface: Surface::create(socket, slave, super::super::surface::cells(80, 24)),
                terminal: None,
            },
        );
        let event_loop = Loop::init().unwrap();
        pane.register_with(&event_loop).unwrap();
        pane.unregister_from(&event_loop);

        helper.write_all(b"w").unwrap();
        // SAFETY: writing to the pty's master, so the terminal has input.
        assert_eq!(1, unsafe { libc::write(master, b"x".as_ptr().cast(), 1) });
        event_loop.add_timer(1, 100, 1).unwrap();
        let mut events = [Event::default(); 4];
        let got = event_loop.wait(&mut events).unwrap();
        assert_eq!(1, got.len());
        assert_eq!(Kind::Timer, got[0].kind);

        // a pane that was never registered is fine
        pane.unregister_from(&event_loop);
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        pane.deinit(Ending::Stopped, None);
    }
}
