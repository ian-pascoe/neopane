# herdr's pane provider places through a tab of its own and sizes by its split ids

herdr's pane provider moves a terminal beside another as its open does
(ADR 0017), and sizes it with `layout.set_split_ratio`, finding the split
from herdr's undocumented split ids, read strictly (SPI-1140).

`place_beside` and `open_beside` share one move: `pane.move` beside the
origin, a `pane.swap` for left or up, since herdr only splits right or
down, then the tab's root split set to the ratio it has, twice, 100 ms
apart. The server's focus stays on the pane it was on: the move focuses
the moved pane only if it had the focus, and the swap's source, which herdr
focuses, is the moved pane if it had the focus, else the origin, the focus
then put back if it was elsewhere. An open's new pane never has the focus,
so its move is `focus:false` and its swap's source is the origin.

herdr refuses a move within one tab (`same_tab`), so `place_beside` from
the origin's own tab takes two moves: out to a new tab of the workspace,
`focus:false`, then beside the origin. A zoomed tab, the origin's or the
terminal's, is refused before anything moves, and so is a terminal in
another workspace, which a move would rename. With `SIZE`, it then sizes
the terminal as `resize` does, as tmux's does.

`resize` reads the tab with `pane.layout` and rebuilds its split tree from
the split ids, `split_<n>_root` or `split_<n>_<bits>`: `<n>` the split's
place in pre-order, the bits its `layout.set_split_ratio` path. It fails
closed: an id of any other form, or splits and panes that aren't where
their ids, their pre-order numbers and herdr's own rounding of each ratio
put them, exit 1, setting nothing. The lowest split holding the terminal
and its origin gets the ratio that gives the terminal's part `SIZE` cells
of content and a frame of two cells for each pane across it.

## Considered Options

- **`pane.swap` alone within a tab.** It moves a pane only to where
  another one is, so it can't give every arrangement; the round trip
  through a tab can.
- **Rolling back a second move that fails**, moving the terminal back
  where it was. That is a third move, which can fail too, and herdr has
  just refused the second; the terminal is left in its tab of its own, and
  the command says where.
- **Finding the split by its rect alone**, the smallest holding both panes.
  The path `layout.set_split_ratio` takes would still come from the id;
  checking the whole tree against the ids is what catches a herdr that
  means something else by them.
- **`layout.export`'s tree** gives the paths without ids, but no rects:
  the split's cells would come from rounding ratios down from the area,
  herdr's arithmetic all the same.
- **`pane.resize`**, which is relative, by steps.

## Consequences

- The tab a move within a tab passes through may show in herdr's tab bar
  for a frame, as the open's does.
- A terminal whose second move fails stays in its tab of its own until the
  daemon places the tab page again, which it does only once the tab
  page's windows change, as it counts a placing done once asked
  (SPI-1194).
- A herdr that changes its split ids, or the rounding of a split, makes
  every `resize` and sized `place_beside` exit 1, changing nothing, until
  the built-in follows it.
- herdr frames each pane, two cells along a split, where Neovim has one
  separator between two windows: in a row of two, the pane the daemon
  sizes gets its columns, and the other three fewer than Neovim gave its
  window (SPI-1193).
