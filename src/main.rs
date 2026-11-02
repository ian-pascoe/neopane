use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

use neorex::daemon::{launch, pane_helper, serve};
use neorex::debug;

const USAGE: &str = "\
usage: neorex [nvim args]    open Neovim in this pane
       neorex <command>

commands:
  debug-keys                 show each key, paste, focus and mouse event as you type
  debug-events [nvim args]   run Neovim and print the redraw events it sends
  debug-tui [nvim args]      run Neovim in this terminal through neorex's renderer
  debug-panes SOCKET         stand-in daemon: drive the terminals of panes that connect
  debug-rex SOCKET           open a split in this Rex session and drive its pane; print Rex events
  pane SOCKET [ID]           hand this terminal to the daemon at SOCKET, then wait
  help                       show this help

neorex runs in a Rex pane: it needs REX_SERVER and REX_SESSION, and uses REX_BLOCK
to find its pane. Only debug-rex reads them, of the debug commands.

The daemon logs to `log` in its private directory, neorex-<launcher pid>.XXXXXX in
$XDG_RUNTIME_DIR, else $TMPDIR, else /tmp (each only when it is an absolute path).
On a clean exit (Neovim quits or crashes, the last pane closes, or a signal) the
daemon removes the log.
When the daemon itself fails, the log and directory stay, and if it failed to start,
neorex prints the log's path.
";

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Runs, and on an error prints its name on stderr and exits with status 1.
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let cmd: &OsStr = if args.len() >= 2 {
        &args[1]
    } else {
        OsStr::new("")
    };
    let rest: &[OsString] = if args.len() >= 2 { &args[2..] } else { &[] };

    if cmd == "help" || cmd == "-h" || cmd == "--help" {
        eprint!("{USAGE}");
        return Ok(());
    }
    if cmd == "debug-keys" && rest.is_empty() {
        return debug::keys::run();
    }
    if cmd == "debug-events" {
        return debug::events::run(rest);
    }
    if cmd == "debug-tui" {
        return debug::tui::run(rest);
    }
    if cmd == "debug-panes" && rest.len() == 1 {
        return debug::panes::run(&rest[0]);
    }
    if cmd == "debug-rex" && rest.len() == 1 {
        return debug::rex::run(&rest[0]);
    }
    if cmd == "pane" && rest.len() == 1 {
        return Ok(pane_helper::run(&rest[0], None)?);
    }
    if cmd == "pane" && rest.len() == 2 {
        let Some(id) = parse_u32(rest[1].as_encoded_bytes()) else {
            usage_exit();
        };
        return Ok(pane_helper::run(&rest[0], Some(id))?);
    }
    // started by the launcher, not by people
    if cmd == "daemon" && !rest.is_empty() {
        return serve::run(&rest[0].to_string_lossy(), &rest[1..]);
    }
    for known in ["debug-keys", "debug-panes", "debug-rex", "pane", "daemon"] {
        if cmd == known {
            usage_exit();
        }
    }
    launch::run(args.get(1..).unwrap_or(&[]))
}

/// Prints the usage and exits with status 2.
fn usage_exit() -> ! {
    eprint!("{USAGE}");
    std::process::exit(2);
}

/// `s` as a decimal `u32`, read the way `parseInt` does:
/// a leading `+` or `-` (only zero is negative and fits), and `_` between
/// digits.
fn parse_u32(s: &[u8]) -> Option<u32> {
    let (negative, digits) = match s.first() {
        Some(b'+') => (false, &s[1..]),
        Some(b'-') => (true, &s[1..]),
        _ => (false, s),
    };
    if digits.is_empty() || digits[0] == b'_' || digits[digits.len() - 1] == b'_' {
        return None;
    }
    let mut n: u32 = 0;
    for &c in digits {
        if c == b'_' {
            continue;
        }
        if !c.is_ascii_digit() {
            return None;
        }
        let digit = u32::from(c - b'0');
        if negative && digit != 0 {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(digit)?;
    }
    Some(n)
}

#[cfg(test)]
mod tests {
    use super::{USAGE, parse_u32};

    #[test]
    fn help_says_where_the_log_is() {
        for want in [
            "REX_SERVER",
            "REX_SESSION",
            "REX_BLOCK",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
            "`log` in its private directory, neorex-<launcher pid>.XXXXXX",
        ] {
            assert!(USAGE.contains(want), "{want}");
        }
    }

    #[test]
    fn pane_ids_parse_like_parse_int() {
        assert_eq!(Some(10), parse_u32(b"10"));
        assert_eq!(Some(10), parse_u32(b"+10"));
        assert_eq!(Some(1000), parse_u32(b"1_000"));
        assert_eq!(Some(0), parse_u32(b"-0"));
        assert_eq!(Some(u32::MAX), parse_u32(b"4294967295"));
        for bad in [
            &b""[..],
            b"+",
            b"-1",
            b"_10",
            b"10_",
            b" 10",
            b"10 ",
            b"0x10",
            b"4294967296",
        ] {
            assert_eq!(None, parse_u32(bad), "{bad:?}");
        }
    }
}
