//! Opening and closing panes: `open_beside` splits the origin's pane with
//! `split-window -d`, leaving the active pane alone, and runs the attach
//! command in the new one, which tmux `execvp`s, as it is two words or
//! more; `close` kills a pane, done if it is already gone.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use super::is_pane_id;
use super::server::Server;
use crate::providers::builtin::Given;

/// The new pane's id, or why there's none.
pub(super) fn open_beside(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let origin = pane_id(given, "origin")?;
    let attach = given.var("attach").unwrap_or_default();
    let words = unquoted(&attach).ok_or("ATTACH isn't as the daemon writes it")?;
    if words.len() < 2 {
        return Err("the attach command is under two words".into());
    }
    let mut args: Vec<OsString> = vec!["split-window".into(), "-d".into()];
    let direction = given.var("direction").unwrap_or_default();
    let (axis, before) = match direction.as_slice() {
        b"left" => ("-h", true),
        b"right" => ("-h", false),
        b"up" => ("-v", true),
        b"down" => ("-v", false),
        other => {
            return Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            ));
        }
    };
    args.push(axis.into());
    if before {
        args.push("-b".into());
    }
    let size = given.var("size").unwrap_or_default();
    if !size.is_empty() {
        if !size.iter().all(u8::is_ascii_digit) {
            return Err(format!(
                "SIZE {:?} isn't cells",
                String::from_utf8_lossy(&size)
            ));
        }
        args.extend(["-l".into(), OsString::from_vec(size)]);
    }
    args.extend(["-t".into(), origin.into()]);
    args.extend(["-P".into(), "-F".into(), "#{pane_id}".into()]);
    args.extend(words.into_iter().map(OsString::from_vec));
    let out = server.run(&args)?;
    let id = String::from_utf8_lossy(&out);
    let id = id.lines().next().unwrap_or_default().trim();
    if !is_pane_id(id.as_bytes()) {
        return Err(format!("split-window printed {id:?}, not a pane id"));
    }
    Ok(id.to_string())
}

/// Kills the pane, or says why it's still there.
pub(super) fn close(given: &Given) -> Result<(), String> {
    let server = Server::of(given)?;
    let pane = pane_id(given, "terminal")?;
    let Err(why) = server.run(&["kill-pane", "-t", &pane]) else {
        return Ok(());
    };
    // gone already, or its server with it
    match server.run(&["list-panes", "-a", "-F", "#{pane_id}"]) {
        Ok(out) if out.split(|&b| b == b'\n').any(|l| l == pane.as_bytes()) => Err(why),
        _ => Ok(()),
    }
}

/// Input `name`, a pane id `%N`, or why not: anything else could name a
/// session or window to tmux.
fn pane_id(given: &Given, name: &str) -> Result<String, String> {
    let id = given.var(name).unwrap_or_default();
    if !is_pane_id(&id) {
        return Err(format!(
            "{} {:?} isn't a tmux pane id",
            name.to_ascii_uppercase(),
            String::from_utf8_lossy(&id)
        ));
    }
    Ok(String::from_utf8_lossy(&id).into_owned())
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
