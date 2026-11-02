//! The daemon's panes, found by id, Neovim window, Rex block or float group.
//!
//! A pane found here is only good until a pane is added or removed: it is a
//! borrow of `Panes`. What outlives one is the pane's `id` (never reused), so code that must call other `&mut` methods in
//! between holds the id and looks the pane up again (`by_id`/`by_id_mut`).
//! Each lookup has a `_mut` twin.

use super::pane::{NeovimWindowHandle, Pane};
use crate::screen::float_groups::Group;

pub struct Panes {
    pub list: Vec<Pane>,
    /// Never reused, so a stale token can't reach a newer pane. 0 is the startup pane.
    pub next_id: u32,
}

impl Default for Panes {
    fn default() -> Panes {
        Panes {
            list: Vec::new(),
            next_id: 1,
        }
    }
}

impl Panes {
    /// Hangs up every pane still open. (Dropping `Panes` does the same.)
    pub fn deinit(&mut self) {
        for mut p in self.list.drain(..) {
            if p.closing.is_none() {
                p.closing = Some(super::pane::Close::Shutdown);
            }
            p.deinit();
        }
    }

    /// Adds a pane that already has its id, such as the startup pane.
    pub fn add(&mut self, pane: Pane) -> &mut Pane {
        self.list.push(pane);
        self.list.last_mut().unwrap()
    }

    /// Adds a pane for `win`, with the next id.
    pub fn add_for_window(&mut self, win: NeovimWindowHandle) -> &mut Pane {
        let id = self.next_id;
        self.next_id += 1;
        self.add(Pane::from_neovim_window(id, win))
    }

    /// Adds a floating pane for group `g`, with the next id.
    pub fn add_for_group(&mut self, g: Group) -> &mut Pane {
        let id = self.next_id;
        self.next_id += 1;
        self.add(Pane::from_group(id, g))
    }

    /// Hangs up pane `i` and forgets it. Set its `closing` first.
    pub fn remove(&mut self, i: usize) {
        self.list.remove(i).deinit();
    }

    pub fn index_of(&self, id: u32) -> Option<usize> {
        self.list.iter().position(|p| p.id == id)
    }

    pub fn by_id(&self, id: u32) -> Option<&Pane> {
        self.list.iter().find(|p| p.id == id)
    }

    pub fn by_id_mut(&mut self, id: u32) -> Option<&mut Pane> {
        self.list.iter_mut().find(|p| p.id == id)
    }

    pub fn by_win(&self, win: i64) -> Option<&Pane> {
        if win == 0 {
            return None;
        }
        self.list
            .iter()
            .find(|p| p.neovim_window_handle == Some(win))
    }

    pub fn by_win_mut(&mut self, win: i64) -> Option<&mut Pane> {
        if win == 0 {
            return None;
        }
        self.list
            .iter_mut()
            .find(|p| p.neovim_window_handle == Some(win))
    }

    pub fn by_block(&self, block: &[u8]) -> Option<&Pane> {
        self.list
            .iter()
            .find(|p| p.rex_block.is_some_and(|b| b == block))
    }

    pub fn by_block_mut(&mut self, block: &[u8]) -> Option<&mut Pane> {
        self.list
            .iter_mut()
            .find(|p| p.rex_block.is_some_and(|b| b == block))
    }

    pub fn by_layer_key(&self, key: i64) -> Option<&Pane> {
        self.list
            .iter()
            .find(|p| p.layer.is_some_and(|l| l.group.key == key))
    }

    pub fn by_layer_key_mut(&mut self, key: i64) -> Option<&mut Pane> {
        self.list
            .iter_mut()
            .find(|p| p.layer.is_some_and(|l| l.group.key == key))
    }
}

impl Drop for Panes {
    fn drop(&mut self) {
        self.deinit();
    }
}

#[cfg(test)]
mod tests {
    use super::super::pane::{self, Close};
    use super::super::surface::Surface;
    use super::*;

    #[test]
    fn ids_are_never_reused() {
        let mut panes = Panes::default();
        let a = panes.add_for_window(1000).id;
        panes.remove(panes.index_of(a).unwrap());
        let b = panes.add_for_window(1001).id;
        assert_ne!(a, b);
        assert!(panes.by_id(a).is_none());
    }

    #[test]
    fn finds_a_pane_by_id_window_or_block() {
        let mut panes = Panes::default();
        let block = pane::rex_block(b"block:01a1891d-7ec1-77ef-b35e-a77e39f0e0e1").unwrap();
        let p = panes.add_for_window(1000);
        p.rex_block = Some(block);
        let id = p.id;
        assert_eq!(id, panes.by_id(id).unwrap().id);
        assert_eq!(id, panes.by_win(1000).unwrap().id);
        assert_eq!(id, panes.by_block(&block).unwrap().id);
        assert!(panes.by_win(0).is_none());
        assert!(panes.by_win(1001).is_none());
    }

    // The group lookup, and hanging up what's still open.
    #[test]
    fn finds_a_floating_pane_by_group_key() {
        let mut panes = Panes::default();
        let group = Group {
            key: 2000,
            ..Group::default()
        };
        let id = panes.add_for_group(group).id;
        assert_eq!(id, panes.by_layer_key(2000).unwrap().id);
        assert!(panes.by_layer_key(2001).is_none());
        assert!(panes.by_win(2000).is_none());
        panes.by_layer_key_mut(2000).unwrap().tab = 3;
        assert_eq!(3, panes.by_id(id).unwrap().tab);
    }

    #[test]
    fn deinit_hangs_up_every_pane_still_open() {
        use std::io::Read;
        let mut panes = Panes::default();
        let (socket, mut helper) = std::os::unix::net::UnixStream::pair().unwrap();
        let (master, slave) = super::super::surface::open_pty(80, 24);
        let mut p = Pane::from_neovim_window(5, 1000);
        p.terminal_surface = Some(Surface::create(socket, slave).unwrap());
        panes.add(p);
        panes.add_for_window(1001).closing = Some(Close::WindowClosed);

        // with nothing to read the terminal's output, restoring it can't wait on it
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        panes.deinit();
        assert!(panes.list.is_empty());
        // the helper sees the hang-up
        let mut byte = [0u8; 1];
        assert_eq!(0, helper.read(&mut byte).unwrap());
    }

    // Dropping Panes hangs up too, and doesn't wait on a terminal nobody
    // reads.
    #[test]
    fn dropping_panes_hangs_up_their_terminals() {
        let mut panes = Panes::default();
        panes.add(Pane::startup(None, Surface::for_test(80, 24)));
        drop(panes);
    }
}
