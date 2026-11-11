//! The help and version texts (#94 decisions 8 and 12), written by hand
//! from the name constant.

use crate::attach::{ATTACH_PROTOCOL_VERSION, build};
use crate::log;
use crate::name::NAME;

/// The pane provider protocol versions this build speaks.
pub const PANE_PROVIDER_PROTOCOLS: &[i64] = &[1];

/// The project's home, named after the tool.
pub fn repo_url() -> String {
    format!("https://github.com/ian-pascoe/{NAME}")
}

/// `--version`: the hello's `build`, then the protocols it speaks.
pub fn version() -> String {
    let providers: Vec<String> = PANE_PROVIDER_PROTOCOLS.iter().map(i64::to_string).collect();
    format!(
        "{}\nattach protocol {ATTACH_PROTOCOL_VERSION}\npane provider protocols {}\n",
        build(),
        providers.join(", ")
    )
}

/// `--help`.
pub fn help() -> String {
    let n = NAME;
    // the environment, aligned past the longest name
    let env = [
        (
            log::env_var(),
            "how much daemons log: warn, info (default), debug",
        ),
        (
            "REX_SERVER, REX_SESSION".to_string(),
            "the Rex session to open panes in (needed for now)",
        ),
        (
            "REX_BLOCK".to_string(),
            "the Rex pane it runs in, which shows the first window",
        ),
    ];
    let width = env.iter().map(|(name, _)| name.len()).max().unwrap_or(0) + 2;
    let env: String = env
        .iter()
        .map(|(name, what)| format!("  {name:width$}{what}\n"))
        .collect();
    let log = log::env_var();
    let url = repo_url();
    format!(
        "\
{n}: one Neovim in the background, shown in every terminal attached to it.

Usage:
  {n} [OPTIONS] [NVIM-ARGS...]
        Start a daemon and show its Neovim here. NVIM-ARGS go to Neovim, as in
        {n} -O a.c b.c. In a daemon's own :terminal, {n} FILE opens
        FILE in a new window there and returns once you're done with it.
  {n} attach [--name N | --address A] [--provider P] [+LINE] [FILE]
        Show a running daemon here, with FILE in a new window.
  {n} list [--json]
        List running daemons.
  {n} status [--name N | --address A] [--json]
        Show one daemon: panes, unsaved files, pane provider, log.
  {n} kill [--name N | --address A | --all] [--force]
        End a daemon. Refused while it has unsaved changes, unless --force,
        which leaves them in swap files.
  {n} provider check [--config FILE] [--provider P] [--daemon-name N]
  {pad}                [--no-find] [--full] [--json]
        Run your config as a terminal here would, and report what it chose.
  {n} provider show [NAME]
        Print a built-in pane provider as Lua.
  {n} provider types [--config FILE]
        Write Lua types for editing your config.

Options (before NVIM-ARGS):
  --name N       Use the daemon named N: attach if it runs, else start it.
                 Without it, a new daemon named after this directory starts.
  --address A    Attach to the daemon at A.
  --provider P   A hint for your config when this terminal chooses the pane
                 provider; none: no pane provider.
  --detach       Start the daemon without showing it, and print its name.
  --no-wait      In a daemon's :terminal, open FILE and return at once.
  -h, --help     Show this help.
  -v, --version  Show the version.

A running daemon takes only [+LINE] FILE. To open a file named like a
command, write ./list. To edit with it, set EDITOR={n}.

Environment:
{env}
A daemon logs to `log` in its private directory, {n}-<launcher pid>.XXXXXX in
$XDG_RUNTIME_DIR, else $TMPDIR, else /tmp (each only when it is an absolute path).
On a clean exit the daemon removes it, unless {log} is debug; when the daemon
fails, it stays, and {n} prints its path.
More: {url}   Developer tools: {n} help debug
",
        pad = " ".repeat(n.len()),
    )
}

/// `help debug`: the developer tools.
pub fn debug_help() -> String {
    let n = NAME;
    format!(
        "\
{n}'s developer tools, which run each layer on its own:

  {n} debug-keys
        Show each key, paste, focus and mouse event as you type.
  {n} debug-events [NVIM-ARGS...]
        Run Neovim and print the redraw events it sends.
  {n} debug-tui [NVIM-ARGS...]
        Run Neovim in this terminal through {n}'s renderer.
  {n} debug-panes SOCKET
        A stand-in daemon: drive the terminals of panes that connect.
  {n} debug-rex SOCKET
        Open a split in this Rex session and drive its pane; print Rex
        events. It reads REX_SERVER, REX_SESSION and REX_BLOCK.
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::name::upper;

    #[test]
    fn help_says_where_the_log_is() {
        for want in [
            "REX_SERVER",
            "REX_SESSION",
            "REX_BLOCK",
            "XDG_RUNTIME_DIR",
            "TMPDIR",
            &format!("`log` in its private directory, {NAME}-<launcher pid>.XXXXXX"),
            &format!("{}_LOG", upper()),
        ] {
            assert!(help().contains(want), "{want}");
        }
    }

    #[test]
    fn help_names_every_command_and_option() {
        let help = help();
        for want in [
            format!("{NAME} [OPTIONS] [NVIM-ARGS...]"),
            format!("{NAME} attach "),
            format!("{NAME} list "),
            format!("{NAME} status "),
            format!("{NAME} kill "),
            format!("{NAME} provider check "),
            format!("{NAME} provider show "),
            format!("{NAME} provider types "),
            format!("EDITOR={NAME}"),
            format!("More: https://github.com/ian-pascoe/{NAME}"),
            format!("Developer tools: {NAME} help debug"),
        ] {
            assert!(help.contains(&want), "{want}");
        }
        for option in [
            "--name N",
            "--address A",
            "--provider P",
            "--detach",
            "--no-wait",
            "-h, --help",
            "-v, --version",
        ] {
            assert!(help.contains(&format!("\n  {option} ")), "{option}");
        }
        assert!(help.starts_with(&format!("{NAME}: one Neovim")));
        // the second line of provider check's synopsis lines up under its first
        let check = help.find("[--config FILE] [--provider P]").unwrap();
        let line_start = help[..check].rfind('\n').unwrap() + 1;
        let next = help[check..].find('\n').unwrap() + check + 1;
        assert_eq!(help[next..].find("[--no-find]"), Some(check - line_start));
    }

    #[test]
    fn debug_help_lists_the_debug_commands() {
        let help = debug_help();
        for command in [
            "debug-keys",
            "debug-events [NVIM-ARGS...]",
            "debug-tui [NVIM-ARGS...]",
            "debug-panes SOCKET",
            "debug-rex SOCKET",
        ] {
            assert!(help.contains(&format!("  {NAME} {command}\n")), "{command}");
        }
        assert!(!help.contains("debug-latency"));
    }

    #[test]
    fn version_is_build_then_protocols() {
        let version = version();
        let lines: Vec<&str> = version.lines().collect();
        assert_eq!(
            lines,
            [build(), "attach protocol 1", "pane provider protocols 1"]
        );
        assert!(version.ends_with('\n'));
        assert!(
            lines[0].starts_with(&format!("{NAME} {} ", env!("CARGO_PKG_VERSION")))
                || lines[0] == format!("{NAME} {}", env!("CARGO_PKG_VERSION"))
        );
    }
}
