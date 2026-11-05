# A pane provider only has to open a terminal; everything else it can do is an optional one-shot command

A pane provider gives the daemon terminals for Neovim's windows, and may place
and focus them (#86). Each thing it can do when the daemon asks is a
capability, and only one is required: `open`, which opens a terminal running
the attach command it is given. The rest are present when configured, and each
is one command run once, so a user can write a pane provider, such as Ghostty
with AeroSpace, in a few lines of shell with no long-running process. When a
capability is missing or fails, that one act falls back as if there were no
pane provider: the window is drawn in its origin pane, its terminal is left
unplaced, or the next key switches focus back.

## Considered Options

- **JSON on stdin.** Structured, but every shell one-liner would need `jq`.
- **`open` placing the terminal when given an origin, if it can.** The daemon
  couldn't tell whether it did, so it would place every terminal again.
- **A `capabilities` command or table.** It goes stale against the commands;
  smart-splits.nvim dropped its own.
- **`focus` required too.** It shuts out terminal providers with no window
  manager to ask, and without it the next key already brings focus back.
- **A new terminal for every window, whatever pane it came from.** For a pane
  in another context, such as another Rex session, it opens where nobody is
  looking.

## Consequences

- The capabilities are `open` and `close`, the terminal provider's; `find`,
  `place-beside`, `resize`, `focus`, `focus-direction`, `report-layout` and
  `events`, the layout provider's; and `open-beside` and `open-float`, which
  open and place in one step for a pane provider that is both. Without
  `open-beside`, the daemon opens the terminal, then places it once it
  attaches. A capability is added only when a pane provider needs it.
- A command is an argv run without a shell. Its inputs are `<NAME>_*`
  variables and whole-word placeholders such as `{terminal}`; `{attach}` is the
  attach command's words, and `<NAME>_ATTACH` is the same as one string any
  shell reads, for a pane provider that types it. It gets stdio only, the
  environment of the attach that chose its pane provider, and never `$<NAME>`.
- Exit 0 is done; stdout holds a terminal id, or JSON from `report-layout`;
  stderr goes to the log. Exit 69, or a command that can't start, means
  unsupported until the next first attach; any other failure, or a timeout,
  fails that act alone, and nothing is retried.
- A terminal id comes from `open`'s stdout or from `find`, which is given the
  attach's pid, tty, title and the variables the pane provider names: the
  daemon never knows a pane provider's own variable names. Ids are opaque, and
  the daemon never acts on a pane whose attach has ended; a pane provider whose
  ids can be reused puts what proves them current into the id.
- A terminal is in a pane provider's reach if the pane provider made it, its
  attach chose the pane provider, or `find` counts it; with no `find`, every
  terminal is. A window opened from a pane out of reach is drawn in that pane.
- A window opened without being entered gets a terminal only when the daemon
  can focus the current window's terminal again; otherwise it is drawn in its
  origin pane, as plain Neovim draws `:pedit`.
- `close` is called whenever the daemon is done with a requested terminal, so
  it must succeed on one already gone. A self-attached terminal is never
  closed. A pane's helper never exits within 300 ms of starting, so a terminal
  that treats a quick exit as a crash, as Ghostty does, still closes.
- Focus requests have a queue of their own, so a slow `open` never holds up
  `<C-w>w`.
- New variables, optional capabilities and JSON fields leave `<NAME>_PROTOCOL`
  as it is; changing the meaning of an existing one bumps it. Commands ignore
  variables they don't know, and the daemon ignores unknown JSON fields and,
  with a warning, unknown capabilities, as an older running daemon reading a
  newer config must.
- Built-in pane providers, Rex's first, offer the same capabilities in-process.

Research: `docs/research/rex-capabilities.md`, `tmux-pane-provider.md`,
`herdr-pane-provider.md`, `aerospace-ghostty-pane-provider.md`,
`pane-provider-prior-art.md` and `pane-provider-capabilities.md`, each on its
`research/` branch.
