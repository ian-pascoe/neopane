# Neovim's screen stays the size of the Rex window; only the tabline is fitted

Widened by ADR 0008: the Rex window is one kind of layout area, and Neovim's
screen is the size of the current pane's layout area; within an area, all of
this still holds. Its floating panes are Rex floating panes; neorex's own kind
is now a float group pane (ADR 0010). A tab page's floating pane is Rex's
tab page area, opened by its `open_tab_page`; its panes still float within
the Rex window's area (ADR 0012).

When a foreign pane shares the top of the Rex window, the tabline panes are
narrower than Neovim's screen and the tabline's right end is never shown (#11).
We keep Neovim's screen (`'columns'` x `'lines'`) the size of the Rex window, and
fit the tabline to the tabline width instead of shrinking the screen to it:
every mapping between grid 1 and the Rex window (pane origins, floating-pane
bounds, a tab page's floating pane, mouse input) is a scale of the Rex window,
and a screen narrower than it would need a 2D mapping with no answer for
window panes outside the tabline panes' columns, such as one below a foreign
pane in the top right.

## Considered Options

- **Shrink Neovim's screen to the tabline width.** Every tabline plugin fits
  with no help, but each grid-1 mapping needs rewriting, and it breaks the
  rule #9 checks (`&columns` and `&lines` match the Rex window).
- **`ext_tabline`, drawn by the daemon.** Drops bufferline and every custom
  `'tabline'`.
- **A bufferline config snippet the user adds.** Works, but puts config on
  every user for the main case.

## Consequences

- No Neovim option sets a tabline's width, so `windows.lua` wraps any `%!`
  `'tabline'`: while it's evaluated, `vim.o.columns` reads as the tabline width,
  and the result is padded with blanks to the screen's width. Tablines whose
  Lua reads `vim.o.columns` fit their own widths too, such as bufferline's;
  lualine's right-hand sections now fit, and only widths it computes from
  `'columns'` (`max_length`) don't. The built-in tabline has no expression to
  evaluate, so neorex draws it itself, fitted (ADR 0002).
- `g:neorex_tabline_width` holds the tabline width while it's narrower than
  the screen, for tablines that want to fit themselves.
- Every non-empty tabline is padded with blanks to the screen's width while
  there's a tabline width, whether or not it read the width, and a
  format-string `'tabline'`, such as lualine's, is padded in place. The padding
  follows a `%<`. A tabline with no `%<` of its own loses only padding, and one
  with its own `%<` is cut there to the tabline width, as Neovim would cut it
  on a screen that wide. Since nothing past the tabline width is ever shown,
  padding can only bring into view what a `%=` right-aligned. The one tabline
  it hurts fills the screen with literal blanks and has an early `%<`: it is
  cut from its start. Widths a tabline computes from `'columns'` fit only
  through the `vim.o` proxy or `g:neorex_tabline_width`.
