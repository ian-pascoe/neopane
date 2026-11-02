//! Keeps typed input behind the window switches before it. Neovim applies
//! `nvim_input` the moment it's read, but runs `nvim_set_current_win` on its
//! next turn of the loop, so keys sent right after a switch would reach the
//! window that was current before it. Only one switch is ever in flight: a
//! switch asked for while another is, and the input typed after it, wait
//! their turn. Input reaches Neovim in the order it was typed, each key
//! after the switches typed before it, with two exceptions:
//!
//! - Focus reports never wait: they only tell us what the terminal has.
//! - Neovim can't make a switch while it waits for a key to finish a
//!   command, such as after `f`; holding every key would wait for a key
//!   that never comes. `probe_again`, called on a timer, asks
//!   whether Neovim waits; if it does, the first key held goes through,
//!   wherever it was typed, to finish the command. The rest stay held, and
//!   the switch follows.
//!
//! An answer to that ask is only good if no key went to Neovim since it was
//! asked, and Neovim may not have read a key that went just before: so no
//! ask is made on the tick after a key went through, and a key going through
//! voids the ask outstanding. A busy Neovim answers none, and no key leaves.
//!
//! A switch that fails, such as for a window that closed first, drops the
//! input typed into its pane behind it.
//!
//! A failed write leaves the switch in flight: Neovim's connection is gone.

use std::io;

use crate::nvim::forwarder::{self, Forwarder};
use crate::nvim::msgpack::Decoder;
use crate::nvim::nvim::Nvim;
use crate::nvim::rpc;
use crate::nvim::windows;
use crate::term::input;

/// What the gate needs of the panes (Zig's `panes: anytype`): the
/// `Forwarder` that parses the input typed into a pane, if it has one.
pub trait AimForwarder {
    /// The pane `pane`'s forwarder, with its targets aimed, or None if the
    /// pane is gone.
    fn aim_forwarder(&mut self, pane: u32) -> Option<&mut Forwarder>;
}

#[derive(Debug, Default)]
pub struct FocusGate {
    /// The switch in flight: asked for, not yet answered.
    pub pending: Option<Switch>,
    /// Our `nvim_get_mode` asked for it, which Neovim answers when it's waiting
    /// for a key, but not when it's busy.
    pub probe: Option<u32>,
    /// The last probe found Neovim waiting for a key, and no key has gone since.
    pub blocked: bool,
    /// A key went through since the last tick, so a probe now may not see it read.
    pub settling: bool,
    /// What waits for the switch in flight, in the order it came. Never
    /// nonempty without `pending`.
    pub queue: Vec<Item>,
}

/// A request, and the pane whose window it makes current.
#[derive(Debug, Clone, Copy)]
pub struct Switch {
    pub req: u32,
    pub pane: u32,
}

#[derive(Debug)]
pub enum Item {
    SwitchTo {
        win: i64,
        pane: u32,
    },
    /// What one read of a pane's terminal gave, and how much has gone.
    Input {
        pane: u32,
        bytes: Vec<u8>,
        read: usize,
    },
}

impl FocusGate {
    /// The request of the switch in flight, if any.
    pub fn in_flight(&self) -> Option<u32> {
        self.pending.map(|p| p.req)
    }

    /// Asks Neovim to make `win`, the window of `pane`, current once the
    /// switches and input before it are through.
    pub fn switch_to(&mut self, nvim: &mut Nvim, win: i64, pane: u32) -> io::Result<()> {
        if self.pending.is_some() {
            self.queue.push(Item::SwitchTo { win, pane });
            return Ok(());
        }
        self.start(nvim, win, pane)
    }

    /// Sends `bytes`, typed into `pane`, through the forwarder
    /// `panes.aim_forwarder(pane)` finds, once the switches asked for so far
    /// are made. Dropped if it finds none.
    pub fn send(
        &mut self,
        nvim: &mut Nvim,
        panes: &mut impl AimForwarder,
        pane: u32,
        bytes: &[u8],
    ) -> forwarder::Result<()> {
        if self.pending.is_some() && input::only_focus_reports(bytes) {
            // no wait, unless the parser is partway through something
            let Some(fwd) = panes.aim_forwarder(pane) else {
                return Ok(());
            };
            if fwd.at_boundary() {
                fwd.feed(nvim, bytes)?;
                return Ok(());
            }
        } else if self.pending.is_none() {
            let Some(fwd) = panes.aim_forwarder(pane) else {
                return Ok(());
            };
            fwd.feed(nvim, bytes)?;
            return Ok(());
        }
        // one item per read: the parser takes the end of a read for the end of
        // a key, as for a lone Escape
        self.queue.push(Item::Input {
            pane,
            bytes: bytes.to_vec(),
            read: 0,
        });
        if self.blocked {
            self.release(nvim, panes)?;
        }
        Ok(())
    }

    /// Neovim answered request `id`, with error `err` or else `result`: if it
    /// was ours, goes on with what waited for it. `panes` is as for `send`.
    /// (The Zig takes the `rpc.Message.Response`; a Rust message's variant
    /// has no type of its own, so its fields come one by one.)
    pub fn answered(
        &mut self,
        nvim: &mut Nvim,
        panes: &mut impl AimForwarder,
        id: u32,
        err: Option<&[u8]>,
        result: Decoder<'_>,
    ) -> forwarder::Result<()> {
        if self.probe == Some(id) {
            self.probe = None;
            let mut result = result;
            if err.is_none() && is_blocking(&mut result).unwrap_or(false) {
                self.blocked = true;
                self.release(nvim, panes)?;
            }
            return Ok(());
        }
        let Some(switching) = self.pending else {
            return Ok(());
        };
        if switching.req != id {
            return Ok(());
        }
        let failed = err.is_some();
        self.probe = None;
        self.blocked = false;
        self.settling = false;
        // `pending` stays until the queue is through, so a failed write leaves
        // input held, never ahead of what's queued
        while !self.queue.is_empty() {
            match self.queue.remove(0) {
                Item::SwitchTo { win, pane } => return Ok(self.start(nvim, win, pane)?),
                Item::Input { pane, bytes, read } => {
                    let Some(fwd) = panes.aim_forwarder(pane) else {
                        continue;
                    };
                    let rest = &bytes[read..];
                    if failed && pane == switching.pane {
                        fwd.discard(nvim, rest)?;
                    } else {
                        fwd.feed(nvim, rest)?;
                    }
                }
            }
        }
        self.pending = None;
        Ok(())
    }

    /// For the timer: asks whether Neovim is waiting for a key, if the switch
    /// in flight is unanswered and no ask is out. Not right after a key went
    /// through, see above.
    pub fn probe_again(&mut self, nvim: &mut Nvim) -> io::Result<()> {
        if self.pending.is_none() || self.probe.is_some() || self.blocked {
            return Ok(());
        }
        if self.settling {
            self.settling = false;
            return Ok(());
        }
        self.probe = Some(nvim.request("nvim_get_mode", 0));
        nvim.flush()
    }

    /// Asks for `win` as the switch in flight. No ask yet whether Neovim waits:
    /// a key sent just before may not be read, which would show as waiting.
    fn start(&mut self, nvim: &mut Nvim, win: i64, pane: u32) -> io::Result<()> {
        self.pending = Some(Switch {
            req: windows::set_current(nvim, win),
            pane,
        });
        self.probe = None;
        self.blocked = false;
        self.settling = false;
        nvim.flush()
    }

    /// Neovim waits for a key: sends the first key held, which it needs to get
    /// on. If none is held the next one sent goes at once.
    fn release(&mut self, nvim: &mut Nvim, panes: &mut impl AimForwarder) -> forwarder::Result<()> {
        let mut i: usize = 0;
        while i < self.queue.len() {
            let Item::Input { pane, bytes, read } = &mut self.queue[i] else {
                i += 1;
                continue;
            };
            let Some(fwd) = panes.aim_forwarder(*pane) else {
                self.queue.remove(i);
                continue;
            };
            let fed = fwd.feed_key(nvim, &bytes[*read..])?;
            *read += fed.read;
            if *read >= bytes.len() {
                self.queue.remove(i);
            }
            if fed.sent_key {
                self.probe = None;
                self.blocked = false;
                self.settling = true;
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Whether the `nvim_get_mode` result at `result` says Neovim is waiting for a key.
pub fn is_blocking(result: &mut Decoder<'_>) -> rpc::Result<bool> {
    let mut pairs = rpc::expect_map(result)?;
    while pairs > 0 {
        let key = rpc::expect_str(result)?;
        if key == b"blocking" {
            return rpc::expect_bool(result);
        }
        rpc::skip(result)?;
        pairs -= 1;
    }
    Ok(false)
}

// The Zig FocusGate has no tests of its own (`NvimSession.zig`'s drive it);
// these cover its queue and `is_blocking` directly.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::msgpack;
    use crate::nvim::nvim::spawn_for_test;

    /// Panes with no forwarders: whatever is sent to one is dropped.
    struct NoPanes;

    impl AimForwarder for NoPanes {
        fn aim_forwarder(&mut self, _pane: u32) -> Option<&mut Forwarder> {
            None
        }
    }

    fn mode_map(pairs: &[(&str, Option<bool>)]) -> Vec<u8> {
        let mut buf = Vec::new();
        msgpack::write_map_header(&mut buf, pairs.len() as u32);
        for (key, value) in pairs {
            msgpack::write_str(&mut buf, key);
            match value {
                Some(b) => msgpack::write_bool(&mut buf, *b),
                None => msgpack::write_str(&mut buf, "n"),
            }
        }
        buf
    }

    #[test]
    fn the_mode_says_whether_neovim_is_blocking() {
        let yes = mode_map(&[("mode", None), ("blocking", Some(true))]);
        assert_eq!(Ok(true), is_blocking(&mut Decoder::of(&yes)));
        let no = mode_map(&[("blocking", Some(false)), ("mode", None)]);
        assert_eq!(Ok(false), is_blocking(&mut Decoder::of(&no)));
        let none = mode_map(&[("mode", None)]);
        assert_eq!(Ok(false), is_blocking(&mut Decoder::of(&none)));
        assert!(is_blocking(&mut Decoder::of(b"\x90")).is_err());
    }

    #[test]
    fn one_switch_is_in_flight_and_what_comes_after_waits_for_it() {
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        let mut gate = FocusGate::default();
        gate.switch_to(&mut nvim, 1000, 1).unwrap();
        let first = gate.in_flight().unwrap();
        gate.switch_to(&mut nvim, 1001, 2).unwrap();
        // input typed behind them, into a pane that's gone by then
        gate.send(&mut nvim, &mut NoPanes, 2, b"abc").unwrap();
        assert_eq!(2, gate.queue.len());
        assert_eq!(Some(first), gate.in_flight());

        // an answer that isn't the switch's changes nothing
        gate.answered(&mut nvim, &mut NoPanes, first + 100, None, Decoder::new())
            .unwrap();
        assert_eq!(Some(first), gate.in_flight());

        // its answer starts the next switch, and what came after it waits
        gate.answered(&mut nvim, &mut NoPanes, first, None, Decoder::new())
            .unwrap();
        let second = gate.in_flight().unwrap();
        assert_ne!(first, second);
        assert_eq!(1, gate.queue.len());

        // the last answer lets the input go, here dropped, and the gate is idle
        gate.answered(&mut nvim, &mut NoPanes, second, None, Decoder::new())
            .unwrap();
        assert_eq!(None, gate.in_flight());
        assert!(gate.queue.is_empty());
    }
}
