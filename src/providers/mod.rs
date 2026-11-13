//! The pane providers: Rex's, built in, and the command runner; choosing
//! one for a self-attach (`choose`); and `provider check`, which chooses as
//! a self-attach here would and reports it (`check`). The daemon holds the
//! one chosen as a `Box<dyn PaneProvider>`, and knows nothing more of it.

pub mod check;
pub(crate) mod choose;
pub(crate) mod command;
pub(crate) mod rex;
