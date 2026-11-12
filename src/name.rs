//! The tool's name. Every user-visible name comes from it (ADR 0005):
//! Neovim's `g:<name>`, `<Name>TabLine`, the `<name>_*` notes, the Rex block
//! label and client name, log prefixes and file names. Only this file may
//! spell it out; a test in it checks the rest of the sources don't.

/// The name: lower-case letters only, so no form of it needs escaping.
pub const NAME: &str = "neorex";

/// The project's home, named after the tool.
pub fn repo_url() -> String {
    format!("https://github.com/ian-pascoe/{NAME}")
}

/// The name in upper case, as in `$<NAME>`.
pub fn upper() -> String {
    upper_of(NAME)
}

/// The name capitalised, as in `<Name>TabLine`.
pub fn title() -> String {
    title_of(NAME)
}

fn upper_of(name: &str) -> String {
    name.to_ascii_uppercase()
}

fn title_of(name: &str) -> String {
    let mut out = name.to_string();
    if let Some(first) = out.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_of_a_name() {
        assert_eq!("NEOPANE", upper_of("neopane"));
        assert_eq!("Neopane", title_of("neopane"));
        assert_eq!("NEOREX", upper_of("neorex"));
        assert_eq!("Neorex", title_of("neorex"));
    }

    #[test]
    fn the_name_is_lower_case_letters() {
        assert!(!NAME.is_empty());
        assert!(NAME.bytes().all(|b| b.is_ascii_lowercase()), "{NAME}");
        assert_eq!(NAME.len(), upper().len());
        assert_eq!(NAME, title().to_ascii_lowercase());
        assert_eq!(upper(), title().to_ascii_uppercase());
    }

    /// The forms of the name the guard looks for. Spelled out, not taken from
    /// `NAME`, so a source still spelling the old name fails after a rename.
    fn spellings() -> [String; 3] {
        [
            "neorex".to_string(),
            "Neorex".to_string(),
            "NEOREX".to_string(),
        ]
    }

    /// The string literals in Rust source `src`, with the line each starts on.
    /// Comments, char literals and lifetimes are skipped.
    fn string_literals(src: &str) -> Vec<(usize, String)> {
        let b = src.as_bytes();
        let mut out = Vec::new();
        let mut line = 1;
        let mut i = 0;
        while i < b.len() {
            match b[i] {
                b'\n' => {
                    line += 1;
                    i += 1;
                }
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    let mut depth = 0;
                    while i < b.len() {
                        if b[i..].starts_with(b"/*") {
                            depth += 1;
                            i += 2;
                        } else if b[i..].starts_with(b"*/") {
                            depth -= 1;
                            i += 2;
                            if depth == 0 {
                                break;
                            }
                        } else {
                            line += usize::from(b[i] == b'\n');
                            i += 1;
                        }
                    }
                }
                b'\'' => {
                    // a char literal: 'x', '\n', '\u{..}'; else a lifetime
                    if b.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        while i < b.len() && b[i] != b'\'' {
                            i += 1;
                        }
                        i += 1;
                    } else if let Some(c) = src[i + 1..].chars().next()
                        && src[i + 1 + c.len_utf8()..].starts_with('\'')
                    {
                        i += 2 + c.len_utf8();
                    } else {
                        i += 1;
                    }
                }
                b'r' | b'b' | b'c' | b'"'
                    if b[i] == b'"'
                        || i == 0
                        || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_') =>
                {
                    // a prefix (b, r, br, c, cr), then any hashes, then the quote
                    let mut j = i;
                    while j < b.len() && matches!(b[j], b'r' | b'b' | b'c') && j - i < 2 {
                        j += 1;
                    }
                    let raw = b[i..j].contains(&b'r');
                    let mut hashes = 0;
                    while raw && b.get(j) == Some(&b'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if b.get(j) != Some(&b'"') {
                        // an identifier
                        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                            i += 1;
                        }
                        continue;
                    }
                    let start = j + 1;
                    let mut k = start;
                    let end = loop {
                        if k >= b.len() {
                            break b.len();
                        }
                        if !raw && b[k] == b'\\' {
                            k += 2;
                            continue;
                        }
                        if b[k] == b'"'
                            && b[k + 1..]
                                .iter()
                                .take(hashes)
                                .filter(|&&h| h == b'#')
                                .count()
                                == hashes
                        {
                            break k;
                        }
                        k += 1;
                    };
                    out.push((line, src[start..end].to_string()));
                    line += src[i..end].matches('\n').count();
                    i = end + 1 + hashes;
                }
                c if c.is_ascii_alphanumeric() || c == b'_' => {
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        out
    }

    #[test]
    fn string_literals_are_found() {
        let src = "fn f<'a>(x: &'a str) { // \"no\"\n\
                   /* \"no\" /* nested */ */ let c = '\"'; let e = '\\'';\n\
                   g(\"one \\\" two\", b\"three\", r#\"fo\"ur\"#, br\"five\", ident\"\"); }";
        let found: Vec<_> = string_literals(src).into_iter().map(|(_, s)| s).collect();
        assert_eq!(vec!["one \\\" two", "three", "fo\"ur", "five", ""], found);
    }

    /// Every file under `dir`.
    fn files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// Where `text` spells out the name, in any of its forms.
    fn spells_name(text: &str) -> bool {
        spellings().iter().any(|s| text.contains(s.as_str()))
    }

    /// Every user-visible name comes from `NAME`: no string literal in `src/`,
    /// nor any line of code in `windows.lua`, spells it out.
    #[test]
    fn only_this_file_spells_the_name() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut all = Vec::new();
        files(&root, &mut all);
        all.sort();
        let mut found = Vec::new();
        for path in all {
            let shown = path.strip_prefix(&root).unwrap().display().to_string();
            if path.ends_with("name.rs") && path.parent() == Some(root.as_path()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match path.extension().and_then(|e| e.to_str()) {
                Some("rs") => {
                    for (line, lit) in string_literals(&text) {
                        if spells_name(&lit) {
                            found.push(format!("src/{shown}:{line}: {lit:?}"));
                        }
                    }
                }
                Some("lua") => {
                    for (n, l) in text.lines().enumerate() {
                        if !l.trim_start().starts_with("--") && spells_name(l) {
                            found.push(format!("src/{shown}:{}: {}", n + 1, l.trim()));
                        }
                    }
                }
                _ => {}
            }
        }
        assert!(
            found.is_empty(),
            "spelled out instead of taken from name::NAME:\n{}",
            found.join("\n")
        );
    }
}
