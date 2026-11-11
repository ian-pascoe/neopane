//! The attach protocol (ADR 0013): the frames a helper and a daemon speak,
//! their payloads, pane ids and the attach command. `docs/attach-protocol.md`
//! is its reference. Nothing here touches a socket.

mod command;
pub mod frame;
mod message;
mod pane_id;
mod value;

pub use command::{attach_command, attach_json, attach_line, attach_vars, posix_quote};
pub use frame::Frame;
pub use message::{
    Accept, End, Ending, Hello, Message, Purpose, Refusal, Reply, Size, Status, Terminal,
    Transport, UNDO_LIMIT,
};
pub use pane_id::{PaneId, PaneIdRegistry};

use crate::name::NAME;

/// The attach protocol version. A helper and a daemon must match exactly,
/// except for status and stop. It changes only when an existing message or
/// field changes meaning.
pub const ATTACH_PROTOCOL_VERSION: i64 = 1;

/// This build, as the hello's `build` and `--version`'s first line:
/// `<name> <Cargo version>`, plus ` (<short commit>[-dirty], <commit date>)`
/// when built from git.
pub fn build() -> &'static str {
    static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        build_string(
            NAME,
            env!("CARGO_PKG_VERSION"),
            option_env!("BUILD_GIT_COMMIT"),
            option_env!("BUILD_GIT_DATE"),
        )
    })
}

fn build_string(name: &str, version: &str, commit: Option<&str>, date: Option<&str>) -> String {
    match (commit, date) {
        (Some(c), Some(d)) => format!("{name} {version} ({c}, {d})"),
        _ => format!("{name} {version}"),
    }
}

/// A frame or payload that breaks the protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A frame kind that isn't an ASCII letter.
    Kind(u8),
    /// A payload over its kind's limit.
    TooLong { kind: u8, len: u64 },
    /// A connection's first frame isn't a hello.
    NotHello(u8),
    /// A payload that isn't a msgpack map with `str` keys.
    Malformed(&'static str),
    /// A required field is missing.
    Missing(&'static str),
    /// A field has the wrong type or value.
    Invalid(&'static str),
    /// A hello of another attach protocol version.
    Version { v: i64, build: String },
}

impl Error {
    /// What the daemon refuses a hello that fails so with.
    pub fn refusal(&self) -> Refusal {
        match self {
            Error::Version { .. } => Refusal::Version,
            _ => Refusal::Hello,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = |k: u8| char::from(k).escape_default().to_string();
        match self {
            Error::Kind(k) => write!(f, "frame kind {} isn't a letter", kind(*k)),
            Error::TooLong { kind: k, len } => write!(
                f,
                "a {} frame of {len} bytes is over the {} allowed",
                kind(*k),
                frame::limit(*k)
            ),
            Error::NotHello(k) => write!(f, "the first frame is {}, not a hello", kind(*k)),
            Error::Malformed(why) => f.write_str(why),
            Error::Missing(field) => write!(f, "missing field {field}"),
            Error::Invalid(field) => write!(f, "invalid field {field}"),
            Error::Version { v, build } => write!(
                f,
                "{build} speaks attach protocol {v}; {} speaks {ATTACH_PROTOCOL_VERSION}",
                self::build()
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_format() {
        assert_eq!("x 1.2.3", build_string("x", "1.2.3", None, None));
        assert_eq!(
            "x 1.2.3 (a1b2c3d, 2026-11-09)",
            build_string("x", "1.2.3", Some("a1b2c3d"), Some("2026-11-09"))
        );
        assert_eq!(
            "x 1.2.3 (a1b2c3d-dirty, 2026-11-09)",
            build_string("x", "1.2.3", Some("a1b2c3d-dirty"), Some("2026-11-09"))
        );
    }

    /// This build's string, with build.rs's commit and date when built from
    /// git.
    #[test]
    fn this_build() {
        let b = build();
        if option_env!("BUILD_GIT_COMMIT").is_none() {
            assert_eq!(format!("{NAME} {}", env!("CARGO_PKG_VERSION")), b);
            return;
        }
        let rest = b
            .strip_prefix(&format!("{NAME} {} (", env!("CARGO_PKG_VERSION")))
            .unwrap_or_else(|| panic!("{b}"));
        let (commit, date) = rest
            .strip_suffix(')')
            .and_then(|r| r.split_once(", "))
            .unwrap_or_else(|| panic!("{b}"));
        let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
        assert!(
            hash.len() >= 7 && hash.bytes().all(|c| c.is_ascii_hexdigit()),
            "{b}"
        );
        let d = date.as_bytes();
        assert!(
            d.len() == 10
                && d[4] == b'-'
                && d[7] == b'-'
                && date.replace('-', "").bytes().all(|c| c.is_ascii_digit()),
            "{b}"
        );
    }

    #[test]
    fn errors_name_their_field_and_refusal() {
        assert_eq!("missing field pane", Error::Missing("pane").to_string());
        assert_eq!(Refusal::Hello, Error::Missing("pane").refusal());
        let v = Error::Version {
            v: 2,
            build: "x 9.0.0".into(),
        };
        assert_eq!(Refusal::Version, v.refusal());
        assert!(v.to_string().contains("x 9.0.0 speaks attach protocol 2"));
    }
}
