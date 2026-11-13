# herdr as a pane provider, refreshed

A follow-up to `research/herdr-pane-provider:docs/research/herdr-pane-provider.md`
(herdr 0.9.5, protocol 22, nothing run live). This note covers what has changed
since then and what the live runs showed, measured against `docs/capabilities.md`
and `docs/config.md`. Facts the earlier note already settled are not repeated.

Sources:

- herdrdev/herdr at the `v0.9.5` tag (commit `2ecf19a9`), cited as `file:line`.
  Links take the form `https://github.com/herdrdev/herdr/blob/v0.9.5/<file>#L<line>`.
- `origin/master` at `ee5bfb50` (2026-10-29), for changes since the release.
- The installed binary's `--help`.
- Runs against private servers. Each ran with `HOME`, `XDG_CONFIG_HOME` and
  `XDG_STATE_HOME` in a temp dir and `HERDR_SOCKET_PATH=<tmp>/h.sock`. `herdr
  status --json` reported that socket. `~/.config/herdr/herdr.sock` did not
  exist, and the mtimes of `~/.config/herdr/*` were the same before and after.
- The rc files of the temp `HOME` were zsh markers (`.zshenv`, `.zprofile`,
  `.zshrc`, `.zlogin`), each appending its name, `$$`, `$0` and `$-` to a log.

Runs used macOS (arm64) with `SHELL=/bin/zsh`, plus Linux aarch64 in an
`alpine:3` container. In this note, "runtime" means one of these runs.

## Release, protocol, and what changed since 0.9.5

- The latest release is still `v0.9.5` (2026-10-14). Runtime: `gh release list
  -R herdrdev/herdr` printed `v0.9.5 Latest`. No preview build is newer: the
  newest is `preview-2026-10-14-fbd2ae23927f`.
- The protocol is still 22: `src/protocol/wire.rs:20` at the tag and on
  master. Runtime: `herdr status --json` printed `"protocol":22` for both the
  client and the server.
- The socket API is unchanged on master. Runtime: the
  `docs/next/api/herdr-api.schema.json` files at `v0.9.5` and `origin/master`
  compared equal in Python. The 39 commits since the release are fixes, CI and
  docs (`git log v0.9.5..origin/master`).
- `CHANGELOG.md` on master has an empty `## Unreleased` section.

### Argv launch in `pane.split` (discussions #1695 and #3141)

- Both are GitHub Discussions on herdrdev/herdr, not issues.
  - [#1695](https://github.com/herdrdev/herdr/discussions/1695), opened
    2026-07-21, asks for an optional `argv` on `pane.split`.
  - [#3141](https://github.com/herdrdev/herdr/discussions/3141), opened
    2026-08-23, asks for `herdr pane split ... -- CMD`.
  - Neither has a maintainer reply. #1695 has two user comments: one links a
    fork branch `elucid/herdr:pane-split-argv`, and one reports a fish prompt
    plugin killing an `exec`'d child.
- In v0.9.5 and on master, `PaneSplitParams` has only `cwd`, `direction`,
  `env`, `focus`, `ratio`, `right_click`, `target_pane_id` and `workspace_id`
  (schema). `herdr pane split --help` lists no command argument.
- `SplitDirection` is only `Right | Down` (`src/api/schema/common.rs:72-75`).
  The new pane always goes right of or below the target.
- argv launch is still possible only through `layout.apply` (a new tab) and
  plugin panes (see the earlier note).
- According to #1695, `agent start -- <argv>` launched argv directly until
  0.7.5. In 0.9.5 `AgentStartParams` is `name`, `kind`, `pane_id`, `args`, and
  `timeout_ms` (schema), and it targets an existing pane.

### Floats

- The public API still has no float. The schema has no float method or
  parameter at either ref.
- [Discussion #3443](https://github.com/herdrdev/herdr/discussions/3443),
  "Native support for persistent floating panes", is open with no maintainer
  answer.
- Issue [#5835](https://github.com/herdrdev/herdr/issues/5835) describes
  floating windows, but in `herdr-app` 0.1.13, a separate desktop app with
  `openOverlay` mods. That is not the herdr CLI/server and was not investigated.

### Focus by id on the CLI

- None. `herdr pane focus` requires `--direction`; its `--pane` is the origin
  of the move (`--help`; `cli-reference.mdx:215`).
- `herdr agent focus <target>` resolves agents only. Runtime: `herdr agent
  focus w1:p3` printed `agent_not_found` for a plain shell pane.
- The raw `pane.focus` request is the only way
  (`src/app/api.rs:1179`; `src/app/api/panes.rs:491`).

## The minimal client

- The CLI has no raw-request subcommand. `herdr api` has only `snapshot` and
  `schema` (`herdr api --help`).
- `nc -U` is enough for one-shot requests. The server closes the connection
  after one reply, so `nc` exits without `-N` or `-w`. Runtime: `printf
  '{"id":"f1","method":"pane.focus","params":{"pane_id":"w1:p4"}}\n' | nc -U
  $HERDR_SOCKET_PATH` printed `{"id":"f1","result":{"type":"pane_info",...
  "focused":true...}}` in `real 0m0.007s`.
- Error replies are JSON with `error.code`, and `nc` still exits 0. Runtime:
  `pane.focus` on `w1:p99` printed `{"id":"c3","error":{"code":"pane_not_found",...}}`.
- `events.subscribe` from a raw client needs a connection that stays open. A
  Python `socket.AF_UNIX` client that sent
  `{"method":"events.subscribe","params":{"subscriptions":[{"type":"pane.exited"},...]}}`
  first got `{"id":"sub","result":{"type":"subscription_started"}}`, then one
  JSON line per event (runtime).

## Headless server

- `herdr server` runs a headless server in the foreground. Runtime: it printed
  `herdr server running; ... api socket: <tmp>/h.sock`.
- It starts with no workspace. Runtime: `herdr api snapshot` printed
  `"workspaces":[]`. `herdr workspace create --cwd D` makes `w1` with root pane
  `w1:p1`.
- With no client, the layout area is 120 x 40 cells
  (`DEFAULT_HEADLESS_COLS`/`ROWS`, `src/config.rs:79-80`). Runtime: `pane
  layout` printed `"area":{"height":40,"width":120,...}`.
- Two servers sharing one config dir share `session.json`. Runtime: a second
  server on `h2.sock` with the same `XDG_CONFIG_HOME` restored the first one's
  workspace `w1`, and its new workspace was `w2`. Use a separate config dir, or
  `--session`, per test server.

## `pane.split` + `pane run "exec CMD"`

- The split returns the new pane at once as `.result.pane.pane_id`. Runtime:
  `herdr pane split w1:p1 --direction right --ratio 0.3` printed
  `{"result":{"pane":{"pane_id":"w1:p2",...,"focused":false,"terminal_id":"term_..."},"type":"pane_info"}}`.
  The new pane is not focused by default.
- Pane numbers are not reused. Runtime: after `w1:p2` exited, the next splits
  were `w1:p3` and `w1:p4`.
- On macOS the shell is a login shell, and every rc file runs before the
  command. Runtime: the marker log printed `.zshenv`, `.zprofile`, `.zshrc`,
  `.zlogin`, each `argv0=-zsh opts=569XZilm`, then the command's own line,
  `cmd-start pid=32929`, with the same pid as the shell.
  - Source: `shell_mode = "auto"` means login on macOS only
    (`src/pane.rs:2023-2031`).
  - Unix login shells use portable-pty's default program with `SHELL` set
    (`src/pane.rs:2132-2137`), hence argv0 `-zsh`.
  - The shell is `terminal.default_shell`, else the server's `$SHELL`
    (`src/pane.rs:1826-1846`).
- On Linux the shell is not a login shell. Runtime in alpine:
  `pane.process_info` printed `"argv":["/bin/sh"]`, not `-sh`
  (`configuration.mdx:85`).
- `exec` replaces the shell, so the pane closes when the command exits.
  Runtime: `exec sh -c '...; sleep 2; exit 7'` kept pid 32929. When it exited,
  `pane list` no longer had `w1:p2`.
  - The subscriber got `pane_exited` (`{"pane_id":"w1:p2","workspace_id":"w1"}`,
    no exit status), then `layout_updated`. There was no `pane_closed`.
  - On Linux the same `exec` pane closed too (runtime: `pane list` printed only
    `w1:p1`).
- Without `exec`, the shell stays and the pane stays. Runtime: `pane run w1:p3
  "sh -c '...; exit 4'"` ran, and `process_info` then printed the foreground
  process `-zsh`.
- The typed command shows in the pane. Runtime: `pane read --source recent`
  after `pane run P "exec sleep 3"` printed `exec sleep 3` (the tty echoing the
  typeahead before the shell started), then
  `ianpascoe@ians-macbook herdr-research.119l % exec sleep 3`.
- `pane run` sent right after `pane split` (no wait for a prompt) worked with
  these trivial rc files: the pty buffered the typeahead.
  - Whether a slow or stdin-reading rc (direnv, nix, a prompt plugin) loses or
    mangles it is *unconfirmed*.
  - #1695's comments report races and a killed child in such setups.
  - The docs say success means "queue admission, not command execution"
    (`socket-api.mdx:141`).

## argv panes via `layout.apply`

- `layout.apply` with `command` arrays runs each argv with no shell and no rc
  files, in a new tab. Runtime: the marker log got only the command's own
  line, `argv-start`.
- The response maps each node to its new `pane_id`. Runtime: the reply's
  `root` held `"pane_id":"w1:p5"` and `"pane_id":"w1:p6"`, each with its
  `command`.
- `focus:false` creates the tab without switching to it (runtime: `tab list`
  printed `w1:t1 True`, `w1:t4 False`).
- When an argv command exits, its pane closes the same way. Runtime: `w1:p5`
  (`exit 3`) produced `pane_exited`, then a `layout_updated` with `w1:p6` alone.
  When the last pane of a tab exited, the tab went too (runtime: `tab list`
  afterwards had only `w1:t1`).
- In process info, an argv pane's `shell_pid` is the command itself. Runtime:
  `/bin/sleep 30` printed `"shell_pid":38542` with the foreground process
  `sleep` at pid 38542.

## Identifying a pane's terminal (`find`)

- `pane.process_info` returns `pane_id`, `shell_pid`,
  `foreground_process_group_id` and `foreground_processes[]`. Each process has
  `pid`, `name`, `argv0` (macOS only at runtime), `argv`, `cmdline` and `cwd`.
- The schema has a `tty` field (`src/api/schema/panes.rs:494-504`), but v0.9.5
  always sets it to `None` (`src/app/api/panes.rs:567`; the same on master).
  Runtime: no response had `tty`.
- The tty can be read from the pid. Runtime: `ps -o tty -p <shell_pid>` printed
  `ttys014` for `w1:p6`. In-pane `tty` printed `/dev/ttys012` for `w1:p5`, and
  `/dev/pts/1` on Linux.
  - A `find` can prove a terminal is a herdr pane's by comparing the attach's
    `TTY` with that tty.
  - `HERDR_PANE_ID` alone is not proof: a tmux inside the pane passes it on
    (below).
- `pane.focus` and every other `PaneTarget` take a public id (`w1:p1`, also
  `p_<n>` and legacy `w1-<n>` forms; `src/app/ids.rs:107-143`). `terminal_id`
  (`term_...`) is not accepted there.
- Cross-workspace `pane.move` assigns a new public id (earlier note), so a
  provider holding pane ids must follow `pane.moved`.

## Focus

- Raw `pane.focus` by id works across tabs: it switches the workspace's active
  tab. Runtime: focusing `w1:p8` in tab `w1:t4` changed `tab list` from
  `w1:t1 True / w1:t4 False` to `w1:t1 False / w1:t4 True`.
- API focus changes emit `pane_focused`. Runtime: `pane.focus` on `w1:p1`
  added exactly one event, `{"pane_id":"w1:p1","type":"pane_focused"}`, and no
  `layout_updated`.
- An unknown or closed id gives `error.code: pane_not_found` (runtime).
- `pane.focus_direction` with no neighbour returns `changed:false` and the
  layout (runtime). This matches the earlier note's no-wrap reading.

## Layout, ratios and swap

- `pane.layout` gives `area`, `focused_pane_id`, `panes[].rect` and
  `splits[]`, all in integer cells of herdr's tab area. Runtime:
  `"panes":[{"pane_id":"w1:p1","rect":{"height":10,"width":60,"x":0,"y":0}},...]`
  and `"splits":[{"direction":"down","id":"split_1_0","ratio":0.25,...}]`.
- A rect includes herdr's pane chrome. Runtime: a 60x30 rect reported
  `"viewport_rows":28`, while a lone 120x40 pane reported 40.
  - Borders are drawn per `ui.pane_borders` (default `auto`: only when split),
    `pane_gaps` and `pane_outer_borders` (`src/ui/panes.rs:196-260`;
    `configuration.mdx:314`).
  - The layout carries no per-pane inset, and `PaneInfo` has only
    `viewport_rows`. The inset in columns is *unconfirmed*.
- `ratio` is the share of the split's first child: the existing pane, on the
  left or top.
  - Runtime: `split --direction right --ratio 0.3` made `w1:p1` 36 of 120
    columns and the new pane 84.
  - `--direction down --ratio 0.25` made the existing pane 10 of 40 rows.
  - A neopane `RATIO`, the new terminal's share, maps to herdr `1 - RATIO`.
- `layout.set_split_ratio` paths are lists of booleans from the root: `[]` is
  the root, `false` goes into `first` and `true` into `second`
  (`src/layout.rs:659-677`).
  - Runtime: `path:[false]` set the left column's down split to 0.6 (rects 24
    and 16 rows).
  - `path:[true]` (a leaf) printed `split_not_found`.
  - `ratio:0.95` was stored as 0.9 (clamp, `src/layout.rs:628-634`).
  - `pane_id` may stand in for `tab_id`.
  - Split ids in `pane.layout` (`split_0_root`, `split_1_0`) look like depth
    plus position, but their mapping to paths was not checked (*unconfirmed*).
- `pane.swap` keeps ids and processes, and the pane ids move with their
  terminals.
  - Runtime: swapping `w1:p1` (focused, right) and `w1:p3` (left) put `w1:p1`
    on the right column with focus still on `w1:p1`.
  - A directional swap with no neighbour printed
    `"changed":false,"reason":"no_neighbor"`.
  - Since `pane.split` only splits right or down, a terminal for the left or
    up side is a split plus a swap.

## Environment in a herdr pane

- Runtime: `env` in an argv pane printed `HERDR_ENV=1`, `HERDR_PANE_ID=w1:p5`,
  `HERDR_TAB_ID=w1:t2`, `HERDR_WORKSPACE_ID=w1`,
  `HERDR_SOCKET_PATH=<tmp>/h.sock`, `HERDR_BIN_PATH=/Users/ianpascoe/.local/bin/herdr`,
  `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=herdr` and
  `TERM_PROGRAM_VERSION=0.9.5`.
  - Sources: `src/pane.rs:100-135`, `:184-220`; `HERDR_BIN_PATH` comes from
    `src/integration/env.rs:28-33`.
- herdr removes the outer terminal's handles: `ITERM_SESSION_ID`,
  `LC_TERMINAL*`, `WEZTERM_PANE`, `KITTY_WINDOW_ID`, `WT_SESSION`, `TMUX`,
  `TMUX_PANE`, `STY` and `ZELLIJ*` (`src/pane.rs:118-135`). It also removes
  some agent session variables (`:188-197`).
- Everything else comes from the server's environment, not the attaching
  client's. That includes `REX_*`, which herdr does not remove.
  - Runtime, herdr in tmux: a herdr server started in a private tmux session
    (`tmux -L herdr-research-outer`, with `-e REX_SESSION=fake-session -e
    REX_SERVER=ws://fake`) gave its panes `REX_SERVER=ws://fake`,
    `REX_SESSION=fake-session`, `TERM_PROGRAM=herdr`, and no `TMUX`.
  - `neopane.default` would choose Rex in such a pane (`docs/config.md`, "The
    `neopane` global").
- Runtime, tmux in herdr: an argv pane running `tmux -L herdr-research-inner
  new-session ...` gave the tmux pane `TERM=tmux-256color`,
  `TERM_PROGRAM=tmux`, `TMUX=...`, `TMUX_PANE=%0`, and also
  `HERDR_PANE_ID=w1:p7` and `HERDR_SOCKET_PATH`.
  - Every tmux pane in it therefore claims the one herdr pane.
  - `TERM_PROGRAM` names the innermost multiplexer.

## Platforms and installation for CI

- v0.9.5 ships `herdr-linux-x86_64`, `herdr-linux-aarch64`,
  `herdr-macos-aarch64`, `herdr-macos-x86_64` and
  `herdr-windows-x86_64.zip` (runtime: `gh release view v0.9.5 --json
  assets`).
- The Linux builds are musl targets (`.github/workflows/release.yml:81`,
  `:86`), so they are static.
  - Runtime: `file` printed `static-pie linked` for x86_64 and `statically
    linked` for aarch64.
  - Runtime: the aarch64 binary ran in `alpine:3` (`docker run --platform
    linux/arm64`), where `herdr --version` printed `herdr 0.9.5`.
  - The headless server, `workspace create`, `pane split`, `pane run "exec
    ..."` (the pane closed afterwards) and `process-info` all worked there.
  - The x86_64 binary was not run (*unconfirmed*).
- Install options:
  - `curl -fsSL https://herdr.dev/install.sh | sh`, which installs to
    `${HERDR_INSTALL_DIR:-$HOME/.local/bin}` after a SHA-256 check against the
    release manifest (`distribution/install.sh:6`, `:17-33`, `:80-114`).
  - `brew install herdr` or `mise use -g herdr` (`README.md:45-48`).
  - Downloading the release asset directly, which needs no installer.

## Follow-up: move-based open, ids, tty, paths, borders, snapshot, focus

These runs used a fresh private server (same temp `HOME`, config dir and
`HERDR_SOCKET_PATH` as above; `~/.config/herdr/*` mtimes unchanged, no
`~/.config/herdr/herdr.sock`). There were two workspaces, `w1` and `w2`, and a
Python subscriber on `pane.*`, `layout.updated`, `tab.*` and `workspace.*`.
Parts of items 5 and 7 were run with one or two TUI clients (`herdr` in a
100x30 PTY) attached to the same private socket; each bullet says which.

### 1. Open beside with no shell: `layout.apply` then `pane.move`

- It works within one workspace. First, `layout.apply` (`workspace_id:"w1"`,
  `focus:false`, one pane `command:["/bin/sleep","1000"]`) made tab `w1:t2`
  with pane `w1:p3`. Then `pane.move`, with `destination` `{"type":"tab",
  "tab_id":"w1:t1","target_pane_id":"w1:p2","split":"down","ratio":0.3}`
  and `focus:false`, printed this (runtime):
  `{"type":"pane_move","move_result":{"changed":true,"previous_pane_id":"w1:p3",
  "previous_tab_id":"w1:t2","pane":{"pane_id":"w1:p3","terminal_id":"term_6602a19f21ab54",
  "tab_id":"w1:t1",...},"target_layout":{...},"closed_tab_id":"w1:t2","focused_pane_id":"w1:p1"}}`.
  - The result key is `move_result`, not `pane_move`.
  - `PaneMoveParams` is `pane_id`, `destination` and `focus` (default
    `false`), per the schema.
- The emptied tab closes. The reply carried `closed_tab_id:"w1:t2"`, and
  `tab list` then had only `w1:t1` (runtime).
- In the same workspace the pane id and `terminal_id` stay the same:
  `w1:p3` and `term_6602a19f21ab54` both before and after (runtime).
- The process survives. `ps -o pid=,tty=,comm= -p 94842` after the move
  printed `94842 ttys025 /bin/sleep` (runtime).
- `ratio` means what it does for a split: the share of the first child, which
  is the target pane. The moved pane is always the second child, below or
  right of the target.
  - Runtime: `split:"down",ratio:0.3` beside `w1:p2` (60x40) gave `w1:p2`
    60x12 and `w1:p3` 60x28. The new split was `split_1_1`, ratio 0.3.
  - `split:"right",ratio:0.7` beside `w1:p1` (60 wide) gave `w1:p1` 42 and
    the moved pane 18.
- With `focus:false` nothing visible changes.
  - Runtime, before and after: `focused_workspace_id w1`, `focused_tab_id
    w1:t1`, `focused_pane_id w1:p1`.
  - The `layout.apply` with `focus:false` didn't switch tabs either.
  - Whether an attached client shows a brief extra tab in its tab bar was not
    observed (*unconfirmed*). The tab exists between the two requests.
- With `focus:true` the moved pane takes focus: the reply and the snapshot
  printed `focused_pane_id:"w1:p4"` (runtime).
- Events with `focus:false` (runtime, in arrival order):
  - from the `layout.apply`: `pane_created`, `layout_updated` (tab `w1:t2`),
    `tab_created`;
  - from the move: `pane_moved` (data: `closed_tab_id`, `previous_pane_id`,
    `previous_tab_id`, `previous_workspace_id`, `pane`), `layout_updated`
    (tab `w1:t1`, all four panes), `tab_closed` (`w1:t2`).
  - There was no `pane_closed`/`pane_created` for the move, and no
    `layout_updated` for the closed tab.
- Events with `focus:true` also included `pane_focused`, `tab_focused` and
  `workspace_focused`. They arrived grouped as `pane_created`, `pane_focused`,
  `pane_moved`, `layout_updated` (new tab), `layout_updated` (target tab),
  `tab_created`, `tab_closed`, `tab_focused`, `workspace_focused`. So event
  order is not causal order: `tab_created` arrived after `pane_moved`.
- A zoomed origin tab refuses the move and leaves the argv tab behind.
  - Runtime: after `pane.zoom` `mode:"on"` on `w1:p1`, the move printed
    `{"changed":false,"reason":"zoomed_tab","closed_tab_id":null}`.
  - Tab `w1:t4` with its pane `w1:p5` stayed, so a provider must close it or
    unzoom first.
- The moved pane's pty is not resized when it moves.
  - Runtime, with a TUI client attached: an argv pane looping `stty size`
    printed `29 73` in its own tab. After a move to a 37x10 slot it still
    printed `29 73` at 1 to 6 s.
  - It became `8 34` only after a later `pane.focus` on it. Its neighbour
    `w1:p2` likewise stayed `7 34` after shrinking to 19 wide, until a later
    `pane.split` in the tab made it `7 16`.
  - So after a move, the attach sees its real size only once something else
    relays out or refocuses the tab.
- Cross-workspace moves also work: `w1:p5` to `w2:t1` beside `w2:p1` (runtime).
  See item 2 for what happens to its id.

### 2. Pane id stability

- Cross-workspace moves change the public id but keep `terminal_id`.
  - Runtime: the reply printed `"previous_pane_id":"w1:p5"`, `"id":"w2:p2"`,
    `"term":"term_6602a1ae1b7f26"`. Before the move, `pane list` had
    `w1:p5#term_6602a1ae1b7f26`.
  - The process stayed alive: `96530 ttys027 /bin/sleep` (runtime).
- The old id keeps resolving to the moved pane. Runtime: `pane.get w1:p5`
  printed `"pane_id":"w2:p2"`.
  - herdr records an alias on a cross-workspace move
    (`src/app/api/panes.rs:1157-1163`) and drops it when the pane dies
    (`src/app/actions.rs:2075-2076`).
  - Aliases are checked first when an id is parsed (`src/app/ids.rs:108-110`).
- Old ids were never reused for new panes in this server.
  - Pane numbers are per workspace and only grow:
    `next_public_pane_number = max(next, number + 1)` (`src/workspace.rs:1143-1146`).
    They are written in a base-32 alphabet `1-9A-Z…`, so the 10th pane is `pA`
    (`src/workspace.rs:104-124`).
  - Runtime: after `w1:p5` moved away, the next split in `w1` was `w1:p6`, and
    in `w2` it was `w2:p3`.
  - Workspace ids come from a process-wide counter (`src/workspace.rs:107-110`).
    Runtime: closing `w3` then creating a workspace gave `w4`.
  - Tab numbers only grow too. Runtime: closing `w2:t2` then creating a tab
    gave `w2:t3`.
  - Whether numbering stays unique across a server restart that restores a
    session is *unconfirmed*: the workspace counter starts at 1 in each
    process.
- In the same workspace, a move keeps both the pane id and `terminal_id`
  (item 1). `terminal_id` was stable across every move seen, but the API
  takes only pane ids (`src/app/ids.rs:107-143`).

### 3. The tty proof for argv panes

- For a `layout.apply` argv pane, `shell_pid` is the argv process itself.
  Runtime, macOS: `command:["sh","-c","tty > F; exec sleep 100"]` printed
  `"shell_pid":32523` with the foreground process `sleep` at pid 32523.
  Without `exec`, it would be the `sh`.
- macOS: in-pane `tty` printed `/dev/ttys009`. `ps -o tty= -p 32523` printed
  `ttys009 ` (no `/dev/`, and a trailing space) (runtime).
- Linux, `alpine:3` aarch64 with the v0.9.5 static binary (runtime):
  - `shell_pid` was 26, also the argv process.
  - In-pane `tty` printed `/dev/pts/1`, and so did `readlink /proc/26/fd/0`.
  - procps `ps -o tty= -p 26` printed `pts/1`.
  - BusyBox's `ps` has no `-p`. Its `ps -o pid,tty,comm` printed `26 136,1
    sleep`, a major,minor pair, not a name.
- So a `find` should compare forms that agree: strip `/dev/` from the attach's
  `TTY` and trim `ps` output, or on Linux use `readlink /proc/<pid>/fd/0`.
  BusyBox `ps` can't be used.

### 4. Split ids and ratio paths

- A `pane.layout` split id is `split_<n>_<path>`. `<n>` is the split's index
  in pre-order, and `<path>` is the `set_split_ratio` path written as bits:
  `0` for `first`, `1` for `second`, and `root` for `[]`
  (`src/app/api/panes.rs:2303-2313`). Splits are listed in pre-order,
  first child before second (`src/layout.rs:528-554`).
  - This is not in the docs (`socket-api.mdx` doesn't mention split ids), so
    it is an implementation detail.
- Runtime: `pane.layout` printed splits `split_0_root`, `split_1_0`,
  `split_2_00` and `split_3_1`.
  - `layout.set_split_ratio` with `path:[false,false]`, `ratio:0.25` changed
    `split_2_00` to 0.25, making `w1:p1` 42x10 and `w1:p6` 42x30.
  - `path:[false,false,true]` printed `split_not_found`.
- `layout.export` gives the same tree, with `first` and `second` in the same
  order, so a path is also the route through `root` (runtime: its root was
  `split right 0.5`, then `first` was `split right 0.7`, then `first` was
  `split down 0.5` over `w1:p1`/`w1:p6`).
- Each split's ratio is relative to its own rect, not the tab. Runtime:
  `split_1_0` at 0.7 within the left 60 columns gave 42.

### 5. Border size

- With defaults (`pane_borders = "auto"`, `pane_outer_borders = true`,
  `pane_gaps = true`, `pane_scrollbars = true`; `src/config/model.rs:1216-1219`),
  a split pane's pty is its rect less 2 rows and 3 columns: a border on every
  side plus one scrollbar gutter column.
  - Runtime (client attached): rect 26x7 gave `stty size` `5 23`; 13x22 gave
    `20 10`; 19x9 gave `7 16`; 18x9 gave `7 15`.
  - Runtime (headless, at creation): 60x40 gave `38 57`.
  - Source: borders from `apply_pane_chrome` (`src/ui/panes.rs:196-260`), and
    the gutter is one column on the primary screen when wider than 4
    (`src/ui/panes.rs:40-51`, `:146-153`).
- A lone pane has no border, only the gutter. Runtime: a lone argv pane in a
  74x29 area printed `29 73`; headless 120x40 printed `40 119`.
- The gutter is reserved for the primary screen ("A new program starts on the
  primary screen, which reserves the gutter", `src/ui/panes.rs:147`).
  Whether an alternate-screen program gets the column back was not checked
  (*unconfirmed*).
- With no client attached, ptys keep the size they started at.
  - Runtime: `w1:p1`, started lone at 120x40, still printed `40 119` after
    two splits had shrunk its rect to 42x10.
  - With a client attached, the area became the client's (`74x29` for a 100x30
    terminal less sidebar and tab bar), and ptys followed. The move
    exception is in item 1.

### 6. `session.snapshot`

- `layouts` covers every tab of every workspace. The code loops over all
  workspaces and all tabs (`src/app/api/session.rs:45-62`).
  - Runtime: tabs `w1:t1`, `w2:t1`, `w2:t3` gave three layouts, including
    `w2:t3` in the unfocused workspace.
- Each layout carries its own `focused_pane_id`, `zoomed`, `area`, `panes`
  and `splits`. Runtime: `{"tab_id":"w2:t3","focused_pane_id":"w2:p5",...}`.
- The snapshot's top level has `focused_workspace_id`, `focused_tab_id` and
  `focused_pane_id` (the server's focus), next to `workspaces` (each with
  `active_tab_id`), `tabs`, `panes`, `agents`, `version` and `protocol`
  (runtime keys).

### 7. Focus

- Focus is one per server, not per client.
  - With no client, `pane.focus` changes `focused_pane_id` (runtime: the
    snapshot's `fp` moved to the target each time).
  - With two clients, each showed what the server focused. Client 2 was
    switched to `w1` with its own keys, and the server's
    `focused_workspace_id` became `w1`. Then `pane.focus` on `w2:p3` set it to
    `w2`, and both clients' screens showed workspace `w2` (runtime).
  - The docs say clients can view different tabs and that the last one to
    interact controls a shared tab's sizes (`concepts.mdx:73`). But an API
    focus moved every attached client here.
- `pane.focus_direction` takes an optional source: `PaneFocusDirectionParams`
  is `direction` plus optional `pane_id` (schema).
  - Without `pane_id`, the source is the focused pane of the server's active
    workspace (`resolve_optional_pane`, `src/app/api/panes.rs:2094-2103`).
  - With an id, focus lands in that pane's workspace and tab
    (`src/app/api/panes.rs:668-693`).
  - Runtime: with the server on `w2:p3`, `{"pane_id":"w1:p1","direction":"down"}`
    printed `changed:true`, `focused_pane_id:"w1:p6"`, and the server moved
    to `w1`/`w1:t1`/`w1:p6`.
  - A source that is gone gives `pane_not_found`.
  - `pane.neighbor` takes the same optional `pane_id`.

### 8. Fixing pty sizes after a move

Runs for items 8 to 12 used a fresh private server, with one TUI client
(`herdr` in a 100x30 PTY) attached, so the tab area was 74x29. Each test pane
was an argv pane looping `stty size > F` every 0.2 s.

- A move leaves both panes of the new split at the wrong size.
  - Runtime: A (`w1:p2`) printed `27 34` before. B (`w1:p3`), moved
    `split:"down",ratio:0.49` beside A, printed `29 73` before.
  - After the move the rects were 37x14 and 37x15, but 1.5 s later A still
    printed `27 34` and B `29 73`.
- `layout.set_split_ratio` to the exact value on the split the move made
  (`path:[true]`, `ratio:0.5`) resized both within 0.15 s.
  - Runtime: A printed `13 34` and B `12 34`, matching the new 37x15 and 37x14
    rects less 2 rows and 3 columns.
  - One `layout_updated` was emitted.
- Setting the value a split already has is not skipped.
  - The handler only reports `split_not_found` when the path misses
    (`src/app/api/layouts.rs:244-252`). It has no `unchanged` result: the
    reply is `layout_split_ratio_set` with only `layout` and `type`.
  - It still emits `layout_updated` (runtime and `:258`), and it still fixes
    stale ptys. Runtime: C, moved `split:"down",ratio:0.5` beside `w1:p1`,
    printed `29 73`. Setting `path:[false]` to its current 0.5 made it
    `12 34`.
- Any split in the tab will do; it doesn't have to be the one the move made.
  - Runtime: D, moved beside `w1:p3` (new split `split_3_11`), printed
    `29 73`.
  - A same-value set on the root split (`path:[]`, 0.5) made D `12 15` and
    its neighbour B `12 16`.
- Passing both `tab_id` and `pane_id` gives `layout_not_found`
  (`src/app/api/layouts.rs:264-265`).
- `pane.swap` does not leave ptys stale. Runtime: after swapping A (37x15)
  with C (37x14), A printed `12 34` and C `13 34` within 1.5 s, with no nudge.
  Nudges afterwards changed nothing.
- Why `pane.move` alone doesn't resize was not traced in the source
  (*unconfirmed*).

### 9. The gutter on the alternate screen

- An alternate-screen program gets the gutter column back, so its width is
  the rect less 2.
  - Runtime, in shell pane `w1:p1` (rect 37x15), `stty size` printed:
    - `13 34` before;
    - `13 35` 1 s after `printf '\033[?1049h'`;
    - `13 34` 1 s after `printf '\033[?1049l'`.
  - Source: the gutter applies only when `pane_scrollbars &&
    !rt.alternate_screen_active()` (`src/ui/panes.rs:37`). There is a test,
    `alternate_screen_reclaims_scrollbar_gutter_and_restores_it_on_exit`
    (`:1338`).
- So the right inset of a split pane is 2 on the primary screen and 1 on the
  alternate screen. A full-screen client such as neopane's attach sees one
  more column than a shell in the same pane.

### 10. Border settings over the socket

- No API method reads them.
  - The schema's property names include no border, gap or scrollbar setting:
    a grep for `border`, `gap` and `scrollbar` found only `scroll`.
  - None of the methods (full list in the schema) reads config.
  - `herdr config` has only `check` and `reset-keys`. `herdr config check`
    printed only `Config: ok` (runtime).
- A provider can read the user's `config.toml` itself (`$XDG_CONFIG_HOME` or
  `~/.config/herdr/`, `src/config/io.rs:30-34`). That is the file, not
  necessarily what the running server loaded before a reload.
- The rows can be measured instead. `PaneInfo.scroll.viewport_rows` is the
  pty's rows. Runtime: `w1:p1` with a 37x15 rect printed `"viewport_rows":13`.
  No API field gives the columns.

### 11. Zoomed panes in `pane.layout`

- `pane.layout` on a zoomed tab returns `zoomed:true` and every pane's
  unzoomed rect, hidden siblings included. The zoomed pane is not given the
  full area.
  - Runtime: after `pane.zoom` `mode:"on"` on `w1:p2`, it printed
    `"zoomed":true,"focused_pane_id":"w1:p2"`, `w1:p2` still 37x14 at
    `0,15`, all five panes and all four splits.
  - `layout.export` and `session.snapshot` likewise printed `zoomed:true`
    with all five panes.
- The zoomed pane's pty is resized to the whole area. Runtime: A printed
  `27 71` in a 74x29 area, less 2 rows and 3 columns, because a zoomed pane in
  a multi-pane tab keeps its frame (`zoomed_pane_borders`,
  `src/ui/panes.rs:53-58`). The hidden panes kept their sizes (C `13 34`, B
  `12 16`, D `12 15`).
- So for a zoomed tab, a provider should report the zoomed pane as filling
  the area and its siblings as hidden. The rects alone don't say so.

### 12. Live handoff

- Exercised with the raw `server.live_handoff` method and `params:{}`, which
  hands off to the same binary. The schema also allows `expected_version`,
  `expected_protocol` and `import_exe`.
  - The repo's own smoke test does the same
    (`scripts/smoke_live_handoff_sessions.sh:211`).
  - `herdr update --handoff` was not run: it installs a new binary, and the
    docs say it is disabled for mise, Homebrew and Nix installs
    (`session-state.mdx:136`). This machine's herdr comes from mise.
- It worked on the private server (runtime):
  - The reply was `{"id":"h","result":{"type":"ok"}}`.
  - The old server (pid 68765) was replaced by `herdr server
    --handoff-import <tmp>/home/.config/herdr/herdr-handoff-68765.sock ...`
    (pid 55901). Its version and protocol were still 0.9.5 and 22.
- Pane ids, tabs and processes survived.
  - Runtime: `w1:p1`..`w1:p5` and `w2:p1` were in the same tabs.
  - Their `shell_pid`s were unchanged: 68919, 33400, 40307, 55757, 58558.
  - The looping panes kept writing.
- Every `terminal_id` changed. Runtime: `term_6602a405e06a01` became
  `term_6602a4843317d1`, and the same for every pane. So `terminal_id` is not
  stable across a handoff, while pane ids are.
- Every connection ends.
  - The TUI client exited 1 with `herdr: server shut down: live update in
    progress; reconnect after handoff completes`.
  - The events subscriber's connection closed without an `events_lost` line.
  - The docs list both: "subscription streams, client sockets ... may be
    interrupted; clients should reconnect" (`session-state.mdx:123`).
- With no client left, the layout fell back to the headless 120x40 area, and
  the ptys followed. Runtime: A, now 60x20, printed `18 57`.

### 13. `pane.move` within one tab

These runs used a fresh private server with one TUI client attached (100x30
PTY, 74x29 tab area). `~/.config/herdr/*` mtimes were unchanged afterwards.

- A move to another slot in the same tab is refused. Runtime: in tab `w1:t1`
  (`w1:p1` left; `w1:p2` over `w1:p3` right), `pane.move` of `w1:p3` with
  `{"type":"tab","tab_id":"w1:t1","target_pane_id":"w1:p1","split":"down"}`
  printed `{"changed":false,"reason":"same_tab","id":"w1:p3"}`. The layout
  was unchanged. The docs say this too: same-tab layout changes "remain
  `pane.swap`" (`socket-api.mdx:283-285`).
- Moving out to a new tab and back works and keeps everything. Runtime:
  - Out: `{"type":"new_tab","workspace_id":"w1"}` with `focus:false` printed
    `changed:true`, id `w1:p3`, tab `w1:t2`, the same `terminal_id`
    `term_6602a53cb78da3`, and `created_tab: w1:t2`.
    - The server's focus stayed `w1:t1`/`w1:p1`. The reply's
      `focused_pane_id:"w1:p3"` is the new tab's own focus.
    - The CLI form is `herdr pane move --new-tab [--tab-label]` (`--help`).
  - Back: `{"type":"tab","tab_id":"w1:t1","target_pane_id":"w1:p1",
    "split":"down","ratio":0.5}` printed `changed:true`, id `w1:p3`, the same
    `terminal_id`, and `closed_tab_id:"w1:t2"`.
    - Focus stayed `w1:p1`.
    - The layout became `w1:p1` over `w1:p3` on the left and `w1:p2` on the
      right.
    - The shell kept its pid, 78540.
  - Its pty printed `12 34` for its 37x14 slot, which is correct. But the slot
    was the same size as before the round trip, so this doesn't show whether
    the move resized it.

### 14. The temporary tab in the client's tab bar

- The client draws the temporary tab for one frame. Method: the client ran
  under `script -q -F` in the PTY, recording its raw output. The applied tab
  was given a unique `tab_label` (`TMPQA`, `TMPQB`, `TMPQC`) to find in the
  stream.
- Runtime: in all three cases the label was drawn in the tab bar (row 1,
  column 36: `\e[1;36H ... TMPQA  ` then ` + `) inside one synchronized-output
  frame (`\e[?2026h`).
  - The cases were the requests back to back, 0.1 s apart and 0.5 s apart.
  - The very next frame redrew that spot as ` +       `, removing the tab,
    and drew the new split.
  - So the tab shows for exactly one frame; no case avoided it.
- Back to back, `layout.apply` plus `pane.move` over `nc -U` took 39 ms wall
  time (runtime). The flash lasts about that long plus the client's frame
  interval. The duration in ms wasn't measured (*unconfirmed*).
- `focus:false` kept the client on its tab throughout. Only the tab bar
  showed the extra tab.

## Not confirmed

- Whether `pane run` typeahead survives slow or stdin-reading rc files.
- The inset of a split pane in columns, and how split ids map to paths.
- Exit status of a pane's command: no event or call exposes it. This was
  checked against the schema and runtime events, not as an exhaustive search.
- The x86_64 Linux binary running.
- How many milliseconds the temporary tab is visible during the move-based
  open: it was seen for one frame each time.
- Pane, tab and workspace numbering after a server restart that restores a
  session.
- Why `pane.move` alone leaves pty sizes stale, when `pane.swap` and
  `layout.set_split_ratio` don't.
- `herdr update --handoff` itself: only the raw `server.live_handoff` to the
  same binary was run.
- herdr-app's floating windows.

## What this means for the pane provider interface

- The basic acts are still one-shot requests, and `nc -U` (or any Unix-socket
  write) is enough. Focus by id has no CLI form, so a provider's `focus`
  needs something like `sh -c 'printf ... | nc -U "$HERDR_SOCKET_PATH"'` or a
  small helper.
- `open_beside` for a herdr table is `pane split <origin> --direction
  right|down --ratio (1-RATIO)` followed by `pane run <new> "exec <attach>"`,
  with `left`/`up` adding a `pane.swap`. It prints the new `pane_id`.
  - The attach then runs under a login shell on macOS, after the user's rc
    files, with its command line visible in the pane.
  - It depends on typeahead surviving the rc files.
  - It gets the server's environment plus `--env`, not the choosing attach's.
- `open_tab_page` can use `layout.apply` with an argv `command`. This is the
  one path today with no shell, rc files or typed text.
- `close` must treat `pane_not_found` as success, since herdr returns an
  error for a pane that is already gone.
- An exited attach closes its pane only because of `exec` (or argv). herdr
  reports it as `pane_exited`, with no status and no `pane_closed`.
- `find` can prove a terminal by matching the attach's `TTY` against the tty
  of `pane.process_info`'s `shell_pid`. herdr's own `tty` field is always
  empty, and `HERDR_PANE_ID` leaks into nested tmux panes, so the variable
  alone (`find_vars`) over-claims.
  - `context_vars = { "HERDR_SOCKET_PATH" }` separates herdr servers.
  - A choice keyed on `REX_*` or `TMUX` can be fooled by what the herdr server
    inherited (`REX_*` kept) or stripped (`TMUX` removed).
- `report_layout` maps directly.
  - One area per herdr tab.
  - `rect` is the pane rect divided by `area`.
  - `focus` is `focused_pane_id`, which is per tab: the focused tab comes from
    `tab list`.
  - Insets for borders are not reported, and no API reads the settings.
    - With default config a split pane's inset is `[1, 1, 2, 1]`, or
      `[1, 1, 1, 1]` once its program is on the alternate screen.
    - A lone pane's is `[0, 0, 1, 0]`, or `[0, 0, 0, 0]` on the alternate
      screen.
    - A provider must derive them from the user's `config.toml`.
  - On a zoomed tab, `pane.layout` still gives unzoomed rects, so the
    provider must report the zoomed pane as filling the area itself.
  - `session.snapshot` gives every tab's layout in one call.
- `events` maps to a held `events.subscribe` connection.
  - Each event line can become a neopane line: `pane_focused` becomes
    `{"focus": id}`, `pane_exited`/`pane_closed` become `{"closed": id}`, and
    the rest are plain lines.
  - Focus changes produce `pane_focused` but no `layout_updated`, so both must
    be subscribed.
- `resize` maps to `layout.set_split_ratio`. The path comes from
  `layout.export`'s tree, or from the `split_<n>_<bits>` id in `pane.layout`,
  an undocumented mapping. `pane.resize` is relative only.
- A shell-free `open_beside` works as `layout.apply` (`focus:false`, one argv
  pane) followed by `pane.move` beside the origin, with `ratio = 1 - RATIO`.
  It keeps the pane id in the same workspace, closes the temporary tab, and
  changes no focus. Four things need handling:
  - a zoomed origin tab returns `zoomed_tab`, and the temporary tab must then
    be closed;
  - the pty isn't resized by the move. A same-value `layout.set_split_ratio`
    on any split of the tab fixes every pane in it at once;
  - left/up still needs a `pane.swap`;
  - an attached client's tab bar shows the temporary tab for one frame;
  - `place_beside` within one tab can't use `pane.move` (`same_tab`). It
    needs a round trip through a `new_tab` move, which keeps the id,
    `terminal_id` and process, or `pane.swap` where the shape already fits;
  - a cross-workspace move renames the pane id, but the old id stays an alias
    while the pane lives, and `terminal_id` is stable.
- Live handoff keeps pane ids and processes but changes every
  `terminal_id` and drops every connection. An `events` command must
  reconnect, and a provider should key terminals by pane id.
- `focus` and `focus_direction` act on the one server-wide focus and drag
  every attached client along. `focus_direction` can take the origin pane
  explicitly.
- `open_float` has no herdr equivalent in v0.9.5 or on master; a herdr
  provider should not declare it.
- CI can test a herdr provider on Linux with the static musl release binary
  and a headless `herdr server` on a private `HERDR_SOCKET_PATH` and config
  dir. No TUI or tty is needed.
