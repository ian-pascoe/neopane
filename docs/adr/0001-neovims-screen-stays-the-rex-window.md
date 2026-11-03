# Neovim's screen stays the size of the Rex window; only the tabline is narrowed

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
  and the result is padded with blanks to the screen's width. Only tablines
  whose Lua reads `vim.o.columns` fit (bufferline, lualine); the built-in
  tabline and Vimscript ones that read `&columns` stay clipped.
- `g:neorex_tabline_width` holds the tabline width while it's narrower than
  the screen, for tablines that want to fit themselves.
- The padding is added only when the tabline read the width: Neovim cuts a
  too-wide tabline from the left, so padding one that ignored it would hide
  its first tabs.
