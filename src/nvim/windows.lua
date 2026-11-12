-- neorex in Neovim: every window except one hidden anchor per tab page is
-- external, shown in its own Rex pane. Run once with nvim_exec_lua(tool,
-- tool_title, chan, cols, rows), `tool` the tool's name and `tool_title`
-- it capitalised; returns { first window, its anchor }.
local tool, tool_title, chan, cols, rows = ...
local api = vim.api
local M = {}
_G[tool] = M

-- tells the daemon: the note `<tool>_<method>`
local function note(method, ...)
  vim.rpcnotify(chan, tool .. '_' .. method, ...)
end

-- tab page -> anchor window
local anchors = {}
-- the window last reported as current
local last
-- tab page made after setup -> { back = the window current before it }.
-- Its windows show in a tab page area, and it closes with the last.
-- The record also holds its window layout, as the daemon is told of it, and
-- how many of its windows are being reopened (`reopening`).
local floating = {}
-- a tab page without a tab page area -> its window layout, as the daemon is
-- told of it: the daemon decides which tab pages it places
local layouts = {}
-- window -> the tab page it belongs to: the one it was made in, or its
-- origin's for a split. External windows follow the current tab page, so
-- that's the only way to tell whose they are.
local home = {}
-- float -> the tab page it opened in, once it has been carried out of it
-- (see `M.carry`)
local float_home = {}
-- tab page -> its own window focused last, which names its tabline label
local focused = {}
-- closing a window because its pane went away
local closing_ours = false
-- the tab page closing, with every window in it
local closing_tab
local group = api.nvim_create_augroup(tool, {})

local function is_anchor(win)
  for _, a in pairs(anchors) do
    if a == win then return true end
  end
  return false
end

local function anchor_of(tab)
  local a = anchors[tab]
  if a and api.nvim_win_is_valid(a) then return a end
  anchors[tab] = nil
end

-- a window a pane shows: external, or ordinary and not an anchor
local function shown(win)
  if not api.nvim_win_is_valid(win) or is_anchor(win) then return false end
  local c = api.nvim_win_get_config(win)
  return c.external or c.relative == ''
end

-- The tab page `win` belongs to, for the tabline: its home tab page; for a
-- float on a window, that window's; for another ordinary float, the one it
-- floats in. Nil for an external window with no home.
local function tab_of(win)
  local c = api.nvim_win_get_config(win)
  if c.external then return home[win] end
  if c.relative == 'win' and c.win and api.nvim_win_is_valid(c.win) then return tab_of(c.win) end
  if c.relative ~= '' then return float_home[win] or api.nvim_win_get_tabpage(win) end
  return home[win] or api.nvim_win_get_tabpage(win)
end

-- The tab page whose tabline label counts `win`, as `tab_of` says. Nil for an
-- anchor, and a window that takes no focus or is hidden.
local function counts_for(win)
  if not api.nvim_win_is_valid(win) or is_anchor(win) then return end
  local c = api.nvim_win_get_config(win)
  if c.focusable == false or c.hide then return end
  return tab_of(win)
end

-- `win` took focus: it names its tab page's label from now on. Not while a
-- tab page closes, which passes focus through each of its windows, other tab
-- pages' that followed into it too.
local function note_focus(win)
  local tab = not closing_tab and counts_for(win)
  if tab then focused[tab] = win end
end

-- Each tab page's own windows: tab page -> { wins = the windows its label
-- counts, in Neovim's order, label = the one it names }. That's its own
-- window focused last, else its first shown one, else the first Neovim
-- keeps in it. Not the one Neovim remembers for it: that may be an external
-- window sitting in another tab page, and nvim_tabpage_get_win() ends Neovim
-- then (0.12.6).
local function own_windows()
  local by_tab = {}
  for _, tab in ipairs(api.nvim_list_tabpages()) do by_tab[tab] = { wins = {} } end
  for _, win in ipairs(api.nvim_list_wins()) do
    local mine = by_tab[counts_for(win) or 0]
    if mine then mine.wins[#mine.wins + 1] = win end
  end
  for tab, mine in pairs(by_tab) do
    local label = focused[tab]
    if not (label and counts_for(label) == tab) then
      label = nil
      for _, win in ipairs(mine.wins) do
        if shown(win) then
          label = win
          break
        end
      end
    end
    mine.label = label or api.nvim_tabpage_list_wins(tab)[1]
  end
  return by_tab
end

local function others_shown(win)
  for _, w in ipairs(api.nvim_list_wins()) do
    if w ~= win and shown(w) then return true end
  end
  return false
end

local making_anchor = false

-- an anchor's own buffer: a scratch one, wiped once another replaces it
local function anchor_buf()
  local buf = api.nvim_create_buf(false, true)
  vim.bo[buf].bufhidden = 'wipe'
  vim.b[buf][tool .. '_anchor'] = true
  return buf
end

-- window -> its pane: `w` x `h`, the rows and columns, statusline included.
-- Layout plugins move windows back into Neovim's layout (see `restore`), so
-- it also has `fight`: `restored`, when we put it back (ms), `off_until`,
-- until when we leave it alone, and `off_for`, how long that was the last time.
local panes = {}
-- window -> { w, h }, statusline included: the size Neovim gave it itself, as
-- last told the daemon, until the daemon sizes it again
local reported_sizes = {}
-- window -> { w, h }: the size Neovim gave a window not yet external as
-- windows split or the screen changed. Another size when it's adopted is one
-- Neovim gave it as asked, as `:split | resize 8` does, and is told.
local laid_sizes = {}
-- when the user last typed, or a pane was resized (ms)
local last_activity = 0
-- statusline formats we've set, to tell them from ones the user set
local ours = {}
-- laststatus=3 was set, and is 2 while neorex runs: every pane shows the one
-- statusline a global one would, the current window's
local mirroring = false
-- window -> { text = the current window's statusline we last gave it, own = what
-- it showed before }
local copied = {}
-- float -> the local statusline it had before we hid it while mirroring
local hidden_stl = {}

local function now_ms()
  return vim.uv.hrtime() / 1e6
end

-- the window's pane, made if need be
local function pane_of(win)
  panes[win] = panes[win] or { fight = { restored = {}, off_until = 0, off_for = 0 } }
  return panes[win]
end

-- we've stopped putting `win` back in its pane for a while
local function waiting(win)
  local p = panes[win]
  return p ~= nil and now_ms() < p.fight.off_until
end

-- edgy.nvim lays windows out again on every resize, and its animation never
-- settles with external windows. It leaves ones marked like this alone.
local function keep_edgy_out(win)
  vim.w[win].edgy_disable = true
end

local function local_stl(win)
  return api.nvim_get_option_value('statusline', { win = win, scope = 'local' })
end

local function set_local_stl(win, stl)
  api.nvim_set_option_value('statusline', stl, { win = win, scope = 'local' })
end

local function has_statusline(win)
  return vim.go.laststatus ~= 0 and local_stl(win) ~= ''
end

-- What a global statusline shows in `win`: the current window's own, which
-- lualine's globalstatus keeps up to date. Nil if that's `win` itself, or the
-- current window isn't a pane (a float, say) or has none: `win` keeps what it
-- shows.
local function global_line(win)
  local cur = api.nvim_get_current_win()
  if cur == win or not shown(cur) then return end
  local stl = local_stl(cur)
  if stl ~= '' then return stl end
end

-- Gives external `win` a local statusline, as they only show those: the global
-- one, or the current window's while mirroring.
local function give_statusline(win)
  local cur = local_stl(win)
  local line = mirroring and global_line(win)
  if line then
    if cur ~= line then set_local_stl(win, line) end
    local c = copied[win]
    copied[win] = { text = line, own = (c and c.text == cur) and c.own or cur }
    return
  end
  local c = copied[win]
  if c and c.text == cur and win == api.nvim_get_current_win() then
    -- the copy of another window's statusline isn't the current window's
    cur = c.own
    set_local_stl(win, cur)
    copied[win] = nil
  end
  if cur ~= '' and (mirroring or not ours[cur]) then return end
  local want = vim.go.laststatus == 0 and '' or vim.go.statusline
  if want ~= '' then ours[want] = true end
  if cur ~= want then set_local_stl(win, want) end
end

-- Each pane's statusline is its own again: what it showed before a copy.
-- Each float's we hid, too, unless it was set since.
local function stop_mirroring()
  mirroring = false
  for win, c in pairs(copied) do
    if api.nvim_win_is_valid(win) and local_stl(win) == c.text then set_local_stl(win, c.own) end
  end
  copied = {}
  for win, stl in pairs(hidden_stl) do
    if api.nvim_win_is_valid(win) and local_stl(win) == '' then set_local_stl(win, stl) end
  end
  hidden_stl = {}
end

-- the text's height: what's left of the pane after the statusline, if it has
-- one
local function text_height(win, h)
  return math.max(h - (has_statusline(win) and 1 or 0), 1)
end

--- Sizes external `win` to fill `w` x `h`, statusline included.
function M.size(win, w, h)
  if not shown(win) then return end
  local p = pane_of(win)
  -- a pane the user resized is no fight
  if p.w ~= w or p.h ~= h then last_activity = now_ms() end
  -- remembered while we wait, for when the window goes back in its pane
  p.w, p.h = w, h
  reported_sizes[win] = nil
  if waiting(win) then return end
  give_statusline(win)
  local th = text_height(win, h)
  -- every resize redraws the window
  if api.nvim_win_get_width(win) == w and api.nvim_win_get_height(win) == th then return end
  api.nvim_win_set_config(win, { width = w, height = th })
end

local function refit()
  for win, p in pairs(panes) do
    if api.nvim_win_is_valid(win) then M.size(win, p.w, p.h) else panes[win] = nil end
  end
end

-- Fits the windows to their panes again, once however many statuslines change
-- meanwhile: refit's own writes fire OptionSet too.
local refit_pending = false
local function request_refit()
  if refit_pending then return end
  refit_pending = true
  vim.schedule(function()
    local ok, err = pcall(refit)
    refit_pending = false
    if not ok then error(err, 0) end
  end)
end

-- Most nodes a layout can have for the daemon to read, a row, a column or a
-- window each. The same as `Layout::MAX_NODES` in windows.rs.
local MAX_LAYOUT_NODES = 63

-- A tab page's `layout`: nested windows { win, w, h } and rows and
-- columns { dir, kids, w, h }, in cells. Once a window is external it's out
-- of winlayout(), so the layout is kept here, and told to the daemon as it
-- changes. `told` is what it last heard, flattened.

-- the width and height of a row or column `dir` of `kids`
local function extent(dir, kids)
  local w, h = 0, 0
  for _, kid in ipairs(kids) do
    if dir == 'row' then
      w, h = w + kid.w, math.max(h, kid.h)
    else
      w, h = math.max(w, kid.w), h + kid.h
    end
  end
  return w, h
end

-- Scales `node` to `w` x `h`, keeping the shares of what's in it. Every kid
-- keeps a cell, so a row of many in one column gets wider than `w`.
local function fit_node(node, w, h)
  local old_w, old_h = math.max(node.w, 1), math.max(node.h, 1)
  node.w, node.h = w, h
  if not node.kids then return end
  local row = node.dir == 'row'
  local total, old_total = row and w or h, row and old_w or old_h
  local used = 0
  for i, kid in ipairs(node.kids) do
    local size = i == #node.kids and total - used or math.floor((row and kid.w or kid.h) * total / old_total + 0.5)
    size = math.max(size, 1)
    used = used + size
    if row then fit_node(kid, size, h) else fit_node(kid, w, size) end
  end
  node.w, node.h = extent(node.dir, node.kids)
end

-- `node` of an earlier layout without the windows in `drop` and any gone
local function prune(node, tab, drop)
  if not node.kids then
    if not drop[node.win] and shown(node.win) and home[node.win] == tab then return node end
    return
  end
  local kids = {}
  for _, kid in ipairs(node.kids) do
    kid = prune(kid, tab, drop)
    if kid then kids[#kids + 1] = kid end
  end
  if #kids <= 1 then return kids[1] end
  node.kids = kids
  node.w, node.h = extent(node.dir, kids)
  return node
end

-- Whether Neovim sized external `win`, not us fitting it to its pane
local function sized_by_neovim(win)
  local p = panes[win]
  -- one waiting to be put back isn't fitted to its pane, whatever size it has
  if not p or not p.w or waiting(win) then return false end
  -- the pane's rows are the text and a statusline
  return api.nvim_win_get_width(win) ~= p.w or math.abs(api.nvim_win_get_height(win) - text_height(win, p.h)) > 1
end

-- Takes the sizes Neovim gave the external windows in `node` since
local function take_sizes(node)
  if not node.kids then
    if api.nvim_win_get_config(node.win).external and sized_by_neovim(node.win) then
      node.w, node.h = api.nvim_win_get_width(node.win), api.nvim_win_get_height(node.win)
    end
    return
  end
  for _, kid in ipairs(node.kids) do take_sizes(kid) end
  node.w, node.h = extent(node.dir, node.kids)
end

-- `node` of winlayout() as one of ours. The anchor stands for the windows
-- already external, which are `external`.
local function layout_from_winlayout(node, anchor, external)
  if node[1] == 'leaf' then
    local win = node[2]
    local w, h = api.nvim_win_get_width(win), api.nvim_win_get_height(win)
    if win ~= anchor then return { win = win, w = w, h = h } end
    if external then fit_node(external, w, h) end
    return external
  end
  local kids = {}
  for _, child in ipairs(node[2]) do
    local kid = layout_from_winlayout(child, anchor, external)
    if kid and kid.dir == node[1] then
      vim.list_extend(kids, kid.kids)
    elseif kid then
      kids[#kids + 1] = kid
    end
  end
  if #kids <= 1 then return kids[1] end
  local w, h = extent(node[1], kids)
  return { dir = node[1], kids = kids, w = w, h = h }
end

-- the windows in winlayout() `node`, added to `set`
local function winlayout_windows(node, set)
  if node[1] == 'leaf' then
    set[node[2]] = true
  else
    for _, kid in ipairs(node[2]) do winlayout_windows(kid, set) end
  end
  return set
end

-- the windows in one of our layouts, added to `set`
local function layout_windows(node, set)
  if node.kids then
    for _, kid in ipairs(node.kids) do layout_windows(kid, set) end
  else
    set[node.win] = true
  end
  return set
end

-- puts window `new` where `old` is in one of our layouts, `node`
local function swap_win(node, old, new)
  if node.win == old then node.win = new end
  for _, kid in ipairs(node.kids or {}) do swap_win(kid, old, new) end
end

-- node kind (0 window, 1 row, 2 column), then its window or kid count, its
-- width and height, then its kids
local function flatten(node, out)
  if node.kids then
    vim.list_extend(out, { node.dir == 'row' and 1 or 2, #node.kids, node.w, node.h })
    for _, kid in ipairs(node.kids) do flatten(kid, out) end
  else
    vim.list_extend(out, { 0, node.win, node.w, node.h })
  end
  return out
end

-- The record holding tab page `tab`'s layout: its floating one, else one of
-- its own while it's open.
local function layout_record(tab)
  if floating[tab] then return floating[tab] end
  if not api.nvim_tabpage_is_valid(tab) then return nil end
  layouts[tab] = layouts[tab] or {}
  return layouts[tab]
end

-- Takes tab page `tab`'s layout from winlayout(), while its windows aren't
-- external, and from what it was and the sizes Neovim has since given
-- external ones; tells the daemon if it changed. Not while any of its windows
-- is being reopened.
local function refresh_layout(tab)
  local t = layout_record(tab)
  if not t or (t.reopening or 0) > 0 or not api.nvim_tabpage_is_valid(tab) then return end
  local anchor = anchor_of(tab)
  local node = vim.fn.winlayout(api.nvim_tabpage_get_number(tab))
  local ordinary = winlayout_windows(node, {})
  ordinary[anchor or 0] = nil
  local earlier = t.layout
  local known = earlier and layout_windows(earlier, {}) or {}
  local any_new = false
  for win in pairs(ordinary) do any_new = any_new or not known[win] end
  local latest
  if earlier and anchor and not any_new then
    -- making windows external moves the others about, so their sizes in
    -- winlayout() aren't the ones they were given
    latest = prune(earlier, tab, {})
    if latest then take_sizes(latest) end
  else
    -- without an anchor, nothing is external: winlayout() has it all
    earlier = anchor and earlier and prune(earlier, tab, ordinary)
    if earlier then take_sizes(earlier) end
    latest = layout_from_winlayout(node, anchor, earlier or nil)
  end
  t.layout = latest
  local flat = latest and flatten(latest, {}) or {}
  -- more than the daemon reads: it's told there's none, and places nothing
  if #flat > 4 * MAX_LAYOUT_NODES then flat = {} end
  local key = table.concat(flat, ',')
  if t.told == key or (t.told == nil and #flat == 0) then return end
  t.told = key
  note('layout', tab, flat)
end

-- after the command that changed it, which may change it more
local function schedule_layout(tab)
  local t = layout_record(tab)
  if not t or t.pending then return end
  t.pending = true
  vim.schedule(function()
    t.pending = false
    if layout_record(tab) == t then refresh_layout(tab) end
  end)
end

-- The sizes of the windows not yet external, after Neovim has laid them out
-- again: for a new window, a new screen size, or one made external.
local function note_laid_sizes()
  for _, win in ipairs(api.nvim_tabpage_list_wins(0)) do
    if shown(win) and not api.nvim_win_get_config(win).external then
      laid_sizes[win] = { api.nvim_win_get_width(win), api.nvim_win_get_height(win) }
    end
  end
end

--- Makes ordinary `win` external at `w` x `h`, giving its tab page an anchor first.
function M.adopt(win, w, h)
  if not shown(win) or waiting(win) or api.nvim_win_get_config(win).external then return end
  local was = laid_sizes[win]
  laid_sizes[win] = nil
  local cols, rows = 0, 0
  if was then
    local now = { api.nvim_win_get_width(win), api.nvim_win_get_height(win) }
    if now[1] ~= was[1] then cols = now[1] end
    if now[2] ~= was[2] then rows = now[2] end
  end
  keep_edgy_out(win)
  local tab = api.nvim_win_get_tabpage(win)
  -- before it leaves winlayout()
  refresh_layout(tab)
  if not anchor_of(tab) then
    making_anchor = true
    anchors[tab] = api.nvim_open_win(anchor_buf(), false, { split = 'left', win = win })
    making_anchor = false
  end
  give_statusline(win)
  api.nvim_win_set_config(win, { external = true, width = w, height = text_height(win, h) })
  local p = pane_of(win)
  p.w, p.h = w, h
  if cols ~= 0 or rows ~= 0 then
    if rows ~= 0 then rows = rows + (has_statusline(win) and 1 or 0) end
    note('resized', win, cols, rows)
  end
  -- the windows left in the layout took its place: no size asked of them
  note_laid_sizes()
end

api.nvim_create_autocmd({ 'WinNew', 'VimResized' }, { group = group, callback = note_laid_sizes })
-- options that take or give rows of the screen
api.nvim_create_autocmd('OptionSet', {
  group = group,
  pattern = { 'cmdheight', 'showtabline', 'laststatus' },
  callback = note_laid_sizes,
})
api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev) laid_sizes[tonumber(ev.match)] = nil end,
})

-- Layout plugins such as edgy.nvim move windows back into Neovim's own
-- layout, as `wincmd J` does, on every resize. A window with a pane that has
-- been moved back is made external again.
--
-- A plugin that moves it back every time we put it in its pane would fight
-- us forever. That's `max_restores` restores within `fight_ms`, with nothing
-- typed and no pane resized since the first: then we leave the window alone
-- for a while, twice as long each time, and try again after. An error while
-- restoring waits `retry_ms`.
-- The README gives these numbers: 5 times in 2 seconds, then 2 s, 4 s, up to a minute.
local max_restores = 5
local fight_ms = 2000
local retry_ms = 500
local min_off_ms = 2000
local max_off_ms = 60000

local check_lost

-- `win` has a pane and isn't in it, and we aren't waiting to try again
local function is_lost(win)
  return shown(win) and not waiting(win) and not api.nvim_win_get_config(win).external
end

-- Looks again for lost windows once `p` has waited `delay` ms.
local function wait_for(p, delay)
  p.fight.off_until = now_ms() + delay
  local function again()
    -- the timer's clock can be a little behind ours
    local left = p.fight.off_until - now_ms()
    if left > 0 then return vim.defer_fn(again, math.ceil(left) + 1) end
    check_lost()
  end
  vim.defer_fn(again, delay + 10)
end

local function restore_one(win, p)
  local f = p.fight
  local now = now_ms()
  -- a fight is restores since the user last did anything
  local recent = vim.tbl_filter(function(t) return t > last_activity and now - t < fight_ms end, f.restored)
  local last = f.restored[#f.restored]
  if last and now - last > max_off_ms then f.off_for = 0 end
  if #recent >= max_restores then
    f.restored = {}
    f.off_for = math.min(math.max(f.off_for * 2, min_off_ms), max_off_ms)
    return wait_for(p, f.off_for)
  end
  table.insert(recent, now)
  f.restored = recent
  M.adopt(win, p.w, p.h)
end

local function restore()
  for win, p in pairs(panes) do
    if not api.nvim_win_is_valid(win) then
      panes[win] = nil
    elseif is_lost(win) and not pcall(restore_one, win, p) then
      wait_for(p, retry_ms)
    end
  end
end

local restore_queued = false
-- whenever Neovim waits for input: plugins move windows inside their own
-- autocommands, and often with 'eventignore' set. Schedules only when a
-- window is lost: SafeState fires again after every scheduled callback.
function check_lost()
  if restore_queued then return end
  for win in pairs(panes) do
    if is_lost(win) then
      restore_queued = true
      vim.schedule(function()
        restore_queued = false
        restore()
      end)
      return
    end
  end
end
api.nvim_create_autocmd({ 'SafeState', 'WinResized' }, { group = group, callback = check_lost })
-- keys the user typed, not ones a plugin runs with :normal or feedkeys
vim.on_key(function(_, typed)
  if typed ~= '' then last_activity = now_ms() end
end, api.nvim_create_namespace(tool))

--- Closes `win`, whose pane went away. Unsaved changes stay in its buffer.
function M.close(win)
  if not api.nvim_win_is_valid(win) then return end
  closing_ours = true
  pcall(api.nvim_win_close, win, true)
  closing_ours = false
  note_laid_sizes()
end

--- The windows a self-attached pane could take: the current window, then
--- each tab page's own in tab page order, each in window order. A flat list,
--- the current window first, never empty.
function M.windows()
  local by_tab = {}
  for _, win in ipairs(api.nvim_list_wins()) do
    if shown(win) then
      local tab = tab_of(win) or api.nvim_win_get_tabpage(win)
      by_tab[tab] = by_tab[tab] or {}
      table.insert(by_tab[tab], win)
    end
  end
  local out = { api.nvim_get_current_win() }
  for _, tab in ipairs(api.nvim_list_tabpages()) do
    for _, win in ipairs(by_tab[tab] or {}) do out[#out + 1] = win end
  end
  return out
end

-- `win` and its home tab page become current
local function enter(win)
  local tab = home[win]
  if api.nvim_win_get_config(win).external and tab and api.nvim_tabpage_is_valid(tab) then
    api.nvim_set_current_tabpage(tab)
  end
  api.nvim_set_current_win(win)
end

--- A self-attached pane takes `win`, an unshown window, at `w` x `h`: it
--- becomes external and current, its home tab page too. Returns it, or 0 if
--- it's gone.
function M.show(win, w, h)
  if not shown(win) then return 0 end
  enter(win)
  if api.nvim_win_get_config(win).external then
    M.size(win, w, h)
  else
    M.adopt(win, w, h)
  end
  return win
end

--- A self-attached pane's new window, external at `w` x `h` and current, in
--- the current tab page: on `file`, if given, its cursor on `line`, else on
--- the buffer the current window shows, as :split opens it. Returns it.
function M.open(file, line, w, h)
  if file == vim.NIL then file = nil end
  if line == vim.NIL then line = nil end
  local cur = api.nvim_get_current_win()
  if not shown(cur) and last and shown(last) then cur = last end
  local buf = shown(cur) and api.nvim_win_get_buf(cur) or api.nvim_create_buf(true, false)
  local view = shown(cur) and api.nvim_win_call(cur, vim.fn.winsaveview)
  -- made with no autocommands, so it's noted before one can flush a redraw
  -- that shows it: its pane is the one that asked. WinNew, then entering
  -- it, follow as they would have.
  local config = { external = true, width = w, height = math.max(h - 1, 1), noautocmd = true }
  local win = api.nvim_open_win(buf, false, config)
  note('opened', win)
  api.nvim_win_call(win, function() api.nvim_exec_autocmds('WinNew', { modeline = false }) end)
  api.nvim_set_current_win(win)
  keep_edgy_out(win)
  home[win] = api.nvim_get_current_tabpage()
  if file then
    local ok, err = pcall(vim.cmd.edit, { args = { file }, magic = { file = false } })
    if not ok then api.nvim_echo({ { tostring(err), 'ErrorMsg' } }, true, {}) end
    if line then
      local last_line = api.nvim_buf_line_count(api.nvim_win_get_buf(win))
      api.nvim_win_set_cursor(win, { math.min(line, last_line), 0 })
    end
  elseif view then
    vim.fn.winrestview(view)
  end
  M.size(win, w, h)
  return win
end

--- Fits the floats hanging from editor floats `tops` to `w` x `h` at `row`,
--- `col` on the screen, as their float group pane was resized to. `cur` is
--- where they are now: row, col, width, height. Returns whether it moved
--- anything.
function M.place_group(tops, row, col, w, h, cur)
  local set = {}
  for _, t in ipairs(tops) do set[t] = true end
  -- a snacks picker lays itself out again from its root box's size
  local ok, Snacks = pcall(require, 'snacks')
  if ok and type(Snacks) == 'table' and Snacks.picker then
    for _, p in ipairs(Snacks.picker.get() or {}) do
      local l = p.layout
      if l and l.root and set[l.root.win] then
        local o = l.opts.layout
        o.width, o.height, o.row, o.col = w, h, row, col
        l:update()
        return true
      end
    end
  end
  -- one float on its own grows by what its pane grew
  if #tops ~= 1 or not api.nvim_win_is_valid(tops[1]) then return false end
  local win = tops[1]
  local c = api.nvim_win_get_config(win)
  if c.relative ~= 'editor' then return false end
  api.nvim_win_set_config(win, {
    relative = 'editor',
    row = c.row + row - cur[1],
    col = c.col + col - cur[2],
    width = math.max(1, c.width + w - cur[3]),
    height = math.max(1, c.height + h - cur[4]),
  })
  return true
end

-- the layout node holding `win`: 'row' or 'col', and whether `win` is first
local function placement(win)
  local function find(node)
    if node[1] == 'leaf' then return end
    for i, c in ipairs(node[2]) do
      if c[1] == 'leaf' and c[2] == win then return node[1], i == 1 end
      local dir, first = find(c)
      if dir then return dir, first end
    end
  end
  return find(vim.fn.winlayout(api.nvim_tabpage_get_number(api.nvim_win_get_tabpage(win))))
end

-- before it's adopted, so its pane opens where Neovim put the window
api.nvim_create_autocmd('WinNew', {
  group = group,
  callback = function()
    if making_anchor then return end
    local win = api.nvim_get_current_win()
    local dir, first = placement(win)
    if not dir then
      -- the first window of a new tab page, as :tabnew, Neogit and Diffview
      -- open. External windows follow the current tab page, so it opens
      -- over them: its windows get a tab page area.
      local tab = api.nvim_win_get_tabpage(win)
      if api.nvim_win_get_config(win).relative == '' and not anchor_of(tab) then
        floating[tab] = floating[tab] or { back = last }
        home[win] = tab
        note('split', win, false, false, 0, tab)
        schedule_layout(tab)
      end
      return
    end
    local from = vim.fn.win_getid(vim.fn.winnr('#'))
    local tab = home[from] or api.nvim_win_get_tabpage(win)
    home[win] = tab
    -- Neovim doesn't keep the scroll when splitting an external window
    if shown(from) and api.nvim_win_get_buf(from) == api.nvim_win_get_buf(win) then
      local view = api.nvim_win_call(from, vim.fn.winsaveview)
      vim.fn.winrestview(view)
    end
    note('split', win, dir == 'row', first, shown(from) and from or 0, floating[tab] and tab or 0)
    schedule_layout(tab)
  end,
})

-- The window a buffer opened in `tab`'s anchor goes to: the one last
-- current, if it's that tab page's, else the tab page's own focused last,
-- else its first shown. Never one 'winfixbuf' keeps on its buffer.
local function anchor_target(tab)
  local function takes(win)
    return shown(win) and tab_of(win) == tab and not vim.wo[win].winfixbuf
  end
  for _, win in ipairs({ last or 0, focused[tab] or 0 }) do
    if takes(win) then return win end
  end
  for _, win in ipairs(api.nvim_list_wins()) do
    if takes(win) then return win end
  end
end

-- Pickers that open a file in the last ordinary window that isn't showing
-- a file can pick an anchor, as snacks.nvim's does when the pane shows oil
-- or a dashboard: nothing would show it. Moves such a buffer, with its view,
-- to a pane of the tab page, the one the user was in if it can, and gives
-- the anchor a scratch buffer again. Focus, if on the anchor, goes with it.
-- Returns whether it moved one.
local function evict(tab)
  local a = anchor_of(tab)
  if not a then return false end
  local buf = api.nvim_win_get_buf(a)
  if vim.b[buf][tool .. '_anchor'] then return false end
  local to = anchor_target(tab)
  if not to then return false end
  local view = api.nvim_win_call(a, vim.fn.winsaveview)
  -- :buffer, as the picker meant to, keeps the jump list; :hide, so a
  -- modified buffer it replaces stays, hidden, even with 'nohidden'. If it
  -- fails anyway, as for a modified one 'bufhidden' would unload, the buffer
  -- stays where it is.
  if not pcall(api.nvim_win_call, to, function() vim.cmd('hide buffer ' .. buf) end) then return false end
  api.nvim_win_call(to, function() vim.fn.winrestview(view) end)
  -- only once the pane shows it: one with 'bufhidden' set would go, hidden
  api.nvim_win_set_buf(a, anchor_buf())
  if api.nvim_get_current_win() == a then api.nvim_set_current_win(to) end
  return true
end

local evict_pending = false
local function evict_all()
  evict_pending = false
  for tab in pairs(anchors) do evict(tab) end
end
-- after the command, which may still move the cursor in it
api.nvim_create_autocmd('BufWinEnter', {
  group = group,
  callback = function()
    if making_anchor or evict_pending then return end
    evict_pending = true
    vim.schedule(evict_all)
  end,
})

-- The window of tab page `tab` to go to: its own focused last, else its
-- first shown one.
local function own_window(tab)
  local f = focused[tab]
  if f and shown(f) and tab_of(f) == tab then return f end
  for _, win in ipairs(api.nvim_list_wins()) do
    if shown(win) and tab_of(win) == tab then return win end
  end
end

-- a tab page was entered since the last report
local tab_entered = false
-- the windows last reported as current before `seen`, the last one: where
-- <C-w>p goes
local seen, previous
api.nvim_create_autocmd('TabEnter', {
  group = group,
  callback = function() tab_entered = true end,
})

local pending = false
local function report()
  pending = false
  local cur = api.nvim_get_current_win()
  local tab = api.nvim_get_current_tabpage()
  local entered = tab_entered
  tab_entered = false
  -- a buffer opened in an anchor: on to the pane it went to
  if is_anchor(cur) and evict(api.nvim_win_get_tabpage(cur)) then return end
  -- :tabnext, gt, a tabline click or a tab page closing went to the tab
  -- page's last current window, which may have closed or be another tab
  -- page's that only sat in it: its own instead
  if entered and (is_anchor(cur) or (shown(cur) and tab_of(cur) ~= tab)) then
    local to = own_window(tab)
    if to and to ~= cur then
      api.nvim_set_current_win(to)
      return
    end
  end
  if is_anchor(cur) then
    -- never shown: move on the way <C-w>w or <C-w>W was going
    local wins = vim.tbl_filter(shown, api.nvim_tabpage_list_wins(0))
    if #wins == 0 then return end
    local prev = vim.fn.win_getid(vim.fn.winnr('#'))
    local to = (last and shown(last)) and last or wins[1]
    if prev == wins[#wins] then
      to = wins[1]
    elseif prev == wins[1] then
      to = wins[#wins]
    end
    api.nvim_set_current_win(to)
    return
  end
  -- a float shows in the pane it floats over, which stays focused, unless
  -- it's in a float group pane: the daemon decides
  if api.nvim_win_get_config(cur).relative ~= '' then
    if cur ~= seen then previous, seen = seen, cur end
    note('float', cur)
    return
  end
  -- another tab page's window, made current inside Neovim, as a plugin's
  -- nvim_set_current_win does: its tab page comes along
  local mine = home[cur]
  if not entered and mine and mine ~= tab and api.nvim_tabpage_is_valid(mine) then
    M.go(cur)
    return
  end
  last = cur
  if cur ~= seen then previous, seen = seen, cur end
  note('win', cur)
end

-- after the command that moved, so :windo and friends report once
local function schedule_report()
  if pending then return end
  pending = true
  vim.schedule(report)
end

-- The editor floats of the float groups with float group panes, as the
-- daemon last said. Each, with the floats hanging from it, moves into
-- whichever tab page is current, so its pane never blanks, and still belongs
-- to the one it opened in (`float_home`): the tabline counts it there, it
-- closes with it, and goes back to it before the tab page it sits in closes.
-- Every other float stays in its tab page, hidden while that isn't current.
local carried = {}

-- The editor float `win` hangs from, through floats on floats, else
-- itself; and how many floats down from it `win` is.
local function float_root(win)
  local depth = 0
  local c = api.nvim_win_get_config(win)
  while depth < 50 and c.relative == 'win' and c.win and api.nvim_win_is_valid(c.win) do
    local up = api.nvim_win_get_config(c.win)
    if up.relative == '' then break end
    win, c, depth = c.win, up, depth + 1
  end
  return win, depth
end

-- The floats sitting in tab page `tab` (every tab page but it, with `others`)
-- that `pick(float, its root)` picks, the ones they hang from first.
local function floats_in(tab, others, pick)
  local found = {}
  for _, w in ipairs(api.nvim_list_wins()) do
    local c = api.nvim_win_get_config(w)
    if c.relative ~= '' and not c.external and (api.nvim_win_get_tabpage(w) == tab) ~= others then
      local root, depth = float_root(w)
      if pick(w, root) then found[#found + 1] = { w, depth } end
    end
  end
  table.sort(found, function(x, y) return x[2] < y[2] end)
  return vim.tbl_map(function(f) return f[1] end, found)
end

-- a window of tab page `tab` to name, to move floats into it
local function window_of(tab)
  local a = anchor_of(tab)
  if a then return a end
  for _, w in ipairs(api.nvim_tabpage_list_wins(tab)) do
    if api.nvim_win_get_config(w).relative == '' then return w end
  end
end

-- Moves `floats` into tab page `tab`, each where it floats, keeping its id.
-- One Neovim can't move stays where it is.
local function move_floats(floats, tab)
  local to = window_of(tab)
  if not to then return end
  for _, f in ipairs(floats) do
    local from = api.nvim_win_get_tabpage(f)
    if from ~= tab then
      local c = api.nvim_win_get_config(f)
      local moved = pcall(api.nvim_win_set_config, f, {
        relative = c.relative,
        win = c.relative == 'win' and c.win or to,
        anchor = c.anchor,
        row = c.row,
        col = c.col,
        bufpos = c.bufpos,
      })
      if moved then float_home[f] = float_home[f] or from end
    end
  end
end

-- the carried floats into the current tab page
local function bring_carried()
  local tab = api.nvim_get_current_tabpage()
  move_floats(floats_in(tab, true, function(_, root) return carried[root] end), tab)
end

--- The editor floats of the float groups with float group panes are `tops`.
function M.carry(tops)
  carried = {}
  for _, w in ipairs(tops) do
    if api.nvim_win_is_valid(w) then carried[w] = true end
  end
  bring_carried()
end

-- before anything is drawn
api.nvim_create_autocmd('TabEnter', { group = group, callback = bring_carried })

--- Makes `win` the current window, as when its pane is focused: its home
--- tab page first, if that isn't current, as `gt` would go there, so
--- TabLeave and TabEnter fire. A float stays where Neovim keeps it; one that
--- takes no focus, as a picker's box, stands for the float hanging from it
--- that does, its input.
function M.go(win)
  local c = api.nvim_win_is_valid(win) and api.nvim_win_get_config(win)
  if c and c.relative ~= '' and not c.external and c.focusable == false then
    local takes = floats_in(api.nvim_win_get_tabpage(win), false, function(f, root)
      return root == win and api.nvim_win_get_config(f).focusable ~= false
    end)
    win = takes[#takes] or win
  end
  local tab = home[win]
  if tab and tab ~= api.nvim_get_current_tabpage() and api.nvim_tabpage_is_valid(tab) and api.nvim_win_is_valid(win) then
    api.nvim_set_current_tabpage(tab)
  end
  if api.nvim_get_current_win() ~= win then api.nvim_set_current_win(win) end
end

-- The current tab page is about to close: the floats other tab pages' that
-- sit in it go back to theirs, as Neovim would close them with it. Neovim
-- allows no move once it's closing, in TabClosedPre, so only before a typed
-- :tabclose, or a middle click after the copy's labels; a close any other
-- way closes them.
local function send_floats_home()
  local closing = api.nvim_get_current_tabpage()
  local back = floats_in(closing, false, function(f)
    local h = float_home[f]
    return h ~= nil and h ~= closing and api.nvim_tabpage_is_valid(h)
  end)
  for _, f in ipairs(back) do move_floats({ f }, float_home[f]) end
end

-- whether Ex command `cmd`, parsed, closes the current tab page
local function closes_current_tab(cmd)
  if cmd.cmd ~= 'tabclose' then return false end
  local n = cmd.range and cmd.range[1] or tonumber(cmd.args[1] and cmd.args[1]:match('^%d+$'))
  return n == nil or n == vim.fn.tabpagenr()
end

api.nvim_create_autocmd('CmdlineLeave', {
  group = group,
  pattern = ':',
  callback = function()
    if vim.v.event.abort then return end
    local line = vim.fn.getcmdline()
    while line and line ~= '' do
      local ok, cmd = pcall(api.nvim_parse_cmd, line, {})
      if not ok then return end
      if closes_current_tab(cmd) then return send_floats_home() end
      line = cmd.nextcmd
    end
  end,
})
-- Closed: floats that belonged to it close too, wherever they sit
api.nvim_create_autocmd('TabClosed', {
  group = group,
  callback = function()
    for f, h in pairs(float_home) do
      if not api.nvim_tabpage_is_valid(h) then
        float_home[f] = nil
        if api.nvim_win_is_valid(f) then pcall(api.nvim_win_close, f, true) end
      end
    end
  end,
})
api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev) float_home[tonumber(ev.match)] = nil end,
})

-- the windows panes show, as the daemon last said; until it says, those
-- fitted to panes
local in_panes

--- Panes show the windows `wins` now.
function M.panes_show(wins)
  in_panes = {}
  for _, w in ipairs(wins) do in_panes[w] = true end
end

-- Where <C-w>w, <C-w>W and <C-w>p may go: a window a pane shows, or a float
-- that takes focus showing in the current tab page.
local function steps_to(win)
  if not api.nvim_win_is_valid(win) then return false end
  local c = api.nvim_win_get_config(win)
  if c.relative == '' or c.external then
    if in_panes then return in_panes[win] == true end
    return panes[win] ~= nil and not is_anchor(win)
  end
  return c.focusable ~= false and not c.hide and api.nvim_win_get_tabpage(win) == api.nvim_get_current_tabpage()
end

-- <C-w>w and <C-w>W in Neovim's order of windows, but only through those
-- `steps_to` allows, so never onto an anchor; with a count, to that one.
-- Another tab page's window brings its tab page along.
local function cycle(forward)
  local count = vim.v.count
  local all = api.nvim_tabpage_list_wins(0)
  local to
  if count > 0 then
    local list = vim.tbl_filter(steps_to, all)
    to = list[math.min(count, #list)]
  else
    local cur = api.nvim_get_current_win()
    local at = 1
    for i, w in ipairs(all) do
      if w == cur then at = i end
    end
    for i = 1, #all do
      local j = (forward and at - 1 + i or at - 1 - i) % #all + 1
      if steps_to(all[j]) then
        to = all[j]
        break
      end
    end
  end
  if to then M.go(to) end
end

for _, k in ipairs({ 'w', '<C-w>' }) do
  vim.keymap.set('n', '<C-w>' .. k, function() cycle(true) end, { desc = 'Go to the next window a pane shows' })
end
vim.keymap.set('n', '<C-w>W', function() cycle(false) end, { desc = 'Go to the previous window a pane shows' })
for _, k in ipairs({ 'p', '<C-p>' }) do
  vim.keymap.set('n', '<C-w>' .. k, function()
    if previous and steps_to(previous) then M.go(previous) end
  end, { desc = 'Go to the window current before' })
end
api.nvim_create_autocmd('WinEnter', {
  group = group,
  callback = function()
    -- a float taking focus gets its own Rex pane: say so now, before the
    -- redraw that first shows it
    local win = api.nvim_get_current_win()
    note_focus(win)
    if api.nvim_win_get_config(win).relative ~= '' then note('entered', win) end
    schedule_report()
  end,
})

-- <C-w>h/j/k/l: Neovim can't tell where the panes sit, so it lands on the
-- hidden anchor, and we'd move on as if cycling. The daemon knows: say which
-- way, and it steps across the windows of the pane first, then has the pane
-- provider move focus (nowhere at an edge); Neovim's current window follows. The
-- maps catch the keys typed and mappings that remap them, in Normal and Visual
-- mode; not a `noremap` mapping to <C-w>h, which skips them, nor `:wincmd`.
-- Not from a float, which Neovim moves out of itself.
local directions = {
  h = { '<C-h>', '<BS>', '<Left>' },
  j = { '<C-j>', '<Down>' },
  k = { '<C-k>', '<Up>' },
  l = { '<C-l>', '<Right>' },
}
for key, synonyms in pairs(directions) do
  local function move()
    local count = vim.v.count1
    if shown(api.nvim_get_current_win()) then
      note('direction', key, count)
    else
      vim.cmd(count .. 'wincmd ' .. key)
    end
  end
  for _, k in ipairs({ key, unpack(synonyms) }) do
    vim.keymap.set({ 'n', 'x' }, '<C-w>' .. k, move, { desc = 'Go to the pane in that direction' })
  end
end

-- The tabline's rows on the screen, as last reported. The daemon draws them
-- over the panes along the top of the Rex window.
local tabline_rows
local function report_tabline()
  local stal = vim.o.showtabline
  local n = (stal == 2 or (stal == 1 and #api.nvim_list_tabpages() > 1)) and 1 or 0
  if n == tabline_rows then return end
  tabline_rows = n
  note('tabline', n)
end
-- whenever Neovim waits for input: plugins such as bufferline set
-- 'showtabline' in their own autocommands, which fire no OptionSet. Not
-- scheduled: SafeState fires again after every scheduled callback.
api.nvim_create_autocmd('SafeState', { group = group, callback = report_tabline })
api.nvim_create_autocmd({ 'TabNew', 'TabClosed' }, {
  group = group,
  callback = function() vim.schedule(report_tabline) end,
})

-- The tabline width: the tabline panes' columns, while they're narrower than
-- the screen, as the daemon last said; else nil. The columns past it are
-- never shown, so every 'tabline' but the copy, which lays itself out to the
-- width, is padded with blanks to the screen's width, which puts the right end
-- of a `%=` layout at the tabline width, and a `%!` one is evaluated with
-- vim.o.columns reading as it.
local tabline_width
-- what a `%!` 'tabline' is set to while it's fitted
local wrapper = '%!v:lua.' .. tool .. '.tabline()'
-- The last 'tabline' we padded: { orig = what a user or plugin set, padded =
-- what we set it to }, or nil. Only that exact string is ours: any other
-- value is a new orig, so we never parse or strip a suffix.
local last_fit
-- The `%!` expression we last wrapped, which `wrapper` evaluates. A plugin may
-- save the wrapper, as lualine does with a 'tabline' that isn't its own, and
-- restore it after we've fitted another value, so it stands for this expression
-- whenever it comes back.
local wrapped_expr
local real_o = vim.o
-- the columns vim.o.columns reads as while a `%!` tabline is evaluated
local proxy_columns
local o_proxy = setmetatable({}, {
  __index = function(_, k)
    if k == 'columns' and proxy_columns then return proxy_columns end
    return real_o[k]
  end,
  __newindex = function(_, k, v) real_o[k] = v end,
})

-- What follows a tabline laid out to `width` on a screen of `columns`: a `%<`,
-- so Neovim cuts a tabline too wide for it there, from the padding, and then
-- blanks.
local function padding(width, columns)
  if columns <= width then return '' end
  return '%<' .. string.rep(' ', columns - width)
end

-- The built-in tabline as neorex draws it (ADR 0002), the copy: Neovim
-- 0.12's draw_tabline, labelling each tab page from its own windows and
-- laid out to the tabline width. 'tabline' reads this while a user or plugin
-- leaves it empty, and only this exact value is ours.
local builtin_copy = '%!v:lua.' .. tool .. '.builtin_tabline()'

-- `s`'s characters, each with any that combine with it
local function chars(s)
  return vim.fn.split(s, [[\zs]])
end

-- Neovim 0.12's draw_tabline, cell for cell, on a row `w` wide. `tabs` are
-- records { count = its windows, modified, name, current }, in order;
-- `showcmd` the pending command, or nil when it isn't drawn in the tabline.
-- Returns the cells, left to right: { text, hl = the highlight group, title =
-- whether Title is combined over it, click = the number of the tab page its
-- label goes to, 'close', 'next' or nil for none }. The right half of a wide
-- character is ''.
local function lay_out_tabline(tabs, w, showcmd)
  local cells, clicks = {}, {}
  for col = 0, w - 1 do cells[col] = { text = ' ', hl = 'TabLineFill' } end
  -- grid_line_put_schar
  local function put(col, text, hl)
    if col < w then cells[col] = { text = text, hl = hl } end
  end
  -- grid_line_puts: `text` from `col`, at most `bytes` of it if given, as
  -- much as fits; a wide character that doesn't is a '>'
  local function puts(col, text, bytes, hl, title)
    local used = 0
    for i, ch in ipairs(chars(text)) do
      used = used + #ch
      if col >= w or (bytes and used > bytes) then break end
      local width = api.nvim_strwidth(ch)
      if width > 2 then ch, width = '\239\191\189', 1 end
      if col + width > w then ch, width = '>', 1 end
      -- overwriting the right half of a wide character cuts that to a '>'
      if i == 1 and col > 0 and cells[col].text == '' then cells[col - 1].text = '>' end
      cells[col] = { text = ch, hl = hl, title = title }
      if width == 2 then cells[col + 1] = { text = '', hl = hl, title = title } end
      col = col + width
    end
  end

  local tabwidth = math.max(#tabs > 0 and math.floor((w - 1 + math.floor(#tabs / 2)) / #tabs) or 0, 6)
  local col, drawn = 0, 0
  for _, tab in ipairs(tabs) do
    if col >= w - 4 then break end
    local scol = col
    local hl = tab.current and 'TabLineSel' or 'TabLine'
    put(col, ' ', hl)
    col = col + 1
    if tab.modified or tab.count > 1 then
      if tab.count > 1 then
        local n = tostring(tab.count)
        if col + #n >= w - 3 then break end
        puts(col, n, #n, hl, true)
        col = col + #n
      end
      if tab.modified then
        put(col, '+', hl)
        col = col + 1
      end
      put(col, ' ', hl)
      col = col + 1
    end
    local room = scol - col + tabwidth - 1
    if room > 0 then
      -- the tail of the name that fits, written whole, then cut to the row
      local name = chars(tab.name)
      local len, first = api.nvim_strwidth(tab.name), 1
      while len > room do
        len = len - api.nvim_strwidth(name[first])
        first = first + 1
      end
      len = math.min(len, w - col - 1)
      puts(col, table.concat(name, '', first), nil, hl)
      col = col + len
    end
    put(col, ' ', hl)
    col = col + 1
    drawn = drawn + 1
    for c = scol, col - 1 do clicks[c] = drawn end
  end

  for c = col, w - 1 do
    cells[c] = { text = ' ', hl = 'TabLineFill' }
    clicks[c] = 'next'
  end
  local close = drawn > 1 and 1 or 0
  if showcmd then
    local width = math.min(10, w - col - close * 3)
    if width > 0 then puts(w - width - close * 2, showcmd, width, 'TabLine') end
  end
  if close == 1 then
    put(w - 1, 'X', 'TabLine')
    clicks[w - 1] = 'close'
  end

  local row = {}
  for c = 0, w - 1 do
    local cell = cells[c]
    row[c + 1] = { text = cell.text, hl = cell.hl, title = cell.title, click = clicks[c] }
  end
  return row
end

-- The copy's groups for the built-in's: a 'tabline' draws every group
-- combined over TabLineFill, so these stand for the built-in's own, each
-- drawn as it is (see `sync_tabline_groups`). TabLineFill needs none.
local tabline_groups = { TabLineSel = tool_title .. 'TabLineSel', TabLine = tool_title .. 'TabLine' }

-- Cells such as `lay_out_tabline` gives as a 'tabline' format string
local function tabline_format(row)
  local out = {}
  local hl, click
  for i, cell in ipairs(row) do
    if cell.click ~= click then
      click = cell.click
      if click == nil then
        out[#out + 1] = '%T'
      elseif click == 'close' then
        out[#out + 1] = '%999X'
      elseif click == 'next' then
        out[#out + 1] = '%@v:lua.' .. tool .. '.tabline_click@'
      else
        out[#out + 1] = '%' .. click .. 'T'
      end
    end
    local key = cell.hl .. (cell.title and ' Title' or '')
    if key ~= hl then
      hl = key
      local g = tabline_groups[cell.hl]
      out[#out + 1] = (g and '%#' .. g .. '#' or '%*') .. (cell.title and '%$Title$' or '')
    end
    -- a wide character without both its halves is blank
    local text = cell.text
    if text == '' then
      local prev = row[i - 1]
      if not prev or api.nvim_strwidth(prev.text) ~= 2 then text = ' ' end
    elseif api.nvim_strwidth(text) == 2 and not (row[i + 1] and row[i + 1].text == '') then
      text = ' '
    end
    out[#out + 1] = (text:gsub('%%', '%%%%'))
  end
  if click ~= nil then out[#out + 1] = '%T' end
  return table.concat(out)
end

-- Each of the copy's groups as last set: by name, { want = what it was set
-- to, got = what Neovim then said it was }
local tabline_group_defs = {}
-- Sets each of the copy's groups to draw over TabLineFill as the built-in's
-- own does on its own: no combining with TabLineFill's attributes, and
-- colors it leaves unset from Normal, the default ones. Only when that
-- changed, or the group did, as `:hi clear` clears it: each set redraws.
local function sync_tabline_groups()
  local normal = api.nvim_get_hl(0, { name = 'Normal', link = false })
  for name, ours in pairs(tabline_groups) do
    local want = api.nvim_get_hl(0, { name = name, link = false })
    for _, k in ipairs({ 'fg', 'bg', 'sp', 'ctermfg', 'ctermbg' }) do
      if want[k] == nil then want[k] = normal[k] end
    end
    want.default = nil
    want.nocombine = true
    want.cterm = vim.tbl_extend('force', want.cterm or {}, { nocombine = true })
    local prev = tabline_group_defs[ours]
    local same = prev and vim.deep_equal(want, prev.want)
      and vim.deep_equal(api.nvim_get_hl(0, { name = ours }), prev.got)
    if not same then
      api.nvim_set_hl(0, ours, want)
      tabline_group_defs[ours] = { want = want, got = api.nvim_get_hl(0, { name = ours }) }
    end
  end
end

-- The copy: the built-in tabline, laid out to the tabline width, else the
-- screen's, and padded to the screen's width as every fitted tabline is.
function M.builtin_tabline()
  sync_tabline_groups()
  local columns = real_o.columns
  local w = math.min(tabline_width or columns, columns)
  local by_tab, cur, tabs = own_windows(), api.nvim_get_current_tabpage(), {}
  for _, tab in ipairs(api.nvim_list_tabpages()) do
    local mine = by_tab[tab]
    local modified = false
    for _, win in ipairs(mine.wins) do
      modified = modified or vim.bo[api.nvim_win_get_buf(win)].modified
    end
    -- as the built-in names it: its `%f`, each directory cut to a character
    local name = api.nvim_eval_statusline('%f', { winid = mine.label, maxwidth = 4096 }).str
    tabs[#tabs + 1] = {
      count = #mine.wins,
      modified = modified,
      name = vim.fn.pathshorten(name, 1),
      current = tab == cur,
    }
  end
  local showcmd
  if vim.o.showcmd and vim.o.showcmdloc == 'tabline' then
    showcmd = api.nvim_eval_statusline('%S', { use_tabline = true }).str
  end
  return tabline_format(lay_out_tabline(tabs, w, showcmd)) .. padding(w, columns)
end

-- A click on the copy after its last label, as on the built-in's: the
-- middle button closes the current tab page, a double click opens one at the
-- end, and any other click goes to the next.
function M.tabline_click(_, clicks, button)
  local ok, err = pcall(function()
    if button == 'm' then
      if #api.nvim_list_tabpages() > 1 then
        send_floats_home()
        vim.cmd.tabclose()
      end
      return
    end
    -- each ends Visual mode as it enters another window, as the built-in's do
    if clicks == 2 then
      vim.cmd.tabnew()
      vim.cmd.tabmove('$')
    else
      vim.cmd.tabnext()
    end
  end)
  if not ok then api.nvim_echo({ { tostring(err), 'ErrorMsg' } }, true, {}) end
end

-- Draws the copy again, where neorex's own records changed and Neovim draws
-- the tabline no more than it would without them.
local function redraw_copy()
  if vim.go.tabline == builtin_copy then vim.cmd.redrawtabline() end
end

-- The one decision about 'tabline': from its `current` value, the tabline
-- `width` (nil if none), the screen's `columns` and the `record` of our last
-- padding. Returns the value to set, or nil to leave it, and the new record.
-- Empty is the copy, which fits itself. A `%!` expression is wrapped, which
-- pads when it's evaluated; a format string is padded in place, as lualine
-- sets one and restores any that doesn't contain its name, so a wrapper would
-- come back.
-- `expr` is the `%!` expression last wrapped: the wrapper, if it shows up
-- again, is that `%!` tabline.
local function fit(current, width, columns, record, expr)
  local orig = current
  if current == builtin_copy then
    orig = ''
  elseif record and current == record.padded then
    orig = record.orig
  elseif current == wrapper and expr then
    orig = '%!' .. expr
  end
  local padded = orig
  if orig == '' then
    padded = builtin_copy
  elseif width then
    padded = orig:sub(1, 2) == '%!' and wrapper or orig .. padding(width, columns)
  end
  local new = padded ~= orig and { orig = orig, padded = padded } or nil
  return padded ~= current and padded or nil, new
end

-- Neovim is quitting: 'tabline' is left alone
local stopping = false

-- Fits 'tabline' to what there is now.
local function fit_tabline()
  if stopping then return end
  local value, record = fit(vim.go.tabline, tabline_width, real_o.columns, last_fit, wrapped_expr)
  -- first: setting it fires OptionSet, which comes back here
  last_fit = record
  -- the wrapper always means the newest expression: one saved while it meant
  -- an older one shows this one when it's put back (accepted)
  if record and record.padded == wrapper then wrapped_expr = record.orig:sub(3) end
  if value then vim.go.tabline = value end
end

-- A fitted `%!` tabline: the expression set, laid out to the tabline width
-- and padded.
function M.tabline()
  -- only before any `%!` tabline was wrapped, if someone set the wrapper
  -- itself: nothing to evaluate
  local expr = wrapped_expr
  if not expr then return '' end
  if not tabline_width then return api.nvim_eval(expr) end
  proxy_columns = math.min(tabline_width, real_o.columns)
  vim.o = o_proxy
  local ok, s = pcall(api.nvim_eval, expr)
  vim.o = real_o
  local pad = padding(proxy_columns, real_o.columns)
  proxy_columns = nil
  if not ok then error(s, 0) end
  return s .. pad
end

-- The daemon's tabline width: `cols`, or nil (vim.NIL) when nothing is hidden.
function M.set_tabline_width(cols)
  if cols == vim.NIL then cols = nil end
  if cols == tabline_width then return end
  tabline_width = cols
  vim.g[tool .. '_tabline_width'] = cols
  fit_tabline()
  vim.cmd.redrawtabline()
end

-- a tabline set again is padded again, and when the screen's width changes,
-- before the next redraw. On SafeState too, as plugins set it in
-- autocommands, which fire no OptionSet.
api.nvim_create_autocmd('OptionSet', { group = group, pattern = 'tabline', callback = fit_tabline })
api.nvim_create_autocmd({ 'SafeState', 'VimResized' }, { group = group, callback = fit_tabline })
-- and the copy goes as neorex stops: 'tabline' is empty again
api.nvim_create_autocmd('VimLeavePre', {
  group = group,
  callback = function()
    stopping = true
    if vim.go.tabline == builtin_copy then vim.go.tabline = '' end
  end,
})

-- A typed :detach is refused: it would close the daemon's channel, and the
-- daemon would end Neovim. A plugin's vim.cmd('detach') still gets through,
-- and the daemon takes it as Neovim exiting.
api.nvim_create_autocmd('CmdlineLeave', {
  group = group,
  pattern = ':',
  callback = function()
    if vim.v.event.abort then return end
    -- each command of a bar-chain too, as in :echo 1 | detach
    local line = vim.fn.getcmdline()
    local found = false
    while line and line ~= '' and not found do
      local ok, cmd = pcall(api.nvim_parse_cmd, line, {})
      if not ok then return end
      found = cmd.cmd == 'detach'
      line = cmd.nextcmd
    end
    if not found then return end
    -- vim.v.event is a copy
    vim.cmd('let v:event.abort = v:true')
    vim.schedule(function()
      api.nvim_echo({ { tool .. ': :detach is refused: it would end Neovim' } }, true, { err = true })
    end)
  end,
})

-- 'cmdheight' as last reported: the rows at the bottom of the screen the
-- daemon keeps clear of every pane's window, for messages and the command
-- line. Each tab page has its own.
local cmdheight
local function report_cmdheight()
  local n = vim.o.cmdheight
  if n == cmdheight then return end
  cmdheight = n
  note('cmdheight', n)
end
-- whenever Neovim waits for input, since autocommands that set it fire no
-- OptionSet, and a tab page change brings its own. Not scheduled: SafeState
-- fires again after every scheduled callback.
api.nvim_create_autocmd('SafeState', { group = group, callback = report_cmdheight })

-- 'equalalways', 'splitbelow', 'splitright', the separator 'fillchars' draws
-- and 'laststatus', as last reported: a pane laying out several windows splits and closes
-- them as these say. Looked at whenever Neovim waits for input, as for
-- 'cmdheight'.
local reported_options
local function report_options()
  local vert = vim.opt_global.fillchars:get().vert or '│'
  local now = table.concat({ tostring(vim.go.equalalways), tostring(vim.go.splitbelow), tostring(vim.go.splitright), vert, vim.go.laststatus }, ' ')
  if now == reported_options then return end
  reported_options = now
  note('options', vim.go.equalalways, vim.go.splitbelow, vim.go.splitright, vert, vim.go.laststatus)
end
api.nvim_create_autocmd('SafeState', { group = group, callback = report_options })

-- Neovim sized an external window itself, as `:resize`, `<C-w>+` and
-- `:vertical resize` do: a pane laying out several windows gives it that size.
-- Each size is told once (see `reported_sizes`).
local function report_resized()
  -- a statusline that came or went changed the text's height, not the
  -- window's: the refit on its way puts it back
  if refit_pending then return end
  for win, p in pairs(panes) do
    local w, h
    if p.w and api.nvim_win_is_valid(win) and not waiting(win) and api.nvim_win_get_config(win).external then
      w = api.nvim_win_get_width(win)
      h = api.nvim_win_get_height(win)
      if w == p.w and h == text_height(win, p.h) then w = nil end
    end
    if not w then
      reported_sizes[win] = nil
    else
      h = h + (has_statusline(win) and 1 or 0)
      local r = reported_sizes[win]
      if not r or r[1] ~= w or r[2] ~= h then
        reported_sizes[win] = { w, h }
        note('resized', win, w, h)
      end
    end
  end
end
-- WinResized too: Neovim may size an external window only as it redraws it
api.nvim_create_autocmd({ 'SafeState', 'WinResized' }, { group = group, callback = report_resized })

-- A tab page's windows may have been resized or moved, with nothing to say
-- so but this. Looked at here, not scheduled: SafeState fires again
-- after every scheduled callback.
api.nvim_create_autocmd('SafeState', {
  group = group,
  callback = function() refresh_layout(api.nvim_get_current_tabpage()) end,
})

-- Neovim draws no statusline in an external window when laststatus=3, but each
-- pane is its own screen: show one in every pane with laststatus=2 instead.
-- Returns whether it changed laststatus. Run from SafeState, as OptionSet
-- doesn't fire for an option set in an autocommand, as LazyVim loads lualine
-- (nor for ours here, which is in one). The same goes for a laststatus=2 set in
-- an autocommand: it can't end the mirroring, as it looks like ours.
local function rewrite_laststatus_3()
  if vim.go.laststatus ~= 3 then return false end
  vim.go.laststatus = 2
  mirroring = true
  return true
end
rewrite_laststatus_3()

-- Neovim draws a float's own statusline at laststatus=2, not at 3. Lualine's
-- globalstatus gives the current float one, for laststatus=3 to show. At the
-- 2 we set, it would take a row over what's under the float: the first entry
-- of a picker's list, under its input. So while mirroring, a float that isn't
-- external shows none, as at 3.
local function is_inner_float(win)
  local c = api.nvim_win_get_config(win)
  return c.relative ~= '' and not c.external
end

local function hide_float_statusline(win)
  if not mirroring or not api.nvim_win_is_valid(win) or not is_inner_float(win) then return end
  local stl = local_stl(win)
  if stl == '' then return end
  hidden_stl[win] = stl
  set_local_stl(win, '')
end

-- a statusline came, went or changed: fit the windows to their panes again. A
-- laststatus other than 3 set by the user ends the mirroring.
api.nvim_create_autocmd('OptionSet', {
  group = group,
  pattern = { 'statusline', 'laststatus' },
  callback = function(ev)
    if ev.match == 'laststatus' and mirroring and tonumber(vim.v.option_new) ~= 3 then
      stop_mirroring()
    end
    -- before the redraw that would show it; the window set is current
    if ev.match == 'statusline' then hide_float_statusline(api.nvim_get_current_win()) end
    request_refit()
  end,
})

api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev)
    local win = tonumber(ev.match)
    copied[win], hidden_stl[win] = nil, nil
  end,
})

-- the current window and its statusline, last time we looked
local seen_win, seen_stl
-- whenever Neovim waits for input: the statusline lualine writes for the
-- current window in an autocommand fires no OptionSet either
api.nvim_create_autocmd('SafeState', {
  group = group,
  callback = function()
    local changed = rewrite_laststatus_3()
    if mirroring and vim.go.laststatus < 2 then
      stop_mirroring()
      changed = true
    end
    -- set in an autocommand, which fires no OptionSet: the redraw comes after
    if mirroring then
      for _, win in ipairs(api.nvim_list_wins()) do hide_float_statusline(win) end
    end
    if mirroring then
      local cur = api.nvim_get_current_win()
      local stl = local_stl(cur)
      changed = changed or cur ~= seen_win or stl ~= seen_stl
      seen_win, seen_stl = cur, stl
    end
    if changed then request_refit() end
  end,
})

-- :q in the last window quits Neovim: make it the tab's only ordinary window
-- first. If Neovim then refuses (unsaved changes), `restore` or the daemon
-- adopts it again.
api.nvim_create_autocmd('QuitPre', {
  group = group,
  callback = function()
    local cur = api.nvim_get_current_win()
    local tab = api.nvim_get_current_tabpage()
    local anchor = anchor_of(tab)
    if not anchor or not shown(cur) or others_shown(cur) then return end
    api.nvim_win_set_config(cur, { split = 'left', win = anchor })
    anchors[tab] = nil
    api.nvim_win_close(anchor, true)
  end,
})

-- A window of tab page `tab`, made after setup, closed: once none is left,
-- close the tab page too, from another one, as external windows would go
-- with it.
local function close_tab(tab)
  local t = floating[tab]
  -- windows being reopened in it are still to come
  if not t or (t.reopening or 0) > 0 then return end
  for w, h in pairs(home) do
    if h == tab and shown(w) then return end
  end
  floating[tab] = nil
  -- the daemon forgets its layout, if it has one
  if t.told and t.told ~= '' then note('layout', tab, {}) end
  -- back to the window it opened over
  if not (last and api.nvim_win_is_valid(last)) then last = t.back end
  if not api.nvim_tabpage_is_valid(tab) or #api.nvim_list_tabpages() == 1 then return end
  if tab == api.nvim_get_current_tabpage() then vim.cmd('silent! tabnext #') end
  if tab == api.nvim_get_current_tabpage() then vim.cmd('silent! tabprevious') end
  if tab == api.nvim_get_current_tabpage() then return end
  pcall(vim.cmd, 'tabclose! ' .. api.nvim_tabpage_get_number(tab))
end

-- Opens external `win`, about to close, again with its buffer and view, in
-- tab page `from`. The daemon gives the new window its pane. Its tab page's
-- layout waits for it, and has it where `win` was.
local function reopen(win, from, enter)
  local c = api.nvim_win_get_config(win)
  local buf = api.nvim_win_get_buf(win)
  local view = api.nvim_win_call(win, vim.fn.winsaveview)
  local t = from and layout_record(from)
  if t then t.reopening = (t.reopening or 0) + 1 end
  note('reopen', win)
  vim.schedule(function()
    -- first, so an error below can't stop its layout for good
    if t then t.reopening = t.reopening - 1 end
    if not api.nvim_buf_is_valid(buf) then buf = api.nvim_create_buf(true, false) end
    local new = api.nvim_open_win(buf, enter, { external = true, width = c.width, height = c.height })
    api.nvim_win_call(new, function() vim.fn.winrestview(view) end)
    keep_edgy_out(new)
    home[new] = from
    for tab, w in pairs(focused) do
      if w == win then focused[tab] = new end
    end
    if last == win then last = new end
    for _, f in pairs(floating) do
      if f.back == win then f.back = new end
    end
    note('reopened', win, new)
    -- unless the tab page closed meanwhile
    if t and layout_record(from) == t then
      if t.layout then swap_win(t.layout, win, new) end
      refresh_layout(from)
    end
    -- its tab page's label counts it again
    redraw_copy()
    -- left on an anchor while there was nowhere to go
    if is_anchor(api.nvim_get_current_win()) then schedule_report() end
  end)
end

-- Neovim never closes its last window, but the anchor lets :close do it. So
-- reopen it, and the daemon gives the new window the same pane. Windows
-- that only followed into a tab page, closed with it by :tabclose, are
-- reopened too, in their home tab page, which stays even if it's floating.
api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev)
    local win = tonumber(ev.match)
    local from = home[win]
    home[win], panes[win] = nil, nil
    if vim.v.exiting ~= vim.NIL then return end
    -- Neovim redraws no tabline for an external window closing, as it would
    -- for a split: its tab page's label counts one fewer
    vim.schedule(redraw_copy)
    local external = shown(win) and api.nvim_win_get_config(win).external
    -- before its home tab page's own close: a floating one stays
    local followed = closing_tab and not closing_ours and from and from ~= closing_tab and api.nvim_tabpage_is_valid(from)
    if external and followed then return reopen(win, from, false) end
    if from then
      -- after the command, which may open more windows in it; its layout
      -- too, as it may not be the current tab page's
      local floats = floating[from]
      vim.schedule(function()
        if floats then close_tab(from) end
        refresh_layout(from)
      end)
      if floats then return end
    end
    if external and not others_shown(win) then reopen(win, from, true) end
  end,
})

-- Every window in a closing tab page closes, external ones from other tab
-- pages too. Not in older Neovim, which lacks the event.
pcall(api.nvim_create_autocmd, 'TabClosedPre', {
  group = group,
  callback = function()
    closing_tab = api.nvim_get_current_tabpage()
    -- for a close that fails
    vim.schedule(function() closing_tab = nil end)
  end,
})
-- Closed, after its windows: focus from here on is real, the window Neovim
-- enters next included. A closed tab page's record goes.
api.nvim_create_autocmd('TabClosed', {
  group = group,
  callback = function()
    closing_tab = nil
    for tab in pairs(focused) do
      if not api.nvim_tabpage_is_valid(tab) then focused[tab] = nil end
    end
    for tab in pairs(layouts) do
      if not api.nvim_tabpage_is_valid(tab) then layouts[tab] = nil end
    end
  end,
})

for _, win in ipairs(api.nvim_list_wins()) do
  if api.nvim_win_get_config(win).relative == '' then home[win] = api.nvim_win_get_tabpage(win) end
end
-- The windows Neovim started with, as `-o` and `-O` open them: each is
-- noted split off the one before it, the way the layout has them, so they
-- are laid out as Neovim had them. Returns the last window of `node`.
local function note_startup_splits(node, dir, from)
  if node[1] == 'leaf' then
    if from then note('split', node[2], dir == 'row', false, from, 0) end
    return node[2]
  end
  local last = from
  for i, child in ipairs(node[2]) do
    last = note_startup_splits(child, i == 1 and dir or node[1], last)
  end
  return last
end
note_startup_splits(vim.fn.winlayout())

local first = api.nvim_get_current_win()
M.adopt(first, cols, rows)
schedule_report()
note_focus(first)
report_tabline()
report_cmdheight()
fit_tabline()
return { first, anchor_of(api.nvim_get_current_tabpage()) }
