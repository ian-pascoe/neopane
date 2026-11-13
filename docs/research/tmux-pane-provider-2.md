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

## Follow-ups

Same method: tmux 3.8 (Homebrew) and a 3.7c built from the release tarball
(tag `3.7c`, commit `e476c123`), private sockets (`nprs3`–`nprs6`, `nprs37`,
`nprs38`), a user client attached from a second private server where focus or
borders mattered, and zellij 0.46.0 from a temp dir with its own
`ZELLIJ_SOCKET_DIR`, `HOME` and XDG dirs. All were killed afterwards.

### `wait-for -E` (3.8)

- **One event name per call.** `wait-for` takes exactly one argument
  (`cmd-wait-for.c:37`, `{ "EF:LSUlvw:", 1, 1 }`), and a sink matches it with
  `strcmp` (`events.c:108`). Runtime: `wait-for -E window-layout-changed
  pane-exited` printed `too many arguments (need at most 1)`;
  `'window-layout-changed,pane-exited'` printed `invalid event: …`. So you
  need one waiter per name.
- **Valid names** are the hook options plus `@user` names
  (`hooks.c:283-291`; the hook list is `options-table.c:1965-2075`). Runtime
  `wait-for -E -l NAME` accepted `window-layout-changed`,
  `window-pane-changed`, `session-window-changed`, `client-session-changed`,
  `window-linked`, `window-unlinked`, `pane-focus-in`, `pane-focus-out`,
  `pane-exited`, `pane-died`, `window-zoomed`, `window-unzoomed`,
  `window-resized`, `pane-resized`, `pane-moved`, `client-focus-out`, `@mine`.
  It refused `layout-change` and `bogus` (`invalid event`).
- **Server-wide.** `events_fire` hands every event to every sink of that name,
  whatever the session (`events.c:94-112`). Runtime: a waiter on
  `window-layout-changed` woke for a split in session `S2` (`window=@1`) while
  the user was on `S`.
- **Narrowing with `-F`.** The filter sees only the payload keys as formats
  (`events-payload.c:485-515`): `#{window}`, `#{pane}`, `#{session}` (ids), and
  `#{session_name}`/`#{window_name}` for session/window items. Runtime:
  `-F '#{==:#{window},@0}'` woke only for `@0`; `-F '#{==:#{session},$0}'`
  worked for `session-window-changed`. `-F '#{==:#{session_name},S}'` and
  `#{window_id}`/`#{hook_window}` never matched (the `session_name` case is
  *unexplained*). Window and pane events carry no session key, so to scope
  them to a session, filter on the session's window ids. `-v` prints each
  payload even when the filter is false (`cmd-wait-for.c:234-235`).
- **Commands that fire `window-layout-changed`.** Runtime, one waiter per
  command, all woke: `swap-pane`, `select-layout even-vertical`,
  `resize-pane -Z` (zoom and unzoom; zoom also fires `window-zoomed`),
  `break-pane -d`, `join-pane -d` (it woke first for the source window `@2`),
  `kill-pane`, `resize-pane`, `split-window`. Other events, each run once:
  `select-pane` gave `window-pane-changed new_pane=… old_pane=…`;
  `new-window` (no `-d`) gave `session-window-changed`; `new-window -d` gave
  `window-linked session=$0 window=@4 window_index=2`; `kill-window` gave
  `window-unlinked`; `switch-client` gave `client-session-changed
  new_session=$1 old_session=$0`.
- **A waiter is not an attached client.** Runtime: while three waiters ran,
  `list-clients` showed only the user's client, `session_attached` stayed
  `1`, and a `client-attached` hook logged nothing. A waiter has no session,
  so it never counts for focus (`window.c:727`).
- **FocusLost stays intact.** Runtime (with an app that had `\033[?1004h` set,
  waiters running): focus-out on the user client delivered `\x1b[O`, focus-in
  `\x1b[I`. The `pane-focus-out` waiter woke with `pane=%8 window=@0`; the
  `client-focus-out` waiter woke with `client=/dev/ttys013 session=$0
  window=@0`.
- **With `TMUX` set and stdin not a tty.** Runtime: `TMUX=<socket>,<pid>,0
  TMUX_PANE=%0 tmux wait-for -E -v … </dev/null` (no `-L`) ran and woke.
  Command clients need no tty. `wait-for -E` refuses only when the command
  has no client at all (`cmd-wait-for.c:279-280`, `not able to wait`).

### Floating panes in 3.7 and 3.8

- 3.7's `new-pane` takes `-x -y -X -Y`, each in cells or `N%` of the window
  (`cmd-split-window.c@3.7c:41-47`; `layout.c@3.7c:1367-1395`). Defaults are
  half the window wide and a quarter high, cascaded from (4,2). 3.7's
  `new-pane` has no `-B` and no `-T`. Its man page entry still describes
  splitting.
- **3.7: the flags give the content.** Runtime (80x24 window, `new-pane -x 20
  -y 6 -X 10 -Y 4`): `pane_left=10 pane_top=4 pane_width=20 pane_height=6`.
  The frame was drawn one cell outside the content, at columns 9–30 and rows
  3–10.
- **3.8: the flags give the frame.** `-x`/`-y` include the two border cells
  (`layout.c@3.8:1747-1759`, `sx -= 2` when the pane has lines). Runtime, same
  command: `pane_left=11 pane_top=5 pane_width=18 pane_height=4`, with the
  frame at columns 10–29 and rows 4–9. So `-X`/`-Y` name the frame's top-left.
- **`-B none` exists in 3.8** (`tmux.1:3665`, `:3707-3709`; `none` is "no
  border for floating panes", `tmux.1:5998`). Runtime: `-B none` gave
  `pane_left=10 pane_top=4 pane_width=20 pane_height=6`, with no frame drawn.
- `pane_left/top/width/height` always report the **content**, in both
  versions, and are window-relative.

### `#{window_layout}`

- **3.8 (v2, JSON):** `{"V":2,"L":CELL}` (`layout-custom.c:30-58`,
  `:284-300`). A cell has `t` (`h` = side by side, `v` = stacked, `p` = pane),
  `w`, `h`, `x`, `y`. Node cells also have `c` (children in order). Pane
  cells also have `I` (`%N`), `i` (index), `a` (active), `l` (last-visited
  index) and, for a float, `z` (`layout-custom.c:343-400`). Floats are child
  cells of the root with `z` set. Runtime: a float under a single tiled pane
  gave a root `{"t":"v",…,"c":[{…"I":"%0"},{"w":18,"h":4,"x":11,"y":5,"z":0,"I":"%1"}]}`.
- **v1 (3.7 and older, and control clients without `new-layouts`):**
  `CSUM,WxH,X,Y` and then either `,ID` for a pane, or `{…}` (side by side) or
  `[…]` (stacked) holding the children, comma-separated
  (`layout-custom.c:404-440`). `CSUM` is a 16-bit rotate-and-add of the rest,
  in 4 hex digits (`layout-custom.c:270-279`). In v1, `ID` is the pane number
  without `%`. Floats are left out of 3.8's v1 (`layout-custom.c:538-552`).
  3.7 appends them as `<WxH,X,Y,ID>`; runtime 3.7c printed
  `eb65,80x24,0,0[80x24,0,0,0,20x6,10,4,1]<20x6,10,4,1>`.
- **Sizes exclude borders.** A pane cell's `w`/`h` is the content. Siblings
  are 1 cell apart (the border), and a node's size spans its children and the
  borders between them. Runtime (3.8, 100x40, `((P | A) / Q) | C`):
  `{"t":"h","w":100,…,"c":[{"t":"v","w":50,…,"c":[{"t":"h","w":50,"h":20,…,"c":[P 25x20 @0,0, A 24x20 @26,0]}, Q 50x19 @0,21]}, C 49x40 @51,0]}`
  (25+1+24 = 50; 50+1+49 = 100). The same layout in v1 was
  `92fa,100x40,0,0{50x40,0,0[50x20,0,0{25x20,0,0,0,24x20,26,0,3},50x19,0,21,2],49x40,51,0,1}`.
- **The split two panes share** is their deepest common ancestor. They share a
  border only if they are in adjacent children `c[k]`, `c[k+1]` of that node,
  and its `t` says which axis.
- `select-layout` accepts either format as a whole layout. Runtime: a
  hand-made v1 string with a correct checksum applied; with `0000,` it
  printed `invalid layout checksum`. A v2 string without `i` printed
  `key "i" not found`. A v2 string whose sizes didn't add up printed `size
  mismatch after applying layout`. `I` is ignored on parse
  (`layout-custom.c:53`), so pane assignment follows `i`/order (*not traced*).

### `resize-pane` on a nested split

Runtime (3.8, 100x40), `((P | A) / Q) | C`:

| Command | Border moved |
|---|---|
| `P -x 30` | P\|A (P became 30) |
| `A -x 30` | P\|A (A, last, became 30) |
| `Q -x 70` | left column\|C (column became 70; P and A both grew) |
| `C -x 20` | left column\|C (C, last, became 20) |
| `P -y 25`, `A -y 10` | (P\|A)/Q |
| `Q -y 10` | (P\|A)/Q (Q, last) |

- So the recipe (resize the pane on the left/top side of the shared border,
  setting it to the width/height wanted for **that whole side**) holds only
  when the pane has no split of the same kind between it and the shared
  split. `-x` stops at the nearest `h` ancestor (`layout.c:874-903`).
- **Counter-example.** In `((P | A) / (X | Y)) | C`, every left pane has its
  own `h` split, so `-x` on P, A, X or Y each moved only its inner border.
  Only `C -x 30` (C is last) moved the column|C border. Runtime confirmed
  both.
- A working rule: to move the border between children `c[k]` and `c[k+1]` of
  a split, pick a pane in `c[k]` reachable without crossing a split of the
  same kind, and set it to `c[k]`'s new size. If none exists and `c[k+1]` is
  last, do the same from `c[k+1]`. Otherwise write the whole layout with
  `select-layout` (above).

### `pane-border-status top`

- Runtime (3.8, 80x24, `(P | C) / B`). With `off`: P `top=0 40x12`, C `top=0
  39x12`, B `top=13 80x11`. With `top`: P `top=1 40x11`, C `top=1 39x11`, B
  `top=13 80x11`. The panes on the top row lose their first row to a status
  line at row 0. Lower panes use the existing separator row (12) as their
  status line and keep their size.
- With `bottom`, P and C were unchanged (row 12 became their status line), and
  B became `80x10`, with a status line on row 23.
- `#{window_layout}` does not change with this option: P's cell stayed
  `"h":12,"y":0`. So layout cells and `pane_top/pane_height` disagree by the
  status row. `pane_at_top` stayed `1` for P and C.

### zellij and tmux nested (zellij 0.46.0, tmux 3.8)

The outer environment was `env -i` plus `TERM=xterm-256color
TERM_PROGRAM=ghostty TERM_PROGRAM_VERSION=1.2.3`.

- **zellij pane, no tmux:** `TERM=xterm-256color TERM_PROGRAM=ghostty
  TERM_PROGRAM_VERSION=1.2.3 ZELLIJ=0 ZELLIJ_PANE_ID=0
  ZELLIJ_SESSION_NAME=nprs5z ZELLIJ_SOCKET_DIR=…`. zellij sets no
  `TERM_PROGRAM` of its own and passes the outer one through.
- **tmux inside zellij** (a private server started from the zellij pane): the
  tmux pane had `TERM=tmux-256color TERM_PROGRAM=tmux TERM_PROGRAM_VERSION=3.8
  COLORTERM=truecolor TMUX=…/nprs5,60196,0 TMUX_PANE=%0` and still
  `ZELLIJ=0 ZELLIJ_PANE_ID=0 ZELLIJ_SESSION_NAME=nprs5z ZELLIJ_SOCKET_DIR`.
  The server's global environment held the zellij values and
  `TERM_PROGRAM=ghostty`, which tmux overrides per pane. tmux pane tty
  `ttys017` ≠ zellij pane tty `ttys016`.
- **zellij inside tmux:** the zellij pane had `TERM=tmux-256color
  TERM_PROGRAM=tmux TERM_PROGRAM_VERSION=3.8 COLORTERM=truecolor
  TMUX=…/nprs6,61123,0 TMUX_PANE=%0 ZELLIJ=0 ZELLIJ_PANE_ID=0
  ZELLIJ_SESSION_NAME=nprs6z`. zellij pane tty `ttys023` ≠ tmux `%0`'s
  `pane_tty` `ttys021`.
- So each inner pane carries both multiplexers' variables. The innermost one
  is shown by `TERM_PROGRAM` only when the inner one is tmux, and by no
  variable when it is zellij. The tty tells them apart in both directions.

### Zoom

Runs used tmux 3.8, 3.7c and 3.2a (private sockets `nprs7`–`nprs9`,
`nprs77`, `nprs72`; killed afterwards). The window was 80x24 (80x23 on 3.2a)
with `P | (A / B)`, and P was zoomed with `resize-pane -Z -t P`. The script
reset the zoom to P before each command.

- **Geometry while zoomed.** The zoomed pane reports the whole window, and
  hidden panes keep their unzoomed values. Runtime 3.8: P `0,0 80x24`, A
  `41,0 39x12`, B `41,13 39x11` (A and B as before the zoom). The same held on
  3.7c and 3.2a. `stty size` inside: P `24 80`, A `12 39`, B `11 39`. Hidden
  ptys keep their size. (Just after the splits, A's `stty` read `24 39` while
  `pane_height` was 12; the pty size can lag the layout. *Cause not traced*.)
- **A quirk: panes created hidden.** `split-window -d -Z -t P` (window stays
  zoomed) made a new pane `%3` that reported `0,0 80x24`, the zoomed size,
  though `#{window_layout}` placed it at `40x11` under P. After unzooming it
  reported `0,13 40x11`. The same happened with `-t` a hidden pane. So while
  `window_zoomed_flag` is 1, `pane_*` of any pane but the zoomed one is not
  reliable.
- **Is the zoomed pane always active?** For tiled panes, yes:
  `window_set_active_pane` unzooms when the new active pane isn't visible
  (`window.c:764-765`). Runtime: `select-pane -t A` (hidden) set zoomed=0 and
  made A active. `select-pane -Z -t B` kept the zoom and moved it to B (B
  active, 80 wide); `cmd-select-pane.c:280-287` pushes and pops the zoom. The
  exception is 3.8 floats above a zoom: after `new-pane -d -A`,
  `select-pane -t <float>` left zoomed=1 with the float active and P still
  80 wide.
- **Layouts.** `#{window_layout}` is the saved, unzoomed tree when zoomed
  (`format.c:859-878` uses `saved_layout_root`). `#{window_visible_layout}`
  is the live one (`format.c:883-897`). Runtime 3.8 while zoomed:
  `window_layout` was the full `h[P, v[A, B]]` tree, and
  `window_visible_layout` was `{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":0,"I":"%0"}}`.
  3.7c printed `d67e,80x24,0,0{40x24,0,0,0,39x24,41,0[…]}` and
  `b25d,80x24,0,0,0`; 3.2a was the same shape.
- **Which commands unzoom.** Runtime results, identical on 3.8, 3.7c and 3.2a
  except where noted:

  | Command | Zoom after |
  |---|---|
  | `select-pane -t <hidden>` | unzoomed |
  | `select-pane -Z -t <hidden>` | kept, now on the target |
  | `split-window -d -t <hidden>` or `-t <zoomed>` | unzoomed |
  | `split-window -d -Z -t <zoomed>` | kept, still P (new pane hidden) |
  | `split-window -Z -t <zoomed>` | kept, on the new pane |
  | `resize-pane -x` on the zoomed or a hidden pane | unzoomed (`cmd-resize-pane.c:94-95`) |
  | `resize-pane -Z -t <hidden>` | unzoomed (it toggles; it doesn't zoom the target, `cmd-resize-pane.c:86-90`) |
  | `resize-pane -x` on a 3.8 float | kept (floats are skipped, `cmd-resize-pane.c:94`) |
  | `join-pane -d` | unzoomed (`cmd-join-pane.c:473-474`) |
  | `new-window -d`, `new-window` | kept (the zoomed window isn't touched) |
  | `new-pane -d` (float) | unzoomed on 3.8 and 3.7c; command missing on 3.2a |
  | `new-pane -d -A` (3.8) | kept; 3.7c: `unknown flag -A` |
  | `kill-pane` (a hidden pane) | unzoomed (`cmd-kill-pane.c:81`) |
  | `swap-pane -d` | unzoomed |
  | `swap-pane -d -Z` (3.8) | kept, now on the swapped-in pane (`cmd-swap-pane.c:85`, `:200-207`) |
  | `select-layout` | unzoomed (`cmd-select-layout.c:84`) |
  | `break-pane` | unzoomed (source: `cmd-break-pane.c:128`; not run) |
  | `kill-pane` the zoomed pane | unzoomed; the others took the space |

## Not confirmed

- Why a sole `ignore-size` control client with `-C` resized on 3.2a but not
  on 3.8, and the 80x23 size on 3.2a.
- Focus-out with a control client on 3.2a (focus code differs before 3.3).
- Events lost between `wait-for -E` loops.
- Why `wait-for -E -F '#{==:#{session_name},S}'` never matched.
- How `select-layout` assigns panes to a hand-made layout's leaves.
- Why a pane's pty size briefly lagged its layout size after `split-window -d`.
- `break-pane` while zoomed (source only).
- Herdr, Rex and Ghostty nesting in either direction (zellij was run; see
  Follow-ups).
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
- On 3.8, `events` is best built from `wait-for -E`: one waiter per event
  name, filtered with `-F '#{==:#{window},@N}'` or `#{session}`, since events
  are server-wide. Waiters are not clients and leave FocusLost and the user's
  hooks alone, and they work with `TMUX` inherited and stdin `/dev/null`.
- `open_float` must use different geometry per version. 3.7's `-x -y -X -Y`
  give the content, and the frame sits one cell outside. 3.8's give the
  frame, so add 2 to the size and subtract 1 from the position for the same
  content, or use `-B none`. `report_layout` should take float rects from
  `pane_*` (content) and report the frame as `inset` [1,1,1,1] unless
  borderless.
- `report_layout` can compute fractions from `#{window_layout}` or from
  `list-panes` `pane_*`. With `pane-border-status` set they differ by a row,
  so `pane_*` is the one that matches what the terminal shows.
- Zoom: while `window_zoomed_flag` is 1, `report_layout` should treat the
  window as showing only the zoomed pane (and 3.8 `-A` floats), with
  `#{window_visible_layout}` or the zoomed pane's `pane_*`. The hidden panes'
  `pane_*` hold stale geometry, or the whole window for panes created
  hidden. `focus` on a hidden pane unzooms (`select-pane`) unless `-Z` is
  passed. `place_beside`, `resize`, `open_beside` and `close` all unzoom, and
  `open_tab_page` (`new-window`) doesn't. A provider that wants to keep the
  user's zoom can pass `-Z` to `split-window`, `select-pane` and `swap-pane`.
  Nothing keeps it for `join-pane`, `resize-pane` or `kill-pane`.
- `resize` must find the split the two panes share in `#{window_layout}` and
  pick a pane that `-x`/`-y` will resolve to that split. Where none exists,
  it must fall back to `select-layout` with a whole recomputed layout.
