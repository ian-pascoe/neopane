//! The pane providers: Rex's, built in, and the command runner.

// built from a config's `provider` once it is chosen, from #129
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod command;
pub(crate) mod rex;

use std::os::fd::RawFd;

use crate::daemon::provider::PaneProvider;
#[cfg(test)]
use crate::daemon::provider::fake::FakeProvider;
use command::CommandProvider;
use rex::RexProvider;

/// The daemon's pane provider, until #115 holds it as a
/// `Box<dyn PaneProvider>`: Rex's or the command runner, concretely; in
/// tests, also the fake.
pub(crate) enum Held {
    Rex(Box<RexProvider>),
    #[cfg_attr(not(test), allow(dead_code))]
    Command(Box<CommandProvider>),
    #[cfg(test)]
    Fake(FakeProvider),
}

impl Held {
    /// It, as the seam the opens and `close` are asked through.
    pub(crate) fn seam(&mut self) -> &mut dyn PaneProvider {
        match self {
            Held::Rex(r) => &mut **r,
            Held::Command(c) => &mut **c,
            #[cfg(test)]
            Held::Fake(f) => f,
        }
    }

    pub(crate) fn seam_ref(&self) -> &dyn PaneProvider {
        match self {
            Held::Rex(r) => &**r,
            Held::Command(c) => &**c,
            #[cfg(test)]
            Held::Fake(f) => f,
        }
    }

    /// Rex's, for its session's attach and connection, not on the seam yet.
    pub(crate) fn rex(&mut self) -> Option<&mut RexProvider> {
        match self {
            Held::Rex(r) => Some(r),
            _ => None,
        }
    }

    /// The fd it is watched by under `Token::Provider`.
    pub(crate) fn fd(&self) -> Option<RawFd> {
        match self {
            Held::Rex(r) => Some(r.session.client.fd()),
            Held::Command(c) => Some(c.fd()),
            #[cfg(test)]
            Held::Fake(_) => None,
        }
    }

    /// The fake, which a test drives.
    #[cfg(test)]
    pub(crate) fn fake(&mut self) -> &mut FakeProvider {
        match self {
            Held::Fake(f) => f,
            _ => panic!("not the fake"),
        }
    }
}
