# Built-in pane providers other than Rex are tables whose every capability runs neopane itself

Amended by SPI-1135 (ADR 0007): where the environment names several
built-ins, the default proves which multiplexer the attach is in with
`<name>.inside`, and its refused guess is only logged.

Built-in pane providers other than Rex are pane provider tables whose every
capability runs neopane's own binary (`{exe}`), which speaks the capability
contract for that multiplexer: `{exe} provider builtin NAME CAPABILITY`
(SPI-1126, SPI-1131). Shell one-liners can't convert a layout, hold a
connection or speak a socket, and a native provider would take away
per-capability override. The command only ever gains forms, since a daemon
upgraded in place runs a newer one.

Each table is static, built without Lua, and passes the config's checker
with no problem. `<name>.builtin.NAME()` returns a fresh copy, taking no
options, so a config can return it as it is or copy it and change one
capability; `<name> provider show NAME` prints the same table as a Lua file;
the built-in default chooses it with no config. tmux's is the first: in a
tmux pane (`TERM_PROGRAM=tmux` and `TMUX` set), the default chooses it, and
each Neovim window gets a tmux pane.

## Considered Options

- **Inline `sh -c` commands calling `tmux` directly.** They fit `find` and
  `close`, but can't turn tmux's layout into `report_layout`'s JSON, wait on
  events, or speak a socket, as zellij's and herdr's need.
- **Native providers, as Rex's runs inside the daemon.** No table to copy,
  so a user can't override one capability and keep the rest; and each would
  need the daemon's loop, as Rex's connection does.
- **A helper binary of its own.** Another file to install and keep in step;
  `{exe}` is always the binary the daemon runs.

## Consequences

- A new whole-word placeholder, `{exe}` (`<NAME>_EXE`), the absolute path of
  the daemon's binary, is given to every capability.
- `provider builtin NAME CAPABILITY` is hidden, dispatched before anything
  else, and its inputs are only the documented `<NAME>_*` variables and the
  choosing attach's environment; the capability word wins over
  `<NAME>_CAPABILITY`. Its four words, inputs, outputs and exit statuses
  are frozen, as `docs/capabilities.md` says; the commands it runs and its
  stderr are not. An unknown name, capability or extra word exits 69, so an
  older binary refuses a newer table's capability as unsupported. Run by
  hand without the variables, it exits 2.
- A table's capability for a version of the multiplexer that lacks it
  exits 69; `find` refuses a version below the built-in's floor, naming it.
  `<name>.builtin.NAME()` runs nothing, so its table is the same everywhere.
- A config that returns `"tmux"` is an error that points at
  `<name>.builtin.tmux()`: only Rex's built-in is a name.
- The core still knows no provider: the daemon reaches a built-in table as
  any table, through the command runner.
