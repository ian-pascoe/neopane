# neopane

Panes for Neovim, built in Rust.

Formerly neorex. Not the original neorex by Alex Locke, which this began as
and continues under that name.

- [What it does](#what-it-does)
- [Install](#install)
- [Using it](#using-it)
- [Pane providers](#pane-providers)
- [Command reference](#command-reference)
- [Neovim arguments](#neovim-arguments)
- [Environment](#environment)
- [Files and logs](#files-and-logs)
- [Troubleshooting](#troubleshooting)
- [Differences from plain Neovim](#differences-from-plain-neovim)
- [Debug commands, build and tests](#debug-commands-build-and-tests)
- [Renamed from neorex](#renamed-from-neorex)
- [Plans](#plans), [Credits](#credits), [License](#license)

## What it does

`neopane` runs one Neovim in the background, a daemon, and shows it in every
terminal attached to it. Buffers, registers, undo and language servers are
shared by every terminal showing it.

With a pane provider, each of Neovim's windows gets a terminal of its own.
In Rex, `:split` and `:vsplit` open real Rex blocks where Neovim would put the
window, and closing a Rex block closes its window. Focusable floats, such as
pickers, open in Rex floating panes with any floats attached to them: a float
group pane. A tab page opened later, as `:tabnew` opens one, gets a tab page
area of its own, a Rex floating pane over the Rex window. The completion
menu, unfocusable floats like notifications, and statuslines are drawn
inside the pane they belong to.

Without a pane provider, or where it can't give a window a terminal, the
window is drawn in the pane it was opened from, laid out as Neovim lays out
splits ([ADR 0009](docs/adr/0009-neorex-not-neovim-lays-out-the-windows-a-pane-shows.md)).
A tab page with no tab page area is drawn whole in the pane it opened in,
and `gt` swaps what that pane shows, as plain Neovim swaps its screen
([ADR 0012](docs/adr/0012-a-tab-page-gets-a-tab-page-area-from-open-tab-page-else-stays-whole-in-the-pane-it-opened-in.md)).
A float group with no float group pane, as with a pane provider that has no
`open_float`, past four float group panes, or when one fails to open, is
drawn in the pane it was last entered from; while it's current and too big
for that pane, Neovim's screen shrinks to the pane so the picker lays itself
out to fit
([ADR 0010](docs/adr/0010-a-float-group-with-no-float-group-pane-is-drawn-in-a-pane-on-a-screen-shrunk-to-fit-it.md)).

No pane provider is needed: in a plain terminal, `neopane` is Neovim drawn by
a daemon, which you can attach to from other terminals, leave running, and
open files in from its own `:terminal`.

![neopane demo](assets/demo.gif)

### Status

Very early and rough: expect bugs and missing features.

neopane has been built with heavy use of AI, first in Zig and now in Rust.
Much of the code still needs cleanup and better design, which is happening
incrementally. Contributions are welcome.

## Install

You need:

- macOS, or Linux: it builds and passes its tests there, but hasn't been
  tried live yet (#43);
- Neovim 0.12 or later, as `nvim` on `PATH`;
- Rust (stable, 2024 edition) to build it;
- [just](https://github.com/casey/just), or Cargo alone;
- [Rex](https://www.superlogical.com/rex), only for windows in Rex blocks of
  their own.

To install it, optimized, as `~/.local/bin/neopane`, with `npane` linked to
it as a shorthand:

```sh
just install
just prefix=/usr/local install
```

It replaces the binary rather than writing over it, which on macOS gets a
new build killed at launch. A running daemon keeps the binary it started
with, so start a new one to use a new build. `just install` removes the
`nrex` symlink an install before the rename made, and warns about a `neorex`
binary beside it without removing it, since that may be the original neorex.

## Using it

### Start

```sh
neopane notes.md
neopane -O a.c b.c
```

`neopane [OPTIONS] [NVIM-ARGS...]` starts a daemon, which runs one Neovim
with your Neovim arguments (see [Neovim arguments](#neovim-arguments)), and
shows it in this terminal. Neovim starts once the daemon has accepted this
terminal's attach, the first attach, at this terminal's size, and the
terminal shows its current window. Nothing is printed on success. When
Neovim quits, `neopane` exits with Neovim's status: `:q` 0, `:cq` 1, `:cq 3`
3, so `:cq` aborts a `git commit` as it does in plain Neovim.

That first attach chooses the daemon's pane provider, which gives windows
terminals of their own (see [Pane providers](#pane-providers)), without
holding anything up: the first window shows at once, and the other windows
of the current tab page, such as `-O a b`'s second, wait for the choice and
then open beside the first. The other tab pages opened at the start, such as
`-p a b`'s second, each get their place when first visited, as one opened
later does.

Each daemon has a daemon name: the name of the directory `neopane` was
started in, cut to its letters, digits, `.`, `_` and `-`, and to 32
characters (`default` if nothing is left), with `-2`, `-3`, … added while a
daemon of that name runs. `ps` shows it as `neopane daemon work`.

If stdin isn't a terminal, `neopane` prints `neopane: stdin isn't a terminal;
to start a daemon without one, use --detach` and exits 1.

A failed start is one line on stderr and exit 1, such as `neopane: work
didn't start: Neovim exited at once (status 1): Unknown option argument:
"--fo" (log: /tmp/neopane-501/work/log)`. Unless a shell started `neopane`
as a job, as when it is a Rex block's own command, it adds ` (press a key)`
and waits, so the line can be read.

### From another terminal

Any other terminal can show the same Neovim: run `neopane attach` there. It
attaches to the one daemon running; with several, name one with `--name`, or
give its daemon address, `--address /tmp/neopane-<uid>/<daemon name>/sock`
(under `$NEOPANE_TMPDIR` if set). It shows an unshown window, Neovim's
current one first, then by tab page and window order, so after `neopane -p a
b c` a second terminal shows `b`; with none, a new window, as `:split` opens
one. The window it shows becomes current.

`neopane attach notes.md` opens `notes.md` there in a new window instead, and
`neopane attach +12 notes.md` puts the cursor on line 12. A relative path is
taken from the directory `attach` runs in. `attach` takes only `[+LINE]
[FILE]`; anything else is a usage error, `neopane: attach takes [+LINE]
[FILE]`.

Closing that terminal closes its windows, and only those; the terminal goes
back to its shell. With no pane provider yet, the attach chooses one, as the
first did. In the pane provider's reach, as in a Rex block of the chosen Rex
session, `:vsplit` there opens a pane beside it; out of its reach, `:vsplit`
draws the new window in that terminal.

`attach` finds its daemon by `--address`, then `--name`, then `$NEOPANE`
once proven (see [Inside a `:terminal`](#inside-a-terminal)), then the only
one running. It never starts one. When it finds none, it says why in one line
and exits 1:

- `neopane: no daemon is running; start one with neopane [FILE]`
- `neopane: 3 daemons are running (notes, scratch, work); choose one with
  --name`
- `neopane: no daemon named work is running (running: notes, scratch)`
- `neopane: no daemon answers at /tmp/neopane-501/work/sock`

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

`neopane list` lists the daemons running, `neopane status` shows one, and
`neopane kill` ends one, refusing while it has unsaved changes unless
`--force` (see [list, status and kill](#list-status-and-kill)).

### When the last pane goes

A daemon lives while a terminal shows one of its windows, or one is on its
way
([ADR 0003](docs/adr/0003-a-daemon-outlives-its-last-pane-only-while-neovim-has-unsaved-changes.md)).
Panes that show only floats, such as a picker's, don't count. When the last
one goes, whatever closed it, the daemon sends Neovim `FocusLost`, so an
autosave plugin can save, and waits for Neovim to be idle, a second at most.
Then:

- With nothing unsaved, it ends Neovim by closing its channel: `VimLeavePre`
  and `VimLeave` run, and swap files go.
- With unsaved changes, buffers `:qa` would refuse to quit over, it stays,
  an unattached daemon, logs their names, and lists the files among them in
  `unsaved` in its directory (see [Files and logs](#files-and-logs)). It
  keeps one window, the last current one, and closes the rest. `neopane
  attach --name work` brings that window back, with your changes; `neopane
  attach --name work FILE` opens FILE and closes the kept window. Saving or
  throwing away the last change ends it, and so do `neopane kill --force` and
  a signal, keeping the changes in swap files.

A terminal that attaches while the daemon waits keeps it. One that attaches
once Neovim is ending is refused with `stopping`. `:q` in the last window
still quits Neovim, as `:q` always does.

### The unsaved-file question

When `neopane [+LINE] [--] FILE` is about to start a daemon (`--name N`
included, when `N` isn't running), from a terminal (stdin and stderr), and
without `--detach`, it first looks for an unattached daemon holding unsaved
changes to FILE: it reads the `unsaved` file of each running daemon, and
matches FILE as the system opens it, links and `..` resolved in the part of
the path that exists, the rest normalised as written. The first such daemon
by name is asked about, on stderr:

```text
neopane: notes.md has unsaved changes in notes (also in notes-2), which no terminal shows. Attach to it? [Y/n]
```

` (also in …)` names the other daemons holding it, if any. Answer `y`, `Y`
or Enter to run `neopane attach --name notes [+LINE] FILE`; `n` or `N` to
start the new daemon as asked; Ctrl-D or Ctrl-C to exit 1 with nothing
started. Anything else asks again. It is asked under `EDITOR=neopane` too.

### Inside a `:terminal`

A daemon sets `$NEOPANE` to its address in its Neovim's environment, so its
`:terminal`s have it. There, `neopane notes.md` opens `notes.md` in a new
window of that Neovim, split off the window showing that `:terminal` (else
the current one), so a pane beside the terminal's in Rex, and returns once
you're done with it: a waited file
([ADR 0015](docs/adr/0015-a-file-opened-from-a-daemons-own-terminal-is-waited-for.md)).
It is done when no window shows it any more (`:wq`, `:q`, `ZZ`, `:bd`, or
its window showing another buffer), or its buffer is unloaded or deleted.
`:w` alone isn't done. It prints nothing and exits 0, with no time limit.
`neopane +12 notes.md` puts the cursor on line 12, and `--` may come before
the file. Naming that daemon with `attach`, `--name` or `--address` does the
same.

`--no-wait` returns as soon as Neovim has the window, or exits 1 after 10 s
with `neopane: work hasn't opened notes.md in 10 s (is Neovim waiting at a
prompt?)`. Outside a daemon's `:terminal`, `--no-wait` does nothing.

It exits 1, with one line, when:

- `neopane: notes.md's terminal closed with unsaved changes`: the file's last
  window went because its pane's terminal closed while it had unsaved
  changes;
- `neopane: work was stopped before notes.md was done`: `neopane kill`;
- `neopane: work ended before notes.md was done`: the daemon went away, as
  when its Neovim quits;
- `neopane: work can't wait for a file (an older build); not waiting`;
- the file couldn't be opened, as after Quit at Neovim's swap file prompt
  (E325): the daemon's line.

Ctrl-C exits 1 quietly. Only `[+LINE] FILE` opens there: bare `neopane`
exits 1 with `neopane: already inside work's Neovim; open a file with neopane
FILE`, and `-p`, `-O`, `-S`, `+cmd` or several files exit 2 with `neopane:
inside work's Neovim, only [+LINE] FILE can be opened; use :tabedit, :vsplit
or :source there`. `--detach` always starts a new kept daemon, even there.

Inside needs proof: `$NEOPANE` names a running daemon of this attach protocol
version, and this process's terminal (the first of stdin, stdout and stderr
that is one) is one of its Neovim's `:terminal`s; a process with no terminal
is trusted on the address alone. A tmux server or terminal app started from a
`:terminal` copies `$NEOPANE` into terminals that aren't Neovim's. Unproven,
`$NEOPANE` is ignored, `neopane` acts as anywhere else, and it warns:

- `neopane: ignoring $NEOPANE: this terminal isn't one of work's (unset
  NEOPANE to stop this warning)`
- `neopane: ignoring $NEOPANE: work runs another version, so this terminal
  can't be proven to be its own`
- `neopane: ignoring $NEOPANE: nothing answers at …`

### `$EDITOR`

`EDITOR=neopane`, with no flag, works everywhere. In a daemon's `:terminal`,
the file is a waited file, as above, so `git commit` gets its message once
you `:wq`. Elsewhere, `neopane FILE` starts a daemon for the file and returns
when its Neovim quits, with Neovim's status, as `nvim` does: `:cq` aborts
`git commit` and `git rebase -i`. In a daemon's `:terminal`, `:cq` quits that
whole Neovim, as plain Neovim would; to abort one commit, leave its message
empty. Callers that add `+N` (less) or `--` (visudo, sudoedit) before the
file work too.

To send edits to a running daemon from outside it, set `EDITOR="neopane
--name work"`: it attaches, and returns when that pane ends. The value has a
space, so it needs a caller that splits it, as git does; macOS `crontab -e`
doesn't.

### `--detach`

`neopane --detach [--name N] [NVIM-ARGS]` starts a kept daemon and needs no
terminal. Neovim starts at once, with NVIM-ARGS, and `neopane` prints only the
daemon's name (`work-2`) on stdout once Neovim answers; it doesn't wait for
`VimEnter`. `neopane attach` then shows it: the first attach shows Neovim's
current tab page, and `neopane attach FILE` opens FILE beside the windows
NVIM-ARGS opened. Started on no file, the empty window Neovim starts with
closes once FILE shows, as the window a last pane leaves does.

A kept daemon stays kept for its whole life: it outlives its last pane with
nothing unsaved, so `list` shows `kept` after its panes and `status` says it
stays `until killed, started with --detach`. It ends only when Neovim quits,
on `neopane kill`, on a signal, or on a crash. While it has no pane, it lists
its unsaved files in `unsaved`, as above.

A failed start is one line on stderr, `neopane: work didn't start: <why>
(log: …)`, and exit 1, with no key wait. `--detach --name N` with `N`
running exits 1 with `neopane: N is already running; attach with neopane
attach --name N`. With `--address` or `--provider` it exits 2: `--provider`
is for the terminal that chooses the pane provider.

## Pane providers

A pane provider gives Neovim's windows terminals of their own, and places
and focuses them: a terminal provider, a layout provider, or both. A daemon
has at most one at a time, chosen by a self-attach, as `neopane` and
`neopane attach` are: by your config, or without one, tmux in a tmux pane,
Rex in a Rex block and none elsewhere.

The daemon keeps its pane provider while a terminal in its reach is attached
or on its way, and drops it otherwise, as when the last of them closes; one
that fails as a whole, as Rex does when it quits, is dropped at once, with a
warning in Neovim. New windows are then drawn in the terminals they were
opened from, and the next self-attach chooses again: the terminals attached
before are checked again, and those in the new pane provider's reach rejoin
it.

`--provider P` on `neopane` or `neopane attach`, else `$NEOPANE_PROVIDER`, is
a hint for that choice: `none` chooses none, even in Rex, without running
your config; any other value reaches your config as `attach.provider`. It
counts only when this attach chooses the pane provider; otherwise the daemon
logs `hint P ignored: work keeps rex`, and shows nothing.

### None needed

With no pane provider, or when the choice fails, every window is drawn in
the terminal it was opened from, laid out as Neovim lays out splits, tab
pages swap in the pane they opened in, and float groups are drawn in the
pane they were entered from. Each terminal attached is a layout area of its
own, and Neovim's screen is the current pane's size.

### Rex

Rex's pane provider is built in. An attach chooses Rex when `REX_SERVER` and
`REX_SESSION` are set in its environment, as Rex sets them in every Rex block: the
daemon connects to that Rex server and Rex session, as the client `neopane`, and
each window gets a Rex block of its own beside the one it was opened from,
labelled `neopane`. Rex checks the choice: the terminal must be the Rex block
`REX_BLOCK` names, its terminal compared with the terminal of that block's
process. The choice fails, with a warning in Neovim, in a terminal started
from a Rex shell, such as a Ghostty window, which inherited stale `REX_*`, or
one whose `REX_BLOCK` is unset or names another Rex block. A later attach is in
Rex's reach in a Rex block of the same server and Rex session, found by its
`REX_BLOCK`. The variables, read from the attach's environment, never the
daemon's own:

- `REX_SERVER`: the URL of the Rex server to connect to.
- `REX_SESSION`: the Rex session to open Rex blocks in.
- `REX_BLOCK`: the id of the Rex block the attach runs in, which Rex checks.

Rex's connection lasts as long as Rex is the daemon's pane provider: it
closes when no Rex block is left attached, or when Rex quits or fails, and a
new one opens when Rex is chosen again, its Rex blocks rejoining.

Neovim's screen is the size of the current pane's layout area, its Rex
window
([ADR 0008](docs/adr/0008-neovims-screen-is-the-size-of-the-current-panes-layout-area.md)).
A tab page's windows in its Rex floating pane are placed as Neovim's layout
has them, at about their sizes, so Diffview's file panel is on the left, and
follow the layout as windows come and go; a window Neovim resizes there,
such as with `<C-w>|`, resizes its Rex block.

`neopane provider check`, in a Rex block, checks Rex's connection: it shows
the Rex server, its version and API revision, whether this build needs a
newer one, and whether Rex proves this terminal is the Rex block `REX_BLOCK`
names ([docs/config.md](docs/config.md#provider-check)).

To try a build without it taking the focus in your own Rex session, which
neopane does as it works, run it in a Rex block of a Rex session of your
own, made with `rex new`.

### tmux

tmux's pane provider is built in, as a table of capabilities that each run
neopane itself
([ADR 0016](docs/adr/0016-built-in-pane-providers-other-than-rex-are-tables-that-run-neopane-itself.md)).
An attach chooses it when `TERM_PROGRAM` is `tmux` and `TMUX` is set, as in
every tmux pane: each window gets a tmux pane of its own, split beside the
one it was opened from, and each tab page a tmux window of its own;
`<C-w>h/j/k/l` move tmux's focus across the panes. A group of floats you
enter, such as a picker, gets a tmux floating pane, framed, on tmux 3.7 or
newer; older ones draw it in its pane. On tmux 3.8 or newer, a move between
panes with tmux's own keys moves Neovim's current window at once, through
`wait-for -E` waiters that attach no client; older ones learn it at the
next key. tmux checks the choice: the terminal's tty must be one of that
tmux server's panes', so a `TMUX` inherited by another terminal chooses
none, which only the log tells. In a tmux started from a Rex pane, whose
panes inherit Rex's variables too, the terminal's tty tells them apart
([docs/config.md](docs/config.md#the-default)). It needs tmux 3.2 or newer.
`neopane provider show tmux` prints its table, which `neopane.builtin.tmux()`
returns for your config to change ([docs/config.md](docs/config.md#tmux));
`--provider none` opts out.

### Your own

Your own Lua file, `~/.config/neopane/init.lua`, chooses the pane provider
for each self-attach: a built-in, a table of capabilities, each a command
neopane runs, or a pair of a terminal provider and a layout provider, such as
Ghostty with AeroSpace
([ADR 0007](docs/adr/0007-the-users-own-lua-chooses-a-daemons-one-pane-provider.md)).
[docs/config.md](docs/config.md) describes the file, and
[docs/capabilities.md](docs/capabilities.md) the contract of each capability's
command.

- `neopane provider check` runs your config as a terminal attaching here
  would, and reports what it chose, then runs its `find`
  ([docs/config.md](docs/config.md#provider-check)). `--config FILE` names
  the file and `--provider P` gives the hint; `--daemon-name N` sets
  `attach.daemon`; `--no-find` skips `find`; `--full` shows lines it would
  cut; `--json` prints one JSON object. It exits 0 for a pane provider or
  none by design (with no config, also a `find` refusing the default's
  guess), 1 for an error or a `find` refusing the config's choice, 2 for a
  usage error.
- `neopane provider show [NAME]` prints a built-in pane provider as Lua, to
  copy and change ([docs/config.md](docs/config.md#built-in-pane-providers)).
  Today the only built-in is `rex`, which has no Lua table.
- `neopane provider types [--config FILE]` writes Lua types for editing your
  config with lua-language-server
  ([docs/config.md](docs/config.md#editor-setup)).

A config that fails shows one warning in Neovim, `neopane: config: …`, and
`none (config error)` in `neopane list`; the log has every problem
([docs/config.md](docs/config.md#errors)).

## Command reference

`neopane --help` (also `-h`, `help`, or `-h` after any command) prints, on
stdout:

```text
neopane: one Neovim in the background, shown in every terminal attached to it.

Usage:
  neopane [OPTIONS] [NVIM-ARGS...]
        Start a daemon and show its Neovim here. NVIM-ARGS go to Neovim, as in
        neopane -O a.c b.c. In a daemon's own :terminal, neopane FILE opens
        FILE in a new window there and returns once you're done with it.
  neopane attach [--name N | --address A] [--provider P] [+LINE] [FILE]
        Show a running daemon here, with FILE in a new window.
  neopane list [--json]
        List running daemons.
  neopane status [--name N | --address A] [--json]
        Show one daemon: panes, unsaved files, pane provider, log.
  neopane kill [--name N | --address A | --all] [--force]
        End a daemon. Refused while it has unsaved changes, unless --force,
        which leaves them in swap files.
  neopane provider check [--config FILE] [--provider P] [--daemon-name N]
                         [--no-find] [--full] [--json]
        Run your config as a terminal here would, and report what it chose.
  neopane provider show [NAME]
        Print a built-in pane provider as Lua.
  neopane provider types [--config FILE]
        Write Lua types for editing your config.

Options (before NVIM-ARGS):
  --name N       Use the daemon named N: attach if it runs, else start it.
                 Without it, a new daemon named after this directory starts.
  --address A    Attach to the daemon at A.
  --provider P   A hint for your config when this terminal chooses the pane
                 provider; none: no pane provider.
  --detach       Start the daemon without showing it, and print its name.
  --no-wait      In a daemon's :terminal, open FILE and return once Neovim
                 has its window (else exit 1 after 10 s).
  -h, --help     Show this help.
  -v, --version  Show the version.

A running daemon takes only [+LINE] FILE. To open a file named like a
command, write ./list. To edit with it, set EDITOR=neopane.

Environment:
  NEOPANE_LOG              how much daemons log: warn, info (default), debug
  NEOPANE_TMPDIR           where daemons keep their files, instead of /tmp
  REX_SERVER, REX_SESSION  set in a Rex pane: Rex gives each window a pane
  REX_BLOCK                the Rex block a terminal runs in, as Rex checks

Each daemon keeps its files in $NEOPANE_TMPDIR, else /tmp, under
/neopane-<uid>/<daemon name>/, and logs to `log` there. The log stays after
the daemon ends; the next daemon of that name keeps it as log.1.
More: https://github.com/ian-pascoe/neopane  Developer tools: neopane help debug
```

### Own options first, then Neovim's

`neopane` reads its own options only from the front of the command line
([ADR 0014](docs/adr/0014-own-options-come-first-and-command-words-never-name-files.md)).
Long ones take ` VALUE` or `=VALUE`, and none can be abbreviated; `-h` and
`-v` are the only short ones. The first word that isn't one of them, and
every word after it, goes to Neovim as typed, a `--` included: `neopane --
--name` opens a file named `--name`. One of `neopane`'s options after
Neovim's have begun is a usage error saying where it goes: `neopane: put
--name before Neovim's arguments: neopane --name N [NVIM-ARGS]`.

Only the first argument can be a command word (`attach`, `list`, `status`,
`kill`, `provider`, `help`, `version`), and it runs the command even when a
file of that name exists, with a note on stderr, `neopane: list is a
command; to open the file, run neopane ./list`. `detach` is held for a later
command: it exits 2, `neopane: detach isn't available yet; to open a file
named detach, run neopane ./detach`.

### list, status and kill

`neopane list [--json]` asks every running daemon at once, giving each 2 s,
and prints a row per daemon, sorted by name, under a header that is always
printed. Directories whose daemon is gone are skipped. With none running,
stderr also gets `neopane: no daemon is running`, and it exits 0.

```text
NAME     PANES   UNSAVED  PROVIDER  DIRECTORY
notes    0       2        none      ~/notes
scratch  0 kept  0        none      /tmp/scratch
work     3       0        rex       ~/src/neorex
old      -       -        -         (neopane 0.0.9, attach protocol 0)
stuck    -       -        -         (didn't answer in 2 s)
```

- PANES: the Neovim panes attached, window and float group panes alike,
  with `kept` after it for a kept daemon.
- UNSAVED: how many buffers have unsaved changes.
- PROVIDER: the pane provider (a pair as `T+L`), `(choosing)` while your
  config runs or the choice is being confirmed, or `none`,
  `none (config error)`, `none (find refused)` or `none (hint none)`.
- DIRECTORY: Neovim's global working directory, `~` standing for `$HOME`.
- A daemon of another attach protocol version shows its build and version
  instead; one that didn't answer, why.

`neopane status [--name N | --address A] [--json]` finds its daemon as
`attach` does, with the same messages, and prints it:

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
  build      neopane 0.0.0 (a1b2c3d, 2026-11-09), attach protocol 1
  log        /tmp/neopane-501/work/log
```

Unsaved files are shown relative to the directory. `stays` reads `until
killed, started with --detach` for a kept daemon. `found unsupported` lists
the capabilities found unsupported since the pane provider was chosen. A
daemon of another version shows only its name, `pid` and `build`, then
`(another version: only its name, pid and build are known)`. A daemon that
doesn't answer in 2 s is a failure: `neopane: work didn't answer in 2 s (log:
…)`, exit 1. Neither `list` nor `status` waits on Neovim: the daemon answers
from what it tracks.

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

### JSON

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
U+FFFD in place of what isn't. With none running, `list --json` prints
`{"daemons":[]}`.

The JSON is stable: fields are only ever added, never removed, renamed, or
changed in type or meaning. Read it ignoring fields you don't know, and take
a missing field as unknown, as for a daemon of another version. `provider
check --json` makes the same promise; its shape is in
[docs/config.md](docs/config.md#--json).

### Version

`neopane -v` (also `--version`, `version`) prints the build, then the
protocols it speaks:

```text
neopane 0.0.0 (a1b2c3d, 2026-11-09)
attach protocol 1
pane provider protocols 1
```

The first line is the build string a helper's hello sends
([docs/attach-protocol.md](docs/attach-protocol.md#the-build-string)): `-dirty`
follows the commit when tracked files differ from it, and a build from
outside git has no parentheses.

### Output and exit statuses

What was asked for (help, version, `list`, `status`, `provider check`'s
report, `provider show`'s Lua, `provider types`' lines, `--detach`'s name)
goes to stdout; everything else goes to stderr as one `neopane: …` line. A
usage error is one line ending `(see neopane --help)`. Output is the same to
a terminal or a pipe, with no colour, but for the unsaved-file question, the
key wait after a failed start, and `provider check`'s pane title. A reader that closes the pipe early
ends `neopane` quietly.

| Status | Meaning |
|---|---|
| 0 | success |
| 1 | a failure or refusal |
| 2 | a usage error, or a refused Neovim argument |
| Neovim's | `neopane` or `neopane attach`, once Neovim quits: `:q` 0, `:cq` 1, `:cq N` N |

## Neovim arguments

Everything after `neopane`'s own options goes to Neovim as typed, read by
Neovim's own rules, to refuse what can't work under a daemon, to find the
file for the unsaved-file question, and to hold a running daemon to
`[+LINE] FILE`. A relative path is taken from the directory `neopane` was
started in, as with `nvim`. Anything Neovim itself rejects fails the start
with Neovim's message.

### Refused

These can't work under a daemon, so `neopane` refuses them with exit 2,
saying why, and starts nothing:

| Word | Why |
|---|---|
| `--embed`, `--headless`, `--listen…` | the daemon starts Neovim with them itself |
| `--server…`, `--remote…` | they talk to another Neovim; open a file in a running daemon with `neopane attach --name N FILE` |
| `-h`, `-v`, `--help`, `--version` after other arguments; `--api-info`, `-?`, and `h` or `v` in a cluster such as `-Rh`, anywhere | Neovim would print and exit; put `-h` or `-v` first for `neopane`'s own |
| `-l`, `-es`, `-Es` | they run Neovim without a screen |
| `-V`, or `-V2` and up, without a file | Neovim wouldn't start; give it one, as `-V3log.txt` |
| `-` | the daemon's Neovim can't read stdin; save it to a file first |
| `-r` or `-L` with no file | it only lists swap files; run `nvim -r` |

### Accepted, with care

- `-u`, `--clean`, `--noplugin`, `-i`: work, but the daemon's own `--cmd
  "let g:neopane = 1"` and its window script still run first.
- `-o`, `-O`, `-p`, `-d`, `-S`: windows and tab pages open as at any start:
  the current tab page's other windows get terminals of their own once the
  pane provider is chosen, and every other tab page gets its place when
  first visited.
- `+cmd`, `-c`, `--cmd`: a command that quits Neovim ends the first attach
  with Neovim's status. The daemon uses one of Neovim's ten `--cmd` slots,
  leaving nine.
- `-n`: no swap files, so `kill --force` loses unsaved changes.
- `-e`, `-E`: start in Ex mode, as `nvim -e` does.

## Environment

| Variable | Read by | Meaning |
|---|---|---|
| `NEOPANE` | `neopane` in a `:terminal` | set by a daemon in its Neovim's environment to its address; see [Inside a `:terminal`](#inside-a-terminal). Never passed on to pane provider commands |
| `NEOPANE_CONFIG` | the daemon, as it starts; `provider check`, `provider types` | your config, in place of `~/.config/neopane/init.lua` |
| `NEOPANE_PROVIDER` | each self-attach; `provider check` | the default for `--provider` |
| `NEOPANE_TMPDIR` | `neopane`; the daemon | where daemons keep their files, instead of `/tmp` |
| `NEOPANE_LOG` | the daemon, as it starts | how much it logs |
| `NEOPANE_TRANSPORT` | every helper; the daemon, as it starts | how a terminal reaches the daemon, for diagnosis |
| `REX_SERVER`, `REX_SESSION`, `REX_BLOCK` | Rex's pane provider | see [Rex](#rex) |

- `NEOPANE_CONFIG` is read once, as the daemon starts, made absolute against
  the directory it started in; a later attach's never counts. For `provider
  check` and `provider types`, it is read from the shell, made absolute
  against its directory. Empty counts as unset, and `~` isn't expanded. A
  file it names that isn't there is an error. Without it,
  `$XDG_CONFIG_HOME/neopane/init.lua`, else `~/.config/neopane/init.lua`, on
  macOS too.
- `NEOPANE_PROVIDER` is read from the attach's environment, never the
  daemon's. `--provider P` wins over it. For `provider check`, `""` means
  no hint; on `neopane` and `neopane attach`, an empty `--provider` falls
  back to `$NEOPANE_PROVIDER`.
- `NEOPANE_TMPDIR`: a relative path is taken from the directory `neopane`
  runs in.
- `NEOPANE_LOG`: `warn`: what went wrong; `info`, the default: that, and
  what happened; `debug`: that, and the trace timestamps
  [docs/latency.md](docs/latency.md) reads. Any other value logs a warning,
  and `info` is used.
- `NEOPANE_TRANSPORT`: `fd`, the default (unset or empty too): the helper
  hands the terminal over to the daemon. `relay`: the helper copies its bytes
  to and from the daemon instead, as it must on Windows
  ([docs/attach-protocol.md](docs/attach-protocol.md#transports)). Every
  helper reads it, and a daemon started under `relay` asks for its panes with
  `--transport relay` added to their attach command, which overrides the
  variable. Any other value prints `neopane: ignoring
  NEOPANE_TRANSPORT=<value> (use fd or relay)`, and `fd` is used. It is in
  this README, not in `--help`.

Pane provider commands get the choosing attach's environment, less
`NEOPANE`, every per-act `NEOPANE_*` variable and every `NEOPANE_ENV_*`,
with their own variables of
[docs/capabilities.md](docs/capabilities.md#variables) set
([Running a command](docs/capabilities.md#running-a-command)). `XDG_CONFIG_HOME`,
`HOME` and `PATH` are read too.

## Files and logs

Each daemon keeps its files in `$NEOPANE_TMPDIR/neopane-<uid>/<daemon
name>/`, `/tmp` standing in for an unset `NEOPANE_TMPDIR`
([ADR 0004](docs/adr/0004-daemons-are-found-by-name-in-a-per-user-directory-under-tmp.md)):

| File | |
|---|---|
| `sock` | the daemon address, for attaches and control requests ([docs/attach-protocol.md](docs/attach-protocol.md)) |
| `nvim.sock` | Neovim's `--listen` address |
| `log` | the daemon's log, with Neovim's own messages |
| `log.1` | the log of the daemon of that name before |
| `unsaved` | while the daemon is unattached with unsaved changes: a first line `unsaved 1`, then each file's absolute path on a line of its own |

The directory `neopane-<uid>` must be yours, a real directory, and mode
0700, or `neopane` refuses to start and names `NEOPANE_TMPDIR`. The daemon
holds a lock on its own directory while it runs, which is how it's known to
be running. Its log stays after it ends, however it ends; the next daemon of
that name keeps it as `log.1`, replacing an older one. An unnamed buffer, or
one a plugin names `oil://…`, is logged but not listed in `unsaved`.

Your config lives in `~/.config/neopane/` (see [Your own](#your-own)).
Leftover `neorex-<uid>` directories from before the rename are left alone.

## Troubleshooting

- Windows open in the terminal you ran `neopane` in, not in Rex blocks: the
  daemon chose no pane provider. With a warning in Neovim (`:messages`), Rex
  couldn't be reached or attached, or didn't confirm the terminal is the
  Rex block `REX_BLOCK` names, as for a terminal started from a Rex shell;
  without one, `REX_SERVER` or `REX_SESSION` is unset, or `NEOPANE_PROVIDER`
  is `none`. `neopane provider check` runs the same choice here and says
  which; so does the log.
- `neopane: config: …` in Neovim and `none (config error)` in `neopane list`:
  your config failed or returned something malformed. `neopane provider
  check` shows every problem
  ([docs/config.md](docs/config.md#errors)).
- `neopane: work didn't start: <why> (log: <path>)`: read the log at that
  path. A daemon that failed before it had a log says only why, such as
  `neopane: /tmp/neopane-501 isn't a private directory (owner 0, mode 0755);
  set NEOPANE_TMPDIR to another place` or `neopane: the socket path for work
  would be 109 bytes, over the 103 allowed; use a shorter --name or
  NEOPANE_TMPDIR`.
- `neopane: the daemon didn't start: <why> (logs: <base>)`: the daemon
  ended before it said anything; its log, if it made one, is under
  `<base>`.
- `neopane: ignoring $NEOPANE: …`: a terminal started from a daemon's
  `:terminal`, such as a tmux server's, inherited `$NEOPANE`. Unset it there.
- A daemon you can't see: `neopane list` shows every daemon running, with
  its unsaved buffers; `neopane attach --name N` shows one, and `neopane
  kill --name N` ends one.
- A daemon's log is `/tmp/neopane-<uid>/<daemon name>/log` (under
  `$NEOPANE_TMPDIR` if set), and the one before it `log.1`. `neopane status`
  prints its path. `NEOPANE_LOG=debug` makes it say more.

## Differences from plain Neovim

### `g:neopane`

Neovim starts with `g:neopane` set to `1`, before your Neovim config loads,
so it can tell it's running under neopane:

```lua
{ 'sphamba/smear-cursor.nvim', cond = vim.g.neopane == nil },
```

### Neovim's screen and the tabline

Neovim's screen is the size of the current pane's layout area: its Rex
window, or the pane alone with no live layout. Only its tabline, command
line, messages and screen floats show, in the current pane. The tabline
shows across the tabline panes along the top of each layout area, and is
fitted to the current pane's; the top panes of another layout area show it
from its start, cut at their width. When a foreign pane, such as a shell,
takes part of the top, the tabline panes are narrower than Neovim's screen,
and neopane fits the tabline to them: every `'tabline'` that isn't empty
ends at the right edge of the tabline panes (a fitted tabline), so the
`%=`-aligned right end of one such as lualine's shows. A tabline too wide
for them is cut as Neovim would cut it on a screen that wide. While a `%!`
`'tabline'` is drawn, `vim.o.columns` reads as the tabline width, so a tabline
whose Lua lays out to it, such as bufferline's, fits its own widths too.
Widths a tabline computes from `'columns'` any other way don't fit by
themselves, such as lualine's `max_length` or a Vimscript `&columns`; they can
read `g:neopane_tabline_width`, set to the tabline width while it's narrower
than the screen:

```lua
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

### Windows laid out in a pane

Windows drawn together in one pane are laid out by neopane, as Neovim lays
out splits: `'splitbelow'`, `'splitright'`, `:aboveleft`, `:belowright`,
`:vertical` and `'equalalways'` count, and `:resize`, `<C-w>+`, `<C-w>_` and
`<C-w>|` resize a window there. A window with another below it always has a
statusline, whatever `'laststatus'` says; at `'laststatus'` 0, neopane draws
it with the text and highlights Neovim would. Not yet: `<C-w>=`,
`'winfixheight'`, `'winfixwidth'`, `'winminheight'`, `'winminwidth'`, `<C-w>H`,
`J`, `K`, `L`, `r`, `R` and `x`, and dragging a separator or statusline.
Never: `:topleft` and `:botright`, which split beside the origin window, and
a split's count.

### Current limitations

- The `<C-w>` swap and move commands (`<C-w>H`, `J`, `K`, `L`, `r`, `R`,
  `x`) do nothing, and resizing a window does nothing to a pane of its own
  outside a tab page area: use the pane provider's own controls, such as
  Rex's. `<C-w>T` works: it moves the window to a new tab page, as `:tab
  split` then `:close` would, so the window is a new one, and it's refused
  for its home tab page's only window.
- `<C-w>h/j/k/l` (also `<C-w><C-h>`, `<C-w><BS>` and `<C-w><Left>` and the
  like, in Normal and Visual mode) move within a pane's windows first, then
  to the neighbouring pane, and stay put at an edge. A count repeats the
  move, up to 20 times. Mappings that remap them work, such as LazyVim's
  `<C-h>`; a `noremap` mapping to `<C-w>h` (kickstart.nvim's) skips them, and
  so do `:wincmd h` and `vim.cmd.wincmd` from a plugin: those step through
  the panes like `<C-w>w`. The move happens after the keys typed with it:
  `<C-w>lZZ` in a mapping or macro runs `ZZ` in the old window.
  - Rex servers whose `session.focus_direction` has no `wrap` option wrap
    around at an edge. Ones without the method do nothing for these keys,
    and log the refusal.
  - Rex looks at every pane in its Rex window, so a move can enter an open
    Rex floating pane, such as a tab page's, and leave it.
- Plugins that arrange windows (sidebars, layout managers) don't get the
  placement they expect, except in a tab page area. A window such a plugin
  moves back into Neovim's own layout goes back to its pane. edgy.nvim works
  without turning it off: neopane tells it to leave the windows in panes
  alone (`vim.w.edgy_disable`), so its edges don't dock anything. A plugin
  that keeps moving a window back as fast as neopane puts it in its pane (5
  times in 2 seconds, with nothing typed and no pane resized) wins for a
  while: that window stays in Neovim's layout, its pane stays blank, and you
  still type into it. neopane tries again after 2 seconds, then 4, up to a
  minute. Turn such a plugin off with `cond = vim.g.neopane == nil`.
- Neovim's tab pages aren't Rex tabs: a tab page gets a Rex floating pane
  and closes with its last window. A float group in a Rex floating pane
  shows over whichever tab page is current, and closes with the tab page it
  opened in: closing the tab page it shows over sends it back to its own
  first, but for the float you're in, which closes, as Neovim closes the
  current float first.
- Neovim's tab-page queries don't match neopane's tab pages (#67). Every
  window shown in a pane sits in Neovim's current tab page, so
  `tabpagebuflist()`, `tabpagewinnr()` and `nvim_tabpage_list_wins()` see all
  of them in the current tab page, along with a hidden window neopane keeps in
  each tab page, and see the other tab pages as holding only that hidden
  window. `winlayout()` sees only the hidden windows. Plugins that label tab
  pages from them, such as lualine's `tabs` component, show the wrong labels.
  The built-in tabline, while `'tabline'` is empty, shows the right ones:
  neopane draws it from its own record of each window's home tab page.
- Plugins that draw cursor effects with screen floats, such as
  smear-cursor.nvim, draw them in the wrong place: Neovim can't tell them
  where a pane's window is.
- No external command line or messages: long messages still show
  "Press ENTER".
- No global statusline: neopane treats `laststatus=3` as `2`, so each pane
  shows a statusline, the current window's: with lualine's
  `globalstatus = true`, the same line in every pane. Set `globalstatus`
  explicitly: lualine skips neopane's windows without it, and its default,
  `vim.go.laststatus == 3`, is false once neopane has turned `laststatus=3`
  into `2`. A pane's own `setlocal statusline` is replaced by that line
  meanwhile, and comes back when the pane takes focus, or when `laststatus`
  is set to another value.

## Debug commands, build and tests

`neopane help debug` lists the debug commands, which run each layer on its
own:

| Command | |
|---|---|
| `neopane debug-keys` | shows each key, paste, focus and mouse event as you type |
| `neopane debug-events [NVIM-ARGS...]` | runs Neovim and prints the redraw events it sends |
| `neopane debug-tui [NVIM-ARGS...]` | runs Neovim in this terminal through neopane's renderer |
| `neopane debug-panes SOCKET` | a stand-in daemon: drives the terminals of panes that connect |
| `neopane debug-rex SOCKET` | opens a split in this Rex session and drives its pane, printing Rex events; it reads `REX_SERVER`, `REX_SESSION` and `REX_BLOCK` from its own environment |

A requested pane's terminal runs the hidden `neopane attach --address A
--pane ID`, which hands its terminal to the daemon
([ADR 0013](docs/adr/0013-a-pane-attaches-with-a-framed-hello-handing-its-terminal-over-on-unix-or-relaying-it.md),
[docs/attach-protocol.md](docs/attach-protocol.md#the-attach-command)) and
waits until the daemon ends the attach. The helper, not the daemon, puts its
terminal in raw mode and back, so a daemon that crashes or is killed leaves
every terminal usable: out of raw mode and the alternate screen, with a line
naming the daemon's log. A pane id, such as `2-k3j9x2qpa8`, is good for that
one pane's terminal only.

To build and test, with [just](https://github.com/casey/just) (`just` lists
the recipes):

```sh
just build
just run some/file.txt   # target/debug/neopane, with Neovim arguments
just test
just golden   # also the golden tests; downloads and builds Ghostty once
just fmt
just lint
```

or with Cargo alone:

```sh
cargo build
cargo test
scripts/golden.sh --test golden
```

Tests that need Neovim skip when `nvim` isn't installed. The golden tests
check every frame neopane draws against
[Ghostty](https://github.com/ghostty-org/ghostty)'s terminal emulator,
pinned to one commit; they need Zig 0.17, and are behind the `golden`
feature, so a plain build never builds Ghostty.
[docs/latency.md](docs/latency.md) is how to check a change for latency.

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
| the repository `ian-pascoe/neorex` (its URLs redirect) | `ian-pascoe/neopane` |

A daemon started before the rename keeps running under its old name, and
leftover `neorex-<uid>` directories are left alone: remove them once no old
daemon runs. `just install` removes the old `nrex` symlink, and only warns
about a `neorex` binary, which may be the original's.

## Plans

Much of neopane will be split out into standalone crates:

- a Neovim RPC client (msgpack, message framing, requests)
- a Rex API client (WebSocket and the control protocol)
- terminal key input (the kitty keyboard protocol)

neopane should then be easier to extend to other editors that provide similar processes for running external windows.

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
