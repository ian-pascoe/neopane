# A float group with no float group pane is drawn in a pane, on a screen shrunk to fit it

Only Rex makes floating panes today, and an entered float group goes to one
(#89). Other pane providers may have none, such as tmux before 3.8, herdr and
AeroSpace with Ghostty, and any request may fail. Since ADR 0008 Neovim's
screen spans the current pane's layout area, so in a live layout a picker
sized from `'columns'` can be bigger than the current pane. A float group with
no float group pane is drawn in a window pane: the one that was current when
one of its floats last became current. While it is current and too big for
that pane, Neovim's screen takes the pane's size, and the picker lays itself
out again to fit, as pickers and float terminals do on `VimResized`.

## Considered Options

- **Each pane draws its slice of the group**, so it looks like one overlay
  across the area, as in plain Neovim. ADR 0008 draws screen floats in the
  current pane only, and a key or click in another pane would make that pane
  current and leave the float.
- **Move the group in and cut it at the pane's edges**, as other screen
  floats are. A Telescope picker in a third of a tmux window would show its
  top left and no preview.
- **Ask Neovim to refit the group to the pane**, as floats follow a Rex
  floating pane. Only a snacks picker or a lone float can be refitted so, and
  one shrunk to a small pane would need its own size remembered to grow back.
- **The group follows the current pane**, as other screen floats do. A float
  terminal the user clicked away from, such as lazygit, would cover the window
  they clicked into.
- **A short grace, then the group drawn in a pane until its floating terminal
  attaches.** A Rex floating pane can take longer than any grace, and a picker
  would flash in the pane, then jump: a Rex regression.
- **A layout-side `place-float` now**, floating a terminal a pair opened. No
  pane provider on the map needs it: AeroSpace can float a window but neither
  size nor place it, and reports no geometry, so each AeroSpace pane is a
  layout area of its own and its float groups fit it. sway and Hyprland can,
  but aren't on the map.
- **A float marker in the pane title**, so a window rule floats the terminal
  as it maps. It changes ADR 0006's title for what only some window managers
  use, and Ghostty keeps a title given at launch for good.
- **A trigger per pane provider** (entered, focusable or never). Never is a
  missing `open-float`, and focusable would float completion menus.

## Consequences

- A float group is a screen float with every float hanging from it; screen
  floats that can take focus and touch are one group. Floats placed on a
  window that isn't a float are never in one, and never get a float group
  pane, as in Rex today.
- An entered float group gets a float group pane when the pane provider has
  `open-float`, the same for every pane provider: at most four at once, the
  top-most. Its bounds are cells relative to the current pane, with fractions
  of that pane's area when it is in a live layout.
- A group whose float group pane is on its way is drawn nowhere until its
  floats are fitted to the new terminal, `open-float` fails, or the attach
  timeout passes, as Rex's are today. A group whose request failed or timed
  out is drawn in a pane for the rest of its life; the next group asks again.
  Exit 69 means unsupported until the pane provider is chosen again (ADR
  0006). A terminal that attaches after its group closed, or after the
  timeout, is closed.
- A float group with no float group pane is drawn in the window pane that was
  current when one of its floats last became current. It stays there when the
  user goes to another pane, and moves when one of its floats becomes current
  from another. Other screen floats, the command line and messages are drawn
  in the current pane (ADR 0008). Every screen float is moved into its pane
  and cut at the edges if still too big.
- When one of its floats becomes current while Neovim's screen is the area's
  size, and the group, as laid out on that screen, is bigger than the current
  pane's room (its width, below its tabline row if it is a tabline pane, and
  above its `'cmdheight'` rows), Neovim's screen takes the current pane's
  size. The group isn't drawn until Neovim has redrawn at that size. Only the
  screen's size changes: tabline panes keep their rows, and the tabline is cut
  to the smaller screen. The shrink is decided once and holds while Neovim's
  current window is a float, so a prompt opened over a picker doesn't grow it
  back; it ends when another window becomes current.
- Then the screen goes back to the area's size, and each of the group's floats
  has its buffer-local `VimResized` autocommands run again with its window
  current, since Telescope, fzf-lua and toggleterm listen only on their own
  buffers. A group left in a pane is then moved in and cut there until it is
  current again. Floats with no resize handler, such as noice's, are clamped
  by Neovim to the smaller screen and get their size back after.
- A float group pane's floats fit its terminal's size, whatever the pane
  provider, and move on Neovim's screen only when the live layout reports
  where its floating terminal went. A float group pane never follows its
  floats, and stays where it was placed when the current pane moves to another
  layout area.
- When a float group pane is ready and one of its floats is current, its
  terminal is focused; when the group closes, the current window's pane is.
  Without `focus`, keys come from whichever terminal has focus, and those from
  the current pane reach the current float. A key or click from a float group
  pane whose floats aren't current makes its focusable float current.
- A float group pane's terminal hanging up closes its group's floats, however
  it was closed.
- Should a pane provider need to float a terminal it opened, it gets
  `place-float`, a layout-provider capability run once `find` has answered for
  the new terminal. It takes `{terminal}` and `open-float`'s inputs: the
  current pane's terminal, the bounds in cells relative to it, its size in
  cells, and `<NAME>_AREA_BOUNDS` when it is in a live layout. On failure the
  terminal is closed, never left tiled, and the group is drawn in a pane; exit
  69 means unsupported until the pane provider is chosen again. With it comes
  `<NAME>_KIND` for `open` (`window` or `float`), so a template can mark a
  float's terminal for a window rule, by class or in the title it sets. Until
  then neither exists, and a pair's float groups are drawn in panes.

Research: `docs/research/float-a-terminal-by-command.md` and
`floats-on-vimresized.md`, each on its `research/` branch.
