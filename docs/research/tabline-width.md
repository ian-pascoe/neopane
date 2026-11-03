# Tabline width under neorex

Facts gathered while designing #11 (the tabline is clipped when a foreign pane
shares the top of the Rex window). Checked against bufferline.nvim 655133c
(2025-01-14) and Neovim 0.12.

## bufferline.nvim

- It sets `'tabline'` to `%!v:lua.nvim_bufferline()`
  (`lua/bufferline.lua:205`), so it is evaluated afresh on every tabline
  redraw, `:redrawtabline` included.
- Its width budget is `vim.o.columns` minus custom areas, offsets, tab-page
  indicators and the close button (`lua/bufferline/ui.lua:666`). It has no
  option for the width.
- It reads `vim.o.columns` through the global `vim` table each render, not
  through a cached local (`ui.lua:666`; it never binds `vim.o` to a local), so
  replacing `vim.o` for the length of one call changes the width it sees.
- Its output is `offsets.left, left_area, core, right_area, offsets.right`,
  where `core` ends with `%=`, the tab-page indicators and the close button
  (`ui.lua:684-697`). The right custom area therefore sits to the right of the
  tab-page indicators.
- Custom areas are read from `require('bufferline.config').options.custom_areas`
  on every render (`lua/bufferline/custom_area.lua`). There is no public API to
  add one after `setup()`; that table is internal.
- Offsets come from `fn.winlayout()` (`lua/bufferline/offset.lua`). Under neorex
  each tab page's layout is only its anchor, so offsets never apply.

## Neovim statusline/tabline evaluation (tested, Neovim 0.12.6, `nvim_eval_statusline` with `use_tabline`)

- A `%=` inside a `%-W.W(...%)` group does not expand: `%-40.40(abc%=xyz%)`
  renders `abcxyz` padded to 40. A wrapper can't confine an inner tabline's
  right alignment to a width with a group.
- Literal spaces after the content do confine it: at 100 columns,
  `abc%=xyz` followed by 58 spaces puts `xyz` at columns 40–42, so the inner
  layout fills the first 42 columns.
- A tabline wider than the screen, with no `%<`, is cut from the start: at 20
  columns, `AAAAAAAAAABBBBBBBBBBCCCCC` renders `<AAAABBBBBBBBBBCCCCC`. Padding
  appended to a tabline that already fills the screen hides its first tabs.
- Swapping `vim.o` for a proxy whose `columns` is 42, then restoring it, works
  from Lua; the real `'columns'` stays 100.

Other renderers read `'columns'` too: lualine's `buffers` and `tabs`
components use `vim.o.columns`, and a Vimscript `'tabline'` would read
`&columns`, which a `vim.o` proxy doesn't cover.

## Neovim external windows vs. the screen size

With `ext_multigrid` on, `win_config_float` does not clamp a float's or an
external window's width and height to `Columns` and `Rows`; that clamp only
runs without multigrid (`src/nvim/winfloat.c`, `win_config_float`, v0.12.0).
An external window may be wider than Neovim's screen.
