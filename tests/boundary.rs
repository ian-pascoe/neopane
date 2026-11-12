//! The core knows nothing of Rex (#92 decision 7): nothing in `src/daemon/`,
//! `src/screen/`, `src/nvim/` or `src/term/`, tests included, may name
//! `crate::rex` or `crate::providers::rex`. The core reaches Rex only
//! through `crate::providers` and the `PaneProvider` trait.

use std::fs;
use std::path::{Path, PathBuf};

/// The core's directories, under `src/`.
const CORE: &[&str] = &["daemon", "screen", "nvim", "term"];

/// The modules the core may not name, as paths from the crate root.
const FORBIDDEN: &[&[&str]] = &[&["rex"], &["providers", "rex"]];

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

/// The paths `code` names from `crate`, each as its segments, with use
/// groups (`crate::{a::b, c}`) spelled out.
fn crate_paths(code: &str) -> Vec<Vec<String>> {
    let toks = tokens(code);
    let mut out = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        if t == "crate" && toks.get(i + 1).is_some_and(|n| n == "::") {
            paths_from(&toks[i + 2..], &mut vec!["crate".into()], &mut out);
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

/// The forbidden modules `source` names: each as written, `crate::…`.
fn names_rex(source: &str) -> Vec<String> {
    crate_paths(&without_comments(source))
        .into_iter()
        .filter(|p| {
            FORBIDDEN
                .iter()
                .any(|f| p.len() > f.len() && p[1..=f.len()].iter().eq(f.iter()))
        })
        .map(|p| p.join("::"))
        .collect()
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
    ] {
        assert!(!names_rex(named).is_empty(), "{named}");
    }
    for clean in [
        "use crate::providers::choose::Chosen;",
        "use crate::{log, providers::choose};",
        "// use crate::rex::client;",
        "/* crate::providers::rex */ use crate::name::NAME;",
        "let rex = crate::daemon::rexish::x;",
        "use crate::providers::rexx;",
    ] {
        assert!(names_rex(clean).is_empty(), "{clean}");
    }
}
