# Every pane provider capability is an optional one-shot command

Amended by ADR 0007 before implementation: no capability is required, a
terminal provider may be paired with a layout provider, reach checks context
variables, and a terminal carries a pane title for `find` to match.

A pane provider gives the daemon terminals for Neovim's windows, and may place
and focus them (#86). Each thing it can do when the daemon asks is a
capability, and each is one command run once, so a user can write a pane
provider, such as Ghostty with AeroSpace, in a few lines with no long-running
process. No capability is required: `open`, which opens a terminal running the
attach command it is given, is what makes a pane provider a terminal provider,
and one without it only places and focuses terminals attached by hand (#87).
When a capability is missing or fails, that one act falls back as if there
were no pane provider: the window is drawn in its origin pane, its terminal is
left unplaced, or the next key switches focus back.

## Considered Options

- **JSON on stdin.** Structured, but every shell one-liner would need `jq`.
- **`open` placing the terminal when given an origin, if it can.** The daemon
  couldn't tell whether it did, so it would place every terminal again.
- **A `capabilities` command or table.** It goes stale against the commands;
  smart-splits.nvim dropped its own.
- **`open` required.** A layout provider alone would be a mistake in config,
  yet a pane provider whose `open` turns out unsupported is in that state
  anyway, and placing and focusing terminals attached by hand is worth having.
- **`focus` required too.** It shuts out terminal providers with no window
  manager to ask, and without it the next key already brings focus back.
- **A new terminal for every window, whatever pane it came from.** For a pane
  in another context, such as another Rex session, it opens where nobody is
  looking.
- **Finding a window only by its terminal's process.** Many terminals run
  every window in one process, so a layout provider paired with one couldn't
  tell its windows apart; a title works with any terminal that shows it.

## Consequences

- The capabilities are `open` and `close`, the terminal provider's; `find`,
  `place-beside`, `resize`, `focus`, `focus-direction`, `report-layout` and
  `events`, the layout provider's; and `open-beside` and `open-float`, which
  open and place in one step for a pane provider that is both in one. Without
  them, as always for a terminal provider paired with a layout provider, the
  daemon opens the terminal, then places it once it attaches. A capability is
  added only when a pane provider needs it.
- A command is an argv run without a shell. Its inputs are `<NAME>_*`
  variables and whole-word placeholders such as `{terminal}`; `{attach}` is the
  attach command's words, and `<NAME>_ATTACH` is the same as one string any
  shell reads, for a pane provider that types it. It gets stdio only, the
  environment of its pane provider's choosing attach, and never `$<NAME>`.
- Exit 0 is done; stdout holds a terminal id, or JSON from `report-layout`;
  stderr goes to the log. Exit 69, or a command that can't start, means
  unsupported until the pane provider is chosen again; any other failure, or a
  timeout, fails that act alone, and nothing is retried.
- A terminal id comes from `open`'s stdout or from `find`, which is given the
  attach's pid, tty, pane title and the variables the pane provider names: the
  daemon never knows a pane provider's own variable names. In a pair, the
  terminal provider's id from `open` serves only `close`, and the layout
  provider's `find` runs for every attach, even when `open` printed an id; its
  id serves the rest. Ids are opaque, and the daemon never acts on a pane whose
  attach has ended; a pane provider whose ids can be reused puts what proves
  them current into the id.
- Before the first `find` command runs for an attach, the daemon writes a pane
  title into the terminal's title: `<name>-` and a token unique to the pane,
  sent as ordinary output, so it works over a relay too. `open` gets it as
  `{title}` and `<NAME>_TITLE`, so a terminal provider can make it the
  window's first title rather than rely on the terminal honouring the escape
  code. `find` gets it as `<NAME>_TITLE` and matches a window whose title
  contains it, retrying for a while, since a window manager may not list a new
  window at once. It has to last only until `find` answers. The terminal's own
  title is saved first, and put back, or cleared where it can't be, when the
  pane hangs up. Rex's in-process `find` doesn't need a title, so Rex panes
  never get one.
- A terminal provider paired with a layout provider must open each terminal in
  a window of its own: a terminal in a split or a tab is found, focused and
  placed as its whole window.
- A terminal is in a pane provider's reach if the pane provider made it, it is
  the choosing attach's, or it passes every check the pane provider has: its
  context variables, which must have the values the choosing attach had, then
  `find`; with neither, every terminal is. Context variables name what the
  commands act within, such as a Rex session; tmux, whose commands reach every
  session of a server, has none and checks the terminal's tty in `find`. A
  pane provider whose variables leak into terminals it doesn't run needs a
  `find`. A window opened from a pane out of reach is drawn in that pane.
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
  as it is; changing the meaning of an existing one bumps it. Each half of a
  pair agrees its own number. Commands ignore variables they don't know, and
  the daemon ignores unknown JSON fields and, with a warning, unknown
  capabilities, as an older running daemon reading a newer config must.
- Built-in pane providers, Rex's first, offer the same capabilities
  in-process. Rex's is only ever used whole, never as half of a pair.

Research: `docs/research/rex-capabilities.md`, `tmux-pane-provider.md`,
`herdr-pane-provider.md`, `aerospace-ghostty-pane-provider.md`,
`pane-provider-prior-art.md`, `pane-provider-capabilities.md` and
`pane-title.md`, each on its `research/` branch.
