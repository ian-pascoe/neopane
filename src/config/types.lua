---@meta
-- __BUILD__
-- Written by `__NAME__ provider types`; it rewrites this file. Types for the
-- config's API, for lua-language-server: see docs/config.md.

---The table init.lua returns. `provider` is its only key.
---@class __NAME__.Config
---@field provider fun(attach: __NAME__.Attach): __NAME__.Choice Called when a terminal chooses the pane provider

---What `provider` may return: nil or false for none, "rex", a pane provider
---table used whole, or a pair.
---@alias __NAME__.Choice nil|false|"rex"|__NAME__.Provider|__NAME__.Pair

---The choosing attach: the self-attach the pane provider is being chosen for.
---Plain tables: editing them changes nothing outside this run.
---@class __NAME__.Attach
---@field env table<string, string> Its terminal's environment, raw bytes
---@field cwd string Its terminal's working directory, absolute
---@field tty string Its terminal's tty, such as "/dev/ttys014"; "" when it has none
---@field pid integer The attaching helper's pid
---@field daemon string The daemon's name
---@field provider? string The hint: --provider, else $__UPPER___PROVIDER; nil without one

---A pane provider table. Each capability is an argv, a list of strings, with
---whole-word placeholders such as {attach}; false counts as absent. See
---docs/capabilities.md.
---@class __NAME__.Provider
---@field name? string Shown in the log, warnings and status; never matched
---@field open? string[]|false Opens a terminal running {attach}; prints its id
---@field close? string[]|false Closes {terminal}
---@field find? string[]|false Prints the attaching terminal's id; exit 69: unsupported
---@field place_beside? string[]|false Moves {terminal} beside {origin}
---@field resize? string[]|false Resizes {terminal} along its split with {origin}
---@field focus? string[]|false Focuses {terminal}
---@field focus_direction? string[]|false Moves focus {count} steps {direction}
---@field report_layout? string[]|false Prints the layout as JSON
---@field events? string[]|false Prints a line whenever the layout changes; runs until it ends
---@field open_beside? string[]|false Opens a terminal beside {origin} in one step (a table returned whole only)
---@field open_float? string[]|false Opens a floating terminal over {origin} (a table returned whole only)
---@field open_tab_page? string[]|false Opens a terminal for a new tab page (a table returned whole only)
---@field timeouts? __NAME__.Timeouts Seconds per capability, over 0, at most 60
---@field attach_timeout? number Seconds from an open command's exit to the attach, default 5, at most 60
---@field protocols? integer[] The __UPPER___PROTOCOL numbers it speaks, default { 1 }
---@field context_vars? string[] Variables a self-attached terminal must share with the choosing attach to be in reach
---@field find_vars? string[] The attaching terminal's variables given to find as $__UPPER___ENV_<VAR>

---Timeouts in seconds, over 0 and at most 60. `events` has none.
---@class __NAME__.Timeouts
---@field open? number|false Default 5
---@field close? number|false Default 2
---@field find? number|false Default 3, its own retries included
---@field place_beside? number|false Default 2
---@field resize? number|false Default 2
---@field focus? number|false Default 2
---@field focus_direction? number|false Default 2
---@field report_layout? number|false Default 1
---@field open_beside? number|false Default 5
---@field open_float? number|false Default 5
---@field open_tab_page? number|false Default 5

---A terminal provider and a layout provider, or one side alone. The terminal
---side gives open, close and the attach timeout; the layout side the layout
---capabilities, context_vars and find_vars.
---@class __NAME__.Pair
---@field name? string Shown, never matched; default "<terminal's>+<layout's>"
---@field terminal? __NAME__.Provider|false
---@field layout? __NAME__.Provider|false

---@class __NAME__.RunOptions
---@field timeout? number Seconds, default 1; cut to the config's time left less 0.25 s
---@field cwd? string Default: the attach's working directory
---@field env? table<string, string|false> Set (string) or remove (false) variables for this command
---@field stdin? string Written to its stdin, then closed; default empty

---@class __NAME__.RunResult
---@field ok boolean It exited with status 0
---@field code? integer Exit status; nil if it couldn't start, timed out or died by a signal
---@field stdout string Up to 1 MiB, raw bytes
---@field stderr string Up to 1 MiB, raw bytes
---@field error? string "can't start: …", "timed out after 1 s", "killed by signal 9", "no time left (the config has 3 s)"

---The built-in pane provider tables: each function returns a fresh copy, to
---return as it is or copy and change. Every capability runs __NAME__ itself, as
---{exe} provider builtin <name> <capability>. They take no options.
---@class __NAME__.Builtin
---@field tmux fun(): __NAME__.Provider tmux's: a pane per window, in the tmux server the choosing attach runs in
---@field herdr fun(): __NAME__.Provider herdr's: a pane per window, in the herdr server the choosing attach's HERDR_SOCKET_PATH names
---@field zellij fun(): __NAME__.Provider zellij's: a pane per window, in the zellij session the choosing attach runs in

---The config's API, also require("__NAME__").
---@class __NAME__
---@field os string "macos" or "linux"
---@field version string The daemon's version
---@field config_dir string The absolute directory of the file that ran
---@field builtin __NAME__.Builtin Functions that each return a fresh built-in pane provider table. "rex" is only a name.
---@field inside table<string, fun(attach: __NAME__.Attach): boolean, string> Functions telling whether the attach's terminal is directly a multiplexer's own pane, and why: tmux and herdr, asking it within 0.5 s, and zellij, from the process table. They read attach.env, attach.tty and attach.pid, and never raise.
__NAME__ = {}

---What `provider` returns by default, as the daemon chooses with no config: of
---the built-ins the environment names (tmux when TERM_PROGRAM is "tmux" and
---TMUX is set and not empty, herdr when TERM_PROGRAM is "herdr" and
---HERDR_SOCKET_PATH is set and not empty, zellij when ZELLIJ is set and
---ZELLIJ_PANE_ID is set and not empty, "rex" when REX_SERVER and REX_SESSION
---are both set), the one named, running nothing; naming several, the first
---multiplexer whose __NAME__.inside proves, tmux's, herdr's then zellij's,
---else "rex" if named; else nil. tmux's, herdr's and zellij's are their
---tables, as __NAME__.builtin.tmux(), __NAME__.builtin.herdr() and
---__NAME__.builtin.zellij() return them. Never raises.
---@param attach __NAME__.Attach
---@return "rex"|__NAME__.Provider|nil
function __NAME__.default(attach) end

---Run a command without a shell and wait for it. Raises only on bad arguments.
---@param argv string[]
---@param opts? __NAME__.RunOptions
---@return __NAME__.RunResult
function __NAME__.run(argv, opts) end

---The absolute path of the first executable `program` on PATH, or nil; with a
---`/`, that path if it is executable. Runs nothing.
---@param program string
---@return string|nil
function __NAME__.which(program) end
