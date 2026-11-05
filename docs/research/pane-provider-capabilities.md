# Pane provider capabilities

Facts gathered for the "pane provider" interface (shell commands that
open/focus/close terminals in Rex, tmux, herdr, Ghostty+AeroSpace). Sources:
tmux master `a73616d`, herdr master `ee5bfb5` (source cloned; the installed
binary is 0.9.5, and master is ahead of it, "Unreleased"), Ghostty main
`081a587`, AeroSpace main `bc6be84`, Rex docs/changelog/feedback via the
superlogical MCP. "Confirmed" = read in primary source; "unconfirmed" = not
found or only second-hand.

## 1. Focus reporting (CSI I / CSI O, DEC 1004)

| Host | Pane-to-pane focus change reports in/out to panes? | Status |
| --- | --- | --- |
| tmux | Yes, if `focus-events on` and the pane enabled 1004; also on window switch | Confirmed |
| herdr | Yes, Lost to the old pane, Gained to the new, if the pane enabled 1004 | Confirmed (source; master, not the 0.9.5 tag) |
| Rex | Probably yes (clicking another pane delivers `ESC[O`); docs silent | Unconfirmed (one user report) |
| Ghostty | Yes, on window key/focus change and surface focus change | Confirmed |

### tmux (confirmed)

- `focus-events` is a server option, default off; "When enabled, focus events
  are requested from the terminal if supported and passed through to
  applications running in tmux" (`tmux.1:5116`, `options-table.c:482`).
- `window_set_active_pane` (the pane-to-pane switch) calls
  `window_pane_update_focus(lastwp)` then `(w->active)` when `focus-events` is
  on (`window.c:779-783`).
- `window_pane_update_focus` writes `\033[O` / `\033[I` to the pane only when
  its screen has `MODE_FOCUSON` (DEC 1004) (`window.c:719-755`). A pane counts as
  focused only if it is its window's active pane and some attached client with
  `CLIENT_FOCUSED` is showing that window.
- Window switches (`select-window`, `next-window`): `session.c:517-520` calls
  `window_update_focus` on old and new windows, gated on `focus-events`.
  Attach/detach: `server-client.c:388-393`. Outer terminal focus in/out:
  `tty-keys.c:984-994`. Closing the active pane refocuses the new one
  (`window.c:1242`).
- Pitfall: if the *outer* terminal does not report focus, `CLIENT_FOCUSED`
  stays set from attach and in/out still flow for internal moves.
  Panes that already exited (`PANE_EXITED`) get nothing.
- Source: https://github.com/tmux/tmux (files above).

### herdr (confirmed in source)

- `sync_focus_events_with_outer_event` (`src/app/api.rs:879-912`): when the
  focused `(workspace, pane)` changes it sends `FocusEvent::Lost` to the old
  pane and `Gained` (or `Lost` if the outer terminal is unfocused) to the new
  one; outer terminal `\x1b[I`/`\x1b[O` are parsed (`src/raw_input.rs:1045-1046`)
  and forwarded to the focused pane.
- `Pane::try_send_focus_event` (`src/pane.rs:3718-3731`) drops the event unless
  the pane's terminal has focus reporting (1004) enabled.
- Tests assert exact bytes `\x1b[?1004h` then `\x1b[I`/`\x1b[O` across pane,
  tab and workspace navigation (`src/server/headless/tests/mod.rs:2082-2292,
  5212-5253, 6641-6725`). Changelog: "manual pane navigation emits focus events
  again (#4077)" (`CHANGELOG.md:178`) so it was regression-prone; shell-client
  (headless) navigation uses a separate path, `client_views.rs:476`.
- Source: https://github.com/ogulcancelik/herdr

### Rex (unconfirmed)

- Rex docs and changelog (searched "focus reporting", "1004", "focus events")
  never mention focus reporting or DEC 1004. Rex embeds libghostty-vt
  (changelog preview.11: "VT compatibility improvements from upstream
  libghostty-vt"), so mode 1004 parsing is probably present.
- One community report: "I turned on focus reporting (`ESC[?1004h`), the pane
  gets `ESC[O`" when clicking another pane; but clicking the *sidebar* sends
  nothing, and focus moves by script (`rex pane focus`) were not tested
  (https://discord.com/channels/1546797889009881158/1567632954715537529,
  Rex Beta 0.1.0 (1758)). Treat as unconfirmed for CLI/Lua-driven focus and
  for the Rex window losing OS focus. A cheap local probe would settle it.

### Ghostty (confirmed)

- macOS: `windowDidBecomeKey/ResignKey` -> `syncFocusToSurfaceTree()` ->
  `surfaceView.focusDidChange(focused)` where focused = window is key AND the
  surface is focused/first responder
  (`macos/Sources/Features/Terminal/BaseTerminalController.swift:350-358,
  1258-1278`; `SurfaceView_AppKit.swift:445`) -> `ghostty_surface_set_focus`.
- `Surface.focusCallback` ignores unchanged state (`src/Surface.zig:3466-3476`);
  `Termio.focusGained` writes the report only if mode `focus_event` (1004) is
  set (`src/termio/Termio.zig:683-697`).
- So switching Ghostty windows (including via AeroSpace) yields in/out.

## 2. Id reuse

Answer: none of these is documented/observed to reuse ids within a server
lifetime, except AeroSpace/macOS window ids, which are unconfirmed.

| Id | Reuse? | Status |
| --- | --- | --- |
| tmux `%N`, `@N`, `$N` | No, monotonic `u_int` counters (wrap at 2^32); reset when the server restarts | Confirmed |
| herdr `wN`, `wN:pN`, `wN:tN` | No within a workspace; workspace counter is process-wide | Confirmed, one edge (below) |
| Rex `block:<uuid>` | Practically no (UUIDv7) | Docs silent; format observed locally |
| AeroSpace `window-id` (CGWindowID) | Unknown; not controlled by AeroSpace | Unconfirmed |

- **tmux**: `tmux.1:924-933`: ids "are unique and are unchanged for the life of
  the session, window or pane in the tmux server". Code: `next_window_pane_id`
  / `next_window_id` post-increment, never decremented (`window.c:61-62, 446,
  1474`); `next_session_id` (`session.c:30,137`). A closed pane id can only be
  reissued after a server restart (counters restart at 0), or ~4 billion
  allocations. Closing `%N` seconds after exit cannot hit a new pane; it fails
  with "can't find pane".
- **herdr**: ids are short base-32-ish handles (`encode_public_number`,
  `src/workspace.rs:112-125`; form `w1`, `w1:p1`, `w1:t1`, letters after 9).
  Per-workspace `next_public_pane_number`/`next_public_tab_number` only grow
  (`workspace.rs:200-201, 522-524, 634`) and are persisted/restored with
  `max(...)` (`src/persist/restore.rs:331-338`). Changelog: "closed tab and pane
  ids no longer retarget later resources (#569)" (`CHANGELOG.md:625`).
  Workspace ids come from a process-wide atomic counter
  (`workspace.rs:104-110`), re-seeded after restart from the max id among
  *surviving* workspaces (`reserve_workspace_ids`, `workspace.rs:151`), so an
  id of a highest-numbered workspace closed before a server restart may be
  reissued after it. (Inferred from code, not tested.) A pane id contains its
  workspace, so a stale pane id from a closed workspace could then alias.
- **Rex**: `rex block ls --json` here shows
  `"block_id": "block:01a1a7b9-7cc4-7ad1-9625-1c6467c6ad8a"`: a UUID whose
  version nibble is 7 (time-ordered, with random bits). Short ids (suffix
  prefixes) are what the CLI shows by default (`--short`), and `rex block
  move` docs say a label that collides with a direction word needs "the full
  block ID or a hexadecimal ID suffix"
  (https://www.superlogical.com/rex/docs/reference/cli#rex-block-move-block-direction-anchor).
  Docs do not promise non-reuse; UUIDv7 makes it a non-issue. Use full ids
  (`--short=false`); a short suffix/prefix could in principle collide.
  Scripts are told to save ids from `--json` creates
  (https://www.superlogical.com/rex/docs/automate/shell-scripting#keeping-track-of-ids).
- **AeroSpace**: `window-id` is the macOS `CGWindowID` (uint32) obtained from
  the private `_AXUIElementGetWindow`
  (`Sources/PrivateApi/include/private.h:26`,
  `Sources/AppBundle/util/accessibility.swift:362-366`); AeroSpace keys its
  `allWindowsMap` by that id and drops it when the window dies
  (`Sources/AppBundle/tree/MacWindow.swift:20-41, 90`). Apple's
  `kCGWindowNumber` docs page would not load via fetch (JS-only), so reuse
  behavior is unconfirmed. Treat ids as session-scoped and not guaranteed
  unique over time: the provider should verify an id still maps to the same
  app/title/pid (`aerospace list-windows --all --format
  '%{window-id} %{app-pid} %{window-title}'`) before closing.

## 3. Ghostty quick-exit behaviour

Answer: option `abnormal-command-exit-runtime`, u32 ms, default **250**
(`src/config/Config.zig:1363`). If the child exits within that many ms,
Ghostty shows an error message instead of closing. It can be set per launch
on macOS with `open -na Ghostty.app --args --abnormal-command-exit-runtime=0
-e cmd`. `ghostty +new-window` does not work on macOS and cannot set it.
Confirmed.

- Docs: "The number of milliseconds of runtime below which we consider a process
  exit to be abnormal... On Linux, this must be paired with a non-zero exit
  code. On macOS, we allow any exit code because of the way shell processes are
  launched via the login command." (`Config.zig:1357-1362`).
- Logic: `Surface.childExited` (`src/Surface.zig:1299-1330`):
  `runtime_ms <= threshold` -> (non-Darwin: exit code 0 is "good", skip) ->
  show the abnormal message and return (surface stays open). **On macOS exit
  code 0 does not help**: a fast command is abnormal regardless. The comparison
  is `<=` on whole milliseconds (`src/termio/Exec.zig:279-283`), so `=0` only
  disables it in practice (a process lives >= 1 ms; a 0 ms runtime would still
  match). Tested: no (not run, to avoid opening windows).
- Otherwise (runtime above the threshold) Ghostty prints "Process exited."
  and closes the surface immediately unless `wait-after-command` is true (default
  false) (`Surface.zig:1334-1372`; `Config.zig:1348-1352`).
- CLI: `help.zig:57-59` and `main_ghostty.zig:86-87` document `open -na
  Ghostty.app --args --foo=bar` for macOS. `-e` sets `initial-command`, forces
  `quit-after-last-window-closed=true`, `gtk-single-instance=false`, and
  `shell-integration=detect` (`Config.zig:1155-1200`). The `-n` flag means a
  new app instance (separate Ghostty process), not a window in the running
  one, so each pane is a process (and a Dock entry).
- `+new-window` (`src/cli/new_window.zig:109-160`): "Only supported on GTK";
  prints "+new-window is not supported on this platform" (`:259-260`). Even on
  GTK it forwards only `--working-directory`, `--command`,
  `--shell-integration`, `--title` and `-e`, so `abnormal-command-exit-runtime`
  could not be passed.
- Workaround if a per-launch flag is unwanted: make the command run longer than
  250 ms before exiting (e.g. the helper sleeps), or set the option in the
  user's Ghostty config.

## 4. AeroSpace latency

Not measured: the AeroSpace server is not running here (`aerospace list-windows
--all --json` prints "Can't connect to AeroSpace server. Is AeroSpace.app
running?", client 0.21.3-Beta). Failed-connection round trips took median
~28.4 ms, max 32.4 ms over 10 runs; that measures process start only, not
the server. Skipped per instructions; nothing that moves focus or windows was
run.
