# Floats and `VimResized`: which plugins re-lay out an open float?

Question: neorex may shrink Neovim's `'columns'`/`'lines'` while a picker float
is the current window, then grow them back when the picker loses focus. Does an
open float re-compute its size and position on resize, and does it do so from its
configured relative sizes (so it grows back)?

Sources are the plugins' default branches, cloned 2026-11-07 and read at the
commits below. Links are permalinks (`blob/<sha>/path#Lnn`).

| Plugin | Commit |
| --- | --- |
| telescope.nvim | `40aedd8` (2026-08-17) |
| fzf-lua | `6ab6a29` (2026-11-06) |
| snacks.nvim | `882c996` (2026-05-25) |
| mini.pick (mini.nvim split repo) | `cd838a8` (2026-11-06) |
| toggleterm.nvim | `9a88eae` (2025-03-09) |
| lazygit.nvim | `9ffc610` (2026-09-19) |
| noice.nvim / nui.nvim | `7bfd942` (2025-11-03) / `10fc361` (2026-08-21) |
| nvim-cmp | `fe29aba` (2026-11-05) |
| blink.cmp | `7b31e1b` (2026-11-04) |
| which-key.nvim | `3aab214` (2025-10-28) |
| Neovim | `de6ea53` (master, 2026-11-07); behaviour also run on v0.12.6 |

## Summary

| Plugin | Re-layouts on resize? | From relative config? | Notes |
| --- | --- | --- | --- |
| Neovim built-in | Yes, in C, before `VimResized` fires: stored float config is re-applied and clamped | Re-applies the *stored* numbers (original request is kept), so it grows back to whatever absolute size was last set | Size clamped to screen; position clamped on compose. Does not know about fractions. |
| telescope.nvim | Yes (`VimResized` → `full_layout_update`) | Yes (`width = 0.8` etc. resolved against current `vim.o.columns/lines`) | Autocmd is **buffer-local to the prompt buffer**: only fires while the prompt buffer is current. |
| fzf-lua | Yes (`VimResized` → `FzfWin:redraw`) | Yes (`normalize_winopts` re-reads `self._o.winopts`, sizes `<1` are fractions of current screen) | Autocmd is **buffer-local to the fzf terminal buffer**. Wiki documents manual `:FzfLua redraw`. |
| snacks.nvim picker | Yes (layout, list, and picker each listen) | Yes (fractions in `Snacks.win:dim`; `layout.preset` function re-evaluated, may swap layout) | Global autocmds (no buffer filter); works regardless of focus. Fixed bug #1348 (invalid window id on layout change). |
| snacks.nvim other floats (terminal, lazygit, win) | Yes (`Snacks.win` `VimResized` → `update`) unless `resize = false` | Yes (`<1` is a fraction of `vim.o.columns/lines`, `0` is full) | Global autocmd, per-window augroup. Float terminals use the same `Snacks.win`. |
| mini.pick | Yes (`VimResized` → `MiniPick.refresh`) | Default config yes (0.618 of available screen). A user `window.config` table is used as-is; a callable is re-called each time | Global autocmd. Forces `redraw` because `getcharstr` blocks redraw. |
| toggleterm.nvim (float) | Yes since PR #186 (`VimResized` → `ui.update_float`) | Default (`columns - 20`, `lines - 10`) yes. `float_opts.width/height` numbers are absolute; only **functions** re-evaluate | Autocmd is **buffer-local to the terminal buffer**. |
| lazygit.nvim | Yes (`VimResized`, deferred 20 ms) | Yes (`scaling_factor` 0.9 × current screen) | Only on the non-plenary path. Global, never-cleared autocmd (one added per open). |
| noice.nvim cmdline popup / popupmenu | **No** resize handler | Computed from percent/auto at show time, not afterwards | `NuiView:update_layout` runs on create and when a hidden view is shown; nui has no `VimResized` handler. Neovim clamps the float meanwhile. |
| nvim-cmp | No resize handler | Computed on each open/reposition from `vim.o.lines/columns` | Transient; menu is re-placed on the next completion event. |
| blink.cmp | Partial: menu `update_position` on `WinResized` (plus `CursorMovedI`, `WinScrolled`) | Computed from `vim.o.lines/columns` at each update | `WinResized` is tab-page-window based, not a screen-resize guarantee. |
| which-key.nvim | No resize handler | Yes, every `show()` (each key press) recomputes from `vim.o.columns/lines` | Float is re-laid out on the next key, not on resize. |

Consequence for neorex, from the sources: the three plugins that scope the
handler to the float's own buffer (telescope, fzf-lua, toggleterm) only
re-layout if the resize happens while that buffer is current. See
[Implications](#implications-for-neorex).

## Neovim built-in

- Resizing goes `screen_resize` → `win_new_screensize` → `win_new_screen_rows` /
  `win_new_screen_cols` → `win_reconfig_floats`, and only **then**
  `VimResized` is applied
  ([drawscreen.c#L320-L383](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/drawscreen.c#L320-L383),
  [window.c#L5810-L5850](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/window.c#L5810-L5850)).
  So by the time a `VimResized` callback runs, Neovim has already clamped every
  float.
- `win_reconfig_floats` calls `win_config_float(wp, wp->w_config)` for each float
  ([winfloat.c#L354-L359](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/winfloat.c#L354-L359)).
- `win_config_float` (non-multigrid UIs) clamps **size**:
  `w_height = MIN(w_height, Rows - border_height - above_ch)`,
  `w_width = MIN(w_width, Columns - border_width)`, where `above_ch` is
  `'cmdheight'` for floats with `zindex < 200`
  ([winfloat.c#L240-L244](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/winfloat.c#L240-L244)).
  It clamps `w_width/w_height`, not `w_config`, so the requested size survives.
- `ui_ext_win_position` clamps **position** when composing:
  `comp_row = MAX(MIN(comp_row, Rows - height_outer - above_ch), 0)` and
  `comp_col = MAX(MIN(comp_col, Columns - width_outer), 0)` (the column clamp is
  skipped for `fixed` floats that are not east-anchored)
  ([window.c#L873-L945](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/window.c#L873-L945),
  clamp at [L947-L949](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/src/nvim/window.c#L947-L949)).
- Docs agree: "Out-of-bounds values, and configurations that make the float not
  fit inside the main editor, are allowed. The builtin implementation truncates
  values so floats are fully within the main screen grid"
  ([api.txt#L3907-L3916](https://github.com/neovim/neovim/blob/de6ea53768fffc4091f5ff57a4c3721681d35e55/runtime/doc/api.txt#L3907-L3916)).
- Tested on v0.12.6 (`nvim --headless`, float `relative='editor'`, 50×15 at
  row 5, col 60, then `vim.o.columns=40; vim.o.lines=10`, then 120×40):
  `nvim_win_get_config` stayed 50×15 throughout; `nvim_win_get_width/height`
  went 50×15 → 38×7 → 50×15. So a float is **clamped, not left, and un-clamps by
  itself** up to the size in its stored config. Position clamping could not be
  observed headless (it is applied at compose time, which needs a UI).
- `:set columns=N` / `vim.o.columns = N` runs `screen_resize` synchronously
  and fires `VimResized` (tested: fires on each set, in the current buffer).
  Autocmds with `buffer = b` only fire when `b` is the current buffer at that
  moment (tested: the buffer-local autocmd fired only after the float's buffer
  was entered).

## telescope.nvim

- `VimResized` autocmd with `buffer = self.prompt_bufnr`, group `PickerInsert` →
  `on_resize_window`
  ([pickers.lua#L710-L717](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers.lua#L710-L717),
  [L1639-L1644](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers.lua#L1639-L1644)).
- That calls `Picker:full_layout_update` → `recalculate_layout` →
  `layout:update()`
  ([L747-L768](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers.lua#L747-L768),
  [L785-L793](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers.lua#L785-L793)).
  `update` calls `picker:get_window_options(vim.o.columns, line_count)` fresh
  ([L147-L152](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers.lua#L147-L152)),
  and the layout strategies resolve `layout_config.width/height` (defaults 0.8 /
  0.9, [config.lua#L80-L81](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/config.lua#L80-L81))
  against those maxima
  ([layout_strategies.lua#L308-L313](https://github.com/nvim-telescope/telescope.nvim/blob/40aedd8a68c78a656a10a8d62d80c54af59420fb/lua/telescope/pickers/layout_strategies.lua#L308-L313)).
  It also keeps results scroll position (`update_scroll`).
- Grows back: yes, recomputed from relative config every time.
- Caveat: the autocmd is buffer-local, so it only runs while the prompt buffer is
  current. Related open-ish issue about flex layout hiding the preview at
  certain widths: [#3138](https://github.com/nvim-telescope/telescope.nvim/issues/3138)
  (not verified by me).

## fzf-lua

- `FzfWin:on` creates a per-event augroup (`clear = true`) and an autocmd with
  `buffer = self.fzf_bufnr` unless `global == true`;
  `setup_autocmds` registers `VimResized → self:redraw()` without `global`
  ([win.lua#L903-L915](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L903-L915)).
  Buffer-local to the fzf terminal buffer.
- `redraw` calls `normalize_winopts()` then `redraw_main`/`redraw_preview`
  ([L844-L853](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L844-L853)).
  `normalize_winopts` deep-clones `self._o.winopts` (the *original* config) and
  `normalize_size`s width/height/row/col against current `vim.o.columns` /
  `vim.o.lines - cmdheight`, then clamps row/col
  ([L574-L627](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L574-L627)).
  It then `win_set_config`s the existing window
  ([L864-L885](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L864-L885)).
  Grows back: yes. The preview layout (`flex`, `flip_columns`) also re-evaluates.
- Hidden picker (`hide`): "VimResized won't emit on hidden buffer"; it stores
  `last_view = {lines, columns, cmdheight}` to compare on resume
  ([L1259-L1270](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L1259-L1270)).
  That comment confirms the buffer-local delivery model.
- Wiki: if the window isn't redrawn, `:FzfLua redraw` /
  `require'fzf-lua'.redraw()` forces it
  ([wiki](https://github.com/ibhagwan/fzf-lua/wiki), FAQ on resizing).
  A comment on closing notes `'cmdheight'` "will be wrong if vim resized (#1462)"
  ([win.lua#L1083-L1084](https://github.com/ibhagwan/fzf-lua/blob/6ab6a29d56888dcf66edfeb082ad4a46e7f075bb/lua/fzf-lua/win.lua#L1083-L1084)).

## snacks.nvim

Picker:

- Three global handlers (`win:on` without `buf`/`win` filters creates a plain
  autocmd in the window's augroup,
  [win.lua#L478-L498](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L478-L498)):
  1. `Snacks.layout` root: `VimResized` → `update()` (skips if the root isn't on
     the current tab)
     ([layout.lua#L158-L164](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/layout.lua#L158-L164)).
     `update` recomputes `parent_width/height` from `vim.o.columns` /
     `vim.o.lines` (minus tabline/cmdheight for side layouts) and lays out boxes
     again from relative sizes
     ([L234-L271](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/layout.lua#L234-L271)).
  2. Picker: `VimResized` → `vim.schedule(set_layout(Snacks.picker.config.layout(opts)))`
     ([picker.lua#L276-L282](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/picker/core/picker.lua#L276-L282)).
     `layout.preset` may be a function; default is
     `vim.o.columns >= 120 and "default" or "vertical"` (docs/picker.md), so the
     whole layout can switch when shrunk. `set_layout` returns early if the
     resolved layout is deep-equal
     ([L355-L363](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/picker/core/picker.lua#L355-L363)),
     else closes and rebuilds the windows.
  3. List: `VimResized` → re-read height, mark dirty, `update`
     ([list.lua#L115-L119](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/picker/core/list.lua#L115-L119)).
- Grows back: yes; sizes `< 1` are fractions of the parent, `0` is full,
  negative positions count from the far edge
  ([win.lua#L1267-L1310](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L1267-L1310));
  parent size is `vim.o.lines/columns` unless `relative = "win"`
  ([L1118-L1127](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L1118-L1127)).
- Known bug: [#1348](https://github.com/folke/snacks.nvim/issues/1348) "invalid
  window id on resize layout change" (closed, fix in PR #1335; a commenter still
  saw it on `dec29f5`, the original reporter could not repro). Layout swaps are
  `vim.schedule`d, so a resize with another resize racing it can touch
  closed windows.
- Side effect: `Layout:update` sets `vim.o.lazyredraw = true`
  ([layout.lua#L238](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/layout.lua#L238)).

Other floats (`Snacks.win`, terminal, lazygit):

- `Snacks.win.new` registers `self:on("VimResized", self.on_resize)`;
  `on_resize` calls `update()` unless `opts.resize == false`
  ([win.lua#L322-L337](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L322-L337)).
  `update` re-runs `nvim_win_set_config(self.win, self:win_opts())`, where
  `win_opts` is built from `dim()` ([L803-L813](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L803-L813)).
- `Snacks.terminal` opens a `Snacks.win` with `position = "float"` when a command
  is given, else `"bottom"`
  ([terminal.lua#L88-L94](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/terminal.lua#L88-L94));
  `Snacks.lazygit` ends in `Snacks.terminal(cmd, opts)`
  ([lazygit.lua#L206](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/lazygit.lua#L206)).
  Both therefore inherit the `VimResized` handler above when floating; the
  `float` style is `width = height = 0.9`
  ([win.lua#L104-L110](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/win.lua#L104-L110)).
  Bottom/side terminals are splits, not floats (no config re-apply).
- Also: dashboard (`WinResized`+`VimResized`,
  [dashboard.lua#L249](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/dashboard.lua#L249)),
  notifier ([notifier.lua#L310](https://github.com/folke/snacks.nvim/blob/882c996cf28183f4d63640de0b4c02ec886d01f2/lua/snacks/notifier.lua#L310)).

## mini.pick

- `au('VimResized', '*', MiniPick.refresh, 'Refresh on resize')`, global
  ([pick.lua#L2051-L2058](https://github.com/echasnovski/mini.pick/blob/cd838a878c0fd5e709338c6f3ed0e152ea3a6ebe/lua/mini/pick.lua#L2051-L2058)).
- `refresh` → `picker_update(active, false, true)`, which recomputes
  `picker_compute_win_config(opts.window.config)` and `nvim_win_set_config`s the
  main window, then `ensure_redraw`
  ([L939-L944](https://github.com/echasnovski/mini.pick/blob/cd838a878c0fd5e709338c6f3ed0e152ea3a6ebe/lua/mini/pick.lua#L939-L944),
  [L2298-L2307](https://github.com/echasnovski/mini.pick/blob/cd838a878c0fd5e709338c6f3ed0e152ea3a6ebe/lua/mini/pick.lua#L2298-L2307)).
- Default size is `floor(0.618 * max)` of `vim.o.columns` /
  (`vim.o.lines - cmdheight - tabline - statusline`), anchored `SW` at the
  bottom-left; width/height then clamped to the screen
  ([L2343-L2370](https://github.com/echasnovski/mini.pick/blob/cd838a878c0fd5e709338c6f3ed0e152ea3a6ebe/lua/mini/pick.lua#L2343-L2370)).
  Grows back: yes for the default. A user `window.config` table with absolute
  numbers is merged over the default each time; a callable is re-called, so
  use a callable for relative sizes (docs: "table or callable returning it",
  [L876-L877](https://github.com/echasnovski/mini.pick/blob/cd838a878c0fd5e709338c6f3ed0e152ea3a6ebe/lua/mini/pick.lua#L876-L877)).
- Caveat: the picker runs inside a blocking `getcharstr` loop, hence the forced
  redraw (comment at L942).

## toggleterm.nvim (float direction)

- For float terminals, `setup_buffer_autocommands` adds `VimResized` with
  `buffer = term.bufnr` → `on_vim_resized` → `ui.update_float` if the terminal is
  open
  ([terminal.lua#L148-L175](https://github.com/akinsho/toggleterm.nvim/blob/9a88eae817ef395952e08650b3283726786fb5fb/lua/toggleterm/terminal.lua#L148-L175)).
  Added by PR #186 after
  [issue #168](https://github.com/akinsho/toggleterm.nvim/issues/168)
  ("update term(float) size on VimResized"); the maintainer there confirms size
  "functions work by calculating the size each time the terminal is opened".
- `update_float` → `nvim_win_set_config(term.window, _get_float_config(term, false))`
  ([ui.lua#L387-L390](https://github.com/akinsho/toggleterm.nvim/blob/9a88eae817ef395952e08650b3283726786fb5fb/lua/toggleterm/ui.lua#L387-L390)).
- `_get_float_config` defaults: `width = columns - 20` (at least 80 unless the
  screen is smaller), `height = lines - 10`, centered; `float_opts.width/height/
  row/col` may be a number (used verbatim, **absolute**) or a function of the
  term, re-evaluated per call
  ([ui.lua#L257-L290](https://github.com/akinsho/toggleterm.nvim/blob/9a88eae817ef395952e08650b3283726786fb5fb/lua/toggleterm/ui.lua#L257-L290)).
  So relative sizes require function-valued options
  ([issue #658](https://github.com/akinsho/toggleterm.nvim/issues/658), open).
- Caveat: buffer-local handler; only fires while the terminal buffer is current.
  The autocmd is registered only for terminals that are float when created.

## lazygit.nvim (kdheepak)

- Default path (`vim.g.lazygit_floating_window_use_plenary` unset): width/height
  are `ceil(vim.o.columns/lines * scaling_factor)` (`height - 1`), row/col
  centered ([window.lua#L15-L32](https://github.com/kdheepak/lazygit.nvim/blob/9ffc6103926718f7070fb4017694543cd7dc9718/lua/lazygit/window.lua#L15-L32);
  default factor 0.9, [plugin/lazygit.vim#L14](https://github.com/kdheepak/lazygit.nvim/blob/9ffc6103926718f7070fb4017694543cd7dc9718/plugin/lazygit.vim#L14-L15)).
- It registers a **global** `VimResized` autocmd (no group, no buffer) that, after
  `vim.defer_fn(…, 20)`, recomputes `get_window_pos()` and `nvim_win_set_config`s
  the window if still valid
  ([window.lua#L66-L83](https://github.com/kdheepak/lazygit.nvim/blob/9ffc6103926718f7070fb4017694543cd7dc9718/lua/lazygit/window.lua#L66-L83)).
  Works regardless of focus; grows back.
- Gaps: the autocmd is never cleared, so one accumulates per open (stale ones
  no-op on `nvim_win_is_valid`). With plenary enabled, `get_window_pos` returns
  early with a plenary window, `open_floating_window` returns before the
  autocmd is created, so there is **no** resize handling
  ([L21-L29](https://github.com/kdheepak/lazygit.nvim/blob/9ffc6103926718f7070fb4017694543cd7dc9718/lua/lazygit/window.lua#L21-L29),
  [L45-L47](https://github.com/kdheepak/lazygit.nvim/blob/9ffc6103926718f7070fb4017694543cd7dc9718/lua/lazygit/window.lua#L45-L47)).
- The 20 ms defer means that a script that shrinks, then immediately grows back,
  may see the first callback compute with the final size (it reads
  `vim.o` at run time), which is harmless.

## noice.nvim (cmdline popup / popupmenu)

- No `VimResized`/`WinResized` anywhere in noice (`grep` over `lua/`), and nui's
  only mention of `VimResized` is an event-name table
  ([nui utils/autocmd.lua#L228](https://github.com/MunifTanjim/nui.nvim/blob/10fc361835c856ba4233ef5ea135b919bf3dce97/lua/nui/utils/autocmd.lua#L228)).
- `NuiView:update_layout` → `nui.popup:update_layout(get_layout())`
  ([view/nui.lua#L245-L247](https://github.com/folke/noice.nvim/blob/7bfd942445fb63089b59f97ca487d605e715f155/lua/noice/view/nui.lua#L245-L247)).
  Called on `create` (L95), when options change in `reset` (L143) and in `show`
  only when the view was not already visible (L296-L299)
  ([L274-L299](https://github.com/folke/noice.nvim/blob/7bfd942445fb63089b59f97ca487d605e715f155/lua/noice/view/nui.lua#L274-L299)).
- `get_layout` resolves `max_width/max_height` (`vim.o.columns - 4`,
  `vim.o.lines - 4`) and negative positions at that moment
  ([util/nui.lua#L150-L190](https://github.com/folke/noice.nvim/blob/7bfd942445fb63089b59f97ca487d605e715f155/lua/noice/util/nui.lua#L150-L190));
  `cmdline_popup` is `row/col = "50%"`, width `auto`, `min_width = 60`
  ([views.lua#L199-L213](https://github.com/folke/noice.nvim/blob/7bfd942445fb63089b59f97ca487d605e715f155/lua/noice/config/views.lua#L199-L213)).
- Net: an already-visible popup is re-laid out only on its next
  show-after-hide or option change; mid-resize it is just clamped by Neovim
  (its stored width/height/row/col are absolute numbers), and un-clamps to those
  numbers. The percent resolution is not redone until the next open. For the
  cmdline, which exists only while typing a command, this is mostly cosmetic.

## nvim-cmp and blink.cmp (brief)

- nvim-cmp: no resize handler. Placement code reads `vim.o.lines/columns` each
  time the menu opens or repositions
  ([custom_entries_view.lua#L196-L214](https://github.com/hrsh7th/nvim-cmp/blob/fe29aba4117227a6df637bc41e4b4fb4e98864a0/lua/cmp/view/custom_entries_view.lua#L196-L214);
  docs view [docs_view.lua#L40](https://github.com/hrsh7th/nvim-cmp/blob/fe29aba4117227a6df637bc41e4b4fb4e98864a0/lua/cmp/view/docs_view.lua#L40);
  wildmenu [wildmenu_entries_view.lua#L130-L132](https://github.com/hrsh7th/nvim-cmp/blob/fe29aba4117227a6df637bc41e4b4fb4e98864a0/lua/cmp/view/wildmenu_entries_view.lua#L130-L132)).
  Not recomputed on screen resize.
- blink.cmp: menu `update_position` on `CursorMovedI`, `WinScrolled`, `WinResized`
  ([menu/init.lua#L81-L83](https://github.com/saghen/blink.cmp/blob/7b31e1b6497be70f620bbad83a789929248697ac/lua/blink/cmp/completion/windows/menu/init.lua#L81-L83);
  signature window [L43](https://github.com/saghen/blink.cmp/blob/7b31e1b6497be70f620bbad83a789929248697ac/lua/blink/cmp/signature/window.lua#L43)),
  using `vim.o.lines/columns` inside `get_cursor_screen_position` /
  `get_direction_with_window_constraints`
  ([lib/window/init.lua#L283-L286](https://github.com/saghen/blink.cmp/blob/7b31e1b6497be70f620bbad83a789929248697ac/lua/blink/cmp/lib/window/init.lua#L283-L286)).
  `WinResized` fires when any window in the current tab page changes size, so a
  screen resize normally triggers it for the normal windows, but this is
  incidental, not a dedicated screen-resize handler.

## which-key.nvim

- No resize handler (`grep` finds neither `VimResized` nor `WinResized`).
- Layout is recomputed on every `View.show()`, which runs on each key press while
  the popup is active: container is
  `Layout.dim(vim.o.columns, vim.o.columns, opts.width)` etc., and the float's
  `width/height/col/row` come from `Layout.dim(…, vim.o.columns / vim.o.lines …)`
  with fractions (`|x| < 1`) and negatives resolved against the current screen
  ([view.lua#L335-L338](https://github.com/folke/which-key.nvim/blob/3aab2147e74890957785941f0c1ad87d0a44c15a/lua/which-key/view.lua#L335-L338),
  [L412-L421](https://github.com/folke/which-key.nvim/blob/3aab2147e74890957785941f0c1ad87d0a44c15a/lua/which-key/view.lua#L412-L421),
  [layout.lua#L16-L29](https://github.com/folke/which-key.nvim/blob/3aab2147e74890957785941f0c1ad87d0a44c15a/lua/which-key/layout.lua#L16-L29)).
  When already open, `update()` → `show()` → `nvim_win_set_config`
  ([view.lua#L100-L118](https://github.com/folke/which-key.nvim/blob/3aab2147e74890957785941f0c1ad87d0a44c15a/lua/which-key/view.lua#L100-L118),
  [win.lua#L88-L89](https://github.com/folke/which-key.nvim/blob/3aab2147e74890957785941f0c1ad87d0a44c15a/lua/which-key/win.lua#L88-L89)).
- Net: stays at the last-computed (Neovim-clamped) geometry until the next key.

## Implications for neorex

1. A resize *while the picker is current* works for every picker/terminal
   plugin that has a handler (telescope, fzf-lua, snacks, mini.pick, toggleterm,
   lazygit.nvim): `vim.o.columns = N` fires `VimResized` synchronously in the
   current buffer, after Neovim has already clamped floats.
2. Telescope, fzf-lua and toggleterm only listen on the float's own buffer. If
   neorex changes `'columns'`/`'lines'` after focus has left the picker (the
   "grow back when it loses focus" step), their handlers will not run, and
   the float keeps the (shrunk) absolute size they last applied. (Neovim's
   built-in un-clamp only restores up to the stored `w_config`, which these
   plugins overwrote with the shrunk values at the previous resize.) Snacks,
   mini.pick, lazygit.nvim handlers are global and will run.
3. To grow back reliably, restore the size while the picker's buffer is still
   current (before focus moves), or re-trigger afterwards with the picker's
   buffer current (`nvim_buf_call` + `nvim_exec_autocmds('VimResized', {})`
   inside `nvim_win_call`/with that buffer current; fzf-lua also offers
   `FzfLua redraw`). Not tested here.
4. Plugins with no handler (noice, cmp, which-key; partly blink) re-compute on
   their next open/key/event; between resizes they rely on Neovim's clamping
   (size up to stored config again on grow).
5. mini.pick/toggleterm only grow back from *relative* config if the user config
   is a callable/function; static absolute numbers stay absolute.
