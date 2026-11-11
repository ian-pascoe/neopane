# A tab page gets a tab page area from `open_tab_page`, else stays whole in the pane it opened in

Amended by #92 before implementation: the first attach's other windows get
terminals of their own where they can (ADR 0009).

A tab page made after startup has always had a Rex floating pane; other pane
providers may have a tmux window or a herdr tab for it, or nothing, and any
request may fail (#90). Plain Neovim shows one tab page at a time on its
screen. So when a tab page first becomes current it gets a tab page area
through `open_tab_page`, an optional capability that opens its first
window's terminal somewhere apart: a tmux window, a herdr tab, Rex's floating
pane. Otherwise, with no pane provider, no `open_tab_page`, an origin out of
reach or a failure, it is drawn whole in the pane it opened in. A window
pane keeps a pane layout for each tab page whose windows it holds and shows
one at a time, that of the window last current in it, so `gt` swaps it as
plain Neovim swaps its screen. Tab pages from `-p` or a session wait for
their place until visited.

## Considered Options

- **A tab page's windows as ordinary windows beside their origin**, each
  with a terminal of its own. Every window always shows, but `:tabnew`
  splits your pane, Diffview squeezes in beside your code and `gt` hides
  nothing. ADR 0010 likewise draws a float group in a pane rather than tiling
  it.
- **Every pane showing only the current tab page.** Plain Neovim for one
  pane, but clicking into another pane would swap the panes you left, and a
  pane with none of the current tab page's windows would go blank.
- **A pane also showing the tab page of the window last opened in it.** A
  plugin opening a split into another tab page would swap a pane the user is
  typing in.
- **Terminals of their own for the later windows of a tab page drawn in a
  pane.** After `gt`, half the tab page would stay on screen beside the
  other.
- **A floating terminal per tab page through `open_float`, for every pane
  provider**, as Rex's. `open_float` opens a float group pane, never current,
  at a group's bounds, while a tab page's place holds window panes that are
  current and split further, and not every floating terminal can be split.
  tmux would get a float over its own window where a tmux window fits.
- **`<NAME>_KIND=tab-page` on `open`.** The daemon couldn't tell whether
  `open` honoured it (ADR 0006).
- **A place for every tab page at the first attach.** Natural in tmux, but
  Rex would stack Rex floating panes, Ghostty would open a window per tab
  page, and a session's tab pages would all open at once. Tiling them all in
  the first area, as Rex did, tiles every window of a session into one Rex
  window.
- **A layout-side `place_tab_page` now.** AeroSpace could move a pair's
  terminal to a workspace of its own, but no built-in needs it yet.

## Consequences

- `open_tab_page` is one step, like `open_beside` and `open_float`, so it
  comes only from a pane provider returned whole, never from a pair's
  halves. It is given `{attach}`, `{title}` and the origin pane's terminal,
  opens a terminal running the attach command in a new tab page area next to
  the origin's (the same Rex window, tmux session or herdr workspace), and
  prints its terminal id; `find`, reach and the attach timeout apply as for
  any requested pane. tmux: `new-window -a -P -F '#{pane_id}'` after the
  origin's window, its id from `display -p` (`-t` takes no pane); herdr:
  `layout.apply` with an argv pane, over its socket; Rex: a Rex floating pane
  at [0.05, 0.05, 0.9, 0.9] of the origin's Rex window. No protocol bump.
- While it's on its way the tab page's first window is unshown, as any
  window waiting for its terminal. Exit 69 means unsupported until the pane
  provider is chosen again; any other failure, or a timeout, leaves that tab
  page drawn in a pane for its life, and the next asks again.
- A tab page area's later windows get terminals beside their origin pane, as
  any window does, beside another of the tab page's panes when the origin
  has none, and wait while its first terminal is on its way. Once each has
  one, the requested panes in the area are placed as Neovim lays out the tab
  page, with `place_beside` and `resize`, and again as that changes.
  Self-attached panes are never moved, nor what another is placed beside:
  their terminals may be anywhere.
- A tab page that came into a pane already showing another tab page stays
  whole in it: its later windows are drawn there, never in terminals of
  their own. Every other new window gets a terminal where the pane provider
  can give one (#83). So with Ghostty and AeroSpace, `:vsplit` opens a
  Ghostty window and `:tabnew` takes over the current one, and the Ghostty
  built-in has no `open_tab_page`: with no layout provider, a window per tab
  page would scatter its splits too.
- Only a window becoming current makes a pane show its tab page, so the
  current pane always shows Neovim's current tab page (ADR 0011), and another
  keeps what it showed and never goes blank. A window opened, not entered,
  into a tab page its pane isn't showing waits there until the pane shows
  it; it is held, not an unshown window.
- `:tabnew` and `:tab split` open a tab page's first window. `<C-w>T` does
  too, then closes the old window; neorex does it itself, as Neovim never
  refuses it with an anchor in every tab page, refusing it when the window
  is its home tab page's only one. The old window goes by the usual rules: a
  pane layout closes up, a requested terminal left empty closes, a
  self-attached one goes back to its shell.
- At the first attach, the choosing attach's pane shows the current tab page's
  current window; the tab page's other windows get terminals of their own as
  new windows do, or are laid out in that pane as Neovim had them where they
  can't (ADR 0009). Every other tab page gets its place when first current. A
  self-attach takes an unshown window, current first, then by tab page and
  window order, so a second terminal after `nvim -p a b c` shows `b` and
  becomes its place. There is no setup tab page.
- A tab page area that is a layout area of its own, such as a tmux window or
  a herdr tab, has its own tabline panes. A tab page's Rex floating pane
  isn't one: its panes float over the Rex window, whose tabline shows above
  them, and a window pane in a floating terminal is never a tabline pane
  (ADR 0008).
- A tab page closes with its last window, wherever that was; a tab page
  area closed from outside closes its windows as their terminals hang up
  (#83). `:tabclose` closes its windows, each requested terminal left empty
  through `close`, and the area goes with its last terminal; a pane drawing
  it shows the tab page it has left, or ends. No capability closes a whole
  area: it would kill the foreign and self-attached panes in it.
- Should a pair need it, `place_tab_page` is a layout-provider capability
  run once `find` has answered for a tab page's first terminal, which `open`
  opened with `<NAME>_KIND=tab-page`. It takes `{terminal}`, the origin
  pane's terminal and `<NAME>_TAB_PAGE`, a token unique to the tab page for
  the daemon's life to name a workspace by; `aerospace move-node-to-workspace
  --window-id {terminal} --focus-follows-window <name>` creates it, and an
  empty one goes by itself. On failure the terminal stays where `open` put
  it, an unplaced pane, and the tab page carries on from it; exit 69 means
  unsupported until the pane provider is chosen again. Until then neither
  exists, and a pair's tab pages are drawn in panes.
- Rex keeps its floating pane per tab page and its placement, and gets the
  tabline and screen size it had. Tab pages from `-p` or a session other
  than the current one now get a Rex floating pane on first visit, rather
  than tiling in the Rex window at startup.

Research: `docs/research/tab-page-places.md`, `tmux-pane-provider.md` and
`herdr-pane-provider.md`, each on its `research/` branch.
