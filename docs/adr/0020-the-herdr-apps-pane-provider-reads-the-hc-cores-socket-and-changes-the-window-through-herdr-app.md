# The Herdr app's pane provider reads the hc core's socket and changes the window through herdr-app

The built-in `herdr-app` pane provider (ADR 0019) speaks to two things:
the hc core, over its JSON-RPC socket, for what it reads and for the
terminals themselves (`hello`, `terminal.get`, `terminal.list`,
`terminal.kill`, the core's `terminal.keep`, `doc.get herdr-app`), and the
Herdr app window, through the `herdr-app` command, for every change to
the window (`pane.split`, `pane.swap`, `pane.focus`, `pane.go`, `tab.new`,
`layout.resize`, `status`), in modules of their own (`core.rs`,
`window.rs`). Each does what the other can't: the core knows terminals,
their processes, sizes and the doc, the app's splits, tabs and spaces,
but has no focus or zoom, which are a window's; `herdr-app do` is the
app's documented way to act, and answers only with the ids it made.

`open_beside` is a `pane.split`, which, unlike herdr's (ADR 0017), runs an
argv: the attach command, with no shell, in a new pane beside the
origin's, right or down. For left and up, `pane.swap` then swaps it with
the origin's pane, its neighbour that side, the focus staying with it.
The split's focus stands: the new pane takes it, as a Neovim split takes
the current window, and the daemon gives it back when it should.

- **Keep off.** The core keeps a terminal its app makes (`keep: true`):
  lost, as by a signal with no exit code, it comes back as a shell. An
  attach killed so would leave a shell where a Neovim window was, and one
  kept across a core restart a pane nobody wants. So each terminal the
  pane provider opens gets the core's `terminal.keep {keep: false}` at
  once; only those, never the user's, and a failure only warns, as the
  terminal works without it.
- **The doc is the map.** The split answers the new pane and terminal,
  but the core's doc is what later capabilities read: the new terminal
  must show there in one block, waited for up to a second, else it is
  killed and the open fails. A split refused, run out of time, or whose
  answer names no terminal fails with nothing to kill: a pane it made all
  the same runs an attach whose pane id the daemon gave up, which is
  refused and exits, and the pane closes with it. A swap failing only
  warns, leaving the pane right or below. They share 4 s of the open's
  5 s; a kill has 0.5 s more, on a connection of its own.
- **Sizes are weights, set on pane ids.** A block's `size` is a weight
  relative to its siblings, or pixels; `layout.resize {id, weight}` on the
  pane ids of the doc sets it, on any node, an unshown tab's too
  (SPI-1218). Weights share a pane's outer extent, chrome included, which
  the core's cols and rows leave out.
- **No zoom.** The app's zoom is in neither the doc nor `status`, only its
  `view.json`, so it is not reported: while a tab is zoomed, Neovim's
  screen is laid out as if it weren't. A split into a zoomed tab unzooms
  it.
- **Two `terminal.keep`s.** The core's `terminal.keep {terminal, keep}` is
  the keep above. `herdr-app do terminal.keep {terminal, place}` is
  unrelated: it gives a running terminal with no pane one, a new tab or
  right or down of the focused pane. The pane provider calls only the
  core's.

## Considered Options

- **Everything through `herdr-app`.** It has no `terminal.get`, `doc.get`
  or keep, and a process per read: `ls` answers ids, not the doc's
  weights or a terminal's process.
- **Everything through the core, writing the doc (`doc.set`).** The doc
  is the app's to write; a terminal the core runs shows nowhere until the
  app puts it in a pane, and focus is the window's.
- **`terminal.run` on the core, then placing it** (`herdr-app do
  terminal.keep {terminal, place}`), or `herdr-app do terminal.open
  {place}`. Both place beside the focused pane, not a pane named, and the
  first is two steps where the split is one, leaving a terminal shown
  nowhere if the second fails.
- **Keep on**, the app's default. An attach killed by a signal would come
  back as a shell in its pane, and every Neovim window again after a core
  restart.
- **`pane.swap {target}`** with the origin's pane, rather than
  `{direction}`. Only `direction` was recorded, with the focus staying on
  the pane swapped; right after a split the origin's pane is the one
  neighbour that side either way.

## Consequences

- `open_beside` needs `doc.get`, the core's `terminal.keep` and
  `terminal.kill`; lacking one, or `herdr-app` lacking `pane.split` or its
  `argv`, it exits 69 naming it and both versions, as `close` does. No
  `herdr-app` exits 69 too, naming both places it looked.
- The new pane halves the origin's room until `resize` sets weights
  (SPI-1218).
- A split in a never-shown tab gives both panes the core's 120×40 until
  the tab is shown: a hidden tab's cols and rows are not to be trusted.
