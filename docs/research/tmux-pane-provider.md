# tmux as a pane provider

Gathered for #78 (map #76) against tmux 3.8, the latest release tag
(`3.8`, commit `7f2a35ad`). Source was read at that tag and in the 3.8 release
tarball; the files cited are identical in both. Behaviour was checked by
building 3.8 from the tarball (macOS, Homebrew libevent 2.1.13,
`--disable-utf8proc --disable-jemalloc --disable-sixel`) and driving a private
server (`tmux -L neorex-research-78 -f /dev/null`). tmux is not installed
locally, so no user session was touched. Items marked *source only* were not
run.

Links: tmux.1 is `https://github.com/tmux/tmux/blob/3.8/tmux.1#L<line>`; source
is `https://github.com/tmux/tmux/blob/3.8/<file>#L<line>`; CHANGES is
`https://github.com/tmux/tmux/blob/3.8/CHANGES`. Below, `tmux.1:N` and
`file.c:N` refer to those.

## Creating panes and reading their ids

- `split-window -P -F FORMAT` prints the new pane through a format; the
  default is `session:window.pane` (`cmd-split-window.c:33`, `:306-310`).
  `-F '#{pane_id}'` gives `%N`. Runtime: `split-window -h -t t:0.0 -P -F
  '#{pane_id}' cmd` printed `%1`.
- Split direction and size: `-h` horizontal, `-v` vertical (default), `-b`
  places the new pane left of or above the target, `-l N` or `-l N%` sets the
  size (`tmux.1:4087`, split-window). Runtime: `split-window -b -l 10 -t %0`
  created a pane above `%0`.
- Floating pane: `new-pane -P -F FORMAT -x W -y H -X X -Y Y cmd` (new in 3.8,
  `tmux.1:3663`; `CHANGES:3`). Runtime: `new-pane -P -F '#{pane_id}' -x 40 -y
  10 -X 5 -Y 5` printed `%6` with `pane_floating_flag=1`.
- Modal pane: `new-pane -O` creates a floating pane that is always active and
  blocks the other panes; a window can have only one (`tmux.1:3663`,
  `CHANGES:10`). Runtime: a second `-O` failed with "window already has a modal
  pane" (`cmd-split-window.c:120`, `spawn.c:281`); `select-pane -t %0` left
  focus on the modal pane.
- Command exit: with default options the pane is removed when its command
  exits (runtime: `sh -c 'exit 3'` pane disappeared). With
  `remain-on-exit on` the pane stays with `pane_dead=1` and
  `pane_dead_status=4` (runtime; `tmux.1:6346`). `-k`/`-m` on split-window and
  new-pane keep a pane open until a key is pressed (`tmux.1:4087`).

## Focus

- `select-pane -t %N` focuses by id (`tmux.1:4019`).
- `select-pane -L|-R|-U|-D` moves to a neighbour (`tmux.1:4019`;
  `cmd-select-pane.c:224-240`). With no neighbour it does nothing
  (`cmd-select-pane.c:241`).
- Neighbour search is `window_pane_find_up/down/left/right` (`window.c:2220`,
  `:2281`, `:2342`, `:2394`). Candidates are the panes sharing the edge; ties go
  to the most recently active pane (`window_pane_choose_best`, `window.c:2172`).
- **It wraps at window edges.** `find_left` sets `edge = w->sx + 1` when the
  pane is at `xoff == 0` (`window.c:2359-2361`), so it matches the panes on the
  right edge. `find_right` sets `edge = 0` at the right edge
  (`window.c:2411-2412`), so it matches panes with `xoff == 0` (`window.c:2422`).
  `find_up` and `find_down` behave the same way vertically. Runtime, with three
  tiled panes (`%0` full height on the left, `%1` top right, `%2` bottom right):
  - `select-pane -L` from `%0` went to `%2`.
  - `select-pane -R` from `%2` went to `%0`.
  - `select-pane -D` from `%2` went to `%1`.
  - `select-pane -U` from `%1` went to `%2`.
  - `select-pane -L` from `%1` went to `%0` (no wrap needed).
- Target tokens `{left-of}` etc. use the same functions (`cmd-find.c:608-625`),
  so they wrap too.
- No-wrap is possible only by composing a guard: `if-shell -F -t %N
  '#{pane_at_left}' '' 'select-pane -L'`. `pane_at_left` is `xoff == 0`
  (`format.c:2222-2230`); `pane_at_right`, `pane_at_top`, `pane_at_bottom` are
  the other three (`format.c:2234` onward; `tmux.1:7423-7427`). Runtime: the
  guard kept focus on `%0` when it was at the left edge, and `select-pane -L`
  from `%2` still moved to `%0`.

## Resizing, moving, joining

- `resize-pane -t %N -x N|P% -y N` sets absolute size; `-L|-R|-U|-D n` adjusts
  by n. For floats, `-L|-R|-U|-D` target the borders and negative values are
  allowed (`tmux.1:3834`). Runtime: `resize-pane -t %0 -x 30` gave `pane_width=30`;
  `-R 2` grew a pane.
- `join-pane -s %src -t %dst [-h|-v] [-b] [-l size]` moves a pane into another
  pane's space (`tmux.1:3266`). `move-pane` does the same and, for floats,
  `-P position`, `-X/-Y` coordinates and `-z z-index` (`tmux.1:3473`). Runtime:
  `join-pane -s %5 -t %0 -h` gave `%0` width 30 and `%5` width 29.
- `kill-pane -t %N` (`tmux.1:3305`; the window is destroyed with its last pane,
  per the man page). Runtime: `kill-pane` on a float and on a tiled pane worked.

## Reading the layout

- `list-panes -t TARGET -F FORMAT` (`tmux.1:3395`). Useful variables
  (`tmux.1:7415-7490`): `pane_id`, `pane_left`, `pane_top`, `pane_width`,
  `pane_height` (cells), `pane_x`, `pane_y`, `pane_floating_flag`, `pane_z`,
  `pane_modal_flag`, `pane_active`, `pane_at_*`, `pane_tty`, `pane_pid`,
  `pane_dead`, `pane_dead_status`, `pane_start_command`. Runtime: all of these
  printed as expected.
- `window_layout` and `window_visible_layout` give a JSON layout that includes
  floating panes (`CHANGES:38`; `tmux.1:7555`). Control clients get the old
  format unless they set `new-layouts` (`CHANGES:38`).
- `pane_tty` gives the pty path (runtime: `/dev/ttys032`); `pane_pid` gives the
  first process.

## Windows (candidates for Neovim tab pages)

- `new-window -P -F '#{window_id} #{pane_id}'`, `select-window -t @N`,
  `kill-window -t @N`, `list-windows -F` (`tmux.1:3570`). Runtime: `new-window -P`
  printed `@1 %8`; `select-window -t @1` made it active; `kill-window` worked.

## Events

- Control mode: `tmux -C attach -t S` keeps a connection open. Commands come in
  on stdin and notifications go out on stdout (`tmux.1:9068`). Names used here:
  `%layout-change` (`tmux.1:9148`; template `control-notify.c:80-81`),
  `%window-pane-changed` (`tmux.1:9247`; `control-notify.c:123`),
  `%pane-mode-changed` (`tmux.1:9176`), `%subscription-changed` (`tmux.1:9207`),
  `%exit` (`tmux.1:9128`). The hooks forwarded as notifications are listed at
  `control-notify.c:362-366`. Runtime: a piped `tmux -C attach` printed
  `%layout-change` after a resize and `%window-pane-changed` after `select-pane`.
- Hooks: `set-hook -g NAME 'command'` runs a command on an event; the hook is
  stored in the server, so one `set-hook` makes a listener with no connection
  of the provider's own (`tmux.1:6421`, `set-hook` at `tmux.1:6436`).
  Events relevant here (`tmux.1:6520-6580`): `pane-focus-in` and
  `pane-focus-out` (need `focus-events on`; `tmux.1:6530`;
  `window.c:737-747`), `pane-exited`, `pane-died` (with `remain-on-exit`),
  `pane-resized`, `pane-moved`, `window-layout-changed`, `window-pane-changed`,
  `window-created`, `window-closed`. Runtime, with a control client attached so
  the window counted as focused: `pane-focus-in` ran per select, and
  `window-layout-changed` and `pane-resized` ran per resize. Without an attached
  client `pane-focus-in` never ran.
- Hook payload: `#{hook_pane}` is the pane where the hook ran (`tmux.1:7370`).
  Runtime: for `pane-exited` `#{hook_pane}` was the exiting pane. Using
  `#{pane_id}` in the same hook named the active pane instead.
- Monitors: `set-hook -B` and `refresh-client -B` check a format once a second
  and fire when it changes (`CHANGES:72-80`; `tmux.1:6436`).

## Popups

- `display-popup` draws a box over the panes of one client. It has no pane id
  (`tmux.1:8359`). Source: a call while a popup is already present edits that
  popup (`popup_present`, `popup.c:506-508`; `cmd-display-menu.c:581`); if another
  overlay is active it does nothing (`cmd-display-menu.c:596-597`); control
  clients get nothing (`cmd-display-menu.c:594-595`); it refuses if the client is
  smaller than the popup (`popup.c:590-591`). So one popup per client, not
  usable from control mode. *Source only*; no attached terminal client was used.

## Keyboard, focus, underline, synchronized output

- `extended-keys on|off|always` sets modifyOtherKeys reporting (`tmux.1:4913`);
  `extended-keys-format csi-u|xterm` picks the encoding (`tmux.1:4947`). The
  `extkeys` terminal feature sends `\E[>4;2m` (`tty-features.c:251-258`).
- Kitty keyboard protocol: not found in the 3.8 source (no `kitty` string). In
  `input.c:347` the only CSI `u` handler is restore-cursor (`INPUT_CSI_RCP`).
  So kitty-protocol passthrough is not supported. *Source only*.
- Focus events: `focus-events on|off` (`tmux.1:4964`, `options-table.c:433`).
  When the app asked for focus reporting, tmux writes `ESC [ I` / `ESC [ O` to
  the pane (`window.c:737-747`). *Source only* for the app side.
- Undercurl: a styled underline goes out via the `Smulx` capability; 3 is curly
  (`tmux.1:8999-9002`; `tty-features.c:156`). Output depends on the outer
  terminal having the feature (`usstyle`, default feature tables at
  `tty-features.c:587-600`). *Source only*.
- Synchronized output: `sync` terminal feature, `Sync=\E[?2026...`
  (`tty-features.c:245-251`; `tmux.1:5135`); tmux starts and ends sync around
  redraws when the outer terminal has it (`tty.c:1600-1625`). Can be turned off
  per terminal (`terminal-features`, e.g. `xterm*:sync@`, `tmux.1:5089`).
  *Source only*.

## Not confirmed

- `display-popup` on a real attached client (source only).
- Kitty keyboard, undercurl, synchronized output and focus-event pass-through
  with a real terminal such as Ghostty, WezTerm or kitty (source only).
- Floating panes in the directional search and in `pane_at_*`: `find_*` iterate
  all panes (`window.c:2366`), but geometry with floats was not run.
- Hooks across a server restart (hooks are server state, but not tested).
- Which tmux version added each 3.8 feature below 3.8 (not checked; `new-pane`
  and modal panes are listed in `CHANGES` under 3.8 only).

## What this means for the pane provider interface

- Yes, the required pane operations are one-shot `tmux` commands with ids read
  back: create (`split-window -P -F '#{pane_id}'`), close (`kill-pane`), focus by
  id (`select-pane -t`), resize (`resize-pane`), join and move (`join-pane`,
  `move-pane`), read layout (`list-panes -F`), windows for tab pages
  (`new-window -P`, `select-window`, `kill-window`), exit state (`pane_dead`).
- Directional focus wraps at window edges. A provider that promises no-wrap
  must compose `if-shell -F '#{pane_at_left}'` (or the other three) or check the
  layout itself before asking tmux to move. This is a quirk to design around,
  not a missing capability.
- Events are not a one-shot call. Two routes exist: a persistent `tmux -C`
  connection, or hooks installed once that each run a shell command per event.
  Either belongs in an optional capability. A provider with only one-shot
  commands can still work by polling `list-panes`.
- Floats, modal panes and popups are tmux 3.8 features (`new-pane`, `-O`), and
  `display-popup` is per client with no pane id. A tmux provider should declare
  floats only when it sees 3.8 or later, and treat popups as a different thing
  from panes.
- Keyboard protocol, undercurl, synchronized output and focus events are
  decided by the outer terminal and tmux's `extended-keys`, `focus-events` and
  `terminal-features` options. None of them needs a long-running process. Kitty
  keyboard passthrough is not in tmux 3.8.
- `pane_tty` gives a pane's pty path as a one-shot readout. That bears on the
  attach design (#91), which must avoid passing the fd, but tmux's own attach
  path was not investigated here.

Answer to the ticket's question: tmux can serve as a pane provider with
one-shot commands for create, close, focus, resize, move, layout and windows.
Two things need design: directional focus wraps, so no-wrap needs a guard, and
push events need a persistent control connection or per-event hooks, so they
should be optional.
