//! The pane providers: Rex's, built in, and later the command runner.

pub(crate) mod rex;

use crate::daemon::provider::PaneProvider;
#[cfg(test)]
use crate::daemon::provider::fake::FakeProvider;
use rex::RexProvider;

/// The daemon's pane provider, until #115 holds it as a
/// `Box<dyn PaneProvider>`: Rex's, concretely; in tests, also the fake.
pub(crate) enum Held {
    Rex(Box<RexProvider>),
    #[cfg(test)]
    Fake(FakeProvider),
}

impl Held {
    /// It, as the seam the opens and `close` are asked through.
    pub(crate) fn seam(&mut self) -> &mut dyn PaneProvider {
        match self {
            Held::Rex(r) => &mut **r,
            #[cfg(test)]
            Held::Fake(f) => f,
        }
    }

    pub(crate) fn seam_ref(&self) -> &dyn PaneProvider {
        match self {
            Held::Rex(r) => &**r,
            #[cfg(test)]
            Held::Fake(f) => f,
        }
    }

    /// Rex's, for its session's attach and connection, not on the seam yet.
    pub(crate) fn rex(&mut self) -> Option<&mut RexProvider> {
        match self {
            Held::Rex(r) => Some(r),
            #[cfg(test)]
            Held::Fake(_) => None,
        }
    }

    /// The fake, which a test drives.
    #[cfg(test)]
    pub(crate) fn fake(&mut self) -> &mut FakeProvider {
        match self {
            Held::Fake(f) => f,
            Held::Rex(_) => panic!("not the fake"),
        }
    }
}
