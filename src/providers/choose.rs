//! Choosing a pane provider for a self-attach (ADR 0007, #92 decisions 24
//! and 25). With no config, the built-in `<name>.default` chooses, run in
//! the daemon itself: Rex when the attach's environment has `REX_SERVER`
//! and `REX_SESSION`, else none. The hint `none` skips choosing.
//!
//! Choosing Rex only starts connecting to it, which never blocks: the
//! daemon's loop takes the steps, and its `find` confirms the choice.

use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::Held;
use super::rex::RexProvider;
use crate::attach::env_value;
use crate::config::{self, Builtin, Choice};
use crate::log::log_warn;
use crate::name::NAME;

/// An attach's environment: names and values, raw.
pub(crate) type Env = [(Vec<u8>, Vec<u8>)];

/// The hint that skips choosing: `--provider none`, `$<NAME>_PROVIDER=none`.
pub(crate) const HINT_NONE: &str = "none";

/// Opens a connection to the Rex server at a `unix://` URL. Tests fake it.
pub(crate) type Connect = Box<dyn FnMut(&str) -> std::io::Result<UnixStream>>;

/// The `Connect` the daemon runs with.
pub(crate) fn connect() -> Connect {
    Box::new(|url| {
        crate::rex::client::connect_to(url).map_err(|e| match e {
            crate::rex::client::ClientError::Io(e) => e,
            e => std::io::Error::other(e.to_string()),
        })
    })
}

/// What came of choosing.
pub(crate) enum Chosen {
    /// A pane provider, which its `find` still has to confirm, and the time
    /// choosing it took, connecting: the choice's 3 s count it.
    Provider { held: Held, spent: Duration },
    /// None, and why; shown in Neovim as a warning if `warn`.
    None { why: String, warn: bool },
}

/// What the in-process `<name>.default` chooses for an attach whose
/// environment is `env`: the config's own `<name>.default`, so the two
/// never differ.
pub(crate) fn default_choice(env: &Env) -> Choice {
    config::default(&config::Attach {
        env: env.to_vec(),
        ..Default::default()
    })
}

/// Chooses for a self-attach with environment `env` and `hint`, connecting
/// to Rex through `connect` if that's the choice.
pub(crate) fn choose(env: &Env, hint: Option<&str>, connect: &mut Connect) -> Chosen {
    let started = Instant::now();
    if hint == Some(HINT_NONE) {
        return Chosen::None {
            why: format!("the hint is {HINT_NONE}"),
            warn: false,
        };
    }
    match default_choice(env) {
        Choice::Builtin(Builtin::Rex) => start_rex(env, connect, started),
        _ => Chosen::None {
            why: "REX_SERVER and REX_SESSION aren't both set".into(),
            warn: false,
        },
    }
}

/// Starts connecting to the Rex server and session `env` names. One that
/// can't be reached, as when the variables are stale, means none.
fn start_rex(env: &Env, connect: &mut Connect, since: Instant) -> Chosen {
    let var =
        |name: &str| String::from_utf8_lossy(env_value(env, name).unwrap_or_default()).into_owned();
    let (server, session) = (var("REX_SERVER"), var("REX_SESSION"));
    let started = connect(&server).and_then(|stream| {
        RexProvider::connecting(stream, &session).map_err(|e| std::io::Error::other(e.to_string()))
    });
    match started {
        Ok(rex) => Chosen::Provider {
            held: Held::Rex(Box::new(rex)),
            spent: since.elapsed(),
        },
        Err(e) => {
            // where, for the log: a warning that long would wait at a prompt
            log_warn!("{NAME} daemon: rex: can't reach Rex at {server}: {e}");
            Chosen::None {
                why: format!("rex: can't reach Rex: {e}"),
                warn: true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> Vec<(Vec<u8>, Vec<u8>)> {
        vars.iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn the_default_chooses_rex_only_with_rex_server_and_rex_session_set() {
        let rex = Choice::Builtin(Builtin::Rex);
        let both = [("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")];
        assert_eq!(rex, default_choice(&env(&both)));
        // set, even if empty
        assert_eq!(
            rex,
            default_choice(&env(&[("REX_SERVER", ""), ("REX_SESSION", "")]))
        );
        assert_eq!(Choice::None, default_choice(&env(&both[..1])));
        assert_eq!(Choice::None, default_choice(&env(&both[1..])));
        assert_eq!(
            Choice::None,
            default_choice(&env(&[("TMUX", "/tmp/t,1,0")]))
        );
    }

    fn connects_nowhere() -> Connect {
        Box::new(|url| panic!("connected to {url}"))
    }

    fn none(c: Chosen) -> (String, bool) {
        match c {
            Chosen::None { why, warn } => (why, warn),
            Chosen::Provider { .. } => panic!("chose a pane provider"),
        }
    }

    #[test]
    fn the_hint_none_or_no_rex_chooses_none_quietly() {
        let in_rex = env(&[("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")]);
        let (_, warn) = none(choose(&in_rex, Some("none"), &mut connects_nowhere()));
        assert!(!warn);
        let (_, warn) = none(choose(&env(&[]), None, &mut connects_nowhere()));
        assert!(!warn);
    }

    #[test]
    fn a_rex_that_cant_be_reached_chooses_none_with_a_warning() {
        // stale REX_*: a Ghostty window started from a Rex shell, Rex since quit
        let stale = env(&[("REX_SERVER", "unix:///gone"), ("REX_SESSION", "session:s")]);
        let mut refused: Connect = Box::new(|_| Err(std::io::ErrorKind::ConnectionRefused.into()));
        let (why, warn) = none(choose(&stale, None, &mut refused));
        assert!(warn);
        assert!(why.starts_with("rex: can't reach Rex: "), "{why}");
    }

    #[test]
    fn rex_is_chosen_connecting() {
        let in_rex = env(&[("REX_SERVER", "unix:///s"), ("REX_SESSION", "session:s")]);
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let mut ours = Some(ours);
        let mut connect: Connect = Box::new(move |url| {
            assert_eq!("unix:///s", url);
            Ok(ours.take().unwrap())
        });
        let Chosen::Provider { held, spent } = choose(&in_rex, Some("tmux"), &mut connect) else {
            panic!("chose none");
        };
        assert_eq!("rex", held.seam_ref().name());
        assert!(spent < Duration::from_secs(3));
    }
}
