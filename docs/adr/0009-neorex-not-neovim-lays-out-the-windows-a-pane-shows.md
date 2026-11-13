# neorex, not Neovim, lays out the windows a pane shows

Amended by ADR 0012 before implementation: a window pane keeps a pane layout
for each tab page whose windows it holds and shows one at a time; a tab page
that came into a pane showing another stays whole in it.

Amended by ADR 0013 before implementation: a late terminal is refused,
closed by its helper exiting as well as by `close`.

Amended by #92 before implementation: windows at the first attach get
terminals of their own where they can.

Amended by #107: with `'equalalways'`, a split or close evens every window
across the pane in the split's direction, as Neovim does, rather than only
the row or column split. A size Neovim gives a window is kept as its share
of the pane.

Amended by #138: a statusline neopane draws, at `'laststatus'` 0, shows the
text and highlights Neovim would draw it with. Neovim evaluates it after each
redraw, and when an option, the mode, the current window or buffer, or a
buffer's modified flag changes. Text that changes only with time, such as a
clock, waits for the next of those, as does an option set inside an
autocommand that isn't nested, which fires no `OptionSet`.

A window that can't have a terminal of its own is drawn in its origin pane,
laid out as Neovim lays out splits (#83, #88). That happens with no terminal
provider, when `open` fails or no attach comes in time, for an origin out of
reach, for a window opened without being entered when the daemon can't
focus the current window's terminal again, and for a later window of a tab
page drawn in a pane over another (ADR 0012). Neovim's own layout of a tab page
serves one screen, and panes in separate terminals each need their own. So
every window a pane shows stays external, and each window pane keeps its own
pane layout: rows and columns of its windows, which neorex splits, closes,
resizes and draws as Neovim would.

## Considered Options

- **Neovim's own layout for a lone pane.** While one pane is attached with no
  pane provider, its windows would stay in Neovim's layout and the pane would
  draw Neovim's whole screen: exactly plain Neovim, tab pages, `:botright`
  and `winlayout()` included. Pane layouts are still needed whenever there
  are more panes, so this would be a second way to draw the same windows,
  with a conversion each way as a second terminal attaches and goes. It is
  not ruled out, and can come later on top of pane layouts.
- **Neovim's own layout in one chosen pane among several.** Only that pane
  could show splits, and Neovim's screen would be pinned to it rather than to
  the current pane (ADR 0008).
- **Neovim's resize rule**, where the bottom or right window takes the change
  first. Exact, but every shrink crushes the last window and nothing gives it
  back, and the `VimResized` autocommands that even windows out run
  `:wincmd =`, which ignores external windows.
- **One window at a time in a pane with no room**, as tabs. A new kind of
  view with nothing like it in Neovim.
- **Handing a window drawn in a pane to a terminal that attaches late.** It
  would move a window the user may be typing in, perhaps to another monitor,
  and #83 gives a late terminal only a window no terminal shows.

## Consequences

- From the start, a pane layout follows these rules:
  - A new window takes half its origin window's place, on the side and in
    the direction Neovim chose (`'splitbelow'`, `'splitright'`, `:aboveleft`,
    `:belowright`, `:vertical`). With `'equalalways'`, a split or close then
    evens every window across the pane that way, as Neovim's `win_equal`
    evens its whole tab page, not only the row or column split.
  - A closing window's space goes to the next window, or with `'splitbelow'`
    or `'splitright'` to the previous one.
  - A size Neovim gives a window (`:resize`, `<C-w>+`, `<C-w>_`, `<C-w>|`)
    is kept, as its share of the pane, and its neighbours give or take the
    difference.
  - Side-by-side windows get a separator, drawn with `'fillchars'` in
    WinSeparator.
  - A window with another below it always has a statusline, whatever
    `'laststatus'` says. Where Neovim draws it none, neopane draws one, as
    Neovim would draw it, evaluated by Neovim only while some window has one.
  - `<C-w>h`, `j`, `k` and `l` move within the pane first, then to the pane
    that way when the layout provider can focus in a direction.
  - A click goes to the window under it.
- Later, each on its own: `<C-w>=` (Neovim's ignores external windows, so
  neorex maps it), `'winfixheight'` and `'winfixwidth'`, `'winminheight'` and
  `'winminwidth'`, `<C-w>H`, `J`, `K`, `L`, `r`, `R` and `x`, and dragging a
  separator or statusline.
- Never:
  - `:topleft` and `:botright`, which split beside the origin window: with
    an external window current, Neovim splits its last ordinary window
    either way, so the two can't be told apart from a plain split.
  - A split's count.
  - `:wincmd =` run as a command.
  - `winlayout()` and the like, which see only the hidden anchor windows.
- A pane with one window is drawn and sized as before, so Rex panes lose
  nothing. Under Rex a pane shows several windows only as a fallback.
- A window is never refused for want of room: it already exists, and Neovim
  checks for room against its own screen, not the pane. A pane too small for
  its windows squeezes them to Neovim's floor, a statusline with no text or a
  column and its separator, with the current window keeping a row. Past
  that, the windows farthest from the current one are cut off at the pane's
  edge, and show again when they become current.
- When a pane's terminal resizes, its windows keep their shares of it.
  Windows at their floor stay there, and get their earlier shares back when
  the pane grows.
- Windows of the current tab page at the first attach, such as those `-o` or
  `-O` open, other than the one the choosing attach's pane shows, wait unshown
  for the pane provider to be chosen, then get terminals of their own as any
  new window does, opened from the choosing attach's pane in the direction
  Neovim's layout has them, so Rex still gives each its own Rex pane. Only
  where they can't are they laid out in that pane as Neovim had them. Other
  tab pages, as from `-p`, get their place when first current (ADR 0012).
- A window waits for its requested terminal for the terminal provider's
  attach timeout (ADR 0007), 5 s by default from `open` finishing, Rex's
  included. Then it is drawn in its origin pane, with a warning. A terminal
  that attaches after that is refused: its helper exits quietly, which closes
  it, and `close` runs too when `open` printed an id (ADR 0013).
- A pane ends when its last window closes, whichever tab page it's in: a
  requested terminal is closed, a self-attached one goes back to its shell.
  Its terminal hanging up closes every window it holds, shown or not (#83).
  Windows drawn in a pane stay there when a pane provider appears later (ADR
  0007). Moving one into a terminal of its own is decided elsewhere.
- A pane keeps a pane layout for each tab page whose windows it holds and
  shows one at a time, that of the window last current in it, so the current
  pane shows Neovim's current tab page (ADRs 0011, 0012).
