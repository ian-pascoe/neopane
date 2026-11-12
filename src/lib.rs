//! neorex: a Rex-native Neovim frontend.

#![warn(unreachable_pub)]

pub mod attach;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod debug;
pub mod log;
pub mod name;
pub mod nvim;
pub(crate) mod placeholder;
pub mod providers;
pub(crate) mod rex;
pub mod screen;
pub(crate) mod term;
pub(crate) mod util;
