use std::ffi::{OsStr, OsString};
use std::process::ExitCode;

use neorex::attach::{PaneId, Transport};
use neorex::daemon::{launch, pane_helper, serve};
use neorex::debug;
use neorex::log;
use neorex::name::NAME;

fn usage() -> String {
    format!(
        "\
usage: {NAME} [nvim args]    open Neovim in this pane
       {NAME} <command>

commands:
  debug-keys                 show each key, paste, focus and mouse event as you type
  debug-events [nvim args]   run Neovim and print the redraw events it sends
  debug-tui [nvim args]      run Neovim in this terminal through {NAME}'s renderer
  debug-panes SOCKET         stand-in daemon: drive the terminals of panes that connect
  debug-rex SOCKET           open a split in this Rex session and drive its pane; print Rex events
  help                       show this help

{NAME} runs in a Rex pane: it needs REX_SERVER and REX_SESSION, and uses REX_BLOCK
to find its pane. Only debug-rex reads them, of the debug commands.

The daemon logs to `log` in its private directory, {NAME}-<launcher pid>.XXXXXX in
$XDG_RUNTIME_DIR, else $TMPDIR, else /tmp (each only when it is an absolute path).
On a clean exit (Neovim quits or crashes, the last pane closes, or a signal) the
daemon removes the log, unless {log} is debug.
{log} sets what the daemon logs: warn, info (the default), or debug, which adds
trace timestamps.
When the daemon itself fails, the log and directory stay, and if it failed to start,
{NAME} prints the log's path.
",
        log = log::env_var()
    )
}

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Runs, and on an error prints its name on stderr and exits with status 1.
fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::from(1)
        }
    }
}

/// What a helper command asks for.
#[derive(Debug, PartialEq, Eq)]
enum Helper {
    /// `attach --address A --pane ID [--transport fd|relay]`: a requested
    /// pane's helper.
    Attach {
        address: OsString,
        pane: PaneId,
        transport: Transport,
    },
    /// `pane`, which older daemons ran: exits 1 with this line.
    Replaced(String),
    Usage,
}

/// The helper command `cmd` with `rest`, if it is one.
fn helper_command(cmd: &OsStr, rest: &[OsString]) -> Option<Helper> {
    if cmd == "pane" {
        return Some(Helper::Replaced(format!(
            "{NAME}: pane was replaced by attach --pane (this came from an older daemon)"
        )));
    }
    if cmd != "attach" {
        return None;
    }
    let (mut address, mut pane, mut transport) = (None, None, None);
    let mut words = rest.iter();
    while let Some(flag) = words.next() {
        let Some(value) = words.next() else {
            return Some(Helper::Usage);
        };
        let text = value.to_str();
        let ok = match flag.to_str() {
            Some("--address") if address.is_none() => {
                address = Some(value.clone());
                true
            }
            Some("--pane") if pane.is_none() => {
                pane = text.and_then(PaneId::parse);
                pane.is_some()
            }
            Some("--transport") if transport.is_none() => {
                transport = text.and_then(Transport::from_name);
                transport.is_some()
            }
            _ => false,
        };
        if !ok {
            return Some(Helper::Usage);
        }
    }
    // a self-attach, without --pane, comes with #105
    match (address, pane) {
        (Some(address), Some(pane)) => Some(Helper::Attach {
            address,
            pane,
            transport: transport.unwrap_or(Transport::Fd),
        }),
        _ => Some(Helper::Usage),
    }
}

/// Prints a helper's line, if any, and exits with its status.
fn helper_exit(exit: pane_helper::Exit) -> ExitCode {
    if let Some(m) = exit.message {
        eprintln!("{NAME}: {m}");
    }
    ExitCode::from(exit.status)
}

fn run() -> Result<ExitCode> {
    let args: Vec<OsString> = std::env::args_os().collect();
    let cmd: &OsStr = if args.len() >= 2 {
        &args[1]
    } else {
        OsStr::new("")
    };
    let rest: &[OsString] = if args.len() >= 2 { &args[2..] } else { &[] };

    let done = |r: Result<()>| r.map(|()| ExitCode::SUCCESS);
    if cmd == "help" || cmd == "-h" || cmd == "--help" {
        eprint!("{}", usage());
        return Ok(ExitCode::SUCCESS);
    }
    if cmd == "debug-keys" && rest.is_empty() {
        return done(debug::keys::run());
    }
    if cmd == "debug-events" {
        return done(debug::events::run(rest));
    }
    if cmd == "debug-tui" {
        return done(debug::tui::run(rest));
    }
    if cmd == "debug-panes" && rest.len() == 1 {
        return done(debug::panes::run(&rest[0]));
    }
    if cmd == "debug-rex" && rest.len() == 1 {
        return done(debug::rex::run(&rest[0]));
    }
    // hidden: for docs/latency.md
    if cmd == "debug-latency" && rest.len() == 1 {
        return done(debug::latency::run(&rest[0]));
    }
    // hidden: run by a requested pane's terminal
    match helper_command(cmd, rest) {
        Some(Helper::Attach {
            address,
            pane,
            transport,
        }) => {
            let want = pane_helper::Want::Requested(pane);
            return Ok(helper_exit(pane_helper::run(&address, want, transport)));
        }
        Some(Helper::Replaced(line)) => {
            eprintln!("{line}");
            return Ok(ExitCode::from(1));
        }
        Some(Helper::Usage) => usage_exit(),
        None => {}
    }
    // started by the launcher, not by people
    if cmd == "daemon" && !rest.is_empty() {
        return done(serve::run(&rest[0].to_string_lossy(), &rest[1..]));
    }
    for known in [
        "debug-keys",
        "debug-panes",
        "debug-rex",
        "debug-latency",
        "daemon",
    ] {
        if cmd == known {
            usage_exit();
        }
    }
    Ok(helper_exit(launch::run(args.get(1..).unwrap_or(&[]))?))
}

/// Prints the usage and exits with status 2.
fn usage_exit() -> ! {
    eprint!("{}", usage());
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::{Helper, helper_command, usage};
    use neorex::attach::{PaneId, Transport};
    use neorex::name::NAME;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn help_says_where_the_log_is() {
        for want in [
            "REX_SERVER",
            "REX_SESSION",
            "REX_BLOCK",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
            &format!("`log` in its private directory, {NAME}-<launcher pid>.XXXXXX"),
            &format!("{}_LOG", neorex::name::upper()),
        ] {
            assert!(usage().contains(want), "{want}");
        }
    }

    fn helper(words: &[&str]) -> Option<Helper> {
        let rest: Vec<OsString> = words[1..].iter().map(OsString::from).collect();
        helper_command(OsStr::new(words[0]), &rest)
    }

    #[test]
    fn pane_points_at_attach() {
        let line =
            format!("{NAME}: pane was replaced by attach --pane (this came from an older daemon)");
        assert_eq!(
            Some(Helper::Replaced(line.clone())),
            helper(&["pane", "a", "b"])
        );
        assert_eq!(Some(Helper::Replaced(line)), helper(&["pane"]));
        assert!(!usage().contains("pane SOCKET"));
    }

    #[test]
    fn attach_takes_an_address_and_a_pane() {
        let pane = PaneId::parse("2-abcdefghij").unwrap();
        let want = |transport| {
            Some(Helper::Attach {
                address: "/tmp/x/sock".into(),
                pane,
                transport,
            })
        };
        assert_eq!(
            want(Transport::Fd),
            helper(&[
                "attach",
                "--address",
                "/tmp/x/sock",
                "--pane",
                "2-abcdefghij"
            ])
        );
        assert_eq!(
            want(Transport::Relay),
            helper(&[
                "attach",
                "--pane",
                "2-abcdefghij",
                "--address",
                "/tmp/x/sock",
                "--transport",
                "relay"
            ])
        );
        for bad in [
            &["attach"][..],
            &["attach", "--address", "/a"],
            &["attach", "--pane", "2-abcdefghij"],
            &["attach", "--address", "/a", "--pane", "2"],
            &[
                "attach",
                "--address",
                "/a",
                "--pane",
                "2-abcdefghij",
                "--pane",
            ],
            &[
                "attach",
                "--address",
                "/a",
                "--address",
                "/b",
                "--pane",
                "2-abcdefghij",
            ],
            &[
                "attach",
                "--address",
                "/a",
                "--pane",
                "2-abcdefghij",
                "--transport",
                "x",
            ],
            &[
                "attach",
                "--address",
                "/a",
                "--pane",
                "2-abcdefghij",
                "file",
            ],
        ] {
            assert_eq!(Some(Helper::Usage), helper(bad), "{bad:?}");
        }
        assert_eq!(None, helper(&["debug-keys"]));
    }
}
