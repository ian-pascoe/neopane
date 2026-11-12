//! The config: the user's `init.lua`, run in a child process of the daemon's
//! own binary, and what it returned, checked by the daemon (ADR 0007, #93).
//!
//! [`run`] starts the child and waits for its answer; [`check`] reads the
//! answer with the daemon's own rules; [`choose`] does both. The child's
//! side is [`child::main`], behind the hidden `config-child` command.

mod api;
pub mod check;
pub mod child;
mod runner;
pub mod tree;
pub mod wire;

use std::time::Duration;

pub use check::{Builtin, Capability, Choice, Chosen, Pair, Problem, Problems, Provider, check};
pub use runner::{Failure, MAX_PRINTED, Outcome, Spawn, run};
pub use wire::{Answer, Attach, Request};

/// The time a config has to answer.
pub const TIME: Duration = Duration::from_secs(3);

/// What running the config and checking its answer came to, for the file
/// named `file`, such as `init.lua`.
pub fn choose(outcome: &Outcome, file: &str) -> Result<Chosen, Problems> {
    match &outcome.answer {
        Ok(answer) => check(answer, file),
        Err(failure) => Err(check::failed(failure.message(file))),
    }
}

/// The built-in default (#93 decision 27), on an environment: `"rex"` when
/// `REX_SERVER` and `REX_SESSION` are both set, even if empty, else none.
/// The daemon's own default and the config's `<name>.default` both use it.
pub fn default_name(var: impl Fn(&str) -> Option<Vec<u8>>) -> Option<&'static str> {
    (var("REX_SERVER").is_some() && var("REX_SESSION").is_some()).then_some(Builtin::Rex.name())
}

/// The built-in default's choice for `attach`, as the daemon makes it with
/// no config file.
pub fn default(attach: &Attach) -> Choice {
    let var = |name: &str| {
        attach
            .env
            .iter()
            .find(|(k, _)| k == name.as_bytes())
            .map(|(_, v)| v.clone())
    };
    match default_name(var) {
        Some(_) => Choice::Builtin(Builtin::Rex),
        None => Choice::None,
    }
}
