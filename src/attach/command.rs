//! The attach command a requested pane's terminal runs, as words and as the
//! two environment variables pane provider commands get.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use super::{PaneId, Transport};
use crate::name;

/// `{attach}`: `[exe, "attach", "--address", A, "--pane", ID]`, with
/// `--transport relay` added under the relay diagnostic.
pub fn attach_command(
    exe: &OsStr,
    address: &OsStr,
    pane: &PaneId,
    transport: Transport,
) -> Vec<OsString> {
    let mut words: Vec<OsString> = vec![
        exe.into(),
        "attach".into(),
        "--address".into(),
        address.into(),
        "--pane".into(),
        pane.to_string().into(),
    ];
    if transport == Transport::Relay {
        words.extend(["--transport".into(), "relay".into()]);
    }
    words
}

/// Bytes a word may hold and still go unquoted.
fn bare(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b)
}

/// `word` as a POSIX shell reads it back: bare when every byte is in
/// `[A-Za-z0-9_@%+=:,./-]`, else in single quotes, each `'` as `'\''`.
pub fn posix_quote(word: &OsStr) -> OsString {
    let b = word.as_bytes();
    if !b.is_empty() && b.iter().all(|&c| bare(c)) {
        return word.into();
    }
    let mut out = Vec::with_capacity(b.len() + 2);
    out.push(b'\'');
    for &c in b {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
    OsString::from_vec(out)
}

/// The words, each quoted as needed, joined by spaces: `<NAME>_ATTACH`.
pub fn attach_line(words: &[OsString]) -> OsString {
    let mut out = OsString::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(" ");
        }
        out.push(posix_quote(w));
    }
    out
}

/// The words as a compact JSON array: `<NAME>_ATTACH_JSON`. None if a word
/// isn't UTF-8.
pub fn attach_json(words: &[OsString]) -> Option<String> {
    let strs: Vec<&str> = words.iter().map(|w| w.to_str()).collect::<Option<_>>()?;
    Some(serde_json::to_string(&strs).expect("strings serialize"))
}

/// The variables a pane provider command gets for `words`: `<NAME>_ATTACH`
/// always, `<NAME>_ATTACH_JSON` when every word is UTF-8.
pub fn attach_vars(words: &[OsString]) -> Vec<(String, OsString)> {
    let upper = name::upper();
    let mut vars = vec![(format!("{upper}_ATTACH"), attach_line(words))];
    if let Some(json) = attach_json(words) {
        vars.push((format!("{upper}_ATTACH_JSON"), json.into()));
    }
    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &[u8]) -> Vec<u8> {
        posix_quote(OsStr::from_bytes(s)).into_vec()
    }

    #[test]
    fn quoting() {
        assert_eq!(b"plain".to_vec(), q(b"plain"));
        assert_eq!(
            b"/tmp/a_b@c%d+e=f:g,h.i-j".to_vec(),
            q(b"/tmp/a_b@c%d+e=f:g,h.i-j")
        );
        assert_eq!(b"'a b'".to_vec(), q(b"a b"));
        assert_eq!(b"'it'\\''s'".to_vec(), q(b"it's"));
        assert_eq!(b"''".to_vec(), q(b""));
        assert_eq!(b"'$HOME'".to_vec(), q(b"$HOME"));
        assert_eq!("'café'".as_bytes().to_vec(), q("café".as_bytes()));
        assert_eq!(b"'\xff'".to_vec(), q(b"\xff"));
        assert_eq!(b"'a*'".to_vec(), q(b"a*"));
    }

    /// Each quoted word reads back as itself in a real shell.
    #[test]
    fn quoted_words_survive_sh() {
        let words: Vec<OsString> = [&b"a b"[..], b"it's", b"", b"$HOME", b"\xc3\xa9", b"x"]
            .iter()
            .map(|w| OsString::from_vec(w.to_vec()))
            .collect();
        let script = format!(
            "for w in {}; do printf '%s\\0' \"$w\"; done",
            attach_line(&words).to_str().unwrap()
        );
        let out = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        let mut expected = Vec::new();
        for w in &words {
            expected.extend_from_slice(w.as_bytes());
            expected.push(0);
        }
        assert_eq!(expected, out.stdout);
    }

    fn id() -> PaneId {
        PaneId::parse("12-k3j9x2qpa8").unwrap()
    }

    #[test]
    fn the_command() {
        let words = attach_command(
            OsStr::new("/opt/my bin/x"),
            OsStr::new("/tmp/x-501/work/sock"),
            &id(),
            Transport::Fd,
        );
        assert_eq!(
            [
                "/opt/my bin/x",
                "attach",
                "--address",
                "/tmp/x-501/work/sock",
                "--pane",
                "12-k3j9x2qpa8"
            ]
            .map(OsString::from)
            .to_vec(),
            words
        );
        assert_eq!(
            "'/opt/my bin/x' attach --address /tmp/x-501/work/sock --pane 12-k3j9x2qpa8",
            attach_line(&words)
        );
        assert_eq!(
            r#"["/opt/my bin/x","attach","--address","/tmp/x-501/work/sock","--pane","12-k3j9x2qpa8"]"#,
            attach_json(&words).unwrap()
        );
        let relay = attach_command(OsStr::new("x"), OsStr::new("a"), &id(), Transport::Relay);
        assert_eq!(
            "x attach --address a --pane 12-k3j9x2qpa8 --transport relay",
            attach_line(&relay)
        );
    }

    #[test]
    fn the_variables() {
        let words = attach_command(OsStr::new("é\"x"), OsStr::new("a"), &id(), Transport::Fd);
        let vars = attach_vars(&words);
        let upper = name::upper();
        assert_eq!(format!("{upper}_ATTACH"), vars[0].0);
        assert_eq!(format!("{upper}_ATTACH_JSON"), vars[1].0);
        assert_eq!(
            r#"["é\"x","attach","--address","a","--pane","12-k3j9x2qpa8"]"#,
            vars[1].1
        );

        let words = attach_command(
            OsStr::from_bytes(b"/bin/\xff"),
            OsStr::new("a"),
            &id(),
            Transport::Fd,
        );
        let vars = attach_vars(&words);
        assert_eq!(1, vars.len(), "no JSON when a word isn't UTF-8");
        assert_eq!(
            b"'/bin/\xff' attach --address a --pane 12-k3j9x2qpa8".to_vec(),
            vars[0].1.as_bytes()
        );
    }
}
