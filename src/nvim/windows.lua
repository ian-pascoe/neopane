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

-- window -> its pane's rows and columns, statusline included
local sizes = {}
-- statusline formats we've set, to tell them from ones the user set
local ours = {}
-- laststatus=3 was set, and is 2 while neorex runs: every pane shows the one
-- statusline a global one would, the current window's
local global = false
-- windows whose statusline is that copy, not their own
local mirrored = {}
-- setting laststatus ourselves
local rewriting = false

local function local_stl(win)
  return api.nvim_get_option_value('statusline', { win = win, scope = 'local' })
end

local function has_statusline(win)
  return vim.go.laststatus ~= 0 and local_stl(win) ~= ''
end

-- What a global statusline shows in `win`: the current window's own, which
-- lualine's globalstatus keeps up to date. Nil if that's `win` itself, or the
-- current window isn't a pane (a float, say), whose statusline we keep.
local function global_line(win)
  local cur = api.nvim_get_current_win()
  if cur == win or not shown(cur) then return end
  local stl = local_stl(cur)
  if stl ~= '' then return stl end
end

-- external windows only show a local statusline: copy the global one
local function give_statusline(win)
  local cur = local_stl(win)
  local line = global and global_line(win)
  if line then
    if cur ~= line then api.nvim_set_option_value('statusline', line, { win = win, scope = 'local' }) end
    mirrored[win] = true
    return
  end
  if cur ~= '' and not ours[cur] and not (global and mirrored[win]) then return end
  local want = vim.go.laststatus == 0 and '' or vim.go.statusline
  if want ~= '' then ours[want] = true end
  if cur ~= want then api.nvim_set_option_value('statusline', want, { win = win, scope = 'local' }) end
end

-- the text's height: what's left of the pane after the statusline, if it has
-- one
local function text_height(win, h)
  return math.max(h - (has_statusline(win) and 1 or 0), 1)
end

--- Sizes external `win` to fill `w` x `h`, statusline included.
function M.size(win, w, h)
  if not shown(win) then return end
  sizes[win] = { w, h }
  give_statusline(win)
  local th = text_height(win, h)
  -- every resize redraws the window
  if api.nvim_win_get_width(win) == w and api.nvim_win_get_height(win) == th then return end
  api.nvim_win_set_config(win, { width = w, height = th })
end

local function refit()
  for win, s in pairs(sizes) do
    if api.nvim_win_is_valid(win) then M.size(win, s[1], s[2]) else sizes[win] = nil end
  end
end

--- Makes ordinary `win` external at `w` x `h`, giving its tab page an anchor first.
function M.adopt(win, w, h)
  if not shown(win) or api.nvim_win_get_config(win).external then return end
  local tab = api.nvim_win_get_tabpage(win)
  if not anchor_of(tab) then
    local buf = api.nvim_create_buf(false, true)
    vim.bo[buf].bufhidden = 'wipe'
    making_anchor = true
    anchors[tab] = api.nvim_open_win(buf, false, { split = 'left', win = win })
    making_anchor = false
  end
  give_statusline(win)
  api.nvim_win_set_config(win, { external = true, width = w, height = text_height(win, h) })
  sizes[win] = { w, h }
end

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
-- 'showtabline' in their own autocommands, which fire no OptionSet
api.nvim_create_autocmd({ 'SafeState', 'TabNew', 'TabClosed' }, {
  group = group,
  callback = function() vim.schedule(report_tabline) end,
})

-- Neovim draws no statusline in an external window when laststatus=3, but each
-- pane is its own screen: show one in every pane with laststatus=2 instead.
-- Returns whether it changed laststatus. Run from SafeState, as OptionSet
-- doesn't fire for an option set in an autocommand, as LazyVim loads lualine.
local function no_global_statusline()
  if vim.go.laststatus ~= 3 then return false end
  rewriting = true
  vim.go.laststatus = 2
  rewriting = false
  global = true
  return true
end
no_global_statusline()

-- a statusline came, went or changed: fit the windows to their panes again
api.nvim_create_autocmd('OptionSet', {
  group = group,
  pattern = { 'statusline', 'laststatus' },
  callback = function() vim.schedule(refit) end,
})

-- laststatus set to something else: each pane's statusline is its own again
api.nvim_create_autocmd('OptionSet', {
  group = group,
  pattern = 'laststatus',
  callback = function()
    if rewriting or not global or vim.v.option_new == '3' then return end
    global = false
    for win in pairs(mirrored) do
      if api.nvim_win_is_valid(win) then api.nvim_set_option_value('statusline', '', { win = win, scope = 'local' }) end
    end
    mirrored = {}
  end,
})

-- the current window and its statusline, last time we looked
local seen_line
-- whenever Neovim waits for input: the statusline lualine writes for the
-- current window in an autocommand fires no OptionSet either
api.nvim_create_autocmd('SafeState', {
  group = group,
  callback = function()
    local changed = no_global_statusline()
    if global then
      local cur = api.nvim_get_current_win()
      local line = cur .. local_stl(cur)
      changed = changed or line ~= seen_line
      seen_line = line
    end
    if changed then vim.schedule(refit) end
  end,
})

-- :q in the last window quits Neovim: make it the tab's only ordinary window
-- first. If Neovim then refuses (unsaved changes), the daemon adopts it again.
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
    home[win] = nil
    if vim.v.exiting ~= vim.NIL then return end
    if from and floating[from] then
      -- after the command, which may open more windows in it
      vim.schedule(function() close_tab(from) end)
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
return { first, anchor_of(api.nvim_get_current_tabpage()) }
