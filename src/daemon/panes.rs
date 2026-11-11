//! The daemon's panes, found by id, Neovim window, Rex block or float group.
//!
//! A pane found here is only good until a pane is added or removed: it is a
//! borrow of `Panes`. What outlives one is the pane's `id` (never reused), so code that must call other `&mut` methods in
//! between holds the id and looks the pane up again (`by_id`/`by_id_mut`).
//! Each lookup has a `_mut` twin.

use super::event_loop::Loop;
use super::pane::{NeovimWindowHandle, Pane};
use crate::attach::{Ending, PaneId, PaneIdRegistry};
use crate::screen::float_groups::Group;

pub(crate) struct Panes {
    pub list: Vec<Pane>,
    /// Never reused, so a stale token can't reach a newer pane. 0 is the startup pane.
    pub next_id: u32,
    /// The pane id of every requested pane, minted as it is added: good for
    /// one attach until its pane goes.
    pub ids: PaneIdRegistry,
}

impl Default for Panes {
    fn default() -> Panes {
        Panes {
            list: Vec::new(),
            next_id: 1,
            ids: PaneIdRegistry::new(),
        }
    }
}

impl Panes {
    /// Hangs up every pane still open, its attach ending as `ending` says.
    /// (Dropping `Panes` does the same, as stopped.)
    pub(crate) fn deinit(&mut self, ending: Ending) {
        for mut p in self.list.drain(..) {
            if p.closing.is_none() {
                p.closing = Some(super::pane::Close::Shutdown);
            }
            p.deinit(ending);
        }
    }

    /// Adds a pane that already has its id, such as the startup pane.
    pub(crate) fn add(&mut self, pane: Pane) -> &mut Pane {
        self.list.push(pane);
        self.list.last_mut().unwrap()
    }

    /// Adds a pane for `win`, with the next id and a new pane id.
    pub(crate) fn add_for_window(&mut self, win: NeovimWindowHandle) -> &mut Pane {
        let id = self.next_id;
        self.next_id += 1;
        let mut pane = Pane::from_neovim_window(id, win);
        pane.attach_id = Some(self.ids.mint());
        self.add(pane)
    }

    /// Adds a floating pane for group `g`, with the next id and a new pane id.
    pub(crate) fn add_for_group(&mut self, g: Group) -> &mut Pane {
        let id = self.next_id;
        self.next_id += 1;
        let mut pane = Pane::from_group(id, g);
        pane.attach_id = Some(self.ids.mint());
        self.add(pane)
    }

    /// Takes every pane's fds off `event_loop`, for before they are hung up.
    pub(crate) fn unregister_all(&self, event_loop: &Loop) {
        for p in &self.list {
            p.unregister_from(event_loop);
        }
    }

    /// Takes pane `i` off `event_loop`, hangs it up and forgets it. Set its
    /// `closing` first. Its pane id is good for no attach after.
    pub(crate) fn remove(&mut self, i: usize, event_loop: &Loop) {
        let pane = self.list.remove(i);
        if let Some(id) = &pane.attach_id {
            self.ids.expire(id);
        }
        pane.unregister_from(event_loop);
        pane.deinit(Ending::Closed);
    }

    /// The pane whose helper's hello names `id`.
    pub(crate) fn by_attach_id(&self, id: &PaneId) -> Option<&Pane> {
        self.list.iter().find(|p| p.attach_id.as_ref() == Some(id))
    }

    pub(crate) fn index_of(&self, id: u32) -> Option<usize> {
        self.list.iter().position(|p| p.id == id)
    }

    pub(crate) fn by_id(&self, id: u32) -> Option<&Pane> {
        self.list.iter().find(|p| p.id == id)
    }

    pub(crate) fn by_id_mut(&mut self, id: u32) -> Option<&mut Pane> {
        self.list.iter_mut().find(|p| p.id == id)
    }

    pub(crate) fn by_win(&self, win: i64) -> Option<&Pane> {
        if win == 0 {
            return None;
        }
        self.list.iter().find(|p| p.holds(win))
    }

    pub(crate) fn by_win_mut(&mut self, win: i64) -> Option<&mut Pane> {
        if win == 0 {
            return None;
        }
        self.list.iter_mut().find(|p| p.holds(win))
    }

    /// The pane of the pane provider's terminal `t`.
    pub(crate) fn by_terminal(&self, t: &str) -> Option<&Pane> {
        self.list
            .iter()
            .find(|p| p.terminal.as_ref().is_some_and(|x| x.as_str() == t))
    }

    pub(crate) fn by_terminal_mut(&mut self, t: &str) -> Option<&mut Pane> {
        self.list
            .iter_mut()
            .find(|p| p.terminal.as_ref().is_some_and(|x| x.as_str() == t))
    }

    pub(crate) fn by_layer_key(&self, key: i64) -> Option<&Pane> {
        self.list
            .iter()
            .find(|p| p.layer.is_some_and(|l| l.group.key == key))
    }
}

impl Drop for Panes {
    fn drop(&mut self) {
        self.deinit(Ending::Stopped);
    }
}

#[cfg(test)]
mod tests {
    use super::super::pane::Close;
    use super::super::provider::TerminalId;
    use super::super::surface::Surface;
    use super::*;
    use crate::attach::Refusal;

    #[test]
    fn a_pane_id_is_good_until_an_attach_takes_it_or_its_pane_goes() {
        let mut panes = Panes::default();
        let taken = panes.add_for_window(1000).attach_id.unwrap();
        let gone = panes.add_for_group(Group::default()).attach_id.unwrap();
        assert_ne!(taken, gone);
        assert_eq!(
            1000,
            panes
                .by_attach_id(&taken)
                .unwrap()
                .neovim_window_handle
                .unwrap()
        );
        assert_eq!(Ok(()), panes.ids.take(&taken));
        assert_eq!(Err(Refusal::PaneTaken), panes.ids.take(&taken));
        let i = panes
            .index_of(panes.by_attach_id(&gone).unwrap().id)
            .unwrap();
        panes.remove(i, &Loop::init().unwrap());
        assert_eq!(Err(Refusal::PaneExpired), panes.ids.take(&gone));
        assert!(panes.by_attach_id(&gone).is_none());
    }

    #[test]
    fn ids_are_never_reused() {
        let mut panes = Panes::default();
        let a = panes.add_for_window(1000).id;
        panes.remove(panes.index_of(a).unwrap(), &Loop::init().unwrap());
        let b = panes.add_for_window(1001).id;
        assert_ne!(a, b);
        assert!(panes.by_id(a).is_none());
    }

    #[test]
    fn finds_a_pane_by_id_window_or_terminal() {
        let mut panes = Panes::default();
        let p = panes.add_for_window(1000);
        p.terminal = Some(TerminalId::from("block:b"));
        let id = p.id;
        assert_eq!(id, panes.by_id(id).unwrap().id);
        assert_eq!(id, panes.by_win(1000).unwrap().id);
        assert_eq!(id, panes.by_terminal("block:b").unwrap().id);
        assert!(panes.by_terminal("block:c").is_none());
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
    }

    #[test]
    fn deinit_hangs_up_every_pane_still_open() {
        let mut panes = Panes::default();
        let (socket, helper) = std::os::unix::net::UnixStream::pair().unwrap();
        let (master, slave) = super::super::surface::open_pty(80, 24);
        let mut p = Pane::from_neovim_window(5, 1000);
        p.terminal_surface =
            Some(Surface::create(socket, slave, super::super::surface::cells(80, 24)).unwrap());
        panes.add(p);
        panes.add_for_window(1001).closing = Some(Close::WindowClosed);

        // with nothing to read the terminal's output, restoring it can't wait on it
        // SAFETY: closing the pty's master.
        unsafe { libc::close(master) };
        panes.deinit(Ending::Stopped);
        assert!(panes.list.is_empty());
        // the helper is told the daemon stopped, then hung up on
        let said = crate::daemon::attach_conn::read_all_frames(&helper);
        let [crate::attach::Message::End(z)] = &said[..] else {
            panic!("{said:?}")
        };
        assert_eq!(Ending::Stopped, z.as_ending());
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
