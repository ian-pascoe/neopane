-- Run once before the config: sends print, io.write, io.stdout and
-- io.stderr through `out`, which writes to the config child's stderr at
-- once, and wraps the API's Rust functions so bad arguments raise a plain
-- message at the caller.
local out, api, run, which = ...
local select, tostring, type, error, concat = select, tostring, type, error, table.concat

io.stdout:setvbuf("no")

local function write(self, ...)
  for i = 1, select("#", ...) do
    local v = select(i, ...)
    if type(v) ~= "string" and type(v) ~= "number" then
      error(("bad argument #%d to 'write' (string expected, got %s)"):format(i, type(v)), 3)
    end
    out(tostring(v))
  end
  return self
end

local function standard_file()
  local f = {}
  function f:write(...) return write(self, ...) end
  function f:flush() return self end
  function f:setvbuf() return true end
  function f:close() return nil, "cannot close standard file" end
  return f
end

io.stdout, io.stderr = standard_file(), standard_file()

function io.write(...) return write(io.stdout, ...) end

function print(...)
  local n, parts = select("#", ...), {}
  for i = 1, n do parts[i] = tostring((select(i, ...))) end
  out(concat(parts, "\t", 1, n) .. "\n")
end

local function raising(f)
  return function(...)
    local result, err = f(...)
    if err then error(err, 2) end
    return result
  end
end

api.run = raising(run)
api.which = raising(which)
for name, f in pairs(api.builtin) do api.builtin[name] = raising(f) end
