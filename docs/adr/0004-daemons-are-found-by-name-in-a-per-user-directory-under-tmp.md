# Daemons are found by name in a per-user directory under /tmp

Amended by ADR 0013 before implementation: the attach command is `<name>
attach --address A --pane ID`, every pane id carries a token unique to the
pane, and a newer `<name>` can always ask an older daemon its status and stop
it.

Amended by #94 before implementation: `$<NAME>` counts only from one of that
Neovim's own terminals, `--address` on the bare form attaches as `attach`
does, an unattached daemon with unsaved changes lists them in `unsaved`, and
logs are kept, the previous one as `log.1`.

An attach has to find a daemon that may have outlived the terminal that
started it (ADR 0003), so each daemon has a name, unique among the user's
running daemons, and lives at a predictable path: `$<NAME>_TMPDIR` (else
`/tmp`) `/<name>-<uid>/<daemon name>/`, holding `sock`, `nvim.sock`, `log`
and, while it is unattached with unsaved changes, `unsaved` (#84, #94). This
replaces the random `mkdtemp` directory per launch, and deliberately ignores
the XDG advice to put sockets in `$XDG_RUNTIME_DIR`: systemd removes that at
the user's last logout while their processes, a daemon attached to tmux over
ssh among them, live on. tmux makes the same choice.

`<name>` and `<NAME>` stand for the tool's name, one constant everywhere:
neorex until the rename, then `neopane` (ADR 0005).

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
  directory nobody has locked is stale; whoever claims its name next cleans
  it, renaming its `log` to `log.1` and replacing any older one; a daemon
  leaves its log when it ends, cleanly or not (#94).
- The default name is the launch directory's basename, cleaned to
  `[A-Za-z0-9._-]` and de-duplicated with `-2`, `-3`, … Its path length is
  checked per name.
- Bare `<name> [FILE…]` always starts a new daemon unless given `--name`
  (attach to it if it runs), `--address` (exactly `attach --address`), or run
  inside a daemon's own Neovim (#94). `attach` looks in order at `--address`,
  `--name`, `$<NAME>` (set only in Neovim's environment, and counted only when
  this terminal is one of that Neovim's `:terminal`s, or the process has no
  terminal at all, since a tmux server or terminal app started from a
  `:terminal` copies it; #94), then the only daemon running; none or several
  is an error listing them. Provider variables such as `REX_SESSION` never
  pick a daemon.
- A daemon address is passed whole everywhere outside the platform layer (the
  attach command, `<name> attach --address A --pane ID`; `$<NAME>`;
  `--address`), so on Windows it can be a named pipe, written `//./pipe/<name>`
  with no `/` in the name so the attach command stays bare words; there,
  addresses that differ only in case or in `/` for `\` are the same. Every
  pane id carries a random token unique to the pane, such as `12-k3j9x2qpa8`,
  since names and paths are reused (ADR 0013).
- From the first tagged release on, the layout is a contract across versions:
  a running daemon keeps the binary it started with, and a newer `<name>` must
  still find it, ask its status and stop it, whatever its attach protocol
  version (ADR 0013). `unsaved` is part of that layout: a first line `unsaved
  1`, then one absolute path per line, written by renaming a finished file
  into place; a reader skips a file whose first line it doesn't know (#94).
