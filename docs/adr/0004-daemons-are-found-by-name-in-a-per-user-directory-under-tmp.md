# Daemons are found by name in a per-user directory under /tmp

An attach has to find a daemon that may have outlived the terminal that
started it (ADR 0003), so each daemon has a name, unique among the user's
running daemons, and lives at a predictable path:
`$<NAME>_TMPDIR` (else `/tmp`) `/<name>-<uid>/<daemon name>/`, holding `sock`,
`nvim.sock` and `log` (#84). This replaces the random `mkdtemp` directory per
launch, and deliberately ignores the XDG advice to put sockets in
`$XDG_RUNTIME_DIR`: systemd removes that at the user's last logout while their
processes, a daemon attached to tmux over ssh among them, live on. tmux makes
the same choice.

## Considered Options

- **Random directories plus an index file.** Two records to keep in step,
  and stale entries.
- **`$XDG_RUNTIME_DIR`, else `$TMPDIR`.** Removed at logout; `$TMPDIR`
  depends on the environment and makes macOS paths long against the 103-byte
  socket limit.
- **Flat sockets, as tmux's `<base>/<name>`.** No room for each daemon's
  Neovim socket, log and lock.
- **Bare `<name>` joins a running daemon.** A second project would land in
  the first project's Neovim and Rex session.

## Consequences

- Predictable paths no longer hide a daemon: the base directory must be a real
  directory (checked with `lstat`), owned by the user, mode 0700, or `<name>`
  refuses and names `$<NAME>_TMPDIR`. Another user who pre-creates it can deny
  service but take nothing over. Don't "fix" this back to random directories:
  discovery rests on it.
- A daemon is live while it holds an exclusive `flock` on its directory, which
  also makes systemd-tmpfiles skip it. Claiming a name is: create or reuse the
  directory, lock it, check nothing answers on `sock` or `nvim.sock`, bind. A
  directory nobody has locked is stale; whoever claims its name next cleans it,
  keeping the old log.
- The default name is the launch directory's basename, cleaned to
  `[A-Za-z0-9._-]` and de-duplicated with `-2`, `-3`, … Its path length is
  checked per name.
- Bare `<name> [FILE…]` always starts a new daemon unless given `--name`,
  `--address`, or run inside a daemon's own Neovim. `attach` looks in order at
  `--address`, `--name`, `$<NAME>` (set only in Neovim's environment), then the
  only daemon running; none or several is an error listing them. Provider
  variables such as `REX_SESSION` never pick a daemon.
- A daemon address is passed whole everywhere outside the platform layer (the
  requested pane's command line, `$<NAME>`, `--address`), so on Windows it can
  be a named pipe. Requested pane ids carry a per-daemon token, since names
  are reused.
- The layout is a contract across versions: a running daemon keeps the binary
  it started with, and a newer `<name>` must still find it.
