# herdr as a pane provider

Facts gathered for #79 (what herdr can offer a neorex pane provider), checked
against herdr 0.9.5 (`herdr --version`; socket protocol 22). Source is the
`v0.9.5` tag of herdrdev/herdr (commit `2ecf19a9`), cited as `file:line` in that
tree; the socket API docs are `docs/next/website/src/content/docs/socket-api.mdx`
in the same tag. The installed binary's `herdr api schema --json` is
byte-identical to the tag's `docs/next/api/herdr-api.schema.json`, so the tag
matches what runs here. Nothing was run against a live server: this machine has
no herdr server running, and the research did not start one or send any
mutating command. Every behaviour below is from help, schema or source, not
observed.

## Identity and CLI shape

- Version: `herdr 0.9.5`, protocol 22 (`herdr --version`; `herdr status --json`
  reports `server.status: not_running`).
- Hierarchy: workspace > tab > pane. Public ids look like `w1:p1`, `w1:t1`
  (`socket-api.mdx`, "Pane control methods use public pane ids such as `w1:p1`").
- Transport: newline-delimited JSON over a Unix socket, default
  `~/.config/herdr/herdr.sock`, overridable by `HERDR_SOCKET_PATH` or
  `--session` (`socket-api.mdx`, "Socket transport" and "Socket paths";
  Windows uses a named pipe, same section).
- The CLI is a thin client over the socket. Each request opens a new connection,
  writes one request, reads one response (`src/api/client.rs:55-57`, `:123-125`).
- The CLI does not start a server; with none running it returns
  `server_not_running` (`src/cli/server_not_running.rs:26-37`).
- Inside a pane, herdr sets `HERDR_SOCKET_PATH`, `HERDR_ENV=1`,
  `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` (`socket-api.mdx`,
  "Process-launching methods accept an `env` object"). `--current` uses
  `HERDR_PANE_ID` (`cli-reference.mdx`, "For pane commands that accept
  `--current`").
- Pane processes get `TERM=xterm-256color`, `COLORTERM=truecolor`,
  `TERM_PROGRAM=herdr` (`src/pane.rs:72`, `:100-106`; `crates/ghostty-vt/src/lib.rs:34`).

## Making a pane and running a command

- `herdr pane split <id> --direction right|down [--ratio F] [--cwd P] [--env K=V] [--focus|--no-focus]`;
  response carries `.result.pane.pane_id` (`cli-reference.mdx` line ~220 and
  "For `pane split`"; `PaneSplitParams` in the schema). Default focus is no.
- `pane.split` has no command parameter. The new pane runs the default shell
  (`PaneSplitParams` fields: cwd, direction, env, focus, ratio, right_click,
  target_pane_id, workspace_id).
- To run a command in a split pane: `herdr pane run <id> "<cmd>"` types the text
  plus Enter into the shell. Success means queued, not executed
  (`cli-reference.mdx`, `pane run`; `socket-api.mdx`, "Successful responses still
  acknowledge queue admission").
- argv (no shell) commands: `layout.apply` with `LayoutNode` `pane.command`
  (`socket-api.mdx`, "`layout.apply` creates a fresh tab from a declarative
  tree"; schema `LayoutNode`; `src/app/api/layouts.rs:577-580`). It makes a new
  tab, or with `tab_id` replaces one and "does not preserve live PTYs, scrollback,
  or running processes" (`socket-api.mdx`, same section). Its nested splits go
  through `split_pane_argv_command_with_ratio` (`src/app/api/layouts.rs:426-440`).
- argv panes in an existing tab: `plugin.pane.open` with placement `split`
  (`socket-api.mdx` ~589-606; `src/app/api/plugins/panes.rs:82`). It needs an
  installed, enabled plugin manifest entry, so it is not a general-purpose call.
- Config custom commands with `type = "pane"` open a temporary zoomed argv pane
  and close it on exit (`configuration.mdx:228`); this is a keybinding, not an
  API method.
- `tab.create [--workspace] [--cwd] [--label] [--env] [--focus]` creates a tab
  with a root shell pane; it takes no command (`TabCreateParams`; CLI help).

## Closing

- `herdr pane close <id>` / raw `pane.close` (`PaneTarget`). Immediate for
  ordinary panes: "Ordinary pane closes and CLI/API closes remain immediate"
  (`cli-reference.mdx`, the `confirm_close` paragraph). The only confirmation
  path in the API is closing a worktree group (`src/app/api/panes.rs:1967-1987`).
- Closing the last pane of a tab removes the tab; of the last tab, the workspace
  (`src/workspace.rs:869-893`; `src/app/actions.rs:938-944`).
- `tab.close <id>` closes a tab (CLI help).
- Teardown of the child process on close: not traced (unconfirmed).

## What a pane's command exiting does

- The pane is removed. The child watcher sends `PaneDied` after `child.wait()`
  (`src/pane.rs:2745-2766`); `handle_pane_died` removes the pane from its tab,
  and the workspace too if it was the last (`src/app/actions.rs:2059-2100`).
- Events on exit: `pane.exited` (`src/app/api.rs:305-316`) and `layout.updated`
  (`src/app/api.rs` test at `:2072-2080` asserts pane_exited comes before
  layout_updated). No `pane.closed` is emitted on this path; `pane_closed` is
  only emitted by explicit close code (`src/app/api/panes.rs:2003`, `:2021`).
  So a provider must treat `pane.exited` as the exit signal (grep of the v0.9.5
  tree; not observed live).
- Exit status is not exposed in the event (`EventData::PaneExited` carries only
  `pane_id` and `workspace_id`, `src/api/schema/events.rs:178`).
- A pane started with `pane.split` + `pane.run` runs in a shell, so when the
  command exits the shell remains and the pane does not close.

## Focus

- By id: raw `pane.focus` (`PaneTarget`), handled at `src/app/api/panes.rs:491-507`.
  The CLI has no by-id focus; `herdr pane focus` takes only `--direction`
  (`cli-reference.mdx` line ~215).
- By direction: `pane.focus_direction` / `herdr pane focus --direction`. Target
  is the nearest pane in that direction whose span overlaps the source
  (`src/layout.rs:369-`; `src/app/api/panes.rs:2109-2120`). No wrap: no neighbour
  returns `reason: no_neighbor` and focus does not move (`src/app/api/panes.rs:663-700`).
- Wrap-around exists only in the TUI client's `cycle_pane_next`/`previous`, which
  takes the focused tab's panes in list order modulo count
  (`src/client/shell/actions.rs:1064-1080`). It is not an API method.
- Neighbour/edge queries: `pane.neighbor`, `pane.edges` (both return the layout
  snapshot; `socket-api.mdx` ~190-193).

## Resizing, splitting ratios and moving

- Sizes are only ratios. A split's first child (the existing pane) gets `ratio`,
  clamped to 0.1-0.9 (`src/layout.rs:619-634`, `valid_split_ratio`; the API
  passes `ratio` through at `src/app/api/panes.rs:76-100`). The no-ratio path
  splits at 0.5 (`src/layout.rs:167`).
- `pane.resize --direction D [--amount F]` moves the nearest split by `F`
  (default 0.05, capped at 0.5; `src/app/api/panes.rs:734-739`). The split ratio is
  clamped 0.1-0.9 (`src/layout.rs:293-296`, `:320-322`, `:326-342`).
- `layout.set_split_ratio` sets a ratio by split path; clamped the same way
  (`LayoutSetSplitRatioParams`; `src/layout.rs:294-295`).
- No absolute cell sizes anywhere in the API.
- `pane.swap` (same tab only; directional or explicit ids; keeps ids, ratios and
  processes) (`socket-api.mdx`, "`pane.swap` supports directional and explicit
  forms", ~259-272).
- `pane.move <id> --tab T --split right|down [--target-pane] [--ratio]`,
  `--new-tab [--workspace] [--label]`, `--new-workspace`
  (`cli-reference.mdx`, pane move lines; `socket-api.mdx` ~273-296). Moves
  involving a zoomed tab return `changed: false, reason: zoomed_tab`. Cross-
  workspace moves keep the terminal but assign a new public pane id; herdr emits
  `pane.moved`, not fake close/create (`socket-api.mdx` ~292-295).
- `pane.zoom [--toggle|--on|--off]` (`socket-api.mdx` ~297-308).

## Reading the layout

- `pane.layout [--pane]`: `workspace_id`, `tab_id`, `zoomed`, outer `area`,
  `focused_pane_id`, pane rects and split rects/ratios, in cells
  (`socket-api.mdx` ~190-193; schema `PaneLayoutSnapshot`, `PaneLayoutRect` with
  uint16 x/y/width/height).
- `layout.export [--tab|--pane]`: portable BSP tree of `pane`/`split` nodes with
  label, cwd, argv command (`socket-api.mdx` ~199-210).
- `pane.list [--workspace]`, `pane.current`, `pane.get`, `tab.list`, `tab.get`,
  `session.snapshot` (one-time bootstrap; not a subscription; `socket-api.mdx`
  "session.snapshot returns...").
- `pane.process_info`: shell pid, foreground process group, and foreground
  processes with pid, name, argv, cwd where the platform exposes them
  (`socket-api.mdx` ~195-197).

## Events and waits

Long-running (one connection stays open):

- `events.subscribe` with `{type: ...}` entries. Pane types: `pane.created`,
  `pane.updated`, `pane.closed`, `pane.focused`, `pane.moved`, `pane.exited`,
  `pane.agent_detected`, `pane.output_matched`, `pane.agent_status_changed`,
  `pane.scroll_changed`. Also `tab.*`, `workspace.*`, `worktree.*`, and
  `layout.updated` (carries the full `PaneLayoutSnapshot` for one tab)
  (`socket-api.mdx` ~773-841; schema `Subscription` oneOf list).
- `pane.focused` fires for manual selection from any attached client; it does
  not identify the client (`socket-api.mdx` ~827).
- Subscriptions are not durable. A slow reader gets `error.code: events_lost` and
  the connection closes; recovery is a fresh subscription plus `session.snapshot`
  (`socket-api.mdx` ~796-810).

One-shot (one request; the response comes when the condition holds or times out):

- `events.wait` with `match_event` (`pane_created`, `pane_closed`,
  `pane_focused`, `pane_moved`, `pane_exited`, `pane_output_changed`,
  `pane_agent_*`, and tab/workspace variants) and `timeout_ms`. It has no
  layout-update match (schema `EventMatch` oneOf list).
- `pane.wait_for_output <id> --match TEXT | --regex RE [--timeout MS]`: searches
  the existing snapshot first, then polls (`cli-reference.mdx` ~410-415;
  `src/api/wait.rs:20-30`).
- `agent.wait`, server-owned and event-driven (agent state only; `socket-api.mdx` ~114).

## Floating panes

- No general floating pane. Panes are tiled BSP leaves in a tab; `pane.split`
  takes only direction and ratio (schema `PaneSplitParams`).
- `overlay` placement (plugin only): a temporary zoomed pane over the tab; focus
  and zoom are restored when its command exits (`src/app/custom_commands.rs:425-440`;
  `src/app/api.rs:281-300`). It is a normal pane with an id.
- `popup` placement (plugin only): session-modal outer terminal with optional
  `width`/`height` in cells or `"80%"`. It has no pane id, is outside all `pane.*`
  APIs, emits no pane lifecycle events, and is closed with `popup.close`
  (`socket-api.mdx` ~589-606; `src/app/api/plugins/mod.rs:492-505`;
  `src/app/api/plugins/panes.rs:11`).
- Config custom commands with `type = "popup"` open a session-modal terminal
  (`herdr --default-config`, lines ~141-147).

## Tabs

- `tab.create`, `tab.list`, `tab.get`, `tab.focus`, `tab.rename`, `tab.move
  --insert-index N`, `tab.close`, all taking a workspace-scoped id (schema
  `TabCreateParams`, `TabMoveParams`, `TabTarget`; CLI help for `herdr tab`).
- Tabs are per workspace; a tab holds one layout. A neorex Tab page could map to
  one herdr tab; cross-tab placement uses `pane.move --tab` or `--new-tab`.
- Tab-level events: `tab.created`, `tab.closed`, `tab.focused`, `tab.renamed`,
  `tab.moved` (`socket-api.mdx` ~820-822).

## Terminal capabilities

- Each pane is a herdr-owned terminal (libghostty via `crates/ghostty-vt`), not
  the outer terminal (`src/pane.rs:96-100`, the comment in `apply_pane_terminal_env`).
- Keyboard: input follows the keyboard mode each app requests (kitty keyboard
  included) (`keyboard.mdx:69`). Herdr's own tracker is for handoff only and is
  "never consulted for live input" (`src/pane/kitty_keyboard.rs:1-5`).
- Focus events: herdr sends focus in/out to a pane whose app enabled DECSET 1004
  (`crates/ghostty-vt/src/lib.rs:191`), from its own pane focus and the outer
  terminal's focus (`src/app/api.rs:876-925`).
- Undercurl: curly underline (`4:3`) is preserved in herdr's SGR output
  (`src/protocol/render_ansi.rs:447`; test `:1194-1201`; `src/pane/terminal.rs:6894`).
- Synchronized output: DECSET 2026 is tracked per pane
  (`crates/ghostty-vt/src/lib.rs:197`; `src/ui/tab_surface.rs:168`).
- Kitty graphics pass through; herdr manages placement and visibility
  (`socket-api.mdx` ~179-188; `configuration.mdx:526-549`).
- Not confirmed: how the outer terminal's keyboard protocol affects what herdr
  receives (`keyboard.mdx:71` says a Kitty-capable host is needed for physical
  keys on non-Latin layouts).

## What this means for the pane provider interface

- Almost every required capability is one-shot. Each call is one socket
  connection and one response (`src/api/client.rs:55-57`). Spawn: `pane.split`
  then `pane.run` (or `layout.apply` for argv panes in a new tab). Close:
  `pane.close`. Focus by direction: `pane.focus_direction`. Focus by id: raw
  `pane.focus` (no CLI form). Resize: `pane.resize` (fractions). Move: `pane.move`
  and `pane.swap`. Layout: `pane.layout`. Tabs: `tab.*`. A pane-provider CLI
  using only these needs no daemon of its own, only a running herdr server.
- Gaps the interface must allow for:
  - Commands in new panes: `split` + `run` leaves a shell behind after the
    command exits, so exit-driven pane close needs argv spawn. Argv spawn exists
    only in `layout.apply` (new tab) and plugin panes, not in `pane.split`.
  - Sizing is by ratio only, clamped 0.1-0.9 per split. Requests for cell sizes
    must be expressed as ratios.
  - No wrap in directional focus; a neorex cycle would be built on `pane.list`
    and `pane.layout`.
  - No generic floats. Overlay and popup are plugin-only, and popup has no pane
    id. A float capability is optional and would be Rex-only unless herdr
    grows a pane-level float API.
- Optional, richer capability (long-running connection): `events.subscribe` for
  `pane.focused`, `pane.exited`, `layout.updated` and `pane.moved`. The
  subscription is not durable, so a provider must reconcile with `session.snapshot`
  after `events_lost`. `events.wait` covers one-shot waits on pane events but
  not layout changes.
- Exit: the provider should report "pane exited" from `pane.exited`, and not
  rely on a `pane.closed` event for process exit.
- Terminal capabilities: for herdr they are a property of the herdr server's
  terminal layer, the same for every pane. The interface need not carry
  per-pane keyboard-protocol, focus-event or undercurl flags for herdr. The
  open question for other providers stays open.
- Windows: herdr's socket is a named pipe on Windows (`socket-api.mdx`,
  "Socket transport"). A provider that talks to the socket via the CLI or raw
  protocol does not depend on passing a tty fd, so this does not hinder Windows.
  (Inference from the API surface: the socket requests carry no fd.)
- Unconfirmed, not traced: exact teardown of a closed pane's process; whether
  `layout.apply` argv runs without a shell; herdr on Linux; live behaviour of
  any call (no server running here).
