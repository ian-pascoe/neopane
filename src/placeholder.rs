//! A capability argv's placeholder syntax (#93 decision 13), which the
//! config's checker and the command runner share: a whole word `{name}` is
//! a placeholder, and `{{name}}` passes as `{name}`.

/// The name in a word that is exactly `{name}`.
pub(crate) fn whole_placeholder(word: &[u8]) -> Option<&str> {
    let inner = word.strip_prefix(b"{")?.strip_suffix(b"}")?;
    is_name(inner).then(|| std::str::from_utf8(inner).unwrap())
}

/// A word that is exactly `{{name}}`, which passes as `{name}`.
pub(crate) fn is_escaped(word: &[u8]) -> bool {
    word.strip_prefix(b"{{")
        .and_then(|w| w.strip_suffix(b"}}"))
        .is_some_and(is_name)
}

/// `[a-z_][a-z0-9_]*`.
fn is_name(s: &[u8]) -> bool {
    match s.split_first() {
        Some((first, rest)) => {
            (first.is_ascii_lowercase() || *first == b'_')
                && rest
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        }
        None => false,
    }
}
