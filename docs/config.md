# The config

Your own Lua file chooses each daemon's pane provider: what gives Neovim's
windows terminals of their own (ADR 0007). With no file, the built-in default
chooses: Rex inside Rex, else none. `docs/capabilities.md` is the contract
of a pane provider's commands.

## The file

The daemon reads `$NEOPANE_CONFIG` if it is set and not empty, made absolute
against the directory `neopane` started in (`~` isn't expanded), else
`$XDG_CONFIG_HOME/neopane/init.lua` (an absolute `XDG_CONFIG_HOME` only),
else `~/.config/neopane/init.lua`, on macOS too. It reads
`$NEOPANE_CONFIG` once, as it starts: a later attach's never counts. With
no file at the default places, the built-in default chooses; a file
`$NEOPANE_CONFIG` names that isn't there is an error (see [Errors](#errors)).

The file runs afresh each time a terminal chooses the pane provider: when
a terminal attaches by itself (as `neopane` or `neopane attach` does) and the
daemon has none. Each run is a child process of the daemon's own binary,
in the choosing attach's working directory (`/` if it has gone), with the
environment its commands get (`docs/capabilities.md`, "Running a command").
It has 3 s to answer; once it has answered, or its time is up, its process
group is killed, so nothing it started outlives the choice. A fix to the
file applies at the next choice, and a file made after the daemon started is
read then too, wherever it is. `--provider none` (or
`NEOPANE_PROVIDER=none`) chooses none without running it.

It returns a table whose only key is `provider`, a function called with the
choosing attach:

```lua
return {
  provider = function(attach)
    if attach.env.TERM_PROGRAM == "ghostty" then
      return { terminal = ghostty, layout = aerospace }
    end
    return neopane.default(attach)
  end,
}
```

The Lua is 5.1, PUC Lua as Neovim users write it, with `string`, `table`,
`math`, `io`, `os`, `coroutine` and `package`, and no sandbox: `io`, `os`
and `dofile` stay. `require` looks only in the file's own `lua/` directory
(`lua/?.lua`, `lua/?/init.lua`), and loads no C modules. `print`,
`io.write`, `io.stdout` and `io.stderr` go to the daemon's log, line by line,
as `config: <line>`, up to 64 KiB a run; stdin is empty. Prefer
`neopane.run` to `os.execute` and `io.popen`: it gives up in time for the
config to answer.

While the file runs, the terminal that chose shows its first window at once;
new windows, and the terminals that attach meanwhile, wait for the choice.

## `attach`

| Field | |
|---|---|
| `env` | The choosing attach's environment, as it attached: names to values, raw bytes |
| `cwd` | Its working directory, absolute |
| `tty` | Its tty, such as `/dev/ttys014`; `""` when it has none |
| `pid` | The attaching helper's pid |
| `daemon` | The daemon's name |
| `provider` | The hint: `--provider P`, else `$NEOPANE_PROVIDER`; nil without one |

They are plain tables: changing them changes nothing outside this run.
The hint means what your code makes of it; only `none` means something to
neopane, and the file doesn't run for it.

## What `provider` returns

- `nil` or `false`: no pane provider. Windows are drawn in the terminal
  they were opened from, as plain Neovim draws splits.
- `"rex"`: Rex's pane provider, built in. It works only in a Rex pane.
- A pane provider table, used whole.
- A pair, `{ terminal = T, layout = L }`, or one side alone, `{ terminal = T }`
  or `{ layout = L }`: `T` is a terminal provider, which opens and closes
  terminals, and `L` a layout provider, which finds, places and focuses
  them. A pair takes only `T`'s `open` and `close` and its attach timeout,
  and only `L`'s other capabilities, `context_vars` and `find_vars`, so it
  always opens, then places. Rex is never half of a pair. An optional
  `name` names the pair, `"<T's name>+<L's name>"` by default.

The chosen pane provider's `find` (its own, or its layout side's) then
confirms the choice for the choosing attach: exit 0 or 69, or no `find`,
accepts it; anything else means no pane provider, with a warning, so a
`TMUX` leaked into a Ghostty window doesn't send new windows to a tmux pane
nobody sees. The file's 3 s are its own: `find` then has 3 s to confirm the
choice, or its own timeout (`timeouts.find`) if that is longer.

`list` and `status` show the result as the pane provider's name (a pair's
as `T+L`), `(choosing)` while the file runs or `find` hasn't answered, or
`none`, `none (config error)`, `none (find refused)` or `none (hint none)`.

## Pane provider tables

Each capability is an argv, a list of strings run once without a shell, its
whole-word placeholders such as `{attach}` replaced; `false` counts as
absent. `docs/capabilities.md` says what each is given, what it prints and
what its exit status means.

| Key | |
|---|---|
| `name` | Shown in the log, warnings, `list` and `status`; never matched |
| `open` | Opens a terminal running `{attach}`, and prints its id |
| `close` | Closes `{terminal}` |
| `find` | Prints the attaching terminal's id; exit 69: unsupported |
| `place_beside` | Moves `{terminal}` beside `{origin}` |
| `resize` | Resizes `{terminal}` along its split with `{origin}` |
| `focus` | Focuses `{terminal}` |
| `focus_direction` | Moves focus `{count}` steps `{direction}` |
| `report_layout` | Prints the layout as JSON |
| `events` | Prints a line whenever the layout changes; runs until it ends |
| `open_beside`, `open_float`, `open_tab_page` | One-step opens, from a table returned whole only |
| `timeouts` | Seconds per capability, over 0 and at most 60 |
| `attach_timeout` | Seconds from an open's exit to its terminal attaching; default 5 |
| `protocols` | The `NEOPANE_PROTOCOL` numbers it speaks; default `{ 1 }` |
| `context_vars` | Variables a terminal must share with the choosing one to be in reach |
| `find_vars` | The attaching terminal's variables `find` gets, as `$NEOPANE_ENV_<VAR>` |

No capability is required: a table with only `open` opens a terminal for
each new window and places none.

## The `neopane` global

Also `require("neopane")`.

| | |
|---|---|
| `neopane.default(attach)` | What the daemon chooses with no file: `"rex"` when `REX_SERVER` and `REX_SESSION` are both set, else nil. Reads only the environment, ignores the hint, never raises |
| `neopane.run(argv, opts)` | Runs a command without a shell and waits for it, at most `opts.timeout` seconds (1 by default, cut to the config's time left less 0.25 s). Returns `{ ok, code, stdout, stderr, error }`; raises only on bad arguments. `opts` also takes `cwd`, `env` (a string sets a variable, `false` removes it) and `stdin` |
| `neopane.which(program)` | The absolute path of the first executable `program` on `PATH`, or nil; with a `/`, that path if it is executable. Runs nothing |
| `neopane.builtin` | Functions that each return a fresh built-in table, to return as it is or copy and change. Empty for now |
| `neopane.inside` | Functions `(attach) -> ok, why` that prove the attach's terminal is a multiplexer's own pane. Empty for now |
| `neopane.os` | `"macos"` or `"linux"` |
| `neopane.version` | The daemon's version |
| `neopane.config_dir` | The absolute directory of the file that ran |

## Errors

Any error means no pane provider for that choice, never the default.

A file `$NEOPANE_CONFIG` names that isn't there is an error. Only an empty
default place, with `$NEOPANE_CONFIG` unset, means the built-in default
chooses.

A file that fails to load or raises, runs out its 3 s, ends without
answering, or returns something malformed is an error too: a wrong type,
an argv with a hole (is a variable nil?), a side with no usable
capability, a paired `"rex"`, or no protocol number shared with neopane.
Anything unknown, such as a key, a field or a placeholder, gets a warning
and is ignored, a capability holding an unknown placeholder with it; a
time over 60 s gets a warning and is cut to 60 s. So an older daemon still
reads a newer file.

Each choice shows its problems as one warning in Neovim, the first error
or else the first warning, with the count of the rest:

```text
neopane: config: provider().layout.focus[2]: expected string, got number (wrap numbers in tostring()) (and 2 more: neopane provider check)
```

The daemon's log has every one, and `list` shows `none (config error)`.
The next terminal that attaches by itself runs the file again.

A file `$NEOPANE_CONFIG` names that isn't there warns with its path:

```text
neopane: config: /home/me/my.lua isn't there (NEOPANE_CONFIG names it); no pane provider
```

A `find` that refuses the choosing attach warns too:

```text
neopane: aerospace: find didn't confirm this terminal (exit 1); no pane provider
```

## `provider check`

```text
neopane provider check [--config FILE] [--provider P] [--daemon-name N] [--no-find] [--full] [--json]
```

Runs your config as a terminal attaching here would, and reports what it
chose. It finds the file as the daemon would, and runs it in the same child
process, with the same checks and the same 3 s, for an attach made from this
shell: its environment and working directory, stdin's tty (`""` if stdin
isn't a terminal), its own pid, the daemon name and the hint. Then it runs
the chosen pane provider's `find` as a self-attach here would: it writes the
pane title `neopane-<token>` into this terminal, runs `find` with the pane
id `0-<token>`, and puts the old title back (clearing it in a terminal that
keeps no title stack). Rex's `find` needs no title. It runs no other
capability. With stdin not a terminal, it writes no title and runs no
`find`, and the result is `ok (find not run)`.

- `--config FILE`: the file; by default `$NEOPANE_CONFIG`, then the default
  places, as [The file](#the-file) says. A file either names that isn't
  there is an error, whose message says which named it.
- `--provider P`: the hint; by default `$NEOPANE_PROVIDER`. `none` skips the
  file; `""` means no hint.
- `--daemon-name N`: `attach.daemon`; by default the name a daemon started
  in this directory gets, with no `-2` suffix.
- `--no-find`: `find` isn't run.
- `--full`: shows everything. Without it, a capability's line is cut at 100
  columns with `…`, and at most 20 of the lines the config printed are
  shown, then `(N more: --full)`.
- `--json`: one JSON object, below.

The report's sections:

| Section | Shows |
|---|---|
| `config` | the file; `no file: neopane.default chooses`; or `skipped: hint none` |
| `attach` | its tty (or `no terminal`), pid, working directory, daemon name and hint |
| `ran` | how long the file took and the lines it printed; or its Lua error, with Lua's traceback where it gives one, the timeout, or why it ended without answering |
| `chose` | none, `"rex"`, a table, a pair or one side alone, its name and Lua path, and whether `provider()` or `neopane.default` chose |
| `whole`, `terminal`, `layout` | each table in use, by its role: its name, Lua path and agreed protocol; each capability it gives, present (its argv on one line, and its timeout), ignored (and why) or absent (and what does without it, such as `open_float  absent: float groups are drawn in panes`). A pair's terminal side lists the one-step capabilities too, absent from a pair, and either side lists any it gave that are ignored. `live layout` (`yes: report_layout and events`, `yes: report_layout (read after each act)` or `no: each pane is its own layout area`), `reach` (its context variables, with this attach's values) and `find vars` show for a table that gives the layout, so not for a terminal side; `attach timeout`, such as `5 s (default)`, for one that opens terminals, so not for a layout side |
| `one-step` | the one-step capabilities, or `none: a pair opens, then places` |
| `rex` | for `"rex"`: the server's URL, version and API revision (`server.version`'s, else the one its welcome agreed), and, for a server older than this build speaks, `missing  every capability (needs Rex API revision N, this server has M)`: Rex doesn't say which revision each method came in, so it isn't told capability by capability |
| `find` | the title written and put back, `find`'s exit status, time and id, and whether the choice is accepted or refused; for Rex, `proven: block:… is this terminal's`, `disproved: REX_BLOCK names another terminal (/dev/ttys009)` (the path where the system names it: on macOS, and for a pseudo-terminal on Linux), or `unprovable: <why>; trusted REX_BLOCK`; or `not run: <why>` |
| `errors`, `warnings` | every problem, with its path |
| `result` | `ok`, `none` or `error`, quoting the exact message Neovim would show |

For example, in Ghostty, for the config in
[Ghostty with AeroSpace](#ghostty-with-aerospace):

```text
config  /Users/me/.config/neopane/init.lua
attach  tty /dev/ttys014, pid 4242, cwd /Users/me/src/work, daemon work, no hint
ran  3.1 ms, 0 lines printed
chose  a pair, ghostty+aerospace, at provider(), by provider()
terminal  ghostty at provider().terminal, protocol 1
  open  sh -c 'exec open -na Ghostty.app --args --title="$NEOPANE_TITLE" --abnormal-command-exit-ru…
  close  absent: a terminal it opened closes only as its attach ends
  open_beside  absent: open, then place_beside
  open_float  absent: float groups are drawn in panes
  open_tab_page  absent: tab pages stay whole in the pane they opened in
  attach timeout  5 s (default)
layout  aerospace at provider().layout, protocol 1
  find  /Users/me/.config/neopane/bin/aerospace-find  (3 s)
  place_beside  absent: a terminal it opened stays where it opened
  resize  absent: a terminal keeps the size it opened at
  focus  aerospace focus --window-id {terminal}  (2 s)
  focus_direction  aerospace focus --boundaries-action stop {direction}  (2 s)
  report_layout  absent: each pane is its own layout area
  events  absent: no live layout
  live layout  no: each pane is its own layout area
  reach  every terminal (no context variables)
  find vars  none
one-step  none: a pair opens, then places
find  accepted: id 31337 (exit 0, 412.0 ms, title neopane-k3j9x2qpa8 written, then restored)
result  ok
```

It exits 0 for a pane provider, or for none by design (`provider()` returned
`nil` or `false`, or the hint is `none`), also when `find` wasn't run; 1 for
an error, or a `find` refusing this terminal; 2 for a usage error. Warnings
don't change it.

### `--json`

```json
{"config":{"path":"/Users/me/.config/neopane/init.lua","state":"file"},
 "attach":{"tty":"/dev/ttys014","pid":4242,"cwd":"/Users/me/src/work","daemon":"work","provider":null},
 "ran":{"seconds":0.0031,"printed":[],"error":null},
 "chose":{"kind":"pair","name":"ghostty+aerospace","path":"provider()","how":"provider"},
 "tables":[{"role":"terminal","name":"ghostty","path":"provider().terminal","protocol":1,
            "capabilities":{"open":{"state":"present","argv":["sh","-c","…","sh","{attach}"],"timeout":5.0},
                            "close":{"state":"absent","fallback":"a terminal it opened closes only as its attach ends"},…},
            "settings":{"name":"ghostty","attach_timeout":5.0},"live_layout":null,
            "context_vars":{},"find_vars":[]},
           {"role":"layout","name":"aerospace","path":"provider().layout","protocol":1,
            "capabilities":{"find":{"state":"present","argv":["/Users/me/.config/neopane/bin/aerospace-find"],"timeout":3.0},…},
            "settings":{"name":"aerospace"},"live_layout":"no","context_vars":{},"find_vars":[]}],
 "one_step":[],
 "find":{"ran":true,"title":"neopane-k3j9x2qpa8","exit":0,"seconds":0.412,"id":"31337","verdict":"accepted","why":null},
 "rex":null,
 "problems":[],
 "result":{"state":"ok","message":null}}
```

- `config.state`: `file`, `none` (no file: `neopane.default` chooses) or
  `skipped` (the hint is `none`).
- `attach.provider`: the hint, or null.
- `ran`: null when the file didn't run; `error` is null when it answered.
- `chose`: null when the config failed. `kind` is `none`, `builtin`,
  `table`, `pair`, `terminal` or `layout` (one side alone); `how` is
  `provider`, `default`, or null for the hint `none`.
- `tables[]`: `role` is `whole`, `terminal` or `layout`. Each capability its
  table lists, as the text report does, is `present` (with `argv`, and `timeout` in seconds, null for
  `events`), `ignored` (with `why`) or `absent` (with `fallback`).
  `live_layout` is `events`, `report_layout`, `no`, or null for a terminal
  side; `context_vars` maps each variable to this attach's value, or null.
- `find.verdict`: `accepted`, `refused` or `not run`, with `why` saying why
  it wasn't run; `exit` is null for a
  signal, a timeout, Rex's, or exit 69 (`why`: `unsupported`), which can't
  be told from a command that can't start.
- `rex`: null unless `"rex"` was chosen; `missing[]` is empty, or holds
  one `{"capability": null, "needs": N}` for a server older than this build
  speaks (null: every capability); `find` is `proven`, `disproved`,
  `unprovable`, or null when it didn't run.
- `problems[]`: `level` is `error` or `warning`; `path` is the Lua path, the
  file's name for its own table, or null.
- `result.state`: `ok`, `none` or `error`; `message` is what Neovim would
  show, or null.

As with `list --json`, fields are only ever added, never removed, renamed,
or changed in type or meaning; a missing one means unknown.

## Built-in pane providers

`neopane provider show` lists the built-ins. Today that is only `rex`, which
has no Lua table: return `"rex"` from `provider()` to use it.
`neopane provider show NAME` prints a built-in that has a Lua table as a Lua
file, to copy into your `init.lua` and change.

## Editor setup

`neopane provider types` writes the config's types for lua-language-server
(LuaLS) into the config's directory: the directory of `--config FILE`, else
of `$NEOPANE_CONFIG`, else `~/.config/neopane` (created if missing).

- `types/neopane.lua`, a `---@meta` file with the global `neopane` and the
  classes `neopane.Config`, `neopane.Attach`, `neopane.Provider`,
  `neopane.Timeouts`, `neopane.Pair`, `neopane.Choice`, `neopane.RunOptions` and
  `neopane.RunResult`. It is rewritten each time: run the command again after
  upgrading neopane. Its second line names the build that wrote it.
- `.luarc.json`, only if there is none: LuaLS's `$schema` and
  `"runtime.version": "Lua 5.1"`. It never sets `workspace.library`, since
  the types file is inside the workspace. Edit it as you like; later runs keep
  it.

```console
$ neopane provider types
wrote ~/.config/neopane/types/neopane.lua
wrote ~/.config/neopane/.luarc.json
no init.lua yet: see https://github.com/ian-pascoe/neorex/blob/main/docs/config.md
```

Open `~/.config/neopane` as the editor's workspace folder. LuaLS reads neither
file in single-file mode, so opening `init.lua` on its own gets no types.

Annotate the table you return to have it checked:

```lua
---@type neopane.Config
return {
  provider = function(attach)
    return neopane.default(attach)
  end,
}
```

[lazydev.nvim](https://github.com/folke/lazydev.nvim) adds Neovim's own
`vim` API to every Lua workspace, which the config doesn't have. Turn it off
where a `.luarc.json` is:

```lua
{
  "folke/lazydev.nvim",
  ft = "lua",
  opts = {
    enabled = function(root)
      return not vim.uv.fs_stat(root .. "/.luarc.json")
    end,
  },
}
```

## Ghostty with AeroSpace

An example, untested: Ghostty opens a window per Neovim window, and
AeroSpace finds, focuses and moves between them. Ghostty's `--title=` (one
word: `--title VALUE` doesn't parse, and `-e` must come last) fixes the
window's title for good, so Ghostty ignores the escape sequences that would
change it, and AeroSpace finds the window by that title. The macOS form,
`open -na Ghostty.app --args --title=… -e …`, is checked against Ghostty's
source but not run; on Linux it is `ghostty +new-window --title=… -e …`.
Tab pages are drawn in the window they opened from; a Ghostty table returned
whole, with `open_tab_page` set to `open`'s argv, gives each its own window.

```lua
-- ~/.config/neopane/init.lua
---@type neopane.Provider
local ghostty = {
  name = "ghostty",
  -- --title= fixes this window's title for good (Ghostty then ignores OSC 0/2); AeroSpace finds it by that title.
  -- macOS (source-verified, not run). Linux: 'exec ghostty +new-window --title="$NEOPANE_TITLE" -e "$@"'
  open = { "sh", "-c",
    'exec open -na Ghostty.app --args --title="$NEOPANE_TITLE" --abnormal-command-exit-runtime=0 -e "$@"',
    "sh", "{attach}" },
}
---@type neopane.Provider
local aerospace = {
  name = "aerospace",
  find = { neopane.config_dir .. "/bin/aerospace-find" },
  focus = { "aerospace", "focus", "--window-id", "{terminal}" },
  focus_direction = { "aerospace", "focus", "--boundaries-action", "stop", "{direction}" }, -- once, whatever the count
}
---@type neopane.Config
return {
  provider = function(attach)
    -- Ghostty first: REX_* leak into a Ghostty window started from a Rex shell; TERM_PROGRAM names the innermost terminal.
    if attach.env.TERM_PROGRAM == "ghostty" and neopane.which("aerospace") then
      return { terminal = ghostty, layout = aerospace }
    end
    return neopane.default(attach)
  end,
}
```

```sh
#!/bin/sh
# ~/.config/neopane/bin/aerospace-find: the AeroSpace window whose title holds $NEOPANE_TITLE.
# AeroSpace may not list a new window at once, so try for about 2.5 s (find has 3 s).
i=0
while [ "$i" -lt 10 ]; do
  ids=$(aerospace list-windows --all --format '%{window-id}|%{window-title}' |
        grep -F -- "$NEOPANE_TITLE" | cut -d'|' -f1)
  case $(printf '%s' "$ids" | grep -c .) in
    1) printf '%s\n' "$ids"; exit 0 ;;
    0) sleep 0.25; i=$((i + 1)) ;;
    *) exit 1 ;;
  esac
done
exit 1
```
