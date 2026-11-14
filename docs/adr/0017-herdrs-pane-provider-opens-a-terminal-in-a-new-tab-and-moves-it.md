# herdr's pane provider opens a terminal in a new tab and moves it

herdr's pane provider opens a terminal beside another by making it in a new
tab with its argv and moving it, because herdr's split takes no command;
typing the command into a shell was rejected as shell-dependent and leaving
a shell behind. A moved pane's terminal keeps its old size until the tab is
laid out again, so the open ends by setting the root split to the ratio it
has (SPI-1138).

`open_beside` is `layout.apply` (a new tab, `focus:false`, in the origin's
workspace, its one pane running the attach command), `pane.move` beside
the origin, a `pane.swap` for left or up, since herdr only splits right or
down, then `layout.set_split_ratio` on the root split (`path: []`) with the
ratio it already has. herdr 0.9.5, protocol 22, is the floor.

## Considered Options

- **`pane.split`, then `pane.run "exec <attach>"`.** The new pane runs the
  user's shell first: a login shell on macOS, with every rc file, and the
  command typed into it, shown in the pane. It depends on the typeahead
  surviving rc files that read stdin or are slow, and on the shell having
  `exec`; a prompt plugin has been seen to kill the child. The tab move has
  none of this.
- **Waiting for argv on `pane.split`** (herdr discussions #1695 and
  #3141): no answer from herdr's maintainers yet. When it comes, the open
  becomes one split, and the move goes.

## Consequences

- The temporary tab shows in herdr's tab bar for one frame.
- A zoomed origin tab is refused before the tab is made; a zoom between
  the two requests makes herdr refuse the move, and the pane made is
  closed. Any failure after the tab is made closes its pane, with a
  deadline of its own, so a request that ran out of time still cleans up.
- The tab is labelled with the pane title, so a `layout.apply` that gets
  no answer can still close the tab it may have made. One herdr makes
  only after that closes itself: its attach is refused, the daemon having
  given its pane up, and ends.
- `pane.swap` focuses its source: the open passes the origin as the
  source, and puts the server's focus back if it was elsewhere.
- Setting the root split is done twice, the second 100 ms later: a
  terminal that went to the alternate screen before a client drew it in
  its new tab is otherwise left a column short, herdr keeping its
  scrollbar's column, until the tab is next laid out.
- Within its workspace, the moved pane keeps its id, which the open
  prints; across workspaces herdr would rename it.
