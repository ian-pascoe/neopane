# The user's own Lua chooses a daemon's one pane provider, which may pair a terminal provider with a layout provider

Amended by SPI-1135, amending #93 decisions 27 and 29: `<name>.default`
reads only the environment while it names one built-in. When it names
several, it asks each multiplexer's `inside` in order and takes the first
that proves, off the daemon's loop. A list of fallbacks tried by `find` was
rejected again: it would make a config that returns the default behave
differently from no config. It names tmux by `TERM_PROGRAM=tmux` and `TMUX`,
and Rex by `REX_SERVER` and `REX_SESSION`. tmux is the only multiplexer
proved today, its proof cut at 0.5 s; other built-in multiplexers would join
the naming and the order with their own built-ins. After the multiplexers
comes Rex if named, else none: Rex is never proven there, its own `find`
proves it. One Rust function is both the config's `<name>.default` and the
daemon's default with no config file, so the two never differ.
`<name>.inside.tmux` runs `find`'s whole check, tmux's version and then the
tty search, so it is true exactly when `find` would accept; `TMUX_PANE`
isn't needed. With no config, `find` refusing the default's guess goes only
to the log, and `list` and `status` show `none (find refused)`; a config's
choice refused still warns, quoting `find`'s first stderr line.

Amended by SPI-1142: with no config, every pane provider the default
guessed that can't start is quiet too, as is one that goes (`Gone`) before
`find` confirmed it. Examples: Rex unreachable, or refusing the session,
when stale `REX_*` leak into a terminal after Rex quit; tmux's built-in
table failing to spawn. Each goes only to the log, and `list` and `status`
show `none (can't start)`. A config's choice that can't start, or goes
before `find` confirmed it, shows the same and still warns; a pane provider
that goes once confirmed warns whoever chose it.

Amended by ADR 0016 (SPI-1131): with no config, the built-in default chooses
tmux's built-in table in a tmux pane (`TERM_PROGRAM=tmux` and `TMUX` set),
before Rex; `<name>.default` returns that table there.

Amended by ADR 0012 before implementation: one-step capabilities come only
from a pane provider returned whole.

Amended by #92 before implementation: the built-in default runs in the daemon;
choosing Rex never blocks; a pane provider that fails as a whole is dropped at
once.

Amended by #93 before implementation: the file's table holds only `provider`;
`false` means none; the daemon, not the config's process, checks what it
returned; nothing the config starts outlives it; `<name>.builtin` holds
functions, and `<name>.inside` proves where an attach is.

Amended by #128 before implementation: `<name>.run` raises on bad arguments,
such as an argv that isn't a list of strings or an unknown option, and
returns what happened otherwise.

Amended by #129 in implementation: the config has its 3 s to itself; the
chosen pane provider's `find` then has 3 s to confirm the choice, or its own
timeout if that is longer. A `$<NAME>_CONFIG` naming a file that isn't
there is an error, not "no file".

A daemon gets its pane provider from the user's own Lua file, which Neovim
never reads (#87). The file returns a `provider` function. When a self-attach
arrives and the daemon has no pane provider, the daemon runs the file afresh
and calls the function with the attach: its environment, working directory,
tty, the daemon's name and any `--provider` hint. The function returns
nothing, a built-in's name, a pane provider, or a terminal provider and a
layout provider to pair:

```lua
return {
  provider = function(attach)
    if attach.env.REX_SESSION then return "rex" end
    if attach.env.TERM_PROGRAM == "ghostty" then
      return { terminal = ghostty, layout = aerospace }
    end
  end,
}
```

So a user sets up Ghostty once and AeroSpace once and combines them in code,
rather than writing out every pairing of terminal and window manager, most of
which nobody would. Each capability is still one command run once (ADR 0006);
the Lua only chooses.

## Considered Options

- **Match rules and an ordered list in a static file**, with pairs named like
  `ghostty+aerospace`. A variable that is set says some ancestor was tmux, not
  that the terminal is in tmux, so the rules grew `TERM_PROGRAM` values, a
  tiebreak order and a tty check, and the names confused. Code says "in Rex,
  Rex; in tmux, tmux; in Ghostty, Ghostty with AeroSpace" as it is meant.
- **One definition per terminal and window manager.** Every combination
  copies its `open`.
- **Neovim's config.** The choice comes before Neovim has read any config, it
  must work under `nvim --clean`, and `g:<name>` is only a flag (ADR 0005).
- **The config's process checking its own result.** After an upgrade in place
  it is a newer binary than its daemon, and would pass fields the daemon
  doesn't know. - **Running the Lua inside the daemon.** A blocking
  `os.execute` can't be interrupted, and a slow config would stall every pane.
- **Lua functions as capabilities.** The interpreter would have to live on, or
  run again for every act.
- **LuaJIT, or Lua 5.4.** LuaJIT doesn't build statically for aarch64 musl or
  in zig cross-builds, and nothing here is hot; 5.4 isn't the dialect Neovim
  users write.
- **A list of fallbacks the daemon tries in turn.** The ordered list again.
- **Several pane providers live at once**, each window's terminal following
  its origin pane's context. Focus can't move between them (`select-pane`
  can't raise its window over Rex's), Neovim's screen would have several live
  layouts, and two tmux servers would need telling apart, all for a case plain
  Neovim already draws.
- **A later attach refreshing the environment.** It would retarget ids the
  daemon holds: each tmux server numbers its panes from `%0`.
- **Executables found on `PATH` (`<name>-tmux`), or a long-running process
  speaking a protocol.** Installing a binary would configure it, and its
  settings would need a describe command; a protocol is a second interface
  whose only gain is one process start per act.
- **A project's own config, behind a trust prompt.** A second trust store to
  build and explain, for what a `--provider` hint in a project's environment
  already does.

## Consequences

- The file is `init.lua` in `$XDG_CONFIG_HOME/<name>/`, else
  `~/.config/<name>/`, on macOS too; later `%LOCALAPPDATA%\<name>\` on
  Windows. `$<NAME>_CONFIG` names another, read from the daemon's environment
  when it starts, never from a later attach. `require` looks only in the
  file's `lua/` directory. The Lua is 5.1, Neovim's portable dialect, built in
  from vendored PUC Lua 5.1.5. With no file, the built-in `<name>.default`
  chooses, run in the daemon itself rather than a child process since it runs
  no user code: Rex inside Rex, so Rex users need no config and no process
  start. The `<name>.default` a config can call is the same function.
- The file runs afresh for each choice, in a child process of the daemon's own
  binary, with the choosing attach's working directory and the environment its
  commands get (ADR 0006). It has 3 s, then it is killed with its process
  group, and once it has answered the group is killed anyway, so nothing it
  started outlives the choice. It returns a table whose only key is
  `provider`. Other keys get a warning, since a daemon-wide setting would be
  read where no daemon starts. The process sends back what `provider` returned
  as plain data (nil, booleans, numbers, strings and tables, with a mark for
  anything else), and the daemon checks it against its own rules: after an
  upgrade in place, the binary at the daemon's path is newer than the daemon.
  `print` goes to the log. There is no sandbox: `io`, `os` and `dofile` stay,
  and only `require` is limited, to the file's `lua/` directory with no C
  modules. `<name>.run(argv)` runs a command without a shell, as a capability
  runs, returns what happened rather than raising, and gives up in time for
  the config to answer. It is never called for anything but choosing.
- The function returns `nil` or `false` for none; a built-in's name; a pane
  provider, a table of capabilities and settings used whole; or `{ terminal =
  T, layout = L }`, which takes only `T`'s `open` and `close` and only `L`'s
  layout capabilities, so a pair always opens, then places. `{ terminal = T }`
  or `{ layout = L }` uses one half alone, taking the same capabilities a pair
  takes from it; `open_beside`, `open_float` and `open_tab_page` come only
  from a pane provider returned whole, and a table holding both capabilities
  and `terminal` or `layout` is an error. Settings such as timeouts, protocol
  numbers, the variables `find` is given and context variables belong to the
  half they serve. Each `<name>.builtin` function returns a fresh table to
  return as it is, or copy and change. Each `<name>.inside` function proves,
  by asking the multiplexer, that an attach's terminal is directly its own, so
  code can fall through to its next choice where variables leaked. Rex is only
  the name `"rex"`, and is never half of a pair. Ids, the pane title and reach
  for a pair are in ADR 0006.
- `--provider none`, or `$<NAME>_PROVIDER=none`, means no pane provider
  without running the file. Any other value reaches the function as a hint:
  a project can set it, but only the user's code gives it meaning.
- The chosen pane provider's `find`, its own or its layout provider's,
  confirms the choice for the choosing attach, after the pane title is
  written: exit 0 or 69, or no `find`, accepts; anything else means no pane
  provider, with a warning. So a `TMUX` leaked into a Ghostty window doesn't
  send new windows to a tmux pane nobody sees.
- Any error means no pane provider for that choice, never the default: the
  file fails to load, runs out its 3 s or ends without answering, or returns
  something malformed, such as a wrong type or an argv with a hole; a half has
  no usable capability; `rex` is paired; or no protocol number is shared.
  Anything unknown (a key, a field, a placeholder) gets a warning and is
  ignored, a capability holding an unknown placeholder with it, and a time
  over 60 s gets a warning and is cut to 60 s, so an older daemon still reads
  a newer file. Each problem names its place, such as
  `provider().layout.focus[2]`. It shows as one Neovim warning per choice, in
  the log, in `list`, and in `<name> provider check`, which runs the file and
  the chosen `find` as a self-attach in that terminal would. The next
  self-attach runs the file again, so a fix applies at once.
- The choosing attach's window shows at once. Only acts that need the pane
  provider wait for a running choice, and self-attaches that arrive meanwhile
  wait their turn, until one gets a pane provider. A requested pane's attach
  never chooses.
- A daemon has at most one pane provider. It is kept while a terminal in its
  reach is attached or on its way, then dropped: `events` is killed, Rex's
  connection closed, and capabilities found unsupported are tried again next
  time. The next self-attach chooses again, so a daemon that went unattached
  starts afresh, as ADR 0003 has it. A later attach never changes the pane
  provider or its environment; a pane from elsewhere, where the pane provider
  can tell, is out of reach and draws its own windows.
- A pane provider that fails as a whole, as when Rex's connection breaks, is
  dropped at once: its pending acts fail and fall back, a warning shows once,
  and the next self-attach chooses again. A pane provider chosen while panes
  are attached checks each of them for reach, so panes Rex made before its
  connection broke rejoin it. Choosing Rex never blocks the daemon:
  connecting, `session.attach` and `find` are steps of its loop, with the
  config's 3 s deadline.
- Pane providers come only from the user's file and the built-ins: never from
  Neovim's config or a project's files, unless the user's own code reads one.

Research: `docs/research/pane-provider-matching.md`, `pane-title.md`,
`embedded-lua.md` and `lua-config-api.md`, each on its `research/` branch.
