//! neorex: a Rex-native Neovim frontend.

#![warn(unreachable_pub)]

pub mod daemon;
pub mod debug;
pub mod name;
pub mod nvim;
pub(crate) mod rex;
pub mod screen;
pub(crate) mod term;
