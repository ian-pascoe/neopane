//! The core knows nothing of Rex (#92 decision 7): nothing in `src/daemon/`,
//! `src/screen/`, `src/nvim/` or `src/term/`, tests included, may name
//! `crate::rex` or `crate::providers::rex`, however it gets there: through
//! `super`, through an alias of `providers`, or through any module path
//! with `rex` in it. The core reaches Rex only through `crate::providers`'
//! own modules, such as `choose`, and the `PaneProvider` trait.

use std::fs;
use std::path::{Path, PathBuf};

/// The core's directories, under `src/`.
const CORE: &[&str] = &["daemon", "screen", "nvim", "term"];

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `source` without its comments, `//` to the end of a line and `/* */`.
/// (A `//` in a string literal cuts it short: no path follows one in a
/// string.)
fn without_comments(source: &str) -> String {
    let mut out = String::new();
    let mut rest = source;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.find('\n').map_or("", |i| &r[i..]);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.find("*/").map_or("", |i| &r[i + 2..]);
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// The words of `code`: identifiers and `::`, `{`, `}`, `,`; whitespace
/// and anything else dropped.
fn tokens(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            word.push(c);
            continue;
        }
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        match c {
            ':' if chars.peek() == Some(&':') => {
                chars.next();
                out.push("::".into());
            }
            '{' | '}' | ',' => out.push(c.to_string()),
            _ => {}
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// The paths `toks` names from `crate` or `super`, each as its segments,
/// with use groups (`crate::{a::b, c}`) spelled out.
fn rooted_paths(toks: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let rooted = t == "crate" || (t == "super" && (i == 0 || toks[i - 1] != "::"));
        if rooted && toks.get(i + 1).is_some_and(|n| n == "::") {
            paths_from(&toks[i + 2..], &mut vec![t.clone()], &mut out);
        }
    }
    out
}

/// Reads the path, or use group, `toks` starts with, after `prefix`, into
/// `out`; returns how many tokens it took.
fn paths_from(toks: &[String], prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) -> usize {
    let mut i = 0;
    let depth = prefix.len();
    loop {
        match toks.get(i).map(String::as_str) {
            Some("{") => {
                i += 1;
                loop {
                    i += paths_from(&toks[i..], prefix, out);
                    match toks.get(i).map(String::as_str) {
                        Some(",") => i += 1,
                        Some("}") => {
                            i += 1;
                            break;
                        }
                        _ => break,
                    }
                }
                prefix.truncate(depth);
                return i;
            }
            Some(w) if w != "::" && w != "," && w != "}" => {
                prefix.push(w.to_string());
                i += 1;
                if toks.get(i).map(String::as_str) == Some("::") {
                    i += 1;
                    continue;
                }
                out.push(prefix.clone());
                prefix.truncate(depth);
                return i;
            }
            _ => {
                prefix.truncate(depth);
                return i;
            }
        }
    }
}

/// How `source` names Rex, or could: each as written.
/// - any path through or to a `rex` module, as `rex::client`, `p::rex` or
///   `providers::rex`, whatever comes before it;
/// - a path from `crate` or `super` with a `rex` segment, as `use crate::rex;`
///   or `use crate::{rex, log};`;
/// - the `providers` module itself, which an alias could take to its `rex`:
///   `use crate::providers;`, `… as p`, `providers::{self, …}`.
fn names_rex(source: &str) -> Vec<String> {
    let toks = tokens(&without_comments(source));
    let mut found = Vec::new();
    for w in toks.windows(2) {
        if w[0] == "rex" && w[1] == "::" {
            found.push("rex::".to_string());
        } else if w[0] == "::" && w[1] == "rex" {
            found.push("::rex".to_string());
        }
    }
    for p in rooted_paths(&toks) {
        let last = p.last().map(String::as_str);
        let via_rex = p[1..].iter().any(|s| s == "rex");
        let providers_itself = last == Some("providers")
            || (last == Some("self") && p.len() > 1 && p[p.len() - 2] == "providers");
        if via_rex || providers_itself {
            found.push(p.join("::"));
        }
    }
    found.dedup();
    found
}

#[test]
fn the_core_names_neither_rex_nor_rexs_pane_provider() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for dir in CORE {
        rust_files(&src.join(dir), &mut files);
    }
    assert!(files.len() > 20, "found only {files:?}");
    let mut found = Vec::new();
    for f in &files {
        let source = fs::read_to_string(f).unwrap();
        for p in names_rex(&source) {
            found.push(format!("{}: {p}", f.strip_prefix(&src).unwrap().display()));
        }
    }
    assert!(
        found.is_empty(),
        "the core names Rex:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_core_file_using_rex_fails_the_check() {
    for named in [
        "use crate::rex::client::Client;",
        "use crate::rex;",
        "use crate::providers::rex::RexProvider;",
        "let p = crate::providers::rex::RexProvider::new(s);",
        "use crate::{log, providers::{choose, rex::RexProvider}};",
        "use crate::{\n    rex::session::Session,\n    name::NAME,\n};",
        "use crate :: rex :: ids;",
        // an alias of `providers`, then its `rex`
        "use crate::providers;\nlet p = providers::rex::RexProvider::new(s);",
        "use crate::providers as p;",
        "use crate::providers::{self, choose};",
        "use crate::{log, providers};",
        "use super::super::providers;",
        // through `super`
        "use super::super::providers::rex::RexProvider;",
        "use super::super::rex;",
        // any module path through `rex`
        "let c = r::rex::client::Client::from_stream(s);",
        "use p::rex as r;",
    ] {
        assert!(!names_rex(named).is_empty(), "{named}");
    }
    for clean in [
        "use crate::providers::choose::Chosen;",
        "use crate::providers::choose::{self, Chosen};",
        "use crate::{log, providers::choose};",
        "use super::super::provider::PaneProvider;",
        "// use crate::rex::client;",
        "/* crate::providers::rex */ use crate::name::NAME;",
        "let rex = crate::daemon::rexish::x;",
        "use crate::providers::rexx;",
        "let rex = 1; log!(\"rex: {rex}\");",
    ] {
        assert!(names_rex(clean).is_empty(), "{clean}");
    }
}
