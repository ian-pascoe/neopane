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
  refused, or hung up if it had attached, and exits, and the pane closes
  with it. A swap failing only
  warns, leaving the pane right or below. They share 4 s of the open's
  5 s; a kill has 0.5 s more, on a connection of its own.
- **Sizes are weights, set on node ids.** A node's `size` is a weight
  relative to its siblings, or pixels; `layout.resize {id, weight}` on
  the ids of the doc sets it, on any node, an unshown tab's too. Weights
  share a pane's outer extent, chrome included, which the core's cols and
  rows leave out. So `resize` converts cells to weights in
  `report_layout`'s model, with its chrome (SPI-1217): a pane's outer
  extent is its share of its tab, from the weights, times the tab's. The
  terminal's part of the lowest split holding it and `ORIGIN` is to have
  `SIZE` cells and each of its panes' chrome along the split, out of what
  that part and `ORIGIN`'s have together: their share of the tab times
  the tab's extent. Every tab of the window has the same room, so the
  shown tab's cells tell any tab's extent, a hidden one's too, whose own
  cells may be stale or the core's 120×40. The pair's ratio is what's
  set: where they are the split's only two, by one `layout.resize` of the
  terminal's part; else by both, keeping their sum, so the split's other
  children keep their room. As herdr's frame does (ADR 0018), the chrome
  goes to the terminal's part, so `ORIGIN`'s comes short of Neovim's by
  about a chrome per pane along the split. A part sized in pixels, or a
  node so in a split along the axis over the pair, is refused rather
  than guessed at, as the app takes pixels off first and the pair's
  share can't be told.
  `open_beside` gives the new pane `RATIO` of the weight it and the
  origin's pane have, chrome ignored, as a split only halves.
- **No zoom.** The app's zoom is in neither the doc nor `status`, only its
  `view.json`, so it isn't reported: while a tab is zoomed, Neovim's
  screen is laid out as if it weren't. `resize` reads `view.json` only to
  measure the window by the shown tab's zoomed pane alone, which fills
  the tab, the others' cells being stale. A split into a zoomed tab unzooms it, as
  `open_beside` does.
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
  the pane swapped. It assumes that right after a split the origin's pane
  is the new pane's neighbour that side: the split puts the new pane
  directly right of or below it, sharing that whole edge. Whether
  `pane.swap {direction}` picks the origin's pane where the new one had
  several neighbours that side, as `pane.go` picks the one focused last,
  wasn't checked live; after a split it has only the one.

## Consequences

- `open_beside` needs `doc.get`, the core's `terminal.keep` and
  `terminal.kill`; lacking one, it exits 69 naming it and both versions,
  as `close` does. It exits 69 too where `herdr-app` lacks `pane.split` or
  its `argv`, naming both versions, and where there is no `herdr-app`,
  naming both places it looked. `find` refuses without `herdr-app`, so
  that happens only if it went after the choice: 69 stops a doomed ask
  until the next choice. This narrows ADR 0019, which refused without
  `herdr-app` only in `find`; `focus` and `focus_direction`, which act
  only through the window, exit 69 without it, and on a `pane.focus` or
  `pane.go` the app lacks, the same way (SPI-1217). `report_layout`
  reports without a focus instead. A `pane.swap` the app lacks only warns.
- The new pane gets `RATIO` of the origin's room in weights, so its
  cells are off from Neovim's by up to a pane's chrome. A failed sizing
  only warns: the split's halves stand. `resize` gives the terminal its
  `SIZE` within rounding and the chrome's error, and `ORIGIN`'s part
  comes short of Neovim's by about a chrome per pane along the split.
- `resize` needs `report_layout`: the daemon sizes only panes a layout
  report places. Its chrome is `report_layout`'s, so the sizes it sets
  and the layout reported agree within rounding, but while the shown tab
  is zoomed, which only `view.json` says and `report_layout` doesn't
  read: then `resize` measures the window by the zoomed pane alone, its
  cells the whole tab's, with the measured chrome, while `report_layout`
  lays the tab out unzoomed with the chrome its cells solve.
- The tab's extent is taken along a line across the shown tab, the sum
  of the cells and chrome of the panes it crosses, not each pane's cells
  over its share: a line crosses the whole tab whatever the weights, so
  cells laid out by the weights before a `layout.resize` just sent, as
  the daemon sends a tab page's resizes one after another, tell it as
  well. Whether `layout.resize` answers before the ptys are resized is
  unchecked.
- A split in a never-shown tab gives both panes the core's 120×40 until
  the tab is shown, and one in a hidden tab leaves its panes' sizes
  stale: a hidden tab's cols and rows are not to be trusted, and neither
  `report_layout` nor `resize` reads them.
