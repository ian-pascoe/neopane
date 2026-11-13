# neopane

Panes for Neovim, built in Rust.

Formerly neorex. Not the original neorex by Alex Locke, which this began as
and continues under that name.

One Neovim runs in the background behind all your panes, so buffers,
registers, undo and language servers are shared. `:split` and `:vsplit` open
real Rex blocks where Neovim would put the window, and closing a block in Rex
closes its window. Focusable floats, such as pickers, open in Rex floating
panes, along with any floats attached to them. The completion menu,
unfocusable floats like notifications, and statuslines are drawn inside the
pane they belong to. With a pane provider that has no `open_float`, past
four float group panes, or when one fails to open, a picker is drawn inside the
pane it was last entered from, and while it's current and too big for that
pane, Neovim's screen shrinks to the pane so the picker lays itself out to
fit.

![neopane demo](assets/demo.gif)

## Status

Very early and rough: expect bugs and missing features.

neopane has been built with heavy use of AI, first in Zig and now in Rust.
Much of the code still needs cleanup and better design, which is happening
incrementally. Contributions are welcome.

## Requirements

- macOS, or Linux: it builds and passes its tests there, but hasn't been tried live yet (#43)
- [Rex](https://www.superlogical.com/rex), for windows in panes of their own;
  without it, neopane runs as plain Neovim in its terminal
- A recent Neovim, 0.12 or later.
- Rust (stable, 2024 edition) to build
- Zig 0.17, only for the golden tests: they build Ghostty

## Build and run

With [just](https://github.com/casey/just) (`just` lists the recipes):

```sh
just build
just test
just golden   # also the golden tests; downloads and builds Ghostty once
```

or with Cargo alone:

```sh
cargo build
cargo test
scripts/golden.sh --test golden
```

The golden tests check every frame neopane draws against
[Ghostty](https://github.com/ghostty-org/ghostty)'s terminal emulator, pinned
to one commit. They're behind the `golden` feature, so a plain build never
builds Ghostty.

To install it, optimized, as `~/.local/bin/neopane`, with `npane` linked to
it as a shorthand:

```sh
just install
just prefix=/usr/local install
```

It replaces the binary rather than writing over it, which on macOS gets a
new build killed at launch. It removes the `nrex` symlink an install before
the rename made, and warns about a `neorex` binary beside it without removing
it, since that may be the original neorex.

Run it with any Neovim arguments, in a Rex block to have each window open in a
pane of its own:

```sh
target/debug/neopane some/file.txt
```

Neovim starts with `g:neopane` set, before your config loads, so the config
can tell it's running under neopane:

```lua
{ 'sphamba/smear-cursor.nvim', cond = vim.g.neopane == nil },
```

Neovim's screen is the size of the current pane's layout area: its Rex window,
or the pane alone with no Rex. The tabline shows across the panes along the
top of each Rex window, and is fitted to the current pane's; the top panes of
another Rex window show it from its start, cut at their width. When another
pane, such as a shell, takes part of the top, those panes are narrower than
Neovim's screen, and neopane fits the tabline to them: every `'tabline'` that
isn't empty ends at the right edge of the tabline panes (a fitted tabline), so
the `%=`-aligned right end of one such as lualine's shows. A tabline too wide
for them is cut as Neovim would cut it on a screen that wide. While a `%!`
`'tabline'` is drawn, `vim.o.columns` reads as the tabline width, so a tabline
whose Lua lays out to it, such as bufferline's, fits its own widths too.
Widths a tabline computes from `'columns'` any other way don't fit by
themselves, such as lualine's `max_length` or a Vimscript `&columns`; they can
read `g:neopane_tabline_width`, set to the tabline width while it's narrower
than the screen:

```lua
local width = vim.g.neopane_tabline_width or vim.o.columns

-- lualine's tabs and buffers components
require('lualine').setup({
  tabline = {
    lualine_a = {
      { 'buffers', max_length = function() return vim.g.neopane_tabline_width or vim.o.columns end },
    },
  },
})
```

```vim
let width = get(g:, 'neopane_tabline_width', &columns)
```

While `'tabline'` is empty, neopane draws the built-in tabline itself: a copy
of Neovim 0.12's, laid out to the tabline panes, whose labels count and name
each tab page's own windows
([ADR 0002](docs/adr/0002-neorex-draws-the-built-in-tabline-itself.md)).
Set `'tabline'` to anything else and that shows instead; set it back to empty,
or put back a saved copy of neopane's value, and the built-in tabline comes
back. neopane's copy differs from the built-in tabline Neovim draws in a few
ways:

- `:set tabline?` shows neopane's value rather than nothing.
- Labels are drawn in the highlight groups `NeopaneTabLineSel` and
  `NeopaneTabLine`, which neopane keeps set from `TabLineSel` and `TabLine`.
- A label window's `'winhighlight'` is ignored.
- A color that `TabLineSel` or `TabLine` leaves unset, and `Normal` does too,
  comes from `TabLineFill`.
- A wide character the close button cuts in half is drawn blank.

`neopane help debug` lists the debug commands, which run each layer on its
own: keys, Neovim's redraw events, the renderer, and pane hand-off.

## How it runs

Running `neopane` starts a daemon in the background, which runs one headless
Neovim. The terminal you ran it in attaches to the daemon first: Neovim starts
only once the daemon has accepted that attach, at that terminal's size, and
the terminal shows its first window. When Neovim quits, `neopane` exits with
Neovim's status: `:q` 0, `:cq` 1, `:cq 3` 3, so `:cq` aborts a `git commit` as
it does in plain Neovim.

That first attach chooses the daemon's pane provider, which gives windows
terminals of their own, without holding anything up: the first window shows
at once, and the windows Neovim opened at the start, such as `-O a b`'s
second, wait for the choice and then open beside the first. Your own
`~/.config/neopane/init.lua` chooses it, in Lua: see
[docs/config.md](docs/config.md). Without one, Rex is chosen in a Rex pane
(see [Rex](#rex)), and none elsewhere. With no pane provider, or when the
choice fails, every window is drawn in the terminal it was opened from, as
plain Neovim draws splits.

The daemon has one pane provider at a time. It keeps it while a terminal in
its reach is attached or on its way, and drops it otherwise, as when the last
of them closes; one that fails as a whole, as Rex does when it quits, is
dropped at once, with a warning in Neovim. New windows are then drawn in the
terminals they were opened from, and the next attach chooses again: the
terminals attached before are checked again, and those in the new pane
provider's reach rejoin it.

These variables are read from the attach's environment, never the daemon's
own:

- `NEOPANE_PROVIDER`: a hint at the pane provider, sent with the attach;
  `none` chooses none, even in Rex, without running your config. Other
  values reach your config as `attach.provider`. `--provider P`, on
  `neopane` or `neopane attach`, gives the hint instead.
- `NEOPANE_LOG`: how much the daemon logs, read as it starts. `warn`: what
  went wrong; `info`, the default: that, and what happened; `debug`: that,
  and the trace timestamps `docs/latency.md` reads. Any other value logs a
  warning, and `info` is used.
- `NEOPANE_TMPDIR`: where daemons keep their files, instead of `/tmp`. A
  relative path is taken from the directory `neopane` runs in.
- `NEOPANE_TRANSPORT`: how a pane's terminal reaches the daemon, for
  diagnosis. `fd`, the default (unset or empty too): the helper hands the
  terminal over. `relay`: the helper copies its bytes to and from the daemon
  instead, as it must on Windows. Every helper reads it, and a daemon started
  under `relay` asks for its panes with `--transport relay` added to their
  command, which overrides the variable. Any other value prints `neopane:
  ignoring NEOPANE_TRANSPORT=<value> (use fd or relay)`, and `fd` is used.
- `NEOPANE_CONFIG`: the config file, in place of
  `~/.config/neopane/init.lua`. The daemon reads it once, as it starts,
  made absolute against the directory it started in; a later attach's never
  counts.

Each window's pane runs `neopane attach --address <the daemon's address>
--pane <pane id>`, which hands its terminal to the daemon (ADR 0013,
`docs/attach-protocol.md`) and waits until the daemon ends the attach; the
pane provider then closes the pane. The helper, not the daemon, puts its terminal in raw mode and
back, so a daemon that crashes or is killed leaves every terminal usable: out
of raw mode and the alternate screen, with a line naming the daemon's log. The
pane id, such as `2-k3j9x2qpa8`, is good for that one pane's terminal only.

### From another terminal

Any other terminal can show the same Neovim: run `neopane attach` there. It
attaches to the one daemon running; with several, name one with `--name`
(see [Several daemons](#several-daemons)), or give its address,
`--address $NEOPANE_TMPDIR/neopane-<uid>/<daemon name>/sock` (`/tmp` for an
unset `NEOPANE_TMPDIR`). It shows a window no terminal shows yet, Neovim's current
one first, then by tab page and window order, so after `neopane -p a b c` a
second terminal shows `b`; with none, a new window, as `:split` opens one.
`neopane attach notes.md` opens `notes.md` there in a new window instead, and
`neopane attach +12 notes.md` puts the cursor on line 12. A relative path is
taken from the directory `attach` runs in. The window it shows
becomes current. Closing that terminal closes its windows, and only those;
the terminal goes back to its shell. With no pane provider yet, the attach
chooses one, as the first did. In the pane provider's reach, as in a Rex block
of the chosen Rex session, `:vsplit` there opens a pane beside it; out of its
reach, `:vsplit` draws the new window in that terminal. Run in the daemon's own `:terminal`, it exits 1 with `neopane: a
terminal in <name>'s own Neovim can't attach to it`.

`attach` finds its daemon by `--address`, then `--name`, then `$NEOPANE`, the
address a daemon sets in its Neovim's `:terminal`s, then the only one
running. It never starts one. When it finds none, it says why in one line and
exits 1:

- `neopane: no daemon is running; start one with neopane [FILE]`
- `neopane: 3 daemons are running (notes, scratch, work); choose one with
  --name`
- `neopane: no daemon named work is running (running: notes, scratch)`
- `neopane: no daemon answers at /tmp/neopane-501/work/sock`

`$NEOPANE` where nothing answers is ignored, with `neopane: ignoring $NEOPANE:
nothing answers at …`. `attach` takes only `[+LINE] [FILE]`; anything else is
a usage error, `neopane: attach takes [+LINE] [FILE]`. `--provider P` is the
hint `NEOPANE_PROVIDER` gives, and wins over it; it counts only when this
attach chooses the pane provider, and otherwise the daemon logs `hint P
ignored: work keeps rex`.

### Several daemons

`neopane --name work` attaches to the daemon named `work` if it runs, exactly
as `neopane attach --name work` would, and otherwise starts a daemon named
exactly `work`, with no `-2` added. So `neopane --name work` in two terminals
shows one daemon in both. A name is 1 to 32 letters, digits, `.`, `_` and
`-`, and not `.` or `..`; anything else exits 2 with `neopane: --name: use
1-32 letters, digits, '.', '_' or '-'`. A running daemon takes only `[+LINE]
FILE`: `neopane --name work -O a b` with `work` running exits 2 with
`neopane: work is running, so it takes only [+LINE] FILE; -O is for starting
a daemon (open windows from Neovim)`.

`neopane --address A [+LINE] [FILE]` is exactly `neopane attach --address A
[+LINE] [FILE]`, and never starts a daemon. `--name` and `--address`
together exit 2.

### When the last pane goes

A daemon lives while a terminal shows one of its windows, or one is on its
way (ADR 0003). Panes that show only floats, such as a picker's, don't count.
When the last one goes, whatever closed it, the daemon sends Neovim
`FocusLost`, so an autosave plugin can save, and waits for Neovim to be idle,
a second at most. Then:

- With nothing unsaved, it ends Neovim, as neopane always has, by closing
  its channel: `VimLeavePre` and `VimLeave` run, and swap files go.
- With unsaved changes, buffers `:qa` would refuse to quit over, it stays,
  logs their names, and lists the files among them in `unsaved` in its
  directory: a first line `unsaved 1`, then each file's absolute path on a
  line of its own. An unnamed buffer, or one a plugin names `oil://…`, is
  logged but not listed. It keeps one window, the last current one, and
  closes the rest. `neopane attach --address …` brings that window back, with
  your changes, and removes `unsaved`; `neopane attach --address … FILE` opens
  FILE and closes the kept window. Saving or throwing away the last change
  ends it, and so do `neopane kill --force` and a signal, keeping the changes
  in swap files.

A terminal that attaches while the daemon waits keeps it. One that attaches
once Neovim is ending is refused with `stopping`. `:q` in the last window
still quits Neovim, as `:q` always does.

If stdin isn't a terminal, `neopane` prints `neopane: stdin isn't a terminal;
to start a daemon without one, use --detach` and exits 1.

Each daemon has a name: the name of the directory `neopane` was started in,
cut to its letters, digits, `.`, `_` and `-`, and to 32 characters (`default`
if nothing is left), with `-2`, `-3`, … added while a daemon of that name
runs. A daemon keeps its files (its own socket `sock`, Neovim's `nvim.sock`,
its `log`, and `unsaved` while it stays for unsaved changes) in `$NEOPANE_TMPDIR/neopane-<uid>/<daemon name>/`, `/tmp`
standing in for an unset `NEOPANE_TMPDIR` (ADR 0004). The directory `neopane-<uid>` must be yours, a real
directory, and mode 0700, or `neopane` refuses to start and names
`NEOPANE_TMPDIR`. The daemon holds a lock on its own directory while it runs,
which is how it's known to be running. Its log stays after it ends, however it
ends; the next daemon of that name keeps it as `log.1`, replacing an older
one. `neopane --help` says the same.

## Rex

Rex's pane provider is built in. An attach chooses Rex when `REX_SERVER` and
`REX_SESSION` are set in its environment, as Rex sets them in every block: the
daemon connects to that Rex server and session, as the client `neopane`, and
each window gets a Rex block of its own beside the one it was opened from,
labelled `neopane`. Rex checks the choice: the terminal must be the Rex block
`REX_BLOCK` names, its terminal compared with the terminal of that block's
process. The choice fails, with a warning in Neovim, in a terminal started
from a Rex shell, such as a Ghostty window, which inherited stale `REX_*`, or
one whose `REX_BLOCK` is unset or names another block. A later attach is in
Rex's reach in a Rex block of the same server and session, found by its
`REX_BLOCK`. The variables, from the attach's environment:

- `REX_SERVER`: the URL of the Rex server to connect to.
- `REX_SESSION`: the Rex session to open blocks in.
- `REX_BLOCK`: the id of the block the attach runs in, which Rex checks.

Rex's connection lasts as long as Rex is the daemon's pane provider: it
closes when no Rex block is left attached, or when Rex quits or fails, and a
new one opens when Rex is chosen again, its blocks rejoining.

`neopane debug-rex SOCKET`, Rex's own debug tool, reads the same three
variables from its own environment.

To try a build without it grabbing the focus in your own Rex session, which
neopane does as it works, make a session of your own with `rex new`, and run it
there, or in a terminal started with `REX_SESSION` set to that session's id
(`REX_SERVER` stays as Rex set it). A terminal started from one of your own
blocks inherits that block's `REX_BLOCK`, which the daemon would use as the first
window's block, in your session: unset `REX_BLOCK`, or set it to a block in the
new session.

## Command reference

`neopane --help` (also `-h`, `help`, or `-h` after any command) prints:

```text
neopane: one Neovim in the background, shown in every terminal attached to it.

Usage:
  neopane [OPTIONS] [NVIM-ARGS...]
  neopane attach [--name N | --address A] [--provider P] [+LINE] [FILE]
  neopane list [--json]
  neopane status [--name N | --address A] [--json]
  neopane kill [--name N | --address A | --all] [--force]
  neopane provider check [--config FILE] [--provider P] [--daemon-name N]
                        [--no-find] [--full] [--json]
  neopane provider show [NAME]
  neopane provider types [--config FILE]
  …
```

Not built yet, each exiting 1 with `neopane: <what> isn't built yet`:
`provider check`, and the bare form's `--detach` and `--no-wait`. `--provider` with `--detach` exits 2: it is for
the terminal that chooses the pane provider.

### Own options first, then Neovim's

`neopane` reads its own options only from the front of the command line
([ADR 0014](docs/adr/0014-own-options-come-first-and-command-words-never-name-files.md)).
Long ones take ` VALUE` or `=VALUE`, and none can be abbreviated. The first
word that isn't one of them, and every word after it, goes to Neovim as
typed, a `--` included: `neopane -- --name` opens a file named `--name`. One of
`neopane`'s options after Neovim's have begun is a usage error saying where it
goes.

Only the first argument can be a command word (`attach`, `list`, `status`,
`kill`, `provider`, `help`, `version`), and it runs the command even when a
file of that name exists, with a note on stderr; `neopane ./list` opens the
file. `detach` is held for a later command.

### list, status and kill

`neopane list` asks every running daemon at once, giving each 2 s, and
prints a row per daemon, sorted by name, under a header that is always
printed. Directories whose daemon is gone are skipped. With none running,
stderr also gets `neopane: no daemon is running`, and it exits 0.

```text
NAME     PANES   UNSAVED  PROVIDER  DIRECTORY
notes    0       2        none      ~/notes
work     3       0        rex       ~/src/neorex
old      -       -        -         (neopane 0.0.9, attach protocol 0)
stuck    -       -        -         (didn't answer in 2 s)
```

- PANES: the terminals attached, window and float group panes alike, with
  `kept` after it for a daemon started with `--detach`.
- UNSAVED: how many buffers have unsaved changes.
- PROVIDER: the pane provider (a pair as `T+L`), `(choosing)` while your
  config runs or the choice is being confirmed, or `none`,
  `none (config error)`, `none (find refused)` or `none (hint none)`.
- DIRECTORY: Neovim's global working directory, `~` standing for `$HOME`.
- A daemon of another attach protocol version shows its build and version
  instead; one that didn't answer, why.

`neopane status [--name N | --address A]` finds its daemon as `attach` does,
with the same messages, and prints it:

```text
work
  directory  ~/src/neorex
  panes      3
  unsaved    README.md, src/main.rs
  stays      while something is unsaved
  provider   rex
             has: open_beside, open_float, open_tab_page, close, find,
                  place_beside, resize, focus, focus_direction, report_layout,
                  events
             found unsupported: open_float
  address    /tmp/neopane-501/work/sock
  pid        4242
  build      neopane 0.1.0 (a1b2c3d, 2026-11-09), attach protocol 1
  log        /tmp/neopane-501/work/log
```

Unsaved files are shown relative to the directory. `found unsupported` lists
the capabilities found unsupported since the pane provider was chosen. A
daemon of another version shows only its name, `pid` and `build`, then
`(another version: only its name, pid and build are known)`. A daemon that
doesn't answer in 2 s is a failure: `neopane: work didn't answer in 2 s (log:
…)`, exit 1.

`--json` prints, for `list`, `{"daemons":[…]}` with one object per row, and
for `status` the one object:

```json
{"name":"work","address":"/tmp/neopane-501/work/sock","pid":4242,
 "build":"neopane 0.1.0 (a1b2c3d, 2026-11-09)","v":1,"same_version":true,
 "dir":"/Users/ian/src/neorex","panes":3,"unsaved":["/Users/ian/src/neorex/README.md"],
 "provider":"rex","capabilities":["open_beside","close","find"],"unsupported":[],
 "kept":false,"log":"/tmp/neopane-501/work/log","terminals":["/dev/ttys004"]}
{"name":"old","address":"…","pid":77,"build":"neopane 0.0.9","v":0,"same_version":false}
{"name":"stuck","address":"…","error":"didn't answer in 2 s"}
```

`unsaved` holds absolute paths, `""` for an unnamed buffer; `terminals` the
ptys of Neovim's running `:terminal` jobs. Paths that aren't UTF-8 have
U+FFFD in place of what isn't. The JSON is stable: fields are only ever
added, never removed, renamed, or changed in type or meaning. Read it
ignoring fields you don't know, and take a missing field as unknown, as for
a daemon of another version.

`neopane kill [--name N | --address A | --all] [--force]` ends a daemon,
found as `attach` finds one, or with `--all` every one running. It never
sends a signal: it asks the daemon to stop, and the daemon ends its attaches
(`stopped`, exit 0), closes the terminals it asked the pane provider for, and
ends Neovim by closing its channel, so swap files are kept. While Neovim has
unsaved changes, the daemon refuses unless `--force`, and nothing ends:

```text
neopane: work has unsaved changes: README.md, src/main.rs (save them, or kill --force to keep them only in swap files)
```

`kill` returns once the daemon is gone, printing nothing, and waits 10 s at
most: `neopane: work is still ending after 10 s (pid 4242)` or `neopane: work
didn't answer in 10 s (log: …)`, exit 1. With several running and no
`--name`: `neopane: 3 daemons are running (notes, scratch, work); choose one
with --name, or use --all`. With none running, `neopane: no daemon is
running` and exit 1; `--all` then exits 0. `--all` prints a line per refusal
or failure, and exits 1 if there was any. `--force` loses unsaved changes
under `-n`, which keeps no swap files.

### Version

`neopane -v` (also `--version`, `version`) prints the build, then the
protocols it speaks:

```text
neopane 0.0.0 (a1b2c3d, 2026-11-09)
attach protocol 1
pane provider protocols 1
```

### Refused Neovim arguments

These can't work under a daemon, so `neopane` refuses them, saying why, and
starts nothing:

| Word | Why |
|---|---|
| `--embed`, `--headless`, `--listen…` | the daemon starts Neovim with them itself |
| `--server…`, `--remote…` | they talk to another Neovim |
| `--help`, `--version`, `--api-info`, `-h`, `-?`, `-v` after other arguments | Neovim would print and exit |
| `-l`, `-es`, `-Es` | they run Neovim without a screen |
| `-V`, or `-V2` and up, without a file | Neovim wouldn't start |
| `-` | the daemon's Neovim can't read stdin |
| `-r` or `-L` with no file | it only lists swap files |

Anything else Neovim itself rejects fails the start.

### Output and exit statuses

What was asked for (help, version, `list`, `status`) goes to stdout; everything else goes to
stderr as one `neopane: …` line. A usage error is one line ending
`(see neopane --help)`. A reader that closes the pipe early ends `neopane`
quietly.

| Status | Meaning |
|---|---|
| 0 | success |
| 1 | a failure or refusal |
| 2 | a usage error, or a refused Neovim argument |
| Neovim's | `neopane`, once Neovim quits: `:q` 0, `:cq` 1, `:cq N` N |

## Troubleshooting

- Windows open in the terminal you ran `neopane` in, not in Rex blocks: the
  daemon chose no pane provider. With a warning in Neovim (`:messages`), Rex
  couldn't be reached or attached, or didn't confirm the terminal is the pane
  `REX_BLOCK` names, as for a terminal started from a Rex shell; without one,
  `REX_SERVER` or `REX_SESSION` is unset, or `NEOPANE_PROVIDER` is `none`. The
  log says which. With a config file, `neopane: config: …` in Neovim and
  `none (config error)` in `neopane list` mean it failed or returned
  something malformed; the log has every problem (see
  [docs/config.md](docs/config.md#errors)).
- `neopane: work didn't start: <why> (log: <path>)`: read the log at that
  path. A daemon that failed before it had a log says only why, such as
  `neopane: /tmp/neopane-501 isn't a private directory (owner 0, mode 0755); set
  NEOPANE_TMPDIR to another place` or `neopane: the socket path for work would
  be 109 bytes, over the 103 allowed; use a shorter --name or NEOPANE_TMPDIR`.
- `neopane: the daemon didn't start: <why> (logs: <base>)`: the daemon ended
  before it said anything; its log, if it made one, is under `<base>`.
- A daemon's log is `/tmp/neopane-<uid>/<daemon name>/log` (under
  `$NEOPANE_TMPDIR` if set), and the one before it `log.1`.
- While it runs, Neovim's own messages go to the same log, since the
  daemon's stderr is Neovim's.

## Current limitations

- The `<C-w>` resize, swap and move commands do nothing. Use Rex's own pane
  controls instead. The exception is resizing a window in a tab page's
  floating pane, below: that resizes its Rex block.
- `<C-w>h/j/k/l` (also `<C-w><C-h>`, `<C-w><BS>` and `<C-w><Left>` and the like,
  in Normal and Visual mode) move to the neighbouring pane in Rex, and stay
  put at an edge. A count repeats the move, up to 20 times. Mappings that
  remap them work, such as LazyVim's `<C-h>`; a `noremap` mapping to `<C-w>h`
  (kickstart.nvim's) skips them, and so do `:wincmd h` and `vim.cmd.wincmd`
  from a plugin: those step through the panes like `<C-w>w`. The move happens
  after the keys typed with it: `<C-w>lZZ` in a mapping or macro runs `ZZ` in
  the old window.
  - Rex servers whose `session.focus_direction` has no `wrap` option wrap
    around at an edge. Ones without the method do nothing for these keys,
    and log the refusal.
  - Rex looks at every pane in its window, so a move can enter an open
    floating pane, such as a tab page's, and leave it.
- Plugins that arrange windows (sidebars, layout managers) don't get the
  placement they expect, except in a tab page's floating pane, below. A
  window such a plugin moves back into Neovim's own layout goes back to its
  pane. edgy.nvim works without turning it off:
  neopane tells it to leave the windows in panes alone (`vim.w.edgy_disable`),
  so its edges don't dock anything. A plugin that keeps moving a window back
  as fast as neopane puts it in its pane (5 times in 2 seconds, with nothing
  typed and no pane resized) wins for a while: that window stays in Neovim's
  layout, its pane stays blank, and you still type into it. neopane tries
  again after 2 seconds, then 4, up to a minute. Turn such a plugin off with
  `cond = vim.g.neopane == nil`.
- Neovim tab pages aren't Rex tabs. A tab page opened after startup, as
  `:tabnew`, Neogit and Diffview open, shows in a Rex floating pane and
  closes with its last window. Its windows' panes are placed as Neovim's
  layout has them, at about their sizes, so Diffview's file panel is on the
  left. They follow the layout as windows come and go, and a window Neovim
  resizes, such as with `<C-w>|`, resizes its pane. Moving windows once they
  show in panes, as with `<C-w>H`, still does nothing.
- A picker or other float group in a Rex floating pane shows over whichever
  tab page is current, and closes with the tab page it opened in: closing
  the tab page it shows over sends it back to its own first, but for the
  float you're in, which closes, as Neovim closes the current float first.
- Neovim's tab-page queries don't match neopane's tab pages (#67). Every
  window shown in a pane sits in Neovim's current tab page, so
  `tabpagebuflist()`, `tabpagewinnr()` and `nvim_tabpage_list_wins()` see all
  of them in the current tab page, along with a hidden window neopane keeps in
  each tab page, and see the other tab pages as holding only that hidden
  window. Plugins that label tab pages from them, such as lualine's `tabs`
  component, show the wrong labels. The built-in tabline, while `'tabline'`
  is empty, shows the right ones: neopane draws it from its own record of each
  window's home tab page.
- Plugins that draw cursor effects with editor floats, such as
  smear-cursor.nvim, draw them in the wrong place: Neovim can't tell them
  where a pane's window is.
- No external command line or messages: long messages still show
  "Press ENTER".
- No global statusline: neopane treats `laststatus=3` as `2`, so each pane
  shows a statusline, the current window's: with lualine's
  `globalstatus = true`, the same line in every pane. Set `globalstatus`
  explicitly: lualine skips neopane's windows without it, and its default,
  `vim.go.laststatus == 3`, is false once neopane has turned `laststatus=3`
  into `2`.
  A pane's own `setlocal statusline` is replaced by that line meanwhile, and
  comes back when the pane takes focus, or when `laststatus` is set to
  another value.

## Plans

Much of neopane will be split out into standalone crates:

- a Neovim RPC client (msgpack, message framing, requests)
- a Rex API client (WebSocket and the control protocol)
- terminal key input (the kitty keyboard protocol)

neopane should then be easier to extend to other editors that provide similar processes for running external windows.

## Renamed from neorex

neopane was called neorex until the rename
([ADR 0005](docs/adr/0005-neorex-becomes-neopane-with-no-aliases-for-its-old-names.md)).
The old names have no aliases: change them by hand.

| Was | Now |
| --- | --- |
| `neorex`, and its shorthand `nrex` | `neopane`, and its shorthand `npane` |
| `vim.g.neorex`, `g:neorex` | `vim.g.neopane`, `g:neopane` (always `1`) |
| `g:neorex_tabline_width` | `g:neopane_tabline_width` |
| `NeorexTabLineSel`, `NeorexTabLine` | `NeopaneTabLineSel`, `NeopaneTabLine` |
| `$NEOREX`, `$NEOREX_PROVIDER`, `$NEOREX_LOG`, `$NEOREX_TMPDIR`, `$NEOREX_TRANSPORT`, `$NEOREX_CONFIG` | `$NEOPANE`, `$NEOPANE_PROVIDER`, `$NEOPANE_LOG`, `$NEOPANE_TMPDIR`, `$NEOPANE_TRANSPORT`, `$NEOPANE_CONFIG` |
| `~/.config/neorex/` | `~/.config/neopane/` |
| `/tmp/neorex-<uid>/` | `/tmp/neopane-<uid>/` |
| the Rex block label and client name `neorex` | `neopane` |

A daemon started before the rename keeps running under its old name, and
leftover `neorex-<uid>` directories are left alone: remove them once no old
daemon runs. `just install` removes the old `nrex` symlink, and only warns
about a `neorex` binary, which may be the original's.

## Credits

neopane began as neorex, created by [Alex Locke](https://github.com/alexrlocke)
in Zig at [alexrlocke/neorex](https://github.com/alexrlocke/neorex). The idea
of showing each of one Neovim's windows in a terminal pane of its own, through
external windows, is his. This repository started as a fork of it and has
continued independently, rewritten in Rust and renamed so the original keeps
its name; the original neorex is still active, and is a separate project. The
commit history from the original is kept, and LICENSE keeps his copyright.

## License

MIT, as the original. See [LICENSE](LICENSE).
