# The user's own Lua chooses a daemon's one pane provider, which may pair a terminal provider with a layout provider

Amended by ADR 0012 before implementation: one-step capabilities come only
from a pane provider returned whole.

Amended by #92 before implementation: the built-in default runs in the daemon;
choosing Rex never blocks; a pane provider that fails as a whole is dropped at
once.

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
- **Running the Lua inside the daemon.** A blocking `os.execute` can't be
  interrupted, and a slow config would stall every pane.
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
  binary, with the choosing attach's environment and working directory. It has
  3 s, then it is killed with its process group. It returns data only:
  strings, numbers and tables. `print` goes to the log. There is no sandbox,
  and `<name>.run(argv)` runs a command without a shell, as a capability runs.
  It is never called for anything but choosing.
- The function returns `nil` for none; a built-in's name; a pane provider, a
  table of capabilities and settings used whole; or `{ terminal = T, layout =
  L }`, which takes only `T`'s `open` and `close` and only `L`'s layout
  capabilities, so a pair always opens, then places. `{ terminal = T }` or
  `{ layout = L }` uses one half alone, taking the same capabilities a pair
  takes from it; `open-beside`, `open-float` and `open-tab-page` come only
  from a pane provider returned whole. Settings such as timeouts, protocol
  numbers, the variables `find` is given and context variables belong to the
  half they serve. `<name>.builtin` gives each built-in as a fresh table to
  return as it is, or copy and change. Rex is only the name `"rex"`, and is
  never half of a pair. Ids, the pane title and reach for a pair are in ADR
  0006.
- `--provider none`, or `$<NAME>_PROVIDER=none`, means no pane provider
  without running the file. Any other value reaches the function as a hint:
  a project can set it, but only the user's code gives it meaning.
- The chosen pane provider's `find`, its own or its layout provider's,
  confirms the choice for the choosing attach, after the pane title is
  written: exit 0 or 69, or no `find`, accepts; anything else means no pane
  provider, with a warning. So a `TMUX` leaked into a Ghostty window doesn't
  send new windows to a tmux pane nobody sees.
- Any error means no pane provider for that choice, never the default: the
  file fails to load, runs out its 3 s or returns a bad value; a half has no
  usable capability; `rex` is paired; or no protocol number is shared. It
  shows as a Neovim warning, in the log, in `list` and in `<name> provider
  check`. The next self-attach runs the file again, so a fix applies at once.
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

Research: `docs/research/pane-provider-matching.md`, `pane-title.md` and
`embedded-lua.md`, each on its `research/` branch.
