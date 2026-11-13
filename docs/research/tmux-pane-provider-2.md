# tmux as a pane provider: refresh

Follows `docs/research/tmux-pane-provider.md` (branch
`research/tmux-pane-provider`), which covered tmux 3.8. This note adds the
minimum tmux version of each thing a provider would use, control mode as an
events source, focus across windows and clients, `join-pane`/`resize-pane`
geometry, pane environment and nesting, and how `split-window` runs its
command. It is read against `docs/capabilities.md` and `docs/config.md`.

Sources: tmux's git repository at release tags (3.8 is commit `7f2a35ad`).
`CHANGES:N` and `tmux.1:N` are those files at tag `3.8`; `file.c:N` is at tag
`3.8` unless another tag is named as `file.c@3.2:N`. Links:
`https://github.com/tmux/tmux/blob/<tag>/<file>#L<line>`. Runs used the
installed tmux 3.8 (Homebrew) and a tmux 3.2a built in a temp dir from the
release tarball (`--disable-utf8proc`, Homebrew libevent), each on private
sockets (`tmux -L nprs2…`, `-f /dev/null`), all killed afterwards. A "user"
client was a real `tmux attach` running inside a pane of a second private
server, so focus reports could be sent to it with `send-keys -H`. Zellij
0.46.0 was downloaded into a temp dir for one nesting run. *Unconfirmed* marks
what was not run or read.

Correction to the earlier note: `new-pane` and floating panes arrived in
**3.7**, not 3.8 (`CHANGES:273-280`, "Floating panes are created with the
new-pane command"). 3.8 added moving/resizing floats by command, modal panes
and `-T`/`-B`/`-W` (`CHANGES:3-30`).

## Minimum versions

Found by `git grep` over the tags (the first tag where the source has it) and
by the CHANGES section that announces it.

| Feature | Since | Evidence |
|---|---|---|
| `split-window -P` | ≤1.5 | `cmd-split-window.c@1.5:35` (`"dl:hp:Pt:v"`) |
| `split-window -F FORMAT` | 1.7 | `cmd-split-window.c@1.7:35-36`, `:141` |
| `#{pane_id}`, `#{pane_tty}` formats | 1.6 | first tag where `format.c` has them; absent at 1.5 |
| `split-window -b` | 2.0 | `cmd-split-window.c@2.0:39` |
| `split-window`/`join-pane -l N%`, `resize-pane -x N%` | 3.1 | `CHANGES:1709-1711` (3.0a→3.1) |
| `join-pane -f` | 3.1 | `CHANGES:1697` |
| `join-pane -l`/`-p` | 1.2 | `cmd-join-pane.c@1.2:47` |
| `join-pane -b`, `move-pane` | 1.7 | `CHANGES:2731-2732` (1.6→1.7) |
| `resize-pane -x/-y` | 1.8 | `CHANGES:2695`; `cmd-resize-pane.c@1.8:34` |
| `list-panes -s` | 1.5 | `cmd-list-panes.c@1.5:38` (`"ast:"`) |
| `list-panes -f FILTER` | 3.2 | `CHANGES:1509` (3.1c→3.2) |
| `pane_at_left/right/top/bottom` | 2.6 | `CHANGES:2144` (2.5→2.6) |
| `#{e|/|f|N:a,b}` arithmetic (with float) | 3.2 | `format.c@3.2:3923` (`use_fp`); no `use_fp`/expression code at 3.1. Not in CHANGES |
| `TERM_PROGRAM=tmux`, `TERM_PROGRAM_VERSION` in panes | 3.2 | `CHANGES:1432`; `environ.c@3.2:260-261` |
| control clients ignored for size until `refresh-client -C` | 2.6 | `CHANGES:2217-2219` |
| `refresh-client -F no-output` | 3.0 | `CHANGES:1794-1795` |
| `refresh-client -f`, `attach -f`, `ignore-size`, `read-only` as flags | 3.2 | `CHANGES:1418-1428` |
| `pause-after`, `refresh-client -B` subscriptions | 3.2 | `CHANGES:1343`, `:1355` |
| multi-word command run by `execvp` | 2.0 | `CHANGES:2575-2576` (1.9a→2.0); `window.c@2.0:829-836` |
| `new-pane`, floating panes | 3.7 | `CHANGES:273-276` |
| `move-pane`/`resize-pane` on floats, modal panes, `new-layouts` flag | 3.8 | `CHANGES:3-39` |
| `wait-for -E` (wait for one hook/notification event) | 3.8 | `CHANGES:72-86`; `tmux.1` wait-for |

Runtime on 3.2a: `split-window -d -h -l 30% -P -F '#{pane_id}'` printed `%1`;
`#{pane_at_left}`, `#{pane_tty}`, `list-panes -s`, `resize-pane -x 40` worked;
`#{e|/|f|4:#{pane_width},#{window_width}}` printed `0.3000` and `#{e|/|f:…}`
printed `0.30` (default precision 2, `format.c@3.2:3923-3926`); panes had
`TERM_PROGRAM=tmux`, `TERM_PROGRAM_VERSION=3.2a`, `TERM=screen`;
`new-pane` printed `unknown command: new-pane`; `wait-for -E` printed
`unknown option -- E`.

Packaged versions:

- Ubuntu 22.04 (jammy): 3.2a (`3.2a-4ubuntu0.2`), per
  `https://packages.ubuntu.com/jammy/tmux`.
- Ubuntu 24.04 (noble): 3.4 (`3.4-1build1`), per
  `https://packages.ubuntu.com/noble/tmux`.
- Debian stable is 13 (trixie) (`https://deb.debian.org/debian/dists/stable/Release`):
  3.5a (`3.5a-3`); trixie-backports 3.7c; bookworm 3.3a; sid 3.8. Runtime:
  `curl 'https://qa.debian.org/madison.php?package=tmux&text=on'`.
- Homebrew: 3.8 (`https://formulae.brew.sh/api/formula/tmux.json`,
  `"stable": "3.8"`).

So every packaged tmux above has `-P -F`, `pane_id`, `pane_at_*`,
`pane_tty`, `-l N%`, float arithmetic, `TERM_PROGRAM` and `refresh-client -f`.
Floats (`new-pane`) need 3.7 (Debian stable lacks them; trixie-backports
has them); `wait-for -E` needs 3.8 (only Homebrew and sid).

## Control mode as an events source

- **Size.** A control client affects window size only once it has sent
  `refresh-client -C` (`resize.c:109-112`: control clients without
  `CLIENT_SIZECHANGED`/`CLIENT_WINDOWSIZECHANGED` are ignored; since 2.6).
  Runtime (3.8, user client 120x40, `window-size latest`): attaching `tmux -C
  attach -t S` and running commands left the window 120x39; after
  `refresh-client -C 60x20` it became 60x20; with `-f ignore-size` attached,
  `-C 50x15` left it 120x39.
- `ignore-size` is ignored when every attached client has it
  (`resize.c:95-108`). Runtime on 3.2a: a sole control client with
  `-f ignore-size` and `-C 50x15` resized the window to 50x15. On 3.8 the same
  run left a 100x40 session at 100x40; why it differs was not traced
  (*unconfirmed*). On 3.2a a control client without `-C` on a session with
  no other client showed the window as 80x23 (`default-size` 80x24 less the
  status line; *cause unconfirmed*). Simplest rule: never send `-C`.
- **Flags.** `attach -f` / `refresh-client -f` take `ignore-size`,
  `new-layouts`, `no-detach-on-destroy`, `no-output`, `pause-after=N`,
  `read-only`, `wait-exit`; `!flag` turns one off (`tmux.1:1119-1148`). The
  `active-pane` flag was removed in 3.8 (`CHANGES:192`). Runtime: `-f no-output`
  stopped all `%output` lines (0 after `send-keys 'echo hi' Enter`).
- **read-only does not make it passive.** The read-only check applies to
  commands sent as a client's argv (`server-client.c:2884-2887`) and to keys;
  control-mode stdin lines go through `cmd_parse_and_append` with no such
  check (`control.c:718-719`). Runtime: after `refresh-client -f read-only`,
  `split-window -d` and `refresh-client -C 50x15` from the control client both
  took effect. A control client is "read-only" only by not sending commands.
- **It is an attached client.** Runtime: `session_attached` went 1→2,
  `list-clients` showed `client-<pid> … attached,focused,control-mode`, and
  user hooks `client-attached` and `client-session-changed` ran for it
  (`#{client_control_mode}` was `1`, so a hook can filter it).
- **It breaks focus-out for the panes.** Every new client starts with
  `CLIENT_FOCUSED` (`server-client.c:307`); only a terminal client's focus
  report clears it (`tty-keys.c:1030-1034`). A pane counts as focused while any
  attached focused client shows its window (`window.c:716-748`), and only
  then gets `\033[O` and `pane-focus-out`. Runtime (3.8, `focus-events on`, an
  app with `\033[?1004h` in raw mode): without a control client, a focus-out
  report to the user client delivered `\x1b[O` and focus-in `\x1b[I`; with a
  control client attached to the same session, neither arrived, and the user
  client's flags dropped `focused` while the control client kept it. So
  Neovim in a tmux pane would never see FocusLost while a provider's control
  client is attached. Same code at 3.3–3.5 (`window.c@3.3:482-494`); 3.2a
  differs (*unconfirmed*).
- **Notifications** (`tmux.1:9113-9255`). Runtime, 3.8:
  - split, kill-pane, a pane's command exiting, resize: `%layout-change @W
    <layout> <visible-layout> <flags>` (twice for one `resize-pane`). There is
    no pane-exit notification; a pane exiting shows only as
    `%layout-change`, or `%window-close` for a window's last pane.
  - pane focus change in a window (`select-pane`, split without `-d`):
    `%window-pane-changed @W %P`.
  - window switch: `%session-window-changed $S @W`.
  - new window: `%window-add @W`; rename: `%window-renamed`.
  - user focus in/out of the terminal: no notification.
- **Hook events without a client (3.8).** `tmux wait-for -E -v NAME` waits for
  the next event `NAME` (hooks and notifications) and prints its payload
  (`tmux.1` wait-for; `CHANGES:72-86`). Runtime: five waiters did not appear
  in `list-clients` and left `session_attached=2`; they printed
  `event=window-pane-changed new_pane=%7 old_pane=%0 pane=%7 window=@0`,
  `event=pane-exited exit_status=3 exit_success=0 pane=%7 window=@0`,
  `event=session-window-changed new_window=@1 … session=$0`,
  `event=window-layout-changed window=@0`, `event=pane-focus-in pane=%7
  window=@0`. Each waiter takes one event and exits, so a loop can miss
  events between waits (*not measured*).

## Focus

- `select-pane -t %N` on a pane in a window that isn't current makes `%N`
  the active pane of its window and does not switch the session's window
  (`cmd-select-pane.c:141` calls only `window_set_active_pane`). Runtime:
  session stayed on `@0`; `@2`'s active pane became `%14`; the client still
  showed `@0:%0`.
- `select-window -t @W` switches the **session's** current window, so every
  client attached to that session moves. Runtime: two clients on `S` both went
  to `@2:%14`; a client on `S2` did not move.
- `switch-client -t %N` without `-c`, run outside tmux, picks the client with
  the latest activity (`cmd-find.c:104-108`, `cmd_find_best_client` at `:113`)
  even if it is on another session, and moves it. Runtime: after typing in
  the `S2` client, `switch-client -t %14` moved that client into `S`. Use
  `-c CLIENT` or avoid it; `select-window` + `select-pane` stay inside the
  session.
- `join-pane`/`move-pane` without `-d` makes the moved pane active and selects
  its window in the destination session (`cmd-join-pane.c:523-527`). Runtime:
  without `-d` the session switched from `@10` to `@8`; with `-d` it stayed.
- `split-window -d` leaves the active pane alone (`cmd-split-window.c:138-139`,
  `spawn.c:604`). Runtime: after three `split-window -d`, `%0` was still
  `pane_active=1`.

## Placing and resizing

- `join-pane -h|-v [-b] [-f] -l N|N% -s SRC -t DST`: the new cell is split off
  `DST` before `SRC` is removed (`cmd-join-pane.c:490-497`); `N%` is of
  `DST`'s width/height, or of the window with `-f`
  (`layout.c:1658-1680`). Runtime (100x40 window): `-h -l 30%` gave `%17`
  width 30 right of `%16`; `-h -f -l 20%` gave width 20.
- **Within one window `-l` is not the final size.** The room `SRC` leaves goes
  to its neighbour, which can be the new cell. Runtime: `%17`(17) `%16`(82);
  `move-pane -b -h -l 25% -s %17 -t %16` gave `%17` width 38 (20 asked + 18
  freed). Follow a move with `resize-pane`.
- `resize-pane -x N` resizes the nearest ancestor split of the left-right kind
  (the cell containing the pane at that level), against its next sibling, or
  its previous one if it is last (`layout.c:874-903`, `:1007-1027`). `-x N%`
  is of the window width (`cmd-resize-pane.c:99`). Runtime:
  - `A | (B / C)`: `resize-pane -t B -x 30` made the B/C column 30 wide (C too);
    `-y 10` on B moved the B/C border; `-y 10` on C (last) also moved it.
  - `A | B | C`: `-x 50` on B took the room from C, not A; `-x 10` on C (last)
    took from B.
  - `-y` on a pane with no top-bottom ancestor did nothing, and printed no
    error.

## Environment and nesting

- A pane gets the server's global environment (from the process that started
  the server) plus the session's, then `TERM` (`default-terminal`),
  `TERM_PROGRAM=tmux`, `TERM_PROGRAM_VERSION`, `COLORTERM=truecolor`, and
  `TMUX=<socket>,<server pid>,<session id>` (`environ.c:252-285`), and
  `TMUX_PANE=%N` (`spawn.c:411`). `update-environment` copies only DISPLAY,
  SSH_*, XDG_SESSION_* and similar (runtime `show -g update-environment`).
- **Other multiplexers' variables leak into tmux.** Runtime: a server started
  with `ZELLIJ=0 ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID REX_SERVER REX_SESSION
  HERDR_SOCKET GHOSTTY_RESOURCES_DIR TERM_PROGRAM=ghostty` set gave its pane
  all of them unchanged except `TERM`, `TERM_PROGRAM`, `TERM_PROGRAM_VERSION`,
  which tmux replaced.
- **tmux's variables leak into zellij.** Runtime: zellij 0.46.0 run in a tmux
  pane gave its own pane `TMUX=…,9`, `TMUX_PANE=%30` (the outer pane),
  `TERM=tmux-256color`, `TERM_PROGRAM=tmux`, `TERM_PROGRAM_VERSION=3.8`, plus
  `ZELLIJ=0`, `ZELLIJ_PANE_ID=0`, `ZELLIJ_SESSION_NAME`. The zellij pane's tty
  (`ttys029`) was not `%30`'s `pane_tty` (`ttys026`).
- **tmux's own "inside" test is tty-based.** `server_client_check_nested`
  says a client is inside this server when `TMUX` is set and its tty is one of
  the server's pane ttys (`server-client.c:211-226`). A command's current pane
  is the pane whose tty is the client's, else `$TMUX_PANE`
  (`cmd-find.c:80-100`), so a leaked `TMUX_PANE` retargets a command with no
  `-t`. The socket comes from `$TMUX` unless `-L`/`-S` is given
  (`tmux.c:557-566`). Runtime: `list-panes -a -f '#{==:#{pane_tty},/dev/ttys029}'
  -F '#{pane_id}'` printed `%31` for the pane whose `tty` was `/dev/ttys029`.
- Herdr and Rex inside tmux, and tmux inside them: *unconfirmed*; not run
  (herdr 0.9.5 is installed but launches a persistent per-user session; Rex
  is not installed). From the runs above, tmux passes their variables through.

## Running the command

- `split-window`, `new-window`, `new-pane`, `respawn-pane` with **two or more**
  words `execvp` them directly; with **one** word, `$SHELL -c WORD` (shell is
  `default-shell`); with none, `default-command` or a login shell
  (`spawn.c:546-574`). Runtime: argv `show.sh 'two words' '$HOME;x'` arrived
  as 2 args, unexpanded; the single string `"show.sh 'two words' \$HOME"`
  arrived with `$HOME` expanded.
- `remain-on-exit` defaults to `off` (`options-table.c:1699-1703`); runtime
  `show -gw remain-on-exit` printed `off`. Values: `on`, `off`, `failed`,
  `key`, `failed-key`.
- 3.8 turns `mouse` on by default (`CHANGES:190`); runtime `show -g mouse`
  printed `on`.

## Not confirmed

- Why a sole `ignore-size` control client with `-C` resized on 3.2a but not
  on 3.8, and the 80x23 size on 3.2a.
- Focus-out with a control client on 3.2a (focus code differs before 3.3).
- Events lost between `wait-for -E` loops.
- Herdr, Rex and Ghostty nesting in either direction.
- Version claims below 3.2a were read from source and CHANGES, not run;
  3.4 (Ubuntu 24.04) and 3.5a (Debian) were not run.

## What this means for the pane provider interface

- A `tmux` provider can target **3.2** as its floor (Ubuntu 22.04 has 3.2a):
  `-P -F '#{pane_id}'`, `-l N%`, `pane_at_*`, `pane_tty`, `list-panes -f`,
  float arithmetic (for `report_layout`'s fractions) and `TERM_PROGRAM` are all
  there. `open_float` needs 3.7, and float moves/resizes 3.8; it should return
  69 below that.
- `find` should do what tmux does for itself: `TMUX` says which server
  (`-S` from its first field), and the attach's `TTY` must be one of that
  server's `pane_tty`s (`list-panes -a -f '#{==:#{pane_tty},TTY}'`). `TMUX`,
  `TMUX_PANE` and `TERM_PROGRAM=tmux` all leak into zellij run inside tmux, so
  `context_vars`/`find_vars` on them alone mis-assign. Every command should
  pass `-t` and the socket explicitly.
- `{attach}` expands to several words, so tmux `execvp`s it with no shell. An
  attach command of one word would go through `$SHELL -c`.
- `open_beside` and `place_beside` should use `-d` (split-window, join-pane,
  move-pane): without it `join-pane` switches the session's window and takes
  focus. `place_beside` within one window must follow `join-pane -l` with
  `resize-pane`, since the freed room lands on the new cell.
- `resize` maps to `resize-pane -x/-y` on the terminal, but tmux moves the
  border of the enclosing split against its next (or previous) sibling, not
  necessarily `ORIGIN`. A provider computes cells from `RATIO` and both
  panes' sizes, and when `ORIGIN` isn't that sibling, resizes `ORIGIN`
  instead.
- `focus` on a pane in another window needs `select-window -t @W` and
  `select-pane -t %N`; both are per session, so every client on the session
  moves. Never `switch-client` without `-c`: it can grab a client on another
  session.
- `events` via `tmux -C attach` costs the user focus-out in every pane and
  counts as an attached client (their `client-attached` hooks run). If used:
  `-f no-output,ignore-size`, never `refresh-client -C`, and read
  `%layout-change`, `%window-pane-changed`, `%session-window-changed`,
  `%window-close` as "run `report_layout`". On 3.8, a loop of `tmux wait-for
  -E` on `window-layout-changed`, `window-pane-changed`,
  `session-window-changed` and `pane-exited` gives the same events, with pane
  ids, without attaching; below 3.8, hooks or polling. Either way `events`
  stays optional, as the earlier note concluded.
