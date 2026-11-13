# zellij as a pane provider

Gathered against zellij 0.46.0, the latest release tag (`v0.46.0`, an
annotated tag on commit `aedee21c`, "chore(release): v0.46.0"). Source was
read in a shallow clone at that tag. Behaviour was checked with the release
binary `zellij-aarch64-apple-darwin.tar.gz` (`zellij --version` printed
`zellij 0.46.0`), downloaded into a temp dir and run on macOS arm64 with
`HOME`, `TMPDIR`, `ZELLIJ_SOCKET_DIR` and `ZELLIJ_CONFIG_DIR` all pointing into
that temp dir, so no user session, config or cache was touched. Sessions were
made with `zellij attach --create-background <name>` and driven with
`zellij --session <name> action ...`; interactive clients were real PTYs
(`TERM=xterm-256color zellij attach <name>`, one 120x40 and one 100x30). All
sessions were killed at the end. Items marked *source only* were not run, and
*unconfirmed* ones were neither run nor found in the source.

Links: source is `https://github.com/zellij-org/zellij/blob/v0.46.0/<file>#L<line>`.
Below, `file:N` refers to that. At the tag, the 0.46.0 entries in
`CHANGELOG.md` still sit under `## [Unreleased]` (`CHANGELOG.md:7-57`); entries
for 0.45.1, 0.45.0, 0.44.x follow. "Runtime:" means a run against the 0.46.0
binary. Minimum versions come from the changelog, or from comparing `cli.rs` at
the `v0.43.1`, `v0.44.0` and `v0.45.0` tags where the changelog is silent.

## Environment in a pane

- A pane's command gets `ZELLIJ_PANE_ID=<N>`, the bare terminal id with no
  `terminal_` prefix (`zellij-server/src/os_input_output_unix.rs:249`). Runtime:
  `new-pane` printed `terminal_1` and the pane's `env` had `ZELLIJ_PANE_ID=1`.
- `ZELLIJ=0` and `ZELLIJ_SESSION_NAME=<name>` are set by the client in its own
  process before it starts the server (`zellij-client/src/lib.rs:1845`, `:1852`,
  `:1898`; `zellij-utils/src/envs.rs:11`, `:19`), so every pane inherits them
  from the server. Runtime: the pane env had `ZELLIJ=0` and
  `ZELLIJ_SESSION_NAME=zr1`; `ps eww` on the server process showed the same two.
- A pane's env is otherwise the env of the client that started the server,
  plus the config's `env` block (`zellij-server/src/lib.rs:1783-1793`). It is
  **not** the env of the `zellij action` caller. Runtime: `FOO=callerenv
  zellij action new-pane -- sh -c env` gave a pane without `FOO`.
- Zellij sets no `TERM`, `TERM_PROGRAM` or `COLORTERM` of its own. Runtime: a
  server started from a shell without `TERM` gave panes without `TERM`; a server
  started by `TERM=xterm-256color zellij attach -c inner` gave panes
  `TERM=xterm-256color`. *Source*: no `TERM` assignment in the spawn path
  (`os_input_output_unix.rs:228-258`).
- `ZELLIJ_SOCKET_DIR` set when the server starts stays in the server's env and
  so reaches every pane (runtime: present in the pane env).
- `ZELLIJ_ORIGIN_PANE_ID` exists only for commands run from `Run` keybindings
  (`zellij-server/src/pty.rs:42-52`; 0.46, `CHANGELOG.md:33`). Runtime: not in
  `new-pane` panes.
- Nested (zellij in zellij): the inner server is started from inside an outer
  pane, so it inherits the outer `ZELLIJ*` variables, then overwrites
  `ZELLIJ_SESSION_NAME` and each pane's `ZELLIJ_PANE_ID`. Runtime: an inner
  session started from outer pane `terminal_21` gave its panes
  `ZELLIJ_PANE_ID=1` and `ZELLIJ_SESSION_NAME=inner`, and the outer
  `list-panes --json` showed `"nested_session_name": "inner"` on pane 21
  (field new with nested-session handling in 0.45.0, `CHANGELOG.md:99`;
  `zellij-utils/src/data.rs:3591`).
- zellij in tmux: a pane inherits whatever the server's starting client had, so
  `TMUX` and `TMUX_PANE` of the tmux pane that started the zellij server leak
  into every zellij pane, whichever terminal later attaches. Runtime (tmux is
  not installed, so `TMUX` was faked): `TMUX=/fake/tmux,1,0 zellij attach -c
  inner` gave the inner pane `TMUX=/fake/tmux,1,0`.
- tmux in zellij: *unconfirmed* (tmux not installed). Expect the mirror image:
  the tmux server keeps the `ZELLIJ_PANE_ID` of the zellij pane that started it.

## Targeting a session from outside

- `zellij --session S action ...` picks S; without `--session` it uses
  `ZELLIJ_SESSION_NAME`, else fails with "Please specify the session name"
  (`src/commands.rs:562-578`).
- `ZELLIJ_SOCKET_DIR` replaces the socket dir; the socket lives under
  `<dir>/contract_version_1/` (`zellij-utils/src/consts.rs:343-353`). Runtime:
  the sockets appeared in `$ZELLIJ_SOCKET_DIR/contract_version_1/`.
- **New in 0.46:** a socket is named after a session id, not the name, and the
  name-to-id map is a SQLite index in the cache dir
  (`zellij-utils/src/sessions.rs:49-61`, `:83-85`;
  `zellij-utils/src/session_index.rs:162-169`). `sessions.rs` at `v0.45.1` has
  no `register_session`. Runtime: the sockets were `3d281619c0a2` and
  `edbb0f519ba0`; with a different `HOME`, `zellij ls` listed only those ids and
  `--session zr1` failed with "Session 'zr1' not found", while `--session
  3d281619c0a2 action list-tabs` worked. So a caller needs the same socket dir
  **and** cache dir (`HOME`, or the platform cache dir) as the session, or the id.
- The server knows nothing of who issued a CLI action except: the CLI client
  sends its own `ZELLIJ_PANE_ID`, if set, as the issuing pane
  (`zellij-client/src/cli_client.rs:37-39`).
- **Which client an action acts for.** A CLI action acts for the *last client
  that pressed a key*, falling back to the CLI connection's own client id
  (`zellij-server/src/route.rs:3222-3237`; "last active" is set only on key
  input, `:3061-3065`). Many screen actions then fall back again to the first
  connected client when the id has no tab (`zellij-server/src/screen.rs:253-292`),
  but not all. Runtime, two clients 10 and 152 attached: after client 152 typed
  keys, `move-focus right` and `new-pane` moved client 152 and left client 10
  alone. Before any client had typed, `focus-pane-id` returned 0 and moved no
  client; the log said "active tab not found for client 87", the CLI's own id.
- With no client attached, focus actions fail: `move-focus left` exited 2 with
  "No connected clients to move focus for" (`screen.rs:11679-11682`).

## Creating panes and reading their ids

- `zellij action new-pane [-- CMD ARGS...]` runs the argv directly with
  `Command::new(cmd).args(args)` after `login_tty`, no shell
  (`os_input_output_unix.rs:230`, `:247-251`). Runtime: `new-pane -- printf
  '[%s]\n' 'a  b' '$HOME' '*'` dumped `[a  b]`, `[$HOME]`, `[*]`. Without a
  command it runs the default shell.
- It prints the new id on stdout as `terminal_<N>` (help: "Returns: Created
  pane ID (format: terminal_<id> or plugin_<id>)"; since 0.44.0,
  `CHANGELOG.md:194`). Runtime: printed `terminal_1`. `zellij run` does the same.
  Every action that takes a pane id accepts `terminal_N`, `plugin_N` or a bare
  `N` meaning `terminal_N` (help text of `close-pane`, `focus-pane-id`, etc.).
- Ids are never reused, even for a pane that failed to open (0.44.0,
  `CHANGELOG.md:233`). Runtime: a `new-pane` that failed with "No room for a new
  pane" used up `terminal_7`; the next pane was `terminal_8`.
- **Directions: only right and down.** `-d left` and `-d right` both make a
  vertical split with the new pane on the right; `up` and `down` both put it
  below (`zellij-server/src/tab/mod.rs:3319`; `screen.rs:11248-11270`). Runtime:
  `-d left` from `terminal_3` (x=60, 60 wide) gave `terminal_3` 30 wide at x=60
  and the new pane at x=90; `-d up` put the new pane below. Help says
  "[right|down]". A split too small fails, exit 2, "No room for a new pane"
  (`tab/mod.rs:184`).
- **Splitting a given pane.** `--near-current-pane` or `--no-focus`, with
  `ZELLIJ_PANE_ID=<N>` in the caller's env, splits pane N instead of the
  focused one (`route.rs:243-264`, `new_pane_routing`). That path never moves
  focus (`should_focus_pane = false`, `screen.rs:11243`) and works for a pane in
  any tab. Runtime: `ZELLIJ_PANE_ID=1 zellij action new-pane
  --near-current-pane -d right -- sleep 100000` split `terminal_1` in half; with
  the client on tab 0, `ZELLIJ_PANE_ID=11 ... --near-current-pane -d down` split
  `terminal_11` in tab 1 and no client moved. `--near-current-pane` exists at
  `v0.44.0`; `--no-focus` came in 0.45.0 (`CHANGELOG.md:88`).
- No size option for tiled splits: the split halves the target pane
  (runtime: 60 → 30 + 30; 60x13 → 60x6 + 60x6 with a neighbour shrinking by a
  row). There is no "before"/left/above placement.
- Without a client and without a target, a directional split splits the first
  selectable pane (`tab/mod.rs:3342-3344`). Runtime: in a session with no
  client, `-d down` split `terminal_0`, not the newest pane.
- Focus: a plain `new-pane` focuses the new pane for the acting client
  (runtime: `list-clients` showed the client on the new pane); `--no-focus`
  leaves every client where it was (runtime). `--tab-id` targets a tab (0.44.1,
  `CHANGELOG.md:140`).
- Command exit: by default a command pane stays, held, after exit (runtime: `sh
  -c 'exit 3'` stayed with `"exited": true, "exit_status": 3, "is_held":
  true`). `-c/--close-on-exit` removes it (runtime: `exit 4` pane gone).
  `--blocking` and `--block-until-exit*` make the CLI wait.
- Floating: `--floating -x X -y Y --width W --height H`, each a bare integer
  (cells) or `N%` (help). Percentages are of the tab's viewport; x and y are
  absolute screen cells, then clamped into the viewport, so they are not offset
  by the tab bar (`zellij-utils/src/pane_size.rs:273-330`). Runtime: `-x 10 -y 5
  --width 40 --height 10` gave rect `10,5 40x10`; `-x 50% -y 50% --width 25%
  --height 25%` on a 120x38 viewport gave `60,19 30x9`; moving a float to `-y 0`
  gave `y=1`, below the one-row tab bar. Omitting x/y with a size centres it
  (`pane_size.rs:288-297`). `--pinned` keeps it on top.
- `change-floating-pane-coordinates -p ID -x -y --width --height` moves and
  sizes an existing float (runtime: `-p 13 -x 0 -y 0 --width 20 --height 5`
  gave `0,1 20x5`). Present at `v0.44.0`, not at `v0.43.1`.
- `toggle-pane-embed-or-floating -p ID` floats or embeds a given pane
  (runtime: embedding `terminal_13` tiled it into the layout and moved the
  acting client's focus onto it).
- `--in-place [--pane-id ID] [--close-replaced-pane]` replaces a pane,
  suspending the old one (runtime: `--in-place --pane-id terminal_12` gave
  `terminal_18` in its rect, `terminal_12` then `is_suppressed: true`).
- `--stacked` stacks onto the focused pane (runtime: `terminal_19` shared
  `terminal_13`'s rect, `terminal_13` turned `is_suppressed: true`). Horizontal
  stacks are 0.46 (`CHANGELOG.md:51`).
- `--name` names the pane; a command pane's title is that name or the command
  line, and ignores OSC 2 (runtime: a command pane printing `OSC 2
  neopane-late` after 1 s kept the title `sh -c sleep 1; ...`; with `-n named`
  it stayed `named`). A shell pane takes OSC 2 (runtime: title became
  `neopane-xyz`).

## Tabs

- `new-tab [-n NAME] [--cwd] [--close-on-exit] [--no-focus] [-- CMD ARGS...]`
  prints the new tab's id as a bare number (help: "Returns: The created tab's
  ID as a single number on stdout"). Runtime: printed `1`. It does not print
  the first pane's id; `list-panes --json` gives it by `tab_id`.
- Tab ids are stable; positions shift (0.44.0, `CHANGELOG.md:196`).
  `go-to-tab-by-id`, `close-tab-by-id`, `rename-tab-by-id` take the id; `go-to-tab`
  takes a 1-based position. Runtime: `go-to-tab-by-id 0` moved the client;
  `go-to-tab-by-id 7` (no such tab) exited 0 and did nothing.
- `current-tab-info` from outside a pane printed "No active tab found for
  current client" (runtime), so use `list-tabs --json` instead.
- Tabs may have different sizes when clients sit on different tabs (0.45.0,
  `CHANGELOG.md:74`). Runtime: with a 120x40 client on tab 1 and a 100x30 one on
  tab 0, `list-tabs` gave tab 1 `120x38` and tab 0 `100x28`; with both on one
  tab it was the smaller size.

## Focus

- `focus-pane-id ID` (0.44.1, `CHANGELOG.md:135`) switches to the pane's tab and
  focuses it (`screen.rs:8724-8752`), **for the acting client only** (see
  [Targeting](#targeting-a-session-from-outside)). Runtime, after client 152
  typed: `focus-pane-id terminal_15` (other tab) and `focus-pane-id
  terminal_1` moved client 152 across tabs. Before any client typed, it exited 0
  and moved nobody. It exits 2 with "Pane Terminal(1) is already focused" or
  "Pane with id Terminal(999) not found" (`screen.rs:14421`, `:14431`).
- `mirror_session true` (default false, `screen.rs:10863`) makes a focus
  change move every connected client (`panes/tiled_panes/mod.rs:1134-1143`).
  Runtime: in a mirrored session, `focus-pane-id` moved the never-typing client
  within its tab, but not to another tab.
- `move-focus left|right|up|down` **does not wrap** and exits 0 when there is
  no neighbour. Runtime with `terminal_10` full height on the left, `11` top
  right, `12` bottom right: `left` from 10 stayed on 10; `down` from 12 stayed;
  `up` from 11 stayed; `right` from 11 stayed; `right` from 10 went to 12, the
  last focused of the two.
- `move-focus-or-tab` crosses to the neighbouring tab at an edge, and wraps
  around the tab list (runtime: `right` from the last tab went to tab 0).
- `focus-next-pane`, `focus-previous-pane`, `focus-last-pane` exist (help).

## Closing, resizing, moving

- `close-pane -p ID` closes any pane by id (0.44.0, `CHANGELOG.md:230`).
  Runtime: exit 0 on a live pane, and **also exit 0** on one already closed or
  never existing (`terminal_999`).
- `resize [-p ID] increase|decrease|+|- [left|right|up|down]` only steps; no
  absolute size (`zellij-utils/src/data.rs:812-823`). Runtime: `resize 40 right`
  failed, exit 2. A tiled step is 5 % of the tab
  (`panes/tiled_panes/tiled_pane_grid.rs:19`); a floating step is 5 columns or 2
  rows (`panes/floating_panes/mod.rs:37-38`). Runtime: one `increase right` on a
  60-wide pane in a 120-wide tab gave 66.
- `move-pane [-p ID] [DIRECTION]` swaps the pane with its neighbour in that
  direction, or rotates without one (`panes/tiled_panes/mod.rs:2621`,
  `:2710-2719`). Runtime: `move-pane -p terminal_13 right` swapped 13 with the
  pane to its right. Nothing moves a pane beside an arbitrary other pane.
- `override-layout --layout-string KDL [--retain-existing-terminal-panes]`
  replaces a tab's layout at runtime (0.44.0). Whether it can re-size or
  re-arrange the existing panes in place is *unconfirmed*. `dump-layout` prints
  KDL with sizes as percentages but **no pane ids** (runtime).

## Reading the layout

- `list-panes --json --all` (0.44.0, `CHANGELOG.md:194`) prints one object per
  pane of every tab, plugins included: the `PaneInfo` fields
  (`zellij-utils/src/data.rs:3539-3595`) flattened with `tab_id`,
  `tab_position`, `tab_name`, and `pane_command`, `pane_cwd` when known
  (`data.rs:3598-3609`). Fields seen at runtime: `id`, `is_plugin`,
  `is_focused`, `is_fullscreen`, `is_floating`, `is_suppressed`,
  `is_collapsed`, `title`, `exited`, `exit_status`, `is_held`, `pane_x`,
  `pane_y`, `pane_columns`, `pane_rows`, `pane_content_x`, `pane_content_y`,
  `pane_content_columns`, `pane_content_rows`, `cursor_coordinates_in_pane`,
  `terminal_command`, `plugin_url`, `is_selectable`, `index_in_pane_group`,
  `default_fg`, `default_bg`, `nested_session_name`, `attention`,
  `command_state`, `program_status`, `tab_id`, `tab_position`, `tab_name`,
  `pane_command`, `pane_cwd`.
- Geometry is in cells, in screen coordinates, frame included; the
  `pane_content_*` fields exclude the frame (`data.rs:3528-3537`). Runtime: a
  tiled pane `0,1 60x38` had content `0,2 59x37` (a title row and a right
  border column); a float `10,5 40x10` had content `11,6 38x8`; a lone pane had
  no frame (content = rect).
- `list-tabs --json --all` (0.44.0, `CHANGELOG.md:196`) gives `TabInfo`
  (`data.rs:3478-3518`): `tab_id`, `position`, `name`, `active`,
  `viewport_rows/columns` (the area for panes, bars excluded),
  `display_area_rows/columns`, `are_floating_panes_visible`,
  `is_fullscreen_active`, `other_focused_clients`, and more. **The viewport's
  origin is not given.** Runtime: tiled panes of a 120x38 viewport covered
  x 0-119, y 1-38; the tab bar is a plugin pane at y 0 and the status bar at
  y 39 (`plugin_url` `tab-bar`/`status-bar`, `is_selectable: false`). So a
  tab's area is the bounding box of its tiled selectable panes, and each pane's
  rect is `(pane_x - x0) / viewport_columns` etc.
- `is_focused` is unreliable for "the user's pane": it is true if *any* client
  id, attached or not, has the pane as its active pane in that layer
  (`panes/active_panes.rs:137-142`; `tiled_panes/mod.rs:3376-3380`; floats
  separately, `floating_panes/mod.rs:1894-1897`). Runtime: after a few
  `focus-pane-id` calls from the CLI, all three tiled panes of a tab showed
  `"is_focused": true` while the one attached client sat on one of them.
- `list-clients` (present at `v0.44.0`) prints a table, `CLIENT_ID
  ZELLIJ_PANE_ID RUNNING_COMMAND`, one row per client with its focused pane
  (`terminal_N`). It has no `--json`. Runtime: `10 terminal_13 sleep 100000`
  and `152 terminal_1 ...`. Once, after `go-to-tab-by-id` from the CLI, it
  also listed a client `1` that was not attached; why is *unconfirmed*.
- Floats: `is_floating`, and `list-tabs`' `are_floating_panes_visible` says
  whether the float layer shows. Hidden stack members and in-place-replaced
  panes show `is_suppressed: true` with a stale rect (runtime).
- Exit: `exited`, `exit_status`, `is_held` (runtime above). `pane_command` of
  a held pane was wrong (runtime: `sleep 100000` for the exited `sh -c 'exit
  3'` pane); use `terminal_command`.
- No pid and no tty in `list-panes`. `pane_command` and `pane_cwd` come from the
  pty thread with a 100 ms timeout each (`route.rs:3996-4053`).

## Proving a terminal belongs to a pane

- From the CLI, no pid or tty per pane. The process a pane runs has
  `ZELLIJ_PANE_ID` and `ZELLIJ_SESSION_NAME` in its env, inherited by whatever it
  starts, so an attach helper started in a pane can report them (runtime above).
- The pane's command is a child of the server with the pane's pty as its
  controlling tty (`os_input_output_unix.rs:251`). Runtime: `ps` showed the
  `sleep 100000` panes as children of `zellij --server ...` on `ttys002` and
  `ttys006`.
- The plugin API has `get_pane_pid(pane_id)` (`zellij-tile/src/shim.rs:2468`;
  present at `v0.44.0`, not `v0.43.1`). Reaching it needs a plugin; *source
  only*.
- A shell pane's OSC 2 title shows in `list-panes`' `title`; a command pane's
  does not (see [Creating panes](#creating-panes-and-reading-their-ids)).

## Events

- No CLI stream of layout or focus changes. `zellij subscribe -p ID... [-f
  json]` (0.44.0, `CHANGELOG.md:222`) streams the *content* of the named panes
  and a `pane_closed` event when one closes (`zellij-client/src/cli_client.rs:877-893`).
  Runtime: subscribing to `terminal_20` and `terminal_22` printed
  `{"event":"pane_update","is_initial":true,...}` for each, then
  `{"event":"pane_closed","pane_id":"terminal_20"}` and the same for 22 as each
  was closed, and exited 0 once both were gone. It only covers the ids given
  at start.
- Plugins can subscribe to `TabUpdate`, `PaneUpdate` (a full `PaneManifest`),
  `PaneClosed` and `ListClients` (`zellij-utils/src/data.rs:1252-1307`), and can
  write to a CLI pipe with `cli_pipe_output` (`shim.rs:1994`), which `zellij
  pipe --plugin URL --name N` prints on stdout (its help). A long-running
  `zellij pipe` fed by such a plugin could be an `events` stream; *source only*,
  not built or run here. Plugins also have `focus_terminal_pane` (switches tab
  and layer, `shim.rs:1791-1806`); which client it acts for in a multi-client
  session is *unconfirmed*.

## Headless sessions for tests

- `zellij attach --create-background NAME [-- CMD ARGS]` starts a session with
  no client (`-b`; the initial command is 0.45.1, `CHANGELOG.md:70`). Runtime: it
  exited 0, and `new-pane`, `new-tab`, `list-panes`, `close-pane`, `resize`,
  `move-pane`, floats and `subscribe` all worked without a client. The
  headless size was 50x50 (viewport 50x48). How to set another size is
  *unconfirmed*.
- Focus needs a client: `move-focus` exits 2 headless (above). A PTY client
  (`zellij attach NAME` in a PTY) is enough; to have CLI focus actions move it,
  it must have pressed a key first.
- A fresh cache shows a welcome or release-notes plugin as a float
  (`plugin_4`, runtime); `close-pane -p plugin_4` removed it. The 0.46 first-run
  screen in an interactive client offered "Install Zellij.app"; it was not
  approved.
- `zellij kill-session NAME` and `kill-all-sessions -y` end servers (runtime);
  killed sessions stay listed as resurrectable in the cache dir.

## Not confirmed

- tmux inside zellij, and zellij inside a real tmux (tmux not installed; `TMUX`
  was faked).
- Whether `override-layout` can rearrange live panes with exact sizes.
- A plugin-based `events` or `focus`; nothing was built.
- Setting a headless session's size.
- Why `list-clients` once listed a client that was not attached.

## What this means for the pane provider interface

- One-shot commands cover most capabilities: `open` and `open_beside`
  (`new-pane -- {attach}` prints `terminal_N`; argv, no shell), `open_float`
  (`--floating -x -y --width --height`, cells), `open_tab_page` (`new-tab --
  {attach}`, then the pane id from `list-panes`), `close` (`close-pane -p`,
  idempotent), `report_layout` (`list-panes --json` plus `list-tabs --json`).
- `open_beside` is possible only for `right` and `down`. `left` and `up` need
  an open to the right/below, then `move-pane` to swap, which swaps sizes too.
  Splitting a given origin needs `ZELLIJ_PANE_ID={origin's id}` in the
  command's environment with `--near-current-pane` or `--no-focus`; a command
  provider would wrap the call (`env ZELLIJ_PANE_ID=... zellij ...`).
- `place_beside` has no direct equivalent: `move-pane` only swaps neighbours.
- `resize` to a `ratio` is not one call: tiled resizes step by 5 % of the tab,
  so a provider must loop and land within 5 %. Floats move and size exactly with
  `change-floating-pane-coordinates`.
- `focus` works only for the client that last pressed a key, and then across
  tabs too; `focus-pane-id` exits 2 when the pane is already focused, which a
  `focus` command should treat as done. `focus_direction` maps to `move-focus`,
  which never wraps, so no guard is needed (unlike tmux).
- `report_layout`: areas are tabs (`tab_id`); each tab's origin is the top-left
  of its tiled panes, its size `viewport_columns` x `viewport_rows`; the
  `inset` comes from `pane_*` minus `pane_content_*`. Focus must come from
  `list-clients` (a table, not JSON), not `is_focused`. Hidden panes
  (`is_suppressed`) and bar plugins (`is_selectable: false`) should be left out.
- `events` has no CLI source. `subscribe` reports closes of known ids only;
  anything more needs a zellij plugin piping out through `zellij pipe`. Without
  it, the daemon's run-after-each-act `report_layout` is the route.
- `find` and reach: `find_vars` `ZELLIJ_PANE_ID` gives the id directly
  (`terminal_$ZELLIJ_PANE_ID`); `context_vars` should be `ZELLIJ_SESSION_NAME`,
  and the provider must run with the session's `ZELLIJ_SOCKET_DIR` and cache
  dir. `ZELLIJ` alone is always `0` and proves nothing. `TMUX` in a zellij
  pane can be stale, so a tmux provider's reach must not trust it there.
- `--no-focus` (0.45.0+) keeps opens from stealing focus; the rest needs 0.44.1
  (`focus-pane-id`), and 0.46 changed socket naming, so a provider should
  require at least 0.44.1 and treat sessions by name as needing 0.46's index.
