# Neovim's screen is the size of the current pane's layout area

Amended by ADR 0010 before implementation: a float group with no float group
pane stays in the window pane that was current when one of its floats last
became current, rather than following the current pane. While it is current
and too big for the current pane, Neovim's screen is the current pane's own
size. A float group pane's bounds are always measured from the current pane.
"Floating pane" below now reads float group pane.

Amended by ADR 0012 before implementation: a tab page area that is a layout
area of its own, such as a tmux window, has its own tabline panes; a window
pane in a floating terminal, such as one in a tab page's Rex floating pane,
is never a tabline pane. By ADR 0011, the current pane's window's home tab
page is always Neovim's current tab page.

Panes need not share a window any more: a terminal provider alone opens
separate terminal windows, on different monitors and at different cell sizes,
and a pane out of the pane provider's reach shares nothing with the rest
(#88). So the Rex window of ADR 0001 becomes one kind of layout area: the
space a group of Neovim panes share, as a live layout reports it. With no
live layout, and for a pane out of reach or not in the live layout, each pane
is a layout area of its own. Neovim's screen is the size of the current
pane's layout area, counted in the current pane's cells, and changes when the
current pane moves to an area of another size. The command line, messages
and floats placed on the screen are shown only in the current pane, so this
is the one size at which all of them are seen.

## Considered Options

- **The largest area.** It changes only as terminals attach and go, but the
  command line and messages wrap at `'columns'`, so in a smaller pane a long
  command runs past its right edge unseen, and a picker sized from
  `'columns'` is cut to its top left.
- **The smallest area.** Everything fits everywhere, but one small terminal
  shrinks the pickers and command line of every big one, and the size still
  changes as terminals attach.
- **Resizing only once the current pane has settled.** The key that makes a
  pane current waits until Neovim has switched windows, and the resize goes
  with the switch, so a picker that key opens is already sized for its pane.
  A delay would size exactly that picker for the old area.
- **The tabline only in the current area.** Windows elsewhere would grow and
  shrink a row on every focus move.
- **The tabline fitted to the narrowest area.** Every pane would see every
  label, but the current pane's tabline would end short, and attaching a
  small terminal would change it.
- **Rects in cells or pixels in the live layout.** Cells differ from terminal
  to terminal and pixels mean nothing to tmux or herdr, while the daemon only
  ever needs shares.

## Consequences

- Within an area ADR 0001 holds: the screen spans the area, the tabline is
  fitted rather than the screen shrunk, and every mapping between grid 1 and
  the area (pane origins, floating-pane bounds, mouse input) is a scale of
  it. The area's size in cells is the current pane's cells over its share of
  the area. So an area whose terminals have different cell sizes is measured
  in the cells of the one everything is drawn in, and moving between two of
  them resizes the screen.
- The screen is resized with the window switch that makes a pane current,
  and only when its size changes, or shrunk to the current pane while a float
  group too big for it is current with no float group pane (ADR 0010). A switch already runs WinLeave and
  WinEnter; VimResized is one more. Under ext_multigrid Neovim never shrinks
  external windows to its screen, so a smaller screen leaves windows in
  bigger panes as they are.
- A focus report alone makes its pane current only once it has stood for
  80 ms with no focus report from another pane since. A key, paste or click
  makes a pane current at once. So cycling through terminal windows switches
  Neovim's current window, and resizes its screen, once, where you stop.
  Focus a pane provider reports, such as Rex's, is taken as before. A
  terminal that sends no focus reports becomes current at its first key or
  click.
- The window panes along the top of each area are its tabline panes, so a
  pane that is its own area always is one. A window pane in a floating
  terminal, such as one in a tab page's Rex floating pane, never is: its
  area's tabline shows above it. That depends only on the pane's own area,
  never on which pane is current or which tab page it shows, so no window
  gains or loses a row when focus moves or a tab page changes. The tabline is laid out and fitted (ADR 0002) to the
  current pane's area, and `g:neorex_tabline_width` follows it. Panes in
  other areas show the same row from its start, cut at their width or blank
  past it.
- The command line, messages and floats placed on the screen are drawn in
  the current pane only, except float groups with no float group pane, which
  stay where they were last current (ADR 0010). Every pane keeps `'cmdheight'` rows clear at its
  bottom, and tabline panes keep their top row. Nothing else of the screen
  is shown.
- A float group pane's bounds are cells relative to the current pane, with
  fractions of its area added when it is in a live layout. Floats tied to a
  window never get one (ADR 0010).
- `report-layout` gives each terminal's rect as fractions of its layout area,
  as Rex gives its panes'. A layout provider that measures cells or pixels
  divides by the area's size (ADR 0006).
- When a live layout arrives, the panes it reports join their areas: the
  screen may change size and a pane may stop being a tabline pane, but no
  terminal is moved and no window changes pane. A pane provider is dropped
  only once no terminal in its reach is attached, so dropping it changes
  nothing on screen. A `report-layout` that fails leaves the last layout in
  place; one found unsupported makes every pane its own area.
