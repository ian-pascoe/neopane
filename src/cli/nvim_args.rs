//! Reads Neovim's arguments by Neovim 0.12's own rules (#94 decision 3),
//! without changing them: to refuse what can't work under a daemon
//! (decision 5), to catch one of our options among them (decision 4), and to
//! find the files they name.

use std::ffi::OsString;

use crate::name::NAME;

/// What reading Neovim's arguments found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The words Neovim takes as files, in order.
    pub files: Vec<OsString>,
}

/// Our own options, which go before Neovim's arguments, each with what its
/// placement message shows after it.
const OWN_OPTIONS: [(&str, &str); 5] = [
    ("--name", " N"),
    ("--address", " A"),
    ("--provider", " P"),
    ("--detach", ""),
    ("--no-wait", ""),
];

/// Reads `words` as Neovim would. Err is the line to print after
/// `<name>: `: a refused word, or one of our options among them.
pub(super) fn read(words: &[OsString]) -> Result<Reading, String> {
    let mut reading = Reading::default();
    // the word that asked for recovery, as typed
    let mut recover = None;
    // the word that started Ex mode: Neovim's flag holds for later words too
    let mut ex_mode: Option<String> = None;
    let mut i = 0;
    while i < words.len() {
        let word = words[i].as_encoded_bytes();
        let shown = words[i].to_string_lossy();
        let last = i + 1 == words.len();
        i += 1;
        if word == b"--" {
            reading.files.extend(words[i..].iter().cloned());
            break;
        }
        if word == b"-" {
            return Err(
                "- reads text from stdin, which the daemon's Neovim can't read: save it to a \
                 file first"
                    .to_string(),
            );
        }
        if let Some(long) = word.strip_prefix(b"--") {
            if let Some((own, value)) = own_option(word) {
                return Err(format!(
                    "put {own} before Neovim's arguments: {NAME} {own}{value} [NVIM-ARGS]"
                ));
            }
            let long = long.to_ascii_lowercase();
            let is = |name: &[u8]| long == name;
            let starts = |name: &[u8]| long.starts_with(name);
            if is(b"embed") || is(b"headless") || starts(b"listen") {
                return Err(format!(
                    "{shown} is the daemon's own: it starts Neovim with --embed --headless \
                     --listen"
                ));
            }
            if starts(b"server") || starts(b"remote") {
                return Err(format!(
                    "{shown} talks to another Neovim; to open a file in a running daemon: \
                     {NAME} attach --name N FILE"
                ));
            }
            if is(b"help") || is(b"version") || is(b"api-info") {
                return Err(prints_and_exits(&shown));
            }
            if starts(b"cmd") || starts(b"startuptime") {
                i += 1;
            }
            continue;
        }
        match word.first() {
            // a command, or a line
            Some(b'+') => continue,
            Some(b'-') => {}
            _ => {
                reading.files.push(words[i - 1].clone());
                continue;
            }
        }
        // letter by letter
        let next_is_option = words
            .get(i)
            .is_some_and(|w| w.as_encoded_bytes().first() == Some(&b'-'));
        let mut j = 1;
        while j < word.len() {
            let letter = word[j];
            j += 1;
            let rest = &word[j..];
            match letter {
                b'c' | b't' => {
                    if rest.is_empty() {
                        i += 1;
                    }
                    break;
                }
                b'q' => {
                    if rest.is_empty() && !last {
                        i += 1;
                    }
                    break;
                }
                b'V' => {
                    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                    let level = std::str::from_utf8(&rest[..digits])
                        .ok()
                        .and_then(|d| d.parse::<u64>().ok())
                        .unwrap_or(10);
                    if level >= 2 && digits == rest.len() {
                        return Err(format!(
                            "{shown} without a file would stop Neovim from starting: give it \
                             one, as {shown}log.txt"
                        ));
                    }
                    break;
                }
                b'w' => {
                    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
                    if digits == 0 {
                        i += 1;
                        break;
                    }
                    j += digits;
                }
                b'o' | b'O' | b'p' => {
                    j += rest.iter().take_while(|b| b.is_ascii_digit()).count();
                }
                b'l' => return Err(without_a_screen(&shown)),
                b's' if let Some(ex) = &ex_mode => {
                    // -es as typed, or -e -s across words
                    let typed = if *ex == shown {
                        shown.to_string()
                    } else {
                        format!("{ex} {shown}")
                    };
                    return Err(without_a_screen(&typed));
                }
                b'u' | b'U' | b'i' | b'W' | b's' => {
                    i += 1;
                    break;
                }
                b'S' => {
                    if !last && !next_is_option {
                        i += 1;
                    }
                    break;
                }
                b'e' | b'E' => ex_mode = Some(shown.to_string()),
                b'h' | b'?' | b'v' => return Err(prints_and_exits(&shown)),
                b'r' | b'L' => recover = Some(shown.clone()),
                _ => {}
            }
        }
    }
    if let Some(shown) = recover
        && reading.files.is_empty()
    {
        return Err(format!(
            "{shown} with no file lists swap files: run nvim {shown}"
        ));
    }
    Ok(reading)
}

/// One of our options, as `--name` or `--name=N`: its name and what its
/// placement message shows after it.
fn own_option(word: &[u8]) -> Option<(&'static str, &'static str)> {
    OWN_OPTIONS.into_iter().find(|(own, _)| {
        word.strip_prefix(own.as_bytes())
            .is_some_and(|rest| rest.is_empty() || rest.first() == Some(&b'='))
    })
}

fn prints_and_exits(shown: &str) -> String {
    format!(
        "{shown} would make Neovim print and exit: put -h or -v first for {NAME}'s own, or run \
         nvim -h"
    )
}

fn without_a_screen(shown: &str) -> String {
    format!("{shown} runs Neovim without a screen: run nvim {shown}")
}
