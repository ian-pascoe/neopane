# Neovim's current tab page follows the current pane

Every window a pane shows is external, and Neovim keeps external windows in
whichever tab page is current, so the current tab page changed only through
`:tabnext` and the like: going to another tab page's pane left it behind,
and `tabpagenr()`, `:tcd`, `t:` variables, `'cmdheight'` and the tabline's
highlighted label belonged to a tab page the current window wasn't in (#67).
A pane can also show several tab pages (ADR 0012), and then needs one meaning
of current. So Neovim's current tab page is always the current window's home
tab page, as in plain Neovim: when a pane becomes current, or a window
becomes current inside Neovim, whose home tab page isn't current, neorex
makes that tab page current, then that window. A float group with a float
group pane is independent of tab pages, as Neovim makes external windows: it
moves into whichever tab page is current (#90).

## Considered Options

- **Never follow**, as before. No autocommands on a pane change and floats
  stay put, but the current window and tab page disagree, and a pane showing
  two tab pages would need a current tab page of its own.
- **Follow only for a tab page with a tab page area.** A pane showing two
  tab pages disagrees the same way, and `:tcd` would mean different things
  under different pane providers.
- **Switch with `:noautocmd`.** Cheaper, but plugins that keep state per tab
  page, such as Diffview and bufferline, would miss the change.
- **Every float follows the current tab page.** Notifications would stay up
  across tab pages, but in a lone pane `gt` would keep one tab page's floats
  over another's, unlike plain Neovim.
- **Float group panes left to Neovim**: hidden, frozen, or closed and opened
  again on every crossing. A floating toggleterm in a Rex floating pane would
  stop updating, or flash, whenever the user clicked another tab page's pane.

## Consequences

- The switch is one request, so Neovim redraws once:
  `nvim_set_current_tabpage`, the float groups with float group panes moved
  in, then `nvim_set_current_win` if the tab page's last current window
  isn't the pane's. Neovim does the rest: BufLeave, WinLeave and TabLeave;
  the tab page's own `'cmdheight'`, with OptionSet; external windows moved
  into the new tab page and the old one's other floats hidden; WinEnter,
  DirChanged where `:tcd` or `:lcd` differ, TabEnter and BufEnter; every
  window redrawn. The daemon takes the external windows Neovim announces
  again, and the redraw, as nothing new; every pane repaints once.
- There is at most one switch per key, click or settled focus report (ADR
  0008's 80 ms), so cycling through terminals switches once, where it stops,
  and an autocommand that goes to another tab page's window can't start a
  loop. No `focus` is sent for a pane already current.
- `:tabnext`, `gt`, a tabline click or a tab page closing makes current the
  tab page's last current window in the current pane if it has one there,
  else its last current window anywhere, and focuses its pane with `focus`,
  which reaches terminals in other tmux windows, herdr tabs and workspaces
  (tmux's is `switch-client`: `select-pane` doesn't change window). Without
  `focus`, the next key from the old terminal takes the current pane back
  (ADR 0006). An anchor, or another tab page's window, never stays current.
  The last current window is the one reported current, or current as its
  tab page was left, as Neovim keeps a tab page's. Neovim's own pick stays
  if it is in the current pane or in no pane yet, and a window made current
  by focusing its pane, or by `<C-w>w`, is never redirected (#118).
- A window made current inside Neovim (`<C-w>w`, `:wincmd p`, a plugin's
  `nvim_set_current_win`) whose home tab page isn't current switches tab
  page, as plain Neovim does for a window in another tab page. `<C-w>w`,
  `<C-w>W` and `<C-w>p` step only through windows panes are showing, so a
  lone pane cycles its own tab page, and Rex still steps into a tab page's
  Rex floating pane: they are mapped, in Normal and Visual mode, which a
  step ends as entering another window does, and keeps with nowhere to go.
  `:wincmd w`, `:wincmd W` and `:wincmd p`, which no mapping sees, step in
  Neovim's own order; one that lands on an anchor moves on to a window a
  pane shows.
- A float group with a float group pane is moved, every float of it, into
  the tab page being entered (`nvim_win_set_config` naming a window of that
  tab page, such as its anchor; Neovim 0.12), before anything is drawn. Its
  float group pane never blanks and works from any tab page: a key or click
  in it makes its float current with no switch. It still belongs to the tab
  page it opened in, and so does a float that joins it away from home, as a
  preview opens beside a picker's list: the tabline counts it there, it
  closes when that tab page closes, and it is moved back before the tab
  page it sits in closes. Neovim allows no move as a tab page starts to
  close (TabClosedPre gives E1312), so it's moved back as the first of that
  tab page's windows closes, before the rest of its floats do; a close that
  then fails should bring it back (untested: a failing `:tabclose` of a
  tab page holding a modified external window hangs Neovim). The
  exception is a float of it that is the current window: Neovim closes
  that one first, as it closes the current float of any tab page that
  closes, and the rest of the group goes home. A close that fires no
  WinClosed or TabClosedPre, as `:noautocmd tabclose` or an
  `'eventignore'` holding them, closes the whole group with the tab page.
  Where Neovim can't move floats between tab pages, its float group pane
  keeps its last frame until its tab page is current again, and a key or
  click in it makes it current.
- Every other float stays in its tab page, hidden while that isn't current,
  as `gt` hides it: float groups drawn in a pane (ADR 0010), floats on
  windows, and screen floats never entered, such as notifications. They come
  back with their tab page, a group in the pane it was last current in.
- `tabpagebuflist()`, `tabpagewinnr()`, `nvim_tabpage_list_wins()` and
  `winlayout()` still see every external window in the current tab page, and
  a moved float group in the tab page it sits in (#67).
- Rex users see TabLeave and TabEnter, and a tab page's notifications hide,
  when they move between panes of different tab pages, as on `gt`; `gt` goes
  to the tab page's own pane, not whichever window was current when it was
  last left.

Research: `docs/research/tab-page-places.md`, on its `research/` branch.
