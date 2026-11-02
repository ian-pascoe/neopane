pub mod events;
pub mod keys;
pub(crate) mod pane_host;
pub mod panes;
pub mod rex;
pub mod tui;

/// The debug commands are a top layer: any error will do.
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
