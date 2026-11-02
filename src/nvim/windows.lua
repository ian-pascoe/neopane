-- neorex in Neovim: every window except one hidden anchor per tab page is
-- external, shown in its own Rex pane. Run once with nvim_exec_lua(chan,
-- cols, rows); returns { first window, its anchor }.
local chan, cols, rows = ...
local api = vim.api
local M = {}
_G.neorex = M

-- tab page -> anchor window
local anchors = {}
-- the window last reported as current
local last
-- tab page made after setup -> { back = the window current before it }.
-- Its windows show in a Rex floating pane, and it closes with the last.
-- The record also holds its window layout, as the daemon is told of it.
local floating = {}
-- window -> the tab page it belongs to: the one it was made in, or its
-- origin's for a split. External windows follow the current tab page, so
-- that's the only way to tell whose they are.
local home = {}
-- closing a window because its pane went away
local closing_ours = false
-- the tab page closing, with every window in it
local closing_tab
local group = api.nvim_create_augroup('neorex', {})

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

local function others_shown(win)
  for _, w in ipairs(api.nvim_list_wins()) do
    if w ~= win and shown(w) then return true end
  end
  return false
end

local making_anchor = false

-- window -> its pane: `w` x `h`, the rows and columns, statusline included.
-- Layout plugins move windows back into Neovim's layout (see `restore`), so
-- it also has `fight`: `restored`, when we put it back (ms), `off_until`,
-- until when we leave it alone, and `off_for`, how long that was the last time.
local panes = {}
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
local function stop_mirroring()
  mirroring = false
  for win, c in pairs(copied) do
    if api.nvim_win_is_valid(win) and local_stl(win) == c.text then set_local_stl(win, c.own) end
  end
  copied = {}
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
-- window each. The same as `Layout.max_nodes` in windows.zig.
local MAX_LAYOUT_NODES = 63

-- A floating tab page's `layout`: nested windows { win, w, h } and rows and
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

-- Takes floating tab page `tab`'s layout from winlayout(), while its windows
-- aren't external, and from what it was and the sizes Neovim has since given
-- external ones; tells the daemon if it changed.
local function refresh_layout(tab)
  local t = floating[tab]
  if not t or not api.nvim_tabpage_is_valid(tab) then return end
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
  vim.rpcnotify(chan, 'neorex_layout', tab, flat)
end

-- after the command that changed it, which may change it more
local function schedule_layout(tab)
  local t = floating[tab]
  if not t or t.pending then return end
  t.pending = true
  vim.schedule(function()
    t.pending = false
    if floating[tab] == t then refresh_layout(tab) end
  end)
end

--- Makes ordinary `win` external at `w` x `h`, giving its tab page an anchor first.
function M.adopt(win, w, h)
  if not shown(win) or waiting(win) or api.nvim_win_get_config(win).external then return end
  keep_edgy_out(win)
  local tab = api.nvim_win_get_tabpage(win)
  -- before it leaves winlayout()
  refresh_layout(tab)
  if not anchor_of(tab) then
    local buf = api.nvim_create_buf(false, true)
    vim.bo[buf].bufhidden = 'wipe'
    making_anchor = true
    anchors[tab] = api.nvim_open_win(buf, false, { split = 'left', win = win })
    making_anchor = false
  end
  give_statusline(win)
  api.nvim_win_set_config(win, { external = true, width = w, height = text_height(win, h) })
  local p = pane_of(win)
  p.w, p.h = w, h
end

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
end, api.nvim_create_namespace('neorex'))

--- Closes `win`, whose pane went away. Unsaved changes stay in its buffer.
function M.close(win)
  if not api.nvim_win_is_valid(win) then return end
  closing_ours = true
  pcall(api.nvim_win_close, win, true)
  closing_ours = false
end

--- Fits the floats hanging from editor floats `tops` to `w` x `h` at `row`,
--- `col` on the screen, as their Rex floating pane was resized to. `cur` is
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
      -- over them: its windows get a Rex floating pane.
      local tab = api.nvim_win_get_tabpage(win)
      if api.nvim_win_get_config(win).relative == '' and not anchor_of(tab) then
        floating[tab] = floating[tab] or { back = last }
        home[win] = tab
        vim.rpcnotify(chan, 'neorex_split', win, false, false, 0, tab)
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
    vim.rpcnotify(chan, 'neorex_split', win, dir == 'row', first, shown(from) and from or 0, floating[tab] and tab or 0)
    if floating[tab] then schedule_layout(tab) end
  end,
})

local pending = false
local function report()
  pending = false
  local cur = api.nvim_get_current_win()
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
  -- it's in a Rex floating pane: the daemon decides
  if api.nvim_win_get_config(cur).relative ~= '' then
    vim.rpcnotify(chan, 'neorex_float', cur)
    return
  end
  last = cur
  vim.rpcnotify(chan, 'neorex_win', cur)
end

-- after the command that moved, so :windo and friends report once
local function schedule_report()
  if pending then return end
  pending = true
  vim.schedule(report)
end
api.nvim_create_autocmd('WinEnter', {
  group = group,
  callback = function()
    -- a float taking focus gets its own Rex pane: say so now, before the
    -- redraw that first shows it
    local win = api.nvim_get_current_win()
    if api.nvim_win_get_config(win).relative ~= '' then vim.rpcnotify(chan, 'neorex_entered', win) end
    schedule_report()
  end,
})

-- <C-w>h/j/k/l: Neovim can't tell where the panes sit, so it lands on the
-- hidden anchor, and we'd move on as if cycling. Rex knows: say which way and
-- let it move focus (nowhere at an edge); Neovim's current window follows. The
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
      vim.rpcnotify(chan, 'neorex_direction', key, count)
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
  vim.rpcnotify(chan, 'neorex_tabline', n)
end
-- whenever Neovim waits for input: plugins such as bufferline set
-- 'showtabline' in their own autocommands, which fire no OptionSet. Not
-- scheduled: SafeState fires again after every scheduled callback.
api.nvim_create_autocmd('SafeState', { group = group, callback = report_tabline })
api.nvim_create_autocmd({ 'TabNew', 'TabClosed' }, {
  group = group,
  callback = function() vim.schedule(report_tabline) end,
})

-- 'cmdheight' as last reported: the rows at the bottom of the screen the
-- daemon keeps clear of every pane's window, for messages and the command
-- line. Each tab page has its own.
local cmdheight
local function report_cmdheight()
  local n = vim.o.cmdheight
  if n == cmdheight then return end
  cmdheight = n
  vim.rpcnotify(chan, 'neorex_cmdheight', n)
end
-- whenever Neovim waits for input, since autocommands that set it fire no
-- OptionSet, and a tab page change brings its own. Not scheduled: SafeState
-- fires again after every scheduled callback.
api.nvim_create_autocmd('SafeState', { group = group, callback = report_cmdheight })

-- A floating tab page's windows may have been resized or moved, with nothing
-- to say so but this. Looked at here, not scheduled: SafeState fires again
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

-- a statusline came, went or changed: fit the windows to their panes again. A
-- laststatus other than 3 set by the user ends the mirroring.
api.nvim_create_autocmd('OptionSet', {
  group = group,
  pattern = { 'statusline', 'laststatus' },
  callback = function(ev)
    if ev.match == 'laststatus' and mirroring and tonumber(vim.v.option_new) ~= 3 then
      stop_mirroring()
    end
    request_refit()
  end,
})

api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev) copied[tonumber(ev.match)] = nil end,
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

-- A window of floating tab page `tab` closed: once none is left, close the
-- tab page too, from another one, as external windows would go with it.
local function close_tab(tab)
  local t = floating[tab]
  if not t then return end
  for w, h in pairs(home) do
    if h == tab and shown(w) then return end
  end
  floating[tab] = nil
  -- the daemon forgets its layout, if it has one
  if t.told and t.told ~= '' then vim.rpcnotify(chan, 'neorex_layout', tab, {}) end
  -- back to the window it opened over
  if not (last and api.nvim_win_is_valid(last)) then last = t.back end
  if not api.nvim_tabpage_is_valid(tab) or #api.nvim_list_tabpages() == 1 then return end
  if tab == api.nvim_get_current_tabpage() then vim.cmd('silent! tabnext #') end
  if tab == api.nvim_get_current_tabpage() then vim.cmd('silent! tabprevious') end
  if tab == api.nvim_get_current_tabpage() then return end
  pcall(vim.cmd, 'tabclose! ' .. api.nvim_tabpage_get_number(tab))
end

-- Opens external `win`, about to close, again with its buffer and view, in
-- tab page `from`. The daemon gives the new window its pane.
local function reopen(win, from, enter)
  local c = api.nvim_win_get_config(win)
  local buf = api.nvim_win_get_buf(win)
  local view = api.nvim_win_call(win, vim.fn.winsaveview)
  vim.rpcnotify(chan, 'neorex_reopen', win)
  vim.schedule(function()
    if not api.nvim_buf_is_valid(buf) then buf = api.nvim_create_buf(true, false) end
    local new = api.nvim_open_win(buf, enter, { external = true, width = c.width, height = c.height })
    api.nvim_win_call(new, function() vim.fn.winrestview(view) end)
    keep_edgy_out(new)
    home[new] = from
    if last == win then last = new end
    for _, t in pairs(floating) do
      if t.back == win then t.back = new end
    end
    vim.rpcnotify(chan, 'neorex_reopened', win, new)
    -- left on an anchor while there was nowhere to go
    if is_anchor(api.nvim_get_current_win()) then schedule_report() end
  end)
end

-- Neovim never closes its last window, but the anchor lets :close do it. So
-- reopen it, and the daemon gives the new window the same pane. Windows
-- that only followed into a tab page, closed with it by :tabclose, are
-- reopened too.
api.nvim_create_autocmd('WinClosed', {
  group = group,
  callback = function(ev)
    local win = tonumber(ev.match)
    local from = home[win]
    home[win], panes[win] = nil, nil
    if vim.v.exiting ~= vim.NIL then return end
    if from and floating[from] then
      -- after the command, which may open more windows in it
      vim.schedule(function()
        close_tab(from)
        refresh_layout(from)
      end)
      return
    end
    if not shown(win) or not api.nvim_win_get_config(win).external then return end
    local followed = closing_tab and not closing_ours and from and from ~= closing_tab and api.nvim_tabpage_is_valid(from)
    if followed then return reopen(win, from, false) end
    if not others_shown(win) then reopen(win, from, true) end
  end,
})

-- Every window in a closing tab page closes, external ones from other tab
-- pages too. Not in older Neovim, which lacks the event.
pcall(api.nvim_create_autocmd, 'TabClosedPre', {
  group = group,
  callback = function()
    closing_tab = api.nvim_get_current_tabpage()
    vim.schedule(function() closing_tab = nil end)
  end,
})

for _, win in ipairs(api.nvim_list_wins()) do
  if api.nvim_win_get_config(win).relative == '' then home[win] = api.nvim_win_get_tabpage(win) end
end
local first = api.nvim_get_current_win()
M.adopt(first, cols, rows)
schedule_report()
report_tabline()
report_cmdheight()
return { first, anchor_of(api.nvim_get_current_tabpage()) }
