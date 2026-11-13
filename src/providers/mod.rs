//! The pane providers, and choosing one:
//!
//! - `rex`: Rex's, built in, running inside the daemon.
//! - `command`: the command runner, which runs a table's capabilities as
//!   commands: the user's own tables, and the built-in ones.
//! - `builtin` and `tmux`: the hidden `provider builtin NAME CAPABILITY`
//!   command, which the built-in tables run, and what it does for tmux.
//! - `choose`: choosing a pane provider for a self-attach.
//! - `check`: `provider check`, which chooses as a self-attach here would,
//!   and reports it.
//! - `tty`: a terminal's device, for any `find` to prove a terminal.
//!
//! The daemon holds the one chosen as a `Box<dyn PaneProvider>`, and knows
//! nothing more of it.

pub mod builtin;
pub mod check;
pub(crate) mod choose;
pub(crate) mod command;
pub(crate) mod rex;
pub(crate) mod tmux;
pub(crate) mod tty;
