//! The pane providers: Rex's, built in; tmux's, a table whose commands are
//! `provider builtin tmux` (`builtin`, `tmux`); and the command runner; choosing
//! one for a self-attach (`choose`); and `provider check`, which chooses as
//! a self-attach here would and reports it (`check`). The daemon holds the
//! one chosen as a `Box<dyn PaneProvider>`, and knows nothing more of it.

pub mod builtin;
pub mod check;
pub(crate) mod choose;
pub(crate) mod command;
pub(crate) mod rex;
pub(crate) mod tmux;
pub(crate) mod tty;
