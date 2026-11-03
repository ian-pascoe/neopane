# Neovim built-in tabline (`'tabline'` empty)

Checked against Neovim v0.12.0 (`src/nvim/statusline.c`, `drawscreen.c`,
`mouse.c`, `window.c`, `path.c`, `highlight_defs.h`, `highlight_group.c`,
`api/vim.c`, `eval/funcs.c`, `runtime/doc/options.txt`) and v0.10.0
(`statusline.c`, `draw_tabline` only). Fetched from
`https://raw.githubusercontent.com/neovim/neovim/<tag>/...`.
Line numbers are v0.12.0 unless marked v0.10.

## Entry point and gating

- `draw_tabline()` is `statusline.c:647-815`.
- Called from `update_screen` when `redraw_tabline` or `type >= UPD_NOT_VALID`
  (`drawscreen.c:616-623`), and at the end of `redraw` (`drawscreen.c:2780-2782`).
- Returns early if the default grid is unallocated (`:654-656`), if the UI has
  `ext_tabline` (`:659-662`, see Q6), or if `tabline_height() < 1` (`:664-666`).
- `tabline_height()` (`window.c:7341-7352`): 0 under `ext_tabline`; 0 when
  `'showtabline'` is 0; for `'showtabline'` 1, 1 only when there is more than one
  tab page.
- `redraw_tabline` is cleared at the start (`:657`) and end (`:814`).
- Built-in is used iff `*p_tal == NUL` (`:673`, else-branch `:675`). The check is
  on the option string, not the evaluated result (see Q7).
- Click defs are cleared first (`:668-670`), so clicks outside labels do nothing
  unless the tail-fill rule below applies.

## 1. Width and overflow

- Tab count: all tab pages, `FOR_ALL_TABS` (`:681-683`).
- `tabwidth = MAX((Columns - 1 + tabcount / 2) / tabcount, 6)` (`:685`).
  Integer division, so it rounds to nearest. Minimum 6, no maximum. v0.10 has
  the same formula and floor, written as explicit `if`s (`statusline.c:739-743`).
- `tabwidth` is a cap, not a padding width. Labels are not padded to it.
  - Name room is `room = scol - col + tabwidth - 1` (`:744`), where `scol` is the
    label's first cell (separator included) and `col` is after the count, `+` and
    the space. The name is only drawn if `room > 0` (`:745`), and then
    `label = used + name + 1 <= tabwidth`.
  - Edge case (derived from the code, not tested): if the count and `+` alone
    use up the room (`room <= 0`), no name is drawn, and the label can exceed
    `tabwidth`. Example: `tabwidth` 6 with a 3-digit count and `+`.
- Too many tabs: trailing tabs are dropped. Labels are drawn left to right, and
  the loop stops at the start of a label once `col >= Columns - 4` (`:690-693`).
  - A count that would reach `Columns - 3` stops the loop too (`:731-733`).
  - The last drawn label can run past `Columns - 4` by up to `tabwidth`.
  - No scrolling to the current tab. Names are not shortened to make the rest
    fit, except the per-name trim in Q2.
  - The `X` still shows if two or more labels were drawn (`:800`), so it can
    close a tab that is not on screen (`mouse.c:570-572`).

## 2. Label contents

Per label, in order (`:697-761`):

- Leading space, `' '` (`:716`).
- Window count, only if `wincount > 1` (`:728-730`), written with `TabLine`
  combined with `HLF_T` (`:734-736`). Counts only windows that are focusable and
  not hidden (`:718-726`), so floats and hidden windows are excluded.
- `+` if any counted window in that tab has a changed buffer (`:722-724`,
  `:738-740`). Not only the current window.
- Space (`:741`).
- Name (`:744-760`). Taken from `cwp->w_buffer`, where `cwp` is `curwin` for the
  current tab and `tp_curwin` otherwise (`:697-703`, `:747`).
  - `shorten_dir(NameBuff)` (`:748`) → `shorten_dir_len(str, 1)`
    (`path.c:315-319`, `:278-311`): each directory component is cut to one
    character, leading `~` and `.` kept, the file name is kept whole.
  - `get_trans_bufname` (`statusline.c:110-118`): `buf_spname()` for special
    buffers, else `home_replace()` (relative to `~`), then `trans_characters()`.
  - Trimmed from the left, one cell at a time, until it fits `room`
    (`:751-754`). The tail of the path is kept. No ellipsis is drawn.
  - Clamped to `Columns - col - 1` (`:755-756`), then written with
    `grid_line_puts(col, p, -1)` and `col += len` (`:758-759`). Unverified: the
    write uses the full string, so cells past `col` are overwritten by what
    follows.
- Trailing space (`:761`).
- No `|` between labels, and no padding after the name. The one exception is
  `use_sep_chars = (t_colors < 8)` (`:652`): then `|` is drawn before each
  label after the first (`:708-710`).
- Fill from the end of the labels to `Columns` (`:775-786`): `'_'` when
  `t_colors < 8`, else `' '` (`:785`), with `HLF_TPF`.
- `showcmd` at `tabline` is drawn at `Columns - sc_width - 2` when
  `tabcount > 1` (`:789-797`). Not part of the labels; included for completeness.

Example (not run): 2 tabs, 80 columns, `tabwidth = (79 + 1) / 2 = 40`. Tab 1
with two windows, one modified, on `~/src/x/y.lua`: ` 2+ ~/s/x/y.lua ` (shortened
form assuming `~/src`). Exact output not verified.

## 3. Highlight groups

- `attr_nosel = HLF_TP` (`TabLine`) (`:650`).
- `attr_fill = HLF_TPF` (`TabLineFill`) (`:651`). Used for the fill (`:786`).
- Current tab label: `win_hl_attr(cwp, HLF_TPS)` (`TabLineSel`) when
  `tp->tp_topframe == topframe` (`:705-707`). `win_hl_attr` honours `'winhighlight'`
  of that window.
- Other tab labels: `win_hl_attr(cwp, HLF_TP)` (`:712-714`, `:772`).
- Window count: `hl_combine_attr(attr, win_hl_attr(cwp, HLF_T))` (`:735`).
- `X`, `showcmd`: `attr_nosel` (`TabLine`) (`:795`, `:801`).
- Defaults (`highlight_group.c`): `TabLineSel` `gui=bold` (`:151`);
  `TabLine` → `StatusLineNC` (`:189`); `TabLineFill` → `TabLine` (`:190`).
- Enum: `HLF_TP`, `HLF_TPS`, `HLF_TPF` at `highlight_defs.h:118-120`; `HLF_T` at
  `:89`. The group name for `HLF_T` was not checked.

## 4. Close button

- Drawn at `Columns - 1` (`:800-801`), as the char `'X'` with `attr_nosel`.
- Condition: `tabcount > 1`, where `tabcount` is the number of labels drawn
  (incremented at `:765`), not the total number of tab pages (`:688`).
- Click: `tab_page_click_defs[Columns - 1]` with `tabnr = 999`,
  `kStlClickTabClose` (`:802-806`).
- `mouse.c:570-572` → `mouse_tab_close(999)` (`:142-158`). 999 = current tab.
  It closes only if `first_tabpage->tp_next != NULL` (`:151-154`).

## 5. Click handling

- Clicks are handled only when `mouse_row == 0` and `firstwin->w_winrow > 0`
  (`mouse.c:532-534`). Only `mouse_col < Columns` (`:543`).
- The array is `tab_page_click_defs`, indexed by screen column (`mouse.c:544`).
  There is no `TabPageIdxs` identifier at v0.12.0 (grep of the files above).
- Filling, `statusline.c:765-772`: each drawn label's cells get
  `{kStlClickTabSwitch, tabnr = tabcount}`, where `tabcount` is the 1-based
  index of the label. Labels are a prefix of the tab list, so this equals the
  tab page number.
- Cells after the last label, `:775-783`: `{kStlClickTabSwitch, tabnr = 0}`.
  - v0.12 only. Clicking there goes to the next tab (`mouse.c:558-560`).
  - Double-click there opens a new tab at the end (`mouse.c:553-556`, `tabnr == 0`
    → `tabpage_move(9999)`).
- `kStlClickTabSwitch` (`mouse.c:550-568`): left/right button → `goto_tabpage(tabnr)`.
  Double-click → `tabpage_new()` then move (`:552-556`). Middle button falls
  through to close (`:568-569`, `:570-571`).
- Drag (`mouse.c:535-538`, `:128-138`): `move_tab_to_mouse()` reorders tabs.
- `%`-expressions (`'tabline'`) can express some of the same click targets:
  - `%nT` (`tabnr = n`, switch), `%nX` (`tabnr = n`, close), `%999X` (close
    current). `%0T` is **disabled**, not "next tab" (`statusline.c:2106-2108`).
    Documented at `options.txt:6430-6437`.
  - Recorded by `stl_fill_click_defs(..., tab_page_click_defs, ...)`
    (`statusline.c:415`); tab-switch and tab-close records are kept only for the
    tabline (`:195-198`).
  - Same mechanism, not the same layout: the built-in's `tabnr` is per column,
    and its label widths and close button are computed in `draw_tabline`.
  - Correction (see the follow-up section): cells after the last `%T` are
    disabled, not "next tab".

## 6. Exposure as a function or expression

- No Vimscript or Lua function renders the built-in tabline.
  - `nvim_eval_statusline(str, {use_tabline=true})` (`api/vim.c`, `:2151`,
    `:2197`, `:2275`) evaluates the caller's string on `curwin`. It does not
    read `'tabline'` or call `draw_tabline`.
  - `:redrawtabline` does call `draw_tabline()` directly, whatever `'tabline'`
    is (`ex_docmd.c:7055-7068`, call at `:7064`). It is not a flag-only redraw.
- `ext_tabline` is the only other route. With `ui_has(kUITabline)` the built-in
  draws nothing (`statusline.c:659-662`). It sends `tabline_update(current, tabs,
  current_buffer, buffers)` (`ui_events.in.h:141`), built by
  `ui_ext_tabline_update` (`:600-644`). Tabs carry `tab` and `name` only
  (`:609-618`, `get_trans_bufname`, no shortening, no count, no `+`). Buffers
  are listed buffers with `buffer` and `name`.
- `'tabline'` cannot reproduce the default exactly. It can get close:
  - Width: `&columns` / `tabpagenr('$')` as in `:685`, with `%{%...%}` and
    `%<`. Not the same rounding, cell-width trimming, or the `room <= 0` edge.
  - Names: no `shorten_dir` (`path.c:278-311`) equivalent in Vimscript. No
    `buf_spname` mapping.
  - Count: needs the focusable/hidden filter (`:718-726`).
  - Highlights: `%#TabLineSel#` etc. ignores the `winhighlight` lookup of the
    window (`:705-714`).
  - `X`: needs the drawn-label count, not the tab count (`:800`).
- `tablineat` appears in the `has()` feature list (`eval/funcs.c:2763`). No
  `f_tablineat` implementation and no entry in `runtime/doc/builtin.txt` were
  found at v0.12.0.

## 7. Empty result from `'tabline'`

- The built-in is chosen by `*p_tal != NUL` (`statusline.c:673`). The result is
  not checked.
- So `'tabline'` set to an expression that returns `""` does not fall back.
  - `win_redr_custom(NULL, ...)` (`:674`, `:216`): `stl = p_tal` (`:245`),
    `maxwidth = Columns` (`:250`).
  - `build_stl_str_hl` returns empty (`:337-338`).
  - `grid_line_fill(col, Columns, ' ', HLF_TPF)` (`:403-406`) paints the whole
    row with `TabLineFill`. Result: a blank row in `TabLineFill`, not the
    built-in tabs.
- To get the built-in back, set `'tabline'` to `""` (the option, not the
  expression result).

## v0.10 → v0.12 `draw_tabline` diff

Same in both: `tabwidth` formula and floor 6 (v0.10 `statusline.c:739-743`;
v0.12 `:685`), label layout, name trim, `room` cap, `X` at `Columns - 1` with
`tabnr 999` on `tabcount > 1`, the `_`/`' '` fill, highlight choice, and the
`ui_has(kUITabline)` and `tabline_height()` guards. Changed in v0.12:

- Count and `+` skip windows that are not focusable or are hidden (`:718-726`).
  v0.10 counted and checked every window in the list (`statusline.c:779-783`).
  This is a material change for floats and hidden windows.
- Cells after the last label get `tabnr 0` / `kStlClickTabSwitch` (`:775-783`).
  New in v0.12.
- Name clamp is written as `MIN(len, Columns - col - 1)` (`:755-756`); v0.10 used
  an explicit `if`. Same result.
- `showcmd` width is written with `n` (`:790-791`); v0.10 computed it inline.
  Same result.
- Refactors without behaviour change: `grid_line_start(&default_gridview, 0)`
  (`:680`, v0.10 `default_grid`); `win_redr_custom` gained a 4th argument
  `ui_event` (`:674`).

Not compared: `shorten_dir`, `get_trans_bufname`, `mouse.c` click handling, and
`tabline_height()` in v0.10. The v0.10 click-handling switch is at
`mouse.c:496-538`, which I did not diff.

## Follow-up: mouse, showcmd, `'tabline'` checks, redraw timing (v0.12.0)

Method: GitHub source at v0.12.0 only. No local `nvim` run: this environment has
no shell tool, so nothing below is executed. Items marked "not verified" were
read from code, not tested.

### (a) Mouse on a custom vs. empty `'tabline'`

Dispatch is the same for both (`mouse.c:532-578`). Only the contents of
`tab_page_click_defs[]` differ. Each column's record comes from one of two
places:

- Built-in: `draw_tabline` clears the array (`statusline.c:668-670`), then
  writes labels (`:765-772`), cells after the last label (`:775-783`,
  `tabnr 0`), and the `X` (`:802-806`).
- Custom: `win_redr_custom` fills all `Columns` cells from the parsed string
  (`statusline.c:403-415`). `stl_fill_click_defs` (`:183-208`) gives each cell
  the def of the last record starting at or before it, so cells after the last
  `%T`-style record keep that record's def.

Answers:

- **Double-click on empty space opens a new tab?** Built-in: yes
  (`mouse.c:553-556`, `tabnr 0` → `tabpage_move(9999)`). Custom: no. The only
  way to write a tab-switch with `tabnr 0` is `%0T`, and `%0T` is recorded as
  `kStlClickDisabled` (`statusline.c:2106-2108`), so the click does nothing
  (`mouse.c:547-548`). Custom tablines cannot reproduce this.
- **Click past the last label switches tabs?** Built-in: yes. `tabnr 0` →
  `goto_tabpage(0)` (`mouse.c:560`; the comment at `:558-559` says "or next one
  if not clicking on a label"). Custom: only if the string has no closing `%T`.
  Then the last label's def runs to the end of the line
  (`stl_fill_click_defs`, `statusline.c:201-207`), so a click there switches to
  that label's tab, and a double-click there uses `tabnr` of the last label
  (`mouse.c:552-556`), not the end. With a closing `%T` (or `%0T`), the rest is
  disabled.
- **Does `%999X` close the current tab like the built-in X?** Yes, same path.
  The close label records `kStlClickTabClose` with `tabnr` = the close number,
  with the sign handling at `statusline.c:1252-1254` and `:2110-2117`. The
  built-in X records `kStlClickTabClose`, `tabnr 999` (`:802-806`). Both go to
  `mouse.c:570-572` → `mouse_tab_close(999)` (`:142-158`), which closes the
  current tab only if `first_tabpage->tp_next != NULL` (`:151-154`).
  Differences: the built-in X is only drawn at `Columns - 1` and only when two
  or more labels are drawn (`statusline.c:800-806`). `%999X` can be anywhere in
  the string. With one tab page, both are no-ops.
- **Middle button on a label** closes that tab in both cases:
  `kStlClickTabSwitch` falls through to close for non-left buttons
  (`mouse.c:551-569`).

### (b) `'showcmdloc'` = `tabline`

- Built-in: drawn only in the built-in branch, when `p_sc && *p_sloc == 't'`
  (`statusline.c:788-797`), at `Columns - sc_width - (tabcount > 1) * 2`, width
  `MIN(10, n)` (`:790-795`). Absent when `'tabline'` is custom, because that
  branch is skipped.
- Custom: needs `%S` in the string. `STL_SHOWCMD` shows `showcmd_buf` only if
  `p_sc && find_option(p_sloc) == opt_idx`, with `opt_idx = kOptTabline` for the
  tabline (`statusline.c:251`, `:1512-1516`). Without `%S`, nothing is shown.
  With `'showcmdloc'` not `tabline`, `%S` shows nothing in the tabline.
- Updates: `display_showcmd` (`normal.c:2083-2089`) calls `draw_tabline()`
  immediately when showcmd is non-empty (so a custom `'tabline'` expression is
  evaluated on each showcmd change). When it clears, it sets `redraw_tabline`.

### (c) Other places that test `'tabline'` being empty / non-empty

Searched in about 35 v0.12.0 source files (some paths may have failed
silently in the fetch; I did not list the misses). Hits on `p_tal` /
`kOptTabline` / `redraw_tabline` / `'tabline'`:

- `statusline.c:245` (`win_redr_custom` source) and `:673` (`draw_tabline`
  gate). Only these two read `p_tal` to decide what to draw.
- `ex_getln.c:884-885`: entering the command line sets `redraw_tabline` if
  `'tabline'` is non-empty. A custom tabline is redrawn on each `:`/`/` entry;
  the built-in is not affected.
- `optiondefs`/`options.lua:9259-9261`: `'tabline'` has `redraw = { 'tabline' }`.
  That is the only option with this flag found in the search. The flag sets
  `redraw_tabline` on any option set (`option.c:3194-3196`).
- `ext_tabline`: `draw_tabline` checks `ui_has(kUITabline)` (`:659-662`) before
  reading `p_tal`, and `tabline_height()` returns 0 (`window.c:7343-7344`). So
  with `ext_tabline` the `'tabline'` expression is never evaluated, and the
  built-in is not drawn either. The UI gets `tabline_update` from
  `ui_ext_tabline_update` regardless of `'tabline'`.
- `:redrawtabline`: calls `draw_tabline()` (`ex_docmd.c:7064`) regardless of
  `'tabline'`. With `'tabline'` set, it re-evaluates the expression; with it
  empty, it draws the built-in.
- `nvim__redraw({tabline = true})` (`api/vim.c:2492-2499`): calls
  `draw_tabline` with no `p_tal` test.

### (d) When the tabline is evaluated and redrawn

- A custom `'tabline'` is evaluated only when `win_redr_custom` runs, which
  happens only when `draw_tabline` runs past its gates. So `%!`/`%{}` run once
  per tabline draw, not on a timer. A re-entrancy guard applies
  (`statusline.c:237-240`).
- `draw_tabline` runs when:
  - `update_screen` has `redraw_tabline` set, or `type >= UPD_NOT_VALID`
    (`drawscreen.c:616-623`). Any full redraw (`UPD_NOT_VALID`) covers it.
  - The end of `redraw` if `redraw_tabline` is set (`drawscreen.c:2780-2782`).
  - `display_showcmd` (see (b)); `:redrawtabline`; `nvim__redraw`.
- Sources of `redraw_tabline = true` (verified in code):
  - `screenclear` (`drawscreen.c:240-244`, also `redraw_all_later(UPD_NOT_VALID)`
    at `:241`).
  - `update_screen` start, when namespace highlights change (`drawscreen.c:590-594`).
  - `changed()` when the buffer goes unmodified → modified, via
    `changed_internal` (`change.c:112-114`, `:151-157`).
  - `unchanged()` when the buffer goes modified → unmodified, or the file format
    changes (`change.c:606-616`).
  - Closing a tab page (`window.c:3295`), `win_new_tabpage` (`:4570`),
    `tabpage_move` (`:5019`).
  - `win_enter_ext` (`window.c:5319`). This runs on window entry. Whether tab
    switches go through it: not verified.
  - Any option set with `kOptFlagRedrTabl` (`option.c:3194-3196`); only
    `'tabline'` has it (`options.lua:9261`).
  - `set_fileformat` (`option.c:6460`), `redraw_titles` (`option.c:1904`),
    `shorten_fnames` (`fileio.c:2389`; caller not traced).
  - Showcmd being cleared (`normal.c:2085`).
- Not set directly by `buffer.c` (no `redraw_tabline` there). So `BufEnter` /
  `BufLeave`, cursor moves and text edits do not reach the tabline except by
  one of the triggers above. Whether a plain buffer switch reaches it: not
  verified.
- The built-in uses the same gating and the same triggers, so the redraw timing
  is identical. Only the content evaluation differs. The built-in's counts and
  `+` are also read at draw time, so they go stale in the same way.

### Corrections to the main sections

- Section 5: `%0T` is disabled (`statusline.c:2106-2108`), not "next tab".
  Cells after the last `%T` are disabled; with no closing `%T`, the last label's
  def extends to the line end (`:201-207`).
- Section 6: `:redrawtabline` calls `draw_tabline()` directly (`ex_docmd.c:7064`);
  it is not a flag-only redraw.

## Sources

- `src/nvim/statusline.c` @ v0.12.0 (lines above), @ v0.10.0 (draw_tabline at `:699`).
- `src/nvim/drawscreen.c` @ v0.12.0: `:240-246`, `:590-624`, `:2780-2782`.
- `src/nvim/mouse.c` @ v0.12.0: `:126-138`, `:142-158`, `:532-580`.
- `src/nvim/window.c` @ v0.12.0: `:3295`, `:4570`, `:5019`, `:5319`, `:7341-7352`.
- `src/nvim/path.c` @ v0.12.0: `:274-319`.
- `src/nvim/highlight_defs.h` @ v0.12.0: `:89`, `:118-120`.
- `src/nvim/highlight_group.c` @ v0.12.0: `:151`, `:189-190`.
- `src/nvim/api/vim.c` @ v0.12.0: `:2151`, `:2197`, `:2275`, `:2492-2499`.
- `src/nvim/api/ui_events.in.h` @ v0.12.0: `:141`.
- `src/nvim/eval/funcs.c` @ v0.12.0: `:2763`.
- `src/nvim/ex_docmd.c` @ v0.12.0: `:7055-7069`.
- `src/nvim/ex_getln.c` @ v0.12.0: `:884-885`.
- `src/nvim/change.c` @ v0.12.0: `:112-157`, `:606-616`.
- `src/nvim/fileio.c` @ v0.12.0: `:2389`.
- `src/nvim/normal.c` @ v0.12.0: `:2083-2089`.
- `src/nvim/option.c` @ v0.12.0: `:1904`, `:3194-3196`, `:6460`.
- `src/nvim/options.lua` @ v0.12.0: `:9259-9261`.
- `runtime/doc/options.txt` @ v0.12.0: `:5916-`, `:6430-6437`, `:6667-6686`.
