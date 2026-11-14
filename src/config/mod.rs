//! The config: the user's `init.lua`, run in a child process of the daemon's
//! own binary, and what it returned, checked by the daemon (ADR 0007, #93).
//!
//! [`run`] starts the child and waits for its answer; [`check`] reads the
//! answer with the daemon's own rules; [`choose`] does both. The child's
//! side is [`child::main`], behind the hidden `config-child` command.

mod api;
pub mod builtin;
pub mod check;
pub mod child;
mod runner;
pub mod show;
pub mod tree;
pub mod types;
pub mod wire;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use builtin::BuiltinTable;
pub use check::{Builtin, Capability, Choice, Chosen, Pair, Problem, Problems, Provider, check};
pub use runner::{Failure, MAX_PRINTED, Outcome, Spawn, run};
pub use wire::{Answer, Attach, Request};

use crate::name::{NAME, upper};

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

/// What the built-in default names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultName {
    /// Rex's, by its name.
    Rex,
    /// A built-in table.
    Table(BuiltinTable),
}

impl DefaultName {
    /// Its name, as `provider()` would return it.
    pub fn name(self) -> &'static str {
        match self {
            DefaultName::Rex => Builtin::Rex.name(),
            DefaultName::Table(table) => table.name(),
        }
    }

    /// What choosing it is.
    pub fn choice(name: Option<DefaultName>) -> Choice {
        match name {
            Some(DefaultName::Rex) => Choice::Builtin(Builtin::Rex),
            Some(DefaultName::Table(table)) => Choice::Whole(table.provider()),
            None => Choice::None,
        }
    }
}

/// What a multiplexer's `inside` proof said of an attach: whether its
/// terminal is directly one of the multiplexer's panes, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proof {
    pub ok: bool,
    pub why: String,
}

/// What the built-in default came to: the built-ins the environment
/// names, in order, the `inside` proofs it ran, and what it chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaulted {
    pub named: Vec<DefaultName>,
    pub proofs: Vec<(BuiltinTable, Proof)>,
    pub chose: Option<DefaultName>,
}

impl Defaulted {
    /// What it chose.
    pub fn choice(&self) -> Choice {
        DefaultName::choice(self.chose)
    }
}

/// The built-ins an environment names, from its variables only, the
/// multiplexers in proving order, then Rex: tmux when `TERM_PROGRAM` is
/// `tmux` and `TMUX` is set and not empty, zellij when `ZELLIJ` is set and
/// `ZELLIJ_PANE_ID` set and not empty, Rex when `REX_SERVER` and
/// `REX_SESSION` are both set, even if empty. A multiplexer's panes
/// inherit whatever started its server, so several may be named.
pub fn named(var: impl Fn(&str) -> Option<Vec<u8>>) -> Vec<DefaultName> {
    let mut named = Vec::new();
    if var("TERM_PROGRAM").as_deref() == Some(b"tmux") && var("TMUX").is_some_and(|t| !t.is_empty())
    {
        named.push(DefaultName::Table(BuiltinTable::Tmux));
    }
    if var("ZELLIJ").is_some() && var("ZELLIJ_PANE_ID").is_some_and(|p| !p.is_empty()) {
        named.push(DefaultName::Table(BuiltinTable::Zellij));
    }
    if var("REX_SERVER").is_some() && var("REX_SESSION").is_some() {
        named.push(DefaultName::Rex);
    }
    named
}

/// Whether the default, on environment `var`, runs any `inside` proof:
/// only when it names several built-ins.
pub fn proves(var: impl Fn(&str) -> Option<Vec<u8>>) -> bool {
    named(var).len() > 1
}

/// The built-in default (#93 decision 27, as SPI-1126 and SPI-1135 amend
/// it), on an environment `var`: the one built-in it names, running
/// nothing; when it names several, the first multiplexer whose `inside`
/// proof, asked through `prove` in order, proves, else Rex if named, else
/// none. Rex is never proven here: its `find` proves it. The daemon's own
/// default and the config's `<name>.default` both use it.
pub fn default_of(
    var: impl Fn(&str) -> Option<Vec<u8>>,
    mut prove: impl FnMut(BuiltinTable) -> Proof,
) -> Defaulted {
    let named = named(var);
    let mut proofs = Vec::new();
    let chose = match named.as_slice() {
        [] => None,
        [one] => Some(*one),
        several => {
            let mut proven = None;
            for name in several {
                let DefaultName::Table(table) = *name else {
                    continue;
                };
                let proof = prove(table);
                let ok = proof.ok;
                proofs.push((table, proof));
                if ok {
                    proven = Some(*name);
                    break;
                }
            }
            proven.or_else(|| {
                several
                    .contains(&DefaultName::Rex)
                    .then_some(DefaultName::Rex)
            })
        }
    };
    Defaulted {
        named,
        proofs,
        chose,
    }
}

/// `attach`'s environment, as a lookup.
pub fn env_of(attach: &Attach) -> impl Fn(&str) -> Option<Vec<u8>> + '_ {
    |name: &str| {
        attach
            .env
            .iter()
            .find(|(k, _)| k == name.as_bytes())
            .map(|(_, v)| v.clone())
    }
}

/// The built-in default for `attach`, its proofs the real ones, each cut
/// at [`INSIDE_TIME`]: as the config's `<name>.default` and the daemon's
/// default with no config file run it.
pub fn default(attach: &Attach) -> Defaulted {
    default_of(env_of(attach), |table| {
        crate::providers::builtin::inside(table, attach)
    })
}

/// The time an `inside` proof has to answer.
pub const INSIDE_TIME: Duration = Duration::from_millis(500);

/// The config file's name, unless `$<NAME>_CONFIG` names another.
pub const FILE_NAME: &str = "init.lua";

/// `$<NAME>_CONFIG`'s name.
pub fn config_var() -> String {
    format!("{}_CONFIG", upper())
}

/// The config file, as the daemon finds it (#94 decisions 33 and 39):
/// `$<NAME>_CONFIG`, made absolute against `cwd`, else
/// `$XDG_CONFIG_HOME/<name>/init.lua`, else `~/.config/<name>/init.lua`.
/// Empty variables count as unset, and a relative `XDG_CONFIG_HOME` is
/// ignored. None without `HOME`.
pub fn default_file(var: impl Fn(&str) -> Option<OsString>, cwd: &Path) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    if let Some(file) = set(&config_var()) {
        return Some(cwd.join(file));
    }
    let base = match set("XDG_CONFIG_HOME").map(PathBuf::from) {
        Some(xdg) if xdg.is_absolute() => xdg,
        _ => PathBuf::from(set("HOME")?).join(".config"),
    };
    Some(base.join(NAME).join(FILE_NAME))
}

#[cfg(test)]
mod default_tests {
    use super::*;

    const TMUX: DefaultName = DefaultName::Table(BuiltinTable::Tmux);
    const ZELLIJ: DefaultName = DefaultName::Table(BuiltinTable::Zellij);

    fn var<'a>(vars: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<Vec<u8>> + 'a {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.as_bytes().to_vec())
        }
    }

    const IN_TMUX: [(&str, &str); 2] = [("TERM_PROGRAM", "tmux"), ("TMUX", "/tmp/t,1,0")];
    const IN_REX: [(&str, &str); 2] = [("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")];
    const IN_ZELLIJ: [(&str, &str); 2] = [("ZELLIJ", "0"), ("ZELLIJ_PANE_ID", "3")];

    fn proof(ok: bool, why: &str) -> Proof {
        Proof {
            ok,
            why: why.into(),
        }
    }

    fn never(table: BuiltinTable) -> Proof {
        panic!("proved {table:?}")
    }

    #[test]
    fn the_environment_names_tmux_then_rex() {
        assert_eq!(named(var(&IN_TMUX)), [TMUX]);
        assert_eq!(named(var(&IN_REX)), [DefaultName::Rex]);
        assert_eq!(
            named(var(&[IN_REX, IN_TMUX].concat())),
            [TMUX, DefaultName::Rex]
        );
        // set, even if empty
        assert_eq!(
            named(var(&[("REX_SERVER", ""), ("REX_SESSION", "")])),
            [DefaultName::Rex]
        );
        assert_eq!(named(var(&IN_REX[..1])), []);
        assert_eq!(named(var(&IN_REX[1..])), []);
        // TMUX alone, or TERM_PROGRAM another terminal's, names no tmux
        assert_eq!(named(var(&IN_TMUX[1..])), []);
        assert_eq!(named(var(&IN_TMUX[..1])), []);
        assert_eq!(
            named(var(&[("TERM_PROGRAM", "ghostty"), ("TMUX", "/tmp/t,1,0")])),
            []
        );
        assert_eq!(named(var(&[("TERM_PROGRAM", "tmux"), ("TMUX", "")])), []);
    }

    #[test]
    fn the_environment_names_zellij_after_tmux_and_before_rex() {
        assert_eq!(named(var(&IN_ZELLIJ)), [ZELLIJ]);
        assert_eq!(
            named(var(&[IN_REX, IN_ZELLIJ, IN_TMUX].concat())),
            [TMUX, ZELLIJ, DefaultName::Rex]
        );
        // ZELLIJ alone, or an empty pane id, names no zellij
        assert_eq!(named(var(&IN_ZELLIJ[..1])), []);
        assert_eq!(named(var(&IN_ZELLIJ[1..])), []);
        assert_eq!(named(var(&[("ZELLIJ", "0"), ("ZELLIJ_PANE_ID", "")])), []);
    }

    #[test]
    fn zellij_named_alone_is_chosen_and_after_tmux_is_proved_last() {
        let d = default_of(var(&IN_ZELLIJ), never);
        assert_eq!(d.chose, Some(ZELLIJ));
        // zellij started from a tmux shell: tmux's variables leak into it
        let both = [IN_TMUX, IN_ZELLIJ].concat();
        let mut asked = Vec::new();
        let d = default_of(var(&both), |t| {
            asked.push(t);
            proof(t == BuiltinTable::Zellij, "")
        });
        assert_eq!(asked, [BuiltinTable::Tmux, BuiltinTable::Zellij]);
        assert_eq!(d.chose, Some(ZELLIJ));
        // neither proving, Rex if named, else none
        let d = default_of(var(&[IN_ZELLIJ, IN_REX].concat()), |_| proof(false, ""));
        assert_eq!(d.chose, Some(DefaultName::Rex));
        let d = default_of(var(&both), |_| proof(false, ""));
        assert_eq!(d.chose, None);
    }

    #[test]
    fn one_built_in_named_is_chosen_proving_nothing() {
        for (vars, want) in [(&IN_TMUX, TMUX), (&IN_REX, DefaultName::Rex)] {
            let d = default_of(var(vars), never);
            assert_eq!(d.chose, Some(want));
            assert!(d.proofs.is_empty());
        }
        let d = default_of(var(&[]), never);
        assert_eq!((d.chose, d.named), (None, vec![]));
        assert!(!proves(var(&IN_TMUX)));
        assert!(proves(var(&[IN_TMUX, IN_REX].concat())));
    }

    #[test]
    fn several_named_take_the_first_multiplexer_that_proves() {
        let both = [IN_TMUX, IN_REX].concat();
        let d = default_of(var(&both), |t| {
            assert_eq!(t, BuiltinTable::Tmux);
            proof(true, "tmux pane %4")
        });
        assert_eq!(d.chose, Some(TMUX));
        assert_eq!(
            d.proofs,
            [(BuiltinTable::Tmux, proof(true, "tmux pane %4"))]
        );
    }

    #[test]
    fn several_named_none_proving_fall_to_rex_unproven() {
        // TMUX and TERM_PROGRAM=tmux leaked into a terminal tmux doesn't run
        let both = [IN_TMUX, IN_REX].concat();
        let why = "this terminal (/dev/ttys003) isn't a pane of the tmux server at /tmp/t";
        let d = default_of(var(&both), |_| proof(false, why));
        assert_eq!(d.chose, Some(DefaultName::Rex));
        assert_eq!(d.named, [TMUX, DefaultName::Rex]);
        assert_eq!(d.proofs, [(BuiltinTable::Tmux, proof(false, why))]);
        assert_eq!(d.choice(), Choice::Builtin(Builtin::Rex));
    }
}
