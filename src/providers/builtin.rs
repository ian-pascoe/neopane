//! The hidden `provider builtin NAME CAPABILITY` command (ADR 0016): what
//! a built-in table's capabilities run, as `{exe} provider builtin NAME
//! CAPABILITY`. It speaks the capability contract (`docs/capabilities.md`)
//! for built-in NAME: its only inputs are the documented `<NAME>_*`
//! variables and the environment of the choosing attach, and its outputs
//! the capability's stdout and exit status. Its words, inputs and outputs
//! are frozen, and it only ever gains forms, since a daemon upgraded in
//! place runs a newer binary at `{exe}`; the commands it runs and its
//! stderr are not.
//!
//! `main` dispatches it before anything else starts. A name, capability or
//! word it doesn't know exits 69, unsupported, so an older binary refuses
//! a newer table's capability as the contract says. Without
//! `<NAME>_PROTOCOL`, which every capability gets, it was run by hand: it
//! exits 2, saying a pane provider table runs it.

use std::ffi::OsString;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use super::{herdr, tmux, zellij};
use crate::config::{Attach, BuiltinTable, Capability, Proof};
use crate::name::{NAME, upper};

/// Exit statuses, as `docs/capabilities.md` gives them.
pub(crate) const DONE: u8 = 0;
pub(crate) const FAILED: u8 = 1;
pub(crate) const USAGE: u8 = 2;
pub(crate) const UNSUPPORTED: u8 = 69;

/// Runs `provider builtin` with `words`, the words after it, in this
/// process's environment: its exit status.
pub fn main(words: &[OsString]) -> u8 {
    let given = Given::from(|name: &str| std::env::var_os(name).map(|v| v.as_bytes().to_vec()));
    run(words, &given)
}

/// `provider builtin` with `words` and `given`: its exit status.
fn run(words: &[OsString], given: &Given) -> u8 {
    let [name, cap] = words else {
        let shown: Vec<_> = words.iter().map(|w| w.to_string_lossy()).collect();
        return unsupported(&format!(
            "takes a built-in and a capability, not {:?}",
            shown.join(" ")
        ));
    };
    let Some(table) = BuiltinTable::from_name(name.as_bytes()) else {
        return unsupported(&format!("no built-in {}", name.to_string_lossy()));
    };
    let Some(cap) =
        Capability::from_name(cap.as_bytes()).filter(|c| table.capabilities().contains(c))
    else {
        return unsupported(&format!(
            "{} has no capability {}",
            table.name(),
            cap.to_string_lossy()
        ));
    };
    if given.var("protocol").is_none() {
        eprintln!(
            "{NAME} provider builtin: a pane provider table runs this, with its {}_* \
             variables, not a person; see {NAME} provider show {}",
            upper(),
            table.name()
        );
        return USAGE;
    }
    match table {
        BuiltinTable::Tmux => tmux::run(cap, given),
        BuiltinTable::Herdr => herdr::run(cap, given),
        BuiltinTable::Zellij => zellij::run(cap, given),
    }
}

/// Built-in `table`'s `inside` proof for `attach`: whether its terminal is
/// directly one of that multiplexer's panes, and why, within
/// [`INSIDE_TIME`](crate::config::INSIDE_TIME). It runs commands with the
/// environment `find` would get, and never fails.
pub(crate) fn inside(table: BuiltinTable, attach: &Attach) -> Proof {
    match table {
        BuiltinTable::Tmux => tmux::inside(attach),
        BuiltinTable::Herdr => herdr::inside(attach),
        BuiltinTable::Zellij => zellij::inside(attach),
    }
}

/// Says why on stderr; exits 69.
fn unsupported(why: &str) -> u8 {
    eprintln!("{NAME} provider builtin: {why}");
    UNSUPPORTED
}

/// What a built-in command is given: its environment, the choosing
/// attach's with the act's `<NAME>_*` variables.
pub(crate) struct Given {
    env: Box<Lookup>,
}

/// A variable's value by its name, raw; None if unset.
type Lookup = dyn Fn(&str) -> Option<Vec<u8>>;

impl Given {
    pub(crate) fn from(env: impl Fn(&str) -> Option<Vec<u8>> + 'static) -> Given {
        Given { env: Box::new(env) }
    }

    /// The input placeholder `name` names, as its variable `<NAME>_NAME`
    /// has it; None if unset.
    pub(crate) fn var(&self, name: &str) -> Option<Vec<u8>> {
        (self.env)(&format!("{}_{}", upper(), name.to_ascii_uppercase()))
    }

    /// Variable `name` of the environment, such as `TMUX`.
    pub(crate) fn env(&self, name: &str) -> Option<Vec<u8>> {
        (self.env)(name)
    }

    /// The attach command's words, from `ATTACH`, or why there are none.
    pub(crate) fn attach_words(&self) -> Result<Vec<OsString>, String> {
        let attach = self.var("attach").unwrap_or_default();
        let words = unquoted(&attach).ok_or("ATTACH isn't as the daemon writes it")?;
        if words.is_empty() {
            return Err("ATTACH is empty".into());
        }
        Ok(words.into_iter().map(OsString::from_vec).collect())
    }
}

/// The words of `line`, as `<NAME>_ATTACH` has them: each bare, or in
/// single quotes with `'\''` for a `'`, separated by one space. None if it
/// isn't that.
fn unquoted(line: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut words = Vec::new();
    let mut word = Vec::new();
    let mut started = false;
    let mut rest = line;
    while let Some((&b, after)) = rest.split_first() {
        rest = after;
        match b {
            b' ' => {
                if !started {
                    return None;
                }
                words.push(std::mem::take(&mut word));
                started = false;
            }
            b'\'' => {
                started = true;
                let end = rest.iter().position(|&c| c == b'\'')?;
                word.extend_from_slice(&rest[..end]);
                rest = &rest[end + 1..];
                if let Some(after) = rest.strip_prefix(b"\\'") {
                    word.push(b'\'');
                    rest = after;
                }
            }
            b => {
                started = true;
                word.push(b);
            }
        }
    }
    if started {
        words.push(word);
    }
    Some(words)
}

/// Prints `line` and a newline on stdout: a terminal id. A reader gone
/// fails the act.
pub(crate) fn print_line(line: &[u8]) -> u8 {
    let mut out = std::io::stdout().lock();
    let wrote = out
        .write_all(line)
        .and_then(|()| out.write_all(b"\n"))
        .and_then(|()| out.flush());
    match wrote {
        Ok(()) => DONE,
        Err(_) => FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::attach_line;

    #[test]
    fn the_attach_line_reads_back_as_its_words() {
        let words: Vec<OsString> = [&b"/a b/x"[..], b"attach", b"it's", b"", b"\xff", b"x'"]
            .iter()
            .map(|w| OsString::from_vec(w.to_vec()))
            .collect();
        let line = attach_line(&words).into_vec();
        let back: Vec<Vec<u8>> = words.into_iter().map(OsString::into_vec).collect();
        assert_eq!(unquoted(&line), Some(back));
        assert_eq!(unquoted(b"a  b"), None);
        assert_eq!(unquoted(b"'open"), None);
    }
}
