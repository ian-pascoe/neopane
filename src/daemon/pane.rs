//! Links a Rex pane to a Neovim window. The three parts arrive in any
//! order.
//!
//! A `Pane` is a plain value whose Surface is boxed (only the Surface needs a
//! stable address), and the rest of the daemon refers to a pane by its `id`,
//! a `u32` looked up in `Panes`, which a borrow of `Panes` couldn't outlive
//! anyway.

use std::os::fd::RawFd;

use super::event_loop::{self, Loop};
use super::surface::{Restore, Surface};
use crate::screen::float_groups::Group;

pub use crate::rex::ids::{block as rex_block, layer as rex_layer};

/// Never 0: Neovim's API reads 0 as the current window.
pub type NeovimWindowHandle = i64;

pub type RexBlock = crate::rex::ids::Block;
pub type RexLayer = crate::rex::ids::Layer;

pub struct Pane {
    /// Matches a helper's hello. 0 is the startup pane.
    pub id: u32,
    pub rex_block: Option<RexBlock>,
    /// None for a floating pane of floats, and the startup pane before setup.
    pub neovim_window_handle: Option<NeovimWindowHandle>,
    /// Owned.
    pub terminal_surface: Option<Box<Surface>>,
    pub closing: Option<Close>,
    /// The tab page made after setup its window belongs to, else 0. Its windows
    /// share a Rex floating pane.
    pub tab: i64,
    /// Set for a Rex floating pane showing a group of floats, not a window.
    pub layer: Option<Layer>,
    /// Where Rex put the pane in its window, as fractions: x, y, w, h.
    pub rect: Option<[f64; 4]>,
    /// A new split's terminal starts at Rex's default size: keep the guessed window size until the app's first resize.
    pub awaiting_layout: bool,
    pub last_asked_win_size: LastAskedWinSize,
}

/// What `Pane::last_asked_win_size` holds: the grid, and the window's cols x rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LastAskedWinSize {
    pub grid: u32,
    pub cols: u32,
    pub rows: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    #[default]
    None,
    /// The request's id.
    Asked(u32),
    /// Answered: the next flush has the new layout.
    Answered,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layer {
    pub group: Group,
    /// Rex's id for it, once made.
    pub id: Option<RexLayer>,
    /// Its floats being refitted to its first size: not drawn until Neovim
    /// has answered and redrawn them, so the guessed size never shows.
    pub fit: Fit,
    /// Where it sits in the window, as fractions: x, y, w, h. What we asked
    /// for, until the user moves it.
    pub bounds: [f64; 4],
}

impl Layer {
    /// A layer for `group`, not yet made in Rex.
    pub fn new(group: Group) -> Layer {
        Layer {
            group,
            id: None,
            fit: Fit::None,
            bounds: [0.0, 0.0, 0.0, 0.0],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Close {
    /// The terminal is already gone, so it isn't restored.
    RexPaneClosed,
    WindowClosed,
    RequestFailed,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    RexBlock,
    TerminalSurface,
    NeovimWindow,
}

/// A set of `Part`s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Parts(u8);

impl Parts {
    pub fn empty() -> Parts {
        Parts(0)
    }

    pub fn init_one(part: Part) -> Parts {
        Parts(1 << part as u8)
    }

    pub fn init_many(parts: &[Part]) -> Parts {
        let mut set = Parts::empty();
        for &part in parts {
            set.insert(part);
        }
        set
    }

    pub fn insert(&mut self, part: Part) {
        self.0 |= 1 << part as u8;
    }

    pub fn contains(&self, part: Part) -> bool {
        self.0 & (1 << part as u8) != 0
    }

    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

/// Above Daemon's fixed tokens, so they never clash.
pub const TOKEN_BASE: usize = 8;

/// Which of a pane's fds an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PaneFd {
    HelperSocket = 0,
    Terminal = 1,
}

/// What `decode_token` finds in a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodedToken {
    pub id: u32,
    pub source_fd: PaneFd,
}

/// The pane may be gone; look it up by id.
pub fn decode_token(t: usize) -> DecodedToken {
    DecodedToken {
        id: u32::try_from((t - TOKEN_BASE) / 2).expect("pane token out of range"),
        source_fd: if (t - TOKEN_BASE).is_multiple_of(2) {
            PaneFd::HelperSocket
        } else {
            PaneFd::Terminal
        },
    }
}

impl Pane {
    /// When the ESC that ended the pane's last read, if one waits, is due to
    /// be the Escape key.
    pub fn escape_deadline(&self) -> Option<std::time::Instant> {
        self.terminal_surface
            .as_ref()
            .and_then(|s| s.forwarder.escape_deadline())
    }

    /// A pane with just `id`: no parts, nothing set.
    fn blank(id: u32) -> Pane {
        Pane {
            id,
            rex_block: None,
            neovim_window_handle: None,
            terminal_surface: None,
            closing: None,
            tab: 0,
            layer: None,
            rect: None,
            awaiting_layout: false,
            last_asked_win_size: LastAskedWinSize::default(),
        }
    }

    /// A pane that only has `id`, for the tests of other modules.
    #[cfg(test)]
    pub fn for_test(id: u32) -> Pane {
        Pane::blank(id)
    }

    /// e.g. `<C-w>v`.
    pub fn from_neovim_window(id: u32, window_handle: NeovimWindowHandle) -> Pane {
        assert!(window_handle != 0);
        Pane {
            neovim_window_handle: Some(window_handle),
            ..Pane::blank(id)
        }
    }

    /// A Rex floating pane for a group of floats.
    pub fn from_group(id: u32, group: Group) -> Pane {
        Pane {
            layer: Some(Layer::new(group)),
            ..Pane::blank(id)
        }
    }

    /// e.g. Cmd+D. Not used yet.
    pub fn from_rex_pane(id: u32, block: RexBlock, surface: Box<Surface>) -> Pane {
        Pane {
            rex_block: Some(block),
            terminal_surface: Some(surface),
            ..Pane::blank(id)
        }
    }

    /// The pane `neorex` was run in. `block` is None without REX_BLOCK.
    pub fn startup(block: Option<RexBlock>, surface: Box<Surface>) -> Pane {
        Pane {
            rex_block: block,
            terminal_surface: Some(surface),
            ..Pane::blank(0)
        }
    }

    /// Needs no Rex block: drawing only uses the terminal.
    pub fn drawable(&self) -> bool {
        self.neovim_window_handle.is_some()
            && self.terminal_surface.is_some()
            && self.closing.is_none()
    }

    /// A floating pane ready to draw its floats.
    pub fn layer_shown(&self) -> bool {
        self.layer.is_some_and(|l| l.fit == Fit::None)
            && self.terminal_surface.is_some()
            && self.closing.is_none()
    }

    /// A floating pane shown or on its way: its floats stay out of other panes.
    pub fn layer_hides(&self) -> bool {
        self.layer.is_some() && self.closing.is_none()
    }

    pub fn missing(&self) -> Parts {
        let mut set = Parts::empty();
        if self.rex_block.is_none() {
            set.insert(Part::RexBlock);
        }
        if self.terminal_surface.is_none() {
            set.insert(Part::TerminalSurface);
        }
        if self.neovim_window_handle.is_none() {
            set.insert(Part::NeovimWindow);
        }
        set
    }

    /// Holds the pane id, not a pointer, so a stale event can't reach a freed pane.
    pub fn token(&self, source_fd: PaneFd) -> usize {
        TOKEN_BASE + 2 * self.id as usize + source_fd as usize
    }

    /// Hanging up makes the helper exit, and Rex closes the pane. A pane
    /// closing while the daemon runs is restored without waiting for its
    /// terminal; one the daemon leaves as it ends (`closing` is `Shutdown`, or
    /// None before the pane was set up) waits a moment for it.
    pub fn deinit(self) {
        let restore = match self.closing {
            Some(Close::RexPaneClosed) => Restore::No,
            Some(Close::WindowClosed | Close::RequestFailed) => Restore::Now,
            Some(Close::Shutdown) | None => Restore::Waiting,
        };
        if let Some(surface) = self.terminal_surface {
            surface.destroy(restore, None);
        }
    }

    /// `deinit` as the daemon gives up, leaving `message` on the restored
    /// terminal.
    pub fn deinit_with_message(self, message: &[u8]) {
        if let Some(surface) = self.terminal_surface {
            surface.destroy(Restore::Waiting, Some(message));
        }
    }

    /// Give helper and terminal fds to the loop
    pub fn register_with(&self, event_loop: &Loop) -> event_loop::Result<()> {
        let surface = self.terminal_surface.as_ref().expect("no terminal surface");
        event_loop.add_read(surface.helper_fd(), self.token(PaneFd::HelperSocket))?;
        let terminal_fd: RawFd = surface.terminal.fd;
        event_loop.add_read(terminal_fd, self.token(PaneFd::Terminal))
    }

    /// Takes the helper and terminal fds off the loop. Call it before the pane
    /// is hung up: closing the terminal's fd doesn't take it off the loop on
    /// Linux, as the helper and Rex's shell hold copies of it. A pane that was
    /// never registered is fine.
    pub fn unregister_from(&self, event_loop: &Loop) {
        if let Some(surface) = &self.terminal_surface {
            let _ = event_loop.remove(surface.helper_fd());
            let _ = event_loop.remove(surface.terminal.fd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::event_loop::Kind;
    use super::*;

    const TEST_ID: &str = "block:01a17fb6-c5b4-7d9c-8e1e-9fd2a65e8a50";

    fn test_block() -> RexBlock {
        TEST_ID.as_bytes()[..42].try_into().unwrap()
    }

    // A Surface here is a real one over a pty, closed as it drops.

    #[test]
    fn each_constructor_starts_with_the_parts_it_was_given() {
        let from_window = Pane::from_neovim_window(3, 1000);
        assert_eq!(3, from_window.id);
        assert_eq!(1000, from_window.neovim_window_handle.unwrap());
        assert_eq!(
            Parts::init_many(&[Part::RexBlock, Part::TerminalSurface]),
            from_window.missing()
        );

        let from_rex = Pane::from_rex_pane(4, test_block(), Surface::for_test(80, 24));
        assert_eq!(4, from_rex.id);
        assert_eq!(Parts::init_one(Part::NeovimWindow), from_rex.missing());

        let at_startup = Pane::startup(Some(test_block()), Surface::for_test(80, 24));
        assert_eq!(0, at_startup.id);
        assert_eq!(Parts::init_one(Part::NeovimWindow), at_startup.missing());

        let without_block = Pane::startup(None, Surface::for_test(80, 24));
        assert_eq!(
            Parts::init_many(&[Part::RexBlock, Part::NeovimWindow]),
            without_block.missing()
        );
    }

    #[test]
    fn missing_and_drawable_for_every_mix_of_parts() {
        for bits in 0..8 {
            let has_block = (bits & 1) != 0;
            let has_surface = (bits & 2) != 0;
            let has_window = (bits & 4) != 0;
            let pane = Pane {
                id: 1,
                rex_block: has_block.then(test_block),
                terminal_surface: has_surface.then(|| Surface::for_test(80, 24)),
                neovim_window_handle: has_window.then_some(1000),
                ..Pane::blank(1)
            };

            let mut want = Parts::empty();
            if !has_block {
                want.insert(Part::RexBlock);
            }
            if !has_surface {
                want.insert(Part::TerminalSurface);
            }
            if !has_window {
                want.insert(Part::NeovimWindow);
            }
            assert_eq!(want, pane.missing());

            // the block doesn't matter
            assert_eq!(has_surface && has_window, pane.drawable());
        }
    }

    #[test]
    fn a_closing_pane_isnt_drawable_whatever_the_reason() {
        for reason in [
            Close::RexPaneClosed,
            Close::WindowClosed,
            Close::RequestFailed,
            Close::Shutdown,
        ] {
            let pane = Pane {
                id: 1,
                rex_block: Some(test_block()),
                terminal_surface: Some(Surface::for_test(80, 24)),
                neovim_window_handle: Some(1000),
                closing: Some(reason),
                ..Pane::blank(1)
            };
            assert!(!pane.drawable());
        }
    }

    #[test]
    fn rex_block_takes_42_byte_ids_only() {
        let block = rex_block(TEST_ID.as_bytes()).unwrap();
        assert_eq!(TEST_ID.as_bytes(), &block);
        assert_eq!(
            Err(crate::rex::ids::Error::BadRexBlock),
            rex_block(&TEST_ID.as_bytes()[..41])
        );
        assert_eq!(
            Err(crate::rex::ids::Error::BadRexBlock),
            rex_block(format!("{TEST_ID}0").as_bytes())
        );
        assert_eq!(Err(crate::rex::ids::Error::BadRexBlock), rex_block(b""));
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
        let (socket, mut helper) = UnixStream::pair().unwrap();
        let pane = Pane::startup(None, Surface::create(socket, slave).unwrap());
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

        // SAFETY: closing the pty's master, so restoring the terminal needn't wait.
        unsafe { libc::close(master) };
        pane.deinit();
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
        let pane = Pane::startup(None, Surface::create(socket, slave).unwrap());
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
        // SAFETY: closing the pty's master, so restoring the terminal needn't wait.
        unsafe { libc::close(master) };
        pane.deinit();
    }
}
