# neorex, not Neovim, lays out the windows a pane shows

A window that can't have a terminal of its own is drawn in its origin pane,
laid out as Neovim lays out splits (#83, #88). That happens with no terminal
provider, when `open` fails or no attach comes in time, for an origin out of
reach, and for a window opened without being entered when the daemon can't
focus the current window's terminal again. Neovim's own layout of a tab page
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
    `:belowright`, `:vertical`), and `'equalalways'` then evens that row or
    column.
  - A closing window's space goes to the next window, or with `'splitbelow'`
    or `'splitright'` to the previous one.
  - A size Neovim gives a window (`:resize`, `<C-w>+`, `<C-w>_`, `<C-w>|`)
    is kept, and its neighbours give or take the difference.
  - Side-by-side windows get a separator, drawn with `'fillchars'` in
    WinSeparator.
  - A window with another below it always has a statusline, whatever
    `'laststatus'` says.
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
- Windows opened at startup with `-o` or `-O` are laid out as Neovim had
  them.
- A window waits for its requested terminal for the terminal provider's
  attach timeout (ADR 0007), 5 s by default from `open` finishing, Rex's
  included. Then it is drawn in its origin pane, with a warning. A terminal
  that attaches after that is closed.
- A pane ends when its last window closes: a requested terminal is closed, a
  self-attached one goes back to its shell. Its terminal hanging up closes
  every window it shows (#83). Windows drawn in a pane stay there when a pane
  provider appears later (ADR 0007). Moving one into a terminal of its own,
  and which tab page's windows a pane lays out (#90), are decided elsewhere.
