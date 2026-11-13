//! The pane providers: Rex's, built in, and the command runner; and
//! choosing one for a self-attach (`choose`). The daemon holds the one
//! chosen as a `Box<dyn PaneProvider>`, and knows nothing more of it.

pub(crate) mod choose;
pub(crate) mod command;
pub(crate) mod rex;
